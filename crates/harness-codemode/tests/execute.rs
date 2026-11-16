//! The script on its own thread, its calls answered by async code: the
//! deadline and cancellation reach a script that waits on a tool and one
//! that computes, and a call running when the run stops is told to stop and
//! awaited, never dropped half done.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use harness_codemode::{ErrorKind, HostCall, RunOptions, ToolInfo, execute};
use tokio::sync::Notify;

fn tools() -> Vec<ToolInfo> {
    vec![
        ToolInfo::new("slow", "Answers after a while, or when it is stopped"),
        ToolInfo::new("echo", "Answers at once"),
    ]
}

/// The host: `slow` waits 30 s unless `stop` is notified; what it saw.
#[derive(Default)]
struct Host {
    stop: Notify,
    stopped: AtomicBool,
    finished: AtomicBool,
}

impl Host {
    async fn answer(&self, calls: Vec<HostCall>) -> Vec<Result<String, String>> {
        let mut answers = Vec::new();
        for call in calls {
            if call.name == "slow" {
                tokio::select! {
                    () = tokio::time::sleep(Duration::from_secs(30)) => {}
                    () = self.stop.notified() => self.stopped.store(true, Ordering::SeqCst),
                }
                // The call settles (its receipt, in ha) after the stop.
                self.finished.store(true, Ordering::SeqCst);
            }
            answers.push(Ok(call.args.unwrap_or_else(|| "null".to_owned())));
        }
        answers
    }
}

fn options(timeout_ms: u64) -> RunOptions {
    RunOptions {
        timeout: Some(Duration::from_millis(timeout_ms)),
        ..RunOptions::default()
    }
}

async fn exec(
    code: &str,
    host: &Arc<Host>,
    timeout_ms: u64,
    cancelled: impl std::future::Future<Output = ()>,
) -> harness_codemode::CodemodeResult {
    execute(
        code.to_owned(),
        tools(),
        Vec::new(),
        options(timeout_ms),
        |calls| {
            let host = Arc::clone(host);
            async move { host.answer(calls).await }
        },
        cancelled,
        || host.stop.notify_waiters(),
    )
    .await
}

#[tokio::test]
async fn answers_calls_from_async_code() {
    let host = Arc::new(Host::default());
    let result = exec(
        "const [a, b] = await Promise.all([tools.echo(1), tools.echo('x')]); text(a); return b",
        &host,
        10_000,
        std::future::pending(),
    )
    .await;
    assert_eq!(result.outcome, Ok(Some("\"x\"".to_owned())));
    assert_eq!(result.output[0].text, "1");
}

#[tokio::test]
async fn times_out_while_a_tool_runs_and_lets_the_tool_settle() {
    let host = Arc::new(Host::default());
    let started = std::time::Instant::now();
    let result = exec(
        "text('before'); await tools.slow({}); text('after')",
        &host,
        200,
        std::future::pending(),
    )
    .await;
    assert_eq!(result.outcome.unwrap_err().kind, ErrorKind::Timeout);
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(result.output.len(), 1);
    assert!(
        host.stopped.load(Ordering::SeqCst),
        "the running call was told to stop"
    );
    assert!(
        host.finished.load(Ordering::SeqCst),
        "the running call settled before the result"
    );
}

#[tokio::test]
async fn times_out_a_script_that_computes() {
    let host = Arc::new(Host::default());
    let result = exec("while (true) {}", &host, 200, std::future::pending()).await;
    assert_eq!(result.outcome.unwrap_err().kind, ErrorKind::Timeout);
}

#[tokio::test]
async fn stops_when_the_caller_cancels() {
    let host = Arc::new(Host::default());
    let result = exec(
        "await tools.slow({})",
        &host,
        60_000,
        tokio::time::sleep(Duration::from_millis(100)),
    )
    .await;
    let error = result.outcome.unwrap_err();
    assert_eq!(error.kind, ErrorKind::Aborted);
    assert!(result.store_writes.set.is_empty());
    assert!(host.finished.load(Ordering::SeqCst));
}
