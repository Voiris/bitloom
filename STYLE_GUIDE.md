# Bitloom Style Guide

This guide documents the conventions used in the Bitloom codebase. It
supplements `rustfmt` and Clippy with project-specific rules for structure,
errors, filesystem operations, external tools, and tests.

When existing code and this guide disagree, prefer making the smallest
consistent change and update this document if the convention has intentionally
changed.

## Tooling

The default Rust channel and required components are selected in
`rust-toolchain.toml`. Formatting rules are defined in `rustfmt.toml` and use
options that require nightly rustfmt. Install it separately with:

```powershell
rustup toolchain install nightly --profile minimal --component rustfmt
```

Format and validate changes with:

```powershell
cargo +nightly fmt
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

Do not override the repository's rustfmt settings or add manual formatting that
`cargo +nightly fmt` will remove.

## Language and naming

Write source code, identifiers, comments, log messages, errors, tests, and
documentation in English.

Follow standard Rust naming:

- modules, functions, methods, and variables use `snake_case`;
- structs, enums, and enum variants use `PascalCase`;
- constants use `SCREAMING_SNAKE_CASE`;
- command argument types end in `Args`;
- configuration types end in `Config`;
- resolved configuration types start with `Resolved`;
- error enums end in `Error`.

Prefer complete, domain-specific names such as `constraints_path`,
`toolchain_version`, and `build_mode`. Short names are appropriate only when
their meaning is conventional and local, such as `err`, `path`, or `config`.

Use the terminology shown to users consistently:

- **synthesis** for the Yosys stage;
- **place-and-route** or **PNR** for the nextpnr stage;
- **packing** for bitstream generation;
- **programming** or **flashing** for writing the FPGA;
- `dev` and `release` for build profiles.

## Imports

Import ordering, grouping, and granularity are controlled by `rustfmt.toml`.
Treat the output of `cargo +nightly fmt` as the source of truth instead of
arranging imports manually.

Avoid glob imports outside tests. A test module may use `use super::*` when it
is testing the adjacent implementation.

## Module structure

Keep each CLI command in `src/commands/<command>/mod.rs`. A command exposes an
async entry point with this shape:

```rust
pub async fn run(args: CommandArgs) -> anyhow::Result<()> {
    // ...
}
```

Use this order within a module where practical:

1. child module declarations;
2. imports;
3. constants;
4. private data types;
5. public or crate-visible entry points;
6. private helpers;
7. the `#[cfg(test)]` test module.

Keep visibility as narrow as possible:

- use private items by default;
- use `pub(super)` for communication with a parent module;
- use `pub(crate)` for APIs shared across the binary;
- use `pub` only for an intentionally public crate-level API.

Split a module when a section represents a distinct responsibility or has its
own data model. Do not create a module solely to shorten a small file.

## Control flow

Prefer early validation and early returns over deeply nested branches.

Use `let ... else` when the alternative exits the current flow:

```rust
let Some(candidate) = candidate else {
    failed_runs += 1;
    continue;
};
```

Use exhaustive `match` expressions for enums. Do not add wildcard arms merely
to avoid handling a new variant.

Use iterator methods when they make the transformation clearer. Prefer an
explicit loop when it must perform several operations, maintain state, or
attach detailed error context.

Do not use `unwrap()`, `expect()`, `panic!()`, `todo!()`, or `unimplemented!()`
on user-controlled or runtime paths. Return a descriptive error instead.
`expect()` is acceptable only for a local invariant that cannot be invalidated
by configuration or external state.

## Error handling

Use `anyhow::Result` at command, orchestration, filesystem, and external-tool
boundaries.

Use `thiserror` for domain errors that callers may need to distinguish, such as
device parsing and support errors.

Choose the error mechanism according to intent:

- `.context(...)` for a fixed high-level explanation;
- `.with_context(...)` when the message includes a path or other runtime data;
- `ensure!(...)` for validation and required postconditions;
- `anyhow::bail!(...)` for an unsupported branch or failed operation that needs
  a custom message;
- `?` when the existing error already has sufficient context.

Include the affected path in filesystem errors:

```rust
tokio::fs::read(&path)
    .await
    .with_context(|| format!("failed to read {}", path.display()))?;
```

Error messages:

- start with lowercase unless they begin with a proper name such as `Yosys`;
- describe the failed operation or violated requirement;
- quote user-provided identifiers with backticks;
- include process exit status when an external command fails;
- do not end with a period.

An unsupported feature must return an error. It must not panic:

```rust
anyhow::bail!("Gowin toolchain support is not implemented")
```

Cleanup after an existing primary error may be best-effort:

```rust
let _ = remove_file_if_exists(&temp_path).await;
```

Do not discard an error if it is the primary operation or if continuing could
leave the project in an unknown state.

