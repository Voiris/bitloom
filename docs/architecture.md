# Graph architecture

## Core decision

Bitloom separates a statically typed data plane from a type-erased control
plane.

```text
TaskRef<T> -> TaskNode<T> -> OnceCell<Result<Arc<T::Output>, Arc<TaskFailure>>>
                    |
                    +---- dyn ErasedNode -> StableDag -> Executor
```

`TaskRef<T>` is the only way consumers read an output. The graph never stores
an output as `Any`, never downcasts, and has no central task/result enum. The
object-safe `ErasedNode` interface exposes only scheduling operations, metadata,
state, fingerprints and rollback.

Each stage owns its public output types: snapshot types live in
`bitloom-snapshot`, synthesis output types in `bitloom-synthesis`, and packed
bitstream types in `bitloom-pack`. `bitloom-artifacts` contains only the generic
`Artifact<K>` path-and-fingerprint reference. Backend-specific artifact kinds,
such as the routed Gowin netlist, stay with their backend.

Each call to `task(value)` creates a new node identity, state cell and result
cell. Therefore the same Rust task type may occur many times with different
parameters. Reusing one cloned `TaskRef<T>` in many consumers creates one node
with many outgoing edges.

## Defining a task

```rust
use std::borrow::Cow;
use std::convert::Infallible;
use std::sync::Arc;

use bitloom_core::{Task, TaskContext, TaskKind, TaskRef, task_dependencies};

struct SnapshotTask;

impl Task for SnapshotTask {
    type Output = Snapshot;
    type Error = SnapshotError;
    type Dependencies = ();

    const KIND: TaskKind = TaskKind::new("bitloom.snapshot");
    const CACHE_SCHEMA: u32 = 1;

    fn label(&self) -> Cow<'static, str> {
        "Load sources".into()
    }

    fn dependencies(&self) -> &() {
        &()
    }

    async fn run(&self, context: TaskContext, (): ()) -> Result<Snapshot, SnapshotError> {
        // Build into staging and register cleanup before publishing artifacts.
        Ok(Snapshot::new())
    }
}

task_dependencies! {
    struct SynthesisDependencies => SynthesisInputs {
        snapshot: TaskRef<SnapshotTask>,
    }
}
```

The macro handles named sets of direct task references. `Dependencies` can be
implemented manually for conditional, collection or domain-specific shapes.
Built-in implementations cover `()`, `TaskRef<T>`, `Option<D>`, `Vec<D>` and
tuples up to four elements. No generated source or `build.rs` is involved.

## Graph construction

A task owns typed references to the concrete nodes that must precede it.
`GraphBuilder` starts at one or more roots, recursively visits those references
and adds edges from dependency to consumer. It validates:

- dependency cycles, with a label path in the error;
- reuse of one `TaskKind` by different Rust task implementations;
- duplicate claimed output paths;
- empty task kinds;
- irreversible tasks that are not terminal nodes.

`NodeIndex` remains private and is valid only inside the current graph.
`TaskInstanceId`, `TaskKind`, `CacheKey` and `OutputFingerprint` are separate
types because they represent different identities.

## Execution and result semantics

The executor uses an indegree ready queue. It launches only ready nodes, up to a
configurable graph-level concurrency limit. A P&R node has its own independent
candidate-level limit.

The result cell contains the complete success or failure:

```text
OnceCell<Result<Arc<Output>, Arc<TaskFailure>>>
```

An error is terminal too. A skipped descendant receives a dependency failure in
its cell, so `TaskRef::result()` never waits forever. Only the executor can
initialize a cell; readers only wait for and clone the stored `Arc`.

Task state is deliberately separate because it changes over time. A task may be
waiting, ready, running, cached, finished, failed, cancelled, skipped or rolling
back. A running task can call `TaskContext::set_progress(completed, total)`,
which is how the aggregate P&R task publishes `Running 17/64`.

Cancellation is cooperative. The executor stops launching new nodes, signals
the shared cancellation token, waits for already-running graph tasks, and
completes all unstarted result cells. A running stage that observes cancellation
returns an error and rolls back only its own partial effects. During CLI graph
execution, `Ctrl+C` cancels this token. Gowin tool adapters then terminate and
reap active Yosys, nextpnr, gowin_pack, and openFPGALoader processes before the
stage reports cancellation.

