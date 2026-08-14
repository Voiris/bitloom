use std::borrow::Cow;
use std::convert::Infallible;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bitloom_core::{
    Dependencies, Executor, GraphBuilder, Task, TaskContext, TaskKind, TaskPhase, TaskRef, task,
};

use super::*;
use crate::blpl::render_blpl_diagnostic;

#[test]
fn legacy_parser_accepts_constants_expressions_magic_and_file_sources() {
    let source = ::blpl::Source::new(
        "partition.blpl",
        "const BASE = 0x1000;\n\
         @flash_size(8MiB);\n\
         @erase_size(default);\n\
         @offset_align(auto);\n\
         top: offset(BASE + 4KiB), size(auto), magic(sized(0x1234, 4B)), file($top);\n\
         firmware: offset(auto), size(64KiB), file(\"firmware.bin\");",
    );

    let parsed = ::blpl::parse(&source).expect("legacy BLPL grammar should parse");
    assert!(parsed.warnings.is_none());
}

#[test]
fn ariadne_uses_the_legacy_parser_span_for_a_missing_semicolon() {
    let contents = "top:\n  offset(0x0),\n  size(auto),\n  file($top)";
    let source = ::blpl::Source::new("partition.blpl", contents);
    let diagnostic = ::blpl::parse(&source).unwrap_err();
    let rendered = render_blpl_diagnostic(Path::new("partition.blpl"), contents, &diagnostic);

    assert_eq!(diagnostic.labels.len(), 1);
    assert_eq!(diagnostic.labels[0].span.into_range(), contents.len()..contents.len());
    assert!(rendered.contains("or ';'"), "{rendered}");
    assert!(!rendered.contains("unterminated statement starts here"));
}

#[tokio::test]
async fn executor_keeps_the_legacy_ariadne_report_when_erasing_the_task_type() {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let path = std::env::temp_dir().join(format!("bitloom-invalid-blpl-{nonce}.blpl"));
    let source = "top: offset(0x0), size(auto), file($top)";
    tokio::fs::write(&path, source).await.unwrap();

    let layout = task(BlplParseTask::new(&path));
    let graph = GraphBuilder::new().with_root(&layout).build().unwrap();
    let rendered = Executor::default().run(&graph).await.unwrap_err().to_string();

    assert!(rendered.contains(&path.display().to_string()));
    assert!(rendered.contains("or ';'"), "{rendered}");
    assert!(!rendered.contains("unterminated statement starts here"));
    tokio::fs::remove_file(path).await.unwrap();
}

#[tokio::test]
async fn flash_task_resolves_legacy_blpl_against_built_bitstreams() {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let directory = std::env::temp_dir().join(format!("bitloom-legacy-blpl-{nonce}"));
    tokio::fs::create_dir_all(&directory).await.unwrap();
    let bitstream_path = directory.join("top.fs");
    let firmware_path = directory.join("firmware.bin");
    let layout_path = directory.join("partition.blpl");
    let image_path = directory.join("flash.bin");
    tokio::fs::write(&bitstream_path, [1, 2, 3]).await.unwrap();
    tokio::fs::write(&firmware_path, [4, 5]).await.unwrap();
    tokio::fs::write(
        &layout_path,
        "@flash_size(64B);\n\
         @offset_align(4B);\n\
         top: offset(0x0), size(4B), magic(0xAA), file($top);\n\
         firmware: offset(auto), size(auto), file(\"firmware.bin\");",
    )
    .await
    .unwrap();

    let layout = task(BlplParseTask::new(&layout_path));
    let bitstream = task(ExistingBitstreamTask::new("top", &bitstream_path));
    let image =
        task(FlashImageTask::new(layout, [NamedBitstream::new("top", bitstream)], &image_path));
    let graph = GraphBuilder::new().with_root(&image).build().unwrap();

    Executor::default().run(&graph).await.unwrap();
    assert_eq!(tokio::fs::read(&image_path).await.unwrap(), [0xaa, 2, 3, 0xff, 4, 5]);
    tokio::fs::remove_dir_all(directory).await.unwrap();
}

