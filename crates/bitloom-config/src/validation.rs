use std::collections::HashSet;

use crate::{ConfigError, ProjectConfig};

impl ProjectConfig {
    pub fn validate(&self, require_target: bool) -> Result<(), ConfigError> {
        if self.project.name.trim().is_empty() {
            return Err(ConfigError::EmptyProjectName);
        }
        if self.project.version.trim().is_empty() {
            return Err(ConfigError::EmptyProjectVersion);
        }
        if self.rtl.sources.is_empty() {
            return Err(ConfigError::NoSourcePatterns);
        }
        if self.bitstreams.is_empty() {
            return Err(ConfigError::NoBitstreams);
        }
        if require_target && self.target.vendor.trim().is_empty() {
            return Err(ConfigError::MissingVendor);
        }
        if require_target && self.target.device.trim().is_empty() {
            return Err(ConfigError::MissingDevice);
        }
        if !self.flash.board.trim().is_empty() && !self.flash.cable.trim().is_empty() {
            return Err(ConfigError::ConflictingFlashConnection);
        }
        let mut names = HashSet::with_capacity(self.bitstreams.len());
        for bitstream in &self.bitstreams {
            if bitstream.name.trim().is_empty() {
                return Err(ConfigError::EmptyBitstreamName);
            }
            if !is_simple_identifier(&bitstream.name) {
                return Err(ConfigError::InvalidBitstreamName { name: bitstream.name.clone() });
            }
            if !names.insert(bitstream.name.as_str()) {
                return Err(ConfigError::DuplicateBitstream { name: bitstream.name.clone() });
            }
            if !is_verilog_identifier(&bitstream.top) {
                return Err(ConfigError::InvalidTop {
                    name: bitstream.name.clone(),
                    top: bitstream.top.clone(),
                });
            }
            if !bitstream.frequency_mhz.is_finite() || bitstream.frequency_mhz <= 0.0 {
                return Err(ConfigError::InvalidFrequency { name: bitstream.name.clone() });
            }
        }
        Ok(())
    }
}

fn is_verilog_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    matches!(chars.next(), Some(first) if first.is_ascii_alphabetic() || first == '_')
        && chars
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '$'))
}

fn is_simple_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    matches!(chars.next(), Some(first) if first.is_ascii_alphabetic() || first == '_')
        && chars.all(|character| character.is_ascii_alphanumeric() || character == '_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BoardPreset;

    #[test]
    fn rejects_duplicate_bitstream_names() {
        let mut config = ProjectConfig::template("demo", Some(BoardPreset::TangNano20k));
        config.bitstreams.push(config.bitstreams[0].clone());

        assert!(matches!(config.validate(true), Err(ConfigError::DuplicateBitstream { .. })));
    }

    #[test]
    fn rejects_bitstream_names_that_could_escape_the_build_directory() {
        let mut config = ProjectConfig::template("demo", Some(BoardPreset::TangNano20k));
        config.bitstreams[0].name = "../../outside".to_owned();

        assert!(matches!(config.validate(true), Err(ConfigError::InvalidBitstreamName { .. })));
    }

    #[test]
    fn allows_line_ending_for_hex_monitor_input() {
        let mut config = ProjectConfig::template("demo", Some(BoardPreset::TangNano20k));
        config.monitor.tx = crate::MonitorTxMode::Hex;

        assert!(config.validate(true).is_ok());
    }
}