External processes publish structured command, stdout, and stderr events to a
`ProcessObserver`. The CLI writes every event to `build/bitloom.log` and can
also forward it live. Producers enqueue log events without waiting for disk;
a dedicated writer thread preserves their chronological order and periodically
flushes buffered lines. Each build branch scopes its events with the configured
bitstream name, and every invocation has a distinct ID, so parallel nextpnr
runs remain attributable in one session log.

## P&R task boundary

`bitloom-pnr::PnrTask` is intentionally one graph node. It:

1. accepts an ordered, unique seed list;
2. polls no more than the configured number of candidate futures at once;
3. publishes progress after every completed attempt;
4. distinguishes retryable candidate failures from fatal backend failures,
   stopping active and unstarted candidates immediately for the latter;
5. selects the greatest finite `PnrScore`, breaking ties by earlier run order;
6. groups repeated retryable diagnostics when every candidate fails.

This boundary matches the operation the user sees and preserves the intended
`64 runs / 8 parallel` behavior. Candidate implementation stays in a generic,
statically typed `PnrBackend<Input>`.

## Build and run pipelines

`bitloom-build` creates only the graph used to produce FPGA bitstreams. One
snapshot contains the shared RTL; every requested top module owns the rest of
its branch:

```text
                 ┌→ Synthesis(top_a) → Pnr(top_a, N runs) → Pack(top_a)
Snapshot(all RTL)┤
                 └→ Synthesis(top_b) → Pnr(top_b, N runs) → Pack(top_b)
```

- `SnapshotTask` copies an explicit, validated set of project-relative source
  and constraint files into a task-owned staging directory, hashes them in
  deterministic path order and atomically publishes the snapshot.
- each `SynthesisTask` receives the same `Arc<Snapshot>`, selects one configured
  top module through its backend parameters and returns its own typed
  `SynthesisOutput`;
- each branch has an aggregate `PnrTask` that receives only that synthesis
  output and selects the branch's best routed artifact;
- each branch's `PackTask` references that exact P&R instance and generates one
  `PackOutput` bitstream for the corresponding design.

Thus several bitstreams mean several independent designs/top modules, not
several pack variants of one routed design.

`BuildTasks` represents this typed node set before graph roots are selected.
The ordinary `build` command makes every `PackTask` a root and seals the graph.
This split lets `run` reuse the exact same task instances while attaching
additional dependencies:

```text
                         ┌→ Synthesis(top_a) → PNR(top_a) → Pack(top_a) ─┐
Snapshot(all RTL) ───────┤                                               │
                         └→ Synthesis(top_b) → PNR(top_b) → Pack(top_b) ─┤
                                                                         ├→ FlashImageTask
BLPLParseTask(partition.blpl syntax) ────────────────────────────────────┘
```

`BLPLParseTask` has no dependency on snapshot, synthesis or PNR, so the
executor schedules legacy-compatible syntax parsing immediately in parallel
with the build branch. Image preparation depends on both the validated BLPL
document and every named pack result. It then runs the unchanged legacy BLPL
resolver with those pack artifacts exposed as builtin file paths; resolution
therefore retains constants, expressions, file metadata, packing rules and
diagnostic spans without making parsing wait for PNR.
`ProgramTask` then performs the external hardware effect. It is marked
irreversible and is necessarily a terminal graph node.

SRAM mode has the same shape but uses `SramImageTask`, which statically depends
on one concrete pack task and on the layout. Validation rejects SRAM mode when
more than one bitstream is configured. The serial monitor begins only after
the graph succeeds; it is an interactive session rather than a finite task.
It owns a small connected/reconnecting state machine outside the task graph.
The session retains its editable input line and cumulative statistics across
reconnections, retries the selected port once per second, and keeps terminal
raw-mode and alternate-screen cleanup behind guards.
Its stateful RX renderer separates transport reads from presentation. UTF-8
retains incomplete code points between reads and flushes an incomplete tail as
a replacement character when a connection ends; ASCII, offsetted hex rows and
unchanged raw bytes use the same renderer boundary. RX selection is independent
from the TX path. Text TX serializes terminal input as UTF-8, while hex TX parses
whitespace-separated two-digit bytes. Both append the configured line ending;
`none` preserves exact byte transmission.

