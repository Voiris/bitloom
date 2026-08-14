use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectConfig {
    pub project: ProjectSection,
    pub rtl: RtlSection,
    #[serde(default)]
    pub toolchain: ToolchainSection,
    #[serde(default)]
    pub target: TargetSection,
    #[serde(default)]
    pub flash: FlashSection,
    #[serde(default)]
    pub monitor: MonitorSection,
    #[serde(rename = "bitstreams")]
    pub bitstreams: Vec<BitstreamConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectSection {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RtlSection {
    pub sources: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolchainSection {
    #[serde(default)]
    pub path: PathBuf,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetSection {
    #[serde(default)]
    pub vendor: String,
    #[serde(default)]
    pub device: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FlashSection {
    #[serde(default)]
    pub board: String,
    #[serde(default)]
    pub cable: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MonitorRxMode {
    #[default]
    Utf8,
    Ascii,
    Hex,
    Raw,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MonitorTxMode {
    #[default]
    Text,
    Hex,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MonitorTxLineEnding {
    #[default]
    Crlf,
    Lf,
    Cr,
    None,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MonitorSection {
    #[serde(default)]
    pub baud: u32,
    #[serde(default)]
    pub port: String,
    #[serde(default)]
    pub rx: MonitorRxMode,
    #[serde(default)]
    pub tx: MonitorTxMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tx_line_ending: Option<MonitorTxLineEnding>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BitstreamConfig {
    pub name: String,
    pub top: String,
    pub frequency_mhz: f64,
    #[serde(default)]
    pub constraints: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ResolvedProject {
    pub root: PathBuf,
    pub config: ProjectConfig,
    pub sources: Vec<PathBuf>,
    pub constraints: Vec<Vec<PathBuf>>,
}
