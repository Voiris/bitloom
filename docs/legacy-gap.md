# Legacy feature gap

This comparison tracks user-facing capabilities, not source compatibility.
The new typed graph remains the architectural source of truth.

## Already covered

- strict project configuration and source/constraint discovery;
- interactive project creation with HDL and toolchain selection;
- complete OSS CAD Suite process environment and GDK cache refresh;
- Yosys synthesis, aggregate multi-run nextpnr PNR and gowin_pack;
- SRAM and flash programming through openFPGALoader;
- interactive serial monitor selection, editable input, reconnect, clear-screen
  control, live transfer/connection statistics, and streaming text, hex and raw
  RX formats;
- multi-bitstream builds and one typed `run` graph with parallel BLPL parsing;
- persistent content-addressed snapshot, synthesis, PNR and pack caching with
  stage-specific invalidation and cache-content verification;
- the complete legacy BLPL language, resolver, span diagnostics and partition
  fingerprint implementation;
- live build/run task table, cancellation and rollback infrastructure.
- persistent scoped command/tool logs, verbose and advanced output controls,
  streaming non-TTY graph progress, and non-TTY serial monitoring.

## Still missing

1. Build profiles and backend tuning: dev/release defaults, PNR run count and
   seed, Yosys switches, and nextpnr placer/router/timing-driven controls.
## Intentionally not carried over

- `build-flash`: `run` supersedes it and uses one graph instead of two commands;
- RTL schematic generation until its user experience is redesigned;
- the legacy `check` command, because it contains no actual validation;
- old configuration/API compatibility;
- broader device presets until support is explicitly added beyond
  `tangnano20k`.
