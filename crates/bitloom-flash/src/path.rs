use std::path::{Component, Path};

pub fn validate_project_relative(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path.components().all(|component| matches!(component, Component::Normal(_)))
}
