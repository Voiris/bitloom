use std::path::PathBuf;
use std::sync::Arc;

use bitloom_artifacts::Artifact;
use bitloom_core::TaskContext;
use bitloom_pack::{PackBackend, PackOutput, PackedBitstream};

use crate::filesystem::{RemoveFile, ensure_output, fingerprint_file, prepare_parent};
use crate::process::run_tool_cancellable;
use crate::{RoutedNetlist, ToolError, Toolchain};

pub struct GowinPackBackend {
    toolchain: Toolchain,
    device: &'static str,
    output: PathBuf,
}

impl GowinPackBackend {
    pub fn new(device: &'static str, output: impl Into<PathBuf>) -> Self {
        Self::with_toolchain(Toolchain::default(), device, output)
    }

    pub fn with_toolchain(
        toolchain: Toolchain,
        device: &'static str,
        output: impl Into<PathBuf>,
    ) -> Self {
        Self { toolchain, device, output: output.into() }
    }
}

impl PackBackend<Artifact<RoutedNetlist>> for GowinPackBackend {
    type Error = ToolError;

    async fn pack(
        &self,
        context: TaskContext,
        routed: Arc<bitloom_pnr::PnrOutput<Artifact<RoutedNetlist>>>,
    ) -> Result<PackOutput, Self::Error> {
        prepare_parent(&self.output).await?;
        context.defer(RemoveFile::new(self.output.clone()));
        let mut command = self.toolchain.command("gowin_pack");
        command
            .arg("--device")
            .arg(self.device)
            .arg("--output")
            .arg(&self.output)
            .arg(routed.selected.artifact.path());
        run_tool_cancellable(
            command,
            "gowin_pack",
            context.cancellation(),
            self.toolchain.process_scope(),
            self.toolchain.process_observer(),
        )
        .await?;
        ensure_output("gowin_pack", &self.output).await?;
        let fingerprint = fingerprint_file(&self.output).await?;
        Ok(PackOutput {
            bitstream: Artifact::<PackedBitstream>::new(self.output.clone(), fingerprint),
        })
    }

    fn output_claims(&self) -> Vec<PathBuf> {
        vec![self.output.clone()]
    }
}
