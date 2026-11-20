use std::io::{self, ErrorKind, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::cursor::MoveToColumn;
use crossterm::queue;
use crossterm::terminal::{Clear, ClearType};

use crate::cli::CleanArgs;
use crate::output::{self, Level};

const PROGRESS_WIDTH: usize = 30;
const PROGRESS_FILLED: &str = "█";
const PROGRESS_EMPTY: &str = "░";
const REFRESH_INTERVAL: Duration = Duration::from_millis(100);
const KIBIBYTE: f64 = 1024.0;
const MEBIBYTE: f64 = KIBIBYTE * 1024.0;
const GIBIBYTE: f64 = MEBIBYTE * 1024.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryKind {
    File,
    Directory,
    Symlink,
}

#[derive(Debug)]
struct DeleteEntry {
    path: PathBuf,
    kind: EntryKind,
    size: u64,
}

#[derive(Debug, Default)]
struct DeletePlan {
    entries: Vec<DeleteEntry>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct CleanStatistics {
    files: u64,
    bytes: u64,
}

struct ProgressLine {
    visible: bool,
    last_rendered_at: Option<Instant>,
}

impl ProgressLine {
    fn new(visible: bool) -> Self {
        Self { visible, last_rendered_at: None }
    }

    fn render(&mut self, completed: usize, total: usize) {
        let now = Instant::now();
        if completed > 0
            && completed < total
            && self.last_rendered_at.is_some_and(|last_rendered_at| {
                now.duration_since(last_rendered_at) < REFRESH_INTERVAL
            })
        {
            return;
        }
        self.render_message(progress_message(completed, total, PROGRESS_WIDTH));
        self.last_rendered_at = Some(now);
    }

    fn render_message(&mut self, message: impl std::fmt::Display) {
        if !self.visible {
            return;
        }

        let mut stdout = io::stdout();
        if queue!(stdout, MoveToColumn(0), Clear(ClearType::CurrentLine))
            .and_then(|()| write!(stdout, "{}", output::formatter().format(Level::Info, message)))
            .and_then(|()| stdout.flush())
            .is_err()
        {
            self.visible = false;
        }
    }

    fn finish(&mut self) {
        if !self.visible {
            return;
        }

        let mut stdout = io::stdout();
        let _ = queue!(stdout, MoveToColumn(0), Clear(ClearType::CurrentLine));
        let _ = stdout.flush();
        self.visible = false;
    }
}

impl Drop for ProgressLine {
    fn drop(&mut self) {
        self.finish();
    }
}

pub async fn run(_args: CleanArgs) -> Result<()> {
    let project_root = std::env::current_dir().context("failed to determine current directory")?;
    let interactive = io::stdout().is_terminal();
    validate_project(&project_root).await?;
    if !interactive {
        output::info("cleaning build artifacts");
    }
    let statistics = remove_build(&project_root, interactive).await?;
    output::success(format!(
        "removed {}, {} total",
        format_file_count(statistics.files),
        format_bytes(statistics.bytes)
    ));
    Ok(())
}

#[cfg(test)]
async fn clean(project_root: &Path, interactive: bool) -> Result<CleanStatistics> {
    validate_project(project_root).await?;
    remove_build(project_root, interactive).await
}

async fn validate_project(project_root: &Path) -> Result<()> {
    let config = bitloom_config::load(project_root).await?;
    config.validate(false)?;
    Ok(())
}

async fn remove_build(project_root: &Path, interactive: bool) -> Result<CleanStatistics> {
    let build_path = project_root.join("build");
    let mut progress = ProgressLine::new(interactive);
    progress.render_message("scanning build artifacts");
    let plan = collect_delete_plan(&build_path).await?;
    let total = plan.entries.len();
    if interactive {
        progress.render(0, total);
    }

    let mut statistics = CleanStatistics::default();
    for (index, entry) in plan.entries.into_iter().enumerate() {
        let removed = match remove_entry(&entry).await {
            Ok(removed) => removed,
            Err(error) => {
                progress.finish();
                return Err(error).with_context(|| {
                    format!(
                        "failed to remove {} after removing {}, {}",
                        entry.path.display(),
                        format_file_count(statistics.files),
                        format_bytes(statistics.bytes)
                    )
                });
            }
        };
        if removed && entry.kind != EntryKind::Directory {
            statistics.files = statistics.files.saturating_add(1);
            statistics.bytes = statistics.bytes.saturating_add(entry.size);
        }
        progress.render(index + 1, total);
    }
    progress.finish();
    Ok(statistics)
}

async fn collect_delete_plan(root: &Path) -> Result<DeletePlan> {
    let mut plan = DeletePlan::default();
    let mut pending = vec![(root.to_owned(), false)];

    while let Some((path, visited)) = pending.pop() {
        let metadata = match tokio::fs::symlink_metadata(&path).await {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("failed to inspect build artifact {}", path.display())
                });
            }
        };
        let file_type = metadata.file_type();
        if is_link_like(&metadata) {
            plan.entries.push(DeleteEntry { path, kind: EntryKind::Symlink, size: metadata.len() });
        } else if file_type.is_dir() {
            if visited {
                plan.entries.push(DeleteEntry { path, kind: EntryKind::Directory, size: 0 });
                continue;
            }

            pending.push((path.clone(), true));
            let mut directory = tokio::fs::read_dir(&path)
                .await
                .with_context(|| format!("failed to inspect build directory {}", path.display()))?;
            let mut children = Vec::new();
            while let Some(entry) = directory
                .next_entry()
                .await
                .with_context(|| format!("failed to inspect build directory {}", path.display()))?
            {
                children.push(entry.path());
            }
            children.sort();
            pending.extend(children.into_iter().rev().map(|child| (child, false)));
        } else {
            plan.entries.push(DeleteEntry { path, kind: EntryKind::File, size: metadata.len() });
        }
    }

    Ok(plan)
}

