use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(unix)]
use std::time::{Duration, Instant};

const FAKE_TOOL_SOURCE: &str = include_str!("support/fake_tool.rs");

static NEXT_TEMP_DIRECTORY: AtomicU64 = AtomicU64::new(1);
static FAKE_TOOL_EXECUTABLE: OnceLock<PathBuf> = OnceLock::new();

struct TestProject {
    root: PathBuf,
    state: PathBuf,
}

impl TestProject {
    fn new(name: &str) -> Self {
        let id = NEXT_TEMP_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("bitloom-cli-{name}-{}-{id}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let state = root.join("fake-state");
        fs::create_dir_all(&state).unwrap();
        create_fake_toolchain(&root.join("toolchain"));
        write_project(&root);
        Self { root, state }
    }

    fn run(&self, arguments: &[&str], environment: &[(&str, &str)]) -> Output {
        let mut command = self.command(arguments);
        command.envs(environment.iter().copied());
        command.output().unwrap()
    }

    fn command(&self, arguments: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_bitloom"));
        command
            .current_dir(&self.root)
            .args(arguments)
            .env("BITLOOM_FAKE_STATE", &self.state)
            .env_remove("BITLOOM_FAKE_FAIL")
            .env_remove("BITLOOM_FAKE_SLEEP")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    fn clear_state(&self) {
        let _ = fs::remove_dir_all(&self.state);
        fs::create_dir_all(&self.state).unwrap();
    }

    fn calls(&self, prefix: &str) -> Vec<PathBuf> {
        let mut calls = fs::read_dir(&self.state)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .and_then(OsStr::to_str)
                    .is_some_and(|name| name.starts_with(prefix) && name.ends_with(".call"))
            })
            .collect::<Vec<_>>();
        calls.sort();
        calls
    }

    fn call_arguments(&self, name: &str) -> String {
        fs::read_to_string(self.state.join(name)).unwrap()
    }
}

impl Drop for TestProject {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn exercises_build_cache_flash_run_clean_and_logging() {
    let project = TestProject::new("workflows");

    let build = project.run(&["build", "--advanced"], &[]);
    assert_success("initial build", &build);
    assert_eq!(project.calls("yosys").len(), 1);
    assert_eq!(project.calls("nextpnr-").len(), 64);
    assert_eq!(project.calls("gowin_pack").len(), 1);
    assert_eq!(fs::read(project.root.join("build/bitstreams/top.fs")).unwrap(), b"fake bitstream");
    let log = fs::read_to_string(project.root.join("build/bitloom.log")).unwrap();
    assert!(log.contains("COMMAND [2:top/yosys]"));
    assert!(log.contains("STDOUT  [2:top/yosys] yosys fake stdout"));
    assert!(log.contains("STDERR  [2:top/yosys] yosys fake stderr"));
    assert!(log.contains("STATUS  [2:top/yosys] exited with"));

    project.clear_state();
    let cached = project.run(&["build"], &[]);
    assert_success("cached rebuild", &cached);
    assert_eq!(project.calls("gdk").len(), 1);
    assert!(project.calls("yosys").is_empty());
    assert!(project.calls("nextpnr-").is_empty());
    assert!(project.calls("gowin_pack").is_empty());
    assert!(stdout(&cached).contains("cached"));
    assert!(project.root.join("build/bitstreams/top.fs").is_file());

    project.clear_state();
    let sram = project.run(&["flash", "--sram"], &[]);
    assert_success("SRAM flash", &sram);
    assert!(project.call_arguments("openfpgaloader.call").contains("--write-sram"));

    project.clear_state();
    let flash = project.run(&["flash", "--flash"], &[]);
    assert_success("persistent flash", &flash);
    assert!(project.root.join("build/flash.bin").is_file());
    assert!(project.call_arguments("openfpgaloader.call").contains("--write-flash"));

    project.clear_state();
    let programmer_failure =
        project.run(&["flash", "--sram"], &[("BITLOOM_FAKE_FAIL", "openfpgaloader")]);
    assert!(!programmer_failure.status.success());
    assert!(stderr(&programmer_failure).contains("synthetic programmer failure"));
    assert_eq!(project.calls("openfpgaloader").len(), 1);

    project.clear_state();
    let run =
        project.run(&["run", "--sram", "--port", "BITLOOM-NO-SUCH-PORT", "--baud", "115200"], &[]);
    assert!(!run.status.success());
    assert_eq!(project.calls("openfpgaloader").len(), 1);
    assert!(stderr(&run).contains("busy, unavailable, or does not exist"));

