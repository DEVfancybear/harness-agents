//! Scripts run end to end, as pi's `sandbox.test.ts` runs them: output,
//! return values, tool calls, the store, errors and limits.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use harness_codemode::{
    CallStatus, CodemodeResult, ErrorKind, Fatal, Host, HostCall, RunOptions, ToolInfo,
    render_result, run,
};

/// Tools answered from a function, every batch recorded.
struct TestHost {
    batches: Vec<Vec<HostCall>>,
    answer: fn(&HostCall) -> Result<String, String>,
}

impl Host for TestHost {
    fn call(&mut self, calls: Vec<HostCall>) -> Result<Vec<Result<String, String>>, Fatal> {
        let answers = calls.iter().map(self.answer).collect();
        self.batches.push(calls);
        Ok(answers)
    }
}

fn tools() -> Vec<ToolInfo> {
    vec![
        ToolInfo::new("read_file", "Read a file"),
        ToolInfo::new("my-tool", "A tool whose name is not an identifier"),
        ToolInfo::new("fail", "Always fails"),
    ]
}

fn answer(call: &HostCall) -> Result<String, String> {
    match call.name.as_str() {
        "read_file" => {
            let args: serde_json::Value =
                serde_json::from_str(call.args.as_deref().unwrap_or("null")).unwrap();
            let path = args["path"].as_str().unwrap_or("?").to_owned();
            Ok(serde_json::Value::String(format!("contents of {path}")).to_string())
        }
        "my-tool" => Ok(call.args.clone().unwrap_or_else(|| "null".to_owned())),
        _ => Err("this tool always fails".to_owned()),
    }
}

fn exec_with(
    code: &str,
    store: Vec<(String, String)>,
    options: &RunOptions,
) -> (CodemodeResult, Vec<Vec<HostCall>>) {
    let mut host = TestHost {
        batches: Vec::new(),
        answer,
    };
    let result = run(
        code,
        tools(),
        store,
        options,
        Arc::new(AtomicBool::new(false)),
        &mut host,
    );
    (result, host.batches)
}

fn exec(code: &str) -> CodemodeResult {
    exec_with(code, Vec::new(), &RunOptions::default()).0
}

fn value(code: &str) -> String {
    match exec(code).outcome {
        Ok(Some(json)) => json,
        Ok(None) => "undefined".to_owned(),
        Err(error) => panic!("{code}: {error:?}"),
    }
}

fn texts(result: &CodemodeResult) -> Vec<String> {
    result.output.iter().map(|item| item.text.clone()).collect()
}

fn failure(code: &str) -> harness_codemode::ScriptError {
    match exec(code).outcome {
        Err(error) => error,
        Ok(value) => panic!("{code}: expected an error, got {value:?}"),
    }
}

#[test]
fn returns_the_value_after_a_json_round_trip_and_supports_top_level_await() {
    assert_eq!(
        value("return { a: [1, 'x', null, undefined], b: undefined }"),
        r#"{"a":[1,"x",null,null]}"#
    );
    assert_eq!(value("const x = await 5; return x * 2"), "10");
    assert_eq!(value("return"), "undefined");
    assert_eq!(value("return 0.1 + 0.2"), "0.30000000000000004");
}

#[test]
fn collects_text_and_console_output_in_order() {
    let result = exec(
        "text('a'); console.log('b', 1, {c: 2}, [NaN]); text({d: Infinity}); console.error(new Error('e')); text(null); text(-Infinity)",
    );
    let output = texts(&result);
    assert_eq!(output[0], "a");
    assert_eq!(output[1], "b 1 {\"c\":2} [null]");
    assert_eq!(output[2], "{\"d\":null}");
    assert!(
        output[3].starts_with("Error: e\n    at codemode.js:1"),
        "{}",
        output[3]
    );
    assert_eq!(output[4], "null");
    assert_eq!(output[5], "-Infinity");
    assert!(!result.output[0].console && result.output[1].console);
}

