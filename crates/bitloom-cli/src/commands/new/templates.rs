use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use include_dir::{Dir, include_dir};

use super::write_project_file;

static TEMPLATES: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/templates");

#[derive(Debug)]
struct PlannedFile {
    path: PathBuf,
    contents: &'static [u8],
}

#[derive(Debug)]
pub(super) struct ResolvedTemplate {
    files: Vec<PlannedFile>,
    used_custom_constraints: bool,
}

impl ResolvedTemplate {
    pub(super) const fn used_custom_constraints(&self) -> bool {
        self.used_custom_constraints
    }

    pub(super) async fn write_to(&self, root: &Path) -> Result<()> {
        for file in &self.files {
            write_project_file(&root.join(&file.path), file.contents).await?;
        }
        Ok(())
    }
}

pub(super) fn resolve(template: &str, hdl: &str, preset: Option<&str>) -> Result<ResolvedTemplate> {
    let hdl_path = format!("{template}/hdl/{hdl}");
    let hdl = TEMPLATES
        .get_dir(&hdl_path)
        .with_context(|| format!("template `{template}` does not support `{hdl}`"))?;

    let custom_constraints_path = format!("{template}/constraints/custom");
    let custom_constraints = TEMPLATES
        .get_dir(&custom_constraints_path)
        .with_context(|| format!("template `{template}` does not provide custom constraints"))?;
    let (constraints, used_custom_constraints) = match preset {
        Some(preset) => {
            let preset_constraints_path = format!("{template}/constraints/{preset}");
            TEMPLATES
                .get_dir(&preset_constraints_path)
                .map_or((custom_constraints, true), |constraints| (constraints, false))
        }
        None => (custom_constraints, false),
    };

    let mut files = Vec::new();
    add_layer(hdl, Path::new("rtl"), &mut files)?;
    add_layer(constraints, Path::new("constraints"), &mut files)?;
    ensure!(!files.is_empty(), "template `{template}` does not contain any files");

    let mut paths = BTreeSet::new();
    for file in &files {
        ensure!(
            paths.insert(file.path.clone()),
            "template `{template}` produces duplicate path {}",
            file.path.display()
        );
    }

    Ok(ResolvedTemplate { files, used_custom_constraints })
}

pub(super) fn template_names() -> Vec<&'static str> {
    let mut templates = TEMPLATES
        .dirs()
        .filter_map(|directory| directory.path().file_name()?.to_str())
        .collect::<Vec<_>>();
    templates.sort_unstable();
    templates
}

pub(super) fn supports_hdl(template: &str, hdl: &str) -> bool {
    TEMPLATES.get_dir(format!("{template}/hdl/{hdl}")).is_some()
}

fn add_layer(
    directory: &'static Dir<'static>,
    output_root: &Path,
    files: &mut Vec<PlannedFile>,
) -> Result<()> {
    let initial_count = files.len();
    collect_files(directory, directory.path(), output_root, files)?;
    ensure!(
        files.len() > initial_count,
        "template layer {} does not contain any files",
        directory.path().display()
    );
    Ok(())
}

fn collect_files(
    directory: &'static Dir<'static>,
    layer_root: &Path,
    output_root: &Path,
    files: &mut Vec<PlannedFile>,
) -> Result<()> {
    for file in directory.files() {
        let relative = file.path().strip_prefix(layer_root).with_context(|| {
            format!("template file {} is outside its layer", file.path().display())
        })?;
        files.push(PlannedFile { path: output_root.join(relative), contents: file.contents() });
    }
    for child in directory.dirs() {
        collect_files(child, layer_root, output_root, files)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_systemverilog_and_custom_constraints() {
        let template = resolve("empty", "systemverilog", None).unwrap();

        assert!(!template.used_custom_constraints());
        assert_eq!(template.files.len(), 2);
        assert!(template.files.iter().any(|file| file.path == Path::new("rtl/top.sv")));
        assert!(template.files.iter().any(|file| file.path == Path::new("constraints/top.cst")));
    }

    #[test]
    fn discovers_templates_and_their_hdl_variants_from_directories() {
        assert_eq!(template_names(), ["blink", "empty"]);
        assert!(supports_hdl("blink", "systemverilog"));
        assert!(supports_hdl("blink", "verilog"));
        assert!(!supports_hdl("blink", "vhdl"));
    }

    #[test]
    fn resolves_blink_with_tang_nano_constraints() {
        let template = resolve("blink", "systemverilog", Some("tangnano20k")).unwrap();

        assert!(!template.used_custom_constraints());
        let constraints = template
            .files
            .iter()
            .find(|file| file.path == Path::new("constraints/top.cst"))
            .unwrap();
        assert!(
            constraints
                .contents
                .windows(b"IO_LOC \"led\" 15;".len())
                .any(|window| { window == b"IO_LOC \"led\" 15;" })
        );
    }

    #[test]
    fn falls_back_to_custom_constraints_for_an_unsupported_preset() {
        let template = resolve("empty", "verilog", Some("tangnano20k")).unwrap();

        assert!(template.used_custom_constraints());
        assert!(template.files.iter().any(|file| file.path == Path::new("rtl/top.v")));
        assert!(!template.files.iter().any(|file| file.path == Path::new("rtl/top.sv")));
    }

    #[test]
    fn rejects_an_unsupported_hdl() {
        let error = resolve("empty", "vhdl", None).unwrap_err();
        assert!(error.to_string().contains("does not support `vhdl`"));
    }
}
