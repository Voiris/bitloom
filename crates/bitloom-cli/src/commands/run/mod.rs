use anyhow::{Result, ensure};
use bitloom_core::{GraphBuilder, task};
use bitloom_flash::{BlplParseTask, FlashImageTask, NamedBitstream, ProgramTask, SramImageTask};
use bitloom_gowin::OpenFpgaloaderBackend;

use crate::cli::{FlashMode, RunArgs};
use crate::commands::{build, flash, monitor};
use crate::{execution, project};

pub async fn run(args: RunArgs) -> Result<()> {
    let mut project_config = project::load_resolved().await?;
    project::clear_build_outputs(&project_config.root).await?;
    let connection = flash::resolve_connection(
        &project_config.config,
        args.board.as_deref(),
        args.cable.as_deref(),
    )?;
    let toolchain = crate::logging::attach(
        project::resolve_toolchain(&project_config.root, &project_config.config)?,
        &project_config.root,
        args.output,
    )?;
    toolchain.refresh_gdk_cache().await?;
    let build = build::create_tasks(&project_config, toolchain.clone()).await?;
    let layout = task(BlplParseTask::new(project_config.root.join("partition.blpl")));
    let programmer = OpenFpgaloaderBackend::with_toolchain(
        toolchain.with_process_scope("program"),
        connection.board,
        connection.cable,
    );

    let graph = match args.mode() {
        FlashMode::Sram => {
            ensure!(
                build.designs.len() == 1,
                "SRAM programming requires exactly one bitstream, found {}",
                build.designs.len()
            );
            let design = &build.designs[0];
            let image = task(SramImageTask::new(layout, design.name.clone(), design.pack.clone()));
            let program = task(ProgramTask::new(image, programmer));
            GraphBuilder::new().with_root(&program).build()?
        }
        FlashMode::Flash => {
            let bitstreams = build
                .designs
                .iter()
                .map(|design| NamedBitstream::new(design.name.clone(), design.pack.clone()));
            let image = task(FlashImageTask::new(
                layout,
                bitstreams,
                project_config.root.join("build/flash.bin"),
            ));
            let program = task(ProgramTask::new(image, programmer));
            GraphBuilder::new().with_root(&program).build()?
        }
    };

    execution::run(&graph, args.output).await?;
    if args.stat {
        build::print_statistics(&build.designs).await?;
    }
    let settings = monitor::resolve_settings(
        &project_config.root,
        &mut project_config.config,
        args.port,
        args.baud,
        args.rx.map(Into::into),
        args.tx.map(Into::into),
        args.tx_line_ending.map(Into::into),
    )
    .await?;
    monitor::monitor(settings).await
}
