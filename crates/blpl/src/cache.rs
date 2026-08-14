use std::collections::{BTreeMap, HashSet};
use std::ffi::OsString;
use std::fmt;
use std::fs::{self, File};
use std::io::{self, ErrorKind, Read};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{Layout, Partition, PartitionSource};

const CACHE_VERSION: u32 = 1;
const FINGERPRINT_DOMAIN: &[u8] = b"bitloom.blpl.partition.v1\0";

/// A content fingerprint for one fully resolved partition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PartitionFingerprint(blake3::Hash);

impl PartitionFingerprint {
    pub fn as_bytes(&self) -> &[u8; 32] {
        self.0.as_bytes()
    }

    pub fn to_hex(&self) -> String {
        self.0.to_hex().to_string()
    }
}

impl fmt::Display for PartitionFingerprint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl Partition {
    /// Fingerprints the bytes that would be used for this partition together
    /// with its final placement and validation settings.
    pub fn fingerprint(&self) -> io::Result<PartitionFingerprint> {
        let mut hasher = blake3::Hasher::new();
        hasher.update(FINGERPRINT_DOMAIN);
        hasher.update(&self.offset.to_le_bytes());
        hasher.update(&self.size.to_le_bytes());
        hasher.update(&(self.magic.len() as u64).to_le_bytes());
        hasher.update(&self.magic);

        match &self.source {
            PartitionSource::File(path) => {
                hasher.update(b"file\0");
                hash_file(path, &mut hasher)?;
            }
        }

        Ok(PartitionFingerprint(hasher.finalize()))
    }
}

fn hash_file(path: &Path, hasher: &mut blake3::Hasher) -> io::Result<()> {
    let mut file = File::open(path).map_err(|error| {
        io::Error::new(error.kind(), format!("failed to open {}: {error}", path.display()))
    })?;
    let mut buffer = [0; 64 * 1024];

    loop {
        let read = file.read(&mut buffer).map_err(|error| {
            io::Error::new(error.kind(), format!("failed to read {}: {error}", path.display()))
        })?;
        if read == 0 {
            return Ok(());
        }
        hasher.update(&buffer[..read]);
    }
}

/// Persistent fingerprints of partitions that were processed successfully.
#[derive(Debug, Clone, Default)]
pub struct PartitionCache {
    partitions: BTreeMap<String, PartitionFingerprint>,
}

impl PartitionCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Loads a cache. A missing cache file is treated as an empty cache.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, CacheError> {
        let path = path.as_ref();
        let content = match fs::read(path) {
            Ok(content) => content,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::new()),
            Err(error) => return Err(CacheError::io("read", path, error)),
        };
        let document: CacheDocument = serde_json::from_slice(&content)?;
        if document.version != CACHE_VERSION {
            return Err(CacheError::UnsupportedVersion(document.version));
        }

        Ok(Self { partitions: document.partitions })
    }

    pub fn save(&self, path: impl AsRef<Path>) -> Result<(), CacheError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
            fs::create_dir_all(parent)
                .map_err(|error| CacheError::io("create cache directory", parent, error))?;
        }
        let document =
            CacheDocument { version: CACHE_VERSION, partitions: self.partitions.clone() };
        let content = serde_json::to_vec_pretty(&document)?;
        let temp_path = sidecar_path(path, ".tmp");
        let backup_path = sidecar_path(path, ".previous");

        remove_file_if_exists(&temp_path)?;
        fs::write(&temp_path, content)
            .map_err(|error| CacheError::io("write", &temp_path, error))?;
        replace_cache_file(&temp_path, path, &backup_path)
    }

    pub fn fingerprint(&self, partition_name: &str) -> Option<PartitionFingerprint> {
        self.partitions.get(partition_name).copied()
    }

    pub fn is_current(&self, partition: &Partition) -> io::Result<bool> {
        let fingerprint = partition.fingerprint()?;
        Ok(self.fingerprint(&partition.name) == Some(fingerprint))
    }

    pub fn outdated_partitions<'layout>(
        &self,
        layout: &'layout Layout,
    ) -> io::Result<Vec<&'layout Partition>> {
        layout
            .partitions
            .iter()
            .filter_map(|partition| match self.is_current(partition) {
                Ok(true) => None,
                Ok(false) => Some(Ok(partition)),
                Err(error) => Some(Err(error)),
            })
            .collect()
    }

    /// Records a partition after it has been processed successfully.
    pub fn record(&mut self, partition: &Partition) -> io::Result<PartitionFingerprint> {
        let fingerprint = partition.fingerprint()?;
        self.partitions.insert(partition.name.clone(), fingerprint);
        Ok(fingerprint)
    }

    pub fn remove(&mut self, partition_name: &str) -> Option<PartitionFingerprint> {
        self.partitions.remove(partition_name)
    }

    /// Removes cache entries for partitions absent from the current layout.
    pub fn retain_layout(&mut self, layout: &Layout) {
        let names = layout
            .partitions
            .iter()
            .map(|partition| partition.name.as_str())
            .collect::<HashSet<_>>();
        self.partitions.retain(|name, _| names.contains(name.as_str()));
    }
}

