use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use bitloom_artifacts::Artifact;
use bitloom_core::TaskContext;
use bitloom_snapshot::{Snapshot, SnapshotFileKind};
use bitloom_synthesis::{SynthesisBackend, SynthesisOutput, SynthesizedNetlist};

use crate::filesystem::{RemoveFile, ensure_output, fingerprint_file, prepare_parent};
use crate::process::{quote_tool_path, run_tool_cancellable};
use crate::{ToolError, Toolchain};

const OPTIMIZATION_PIPELINE: &str = "proc\nopt\nfsm\nopt\nmemory\nopt\ntechmap\nopt\n";

pub struct GowinSynthesisBackend {
    toolchain: Toolchain,
    top: Arc<str>,
    family: &'static str,
    output: PathBuf,
}

impl GowinSynthesisBackend {
    pub fn new(top: impl Into<Arc<str>>, family: &'static str, output: impl Into<PathBuf>) -> Self {
        Self::with_toolchain(Toolchain::default(), top, family, output)
    }

    pub fn with_toolchain(
        toolchain: Toolchain,
        top: impl Into<Arc<str>>,
        family: &'static str,
        output: impl Into<PathBuf>,
    ) -> Self {
        Self { toolchain, top: top.into(), family, output: output.into() }
    }
}

impl SynthesisBackend for GowinSynthesisBackend {
    type Error = ToolError;

    async fn synthesize(
        &self,
        context: TaskContext,
        snapshot: Arc<Snapshot>,
    ) -> Result<SynthesisOutput, Self::Error> {
        prepare_parent(&self.output).await?;
        let script_path = self.output.with_extension("ys");
        let script = synthesis_script(&snapshot, &self.top, self.family, &self.output);
        context.defer(RemoveFile::new(script_path.clone()));
        tokio::fs::write(&script_path, script)
            .await
            .map_err(|source| ToolError::Prepare { path: script_path.clone(), source })?;
        context.defer(RemoveFile::new(self.output.clone()));

        let mut command = self.toolchain.command("yosys");
        command.arg("-s").arg(self.output.with_extension("ys"));
        run_tool_cancellable(
            command,
            "yosys",
            context.cancellation(),
            self.toolchain.process_scope(),
            self.toolchain.process_observer(),
        )
        .await?;
        ensure_output("Yosys", &self.output).await?;
        let fingerprint = fingerprint_file(&self.output).await?;
        Ok(SynthesisOutput {
            netlist: Artifact::<SynthesizedNetlist>::new(self.output.clone(), fingerprint),
        })
    }

    fn output_claims(&self) -> Vec<PathBuf> {
        vec![self.output.clone()]
    }
}

fn synthesis_script(snapshot: &Snapshot, top: &str, family: &str, output: &Path) -> String {
    let sources = snapshot
        .files
        .iter()
        .filter(|file| file.kind == SnapshotFileKind::Source)
        .map(|file| snapshot.root.path().join(&file.relative_path))
        .collect::<Vec<_>>();
    let include_dirs = sources.iter().filter_map(|source| source.parent()).collect::<BTreeSet<_>>();
    let mut script = String::new();
    for source in &sources {
        script.push_str("read_verilog");
        if source.extension() == Some(OsStr::new("sv")) {
            script.push_str(" -sv");
        }
        for include_dir in &include_dirs {
            script.push_str(" -I ");
            script.push_str(&quote_tool_path(include_dir));
        }
        script.push(' ');
        script.push_str(&quote_tool_path(source));
        script.push('\n');
    }
    script.push_str("hierarchy -top ");
    script.push_str(top);
    script.push('\n');
    script.push_str(OPTIMIZATION_PIPELINE);
    script.push_str("synth_gowin -top ");
    script.push_str(top);
    script.push_str(" -family ");
    script.push_str(family);
    script.push_str(" -json ");
    script.push_str(&quote_tool_path(output));
    script.push('\n');
    script
}

#[cfg(test)]
mod tests {
    use bitloom_core::OutputFingerprint;
    use bitloom_snapshot::{SnapshotDirectory, SnapshotFile};

    use super::*;

    fn snapshot(root: &str, files: &[(&str, SnapshotFileKind)]) -> Snapshot {
        Snapshot {
            root: Artifact::<SnapshotDirectory>::new(root, OutputFingerprint::new([0; 32])),
            source_fingerprint: OutputFingerprint::new([0; 32]),
            files: files
                .iter()
                .map(|(path, kind)| SnapshotFile {
                    relative_path: PathBuf::from(path),
                    kind: *kind,
                    fingerprint: OutputFingerprint::new([0; 32]),
                })
                .collect(),
        }
    }

    #[test]
    fn adds_every_unique_source_directory_to_read_verilog() {
        let snapshot = snapshot(
            "build/snapshot",
            &[
                ("rtl/core/top.sv", SnapshotFileKind::Source),
                ("rtl/core/helper.v", SnapshotFileKind::Source),
                ("rtl/shared/reset.v", SnapshotFileKind::Source),
                ("constraints/top.cst", SnapshotFileKind::Constraint),
            ],
        );

        let script = synthesis_script(&snapshot, "top", "gw2a", Path::new("synthesis.json"));
        let read_commands =
            script.lines().filter(|line| line.starts_with("read_verilog")).collect::<Vec<_>>();

        assert_eq!(read_commands.len(), 3);
        for command in read_commands {
            assert_eq!(command.matches(" -I ").count(), 2);
            assert!(command.contains("-I \"build/snapshot/rtl/core\""));
            assert!(command.contains("-I \"build/snapshot/rtl/shared\""));
            assert!(!command.contains("constraints"));
        }
    }

    #[test]
    fn quotes_include_directories_containing_spaces() {
        let snapshot =
            snapshot("build output/snapshot", &[("rtl modules/top.sv", SnapshotFileKind::Source)]);

        let script = synthesis_script(&snapshot, "top", "gw2a", Path::new("synthesis.json"));

        assert!(script.contains("-I \"build output/snapshot/rtl modules\""));
        assert!(script.contains("\"build output/snapshot/rtl modules/top.sv\""));
    }

    #[test]
    fn applies_optimization_pipeline_before_gowin_synthesis() {
        let snapshot = snapshot("build/snapshot", &[("rtl/top.sv", SnapshotFileKind::Source)]);

        let script = synthesis_script(&snapshot, "top", "gw2a", Path::new("synthesis.json"));
        let commands = script.lines().skip(1).collect::<Vec<_>>();

        assert_eq!(
            commands,
            [
                "hierarchy -top top",
                "proc",
                "opt",
                "fsm",
                "opt",
                "memory",
                "opt",
                "techmap",
                "opt",
                "synth_gowin -top top -family gw2a -json \"synthesis.json\"",
            ]
        );
    }
}
