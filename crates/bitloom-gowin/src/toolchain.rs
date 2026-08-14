use std::collections::HashMap;
use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use bitloom_core::OutputFingerprint;
use blake3::Hasher;
use tokio::process::Command;

use crate::process::run_tool;
use crate::{ProcessObserver, ToolError};

type CommandEnvironment = HashMap<OsString, OsString>;

#[derive(Clone)]
pub struct Toolchain {
    root: Option<Arc<PathBuf>>,
    environment: Arc<CommandEnvironment>,
    observer: Arc<dyn ProcessObserver>,
    process_scope: Arc<str>,
}

impl Default for Toolchain {
    fn default() -> Self {
        Self {
            root: None,
            environment: Arc::new(CommandEnvironment::new()),
            observer: Arc::new(()),
            process_scope: Arc::from("toolchain"),
        }
    }
}

impl Toolchain {
    pub fn from_path(path: impl Into<PathBuf>) -> Result<Self, ToolError> {
        let path = path.into();
        if path.as_os_str().is_empty() {
            return Ok(Self::default());
        }
        let environment = oss_cad_environment(&path)?;
        Ok(Self {
            root: Some(Arc::new(path)),
            environment: Arc::new(environment),
            observer: Arc::new(()),
            process_scope: Arc::from("toolchain"),
        })
    }

    pub fn with_process_observer(mut self, observer: Arc<dyn ProcessObserver>) -> Self {
        self.observer = observer;
        self
    }

    pub fn with_process_scope(mut self, scope: impl Into<Arc<str>>) -> Self {
        self.process_scope = scope.into();
        self
    }

    pub fn uses_system_path(&self) -> bool {
        self.root.is_none()
    }

    pub async fn refresh_gdk_cache(&self) -> Result<(), ToolError> {
        let Some(root) = &self.root else {
            return Ok(());
        };
        let executable = executable_path(&root.join("lib"), "gdk-pixbuf-query-loaders");
        let mut command = Command::new(executable);
        command.arg("--update-cache").envs(self.environment.iter());
        run_tool(command, "gdk-pixbuf", self.process_scope.clone(), self.observer.clone()).await
    }

    pub async fn cache_fingerprint(&self) -> Result<OutputFingerprint, ToolError> {
        let mut hasher = Hasher::new();
        hasher.update(b"bitloom-oss-cad-suite-v1\0");
        if let Some(root) = &self.root {
            hash_os(&mut hasher, root.as_os_str());
            let version_path = root.join("VERSION");
            match tokio::fs::read(&version_path).await {
                Ok(version) => {
                    hasher.update(b"version\0");
                    hasher.update(&version);
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    hash_tool_binaries(&mut hasher, Some(root)).await?;
                }
                Err(source) => return Err(ToolError::Read { path: version_path, source }),
            }
        } else {
            hasher.update(b"system-path\0");
            hash_tool_binaries(&mut hasher, None).await?;
        }

        let mut environment = self.environment.iter().collect::<Vec<_>>();
        environment.sort_by(|(first, _), (second, _)| first.cmp(second));
        for (name, value) in environment {
            hash_os(&mut hasher, name);
            hash_os(&mut hasher, value);
        }
        Ok(OutputFingerprint::new(*hasher.finalize().as_bytes()))
    }

    pub(crate) fn command(&self, executable: &'static str) -> Command {
        let mut command = Command::new(executable);
        command.envs(self.environment.iter());
        command
    }

    pub(crate) fn process_observer(&self) -> Arc<dyn ProcessObserver> {
        self.observer.clone()
    }

    pub(crate) fn process_scope(&self) -> Arc<str> {
        self.process_scope.clone()
    }
}

async fn hash_tool_binaries(hasher: &mut Hasher, root: Option<&Path>) -> Result<(), ToolError> {
    for name in ["yosys", "nextpnr-himbaechel", "gowin_pack"] {
        let path = match root {
            Some(root) => executable_path(&root.join("bin"), name),
            None => find_on_path(name)?,
        };
        let contents = tokio::fs::read(&path)
            .await
            .map_err(|source| ToolError::Read { path: path.clone(), source })?;
        hasher.update(name.as_bytes());
        hasher.update(&[0]);
        hash_os(hasher, path.as_os_str());
        hasher.update(&contents);
    }
    Ok(())
}