fn sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    let mut name = OsString::from(".");
    name.push(path.file_name().unwrap_or_default());
    name.push(suffix);
    path.with_file_name(name)
}

fn remove_file_if_exists(path: &Path) -> Result<(), CacheError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(CacheError::io("remove", path, error)),
    }
}

fn replace_cache_file(temp_path: &Path, path: &Path, backup_path: &Path) -> Result<(), CacheError> {
    remove_file_if_exists(backup_path)?;

    let had_previous = match fs::rename(path, backup_path) {
        Ok(()) => true,
        Err(error) if error.kind() == ErrorKind::NotFound => false,
        Err(error) => return Err(CacheError::io("move", path, error)),
    };

    if let Err(error) = fs::rename(temp_path, path) {
        if had_previous {
            let _ = fs::rename(backup_path, path);
        }
        return Err(CacheError::io("move", temp_path, error));
    }

    if had_previous {
        remove_file_if_exists(backup_path)?;
    }
    Ok(())
}

#[derive(Debug, Serialize, Deserialize)]
struct CacheDocument {
    version: u32,
    partitions: BTreeMap<String, PartitionFingerprint>,
}

#[derive(Debug)]
pub enum CacheError {
    Io { operation: &'static str, path: PathBuf, source: io::Error },
    Format(serde_json::Error),
    UnsupportedVersion(u32),
}

impl fmt::Display for CacheError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { operation, path, source } => {
                write!(formatter, "failed to {operation} {}: {source}", path.display())
            }
            Self::Format(error) => write!(formatter, "invalid partition cache: {error}"),
            Self::UnsupportedVersion(version) => {
                write!(formatter, "unsupported partition cache version {version}")
            }
        }
    }
}

impl std::error::Error for CacheError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Format(error) => Some(error),
            Self::UnsupportedVersion(_) => None,
        }
    }
}

impl CacheError {
    fn io(operation: &'static str, path: &Path, source: io::Error) -> Self {
        Self::Io { operation, path: path.to_owned(), source }
    }
}

impl From<serde_json::Error> for CacheError {
    fn from(error: serde_json::Error) -> Self {
        Self::Format(error)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static NEXT_TEMP_DIR: AtomicU64 = AtomicU64::new(0);

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let id = NEXT_TEMP_DIR.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("blpl-cache-{}-{id}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).expect("test directory should be created");
            Self(path)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn partition(name: &str, source: PathBuf) -> Partition {
        Partition {
            name: name.to_owned(),
            offset: 0,
            size: 3,
            magic: Box::new([]),
            source: PartitionSource::File(source),
        }
    }

    #[test]
    fn detects_content_and_layout_changes() {
        let directory = TestDir::new();
        let source = directory.0.join("partition.bin");
        fs::write(&source, [1, 2, 3]).unwrap();
        let mut partition = partition("app", source.clone());
        let mut cache = PartitionCache::new();

        assert!(!cache.is_current(&partition).unwrap());
        cache.record(&partition).unwrap();
        assert!(cache.is_current(&partition).unwrap());

        fs::write(&source, [1, 2, 4]).unwrap();
        assert!(!cache.is_current(&partition).unwrap());

        fs::write(&source, [1, 2, 3]).unwrap();
        partition.offset = 4;
        assert!(!cache.is_current(&partition).unwrap());
    }

    #[test]
    fn persists_fingerprints_and_prunes_stale_partitions() {
        let directory = TestDir::new();
        let app_source = directory.0.join("app.bin");
        let stale_source = directory.0.join("stale.bin");
        fs::write(&app_source, [1, 2, 3]).unwrap();
        fs::write(&stale_source, [4, 5, 6]).unwrap();
        let app = partition("app", app_source);
        let stale = partition("stale", stale_source);
        let cache_path = directory.0.join("nested").join("partitions.json");

        let mut cache = PartitionCache::new();
        cache.record(&app).unwrap();
        cache.record(&stale).unwrap();
        cache.save(&cache_path).unwrap();
        assert!(!sidecar_path(&cache_path, ".tmp").exists());
        assert!(!sidecar_path(&cache_path, ".previous").exists());

        let mut loaded = PartitionCache::load(&cache_path).unwrap();
        assert!(loaded.is_current(&app).unwrap());
        let layout = Layout { partitions: vec![app].into_boxed_slice() };
        assert!(loaded.outdated_partitions(&layout).unwrap().is_empty());

        loaded.retain_layout(&layout);
        assert!(loaded.fingerprint("stale").is_none());
    }

    #[test]
    fn missing_cache_is_empty() {
        let directory = TestDir::new();
        let cache = PartitionCache::load(directory.0.join("missing.json")).unwrap();

        assert!(cache.fingerprint("app").is_none());
    }
}
