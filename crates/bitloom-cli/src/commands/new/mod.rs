mod templates;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use bitloom_config::BoardPreset;
use dialoguer::console::Term;
use dialoguer::theme::ColorfulTheme;
use dialoguer::{Confirm, Input, Select};

use crate::cli::NewArgs;
use crate::output;

const DEFAULT_TEMPLATE: &str = "empty";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HdlLanguage {
    SystemVerilog,
    Verilog,
}

impl HdlLanguage {
    const ALL: [Self; 2] = [Self::SystemVerilog, Self::Verilog];

    const fn display_name(self) -> &'static str {
        match self {
            Self::SystemVerilog => "SystemVerilog",
            Self::Verilog => "Verilog",
        }
    }

    const fn template_name(self) -> &'static str {
        match self {
            Self::SystemVerilog => "systemverilog",
            Self::Verilog => "verilog",
        }
    }
}

#[derive(Debug, Clone)]
struct BoardAnswers {
    preset: Option<BoardPreset>,
    device: String,
}

struct NewProjectAnswers {
    path: PathBuf,
    name: String,
    template: String,
    hdl: HdlLanguage,
    board: BoardAnswers,
    toolchain_path: PathBuf,
}

pub async fn run(_args: NewArgs) -> Result<()> {
    let answers = prompt_new_project()?;
    create_project(answers).await
}

async fn create_project(answers: NewProjectAnswers) -> Result<()> {
    let path = absolute_path(answers.path)?;
    if tokio::fs::try_exists(&path).await? {
        let mut entries = tokio::fs::read_dir(&path)
            .await
            .with_context(|| format!("failed to inspect {}", path.display()))?;
        ensure!(
            entries.next_entry().await?.is_none(),
            "project directory is not empty: {}",
            path.display()
        );
    }

    let preset_name = answers.board.preset.map(BoardPreset::board_name);
    let template = templates::resolve(&answers.template, answers.hdl.template_name(), preset_name)?;

    if !tokio::fs::try_exists(&path).await? {
        tokio::fs::create_dir_all(&path)
            .await
            .with_context(|| format!("failed to create {}", path.display()))?;
    }

    let config = project_config(&answers.name, &answers.board, answers.toolchain_path);
    write_project_file(&path.join(bitloom_config::FILE_NAME), config.to_toml()?.as_bytes()).await?;
    template.write_to(&path).await?;
    write_project_file(
        &path.join("partition.blpl"),
        b"@flash_size(8MiB);\n@offset_align(4KiB);\n\ntop:\n  offset(0x0),\n  size(auto),\n  file($top);\n",
    )
    .await?;
    write_project_file(&path.join(".gitignore"), b"/build/\n").await?;
    if answers.template != "empty"
        && template.used_custom_constraints()
        && let Some(preset) = answers.board.preset
    {
        output::warning(format_args!(
            "template `{}` has no constraints for {}; using custom constraints; \
             configure constraints/top.cst before building",
            answers.template,
            preset.display_name()
        ));
    }
    output::line(format_args!("Created Bitloom project at {}", path.display()))?;
    Ok(())
}

