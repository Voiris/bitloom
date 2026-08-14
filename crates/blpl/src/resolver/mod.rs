mod constant;
mod context;
mod directive;
mod packing;
mod partition;
pub mod resolved;
mod value;

use std::path::Path;

use constant::resolve_constants;
use context::Context;
use directive::resolve_directives;
use packing::pack_layout;
use partition::resolve_partitions;

use crate::CompileOptions;
use crate::diagnostics::{DiagResult, DiagSuccess, Diagnostic};
use crate::parser::unresolved::UnresolvedLayout;

pub(super) fn resolve_layout<'src>(
    layout: UnresolvedLayout<'src>,
    options: &CompileOptions,
    base_dir: &Path,
) -> DiagResult<resolved::Layout> {
    let mut context = Context::from_options(options, base_dir);
    let mut warnings = Vec::new();

    let constants = resolve_constants(&layout.constants, &mut context)?;
    append_warnings(&mut warnings, constants.warnings);

    let directives = resolve_directives(&layout.directives, &mut context)?;
    append_warnings(&mut warnings, directives.warnings);

    let partitions = resolve_partitions(&layout.partitions, &context)?;
    append_warnings(&mut warnings, partitions.warnings);

    let packed = pack_layout(partitions.value, &context.flash, &context.layout)?;
    append_warnings(&mut warnings, packed.warnings);

    Ok(DiagSuccess::with_warnings(packed.value, warnings))
}