    let clean = project.run(&["clean"], &[]);
    assert_success("clean", &clean);
    assert!(!project.root.join("build").exists());
    assert!(stdout(&clean).contains("removed"));
}

#[test]
fn recovers_from_fatal_pnr_failure_without_repeating_completed_stages() {
    let project = TestProject::new("recovery");

    let failed = project.run(&[("build")], &[("BITLOOM_FAKE_FAIL", "nextpnr-fatal")]);
    assert!(!failed.status.success());
    assert!(stderr(&failed).contains("place-and-route cannot continue"));
    assert!((1..=8).contains(&project.calls("nextpnr-").len()));
    assert_eq!(project.calls("yosys").len(), 1);
    assert!(!project.root.join("build/bitstreams/top.fs").exists());
    assert_no_files_with_prefix(&project.root.join("build/designs/top/pnr"), "candidate-");

    project.clear_state();
    let recovered = project.run(&["build"], &[]);
    assert_success("recovery build", &recovered);
    assert!(project.calls("yosys").is_empty());
    assert_eq!(project.calls("nextpnr-").len(), 64);
    assert_eq!(project.calls("gowin_pack").len(), 1);
}

#[cfg(unix)]
#[test]
fn ctrl_c_cancels_the_active_tool_and_rolls_back_partial_outputs() {
    let project = TestProject::new("cancellation");
    let mut command = project.command(&["build"]);
    command.env("BITLOOM_FAKE_SLEEP", "yosys");
    let child = command.spawn().unwrap();
    wait_for_path(&project.state.join("yosys.call"));

    let signal = Command::new("kill").args(["-INT", &child.id().to_string()]).status().unwrap();
    assert!(signal.success());
    let output = child.wait_with_output().unwrap();

    assert!(!output.status.success());
    assert!(stderr(&output).contains("cancel"));
    assert!(!project.root.join("build/designs/top/synthesis.ys").exists());
    assert!(!project.root.join("build/designs/top/synthesis.json").exists());
}

fn compiled_fake_tool() -> &'static Path {
    FAKE_TOOL_EXECUTABLE.get_or_init(|| {
        let target_tmp = option_env!("CARGO_TARGET_TMPDIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join("bitloom-fake-tool");
        fs::create_dir_all(&target_tmp).unwrap();
        let source = target_tmp.join("fake_tool.rs");
        fs::write(&source, FAKE_TOOL_SOURCE).unwrap();
        let executable = target_tmp.join(format!("fake-tool{}", std::env::consts::EXE_SUFFIX));
        let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
        let output = Command::new(rustc)
            .args([OsStr::new("--edition=2024"), source.as_os_str(), OsStr::new("-o")])
            .arg(&executable)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "failed to compile fake tool:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        executable
    })
}

fn create_fake_toolchain(root: &Path) {
    let bin = root.join("bin");
    let lib = root.join("lib");
    fs::create_dir_all(&bin).unwrap();
    fs::create_dir_all(&lib).unwrap();
    fs::write(root.join("VERSION"), b"fake-2026-08-14").unwrap();
    for (directory, name) in [
        (&bin, "yosys"),
        (&bin, "nextpnr-himbaechel"),
        (&bin, "gowin_pack"),
        (&bin, "openFPGALoader"),
        (&lib, "gdk-pixbuf-query-loaders"),
    ] {
        let destination = directory.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
        fs::hard_link(compiled_fake_tool(), &destination)
            .or_else(|_| fs::copy(compiled_fake_tool(), &destination).map(|_| ()))
            .unwrap();
    }
}

fn write_project(root: &Path) {
    fs::create_dir_all(root.join("rtl")).unwrap();
    fs::create_dir_all(root.join("constraints")).unwrap();
    let toolchain =
        root.join("toolchain").to_string_lossy().replace('\\', "\\\\").replace('"', "\\\"");
    fs::write(
        root.join("bitloom.toml"),
        format!(
            r#"[project]
name = "integration-test"
version = "0.1.0"

[rtl]
sources = ["rtl/**/*.sv"]

[toolchain]
path = "{toolchain}"

[target]
vendor = "gowin"
device = "GW2AR-LV18QN88C8/I7"

[flash]
board = "tangnano20k"

[monitor]
baud = 115200
port = "BITLOOM-NO-SUCH-PORT"

[[bitstreams]]
name = "top"
top = "top"
frequency_mhz = 27.0
constraints = ["constraints/top.cst"]
"#
        ),
    )
    .unwrap();
    fs::write(
        root.join("rtl/top.sv"),
        b"module top(input logic clk, output logic led); assign led = clk; endmodule\n",
    )
    .unwrap();
    fs::write(root.join("constraints/top.cst"), b"IO_LOC \"clk\" 4;\nIO_LOC \"led\" 15;\n")
        .unwrap();
    fs::write(
        root.join("partition.blpl"),
        b"@flash_size(1MiB);\n@offset_align(4KiB);\n\ntop:\n  offset(0x0),\n  size(auto),\n  file($top);\n",
    )
    .unwrap();
}

fn assert_success(operation: &str, output: &Output) {
    assert!(
        output.status.success(),
        "{operation} failed\nstdout:\n{}\nstderr:\n{}",
        stdout(output),
        stderr(output)
    );
}

fn assert_no_files_with_prefix(directory: &Path, prefix: &str) {
    if !directory.exists() {
        return;
    }
    assert!(!fs::read_dir(directory).unwrap().any(|entry| {
        entry.unwrap().file_name().to_str().is_some_and(|name| name.starts_with(prefix))
    }));
}

#[cfg(unix)]
fn wait_for_path(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !path.exists() {
        assert!(Instant::now() < deadline, "timed out waiting for {}", path.display());
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}