#[test]
fn calls_tools_and_records_them() {
    let (result, batches) = exec_with(
        "const source = await tools.read_file({ path: 'package.json' });\n\
         text('bytes ' + source.length);\n\
         return source.toUpperCase();",
        Vec::new(),
        &RunOptions::default(),
    );
    assert_eq!(
        result.outcome,
        Ok(Some("\"CONTENTS OF PACKAGE.JSON\"".to_owned()))
    );
    assert_eq!(texts(&result), vec!["bytes 24".to_owned()]);
    assert_eq!(batches.len(), 1);
    assert_eq!(
        batches[0][0].args.as_deref(),
        Some(r#"{"path":"package.json"}"#)
    );
    assert_eq!(result.calls.len(), 1);
    assert_eq!(result.calls[0].status, CallStatus::Ok);
}

#[test]
fn hands_promise_all_to_the_host_as_one_batch() {
    let (result, batches) = exec_with(
        "const paths = ['a', 'b', 'c'];\n\
         const all = await Promise.all(paths.map(path => tools.read_file({ path })));\n\
         return all.join('|');",
        Vec::new(),
        &RunOptions::default(),
    );
    assert_eq!(
        result.outcome,
        Ok(Some(
            "\"contents of a|contents of b|contents of c\"".to_owned()
        ))
    );
    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0].len(), 3);
}

