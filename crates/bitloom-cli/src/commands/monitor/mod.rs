mod receiver;
mod statistics;

use std::io::{self, IsTerminal, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use bitloom_config::{MonitorRxMode, MonitorTxLineEnding, MonitorTxMode};
use crossterm::cursor::{MoveTo, MoveToColumn};
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyModifiers};
use crossterm::queue;
use crossterm::terminal::{Clear, ClearType, disable_raw_mode, enable_raw_mode};
use dialoguer::theme::ColorfulTheme;
use dialoguer::{Confirm, Input, Select};
use futures_util::StreamExt;
use owo_colors::OwoColorize;
use tokio::io::{self as tokio_io, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::time::{MissedTickBehavior, interval};
use tokio_serial::{
    SerialPortBuilderExt, SerialPortInfo, SerialPortType, SerialStream, available_ports,
};

use self::receiver::RxStream;
use self::statistics::{REFRESH_INTERVAL, ScreenGuard, Statistics, render as render_statistics};
use crate::cli::MonitorArgs;
use crate::{output, project};

const BUFFER_SIZE: usize = 4096;
const STANDARD_BAUD_RATES: &[u32] = &[4800, 9600, 19200, 38400, 57600, 115200, 230400];
const DEFAULT_BAUD_INDEX: usize = 5;
const PROMPT: &str = "> ";
const RECONNECT_DELAY: Duration = Duration::from_secs(1);

pub(super) struct Settings {
    port: String,
    baud: u32,
    rx: MonitorRxMode,
    tx: TxSettings,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TxSettings {
    Text(MonitorTxLineEnding),
    Hex(MonitorTxLineEnding),
}

#[derive(Debug)]
struct OutputState {
    pending_rx: Vec<u8>,
    line_buffered: bool,
}

struct RawModeGuard;

struct InputLine {
    characters: Vec<char>,
    cursor: usize,
    tx: TxSettings,
}

enum InputAction {
    Changed,
    ClearTerminal,
    Stop,
    ShowStatistics,
    Submit(Vec<u8>),
}

#[derive(Debug, PartialEq, Eq)]
enum HexInput {
    Empty,
    Valid(Vec<u8>),
    Incomplete,
    Invalid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConnectionExit {
    StoppedByUser,
    InputClosed,
    DeviceDisconnected,
}

enum ReconnectExit {
    StoppedByUser,
    Connected(SerialStream),
}

impl RawModeGuard {
    fn enable() -> io::Result<Self> {
        enable_raw_mode()?;
        Ok(Self)
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();

        let mut stdout = io::stdout();
        let _ = queue!(stdout, MoveToColumn(0), Clear(ClearType::CurrentLine));
        let _ = stdout.flush();
    }
}

impl Default for OutputState {
    fn default() -> Self {
        Self { pending_rx: Vec::new(), line_buffered: true }
    }
}

impl OutputState {
    fn new(rx: MonitorRxMode) -> Self {
        Self { pending_rx: Vec::new(), line_buffered: rx != MonitorRxMode::Raw }
    }
}

impl Default for InputLine {
    fn default() -> Self {
        Self::new(TxSettings::Text(MonitorTxLineEnding::Crlf))
    }
}

impl InputLine {
    fn new(tx: TxSettings) -> Self {
        Self { characters: Vec::new(), cursor: 0, tx }
    }

    fn handle_key(&mut self, key: KeyEvent) -> InputAction {
        if control_key(&key, &['c', 'C', 'с', 'С']) {
            return InputAction::Stop;
        }
        if control_key(&key, &['t', 'T', 'е', 'Е']) {
            return InputAction::ShowStatistics;
        }
        if control_key(&key, &['l', 'L', 'д', 'Д']) {
            return InputAction::ClearTerminal;
        }

        match key.code {
            KeyCode::Enter => self.submit_action(),
            KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.characters.insert(self.cursor, character);
                self.cursor += 1;
                InputAction::Changed
            }
            KeyCode::Backspace if self.cursor > 0 => {
                self.cursor -= 1;
                self.characters.remove(self.cursor);
                InputAction::Changed
            }
            KeyCode::Delete if self.cursor < self.characters.len() => {
                self.characters.remove(self.cursor);
                InputAction::Changed
            }
            KeyCode::Left if self.cursor > 0 => {
                self.cursor -= 1;
                InputAction::Changed
            }
            KeyCode::Right if self.cursor < self.characters.len() => {
                self.cursor += 1;
                InputAction::Changed
            }
            KeyCode::Home => {
                self.cursor = 0;
                InputAction::Changed
            }
            KeyCode::End => {
                self.cursor = self.characters.len();
                InputAction::Changed
            }
            _ => InputAction::Changed,
        }
    }

    fn mark_submitted(&mut self) {
        self.characters.clear();
        self.cursor = 0;
    }

    fn submit_action(&self) -> InputAction {
        match self.tx {
            TxSettings::Text(line_ending) => {
                let mut bytes = self.characters.iter().collect::<String>().into_bytes();
                bytes.extend_from_slice(line_ending_bytes(line_ending));
                if bytes.is_empty() { InputAction::Changed } else { InputAction::Submit(bytes) }
            }
            TxSettings::Hex(line_ending) => match parse_hex_input(&self.characters) {
                HexInput::Valid(mut bytes) => {
                    bytes.extend_from_slice(line_ending_bytes(line_ending));
                    InputAction::Submit(bytes)
                }
                HexInput::Empty => {
                    let bytes = line_ending_bytes(line_ending).to_vec();
                    if bytes.is_empty() { InputAction::Changed } else { InputAction::Submit(bytes) }
                }
                HexInput::Incomplete | HexInput::Invalid => InputAction::Changed,
            },
        }
    }
}

pub async fn run(args: MonitorArgs) -> Result<()> {
    let (root, mut config) = project::load_config(false).await?;
    let settings = resolve_settings(
        &root,
        &mut config,
        args.port,
        args.baud,
        args.rx.map(Into::into),
        args.tx.map(Into::into),
        args.tx_line_ending.map(Into::into),
    )
    .await?;
    monitor(settings).await
}

pub(super) async fn resolve_settings(
    project_root: &Path,
    config: &mut bitloom_config::ProjectConfig,
    port_override: Option<String>,
    baud_override: Option<u32>,
    rx_override: Option<MonitorRxMode>,
    tx_override: Option<MonitorTxMode>,
    tx_line_ending_override: Option<MonitorTxLineEnding>,
) -> Result<Settings> {
    let interactive = interactive_terminal();
    let rx = rx_override.unwrap_or(config.monitor.rx);
    let tx_mode = tx_override.unwrap_or(config.monitor.tx);
    let tx_line_ending =
        tx_line_ending_override.or(config.monitor.tx_line_ending).unwrap_or_default();
    let tx = match tx_mode {
        MonitorTxMode::Text => TxSettings::Text(tx_line_ending),
        MonitorTxMode::Hex => TxSettings::Hex(tx_line_ending),
    };
    let (port, save_port) =
        match port_override.or_else(|| nonempty(&config.monitor.port).map(ToOwned::to_owned)) {
            Some(port) => (port, false),
            None if interactive => select_serial_port()?,
            None => bail!("serial port is not configured; use `--port <port>`"),
        };
    let (baud, save_baud) =
        match baud_override.or((config.monitor.baud > 0).then_some(config.monitor.baud)) {
            Some(baud) => (baud, false),
            None if interactive => select_baud_rate()?,
            None => bail!("serial baud rate is not configured; use `--baud <rate>`"),
        };
    if save_port {
        config.monitor.port = port.clone();
    }
    if save_baud {
        config.monitor.baud = baud;
    }
    if save_port || save_baud {
        bitloom_config::save(project_root, config).await?;
    }
    Ok(Settings { port, baud, rx, tx })
}

pub(super) async fn monitor(settings: Settings) -> Result<()> {
    let port = settings.port;
    let baud = settings.baud;
    let rx = settings.rx;
    let tx = settings.tx;
    let serial = open_serial(&port, baud)
        .with_context(|| format!("serial port `{port}` is busy, unavailable, or does not exist"))?;

    if !interactive_terminal() {
        return monitor_stream(&port, baud, rx, serial).await;
    }

    let formatter = output::formatter();
    output::line(formatter.format(
        output::Level::Info,
        format_args!(
            "monitoring {port} at {baud} baud (RX {}, TX {}) (Ctrl+C stop, Ctrl+L clear, Ctrl+T statistics)",
            rx_name(rx),
            tx_name(tx),
        ),
    ))?;

    {
        let _raw_mode = RawModeGuard::enable().context("failed to enable terminal raw mode")?;
        monitor_session(&port, baud, rx, tx, serial).await?;
    }

    output::line(formatter.format(output::Level::Success, "monitor stopped"))?;
    Ok(())
}

fn interactive_terminal() -> bool {
    io::stdin().is_terminal() && io::stdout().is_terminal()
}

async fn monitor_stream(
    port: &str,
    baud: u32,
    rx: MonitorRxMode,
    mut serial: SerialStream,
) -> Result<()> {
    let mut stdin = tokio_io::stdin();
    let mut stdout = tokio_io::stdout();
    let mut receiver = RxStream::new(rx);

    loop {
        let exit = stream_connection(serial, &mut stdin, &mut stdout, &mut receiver).await?;
        let trailing = receiver.finish();
        if !trailing.is_empty() {
            stdout.write_all(&trailing).await?;
            stdout.flush().await?;
        }

        match exit {
            ConnectionExit::StoppedByUser | ConnectionExit::InputClosed => return Ok(()),
            ConnectionExit::DeviceDisconnected => {
                output::warning("device disconnected; waiting to reconnect...");
            }
        }

        let Some(reconnected) = wait_for_stream_reconnection(port, baud).await? else {
            return Ok(());
        };
        serial = reconnected;
        receiver = RxStream::new(rx);
    }
}

async fn stream_connection<S, R, W>(
    serial: S,
    stdin: &mut R,
    stdout: &mut W,
    receiver: &mut RxStream,
) -> Result<ConnectionExit>
where
    S: AsyncRead + AsyncWrite + Unpin,
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let (mut serial_rx, mut serial_tx) = tokio_io::split(serial);
    let mut serial_buffer = [0_u8; BUFFER_SIZE];
    let mut stdin_buffer = [0_u8; BUFFER_SIZE];

    loop {
        tokio::select! {
            read = serial_rx.read(&mut serial_buffer) => {
                let Ok(count) = read else {
                    return Ok(ConnectionExit::DeviceDisconnected);
                };
                if count == 0 {
                    return Ok(ConnectionExit::DeviceDisconnected);
                }
                let rendered = receiver.push(&serial_buffer[..count]);
                if !rendered.is_empty() {
                    stdout.write_all(&rendered).await?;
                    stdout.flush().await?;
                }
            }
            read = stdin.read(&mut stdin_buffer) => {
                let count = read?;
                if count == 0 {
                    return Ok(ConnectionExit::InputClosed);
                }
                if serial_tx.write_all(&stdin_buffer[..count]).await.is_err()
                    || serial_tx.flush().await.is_err()
                {
                    return Ok(ConnectionExit::DeviceDisconnected);
                }
            }
            signal = tokio::signal::ctrl_c() => {
                signal.context("failed to listen for Ctrl+C")?;
                return Ok(ConnectionExit::StoppedByUser);
            }
        }
    }
}

async fn wait_for_stream_reconnection(port: &str, baud: u32) -> Result<Option<SerialStream>> {
    let mut reconnect = interval(RECONNECT_DELAY);
    reconnect.set_missed_tick_behavior(MissedTickBehavior::Skip);
    reconnect.tick().await;

    loop {
        tokio::select! {
            _ = reconnect.tick() => {
                if let Ok(serial) = open_serial(port, baud) {
                    return Ok(Some(serial));
                }
            }
            signal = tokio::signal::ctrl_c() => {
                signal.context("failed to listen for Ctrl+C")?;
                return Ok(None);
            }
        }
    }
}

async fn monitor_session(
    port: &str,
    baud: u32,
    rx: MonitorRxMode,
    tx: TxSettings,
    mut serial: SerialStream,
) -> Result<()> {
    let mut statistics = Statistics::new(Instant::now());
    let mut statistics_screen = ScreenGuard::new();
    let mut statistics_refresh = interval(REFRESH_INTERVAL);
    statistics_refresh.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut events = EventStream::new();
    let mut stdout = io::stdout();
    let mut input = InputLine::new(tx);
    let mut receiver = RxStream::new(rx);
    let mut output = OutputState::new(rx);
    let formatter = output::formatter();

    redraw_input(&mut stdout, &input)?;

    loop {
        let exit = monitor_connection(
            serial,
            &mut statistics,
            &mut statistics_screen,
            &mut statistics_refresh,
            &mut events,
            &mut stdout,
            &mut input,
            &mut receiver,
            &mut output,
        )
        .await?;

        let trailing = receiver.finish();
        if !trailing.is_empty() {
            if statistics_screen.is_active() {
                statistics_screen.leave(&mut stdout)?;
            }
            print_serial_output(&mut stdout, &trailing, &input, &mut output)?;
        }

        match exit {
            ConnectionExit::StoppedByUser | ConnectionExit::InputClosed => return Ok(()),
            ConnectionExit::DeviceDisconnected => {
                statistics.mark_disconnected(Instant::now());
                if statistics_screen.is_active() {
                    statistics_screen.leave(&mut stdout)?;
                }
                print_notice(
                    &mut stdout,
                    formatter.format(
                        output::Level::Warning,
                        "device disconnected; waiting to reconnect...",
                    ),
                    &input,
                    &mut output,
                )?;
            }
        }

        match wait_for_reconnection(
            port,
            baud,
            &mut statistics,
            &mut statistics_screen,
            &mut statistics_refresh,
            &mut events,
            &mut stdout,
            &mut input,
            &mut output,
        )
        .await?
        {
            ReconnectExit::StoppedByUser => return Ok(()),
            ReconnectExit::Connected(reconnected) => {
                statistics.mark_reconnected(Instant::now());
                print_notice(
                    &mut stdout,
                    formatter.format(
                        output::Level::Info,
                        format_args!("device reconnected on `{port}` at {baud} baud"),
                    ),
                    &input,
                    &mut output,
                )?;
                serial = reconnected;
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn monitor_connection(
    serial: SerialStream,
    statistics: &mut Statistics,
    statistics_screen: &mut ScreenGuard,
    statistics_refresh: &mut tokio::time::Interval,
    events: &mut EventStream,
    stdout: &mut impl Write,
    input: &mut InputLine,
    receiver: &mut RxStream,
    output: &mut OutputState,
) -> Result<ConnectionExit> {
    let (mut serial_rx, mut serial_tx) = tokio_io::split(serial);
    let mut serial_buffer = [0_u8; BUFFER_SIZE];

    loop {
        tokio::select! {
            read = serial_rx.read(&mut serial_buffer) => {
                let Ok(count) = read else {
                    return Ok(ConnectionExit::DeviceDisconnected);
                };
                if count == 0 {
                    return Ok(ConnectionExit::DeviceDisconnected);
                }

                statistics.record_rx(count, Instant::now());
                if !statistics_screen.is_active() {
                    let rendered = receiver.push(&serial_buffer[..count]);
                    if !rendered.is_empty() {
                        print_serial_output(stdout, &rendered, input, output)?;
                    }
                } else {
                    let _ = receiver.push(&serial_buffer[..count]);
                }
            }
            _ = statistics_refresh.tick() => {
                if statistics_screen.is_active() {
                    render_statistics(stdout, statistics, false)?;
                }
            }
            event = events.next() => {
                let Some(key) = terminal_key(event)? else {
                    continue;
                };

                if control_key(&key, &['c', 'C', 'с', 'С']) {
                    return Ok(ConnectionExit::StoppedByUser);
                }
                if statistics_screen.is_active() {
                    statistics_screen.leave(stdout)?;
                    redraw_input(stdout, input)?;
                    continue;
                }

                match input.handle_key(key) {
                    InputAction::Changed => redraw_input(stdout, input)?,
                    InputAction::ClearTerminal => clear_terminal(stdout, input, output)?,
                    InputAction::Stop => return Ok(ConnectionExit::StoppedByUser),
                    InputAction::ShowStatistics => {
                        statistics_screen.enter(stdout)?;
                        render_statistics(stdout, statistics, true)?;
                    }
                    InputAction::Submit(bytes) => {
                        if serial_tx.write_all(&bytes).await.is_err()
                            || serial_tx.flush().await.is_err()
                        {
                            return Ok(ConnectionExit::DeviceDisconnected);
                        }
                        statistics.record_tx(bytes.len() as u64, Instant::now());
                        input.mark_submitted();
                        redraw_input(stdout, input)?;
                    }
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn wait_for_reconnection(
    port: &str,
    baud: u32,
    statistics: &mut Statistics,
    statistics_screen: &mut ScreenGuard,
    statistics_refresh: &mut tokio::time::Interval,
    events: &mut EventStream,
    stdout: &mut impl Write,
    input: &mut InputLine,
    output: &mut OutputState,
) -> Result<ReconnectExit> {
    let mut reconnect = interval(RECONNECT_DELAY);
    reconnect.set_missed_tick_behavior(MissedTickBehavior::Skip);
    reconnect.tick().await;

    loop {
        tokio::select! {
            _ = reconnect.tick() => {
                if let Ok(serial) = open_serial(port, baud) {
                    if statistics_screen.is_active() {
                        statistics_screen.leave(stdout)?;
                    }
                    return Ok(ReconnectExit::Connected(serial));
                }
            }
            _ = statistics_refresh.tick() => {
                if statistics_screen.is_active() {
                    render_statistics(stdout, statistics, false)?;
                }
            }
            event = events.next() => {
                let Some(key) = terminal_key(event)? else {
                    continue;
                };

                if control_key(&key, &['c', 'C', 'с', 'С']) {
                    return Ok(ReconnectExit::StoppedByUser);
                }
                if statistics_screen.is_active() {
                    statistics_screen.leave(stdout)?;
                    redraw_input(stdout, input)?;
                    continue;
                }

                match input.handle_key(key) {
                    InputAction::Changed | InputAction::Submit(_) => redraw_input(stdout, input)?,
                    InputAction::ClearTerminal => clear_terminal(stdout, input, output)?,
                    InputAction::Stop => return Ok(ReconnectExit::StoppedByUser),
                    InputAction::ShowStatistics => {
                        statistics_screen.enter(stdout)?;
                        render_statistics(stdout, statistics, true)?;
                    }
                }
            }
        }
    }
}

fn open_serial(port: &str, baud: u32) -> tokio_serial::Result<SerialStream> {
    tokio_serial::new(port, baud).open_native_async()
}

const fn rx_name(rx: MonitorRxMode) -> &'static str {
    match rx {
        MonitorRxMode::Utf8 => "utf8",
        MonitorRxMode::Ascii => "ascii",
        MonitorRxMode::Hex => "hex",
        MonitorRxMode::Raw => "raw",
    }
}

const fn tx_name(tx: TxSettings) -> &'static str {
    match tx {
        TxSettings::Text(MonitorTxLineEnding::Crlf) => "text/crlf",
        TxSettings::Text(MonitorTxLineEnding::Lf) => "text/lf",
        TxSettings::Text(MonitorTxLineEnding::Cr) => "text/cr",
        TxSettings::Text(MonitorTxLineEnding::None) => "text/none",
        TxSettings::Hex(MonitorTxLineEnding::Crlf) => "hex/crlf",
        TxSettings::Hex(MonitorTxLineEnding::Lf) => "hex/lf",
        TxSettings::Hex(MonitorTxLineEnding::Cr) => "hex/cr",
        TxSettings::Hex(MonitorTxLineEnding::None) => "hex/none",
    }
}

const fn line_ending_bytes(line_ending: MonitorTxLineEnding) -> &'static [u8] {
    match line_ending {
        MonitorTxLineEnding::Crlf => b"\r\n",
        MonitorTxLineEnding::Lf => b"\n",
        MonitorTxLineEnding::Cr => b"\r",
        MonitorTxLineEnding::None => b"",
    }
}

fn parse_hex_input(characters: &[char]) -> HexInput {
    let input = characters.iter().collect::<String>();
    let tokens = input.split_whitespace().collect::<Vec<_>>();
    if tokens.is_empty() {
        return HexInput::Empty;
    }

    let mut bytes = Vec::with_capacity(tokens.len());
    for (index, token) in tokens.iter().enumerate() {
        if !token.bytes().all(|byte| byte.is_ascii_hexdigit()) || token.len() > 2 {
            return HexInput::Invalid;
        }
        if token.len() == 1 {
            return if index + 1 == tokens.len() {
                HexInput::Incomplete
            } else {
                HexInput::Invalid
            };
        }
        bytes.push(u8::from_str_radix(token, 16).expect("validated two-digit hexadecimal byte"));
    }
    HexInput::Valid(bytes)
}

fn terminal_key(event: Option<Result<Event, io::Error>>) -> Result<Option<KeyEvent>> {
    let Some(event) = event else {
        bail!("terminal event stream closed");
    };
    let Event::Key(key) = event? else {
        return Ok(None);
    };
    Ok(key.is_press().then_some(key))
}

fn control_key(key: &KeyEvent, characters: &[char]) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char(character) if characters.contains(&character))
}

fn redraw_input(stdout: &mut impl Write, input: &InputLine) -> io::Result<()> {
    queue!(stdout, MoveToColumn(0), Clear(ClearType::CurrentLine))?;
    match input.tx {
        TxSettings::Hex(_) => match parse_hex_input(&input.characters) {
            HexInput::Incomplete => write!(stdout, "{}", PROMPT.yellow())?,
            HexInput::Invalid => write!(stdout, "{}", PROMPT.red())?,
            HexInput::Empty | HexInput::Valid(_) => write!(stdout, "{PROMPT}")?,
        },
        TxSettings::Text(_) => write!(stdout, "{PROMPT}")?,
    }
    for character in &input.characters {
        write!(stdout, "{character}")?;
    }
    let cursor_column = PROMPT.chars().count().saturating_add(input.cursor);
    queue!(stdout, MoveToColumn(cursor_column.min(u16::MAX as usize) as u16))?;
    stdout.flush()
}

fn clear_terminal(
    stdout: &mut impl Write,
    input: &InputLine,
    state: &mut OutputState,
) -> io::Result<()> {
    state.pending_rx.clear();
    queue!(stdout, MoveTo(0, 0), Clear(ClearType::All))?;
    redraw_input(stdout, input)
}

fn print_notice(
    stdout: &mut impl Write,
    message: impl std::fmt::Display,
    input: &InputLine,
    state: &mut OutputState,
) -> io::Result<()> {
    queue!(stdout, MoveToColumn(0), Clear(ClearType::CurrentLine))?;

    if !state.pending_rx.is_empty() {
        stdout.write_all(&state.pending_rx)?;
        state.pending_rx.clear();
        write!(stdout, "\r\n")?;
    }

    write!(stdout, "{message}\r\n")?;
    redraw_input(stdout, input)
}

fn print_serial_output(
    stdout: &mut impl Write,
    bytes: &[u8],
    input: &InputLine,
    state: &mut OutputState,
) -> io::Result<()> {
    if !state.line_buffered {
        queue!(stdout, MoveToColumn(0), Clear(ClearType::CurrentLine))?;
        stdout.write_all(bytes)?;
        return redraw_input(stdout, input);
    }

    state.pending_rx.extend_from_slice(bytes);

    let Some(last_newline) = state.pending_rx.iter().rposition(|byte| *byte == b'\n') else {
        return Ok(());
    };

    queue!(stdout, MoveToColumn(0), Clear(ClearType::CurrentLine))?;

    stdout.write_all(&state.pending_rx[..=last_newline])?;

    state.pending_rx.drain(..=last_newline);

    redraw_input(stdout, input)
}

fn select_serial_port() -> Result<(String, bool)> {
    let ports = available_ports().context("failed to enumerate serial ports")?;
    match ports.as_slice() {
        [] => bail!("no serial ports found; pass `--port <port>`"),
        [port] => Ok((port.port_name.clone(), false)),
        ports => select_port_interactively(ports),
    }
}

fn select_port_interactively(ports: &[SerialPortInfo]) -> Result<(String, bool)> {
    let items = ports.iter().map(format_port).collect::<Vec<_>>();
    let index = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("Serial port")
        .items(&items)
        .default(0)
        .interact()
        .context("failed to select serial port")?;
    let save = confirm_save("Save selected serial port to bitloom.toml?")?;
    Ok((ports[index].port_name.clone(), save))
}

fn format_port(port: &SerialPortInfo) -> String {
    match &port.port_type {
        SerialPortType::UsbPort(info) => {
            let product = info.product.as_deref().unwrap_or("USB serial device");
            format!("{} - {} [{:04x}:{:04x}]", port.port_name, product, info.vid, info.pid)
        }
        _ => port.port_name.clone(),
    }
}

fn select_baud_rate() -> Result<(u32, bool)> {
    let index = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("Baud rate")
        .items(STANDARD_BAUD_RATES)
        .item("Custom...")
        .default(DEFAULT_BAUD_INDEX)
        .interact()
        .context("failed to select baud rate")?;
    let baud = if index < STANDARD_BAUD_RATES.len() {
        STANDARD_BAUD_RATES[index]
    } else {
        Input::<u64>::with_theme(&ColorfulTheme::default())
            .with_prompt("Custom baud rate")
            .validate_with(|value: &u64| {
                (1..=u64::from(u32::MAX))
                    .contains(value)
                    .then_some(())
                    .ok_or_else(|| format!("baud rate must be between 1 and {}", u32::MAX))
            })
            .interact_text()
            .context("failed to enter baud rate")? as u32
    };
    let save = confirm_save("Save selected baud rate to bitloom.toml?")?;
    Ok((baud, save))
}

fn confirm_save(prompt: &str) -> Result<bool> {
    Confirm::with_theme(&ColorfulTheme::default())
        .with_prompt(prompt)
        .default(true)
        .interact()
        .context("failed to confirm configuration update")
}

fn nonempty(value: &str) -> Option<&str> {
    (!value.trim().is_empty()).then_some(value)
}

#[cfg(test)]
mod tests {
    use crossterm::event::KeyEvent;

    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn edits_input_at_the_cursor_and_submits_crlf() {
        let mut input = InputLine::default();
        input.handle_key(key(KeyCode::Char('a')));
        input.handle_key(key(KeyCode::Char('c')));
        input.handle_key(key(KeyCode::Left));
        input.handle_key(key(KeyCode::Char('b')));

        let InputAction::Submit(bytes) = input.handle_key(key(KeyCode::Enter)) else {
            panic!("enter should submit the line");
        };
        assert_eq!(bytes, b"abc\r\n");
        assert_eq!(input.characters, ['a', 'b', 'c']);

        input.mark_submitted();
        assert!(input.characters.is_empty());
        assert_eq!(input.cursor, 0);
    }

    #[test]
    fn supports_delete_backspace_home_and_end() {
        let mut input =
            InputLine { characters: "abcd".chars().collect(), cursor: 2, ..InputLine::default() };
        input.handle_key(key(KeyCode::Backspace));
        assert_eq!(input.characters.iter().collect::<String>(), "acd");
        assert_eq!(input.cursor, 1);

        input.handle_key(key(KeyCode::Delete));
        assert_eq!(input.characters.iter().collect::<String>(), "ad");
        input.handle_key(key(KeyCode::End));
        assert_eq!(input.cursor, 2);
        input.handle_key(key(KeyCode::Home));
        assert_eq!(input.cursor, 0);
    }

    #[test]
    fn recognizes_monitor_control_keys_with_latin_and_cyrillic_layouts() {
        for character in ['c', 'C', 'с', 'С'] {
            let event = KeyEvent::new(KeyCode::Char(character), KeyModifiers::CONTROL);
            assert!(matches!(InputLine::default().handle_key(event), InputAction::Stop));
        }
        for character in ['t', 'T', 'е', 'Е'] {
            let event = KeyEvent::new(KeyCode::Char(character), KeyModifiers::CONTROL);
            assert!(matches!(InputLine::default().handle_key(event), InputAction::ShowStatistics));
        }
        for character in ['l', 'L', 'д', 'Д'] {
            let event = KeyEvent::new(KeyCode::Char(character), KeyModifiers::CONTROL);
            assert!(matches!(InputLine::default().handle_key(event), InputAction::ClearTerminal));
        }
    }

    #[test]
    fn serial_output_keeps_partial_line_between_chunks() {
        let mut stdout = Vec::new();
        let input = InputLine::default();
        let mut state = OutputState::default();

        print_serial_output(&mut stdout, b"SPI_FAST_FLASH_BO", &input, &mut state).unwrap();

        assert_eq!(state.pending_rx, b"SPI_FAST_FLASH_BO");
        assert!(stdout.is_empty());

        print_serial_output(&mut stdout, b"OT)\r\n", &input, &mut state).unwrap();

        assert!(state.pending_rx.is_empty());

        let output = String::from_utf8(stdout).unwrap();
        assert!(output.contains("SPI_FAST_FLASH_BOOT)\r\n"));
    }

    #[test]
    fn serial_output_does_not_split_line_at_read_boundary() {
        let mut stdout = Vec::new();
        let input = InputLine::default();
        let mut state = OutputState::default();

        print_serial_output(&mut stdout, b"load:0x3fce3808,len:0x44", &input, &mut state).unwrap();

        print_serial_output(&mut stdout, b"c\r\n", &input, &mut state).unwrap();

        assert!(state.pending_rx.is_empty());

        let output = String::from_utf8(stdout).unwrap();
        assert!(output.contains("load:0x3fce3808,len:0x44c\r\n"));
    }

    #[test]
    fn serial_output_flushes_complete_lines_and_keeps_partial_tail() {
        let mut stdout = Vec::new();
        let input = InputLine::default();
        let mut state = OutputState::default();

        print_serial_output(&mut stdout, b"first\r\nsecond\r\nthi", &input, &mut state).unwrap();

        assert_eq!(state.pending_rx, b"thi");

        let output = String::from_utf8(stdout).unwrap();
        assert!(output.contains("first\r\nsecond\r\n"));
        assert!(!output.contains("thi"));
    }

    #[test]
    fn serial_output_continues_partial_tail_on_next_read() {
        let mut stdout = Vec::new();
        let input = InputLine::default();
        let mut state = OutputState::default();

        print_serial_output(&mut stdout, b"first\r\nsec", &input, &mut state).unwrap();

        assert_eq!(state.pending_rx, b"sec");

        print_serial_output(&mut stdout, b"ond\r\nthird\r\n", &input, &mut state).unwrap();

        assert!(state.pending_rx.is_empty());

        let output = String::from_utf8(stdout).unwrap();

        assert!(output.contains("first\r\n"));
        assert!(output.contains("second\r\nthird\r\n"));
    }

    #[test]
    fn serial_output_does_not_render_incomplete_line() {
        let mut stdout = Vec::new();
        let input = InputLine::default();
        let mut state = OutputState::default();

        print_serial_output(&mut stdout, b"unfinished", &input, &mut state).unwrap();

        assert_eq!(state.pending_rx, b"unfinished");
        assert!(stdout.is_empty());
    }

    #[test]
    fn notice_flushes_pending_rx_before_message() {
        let mut stdout = Vec::new();
        let input = InputLine::default();
        let mut state =
            OutputState { pending_rx: b"fatal error".to_vec(), ..OutputState::default() };

        print_notice(
            &mut stdout,
            "Device disconnected; waiting to reconnect...",
            &input,
            &mut state,
        )
        .unwrap();

        assert!(state.pending_rx.is_empty());

        let output = String::from_utf8(stdout).unwrap();

        assert!(output.contains("fatal error\r\nDevice disconnected; waiting to reconnect...\r\n"));
    }

    #[test]
    fn clear_terminal_discards_pending_rx() {
        let mut stdout = Vec::new();
        let input = InputLine::default();
        let mut state =
            OutputState { pending_rx: b"unfinished".to_vec(), ..OutputState::default() };

        clear_terminal(&mut stdout, &input, &mut state).unwrap();

        assert!(state.pending_rx.is_empty());
    }

    #[test]
    fn text_input_uses_the_selected_line_ending() {
        for (line_ending, expected) in [
            (MonitorTxLineEnding::Crlf, b"hello\r\n".as_slice()),
            (MonitorTxLineEnding::Lf, b"hello\n".as_slice()),
            (MonitorTxLineEnding::Cr, b"hello\r".as_slice()),
            (MonitorTxLineEnding::None, b"hello".as_slice()),
        ] {
            let mut input = InputLine::new(TxSettings::Text(line_ending));
            input.characters = "hello".chars().collect();
            let InputAction::Submit(bytes) = input.handle_key(key(KeyCode::Enter)) else {
                panic!("non-empty text should be submitted");
            };
            assert_eq!(bytes, expected);
        }
    }

    #[test]
    fn empty_text_without_a_line_ending_sends_nothing() {
        let mut input = InputLine::new(TxSettings::Text(MonitorTxLineEnding::None));
        assert!(matches!(input.handle_key(key(KeyCode::Enter)), InputAction::Changed));
    }

    #[test]
    fn raw_output_is_not_held_until_a_newline() {
        let mut stdout = Vec::new();
        let input = InputLine::default();
        let mut state = OutputState::new(MonitorRxMode::Raw);

        print_serial_output(&mut stdout, &[0xff, 0x00], &input, &mut state).unwrap();

        assert!(state.pending_rx.is_empty());
        assert!(stdout.windows(2).any(|bytes| bytes == [0xff, 0x00]));
    }

    #[test]
    fn hex_input_requires_complete_two_digit_bytes() {
        assert_eq!(
            parse_hex_input(&"ff 00 A5".chars().collect::<Vec<_>>()),
            HexInput::Valid(vec![0xff, 0x00, 0xa5])
        );
        assert_eq!(parse_hex_input(&"ff 0".chars().collect::<Vec<_>>()), HexInput::Incomplete);
        assert_eq!(parse_hex_input(&"f 00".chars().collect::<Vec<_>>()), HexInput::Invalid);
        assert_eq!(parse_hex_input(&"ff zz".chars().collect::<Vec<_>>()), HexInput::Invalid);
        assert_eq!(parse_hex_input(&"000".chars().collect::<Vec<_>>()), HexInput::Invalid);
    }

    #[test]
    fn invalid_hex_input_is_retained_on_enter() {
        let mut input = InputLine::new(TxSettings::Hex(MonitorTxLineEnding::Crlf));
        input.characters = "ff zz".chars().collect();

        assert!(matches!(input.handle_key(key(KeyCode::Enter)), InputAction::Changed));
        assert_eq!(input.characters.iter().collect::<String>(), "ff zz");
    }

    #[test]
    fn valid_hex_input_submits_exact_bytes() {
        let mut input = InputLine::new(TxSettings::Hex(MonitorTxLineEnding::None));
        input.characters = "48 65 6C 6C 6F 2C 20 77 6F 72 6C 64 21".chars().collect();

        let InputAction::Submit(bytes) = input.handle_key(key(KeyCode::Enter)) else {
            panic!("valid hexadecimal bytes should be submitted");
        };
        assert_eq!(bytes, b"Hello, world!");
    }

    #[test]
    fn hex_input_appends_the_selected_line_ending() {
        let mut input = InputLine::new(TxSettings::Hex(MonitorTxLineEnding::Crlf));
        input.characters = "48 69".chars().collect();

        let InputAction::Submit(bytes) = input.handle_key(key(KeyCode::Enter)) else {
            panic!("valid hexadecimal bytes should be submitted");
        };
        assert_eq!(bytes, b"Hi\r\n");
    }

    #[test]
    fn hex_prompt_marks_incomplete_and_invalid_input() {
        let mut input = InputLine::new(TxSettings::Hex(MonitorTxLineEnding::Crlf));
        input.characters = "f".chars().collect();
        let mut incomplete = Vec::new();
        redraw_input(&mut incomplete, &input).unwrap();
        assert!(incomplete.windows(5).any(|bytes| bytes == b"\x1b[33m"));

        input.characters = "zz".chars().collect();
        let mut invalid = Vec::new();
        redraw_input(&mut invalid, &input).unwrap();
        assert!(invalid.windows(5).any(|bytes| bytes == b"\x1b[31m"));
    }

    #[tokio::test]
    async fn resolves_monitor_overrides_for_text_and_hex_input() {
        let mut config = bitloom_config::ProjectConfig::template(
            "demo",
            Some(bitloom_config::BoardPreset::TangNano20k),
        );
        config.monitor.port = "COM7".to_owned();
        config.monitor.baud = 115_200;

        let settings = resolve_settings(
            Path::new("."),
            &mut config,
            None,
            None,
            Some(MonitorRxMode::Ascii),
            Some(MonitorTxMode::Text),
            Some(MonitorTxLineEnding::Lf),
        )
        .await
        .unwrap();
        assert_eq!(settings.rx, MonitorRxMode::Ascii);
        assert_eq!(settings.tx, TxSettings::Text(MonitorTxLineEnding::Lf));

        let settings = resolve_settings(
            Path::new("."),
            &mut config,
            None,
            None,
            None,
            Some(MonitorTxMode::Hex),
            Some(MonitorTxLineEnding::Crlf),
        )
        .await
        .unwrap();
        assert_eq!(settings.tx, TxSettings::Hex(MonitorTxLineEnding::Crlf));
    }

    #[tokio::test]
    async fn hex_cli_override_uses_configured_line_ending() {
        let mut config = bitloom_config::ProjectConfig::template(
            "demo",
            Some(bitloom_config::BoardPreset::TangNano20k),
        );
        config.monitor.port = "COM7".to_owned();
        config.monitor.baud = 115_200;

        let settings = resolve_settings(
            Path::new("."),
            &mut config,
            None,
            None,
            None,
            Some(MonitorTxMode::Hex),
            None,
        )
        .await
        .unwrap();

        assert_eq!(settings.tx, TxSettings::Hex(MonitorTxLineEnding::Crlf));
    }

    #[tokio::test]
    async fn non_tty_stream_copies_serial_and_standard_input_bytes() {
        let (serial, mut device) = tokio_io::duplex(64);
        let (mut input_writer, mut input_reader) = tokio_io::duplex(64);
        let (mut output_writer, mut output_reader) = tokio_io::duplex(64);
        let session = tokio::spawn(async move {
            let mut receiver = RxStream::new(MonitorRxMode::Raw);
            stream_connection(serial, &mut input_reader, &mut output_writer, &mut receiver)
                .await
                .unwrap()
        });

        input_writer.write_all(b"host to fpga").await.unwrap();
        let mut transmitted = [0_u8; 12];
        device.read_exact(&mut transmitted).await.unwrap();
        assert_eq!(&transmitted, b"host to fpga");

        device.write_all(b"fpga to host").await.unwrap();
        let mut received = [0_u8; 12];
        output_reader.read_exact(&mut received).await.unwrap();
        assert_eq!(&received, b"fpga to host");

        drop(input_writer);
        assert_eq!(session.await.unwrap(), ConnectionExit::InputClosed);
    }
}
