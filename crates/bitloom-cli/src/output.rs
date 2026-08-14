use std::fmt::{self, Display, Formatter as FmtFormatter};
use std::io::{self, Write};

use owo_colors::OwoColorize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code, reason = "all levels are part of the shared output formatting contract")]
pub enum Level {
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Formatter;

#[derive(Debug, Clone, Copy)]
pub struct Formatted<T> {
    level: Level,
    message: T,
}

#[derive(Debug, Clone, Copy)]
#[allow(dead_code, reason = "reserved for forwarded external-tool output")]
pub struct FormattedTool<'a, T> {
    tool: &'a str,
    message: T,
}

pub const fn formatter() -> Formatter {
    Formatter
}

impl Formatter {
    pub const fn format<T>(self, level: Level, message: T) -> Formatted<T> {
        Formatted { level, message }
    }

    #[allow(dead_code, reason = "reserved for forwarded external-tool output")]
    pub const fn tool<'a, T>(self, tool: &'a str, message: T) -> FormattedTool<'a, T> {
        FormattedTool { tool, message }
    }
}

impl<T: Display> Display for Formatted<T> {
    fn fmt(&self, formatter: &mut FmtFormatter<'_>) -> fmt::Result {
        write!(formatter, "{} ", "[bitloom]".bright_cyan().bold())?;
        match self.level {
            Level::Info => self.message.fmt(formatter),
            Level::Success => write!(formatter, "{}", self.message.green()),
            Level::Warning => write!(formatter, "{}", self.message.yellow()),
            Level::Error => write!(formatter, "{}", self.message.red().bold()),
        }
    }
}

impl<T: Display> Display for FormattedTool<'_, T> {
    fn fmt(&self, formatter: &mut FmtFormatter<'_>) -> fmt::Result {
        write!(formatter, "{} {}", format_args!("[{}]", self.tool).bright_black(), self.message)
    }
}

pub fn line(message: impl Display) -> io::Result<()> {
    writeln!(io::stdout(), "{message}")
}

#[allow(dead_code, reason = "used by commands that report progress")]
pub fn info(message: impl Display) {
    let _ = writeln!(io::stdout(), "{}", formatter().format(Level::Info, message));
}

#[allow(dead_code, reason = "used by commands that report completion")]
pub fn success(message: impl Display) {
    let _ = writeln!(io::stdout(), "{}", formatter().format(Level::Success, message));
}

pub fn warning(message: impl Display) {
    let _ = writeln!(io::stderr(), "{}", formatter().format(Level::Warning, message));
}

pub fn error(message: impl Display) {
    let _ = writeln!(io::stderr(), "{}", formatter().format(Level::Error, message));
}

#[allow(dead_code, reason = "reserved for forwarded external-tool output")]
pub fn tool(tool: &str, message: impl Display, is_stderr: bool) {
    let message = formatter().tool(tool, message);
    if is_stderr {
        let _ = writeln!(io::stderr(), "{message}");
    } else {
        let _ = writeln!(io::stdout(), "{message}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_bitloom_levels_with_legacy_colors_and_no_newline() {
        let formatter = formatter();
        let colored_prefix = "[bitloom]".bright_cyan();
        let prefix = colored_prefix.bold();

        assert_eq!(
            formatter.format(Level::Info, "starting").to_string(),
            format!("{prefix} starting")
        );
        assert_eq!(
            formatter.format(Level::Success, "finished").to_string(),
            format!("{prefix} {}", "finished".green())
        );
        assert_eq!(
            formatter.format(Level::Warning, "disconnected").to_string(),
            format!("{prefix} {}", "disconnected".yellow())
        );
        assert_eq!(
            formatter.format(Level::Error, "failed").to_string(),
            format!("{prefix} {}", "failed".red().bold())
        );
        assert!(!formatter.format(Level::Info, "starting").to_string().ends_with('\n'));
    }

    #[test]
    fn formats_tool_output_with_a_dim_source_label() {
        assert_eq!(
            formatter().tool("Yosys", "executing command").to_string(),
            format!("{} executing command", "[Yosys]".bright_black())
        );
    }
}
