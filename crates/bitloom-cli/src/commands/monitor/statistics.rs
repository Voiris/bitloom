use std::collections::VecDeque;
use std::io::{self, Write};
use std::time::{Duration, Instant};

use crossterm::cursor::{Hide, MoveTo, Show};
use crossterm::queue;
use crossterm::terminal::{Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen};

const RATE_WINDOW: Duration = Duration::from_secs(5);
pub(super) const REFRESH_INTERVAL: Duration = Duration::from_secs(1);
const LABEL_WIDTH: usize = 24;
const VALUE_WIDTH: usize = 12;

#[cfg(windows)]
const NEWLINE: &str = "\r\n";
#[cfg(not(windows))]
const NEWLINE: &str = "\n";

pub(super) struct ScreenGuard {
    active: bool,
}

pub(super) struct Statistics {
    started_at: Instant,
    connected_at: Option<Instant>,
    connected: Duration,
    rx_bytes: u64,
    tx_bytes: u64,
    rx_samples: VecDeque<(Instant, u64)>,
    tx_samples: VecDeque<(Instant, u64)>,
    reconnects: u64,
}

struct Snapshot {
    rx_bytes: u64,
    tx_bytes: u64,
    rx_rate: u64,
    tx_rate: u64,
    average_rx_rate: u64,
    average_tx_rate: u64,
    connected: Duration,
    disconnected: Duration,
    total: Duration,
    reconnects: u64,
}

impl ScreenGuard {
    pub(super) const fn new() -> Self {
        Self { active: false }
    }

    pub(super) const fn is_active(&self) -> bool {
        self.active
    }

    pub(super) fn enter(&mut self, stdout: &mut impl Write) -> io::Result<()> {
        queue!(stdout, EnterAlternateScreen, Hide)?;
        stdout.flush()?;
        self.active = true;
        Ok(())
    }

    pub(super) fn leave(&mut self, stdout: &mut impl Write) -> io::Result<()> {
        queue!(stdout, Show, LeaveAlternateScreen)?;
        stdout.flush()?;
        self.active = false;
        Ok(())
    }
}

impl Drop for ScreenGuard {
    fn drop(&mut self) {
        if !self.active {
            return;
        }

        let mut stdout = io::stdout();
        let _ = queue!(stdout, Show, LeaveAlternateScreen);
        let _ = stdout.flush();
    }
}

impl Statistics {
    pub(super) fn new(now: Instant) -> Self {
        Self {
            started_at: now,
            connected_at: Some(now),
            connected: Duration::ZERO,
            rx_bytes: 0,
            tx_bytes: 0,
            rx_samples: VecDeque::new(),
            tx_samples: VecDeque::new(),
            reconnects: 0,
        }
    }

    pub(super) fn record_rx(&mut self, bytes: usize, now: Instant) {
        let bytes = bytes as u64;
        self.rx_bytes = self.rx_bytes.saturating_add(bytes);
        prune_rate_samples(&mut self.rx_samples, now);
        self.rx_samples.push_back((now, bytes));
    }

    pub(super) fn record_tx(&mut self, bytes: u64, now: Instant) {
        self.tx_bytes = self.tx_bytes.saturating_add(bytes);
        prune_rate_samples(&mut self.tx_samples, now);
        self.tx_samples.push_back((now, bytes));
    }

    pub(super) fn mark_disconnected(&mut self, now: Instant) {
        if let Some(connected_at) = self.connected_at.take() {
            self.connected += now.saturating_duration_since(connected_at);
        }
    }

    pub(super) fn mark_reconnected(&mut self, now: Instant) {
        self.connected_at = Some(now);
        self.reconnects = self.reconnects.saturating_add(1);
    }

    fn snapshot(&mut self, now: Instant) -> Snapshot {
        let connected = self.connected
            + self
                .connected_at
                .map_or(Duration::ZERO, |connected_at| now.saturating_duration_since(connected_at));
        let total = now.saturating_duration_since(self.started_at);

        Snapshot {
            rx_bytes: self.rx_bytes,
            tx_bytes: self.tx_bytes,
            rx_rate: recent_rate(&mut self.rx_samples, now, self.started_at),
            tx_rate: recent_rate(&mut self.tx_samples, now, self.started_at),
            average_rx_rate: average_rate(self.rx_bytes, total),
            average_tx_rate: average_rate(self.tx_bytes, total),
            connected,
            disconnected: total.saturating_sub(connected),
            total,
            reconnects: self.reconnects,
        }
    }
}