fn find_on_path(name: &str) -> Result<PathBuf, ToolError> {
    let path = env::var_os("PATH").unwrap_or_default();
    for directory in env::split_paths(&path) {
        let candidate = executable_path(&directory, name);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(ToolError::Environment {
        message: format!("cannot fingerprint `{name}` because it was not found on PATH"),
    })
}

fn hash_os(hasher: &mut Hasher, value: &std::ffi::OsStr) {
    let bytes = value.as_encoded_bytes();
    hasher.update(&(bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

fn oss_cad_environment(root: &Path) -> Result<CommandEnvironment, ToolError> {
    let current_path = env::var_os("PATH").unwrap_or_default();
    let path = env::join_paths(
        [root.join("bin"), root.join("lib")].into_iter().chain(env::split_paths(&current_path)),
    )
    .map_err(|error| ToolError::Environment { message: error.to_string() })?;
    let mut environment = CommandEnvironment::with_capacity(11);
    environment.insert("YOSYSHQ_ROOT".into(), root.into());
    environment.insert("SSL_CERT_FILE".into(), root.join("etc/cacert.pem").into_os_string());
    environment.insert("PATH".into(), path);
    environment.insert("PYTHON_EXECUTABLE".into(), root.join("lib/python3.exe").into_os_string());
    environment.insert("QT_PLUGIN_PATH".into(), root.join("lib/qt5/plugins").into_os_string());
    environment.insert("QT_LOGGING_RULES".into(), "*=false".into());
    environment.insert("GTK_EXE_PREFIX".into(), root.into());
    environment.insert("GTK_DATA_PREFIX".into(), root.into());
    environment.insert(
        "GDK_PIXBUF_MODULEDIR".into(),
        root.join("lib/gdk-pixbuf-2.0/2.10.0/loaders").into_os_string(),
    );
    environment.insert(
        "GDK_PIXBUF_MODULE_FILE".into(),
        root.join("lib/gdk-pixbuf-2.0/2.10.0/loaders.cache").into_os_string(),
    );
    environment.insert(
        "OPENFPGALOADER_SOJ_DIR".into(),
        root.join("share/openFPGALoader").into_os_string(),
    );
    Ok(environment)
}

fn executable_path(directory: &Path, name: &str) -> PathBuf {
    if cfg!(windows) { directory.join(format!("{name}.exe")) } else { directory.join(name) }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    #[test]
    fn creates_complete_oss_cad_suite_environment() {
        let root = PathBuf::from("C:/oss-cad-suite");
        let toolchain = Toolchain::from_path(&root).unwrap();

        assert_eq!(
            toolchain.environment.get(OsStr::new("YOSYSHQ_ROOT")),
            Some(&root.into_os_string())
        );
        for name in [
            "SSL_CERT_FILE",
            "PATH",
            "PYTHON_EXECUTABLE",
            "QT_PLUGIN_PATH",
            "QT_LOGGING_RULES",
            "GTK_EXE_PREFIX",
            "GTK_DATA_PREFIX",
            "GDK_PIXBUF_MODULEDIR",
            "GDK_PIXBUF_MODULE_FILE",
            "OPENFPGALOADER_SOJ_DIR",
        ] {
            assert!(toolchain.environment.contains_key(OsStr::new(name)), "missing {name}");
        }
    }

    #[tokio::test]
    async fn cache_fingerprint_tracks_suite_version() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should follow the Unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("bitloom-toolchain-cache-{nonce}"));
        tokio::fs::create_dir_all(&root).await.unwrap();
        tokio::fs::write(root.join("VERSION"), "2026-08-01").await.unwrap();
        let toolchain = Toolchain::from_path(&root).unwrap();
        let first = toolchain.cache_fingerprint().await.unwrap();

        tokio::fs::write(root.join("VERSION"), "2026-08-02").await.unwrap();
        let second = toolchain.cache_fingerprint().await.unwrap();

        assert_ne!(first, second);
        tokio::fs::remove_dir_all(root).await.unwrap();
    }
}
