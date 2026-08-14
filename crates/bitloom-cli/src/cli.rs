use bitloom_config::{MonitorRxMode, MonitorTxLineEnding, MonitorTxMode};
use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(name = "bitloom", version, about = "FPGA project build tool")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Create a new Bitloom project
    New(NewArgs),
    /// Build every configured bitstream
    Build(BuildArgs),
    /// Remove generated build artifacts and persistent cache entries
    Clean(CleanArgs),
    /// Program already built bitstreams
    Flash(FlashArgs),
    /// Open the serial monitor
    Monitor(MonitorArgs),
    /// Build, program and open the serial monitor
    Run(RunArgs),
}

#[derive(Debug, Args)]
pub struct NewArgs {}

#[derive(Debug, Args)]
pub struct BuildArgs {
    /// Show resource utilization and Fmax for each built bitstream
    #[arg(long)]
    pub stat: bool,
    #[command(flatten)]
    pub output: OutputArgs,
}

#[derive(Debug, Args)]
pub struct CleanArgs {}

#[derive(Debug, Clone, Copy, Default, Args)]
pub struct OutputArgs {
    /// Stream output from external tools
    #[arg(short, long)]
    pub verbose: bool,
    /// Show external commands before running them
    #[arg(short, long)]
    pub advanced: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlashMode {
    Sram,
    Flash,
}

#[derive(Debug, Args)]
pub struct FlashArgs {
    #[command(flatten)]
    pub output: OutputArgs,
    #[arg(short = 'b', long, conflicts_with = "cable")]
    pub board: Option<String>,
    #[arg(short = 'c', long, conflicts_with = "board")]
    pub cable: Option<String>,
    #[arg(short = 's', long, conflicts_with = "flash")]
    pub sram: bool,
    #[arg(short = 'f', long, conflicts_with = "sram")]
    pub flash: bool,
}

impl FlashArgs {
    pub fn mode(&self) -> FlashMode {
        if self.flash { FlashMode::Flash } else { FlashMode::Sram }
    }
}

#[derive(Debug, Args)]
pub struct MonitorArgs {
    #[arg(short = 'p', long)]
    pub port: Option<String>,
    #[arg(short = 'b', long)]
    pub baud: Option<u32>,
    /// Select how received bytes are displayed
    #[arg(long, value_enum)]
    pub rx: Option<MonitorRxModeArg>,
    /// Select how typed input is encoded
    #[arg(long, value_enum)]
    pub tx: Option<MonitorTxModeArg>,
    /// Append this line ending to transmitted input
    #[arg(long, value_enum)]
    pub tx_line_ending: Option<MonitorTxLineEndingArg>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum MonitorRxModeArg {
    Utf8,
    Ascii,
    Hex,
    Raw,
}

impl From<MonitorRxModeArg> for MonitorRxMode {
    fn from(value: MonitorRxModeArg) -> Self {
        match value {
            MonitorRxModeArg::Utf8 => Self::Utf8,
            MonitorRxModeArg::Ascii => Self::Ascii,
            MonitorRxModeArg::Hex => Self::Hex,
            MonitorRxModeArg::Raw => Self::Raw,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum MonitorTxModeArg {
    Text,
    Hex,
}

impl From<MonitorTxModeArg> for MonitorTxMode {
    fn from(value: MonitorTxModeArg) -> Self {
        match value {
            MonitorTxModeArg::Text => Self::Text,
            MonitorTxModeArg::Hex => Self::Hex,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum MonitorTxLineEndingArg {
    Crlf,
    Lf,
    Cr,
    None,
}

impl From<MonitorTxLineEndingArg> for MonitorTxLineEnding {
    fn from(value: MonitorTxLineEndingArg) -> Self {
        match value {
            MonitorTxLineEndingArg::Crlf => Self::Crlf,
            MonitorTxLineEndingArg::Lf => Self::Lf,
            MonitorTxLineEndingArg::Cr => Self::Cr,
            MonitorTxLineEndingArg::None => Self::None,
        }
    }
}

#[derive(Debug, Args)]
pub struct RunArgs {
    /// Show resource utilization and Fmax for each built bitstream
    #[arg(long)]
    pub stat: bool,
    #[command(flatten)]
    pub output: OutputArgs,
    #[arg(short = 'b', long, conflicts_with = "cable")]
    pub board: Option<String>,
    #[arg(short = 'c', long, conflicts_with = "board")]
    pub cable: Option<String>,
    #[arg(short = 's', long, conflicts_with = "flash")]
    pub sram: bool,
    #[arg(short = 'f', long, conflicts_with = "sram")]
    pub flash: bool,
    #[arg(short = 'p', long)]
    pub port: Option<String>,
    #[arg(long)]
    pub baud: Option<u32>,
    /// Select how received bytes are displayed
    #[arg(long, value_enum)]
    pub rx: Option<MonitorRxModeArg>,
    /// Select how typed input is encoded
    #[arg(long, value_enum)]
    pub tx: Option<MonitorTxModeArg>,
    /// Append this line ending to transmitted input
    #[arg(long, value_enum)]
    pub tx_line_ending: Option<MonitorTxLineEndingArg>,
}

impl RunArgs {
    pub fn mode(&self) -> FlashMode {
        if self.flash { FlashMode::Flash } else { FlashMode::Sram }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_requested_commands() {
        assert!(matches!(
            Cli::try_parse_from(["bitloom", "new"]).unwrap().command,
            Command::New(_)
        ));
        assert!(Cli::try_parse_from(["bitloom", "new", "demo"]).is_err());
        assert!(matches!(
            Cli::try_parse_from(["bitloom", "build"]).unwrap().command,
            Command::Build(_)
        ));
        let Command::Build(build) =
            Cli::try_parse_from(["bitloom", "build", "--stat"]).unwrap().command
        else {
            panic!("expected build command");
        };
        assert!(build.stat);
        assert!(!build.output.verbose);
        assert!(!build.output.advanced);
        let Command::Build(build) =
            Cli::try_parse_from(["bitloom", "build", "--verbose", "--advanced"]).unwrap().command
        else {
            panic!("expected build command");
        };
        assert!(build.output.verbose);
        assert!(build.output.advanced);
        assert!(Cli::try_parse_from(["bitloom", "build", "-s"]).is_err());
        assert!(matches!(
            Cli::try_parse_from(["bitloom", "clean"]).unwrap().command,
            Command::Clean(_)
        ));
        assert!(matches!(
            Cli::try_parse_from(["bitloom", "flash", "--flash"]).unwrap().command,
            Command::Flash(_)
        ));
        assert!(matches!(
            Cli::try_parse_from([
                "bitloom",
                "monitor",
                "--port",
                "COM7",
                "--baud",
                "115200",
                "--rx",
                "utf8",
                "--tx",
                "text",
                "--tx-line-ending",
                "lf",
            ])
            .unwrap()
            .command,
            Command::Monitor(MonitorArgs {
                rx: Some(MonitorRxModeArg::Utf8),
                tx: Some(MonitorTxModeArg::Text),
                tx_line_ending: Some(MonitorTxLineEndingArg::Lf),
                ..
            })
        ));
        let Command::Run(run) = Cli::try_parse_from([
            "bitloom", "run", "--stat", "--sram", "--port", "COM7", "--rx", "raw", "--tx", "hex",
        ])
        .unwrap()
        .command
        else {
            panic!("expected run command");
        };
        assert!(run.stat);
        assert_eq!(run.rx, Some(MonitorRxModeArg::Raw));
        assert_eq!(run.tx, Some(MonitorTxModeArg::Hex));
    }

    #[test]
    fn rejects_conflicting_flash_modes() {
        assert!(Cli::try_parse_from(["bitloom", "flash", "--sram", "--flash"]).is_err());
    }

    #[test]
    fn line_ending_can_override_configuration_independently() {
        let Command::Monitor(args) =
            Cli::try_parse_from(["bitloom", "monitor", "--tx-line-ending", "lf"]).unwrap().command
        else {
            panic!("expected monitor command");
        };
        assert_eq!(args.tx_line_ending, Some(MonitorTxLineEndingArg::Lf));
    }
}
