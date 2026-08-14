use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use bitloom_config::{ProjectConfig, ResolvedProject};
use bitloom_gowin::Toolchain;

pub async fn load_resolved() -> Result<ResolvedProject> {
    let (root, config) = load_config(true).await?;
    Ok(bitloom_config::resolve(root, config)?)
}

pub async fn load_config(require_target: bool) -> Result<(PathBuf, ProjectConfig)> {
    let root = std::env::current_dir().context("failed to determine current directory")?;
    let config = bitloom_config::load(&root).await?;
    config.validate(require_target)?;
    Ok((root, config))
}

pub fn bitstream_path(root: &Path, name: &str) -> PathBuf {
    root.join("build/bitstreams").join(format!("{name}.fs"))
}

pub fn resolve_toolchain(project_root: &Path, config: &ProjectConfig) -> Result<Toolchain> {
    if config.toolchain.path.as_os_str().is_empty() {
        return Ok(Toolchain::default());
    }
    let path = if config.toolchain.path.is_absolute() {
        config.toolchain.path.clone()
    } else {
        project_root.join(&config.toolchain.path)
    };
    ensure!(path.is_dir(), "toolchain path is not a directory: {}", path.display());
    Ok(Toolchain::from_path(path)?)
}

pub async fn clear_build_outputs(root: &Path) -> Result<()> {
    let path = root.join("build");
    let mut entries = match tokio::fs::read_dir(&path).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to inspect build directory {}", path.display()));
        }
    };
    while let Some(entry) = entries.next_entry().await? {
        if entry.file_name() == "cache" {
            continue;
        }
        let entry_path = entry.path();
        let file_type = entry.file_type().await?;
        if file_type.is_dir() {
            tokio::fs::remove_dir_all(&entry_path).await.with_context(|| {
                format!("failed to remove old build directory {}", entry_path.display())
            })?;
        } else {
            tokio::fs::remove_file(&entry_path).await.with_context(|| {
                format!("failed to remove old build artifact {}", entry_path.display())
            })?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    #[tokio::test]
    async fn clears_outputs_but_preserves_persistent_cache() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should follow the Unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("bitloom-clear-build-{nonce}"));
        tokio::fs::create_dir_all(root.join("build/cache/synthesis")).await.unwrap();
        tokio::fs::create_dir_all(root.join("build/designs/top")).await.unwrap();
        tokio::fs::write(root.join("build/cache/synthesis/entry"), b"cached").await.unwrap();
        tokio::fs::write(root.join("build/designs/top/synthesis.json"), b"generated")
            .await
            .unwrap();
        tokio::fs::write(root.join("build/flash.bin"), b"generated").await.unwrap();

        clear_build_outputs(&root).await.unwrap();

        assert_eq!(
            tokio::fs::read(root.join("build/cache/synthesis/entry")).await.unwrap(),
            b"cached"
        );
        assert!(!root.join("build/designs").exists());
        assert!(!root.join("build/flash.bin").exists());

        tokio::fs::remove_dir_all(root).await.unwrap();
    }
}
