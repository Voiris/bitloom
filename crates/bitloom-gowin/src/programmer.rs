use std::ffi::OsString;
use std::sync::Arc;

use bitloom_core::TaskContext;
use bitloom_flash::{ProgramImage, ProgramMode, ProgrammerBackend};

use crate::process::run_tool_cancellable;
use crate::{ToolError, Toolchain};

pub struct OpenFpgaloaderBackend {
    toolchain: Toolchain,
    board: Option<Arc<str>>,
    cable: Option<Arc<str>>,
}

impl OpenFpgaloaderBackend {
    pub fn new(board: Option<impl Into<Arc<str>>>, cable: Option<impl Into<Arc<str>>>) -> Self {
        Self::with_toolchain(Toolchain::default(), board, cable)
    }

    pub fn with_toolchain(
        toolchain: Toolchain,
        board: Option<impl Into<Arc<str>>>,
        cable: Option<impl Into<Arc<str>>>,
    ) -> Self {
        Self { toolchain, board: board.map(Into::into), cable: cable.map(Into::into) }
    }
}

impl ProgrammerBackend for OpenFpgaloaderBackend {
    type Error = ToolError;

    async fn program(
        &self,
        context: TaskContext,
        image: Arc<ProgramImage>,
    ) -> Result<(), Self::Error> {
        let mut command = self.toolchain.command("openFPGALoader");
        command.args(openfpgaloader_arguments(
            self.board.as_deref(),
            self.cable.as_deref(),
            &image,
        ));
        run_tool_cancellable(
            command,
            "openFPGALoader",
            context.cancellation(),
            self.toolchain.process_scope(),
            self.toolchain.process_observer(),
        )
        .await
    }
}

pub fn openfpgaloader_arguments(
    board: Option<&str>,
    cable: Option<&str>,
    image: &ProgramImage,
) -> Vec<OsString> {
    let mut arguments = Vec::new();
    if let Some(board) = board {
        arguments.extend([OsString::from("--board"), OsString::from(board)]);
    }
    if let Some(cable) = cable {
        arguments.extend([OsString::from("--cable"), OsString::from(cable)]);
    }
    arguments.push(OsString::from(match image.mode {
        ProgramMode::Sram => "--write-sram",
        ProgramMode::Flash => "--write-flash",
    }));
    arguments.push(image.path.as_os_str().to_owned());
    arguments
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use bitloom_core::OutputFingerprint;

    use super::*;

    #[test]
    fn creates_programmer_arguments() {
        let image = ProgramImage {
            path: PathBuf::from("build/flash.bin"),
            mode: ProgramMode::Flash,
            fingerprint: OutputFingerprint::new([0; 32]),
        };
        assert_eq!(
            openfpgaloader_arguments(Some("tangnano20k"), None, &image),
            ["--board", "tangnano20k", "--write-flash", "build/flash.bin"].map(OsString::from)
        );
    }
}