fn prompt_new_project() -> Result<NewProjectAnswers> {
    let theme = ColorfulTheme::default();
    let term = Term::stderr();
    let path = Input::<String>::with_theme(&theme)
        .with_prompt("Project path")
        .default("my-project".to_owned())
        .validate_with(|input: &String| validate_project_path(input))
        .interact_text_on(&term)?;
    let default_name = Path::new(&path)
        .file_name()
        .and_then(|name| name.to_str())
        .context("project path must end with a UTF-8 directory name")?
        .to_owned();
    let name = Input::<String>::with_theme(&theme)
        .with_prompt("Project name")
        .default(default_name)
        .validate_with(|input: &String| validate_project_name(input))
        .interact_text_on(&term)?;
    let templates = templates::template_names();
    ensure!(!templates.is_empty(), "no project templates are embedded");
    let default_template =
        templates.iter().position(|template| *template == DEFAULT_TEMPLATE).unwrap_or(0);
    let template_index = Select::with_theme(&theme)
        .with_prompt("Template")
        .items(&templates)
        .default(default_template)
        .interact_on(&term)?;
    let template = templates[template_index].to_owned();
    let hdl_languages = HdlLanguage::ALL
        .into_iter()
        .filter(|hdl| templates::supports_hdl(&template, hdl.template_name()))
        .collect::<Vec<_>>();
    ensure!(!hdl_languages.is_empty(), "template `{template}` does not support any HDL language");
    let hdl_names = hdl_languages.iter().map(|hdl| hdl.display_name()).collect::<Vec<_>>();
    let hdl_index = Select::with_theme(&theme)
        .with_prompt("HDL language")
        .items(&hdl_names)
        .default(0)
        .interact_on(&term)?;
    let hdl = hdl_languages[hdl_index];
    let board = prompt_board(&theme, &term)?;
    let toolchain = Input::<String>::with_theme(&theme)
        .with_prompt("OSS CAD Suite directory (empty uses PATH)")
        .allow_empty(true)
        .validate_with(|input: &String| validate_toolchain_path(input))
        .interact_text_on(&term)?;
    let toolchain_path = if toolchain.trim().is_empty() {
        PathBuf::new()
    } else {
        std::path::absolute(toolchain.trim()).context("failed to resolve toolchain directory")?
    };
    Ok(NewProjectAnswers { path: PathBuf::from(path), name, template, hdl, board, toolchain_path })
}

fn prompt_board(theme: &ColorfulTheme, term: &Term) -> Result<BoardAnswers> {
    let board_index = Select::with_theme(theme)
        .with_prompt("Preset")
        .items(["Tang Nano 20K", "No Preset"])
        .default(0)
        .interact_on(term)?;
    match board_index {
        0 => prompt_preset(BoardPreset::TangNano20k, theme, term),
        1 => Ok(BoardAnswers { preset: None, device: String::new() }),
        _ => bail!("selected board is not supported"),
    }
}

fn prompt_preset(preset: BoardPreset, theme: &ColorfulTheme, term: &Term) -> Result<BoardAnswers> {
    write_preset_summary(preset, term)?;
    let accepted = Confirm::with_theme(theme)
        .with_prompt("Is everything correct?")
        .default(true)
        .interact_on(term)?;
    let device = if accepted {
        preset.device().to_owned()
    } else {
        prompt_required_text("Device", preset.device(), theme, term)?
    };
    Ok(BoardAnswers { preset: Some(preset), device })
}

fn write_preset_summary(preset: BoardPreset, term: &Term) -> Result<()> {
    term.write_line("")?;
    term.write_line("Preset parameters:")?;
    term.write_line(&format!("  Vendor:       {}", preset.vendor_display_name()))?;
    term.write_line(&format!("  Device:       {}", preset.device()))?;
    term.write_line(&format!("  Frequency:    {} MHz", format_frequency(preset.frequency_mhz())))?;
    term.write_line(&format!("  Programmer:   {}", preset.board_name()))?;
    term.write_line(&format!("  Pinout:       {}", preset.display_name()))?;
    term.write_line("")?;
    Ok(())
}

fn prompt_required_text(
    prompt: &str,
    initial: &str,
    theme: &ColorfulTheme,
    term: &Term,
) -> Result<String> {
    let value = Input::<String>::with_theme(theme)
        .with_prompt(prompt)
        .with_initial_text(initial)
        .validate_with(|value: &String| validate_required(value))
        .interact_text_on(term)?;
    Ok(value.trim().to_owned())
}

fn validate_required(input: &str) -> Result<(), &'static str> {
    if input.trim().is_empty() { Err("Value cannot be empty") } else { Ok(()) }
}

fn format_frequency(frequency_mhz: f64) -> String {
    if frequency_mhz.fract() == 0.0 {
        format!("{frequency_mhz:.0}")
    } else {
        frequency_mhz.to_string()
    }
}

fn project_config(
    project_name: &str,
    board: &BoardAnswers,
    toolchain_path: PathBuf,
) -> bitloom_config::ProjectConfig {
    let mut config = bitloom_config::ProjectConfig::template(project_name, board.preset);
    config.target.device.clone_from(&board.device);
    config.toolchain.path = toolchain_path;
    config
}

