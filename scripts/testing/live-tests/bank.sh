#!/usr/bin/env bash
# The action bank (HarnessMM's guarded decision reuse,
# docs/architecture/ACTION_BANK.en.md), with a real model, in `ha exec`:
#
#   1. With HA_ACTION_BANK=on, a coding task (write a module and its test
#      file, run the tests): the model authors a bank, at least one call runs
#      without a model step (a `bank/dispatched` certificate, the JSON's
#      `bank.dispatched`), fewer model calls than steps plus bypasses would
#      be, the tests pass when run by the suite, and the conversation resumes
#      (`ha exec --resume`) with a follow-up that needs the earlier result.
#   2. A guard that does not hold returns to the model: a test that fails
#      leaves the banked commit after it unrun (`guard_false`), and nothing is
#      committed.
#   3. In `--approval auto-edit` a banked command still goes through the
#      approval gate: nobody can answer in `ha exec`, so it is refused, and
#      the model is called.
#   4. An A/B: three small coding tasks with the bank off and on, REPEATS (2)
#      times each, from one snapshot of the project: success, model calls,
#      input / output tokens and bypasses, in $RX/bank-ab.md. Reported, not
#      judged.
#
# A precondition the model must meet (it authored a bank of the shape the
# check needs) is retried up to three times and named when it never held.
# CONTROL=<an older ha.exe> runs check 1 on that binary as well: 0.9.3 has no
# bank, so HA_ACTION_BANK=on offers no bank tool and dispatches nothing.
#
#   REPEATS=2 AB=1 bash scripts/testing/live-tests/bank.sh
set -u
source "$(dirname "$0")/env.sh"
fresh bank
export HA_DAEMON=off HA_AUTO_REFINE=off HA_SELF_EVAL=off HA_SESSION_LOG=off
export HA_PYTHON=${HA_PYTHON:-$PY}
# The fixture's tests run on PY; the agent is told its full path.
PYW=$("$PY" -c "import sys; print(sys.executable.replace(chr(92), '/'))")
export PATH="$(dirname "$("$PY" -c "import sys; print(sys.executable)")"):$PATH"
D="$HA_HOME/data"
REPEATS=${REPEATS:-2}
AB=${AB:-1}
TESTS="\"$PYW\" -m unittest discover -s tests -v"

