use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to read {path}: {source}")]
    Read { path: PathBuf, source: std::io::Error },
    #[error("failed to write {path}: {source}")]
    Write { path: PathBuf, source: std::io::Error },
    #[error("failed to parse {path}: {source}")]
    Parse { path: PathBuf, source: toml::de::Error },
    #[error("failed to serialize project configuration: {0}")]
    Serialize(#[from] toml::ser::Error),
    #[error("project name cannot be empty")]
    EmptyProjectName,
    #[error("project version cannot be empty")]
    EmptyProjectVersion,
    #[error("`rtl.sources` must contain at least one pattern")]
    NoSourcePatterns,
    #[error("configuration must contain at least one `[[bitstreams]]` entry")]
    NoBitstreams,
    #[error("bitstream name cannot be empty")]
    EmptyBitstreamName,
    #[error("bitstream name `{name}` must be a simple identifier")]
    InvalidBitstreamName { name: String },
    #[error("bitstream `{name}` appears more than once")]
    DuplicateBitstream { name: String },
    #[error("bitstream `{name}` has invalid top module `{top}`")]
    InvalidTop { name: String, top: String },
    #[error("bitstream `{name}` frequency must be finite and greater than zero")]
    InvalidFrequency { name: String },
    #[error("`target.vendor` is required for building")]
    MissingVendor,
    #[error("`target.device` is required for building")]
    MissingDevice,
    #[error("`flash.board` and `flash.cable` cannot both be configured")]
    ConflictingFlashConnection,
    #[error("unsupported board `{name}`; currently supported: tangnano20k")]
    UnsupportedBoard { name: String },
    #[error("invalid glob `{pattern}`: {message}")]
    InvalidGlob { pattern: String, message: String },
    #[error("failed to walk project directory {path}: {message}")]
    Walk { path: PathBuf, message: String },
    #[error("source pattern set matched no files")]
    NoSources,
    #[error("configured path must be project-relative: {path}")]
    InvalidPath { path: PathBuf },
    #[error("constraint pattern `{pattern}` matched no files for bitstream `{name}`")]
    NoConstraints { name: String, pattern: String },
}
