# Bitloom

Bitloom is a command-line build tool for FPGA projects. It wraps the Gowin
open-source toolchain in a small, reproducible workflow:

```text
Verilog / SystemVerilog → Yosys → nextpnr-himbaechel → gowin_pack → bitstream
```

It can create a project, build one or more bitstreams, reuse unchanged build
stages, program SRAM or persistent flash, and open an interactive serial
monitor.

> [!WARNING]
> Bitloom is under active development. Configuration and command-line
> interfaces may change at any time.

## Features

- Interactive Verilog or SystemVerilog project creation
- Yosys synthesis, nextpnr place-and-route and bitstream packing
- Multiple top modules and bitstreams in one project
- Incremental builds with persistent caching
- SRAM and persistent flash programming through `openFPGALoader`
- BLPL layouts for assembling multi-bitstream flash images
- Interactive serial monitor with automatic reconnection
- Safe project cleaning with progress and removal statistics

## Requirements

- Windows
- Rust nightly with Cargo when building from source
- An extracted OSS CAD Suite installation
- A supported Gowin FPGA board and its pin constraints

The OSS CAD Suite path can be stored in the project configuration. An empty
path uses tools available through the process environment.

## Supported hardware

| Board          | FPGA device             |
|----------------|-------------------------|
| Tang Nano 20K  | `GW2AR-LV18QN88C8/I7`   |

## Installation

Build and install the binary from this repository:

```powershell
cargo install --path crates/bitloom-cli
```

For local development, commands can also be run from the repository with:

```powershell
cargo run -p bitloom -- <command>
```

## Quick start

Create a project:

```powershell
bitloom new
```

The wizard asks for the project path, project name, HDL language, board and OSS
CAD Suite directory. It produces this layout:

```text
my-project/
├── .gitignore
├── bitloom.toml
├── partition.blpl
├── constraints/
│   └── top.cst
└── rtl/
    └── top.sv
```

Update the pin constraints and RTL, then build from the project directory:

```powershell
cd my-project
bitloom build
```

The default bitstream is written to:

```text
build/bitstreams/top.fs
```

Build, program SRAM and open the serial monitor:

```powershell
bitloom run --sram
```

Press `Ctrl+C` to leave the monitor.

## Commands

| Command                   | Description                                      |
|---------------------------|--------------------------------------------------|
| `bitloom new`             | Interactively create a project                   |
| `bitloom build`           | Build every configured bitstream                 |
| `bitloom clean`           | Remove build outputs and persistent cache        |
| `bitloom flash`           | Program already built bitstreams                 |
| `bitloom monitor`         | Open the serial monitor                          |
| `bitloom run`             | Build, program and open the serial monitor       |

Run `bitloom <command> --help` for command-specific options.

`build`, `flash`, and `run` save commands and complete tool output to
`build/bitloom.log`. Use `--verbose` to stream tool output and `--advanced` to
show commands. Redirected build output uses streaming progress automatically.

## Configuration

Every command except `new` operates on the `bitloom.toml` in the current
directory. A Tang Nano 20K project uses this shape:

```toml
[project]
name = "my-project"
version = "0.1.0"

[rtl]
sources = ["rtl/**/*.sv", "rtl/**/*.v"]
exclude = []

[toolchain]
path = "C:/oss-cad-suite"

[target]
vendor = "gowin"
device = "GW2AR-LV18QN88C8/I7"

[flash]
board = "tangnano20k"
cable = ""

[monitor]
baud = 115200
port = "COM7"
rx = "utf8"
tx = "text"
tx_line_ending = "crlf"

[[bitstreams]]
name = "top"
top = "top"
frequency_mhz = 27.0
constraints = ["constraints/top.cst"]
```

Add another `[[bitstreams]]` table to build another top module:

```toml
[[bitstreams]]
name = "recovery"
top = "recovery_top"
frequency_mhz = 27.0
constraints = ["constraints/recovery.cst"]
```

The resulting files are `build/bitstreams/top.fs` and
`build/bitstreams/recovery.fs`.

## Incremental builds

`build` and `run` keep a persistent cache under `build/cache`. Unchanged stages
are restored without relaunching their external tools. `bitloom clean` removes
the entire build directory, including this cache.

## Flash layouts

Persistent flash mode uses `partition.blpl` to arrange built bitstreams and
other data in the flash image. The generated project starts with:

```blpl
@flash_size(8MiB);
@offset_align(4KiB);

top:
  offset(0x0),
  size(auto),
  file($top);
```

See the generated file and BLPL documentation in the source tree for more
advanced layouts.

## Serial monitor

Bitloom can select and remember the serial port and baud rate interactively.
The monitor reconnects automatically when the device disappears.

- `Ctrl+C` stops the monitor;
- `Ctrl+L` clears the terminal;
- `Ctrl+T` opens live RX/TX and connection statistics;
- the input line supports text and hexadecimal transmission.

With redirected input or output, the monitor becomes a plain stream: stdin is
forwarded to the serial port and received data is written to stdout. Port and
baud must already be configured or provided on the command line; use `--rx raw`
for an exact byte stream.

## Architecture

See [Architecture](docs/architecture.md) for task execution, caching and crate
boundaries. Remaining differences from the old implementation are tracked in
[Legacy feature gap](docs/legacy-gap.md).

## Development

Format and validate the workspace with:

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

See [CONTRIBUTING.md](CONTRIBUTING.md) and the
[Bitloom Style Guide](STYLE_GUIDE.md) before making changes.

## License

Bitloom is available under the [MIT License](LICENSE).
