use std::path::Path;

use crate::{ConfigError, FILE_NAME, ProjectConfig};

impl ProjectConfig {
    pub fn to_toml(&self) -> Result<String, ConfigError> {
        Ok(toml::to_string_pretty(self)?)
    }
}

pub async fn load(root: impl AsRef<Path>) -> Result<ProjectConfig, ConfigError> {
    let path = root.as_ref().join(FILE_NAME);
    let source = tokio::fs::read_to_string(&path)
        .await
        .map_err(|source| ConfigError::Read { path: path.clone(), source })?;
    toml::from_str::<ProjectConfig>(&source).map_err(|source| ConfigError::Parse { path, source })
}

pub async fn save(root: impl AsRef<Path>, config: &ProjectConfig) -> Result<(), ConfigError> {
    let path = root.as_ref().join(FILE_NAME);
    let source = config.to_toml()?;
    tokio::fs::write(&path, source).await.map_err(|source| ConfigError::Write { path, source })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BoardPreset;

    #[test]
    fn template_round_trips_through_toml() {
        let config = ProjectConfig::template("demo", Some(BoardPreset::TangNano20k));
        let source = config.to_toml().unwrap();
        assert!(source.contains("[toolchain]\npath = \"\""));
        let decoded: ProjectConfig = toml::from_str(&source).unwrap();

        assert_eq!(decoded.target.device, "GW2AR-LV18QN88C8/I7");
        assert_eq!(decoded.flash.board, "tangnano20k");
        assert!(decoded.toolchain.path.as_os_str().is_empty());
        assert_eq!(decoded.monitor.rx, crate::MonitorRxMode::Utf8);
        assert_eq!(decoded.monitor.tx, crate::MonitorTxMode::Text);
        assert_eq!(decoded.monitor.tx_line_ending, Some(crate::MonitorTxLineEnding::Crlf));
        assert_eq!(decoded.bitstreams[0].top, "top");
    }
}
