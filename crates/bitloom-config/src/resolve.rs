use std::path::{Component, Path, PathBuf};

use globset::{Glob, GlobSetBuilder};
use walkdir::WalkDir;

use crate::{ConfigError, ProjectConfig, ResolvedProject};

pub fn resolve(
    root: impl Into<PathBuf>,
    config: ProjectConfig,
) -> Result<ResolvedProject, ConfigError> {
    config.validate(true)?;
    let root = root.into();
    let sources = expand_patterns(&root, &config.rtl.sources, &config.rtl.exclude)?;
    if sources.is_empty() {
        return Err(ConfigError::NoSources);
    }

    let constraints = config
        .bitstreams
        .iter()
        .map(|bitstream| {
            let mut resolved = Vec::new();
            for pattern in &bitstream.constraints {
                let matches = expand_patterns(&root, std::slice::from_ref(pattern), &[])?;
                if matches.is_empty() {
                    return Err(ConfigError::NoConstraints {
                        name: bitstream.name.clone(),
                        pattern: pattern.clone(),
                    });
                }
                resolved.extend(matches);
            }
            resolved.sort();
            resolved.dedup();
            Ok(resolved)
        })
        .collect::<Result<Vec<_>, ConfigError>>()?;

    Ok(ResolvedProject { root, config, sources, constraints })
}

fn expand_patterns(
    root: &Path,
    includes: &[String],
    excludes: &[String],
) -> Result<Vec<PathBuf>, ConfigError> {
    let includes = build_glob_set(includes)?;
    let excludes = build_glob_set(excludes)?;
    let mut paths = Vec::new();
    for entry in WalkDir::new(root).sort_by_file_name() {
        let entry = entry.map_err(|error| ConfigError::Walk {
            path: root.to_owned(),
            message: error.to_string(),
        })?;
        if !entry.file_type().is_file() {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(root)
            .map_err(|_| ConfigError::InvalidPath { path: entry.path().to_owned() })?;
        if relative.components().any(|component| {
            matches!(component, Component::Normal(value) if value == "build" || value == ".git")
        }) {
            continue;
        }
        let normalized = relative.to_string_lossy().replace('\\', "/");
        if includes.is_match(&normalized) && !excludes.is_match(&normalized) {
            paths.push(relative.to_owned());
        }
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn build_glob_set(patterns: &[String]) -> Result<globset::GlobSet, ConfigError> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        builder.add(Glob::new(pattern).map_err(|error| ConfigError::InvalidGlob {
            pattern: pattern.clone(),
            message: error.to_string(),
        })?);
    }
    builder.build().map_err(|error| ConfigError::InvalidGlob {
        pattern: patterns.join(", "),
        message: error.to_string(),
    })
}
