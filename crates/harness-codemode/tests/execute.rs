//! The script on its own thread, its calls answered by async code: the
//! deadline and cancellation reach a script that waits on a tool and one
//! that computes.

use std::time::Duration;

use harness_codemode::{ErrorKind, HostCall, RunOptions, ToolInfo, execute};

fn tools() -> Vec<ToolInfo> {
    vec![
        ToolInfo::new("slow", "Answers after a while"),
        ToolInfo::new("echo", "Answers at once"),
    ]
}

async fn answer(calls: Vec<HostCall>) -> Vec<Result<String, String>> {
    let mut answers = Vec::new();
    for call in calls {
        if call.name == "slow" {
            tokio::time::sleep(Duration::from_secs(30)).await;
        }
        answers.push(Ok(call.args.unwrap_or_else(|| "null".to_owned())));
    }
    answers
}

fn options(timeout_ms: u64) -> RunOptions {
    RunOptions {
        timeout: Some(Duration::from_millis(timeout_ms)),
        ..RunOptions::default()
    }
}

#[tokio::test]
async fn answers_calls_from_async_code() {
    let result = execute(
        "const [a, b] = await Promise.all([tools.echo(1), tools.echo('x')]); text(a); return b"
            .to_owned(),
        tools(),
        Vec::new(),
        options(10_000),
        answer,
        std::future::pending(),
    )
    .await;
    assert_eq!(result.outcome, Ok(Some("\"x\"".to_owned())));
    assert_eq!(result.output[0].text, "1");
}

#[tokio::test]
async fn times_out_while_a_tool_runs() {
    let started = std::time::Instant::now();
    let result = execute(
        "text('before'); await tools.slow({}); text('after')".to_owned(),
        tools(),
        Vec::new(),
        options(200),
        answer,
        std::future::pending(),
    )
    .await;
    assert_eq!(result.outcome.unwrap_err().kind, ErrorKind::Timeout);
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(result.output.len(), 1);
}

#[tokio::test]
async fn times_out_a_script_that_computes() {
    let result = execute(
        "while (true) {}".to_owned(),
        tools(),
        Vec::new(),
        options(200),
        answer,
        std::future::pending(),
    )
    .await;
    assert_eq!(result.outcome.unwrap_err().kind, ErrorKind::Timeout);
}

#[tokio::test]
async fn stops_when_the_caller_cancels() {
    let result = execute(
        "await tools.slow({})".to_owned(),
        tools(),
        Vec::new(),
        options(60_000),
        answer,
        tokio::time::sleep(Duration::from_millis(100)),
    )
    .await;
    let error = result.outcome.unwrap_err();
    assert_eq!(error.kind, ErrorKind::Aborted);
    assert!(result.store_writes.set.is_empty());
}