fn validate_project_path(input: &str) -> Result<(), &'static str> {
    if input.trim().is_empty() {
        return Err("Project path cannot be empty");
    }
    let path = Path::new(input);
    if path.file_name().is_none() {
        return Err("Project path must end with a directory name");
    }
    if path.exists() && !path.is_dir() {
        return Err("Project path points to a file");
    }
    Ok(())
}

fn validate_project_name(input: &str) -> Result<(), &'static str> {
    let mut chars = input.chars();
    if !chars.next().is_some_and(|character| character.is_ascii_alphanumeric()) {
        return Err("Project name must start with A-Z, a-z, or 0-9");
    }
    if chars.all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-')) {
        Ok(())
    } else {
        Err("Only A-Z, a-z, 0-9, '_' and '-' are allowed")
    }
}

fn validate_toolchain_path(input: &str) -> Result<(), &'static str> {
    if input.trim().is_empty() {
        return Ok(());
    }
    let path = Path::new(input);
    if !path.exists() {
        return Err("Directory does not exist");
    }
    if !path.is_dir() {
        return Err("Path must point to a directory");
    }
    Ok(())
}

async fn write_project_file(path: &Path, contents: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    tokio::fs::write(path, contents)
        .await
        .with_context(|| format!("failed to write {}", path.display()))
}

fn absolute_path(path: PathBuf) -> Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path);
    }
    Ok(std::env::current_dir()?.join(path))
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    #[test]
    fn validates_interactive_project_answers() {
        assert!(validate_project_path("demo").is_ok());
        assert!(validate_project_path("").is_err());
        assert!(validate_project_name("fpga-demo_1").is_ok());
        assert!(validate_project_name("bad/name").is_err());
        assert!(validate_toolchain_path("").is_ok());
        assert!(validate_required("GW2AR-LV18QN88C8/I7").is_ok());
        assert!(validate_required("  ").is_err());
    }

    #[test]
    fn custom_board_leaves_preset_strings_empty() {
        let board = BoardAnswers { preset: None, device: String::new() };
        let config = project_config("demo", &board, PathBuf::new());
        assert!(config.target.vendor.is_empty());
        assert!(config.target.device.is_empty());
        assert!(config.flash.board.is_empty());
    }

    #[test]
    fn changing_a_preset_only_overrides_its_device() {
        let board = BoardAnswers {
            preset: Some(BoardPreset::TangNano20k),
            device: "GW2A-18-custom".to_owned(),
        };
        let config = project_config("demo", &board, PathBuf::new());
        assert_eq!(config.target.vendor, "gowin");
        assert_eq!(config.target.device, "GW2A-18-custom");
        assert_eq!(config.flash.board, "tangnano20k");
        assert_eq!(config.bitstreams[0].frequency_mhz, 27.0);
    }

    #[tokio::test]
    async fn creates_a_complete_project_from_validated_answers() {
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let path = std::env::temp_dir().join(format!("bitloom-new-project-{nonce}"));
        let answers = NewProjectAnswers {
            path: path.clone(),
            name: "integration-demo".to_owned(),
            template: "empty".to_owned(),
            hdl: HdlLanguage::SystemVerilog,
            board: BoardAnswers {
                preset: Some(BoardPreset::TangNano20k),
                device: BoardPreset::TangNano20k.device().to_owned(),
            },
            toolchain_path: PathBuf::from("fake-toolchain"),
        };

        create_project(answers).await.unwrap();

        assert!(path.join(bitloom_config::FILE_NAME).is_file());
        assert!(path.join("rtl/top.sv").is_file());
        assert!(path.join("constraints/top.cst").is_file());
        assert!(path.join("partition.blpl").is_file());
        assert_eq!(tokio::fs::read(path.join(".gitignore")).await.unwrap(), b"/build/\n");
        let config = bitloom_config::load(&path).await.unwrap();
        assert_eq!(config.project.name, "integration-demo");
        assert_eq!(config.toolchain.path, PathBuf::from("fake-toolchain"));
        assert_eq!(
            tokio::fs::read_to_string(path.join("rtl/top.sv")).await.unwrap(),
            "module top;\nendmodule\n"
        );
        assert!(tokio::fs::read(path.join("constraints/top.cst")).await.unwrap().is_empty());

        tokio::fs::remove_dir_all(path).await.unwrap();
    }
}