fn append_warnings(target: &mut Vec<Diagnostic>, warnings: Option<Vec<Diagnostic>>) {
    if let Some(mut warnings) = warnings {
        target.append(&mut warnings);
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::Source;
    use crate::parser::parse_source;

    static NEXT_TEMP_DIR: AtomicU64 = AtomicU64::new(0);

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let id = NEXT_TEMP_DIR.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("blpl-resolver-{}-{id}", std::process::id(),));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).expect("test directory should be created");
            Self(path)
        }

        fn file(&self, name: &str, contents: &[u8]) -> PathBuf {
            let path = self.0.join(name);
            fs::write(&path, contents).expect("test file should be written");
            path
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn path_literal(path: &Path) -> String {
        path.to_string_lossy().into_owned()
    }

    fn source(content: String, directory: &TestDir) -> Source {
        Source { path: directory.0.join("layout.blpl"), content }
    }

    fn resolve_source(source: &Source) -> DiagResult<resolved::Layout> {
        crate::compile(source, &CompileOptions::default())
    }

    #[test]
    fn resolves_auto_placement_alignment_file_size_and_magic() {
        let directory = TestDir::new();
        let first = directory.file("first.bin", &[1, 2, 3]);
        let second = directory.file("second.bin", &[1, 2, 3, 4, 5]);
        let source = source(
            format!(
                "@flash_size(64B);\n\
                 @offset_align(8B);\n\
                 first: size(auto), file(\"{}\");\n\
                 second: offset(auto), size(8B), magic(sized(0x1234, 4B)), file(\"{}\");",
                path_literal(&first),
                path_literal(&second),
            ),
            &directory,
        );

        let resolved = resolve_source(&source).expect("layout should resolve");

        assert!(resolved.warnings.is_none());
        assert_eq!(resolved.value.partitions.len(), 2);
        assert_eq!(resolved.value.partitions[0].offset, 0);
        assert_eq!(resolved.value.partitions[0].size, 3);
        assert_eq!(resolved.value.partitions[1].offset, 8);
        assert_eq!(resolved.value.partitions[1].size, 8);
        assert_eq!(&*resolved.value.partitions[1].magic, &[0, 0, 0x12, 0x34]);
    }

    #[test]
    fn public_api_resolves_relative_partition_paths() {
        let directory = TestDir::new();
        let file = directory.file("data.bin", &[1, 2, 3]);
        let source = source("data: file(\"data.bin\");".to_owned(), &directory);

        let resolved = crate::compile(&source, &CompileOptions::new()).unwrap();

        assert!(matches!(
            &resolved.value.partitions[0].source,
            resolved::Source::File(path) if path == &file
        ));
    }

    #[test]
    fn public_api_supplies_builtin_partition_paths() {
        let directory = TestDir::new();
        let file = directory.file("bitstream.bin", &[1, 2, 3]);
        let source = source("bitstream: file($bitstream);".to_owned(), &directory);
        let options = CompileOptions::new().with_builtin_path("bitstream", &file);

        let resolved = crate::compile(&source, &options).unwrap();

        assert!(matches!(
            &resolved.value.partitions[0].source,
            resolved::Source::File(path) if path == &file
        ));
    }

    #[test]
    fn warns_and_sorts_partitions_declared_out_of_offset_order() {
        let directory = TestDir::new();
        let first = directory.file("first.bin", &[1]);
        let second = directory.file("second.bin", &[2]);
        let source = source(
            format!(
                "first: offset(16B), size(auto), file(\"{}\");\n\
                 second: offset(0B), size(auto), file(\"{}\");",
                path_literal(&first),
                path_literal(&second),
            ),
            &directory,
        );

        let resolved = resolve_source(&source).expect("out-of-order layout should still resolve");
        let warnings = resolved.warnings.expect("offset order should produce a warning");

        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].severity, crate::diagnostics::Severity::Warning);
        assert_eq!(resolved.value.partitions[0].name, "second");
        assert_eq!(resolved.value.partitions[1].name, "first");
    }

    #[test]
    fn rejects_partition_size_crossing_the_next_offset() {
        let directory = TestDir::new();
        let first = directory.file("first.bin", &[1]);
        let second = directory.file("second.bin", &[2]);
        let source = source(
            format!(
                "first: offset(0B), size(16B), file(\"{}\");\n\
                 second: offset(8B), size(1B), file(\"{}\");",
                path_literal(&first),
                path_literal(&second),
            ),
            &directory,
        );

        let error = resolve_source(&source).expect_err("overlapping partitions should fail");

        assert!(error.message.contains("overlaps"));
    }

    #[test]
    fn reports_missing_file_on_file_clarification() {
        let directory = TestDir::new();
        let missing = directory.0.join("missing.bin");
        let source = source(format!("data: file(\"{}\");", path_literal(&missing)), &directory);
        let parsed = parse_source(&source).expect("layout should parse");
        let file_span = parsed.value.partitions[0].inner.clarifications[0].inner.name.span;

        let error = crate::compile(&source, &CompileOptions::default())
            .expect_err("missing file should fail");

        assert!(error.message.starts_with("cannot read partition file"));
        assert_eq!(error.labels[0].span, file_span);
    }

    #[test]
    fn magic_does_not_make_an_empty_partition_non_empty() {
        let directory = TestDir::new();
        let empty = directory.file("empty.bin", &[]);
        let source =
            source(format!("data: magic(0x12), file(\"{}\");", path_literal(&empty),), &directory);

        let error = resolve_source(&source).expect_err("empty content source should fail");

        assert!(error.message.ends_with("has an empty content source"));
        assert!(error.note.as_deref().is_some_and(|note| note.contains("magic")));
    }

    #[test]
    fn magic_is_not_a_partition_content_source() {
        let directory = TestDir::new();
        let source = source("data: magic(0x12);".to_owned(), &directory);

        let error = resolve_source(&source).expect_err("partition without a source should fail");

        assert!(error.message.ends_with("has no content source"));
        assert!(
            error
                .help
                .as_deref()
                .is_some_and(|help| help.contains("currently") && help.contains("file"))
        );
    }

    #[test]
    fn rejects_file_larger_than_partition_size() {
        let directory = TestDir::new();
        let file = directory.file("data.bin", &[1, 2, 3, 4]);
        let source =
            source(format!("data: size(2B), file(\"{}\");", path_literal(&file)), &directory);

        let error = resolve_source(&source).expect_err("undersized partition should fail");

        assert!(error.message.starts_with("content source does not fit"));
    }

    #[test]
    fn rejects_partition_extending_beyond_flash_size() {
        let directory = TestDir::new();
        let file = directory.file("data.bin", &[0; 8]);
        let source = source(
            format!(
                "@flash_size(16B);\n\
                 data: offset(12B), size(auto), file(\"{}\");",
                path_literal(&file),
            ),
            &directory,
        );

        let error = resolve_source(&source).expect_err("partition beyond flash should fail");

        assert!(error.message.ends_with("exceeds the flash size"));
    }

    #[test]
    fn rejects_explicit_offset_not_matching_alignment() {
        let directory = TestDir::new();
        let file = directory.file("data.bin", &[1]);
        let source = source(
            format!(
                "@offset_align(4B);\n\
                 data: offset(2B), file(\"{}\");",
                path_literal(&file),
            ),
            &directory,
        );

        let error = resolve_source(&source).expect_err("misaligned offset should fail");

        assert!(error.message.ends_with("offset is not aligned"));
    }
}
