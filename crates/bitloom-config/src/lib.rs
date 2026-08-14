//! Project configuration, validation and board presets.

mod defaults;
mod error;
mod io;
mod models;
mod resolve;
mod validation;

pub use defaults::BoardPreset;
pub use error::ConfigError;
pub use io::{load, save};
pub use models::{
    BitstreamConfig, FlashSection, MonitorRxMode, MonitorSection, MonitorTxLineEnding,
    MonitorTxMode, ProjectConfig, ProjectSection, ResolvedProject, RtlSection, TargetSection,
    ToolchainSection,
};
pub use resolve::resolve;

pub const FILE_NAME: &str = "bitloom.toml";
