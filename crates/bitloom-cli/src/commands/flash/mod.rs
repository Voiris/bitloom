use std::sync::Arc;

use anyhow::{Result, ensure};
use bitloom_core::{GraphBuilder, task};
use bitloom_flash::{
    BlplParseTask, ExistingBitstreamTask, FlashImageTask, NamedBitstream, ProgramTask,
    SramImageTask,
};
use bitloom_gowin::OpenFpgaloaderBackend;

use crate::cli::{FlashArgs, FlashMode};
use crate::{execution, project};

pub(super) struct Connection {
    pub board: Option<Arc<str>>,
    pub cable: Option<Arc<str>>,
}

pub async fn run(args: FlashArgs) -> Result<()> {
    let (root, config) = project::load_config(false).await?;
    let connection = resolve_connection(&config, args.board.as_deref(), args.cable.as_deref())?;
    let layout = task(BlplParseTask::new(root.join("partition.blpl")));
    let existing = config
        .bitstreams
        .iter()
        .map(|item| {
            (
                Arc::<str>::from(item.name.as_str()),
                task(ExistingBitstreamTask::new(
                    &item.name,
                    project::bitstream_path(&root, &item.name),
                )),
            )
        })
        .collect::<Vec<_>>();
    let programmer = OpenFpgaloaderBackend::with_toolchain(
        crate::logging::attach(project::resolve_toolchain(&root, &config)?, &root, args.output)?
            .with_process_scope("program"),
        connection.board,
        connection.cable,
    );

    let graph = match args.mode() {
        FlashMode::Sram => {
            ensure!(
                existing.len() == 1,
                "SRAM programming requires exactly one bitstream, found {}",
                existing.len()
            );
            let image =
                task(SramImageTask::new(layout, existing[0].0.clone(), existing[0].1.clone()));
            let program = task(ProgramTask::new(image, programmer));
            GraphBuilder::new().with_root(&program).build()?
        }
        FlashMode::Flash => {
            let bitstreams = existing
                .iter()
                .map(|(name, bitstream)| NamedBitstream::new(name.clone(), bitstream.clone()));
            let image = task(FlashImageTask::new(layout, bitstreams, root.join("build/flash.bin")));
            let program = task(ProgramTask::new(image, programmer));
            GraphBuilder::new().with_root(&program).build()?
        }
    };
    execution::run(&graph, args.output).await?;
    Ok(())
}

pub(super) fn resolve_connection(
    config: &bitloom_config::ProjectConfig,
    board_override: Option<&str>,
    cable_override: Option<&str>,
) -> Result<Connection> {
    let (board, cable) = if let Some(board) = board_override {
        (Some(board), None)
    } else if let Some(cable) = cable_override {
        (None, Some(cable))
    } else {
        (nonempty(&config.flash.board), nonempty(&config.flash.cable))
    };
    ensure!(board.is_some() || cable.is_some(), "flash board or cable is not configured");
    Ok(Connection { board: board.map(Arc::from), cable: cable.map(Arc::from) })
}

fn nonempty(value: &str) -> Option<&str> {
    (!value.trim().is_empty()).then_some(value)
}