now_ms() { "$PY" -c "import time; print(int(time.time()*1000))"; }
# Records of one kind and event since a time, one JSON per line.
records() {
  cat "$D"/diagnostics/*.jsonl 2>/dev/null | "$PY" -c "
import json, sys
kind, event, since = sys.argv[1], sys.argv[2], int(sys.argv[3])
for line in sys.stdin:
    try:
        r = json.loads(line)
    except ValueError:
        continue
    if r.get('kind') == kind and r.get('event') == event and r.get('ms', 0) >= since:
        print(json.dumps(r, ensure_ascii=False))
" "$1" "$2" "$3"
}
count() { records "$@" | wc -l | tr -d ' '; }
# A field of a run's JSON by a dotted path: `jget <file> bank.dispatched`.
jget() {
  "$PY" -c "
import json, sys
try:
    v = json.load(open(sys.argv[1], encoding='utf-8'))
    for k in sys.argv[2].split('.'):
        v = v.get(k) if isinstance(v, dict) else None
except Exception:
    v = None
print('' if v is None else v)" "$1" "$2"
}
# One `ha exec` run in <dir>: `run <dir> <name> <prompt> [ha exec options...]`.
run() {
  local dir="$1" name="$2" prompt="$3"; shift 3
  printf '\n## %s (ha exec %s)\n\n> %s\n\n' "$(date +%H:%M:%S)" "$name" "$prompt" >> "$LOG"
  (cd "$dir" && "${HAX:-$HA}" exec --model "$MODEL" "${TH[@]}" --output-format json "$@" "$prompt" \
    > "$RX/$name.json" 2> "$RX/$name.err")
  jget "$RX/$name.json" response >> "$LOG"
}
# The fixture: a small Python package with a test folder.
fixture() {
  project "$1"
  mkdir -p "$1/app" "$1/tests"
  : > "$1/app/__init__.py"
  : > "$1/tests/__init__.py"
  (cd "$1" && git add -A && git -c user.name=t -c user.email=t@t commit -qm fixture)
}

# --- 1. A banked call runs without a model step, and the conversation resumes.
P1="$RX/p1"
fixture "$P1"
TASK1="Create app/stats.py with a function median(values) that returns the median of a non-empty list of numbers (the mean of the two middle values for an even count), and tests/test_stats.py with unittest tests for an odd count, an even count and an unsorted list. Then run the tests with: $TESTS . You already know every call you will make: write both files and, in the same response, queue the test run with action_bank (after root, when ok)."
held=0
for attempt in 1 2 3; do
  since=$(now_ms)
  HA_ACTION_BANK=on run "$P1" "task1-$attempt" "$TASK1" --approval full-auto
  if [ "$(count bank authored "$since")" -ge 1 ]; then held=1; T1="$RX/task1-$attempt.json"; S1=$since; break; fi
  (cd "$P1" && git reset -q --hard && git clean -qfd)
done
check "precondition: the model authored a bank (attempts: $attempt)" '[ "$held" = 1 ]'
if [ "$held" = 1 ]; then
  dispatched=$(jget "$T1" bank.dispatched)
  steps=$(jget "$T1" steps)
  calls=$(jget "$T1" usage.model_calls)
  check "a banked call ran without a model step (bank.dispatched=$dispatched, model_steps_saved=$(jget "$T1" bank.model_steps_saved))" '[ "${dispatched:-0}" -ge 1 ] && [ "$(count bank dispatched "$S1")" -ge 1 ]'
  check "fewer model calls ($calls) than steps plus bypasses would be ($steps + $dispatched)" '[ -n "$calls" ] && [ "$calls" -lt $(( ${steps:-0} + ${dispatched:-0} )) ]'
  certified=$(records bank dispatched "$S1" | "$PY" -c '
import json, sys
keys = ("graph", "node", "predecessor_receipt", "args_hash", "tool_receipt", "tool_receipt_hash", "replaced_step")
lines = [json.loads(l)["fields"] for l in sys.stdin if l.strip()]
print(bool(lines) and all(all(f.get(k) not in (None, "") for k in keys) for f in lines))')
  check "each certificate binds the graph, node, args, receipt and the step it replaced" '[ "$certified" = True ]'
  check "the task succeeded: the tests pass when the suite runs them" '(cd "$P1" && "$PY" -m unittest discover -s tests >/dev/null 2>&1) && [ -f "$P1/app/stats.py" ]'
  check "the bank tally is in the JSON (authored >= 1, an authoring estimate)" '[ "$(jget "$T1" bank.authored)" -ge 1 ] && [ "$(jget "$T1" bank.author_tokens_estimate)" -gt 0 ]'
  session=$(jget "$T1" session_id)
  HA_ACTION_BANK=on run "$P1" resume "Without running anything again: how many tests ran in the test run you did before, and did they pass? Answer in one line, with the number." --approval full-auto --resume "$session"
  check "the conversation resumes and the follow-up uses the banked test run's result" '[ "$(jget "$RX/resume.json" stop)" = final ] && jget "$RX/resume.json" response | grep -Eq "\b3\b|three"'
  check "a replayed banked call is no orphaned receipt (the resumed run did not fail on its transcript)" '! grep -qi "announced\|before it was announced\|answered twice" "$RX/resume.err"'
fi

# --- 2. A guard that does not hold returns to the model.
P2="$RX/p2"
fixture "$P2"
printf 'def total(items):\n    return sum(amount for _, amount in items) + 1\n' > "$P2/app/calc.py"
(cd "$P2" && git add -A && git -c user.name=t -c user.email=t@t commit -qm calc)
TASK2="Write tests/test_calc.py with one unittest test: total([('a', 2), ('b', 3)]) from app.calc equals 5. Do not change app/calc.py. In the same response, queue with action_bank: node 'test' after root when ok runs the tests with run_shell: $TESTS ; node 'commit' after 'test' when ok runs run_shell: git add -A && git -c user.name=t -c user.email=t@t commit -m tests . Then report whether the tests passed."
held=0
for attempt in 1 2 3; do
  since=$(now_ms)
  HA_ACTION_BANK=on run "$P2" "guard-$attempt" "$TASK2" --approval full-auto
  edges=$(records bank authored "$since" | "$PY" -c "import json,sys; print(' '.join(json.loads(l)['fields'].get('edges','') for l in sys.stdin if l.strip()))")
  if printf '%s' "$edges" | grep -q "<-root" && [ "$(records bank authored "$since" | "$PY" -c "import json,sys; print(max([json.loads(l)['fields'].get('nodes',0) for l in sys.stdin if l.strip()] or [0]))")" -ge 2 ]; then
    held=1; S2=$since; T2="$RX/guard-$attempt.json"; break
  fi
  (cd "$P2" && git reset -q --hard && git clean -qfd)
done
check "precondition: a chain of two banked nodes was authored (attempts: $attempt)" '[ "$held" = 1 ]'
if [ "$held" = 1 ]; then
  check "the failing test run ran from the bank (a certificate with status failed)" '[ "$(records bank dispatched "$S2" | grep -cE "\"status\": *\"failed\"")" -ge 1 ]'
  check "the commit after it was dropped: its guard did not hold" '[ "$(records bank discarded "$S2" | grep -c "guard_false")" -ge 1 ]'
  check "nothing was committed" '[ "$(cd "$P2" && git rev-list --count HEAD)" = 2 ]'
  check "the model was called after the failure and answered" '[ "$(jget "$T2" stop)" = final ] && [ "$(jget "$T2" steps)" -ge 2 ]'
fi

# --- 3. A banked command still meets the approval gate.
P3="$RX/p3"
fixture "$P3"
TASK3="Write app/hello.py containing def hello(): return 'hi', and in the same response queue with action_bank: node 'stage' after root when ok runs run_shell: git add app/hello.py . Then say whether the file was staged."
held=0
for attempt in 1 2 3; do
  since=$(now_ms)
  HA_ACTION_BANK=on run "$P3" "ask-$attempt" "$TASK3" --approval auto-edit
  if [ "$(count bank dispatched "$since")" -ge 1 ]; then held=1; S3=$since; T3="$RX/ask-$attempt.json"; break; fi
  (cd "$P3" && git reset -q --hard && git clean -qfd)
done
check "precondition: a banked command was dispatched (attempts: $attempt)" '[ "$held" = 1 ]'
if [ "$held" = 1 ]; then
  check "in auto-edit the banked command was refused by the approval gate, not run" '[ "$(records bank dispatched "$S3" | grep -cE "\"status\": *\"unclear\"")" -ge 1 ]'
  check "the model was called after the refusal" '[ "$(jget "$T3" steps)" -ge 2 ]'
fi

# --- Control: an older ha with HA_ACTION_BANK=on has no bank.
if [ -n "${CONTROL:-}" ]; then
  PC="$RX/pc"
  fixture "$PC"
  since=$(now_ms)
  HAX="$CONTROL" HA_ACTION_BANK=on run "$PC" control "$TASK1" --approval full-auto
  check "control ($CONTROL): no bank object, nothing authored or dispatched" '[ -z "$(jget "$RX/control.json" bank.enabled)" ] && [ "$(count bank dispatched "$since")" = 0 ]'
fi

# --- 4. A/B: the bank off against on.
if [ "$AB" = 1 ]; then
  BASE="$RX/ab-base"
  fixture "$BASE"
  printf 'def parse_pairs(text):\n    """Pairs from "a=1,b=2"."""\n    return [tuple(part.split("=")) for part in text.split(",")]\n' > "$BASE/app/pairs.py"
  (cd "$BASE" && git add -A && git -c user.name=t -c user.email=t@t commit -qm pairs)
  # Neutral prompts: the same for both modes; the bank is only offered.
  ab_prompt() {
    case "$1" in
      slug) echo "Create app/slug.py with slugify(text): lowercase, runs of non-alphanumeric characters become one '-', no leading or trailing '-'. Add tests/test_slug.py with three unittest tests, and run the tests with: $TESTS" ;;
      pairs) echo "app/pairs.py's parse_pairs returns strings; make it return (name, int) pairs and ignore empty parts. Add tests/test_pairs.py with unittest tests for both, and run the tests with: $TESTS" ;;
      roman) echo "Create app/roman.py with to_roman(n) for 1..3999. Add tests/test_roman.py with unittest tests for 4, 9, 14, 1994 and 3999, and run the tests with: $TESTS" ;;
    esac
  }
  ab_check() {
    case "$1" in
      slug) echo "from app.slug import slugify; assert slugify('  Hello, World!! ') == 'hello-world'" ;;
      pairs) echo "from app.pairs import parse_pairs; assert parse_pairs('a=1,,b=2') == [('a', 1), ('b', 2)]" ;;
      roman) echo "from app.roman import to_roman; assert to_roman(1994) == 'MCMXCIV' and to_roman(3999) == 'MMMCMXCIX'" ;;
    esac
  }
  : > "$RX/ab.tsv"
  for repeat in $(seq 1 "$REPEATS"); do
    for task in slug pairs roman; do
      for mode in off on; do
        dir="$RX/ab-$task-$mode-$repeat"
        rm -rf "$dir"; cp -r "$BASE" "$dir"
        HA_ACTION_BANK=$mode run "$dir" "ab-$task-$mode-$repeat" "$(ab_prompt "$task")" --approval full-auto
        ok=0
        (cd "$dir" && "$PY" -c "$(ab_check "$task")" >/dev/null 2>&1 && "$PY" -m unittest discover -s tests >/dev/null 2>&1) && ok=1
        j="$RX/ab-$task-$mode-$repeat.json"
        printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$mode" "$task" "$repeat" "$ok" \
          "$(jget "$j" usage.model_calls)" "$(jget "$j" usage.input_tokens)" "$(jget "$j" usage.output_tokens)" \
          "$(jget "$j" bank.dispatched)" "$(jget "$j" bank.authored)" >> "$RX/ab.tsv"
      done
    done
  done
  "$PY" - "$RX/ab.tsv" "$RX/bank-ab.md" "$MODEL" "${THINKING:-}" <<'EOF'
import sys
rows = [line.rstrip("\n").split("\t") for line in open(sys.argv[1], encoding="utf-8") if line.strip()]
num = lambda v: float(v) if v not in ("", None) else 0.0
out = [f"# Action bank A/B ({sys.argv[3]}, thinking {sys.argv[4] or 'default'})", "",
       "| mode | runs | success | model calls (mean) | input tokens (mean) | output tokens (mean) | banks (mean) | bypasses (mean) |",
       "|---|---|---|---|---|---|---|---|"]
summary = {}
for mode in ("off", "on"):
    sel = [r for r in rows if r[0] == mode]
    if not sel:
        continue
    n = len(sel)
    mean = lambda i: sum(num(r[i]) for r in sel) / n
    summary[mode] = (mean(4), mean(5), mean(6))
    out.append(f"| {mode} | {n} | {sum(int(r[3]) for r in sel)}/{n} | {mean(4):.2f} | {mean(5):.0f} | {mean(6):.0f} | {mean(8):.2f} | {mean(7):.2f} |")
if "off" in summary and "on" in summary:
    off, on = summary["off"], summary["on"]
    pct = lambda a, b: (b - a) / a * 100 if a else 0.0
    out += ["", f"On against off: model calls {pct(off[0], on[0]):+.1f}%, input tokens {pct(off[1], on[1]):+.1f}%, output tokens {pct(off[2], on[2]):+.1f}%."]
out += ["", "| mode | task | repeat | ok | model calls | input | output | bypasses | banks |", "|---|---|---|---|---|---|---|---|---|"]
for r in rows:
    out.append("| " + " | ".join([r[0], r[1], r[2], r[3], r[4], r[5], r[6], r[7] or "-", r[8] or "-"]) + " |")
open(sys.argv[2], "w", encoding="utf-8").write("\n".join(out) + "\n")
print("\n".join(out))
EOF
  check "the A/B report has both modes and every run reported its usage" '[ "$(grep -c "^| off \|^| on " "$RX/bank-ab.md")" -ge 2 ] && ! awk -F"\t" "\$5==\"\" || \$5==0 {bad=1} END{exit !bad}" "$RX/ab.tsv"'
  check "with the bank off no run authored or dispatched anything" '! awk -F"\t" "\$1==\"off\" && (\$8!=\"\" || \$9!=\"\") {bad=1} END{exit !bad}" "$RX/ab.tsv"'
fi

result
