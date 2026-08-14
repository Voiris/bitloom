use crate::ConfigError;
use crate::models::{
    BitstreamConfig, FlashSection, MonitorSection, MonitorTxLineEnding, ProjectConfig,
    ProjectSection, RtlSection, TargetSection, ToolchainSection,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoardPreset {
    TangNano20k,
}

impl BoardPreset {
    pub fn parse(name: &str) -> Result<Self, ConfigError> {
        match name {
            "tangnano20k" => Ok(Self::TangNano20k),
            _ => Err(ConfigError::UnsupportedBoard { name: name.to_owned() }),
        }
    }

    pub const fn board_name(self) -> &'static str {
        match self {
            Self::TangNano20k => "tangnano20k",
        }
    }

    pub const fn display_name(self) -> &'static str {
        match self {
            Self::TangNano20k => "Tang Nano 20K",
        }
    }

    pub const fn vendor(self) -> &'static str {
        match self {
            Self::TangNano20k => "gowin",
        }
    }

    pub const fn vendor_display_name(self) -> &'static str {
        match self {
            Self::TangNano20k => "Gowin",
        }
    }

    pub const fn device(self) -> &'static str {
        match self {
            Self::TangNano20k => "GW2AR-LV18QN88C8/I7",
        }
    }

    pub const fn frequency_mhz(self) -> f64 {
        match self {
            Self::TangNano20k => 27.0,
        }
    }
}

impl ProjectConfig {
    pub fn template(project_name: impl Into<String>, board: Option<BoardPreset>) -> Self {
        let (vendor, device, board_name, frequency_mhz) = board
            .map_or(("", "", "", 27.0), |board| {
                (board.vendor(), board.device(), board.board_name(), board.frequency_mhz())
            });
        Self {
            project: ProjectSection { name: project_name.into(), version: "0.1.0".to_owned() },
            rtl: RtlSection {
                sources: vec!["rtl/**/*.sv".to_owned(), "rtl/**/*.v".to_owned()],
                exclude: Vec::new(),
            },
            toolchain: ToolchainSection::default(),
            target: TargetSection { vendor: vendor.to_owned(), device: device.to_owned() },
            flash: FlashSection { board: board_name.to_owned(), cable: String::new() },
            monitor: MonitorSection {
                tx_line_ending: Some(MonitorTxLineEnding::Crlf),
                ..MonitorSection::default()
            },
            bitstreams: vec![BitstreamConfig {
                name: "top".to_owned(),
                top: "top".to_owned(),
                frequency_mhz,
                constraints: vec!["constraints/top.cst".to_owned()],
            }],
        }
    }
}