#[test]
fn exposes_normalized_names_and_all_tools() {
    assert_eq!(value("return await tools.my_tool({ x: 1 })"), r#"{"x":1}"#);
    assert_eq!(value("return await tools['my-tool']([2])"), "[2]");
    assert_eq!(
        value("return ALL_TOOLS.map(t => t.name)"),
        r#"["read_file","my_tool","fail"]"#
    );
    assert_eq!(
        value("return 'read_file' in tools && !('nope' in tools)"),
        "true"
    );
    let error = failure("ALL_TOOLS.push(1)");
    assert!(error.message.contains("frozen"), "{error:?}");
}

#[test]
fn turns_tool_errors_into_catchable_errors() {
    assert_eq!(
        value(
            "try { await tools.fail({}) } catch (e) { return [e instanceof Error, e.message, e.tool] }"
        ),
        r#"[true,"this tool always fails","fail"]"#
    );
    let result = exec("await tools.fail({})");
    let error = result.outcome.clone().unwrap_err();
    assert_eq!(error.kind, ErrorKind::Script);
    assert_eq!(error.message, "this tool always fails");
    assert_eq!(result.calls[0].status, CallStatus::Error);
    assert_eq!(
        value(
            "const r = await Promise.allSettled([tools.fail({}), tools.read_file({path: 'x'})]); return r.map(x => x.status)"
        ),
        r#"["rejected","fulfilled"]"#
    );
}

#[test]
fn names_close_matches_for_a_tool_that_does_not_exist() {
    let error = failure("await tools.readFile({ path: 'x' })");
    assert!(
        error.message.contains("tools.readFile does not exist"),
        "{}",
        error.message
    );
    assert!(
        error.message.contains("Did you mean tools.read_file?"),
        "{}",
        error.message
    );
    let error = failure("await tools.zzz()");
    assert!(
        error
            .message
            .contains("Available: read_file, my_tool, fail."),
        "{}",
        error.message
    );
}

#[test]
fn does_not_run_calls_nothing_awaited() {
    let (result, batches) = exec_with(
        "tools.read_file({ path: 'never' }); return 1",
        Vec::new(),
        &RunOptions::default(),
    );
    assert!(batches.is_empty());
    assert_eq!(result.calls.len(), 1);
    assert_eq!(result.calls[0].status, CallStatus::Cancelled);
    assert!(
        render_result(&result, 1000).contains("never awaited"),
        "{}",
        render_result(&result, 1000)
    );
}

#[test]
fn reads_the_store_and_reports_writes() {
    let (result, _) = exec_with(
        "const runs = (load('runs') ?? 0) + 1; store('runs', runs); store('gone', undefined); const o = load('obj'); o.x = 9; return [runs, load('obj').x]",
        vec![
            ("runs".to_owned(), "2".to_owned()),
            ("obj".to_owned(), r#"{"x":1}"#.to_owned()),
            ("gone".to_owned(), "1".to_owned()),
        ],
        &RunOptions::default(),
    );
    assert_eq!(result.outcome, Ok(Some("[3,1]".to_owned())));
    assert_eq!(
        result.store_writes.set,
        vec![("runs".to_owned(), "3".to_owned())]
    );
    assert_eq!(result.store_writes.delete, vec!["gone".to_owned()]);
    let failed = exec("store('a', 1); throw new Error('x')");
    assert!(failed.store_writes.set.is_empty());
    let error = failure("store('big', 'x'.repeat(300000))");
    assert!(
        error.message.contains("more than the limit"),
        "{}",
        error.message
    );
    let error = failure("store(1, 2)");
    assert!(error.message.contains("key must be a string"));
}

#[test]
fn ends_successfully_on_exit_keeping_output_and_writes() {
    let result = exec("text('before'); store('k', true); exit(); text('after')");
    assert_eq!(result.outcome, Ok(None));
    assert_eq!(texts(&result), vec!["before".to_owned()]);
    assert_eq!(
        result.store_writes.set,
        vec![("k".to_owned(), "true".to_owned())]
    );
}

#[test]
fn reports_syntax_and_thrown_errors_with_the_line() {
    let error = failure("const a = 1;\nconst b = ;");
    assert_eq!(error.name.as_deref(), Some("SyntaxError"));
    assert!(
        error.stack.as_deref().unwrap().contains("codemode.js:2"),
        "{error:?}"
    );
    let error = failure("const a = 1;\n\nthrow new TypeError('bad ' + a)");
    assert_eq!(error.name.as_deref(), Some("TypeError"));
    assert_eq!(error.message, "bad 1");
    assert!(
        error.stack.as_deref().unwrap().contains("codemode.js:3"),
        "{error:?}"
    );
    let error = failure("throw 'plain'");
    assert_eq!(error.message, "plain");
    let error = failure("undefinedThing + 1");
    assert_eq!(error.message, "undefinedThing is not defined");
    let error = failure("const o = {}; o.f()");
    assert_eq!(error.message, "o.f is not a function");
    let error = failure("null.x");
    assert!(
        error
            .message
            .contains("Cannot read properties of null (reading 'x')")
    );
}

#[test]
fn runs_ordinary_javascript() {
    assert_eq!(
        value(
            "const words = 'the quick brown fox jumps over the lazy dog'.split(' ');\n\
             const counts = {};\n\
             for (const w of words) counts[w] = (counts[w] || 0) + 1;\n\
             const top = Object.entries(counts).sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0])).slice(0, 2);\n\
             return top;"
        ),
        r#"[["the",2],["brown",1]]"#
    );
    assert_eq!(
        value(
            "const { a, b: [c, ...d], ...rest } = { a: 1, b: [2, 3, 4], e: 5, f: 6 }; return [a, c, d, rest]"
        ),
        r#"[1,2,[3,4],{"e":5,"f":6}]"#
    );
    assert_eq!(
        value("let s = 0; for (let i = 0; i < 10; i++) { if (i % 2) continue; s += i } return s"),
        "20"
    );
    assert_eq!(
        value(
            "const fns = []; for (let i = 0; i < 3; i++) fns.push(() => i); return fns.map(f => f())"
        ),
        "[0,1,2]"
    );
    assert_eq!(
        value("function fib(n) { return n < 2 ? n : fib(n - 1) + fib(n - 2) } return fib(15)"),
        "610"
    );
    assert_eq!(
        value("const o = { n: 2, twice() { return this.n * 2 } }; return o.twice()"),
        "4"
    );
    assert_eq!(value("return `a${1 + 1}b${'c'.repeat(2)}`"), r#""a2bcc""#);
    assert_eq!(
        value("const x = null; return [x?.y?.z, x ?? 'd', (undefined)?.()]"),
        r#"[null,"d",null]"#
    );
    assert_eq!(
        value(
            "outer: for (const a of [1, 2]) { for (const b of [1, 2]) { if (b === 2) continue outer; if (a === 2) break outer; } } return 'ok'"
        ),
        r#""ok""#
    );
    assert_eq!(
        value("switch (3) { case 1: return 'a'; case 3: case 4: return 'b'; default: return 'c' }"),
        r#""b""#
    );
    assert_eq!(
        value(
            "let r = []; try { throw new RangeError('x') } catch ({ name }) { r.push(name) } finally { r.push('f') } return r"
        ),
        r#"["RangeError","f"]"#
    );
    assert_eq!(
        value(
            "const m = new Map([[1, 'a']]); m.set('k', 2); const s = new Set([1, 1, 2]); return [m.get(1), m.size, [...s], s.has(2)]"
        ),
        r#"["a",2,[1,2],true]"#
    );
    assert_eq!(
        value("return [typeof undeclared, typeof null, typeof (() => 1), typeof tools]"),
        r#"["undefined","object","function","object"]"#
    );
    assert_eq!(
        value("return [1, [2, [3, [4]]]].flat(Infinity)"),
        "[1,2,3,4]"
    );
}

#[test]
fn runs_built_ins_as_javascript_does() {
    assert_eq!(value("return [3, 1, 10, 2].sort()"), "[1,10,2,3]");
    assert_eq!(
        value("return Array.from({ length: 3 }, (_, i) => i * i)"),
        "[0,1,4]"
    );
    assert_eq!(
        value("return JSON.parse('{\"b\":1,\"a\":[true]}')"),
        r#"{"b":1,"a":[true]}"#
    );
    assert_eq!(
        value("return JSON.stringify({a: [1]}, null, 2)"),
        r#""{\n  \"a\": [\n    1\n  ]\n}""#
    );
    assert_eq!(
        value(
            "return [parseInt('42px'), parseFloat('3.5e2x'), Number('0x10'), (1234.5678).toFixed(2), (1234567.891).toLocaleString()]"
        ),
        r#"[42,350,16,"1234.57","1,234,567.891"]"#
    );
    assert_eq!(
        value("return [Math.max(1, 5, 3), Math.round(2.5), Math.round(-2.5), 7 % -3, 2 ** 10]"),
        "[5,3,-2,1,1024]"
    );
    assert_eq!(
        value("let a = 1, b = 2; [a, b] = [b, a]; return [a, b]"),
        "[2,1]"
    );
    assert_eq!(
        value("const o = {a: 1}; o.b ??= 2; o.a ||= 5; o.a += 1; return o"),
        r#"{"a":2,"b":2}"#
    );
    assert_eq!(
        value("return [...'héllo'].length + 'héllo'.slice(1, 3)"),
        r#""5él""#
    );
}

#[test]
fn runs_regular_expressions_as_javascript_does() {
    assert_eq!(
        value(r"return 'a1b22c333'.match(/\d+/g)"),
        r#"["1","22","333"]"#
    );
    assert_eq!(
        value(
            r"const m = 'key=value'.match(/(?<k>\w+)=(?<v>\w+)/); return [m[1], m.groups.v, m.index]"
        ),
        r#"["key","value",0]"#
    );
    assert_eq!(
        value(r"return 'John Smith'.replace(/(\w+)\s(\w+)/, '$2, $1')"),
        r#""Smith, John""#
    );
    assert_eq!(
        value(r"return 'a-b_c'.replace(/[-_]/g, m => m === '-' ? '+' : '*')"),
        r#""a+b*c""#
    );
    assert_eq!(
        value(r"return 'a, b,c'.split(/\s*,\s*/)"),
        r#"["a","b","c"]"#
    );
    assert_eq!(
        value(r"return [/^src\/.+\.rs$/.test('src/lib.rs'), /a/i.test('A'), 'x'.search(/y/)]"),
        "[true,true,-1]"
    );
    assert_eq!(
        value(r"return [...'a1b2'.matchAll(/[a-z](\d)/g)].map(m => m[1])"),
        r#"["1","2"]"#
    );
    assert_eq!(value(r"return 'aaa'.replaceAll('a', 'b')"), r#""bbb""#);
    assert_eq!(value(r"return /(?<=\$)\d+/.exec('cost $42')[0]"), r#""42""#);
    let error = failure("new RegExp('(')");
    assert_eq!(error.name.as_deref(), Some("SyntaxError"));
}

#[test]
fn handles_promises_without_timers() {
    assert_eq!(
        value("const p = new Promise(r => r(5)); return await p.then(x => x + 1)"),
        "6"
    );
    assert_eq!(
        value("return await Promise.reject(new Error('no')).catch(e => e.message)"),
        r#""no""#
    );
    assert_eq!(
        value(
            "async function f(x) { const t = await tools.read_file({ path: x }); return t.length } return await f('ab')"
        ),
        "14"
    );
    assert_eq!(
        value("const r = await Promise.race([tools.read_file({path: 'a'}), 2]); return r"),
        r#""contents of a""#
    );
    let error = failure("await new Promise(() => {})");
    assert!(error.message.contains("can never settle"), "{error:?}");
    let error = failure("await new Promise(r => setTimeout(r, 10))");
    assert_eq!(error.message, "setTimeout is not defined");
}

#[test]
fn has_no_host_capabilities() {
    assert_eq!(
        value(
            "return [typeof process, typeof require, typeof fetch, typeof setTimeout, typeof WebAssembly, typeof globalThis.process]"
        ),
        r#"["undefined","undefined","undefined","undefined","undefined","undefined"]"#
    );
    assert!(failure("import('fs')").message.contains("not available"));
    assert!(
        failure("class A {}")
            .message
            .contains("class is not supported")
    );
    assert!(failure("tools.read_file = 1").message.contains("read only"));
    assert!(failure("Math.PI = 3").message.contains("read only"));
}

#[test]
fn stops_a_spinning_script_at_its_deadline() {
    let options = RunOptions {
        timeout: Some(Duration::from_millis(100)),
        ..RunOptions::default()
    };
    for code in [
        "while (true) {}",
        "for (;;) { await null }",
        "let i = 0; do { i++ } while (true)",
    ] {
        let (result, _) = exec_with(code, Vec::new(), &options);
        assert_eq!(
            result.outcome.unwrap_err().kind,
            ErrorKind::Timeout,
            "{code}"
        );
    }
    // `catch` does not stop it.
    let (result, _) = exec_with(
        "try { while (true) {} } catch { text('caught') }",
        Vec::new(),
        &options,
    );
    assert_eq!(result.outcome.unwrap_err().kind, ErrorKind::Timeout);
    assert!(result.output.is_empty());
}

#[test]
fn fails_a_script_past_its_limits_even_when_it_catches() {
    let options = RunOptions {
        max_output_items: 10,
        ..RunOptions::default()
    };
    let (result, _) = exec_with(
        "for (let i = 0; i < 100; i++) { try { text(i) } catch {} }",
        Vec::new(),
        &options,
    );
    let error = result.outcome.unwrap_err();
    assert!(
        error.message.contains("script output exceeded"),
        "{error:?}"
    );
    assert_eq!(result.output.len(), 10);
    let options = RunOptions {
        memory_bytes: 1024 * 1024,
        ..RunOptions::default()
    };
    let (result, _) = exec_with(
        "const a = []; try { while (true) a.push('x'.repeat(1000)) } catch { return 'caught' }",
        Vec::new(),
        &options,
    );
    assert!(
        result
            .outcome
            .unwrap_err()
            .message
            .contains("out of memory")
    );
    let error = failure(
        "function f() { return f() } try { f() } catch (e) { throw new Error('caught ' + e.name) }",
    );
    assert_eq!(error.message, "caught RangeError");
}

#[test]
fn renders_the_result_for_the_model() {
    let result = exec("text('line'); return { n: 1 }");
    assert_eq!(render_result(&result, 1000), "line\n=> {\"n\":1}");
    let result = exec("text('x'.repeat(5000))");
    let rendered = render_result(&result, 100);
    assert!(rendered.contains("characters of output cut"), "{rendered}");
    assert!(rendered.chars().count() < 400);
    let result = exec("throw new Error('boom')");
    assert!(render_result(&result, 1000).starts_with("Script error: Error: boom"));
    assert_eq!(
        render_result(&exec(""), 100),
        "(the script printed nothing and returned undefined)"
    );
}

/// What the fresh review of the first version found, each now right.
#[test]
fn review_findings_hold() {
    // `for...of` reads the array as it goes: a queue grows while it is walked.
    assert_eq!(
        value(
            "const seen = []; const queue = [1]; for (const n of queue) { seen.push(n); if (n < 4) queue.push(n + 1) } return seen"
        ),
        "[1,2,3,4]"
    );
    // `toFixed` rounds a tie away from zero, and a zero has no sign.
    assert_eq!(
        value(
            "return [(12.5).toFixed(0), (0.125).toFixed(2), (1.005).toFixed(2), (-0).toFixed(2), (-1.5).toFixed(0), (9.995).toFixed(2), (999.5).toFixed(0), (2.5).toPrecision(1)]"
        ),
        r#"["13","0.13","1.00","0.00","-2","9.99","1000","3"]"#
    );
    // `a[i++]++` evaluates `i++` once.
    assert_eq!(
        value("const a = [0, 0, 0]; let i = 0; a[i++]++; return [a, i]"),
        "[[1,0,0],1]"
    );
    assert_eq!(
        value(
            "let calls = 0; const o = { n: 1 }; const get = () => { calls++; return o }; get().n++; return [o.n, calls]"
        ),
        "[2,1]"
    );
    // `var` is hoisted, and `var x;` keeps what `x` holds.
    assert_eq!(value("if (false) { var r = 1 } return r"), "undefined");
    assert_eq!(value("var x = 5; var x; return x"), "5");
    assert_eq!(
        value("function f() { if (true) { var y = 2 } return y } return f()"),
        "2"
    );
}

#[test]
fn large_texts_and_deep_values_stay_within_bounds() {
    // A regular expression over a large text shares it with every match.
    assert_eq!(
        value("const s = 'a\t'.repeat(200000); return s.replace(/\t/g, ' ').length"),
        "400000"
    );
    // Splitting a large text that is not ASCII is linear.
    let started = std::time::Instant::now();
    assert_eq!(
        value(r"return 'é line\n'.repeat(50000).split(/\n/).length"),
        "50001"
    );
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "{:?}",
        started.elapsed()
    );
    // A chain hundreds of thousands deep is freed without a stack overflow.
    assert_eq!(
        value("let h = null; for (let i = 0; i < 300000; i++) h = { next: h }; return 1"),
        "1"
    );
    // JSON past ten thousand levels is refused, not a hang.
    let error = failure(
        "let h = null; for (let i = 0; i < 20000; i++) h = { next: h }; return JSON.stringify(h).length",
    );
    assert!(error.message.contains("nested"), "{error:?}");
}

/// Found by the live suite: a file with CRLF line ends.
#[test]
fn lines_end_at_carriage_returns_as_in_javascript() {
    assert_eq!(
        value(
            r"const t = 'a: 1\r\nweight: 42\r\nb: 2\r\n'; return [t.match(/^weight: (\d+)$/m)[1], t.match(/^b.*$/m)[0], 'x\ry'.match(/x.y/)]"
        ),
        r#"["42","b: 2",null]"#
    );
    assert_eq!(value(r"return 'x\ry'.match(/x.y/s)[0].length"), "3");
}