## Async and filesystem operations

Use `tokio::fs` inside async workflows. Synchronous filesystem access is
acceptable for small operations in explicitly synchronous infrastructure, such
as initializing the process-wide logger.

Use helpers from `crate::filesystem` for repeated operations such as:

- creating parent directories and writing a file;
- removing a file or directory if it exists;
- promoting a completed temporary artifact.

Project input paths must remain inside the project root. Reject absolute paths,
parent components (`..`), and paths into the generated `build` directory.

Generated output must not replace a valid artifact until the new output is
complete:

1. remove a stale temporary path;
2. write or generate the temporary artifact;
3. verify that the operation succeeded;
4. move the temporary artifact into its final location;
5. clean up temporary or backup paths.

Prefix temporary artifacts with a dot and suffix them with `.tmp`, for example
`.netlist.json.tmp`.

## Configuration

Configuration models belong in `src/config/models.rs`; default values belong in
`src/config/defaults.rs`; serialization and loading belong in
`src/config/io.rs`.

Configuration structs should use `#[serde(deny_unknown_fields)]`. Add
`#[serde(default)]` only when omission has a documented and safe meaning.

Keep serialized names stable and user-oriented. Use lowercase enum values in
TOML unless compatibility with an external tool requires another spelling.

Separate:

- values written by the user;
- global backend defaults;
- profile-specific overrides;
- fully resolved values consumed by build stages.

Resolve defaults once before invoking a tool. Tool argument construction should
not reinterpret optional configuration independently.

When a configuration value affects a build stage, include it in that stage's
fingerprint.

## External tools

Build external commands with `tokio::process::Command` and execute them through
`crate::process::run` so stdout, stderr, and logging remain consistent.

Construct arguments in a pure helper when they contain non-trivial branching:

```rust
fn arguments(/* ... */) -> Vec<OsString> {
    // ...
}
```

This helper should be unit-tested without launching the actual tool.

Pass paths as `Path` or `OsString` for process arguments. Convert paths to
strings only when generating a tool-specific script or embedded option that
requires text.

For every tool invocation:

1. validate required inputs;
2. log the stage being started;
3. use the configured working directory and environment;
4. check the exit status;
5. verify that expected outputs exist;
6. publish temporary outputs only after validation;
7. log the final artifact.

Use the executable's proper name consistently in messages, such as `Yosys`,
`nextpnr`, `gowin_pack`, and `openFPGALoader`.

## Output and logging

User-facing output goes through `crate::output`. Do not call `println!` or
`eprintln!` directly outside the output implementation.

Interactive terminal renderers may obtain a formatter from `crate::output` and
write the resulting `Display` value through their own writer when they must
control cursor placement and line endings. Forwarded raw device bytes are not
Bitloom messages and must remain unformatted.

Use levels consistently:

- `info` for progress and selected settings;
- `success` for a completed stage or command;
- `warning` for a recoverable problem;
- `error` at the top-level command boundary;
- `tool` for forwarded process output.

Messages should be concise and actionable. Include output paths when announcing
completed artifacts.

## Tests

Keep unit tests next to the implementation in a `#[cfg(test)] mod tests`
module.

Name tests after observable behavior:

```rust
fn rejects_board_and_cable_together()
fn selects_the_earliest_invalidated_build_stage()
fn combines_snapshot_cst_files_in_stable_order()
```

Prefer testing:

- accepted and rejected configuration;
- CLI conflicts and defaults;
- pure command argument construction;
- boundary values and malformed input;
- deterministic ordering;
- cache invalidation;
- cleanup and artifact replacement.

`unwrap()` and `expect()` are acceptable in tests when failure should fail the
test immediately. Use `expect()` when its message materially improves the
failure report.

Tests that touch the filesystem must:

- operate inside a dedicated temporary directory;
- use a unique path;
- remove stale state before starting;
- clean up after success;
- never depend on a developer's installed toolchain or hardware.

Do not weaken assertions to accommodate nondeterministic output. Make the
production behavior deterministic instead.

## Comments and documentation

Prefer code that expresses its intent through types, names, and small helpers.
Add a comment when it explains:

- why a non-obvious constraint exists;
- an external toolchain requirement;
- a safety or recovery invariant;
- behavior that would otherwise look accidental.

Do not add comments that merely restate the next line of code.

Use `///` documentation for crate-level APIs or domain types whose contract is
not evident from their fields and signatures. Internal implementation helpers
usually do not need API documentation.

Update `README.md` when a change affects installation, supported devices,
commands, generated project structure, or `bitloom.toml`.

## Scope of changes

Keep refactoring separate from behavioral changes when practical. Avoid
unrelated formatting or renaming in a focused fix.

New code should follow this guide. Existing code may be brought into alignment
incrementally when it is already being modified.