#[tokio::test]
async fn executor_keeps_early_and_late_blpl_warnings_without_duplicates() {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let directory = std::env::temp_dir().join(format!("bitloom-warning-blpl-{nonce}"));
    tokio::fs::create_dir_all(&directory).await.unwrap();
    let bitstream_path = directory.join("top.fs");
    let layout_path = directory.join("partition.blpl");
    let image_path = directory.join("flash.bin");
    tokio::fs::write(&bitstream_path, [1, 2, 3]).await.unwrap();
    tokio::fs::write(
        &layout_path,
        "top: offset(0x0), size(auto), file($top);\n\
         @offset_align(4B);\n\
         @flash_size(64B);",
    )
    .await
    .unwrap();

    let layout = task(BlplParseTask::new(&layout_path));
    let bitstream = task(ExistingBitstreamTask::new("top", &bitstream_path));
    let image = task(FlashImageTask::new(
        layout.clone(),
        [NamedBitstream::new("top", bitstream)],
        &image_path,
    ));
    let graph = GraphBuilder::new().with_root(&image).build().unwrap();
    let report = Executor::default().run(&graph).await.unwrap();

    let parse_warnings = layout.snapshot().warnings;
    assert_eq!(parse_warnings.len(), 2);
    assert!(
        parse_warnings
            .iter()
            .all(|warning| warning.contains("directive declared after partitions"))
    );
    assert!(parse_warnings.iter().all(|warning| !warning.contains('\u{1b}')));

    let compile_warnings = image.snapshot().warnings;
    assert_eq!(compile_warnings.len(), 1);
    assert!(compile_warnings[0].contains("directive `flash_size` is out of order"));
    assert_eq!(report.tasks.iter().map(|task| task.warnings.len()).sum::<usize>(), 3);

    tokio::fs::remove_dir_all(directory).await.unwrap();
}

struct SlowBuildTask;

impl Task for SlowBuildTask {
    type Output = ();
    type Error = Infallible;
    type Dependencies = ();

    const KIND: TaskKind = TaskKind::new("test.slow-build");

    fn label(&self) -> Cow<'static, str> {
        "Slow build".into()
    }

    fn dependencies(&self) -> &Self::Dependencies {
        &()
    }

    async fn run(&self, _context: TaskContext, (): ()) -> Result<Self::Output, Self::Error> {
        tokio::time::sleep(Duration::from_millis(250)).await;
        Ok(())
    }
}

struct JoinTask {
    dependencies: (TaskRef<SlowBuildTask>, TaskRef<BlplParseTask>),
}

impl Task for JoinTask {
    type Output = ();
    type Error = Infallible;
    type Dependencies = (TaskRef<SlowBuildTask>, TaskRef<BlplParseTask>);

    const KIND: TaskKind = TaskKind::new("test.join");

    fn label(&self) -> Cow<'static, str> {
        "Join".into()
    }

    fn dependencies(&self) -> &Self::Dependencies {
        &self.dependencies
    }

    async fn run(
        &self,
        _context: TaskContext,
        _inputs: <Self::Dependencies as Dependencies>::Resolved,
    ) -> Result<Self::Output, Self::Error> {
        Ok(())
    }
}

#[tokio::test]
async fn parses_layout_while_an_independent_build_branch_is_running() {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let path = std::env::temp_dir().join(format!("bitloom-parallel-blpl-{nonce}.blpl"));
    tokio::fs::write(&path, "top: offset(0x0), size(auto), file($top);").await.unwrap();
    let build = task(SlowBuildTask);
    let layout = task(BlplParseTask::new(&path));
    let join = task(JoinTask { dependencies: (build.clone(), layout.clone()) });
    let graph = GraphBuilder::new().with_root(&join).build().unwrap();
    let executor = Executor::default();
    let execution = executor.run(&graph);
    tokio::pin!(execution);

    tokio::select! {
        result = &mut execution => panic!("graph finished before concurrency could be observed: {result:?}"),
        () = tokio::time::sleep(Duration::from_millis(50)) => {}
    }
    assert_eq!(layout.snapshot().phase, TaskPhase::Finished);
    assert_eq!(build.snapshot().phase, TaskPhase::Running);
    execution.await.unwrap();

    tokio::fs::remove_file(path).await.unwrap();
}