pub(super) fn render(
    stdout: &mut impl Write,
    statistics: &mut Statistics,
    redraw_all: bool,
) -> io::Result<()> {
    let snapshot = statistics.snapshot(Instant::now());
    let rx_line = format_line("RX:", &format_bytes(snapshot.rx_bytes));
    let tx_line = format_line("TX:", &format_bytes(snapshot.tx_bytes));
    let rx_rate_line = format_line("RX Rate (last 5s):", &format_rate(snapshot.rx_rate));
    let tx_rate_line = format_line("TX Rate (last 5s):", &format_rate(snapshot.tx_rate));
    let average_rx_rate_line = format_line("Avg RX Rate:", &format_rate(snapshot.average_rx_rate));
    let average_tx_rate_line = format_line("Avg TX Rate:", &format_rate(snapshot.average_tx_rate));
    let connected_line = format_line("Connected:", &format_duration(snapshot.connected));
    let disconnected_line = format_line("Disconnected:", &format_duration(snapshot.disconnected));
    let total_line = format_line("Total:", &format_duration(snapshot.total));
    let reconnects_line = format_line("Reconnects:", &snapshot.reconnects.to_string());

    if redraw_all {
        queue!(stdout, MoveTo(0, 0), Clear(ClearType::All))?;
        if snapshot.reconnects > 0 {
            write!(
                stdout,
                "Serial Monitor Statistics{NEWLINE}\
                 {NEWLINE}\
                 {rx_line}{NEWLINE}\
                 {tx_line}{NEWLINE}\
                 {NEWLINE}\
                 {rx_rate_line}{NEWLINE}\
                 {tx_rate_line}{NEWLINE}\
                 {NEWLINE}\
                 {average_rx_rate_line}{NEWLINE}\
                 {average_tx_rate_line}{NEWLINE}\
                 {NEWLINE}\
                 {connected_line}{NEWLINE}\
                 {disconnected_line}{NEWLINE}\
                 {total_line}{NEWLINE}\
                 {reconnects_line}{NEWLINE}\
                 {NEWLINE}\
                 Press any key to return..."
            )?;
        } else {
            write!(
                stdout,
                "Serial Monitor Statistics{NEWLINE}\
                 {NEWLINE}\
                 {rx_line}{NEWLINE}\
                 {tx_line}{NEWLINE}\
                 {NEWLINE}\
                 {rx_rate_line}{NEWLINE}\
                 {tx_rate_line}{NEWLINE}\
                 {NEWLINE}\
                 {average_rx_rate_line}{NEWLINE}\
                 {average_tx_rate_line}{NEWLINE}\
                 {NEWLINE}\
                 {connected_line}{NEWLINE}\
                 {total_line}{NEWLINE}\
                 {NEWLINE}\
                 Press any key to return..."
            )?;
        }
    } else {
        rewrite_line(stdout, 2, &rx_line)?;
        rewrite_line(stdout, 3, &tx_line)?;
        rewrite_line(stdout, 5, &rx_rate_line)?;
        rewrite_line(stdout, 6, &tx_rate_line)?;
        rewrite_line(stdout, 8, &average_rx_rate_line)?;
        rewrite_line(stdout, 9, &average_tx_rate_line)?;
        rewrite_line(stdout, 11, &connected_line)?;

        if snapshot.reconnects > 0 {
            rewrite_line(stdout, 12, &disconnected_line)?;
            rewrite_line(stdout, 13, &total_line)?;
            rewrite_line(stdout, 14, &reconnects_line)?;
            queue!(stdout, MoveTo(0, 16))?;
        } else {
            rewrite_line(stdout, 12, &total_line)?;
            queue!(stdout, MoveTo(0, 14))?;
        }
    }

    stdout.flush()
}

fn recent_rate(samples: &mut VecDeque<(Instant, u64)>, now: Instant, started_at: Instant) -> u64 {
    prune_rate_samples(samples, now);
    let bytes = samples.iter().map(|(_, bytes)| bytes).sum::<u64>();
    average_rate(bytes, now.saturating_duration_since(started_at).min(RATE_WINDOW))
}

fn average_rate(bytes: u64, elapsed: Duration) -> u64 {
    if elapsed.is_zero() {
        return 0;
    }
    (bytes as f64 / elapsed.as_secs_f64()).round() as u64
}

fn prune_rate_samples(samples: &mut VecDeque<(Instant, u64)>, now: Instant) {
    while samples
        .front()
        .is_some_and(|(sampled_at, _)| now.saturating_duration_since(*sampled_at) > RATE_WINDOW)
    {
        samples.pop_front();
    }
}

