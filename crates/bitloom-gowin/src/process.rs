use std::future::pending;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use bitloom_core::Cancellation;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::Command;
use tokio::task::JoinHandle;

use crate::{ProcessObserver, ProcessStream, ToolError};

static NEXT_INVOCATION: AtomicU64 = AtomicU64::new(1);

pub(crate) async fn run_tool(
    command: Command,
    tool: &'static str,
    scope: Arc<str>,
    observer: Arc<dyn ProcessObserver>,
) -> Result<(), ToolError> {
    run_tool_inner(command, tool, scope, observer, None).await
}

pub(crate) async fn run_tool_cancellable(
    command: Command,
    tool: &'static str,
    cancellation: Cancellation,
    scope: Arc<str>,
    observer: Arc<dyn ProcessObserver>,
) -> Result<(), ToolError> {
    if cancellation.is_cancelled() {
        return Err(ToolError::Cancelled { tool });
    }
    run_tool_inner(command, tool, scope, observer, Some(cancellation)).await
}

async fn run_tool_inner(
    mut command: Command,
    tool: &'static str,
    scope: Arc<str>,
    observer: Arc<dyn ProcessObserver>,
    cancellation: Option<Cancellation>,
) -> Result<(), ToolError> {
    let invocation = NEXT_INVOCATION.fetch_add(1, Ordering::Relaxed);
    observer
        .command(&scope, invocation, tool, command.as_std())
        .map_err(|error| ToolError::Output { tool, message: error.to_string() })?;
    command.kill_on_drop(true).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|source| ToolError::Start { tool, source })?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| ToolError::Output { tool, message: "stdout pipe is unavailable".into() })?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| ToolError::Output { tool, message: "stderr pipe is unavailable".into() })?;
    let observer_for_status = observer.clone();
    let scope_for_status = scope.clone();
    let stdout = tokio::spawn(read_pipe(
        stdout,
        observer.clone(),
        scope.clone(),
        invocation,
        tool,
        ProcessStream::Stdout,
    ));
    let stderr =
        tokio::spawn(read_pipe(stderr, observer, scope, invocation, tool, ProcessStream::Stderr));

    let status = tokio::select! {
        status = child.wait() => {
            Some(status.map_err(|source| ToolError::Wait { tool, source })?)
        }
        () = cancelled(cancellation) => {
            child
                .kill()
                .await
                .map_err(|source| ToolError::Terminate { tool, source })?;
            None
        }
    };

    let Some(status) = status else {
        drain_after_cancellation(stdout).await;
        drain_after_cancellation(stderr).await;
        observer_error_on_cancelled(&*observer_for_status, &scope_for_status, invocation, tool)?;
        return Err(ToolError::Cancelled { tool });
    };

    let _stdout = collect_output(stdout, tool).await?;
    let stderr = collect_output(stderr, tool).await?;
    observer_for_status
        .finished(&scope_for_status, invocation, tool, &status)
        .map_err(|error| ToolError::Output { tool, message: error.to_string() })?;
    if status.success() {
        return Ok(());
    }
    let stderr = decode_output(&stderr).trim().to_owned();
    Err(ToolError::Exit { tool, status, stderr })
}

fn observer_error_on_cancelled(
    observer: &dyn ProcessObserver,
    scope: &str,
    invocation: u64,
    tool: &'static str,
) -> Result<(), ToolError> {
    observer
        .cancelled(scope, invocation, tool)
        .map_err(|error| ToolError::Output { tool, message: error.to_string() })
}

async fn cancelled(cancellation: Option<Cancellation>) {
    match cancellation {
        Some(cancellation) => cancellation.cancelled().await,
        None => pending().await,
    }
}

async fn read_pipe(
    pipe: impl AsyncRead + Unpin,
    observer: Arc<dyn ProcessObserver>,
    scope: Arc<str>,
    invocation: u64,
    tool: &'static str,
    stream: ProcessStream,
) -> std::io::Result<Vec<u8>> {
    let mut reader = BufReader::new(pipe);
    let mut output = Vec::new();
    let mut line = Vec::new();
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line).await? == 0 {
            break;
        }
        output.extend_from_slice(&line);
        let message = decode_output(line.trim_ascii_end());
        observer.output(&scope, invocation, tool, stream, &message)?;
    }
    Ok(output)
}

