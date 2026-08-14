use std::process::{Command, ExitStatus};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessStream {
    Stdout,
    Stderr,
}

pub trait ProcessObserver: Send + Sync + 'static {
    fn command(
        &self,
        _scope: &str,
        _invocation: u64,
        _tool: &str,
        _command: &Command,
    ) -> std::io::Result<()> {
        Ok(())
    }

    fn output(
        &self,
        _scope: &str,
        _invocation: u64,
        _tool: &str,
        _stream: ProcessStream,
        _line: &str,
    ) -> std::io::Result<()> {
        Ok(())
    }

    fn finished(
        &self,
        _scope: &str,
        _invocation: u64,
        _tool: &str,
        _status: &ExitStatus,
    ) -> std::io::Result<()> {
        Ok(())
    }

    fn cancelled(&self, _scope: &str, _invocation: u64, _tool: &str) -> std::io::Result<()> {
        Ok(())
    }
}

impl ProcessObserver for () {}
