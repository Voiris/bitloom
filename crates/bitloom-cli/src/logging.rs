use std::ffi::OsStr;
use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{Context, Result};
use bitloom_gowin::{ProcessObserver, ProcessStream, Toolchain};

use crate::cli::OutputArgs;
use crate::output;

pub const LOG_FILE_NAME: &str = "bitloom.log";

pub struct SessionLogger {
    sender: mpsc::Sender<LogMessage>,
    writer: Mutex<Option<JoinHandle<io::Result<()>>>>,
    live_tools: bool,
    live_commands: bool,
}

enum LogMessage {
    Line(String),
    Shutdown,
}

impl SessionLogger {
    fn initialize(project_root: &Path, options: OutputArgs) -> Result<(Arc<Self>, PathBuf)> {
        let build = project_root.join("build");
        std::fs::create_dir_all(&build)
            .with_context(|| format!("failed to create log directory {}", build.display()))?;
        let path = build.join(LOG_FILE_NAME);
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&path)
            .with_context(|| format!("failed to open tool log {}", path.display()))?;
        let (sender, receiver) = mpsc::channel();
        let writer = thread::Builder::new()
            .name("bitloom-log-writer".into())
            .spawn(move || write_log(file, receiver))
            .with_context(|| format!("failed to start tool log writer for {}", path.display()))?;
        sender
            .send(LogMessage::Line("INFO    [bitloom] started bitloom session".into()))
            .with_context(|| format!("failed to initialize tool log {}", path.display()))?;
        Ok((
            Arc::new(Self {
                sender,
                writer: Mutex::new(Some(writer)),
                live_tools: options.verbose || !io::stdout().is_terminal(),
                live_commands: options.advanced,
            }),
            path,
        ))
    }

    fn write(&self, level: &str, source: &str, message: &str) -> io::Result<()> {
        self.sender
            .send(LogMessage::Line(format!("{level:<7} [{source}] {message}")))
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "tool log writer stopped"))
    }
}

impl Drop for SessionLogger {
    fn drop(&mut self) {
        let _ = self.sender.send(LogMessage::Shutdown);
        let writer = self.writer.get_mut().unwrap_or_else(|poisoned| poisoned.into_inner()).take();
        if let Some(writer) = writer {
            let _ = writer.join();
        }
    }
}

fn write_log(file: File, receiver: mpsc::Receiver<LogMessage>) -> io::Result<()> {
    let mut file = BufWriter::new(file);
    loop {
        match receiver.recv_timeout(Duration::from_millis(100)) {
            Ok(LogMessage::Line(line)) => writeln!(file, "{line}")?,
            Ok(LogMessage::Shutdown) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                return file.flush();
            }
            Err(mpsc::RecvTimeoutError::Timeout) => file.flush()?,
        }
    }
}

impl ProcessObserver for SessionLogger {
    fn command(
        &self,
        scope: &str,
        invocation: u64,
        tool: &str,
        command: &Command,
    ) -> io::Result<()> {
        let source = source_name(scope, tool, invocation);
        let command = format_command(command);
        self.write("COMMAND", &source, &format!("$ {command}"))?;
        if self.live_commands {
            output::tool(&source, format_args!("$ {command}"), false);
        }
        Ok(())
    }

    fn output(
        &self,
        scope: &str,
        invocation: u64,
        tool: &str,
        stream: ProcessStream,
        line: &str,
    ) -> io::Result<()> {
        let source = source_name(scope, tool, invocation);
        let (level, is_stderr) = match stream {
            ProcessStream::Stdout => ("STDOUT", false),
            ProcessStream::Stderr => ("STDERR", true),
        };
        self.write(level, &source, line)?;
        if self.live_tools {
            output::tool(&source, line, is_stderr);
        }
        Ok(())
    }

    fn finished(
        &self,
        scope: &str,
        invocation: u64,
        tool: &str,
        status: &std::process::ExitStatus,
    ) -> io::Result<()> {
        self.write(
            "STATUS",
            &source_name(scope, tool, invocation),
            &format!("exited with {status}"),
        )
    }

    fn cancelled(&self, scope: &str, invocation: u64, tool: &str) -> io::Result<()> {
        self.write("STATUS", &source_name(scope, tool, invocation), "cancelled")
    }
}

pub fn attach(toolchain: Toolchain, project_root: &Path, options: OutputArgs) -> Result<Toolchain> {
    let (logger, path) = SessionLogger::initialize(project_root, options)?;
    output::info(format_args!("tool log: {}", path.display()));
    Ok(toolchain.with_process_observer(logger))
}

fn source_name(scope: &str, tool: &str, invocation: u64) -> String {
    format!("{invocation}:{scope}/{tool}")
}

fn format_command(command: &Command) -> String {
    std::iter::once(command.get_program())
        .chain(command.get_args())
        .map(quote_command_part)
        .collect::<Vec<_>>()
        .join(" ")
}

fn quote_command_part(part: &OsStr) -> String {
    let part = part.to_string_lossy();
    if !part.is_empty()
        && part
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "_-./:\\=".contains(character))
    {
        part.into_owned()
    } else {
        quote_unsafe_command_part(&part)
    }
}

#[cfg(windows)]
fn quote_unsafe_command_part(part: &str) -> String {
    format!("'{}'", part.replace('\'', "''"))
}

#[cfg(not(windows))]
fn quote_unsafe_command_part(part: &str) -> String {
    format!("'{}'", part.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    #[test]
    fn formats_commands_and_quotes_unsafe_arguments() {
        let mut command = Command::new("yosys");
        command.args(["-q", "-s", r"D:\FPGA Projects\synth.ys"]);

        assert_eq!(format_command(&command), r"yosys -q -s 'D:\FPGA Projects\synth.ys'");
    }

    #[test]
    fn gives_parallel_invocations_distinct_sources() {
        assert_eq!(source_name("top", "nextpnr", 3), "3:top/nextpnr");
        assert_eq!(source_name("recovery", "nextpnr", 4), "4:recovery/nextpnr");
    }

    #[test]
    fn persists_commands_stdout_and_stderr_with_bitstream_scopes() {
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("bitloom-log-{nonce}"));
        let (logger, path) = SessionLogger::initialize(&root, OutputArgs::default()).unwrap();
        let mut command = Command::new("nextpnr-himbaechel");
        command.args(["--seed", "42"]);

        logger.command("top", 7, "nextpnr", &command).unwrap();
        logger.output("top", 7, "nextpnr", ProcessStream::Stdout, "placed 10 cells").unwrap();
        logger.output("top", 7, "nextpnr", ProcessStream::Stderr, "timing warning").unwrap();
        drop(logger);

        let log = std::fs::read_to_string(&path).unwrap();
        assert!(log.contains("COMMAND [7:top/nextpnr] $ nextpnr-himbaechel --seed 42"));
        assert!(log.contains("STDOUT  [7:top/nextpnr] placed 10 cells"));
        assert!(log.contains("STDERR  [7:top/nextpnr] timing warning"));
        std::fs::remove_dir_all(root).unwrap();
    }
}