fn decode_output(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

async fn collect_output<R>(
    output: JoinHandle<std::io::Result<R>>,
    tool: &'static str,
) -> Result<R, ToolError> {
    output
        .await
        .map_err(|error| ToolError::Output { tool, message: error.to_string() })?
        .map_err(|error| ToolError::Output { tool, message: error.to_string() })
}

async fn drain_after_cancellation(output: JoinHandle<std::io::Result<Vec<u8>>>) {
    let _ = output.await;
}

pub(crate) fn quote_tool_path(path: &Path) -> String {
    format!("\"{}\"", tool_path(path).replace('"', "\\\""))
}

pub(crate) fn tool_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::time::Duration;

    use bitloom_core::RunContext;

    use super::*;

    const CHILD_PROCESS: &str = "BITLOOM_CANCELLATION_TEST_CHILD";

    #[derive(Default)]
    struct RecordingObserver {
        commands: Mutex<Vec<String>>,
        output: Mutex<Vec<(ProcessStream, String)>>,
    }

    impl ProcessObserver for RecordingObserver {
        fn command(
            &self,
            scope: &str,
            invocation: u64,
            tool: &str,
            _command: &std::process::Command,
        ) -> std::io::Result<()> {
            self.commands.lock().unwrap().push(format!("{invocation}:{scope}/{tool}"));
            Ok(())
        }

        fn output(
            &self,
            _scope: &str,
            _invocation: u64,
            _tool: &str,
            stream: ProcessStream,
            line: &str,
        ) -> std::io::Result<()> {
            self.output.lock().unwrap().push((stream, line.to_owned()));
            Ok(())
        }
    }

    #[tokio::test]
    async fn cancellation_terminates_and_reaps_the_tool() {
        if let Some(mode) = std::env::var_os(CHILD_PROCESS) {
            if mode == "output" {
                println!("tool stdout");
                eprintln!("tool stderr");
            } else {
                std::thread::sleep(Duration::from_secs(60));
            }
            return;
        }

        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .arg("--exact")
            .arg("process::tests::cancellation_terminates_and_reaps_the_tool")
            .arg("--nocapture")
            .env(CHILD_PROCESS, "sleep");
        let run = RunContext::default();
        let cancellation = run.cancellation();
        let tool = run_tool_cancellable(
            command,
            "test tool",
            cancellation,
            Arc::from("test"),
            Arc::new(()),
        );
        tokio::pin!(tool);

        tokio::time::sleep(Duration::from_millis(100)).await;
        run.cancel();
        let error = tokio::time::timeout(Duration::from_secs(5), tool).await.unwrap().unwrap_err();

        assert!(matches!(error, ToolError::Cancelled { tool: "test tool" }));
    }

    #[tokio::test]
    async fn observer_receives_scoped_commands_and_both_output_streams() {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .arg("--exact")
            .arg("process::tests::cancellation_terminates_and_reaps_the_tool")
            .arg("--nocapture")
            .env(CHILD_PROCESS, "output");
        let observer = Arc::new(RecordingObserver::default());

        run_tool(command, "test tool", Arc::from("top"), observer.clone()).await.unwrap();

        let commands = observer.commands.lock().unwrap();
        assert_eq!(commands.len(), 1);
        assert!(commands[0].ends_with(":top/test tool"));
        let output = observer.output.lock().unwrap();
        assert!(output.iter().any(|line| line == &(ProcessStream::Stdout, "tool stdout".into())));
        assert!(output.iter().any(|line| line == &(ProcessStream::Stderr, "tool stderr".into())));
    }

    #[test]
    fn decodes_utf8_and_replaces_invalid_bytes() {
        assert_eq!(decode_output("схема готова".as_bytes()), "схема готова");
        assert_eq!(decode_output(&[b'a', 0xff, b'b']), "a�b");
    }
}