Firmware is deliberately absent. In the future a firmware task can become one
more typed input of image assembly without changing synthesis, PNR or pack
tasks.

## OSS CAD Suite environment

`[toolchain].path` identifies the OSS CAD Suite root rather than only an
executable directory. The Gowin adapter derives one immutable environment and
applies it to Yosys, nextpnr, gowin_pack and openFPGALoader processes. It
contains the suite `bin` and `lib` directories in `PATH` plus YosysHQ, Python,
SSL, Qt, GTK, GDK pixbuf and openFPGALoader data variables. `build` and `run`
also refresh the suite GDK loader cache before graph execution. An empty path
leaves process lookup and environment unchanged.

## Incremental cache

Caching is opt-in per task through `TaskCache<T::Output>`. Its API is typed all
the way through restore and store. A key implementation receives:

- the stable task kind;
- the cache schema version;
- ordered dependency output fingerprints.

The adapter remains responsible for hashing canonical task parameters,
toolchain identity and relevant environment. Cache errors are either ignored as
misses (`BestEffort`) or fail the task (`Strict`). Cache hits populate the same
result cell and do not produce rollback journals.

The CLI wires persistent adapters for all ordinary build stages under
`build/cache`. Each entry is published through a staging directory and contains
typed JSON metadata plus a copied artifact. Restore verifies the artifact bytes
against the stored fingerprint before atomically publishing it back to the
working build tree; malformed or corrupt entries are removed and treated as
misses.

The snapshot retains separate full and source-only fingerprints. Its cache key
contains every source and constraint input so the snapshot itself is refreshed
on either kind of change, while the fingerprint exposed to synthesis contains
only RTL inputs. Each PNR cache key separately hashes that design's constraints.
Consequently a constraint-only edit does not invalidate synthesis. Pack keys use
the selected routed artifact fingerprint, so packing can also remain cached when
a repeated PNR run produces byte-identical output.

Non-cacheable dependencies can still participate in downstream keys by
implementing `Task::output_fingerprint`. If any dependency has no fingerprint,
the downstream cache is skipped rather than accepting an incomplete key.

## Rollback

Tasks register compensating actions through `TaskContext::defer`. The action is
recorded before the effect it protects. The journal belongs to one in-progress
stage attempt. If that attempt fails, panics, is cancelled through its task
contract, or cannot complete a strict cache store, its journal runs immediately
in reverse order and removes partial artifacts such as a generated Yosys `.ys`
script.

Successful completion is the stage's commit boundary: its journal is discarded,
its result remains available, and failures in later nodes never roll it back.
This preserves completed artifacts for incremental reuse while the executor
continues to run one DAG. A cache hit is already committed and creates no
journal.

Irreversible tasks must be terminal and are held behind reversible work by the
scheduler. No runtime can promise atomic recovery after an irreversible external
effect, so that boundary remains explicit.

## CLI integration tests

The CLI workflow tests compile one small test-only executable and expose it as
the complete OSS CAD Suite toolchain. Real Bitloom subprocesses then exercise
builds, cached rebuilds, cleanup, programming, recovery, logging, and the run
handoff without an installed FPGA toolchain or connected hardware. Unix runs
also deliver a real interrupt to verify cancellation and partial-output
rollback; process-level cancellation remains covered on every platform.

## UI isolation

The core exposes immutable `TaskSnapshot` values through `TaskMonitor`.
`bitloom-ui` owns all ratatui/crossterm details. A display failure does not drop
the observed execution future: the reporter waits for execution to finish and
returns the rendering error alongside its output.

The same snapshot model supports an interactive table and deterministic plain
text:

```text
 ELAPSED   TASK                         STATUS
──────────────────────────────────────────────────────────────
 00:00.3   Load sources                 ✓ Finished
 00:24.3   Place & Route                ● Running 17/64
 --:--.-   Generate bitstream           ◌ Waiting
```