fn is_link_like(metadata: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;

        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        metadata.file_type().is_symlink()
            || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

async fn remove_entry(entry: &DeleteEntry) -> io::Result<bool> {
    let result = match entry.kind {
        EntryKind::File => tokio::fs::remove_file(&entry.path).await,
        EntryKind::Directory => tokio::fs::remove_dir(&entry.path).await,
        EntryKind::Symlink => match tokio::fs::remove_file(&entry.path).await {
            Ok(()) => return Ok(true),
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
            Err(file_error) => match tokio::fs::remove_dir(&entry.path).await {
                Ok(()) => return Ok(true),
                Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
                Err(_) => return Err(file_error),
            },
        },
    };

    match result {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn progress_message(completed: usize, total: usize, width: usize) -> String {
    let completed = completed.min(total);
    let filled = completed.saturating_mul(width).checked_div(total).unwrap_or(width);
    let percentage = completed.saturating_mul(100).checked_div(total).unwrap_or(100);
    format!(
        "cleaning [{}{}] {percentage:>3}%",
        PROGRESS_FILLED.repeat(filled),
        PROGRESS_EMPTY.repeat(width.saturating_sub(filled))
    )
}

fn format_file_count(files: u64) -> String {
    let count = group_digits(files);
    if files == 1 { format!("{count} file") } else { format!("{count} files") }
}

fn group_digits(value: u64) -> String {
    let digits = value.to_string();
    let mut output = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, character) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            output.push(',');
        }
        output.push(character);
    }
    output
}

fn format_bytes(bytes: u64) -> String {
    let value = bytes as f64;
    if value >= GIBIBYTE {
        format!("{:.1} GiB", value / GIBIBYTE)
    } else if value >= MEBIBYTE {
        format!("{:.1} MiB", value / MEBIBYTE)
    } else if value >= KIBIBYTE {
        format!("{:.1} KiB", value / KIBIBYTE)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use bitloom_config::{BoardPreset, ProjectConfig};

    use super::*;

    fn test_root(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should follow the Unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("bitloom-clean-{name}-{nonce}"))
    }

    async fn write_project(root: &Path) {
        let _ = tokio::fs::remove_dir_all(root).await;
        tokio::fs::create_dir_all(root).await.unwrap();
        bitloom_config::save(
            root,
            &ProjectConfig::template("clean-test", Some(BoardPreset::TangNano20k)),
        )
        .await
        .unwrap();
    }

    #[cfg(unix)]
    fn symlink_file(source: &Path, destination: &Path) -> io::Result<()> {
        std::os::unix::fs::symlink(source, destination)
    }

    #[cfg(windows)]
    fn symlink_file(source: &Path, destination: &Path) -> io::Result<()> {
        std::os::windows::fs::symlink_file(source, destination)
    }

    #[tokio::test]
    async fn refuses_to_clean_a_directory_without_a_valid_project() {
        let root = test_root("invalid");
        let artifact = root.join("build/important.bin");
        let _ = tokio::fs::remove_dir_all(&root).await;
        tokio::fs::create_dir_all(artifact.parent().unwrap()).await.unwrap();
        tokio::fs::write(&artifact, b"keep").await.unwrap();

        assert!(clean(&root, false).await.is_err());
        assert!(artifact.exists());

        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn removes_build_outputs_and_cache_with_exact_statistics() {
        let root = test_root("statistics");
        write_project(&root).await;
        tokio::fs::create_dir_all(root.join("build/cache/synthesis")).await.unwrap();
        tokio::fs::create_dir_all(root.join("build/bitstreams")).await.unwrap();
        tokio::fs::write(root.join("build/cache/synthesis/entry"), vec![0_u8; 1024]).await.unwrap();
        tokio::fs::write(root.join("build/bitstreams/top.fs"), vec![0_u8; 2048]).await.unwrap();

        let statistics = clean(&root, false).await.unwrap();

        assert_eq!(statistics, CleanStatistics { files: 2, bytes: 3072 });
        assert!(!root.join("build").exists());
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn cleaning_a_project_without_build_outputs_succeeds() {
        let root = test_root("empty");
        write_project(&root).await;

        assert_eq!(clean(&root, false).await.unwrap(), CleanStatistics::default());

        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn removes_a_build_symlink_without_touching_its_target() {
        let root = test_root("symlink");
        write_project(&root).await;
        let external = root.join("important.bin");
        let link = root.join("build/link.bin");
        tokio::fs::create_dir_all(link.parent().unwrap()).await.unwrap();
        tokio::fs::write(&external, b"keep").await.unwrap();
        if let Err(error) = symlink_file(&external, &link) {
            if error.kind() == ErrorKind::PermissionDenied || error.raw_os_error() == Some(1314) {
                tokio::fs::remove_dir_all(root).await.unwrap();
                return;
            }
            panic!("failed to create test symlink: {error}");
        }

        let statistics = clean(&root, false).await.unwrap();

        assert_eq!(statistics.files, 1);
        assert_eq!(tokio::fs::read(&external).await.unwrap(), b"keep");
        assert!(!root.join("build").exists());
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[test]
    fn renders_progress_and_human_readable_totals() {
        assert_eq!(progress_message(1, 4, 8), "cleaning [██░░░░░░]  25%");
        assert_eq!(progress_message(0, 0, 4), "cleaning [████] 100%");
        assert_eq!(format_file_count(1), "1 file");
        assert_eq!(format_file_count(11_741), "11,741 files");
        assert_eq!(format_bytes(5 * 1024 * 1024 * 1024), "5.0 GiB");
    }
}