fn format_bytes(bytes: u64) -> String {
    const KIBIBYTE: f64 = 1024.0;
    const MEBIBYTE: f64 = KIBIBYTE * 1024.0;
    const GIBIBYTE: f64 = MEBIBYTE * 1024.0;
    const TEBIBYTE: f64 = GIBIBYTE * 1024.0;
    let value = bytes as f64;

    if value >= TEBIBYTE {
        format!("{:.1} TiB", value / TEBIBYTE)
    } else if value >= GIBIBYTE {
        format!("{:.1} GiB", value / GIBIBYTE)
    } else if value >= MEBIBYTE {
        format!("{:.1} MiB", value / MEBIBYTE)
    } else if value >= KIBIBYTE {
        format!("{:.1} KiB", value / KIBIBYTE)
    } else {
        format!("{bytes} B")
    }
}

fn format_rate(bytes_per_second: u64) -> String {
    format!("{}/s", format_bytes(bytes_per_second))
}

fn format_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    let hours = seconds / 3_600;
    let minutes = (seconds % 3_600) / 60;
    let seconds = seconds % 60;
    format!("{hours:02}:{minutes:02}:{seconds:02}")
}

fn format_line(label: &str, value: &str) -> String {
    format!("{label:<LABEL_WIDTH$}{value:>VALUE_WIDTH$}")
}

fn rewrite_line(stdout: &mut impl Write, row: u16, text: &str) -> io::Result<()> {
    queue!(stdout, MoveTo(0, row), Clear(ClearType::CurrentLine))?;
    write!(stdout, "{text}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_byte_counts_with_iec_units() {
        assert_eq!(format_bytes(347), "347 B");
        assert_eq!(format_bytes(1024), "1.0 KiB");
        assert_eq!(format_bytes(13_421_773), "12.8 MiB");
        assert_eq!(format_bytes(5 * 1024 * 1024 * 1024), "5.0 GiB");
        assert_eq!(format_rate(0), "0 B/s");
    }

    #[test]
    fn formats_durations_without_limiting_hours() {
        assert_eq!(format_duration(Duration::from_secs(18 * 3_600 + 42 * 60)), "18:42:00");
        assert_eq!(format_duration(Duration::from_secs(100 * 3_600 + 2)), "100:00:02");
    }

    #[test]
    fn tracks_connected_and_disconnected_time_across_reconnections() {
        let started_at = Instant::now();
        let mut statistics = Statistics::new(started_at);
        statistics.mark_disconnected(started_at + Duration::from_secs(10));
        statistics.mark_reconnected(started_at + Duration::from_secs(15));

        let snapshot = statistics.snapshot(started_at + Duration::from_secs(25));
        assert_eq!(snapshot.connected, Duration::from_secs(20));
        assert_eq!(snapshot.disconnected, Duration::from_secs(5));
        assert_eq!(snapshot.total, Duration::from_secs(25));
        assert_eq!(snapshot.reconnects, 1);
    }

    #[test]
    fn tracks_payload_totals_and_recent_rates_separately() {
        let started_at = Instant::now();
        let mut statistics = Statistics::new(started_at);
        statistics.record_rx(1_024, started_at + Duration::from_millis(100));
        statistics.record_rx(512, started_at + Duration::from_millis(500));
        statistics.record_tx(12, started_at + Duration::from_millis(500));

        let active = statistics.snapshot(started_at + Duration::from_secs(1));
        assert_eq!(active.rx_bytes, 1_536);
        assert_eq!(active.tx_bytes, 12);
        assert_eq!(active.rx_rate, 1_536);
        assert_eq!(active.tx_rate, 12);

        let idle = statistics.snapshot(started_at + Duration::from_secs(6));
        assert_eq!(idle.rx_rate, 0);
        assert_eq!(idle.tx_rate, 0);
        assert_eq!(idle.average_rx_rate, 256);
        assert_eq!(idle.average_tx_rate, 2);
    }

    #[test]
    fn shows_disconnection_statistics_only_after_a_reconnection() {
        let started_at = Instant::now();
        let mut statistics = Statistics::new(started_at);
        let mut output = Vec::new();
        render(&mut output, &mut statistics, true).unwrap();
        let output = String::from_utf8(output).unwrap();
        assert!(!output.contains("Disconnected:"));
        assert!(!output.contains("Reconnects:"));

        statistics.mark_disconnected(started_at);
        statistics.mark_reconnected(started_at);
        let mut output = Vec::new();
        render(&mut output, &mut statistics, true).unwrap();
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("Disconnected:"));
        assert!(output.contains("Reconnects:"));
    }
}
