//! Terminal progress UI for Bitloom task graphs.

use std::future::Future;
use std::io::{self, Stdout};
use std::sync::Arc;
use std::time::Duration;

use bitloom_core::{TaskMonitor, TaskPhase, TaskSnapshot};
use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Paragraph, Row, Table, Widget};
use ratatui::{Terminal, TerminalOptions, Viewport};
use tokio::time::{MissedTickBehavior, interval};

const REFRESH_INTERVAL: Duration = Duration::from_millis(100);
const TEXT_TABLE_WIDTH: usize = 62;
const TEXT_LABEL_WIDTH: usize = 28;

pub struct TaskTable<'a> {
    tasks: &'a [TaskSnapshot],
}

impl<'a> TaskTable<'a> {
    pub const fn new(tasks: &'a [TaskSnapshot]) -> Self {
        Self { tasks }
    }
}

impl Widget for TaskTable<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        if area.is_empty() {
            return;
        }

        let [header_area, separator_area, rows_area] =
            Layout::vertical([Constraint::Length(1), Constraint::Length(1), Constraint::Min(0)])
                .areas(area);
        let status_width = self
            .tasks
            .iter()
            .map(|task| status_text(task).chars().count())
            .max()
            .unwrap_or(14)
            .max(14) as u16;
        let widths = [Constraint::Length(9), Constraint::Min(8), Constraint::Length(status_width)];

        Table::new(
            [Row::new(["ELAPSED", "TASK", "STATUS"]).style(Style::default().bold())],
            widths,
        )
        .column_spacing(2)
        .render(header_area, buffer);

        Paragraph::new("─".repeat(separator_area.width as usize))
            .style(Style::default().fg(Color::DarkGray))
            .render(separator_area, buffer);

        let rows = self.tasks.iter().map(|task| {
            Row::new([format_elapsed(task.elapsed), task.label.to_string(), status_text(task)])
                .style(status_style(task.phase))
        });
        Table::new(rows, widths).column_spacing(2).render(rows_area, buffer);
    }
}

/// Result of observing another future. A renderer error is reported only after
/// the observed operation finishes, so a broken terminal cannot cancel tasks.
pub struct Observed<T> {
    pub output: T,
    pub rendering_error: Option<io::Error>,
}

pub struct TerminalReporter {
    terminal: Terminal<CrosstermBackend<Stdout>>,
    warning_counts: Vec<usize>,
    finished: bool,
}

impl TerminalReporter {
    pub fn new(task_count: usize) -> io::Result<Self> {
        let height = u16::try_from(task_count.saturating_add(2)).unwrap_or(u16::MAX);
        let terminal = Terminal::with_options(
            CrosstermBackend::new(io::stdout()),
            TerminalOptions { viewport: Viewport::Inline(height) },
        )?;
        Ok(Self { terminal, warning_counts: vec![0; task_count], finished: false })
    }

    pub fn render(&mut self, monitor: &TaskMonitor) -> io::Result<()> {
        let snapshots = monitor.snapshots();
        self.insert_new_warnings(&snapshots)?;
        self.terminal.draw(|frame| {
            frame.render_widget(TaskTable::new(&snapshots), frame.area());
        })?;
        Ok(())
    }

    fn insert_new_warnings(&mut self, tasks: &[TaskSnapshot]) -> io::Result<()> {
        self.warning_counts.resize(tasks.len(), 0);
        let mut warnings = Vec::new();
        let mut next_counts = self.warning_counts.clone();
        for (index, task) in tasks.iter().enumerate() {
            let previous_count = self.warning_counts[index].min(task.warnings.len());
            warnings.extend(task.warnings[previous_count..].iter().cloned());
            next_counts[index] = task.warnings.len();
        }
        if warnings.is_empty() {
            return Ok(());
        }

        let block = warnings.iter().map(AsRef::as_ref).collect::<Vec<_>>().join("\n\n");
        insert_warning_block(&mut self.terminal, &block)?;
        self.warning_counts = next_counts;
        Ok(())
    }

    pub async fn observe<F>(&mut self, monitor: &TaskMonitor, future: F) -> Observed<F::Output>
    where
        F: Future,
    {
        let mut rendering_error = self.render(monitor).err();
        let mut refresh = interval(REFRESH_INTERVAL);
        refresh.set_missed_tick_behavior(MissedTickBehavior::Skip);
        tokio::pin!(future);

        let output = loop {
            tokio::select! {
                output = &mut future => break output,
                _ = refresh.tick(), if rendering_error.is_none() => {
                    rendering_error = self.render(monitor).err();
                }
            }
        };

        if rendering_error.is_none() {
            rendering_error = self.render(monitor).err();
        }
        Observed { output, rendering_error }
    }

    /// Leaves the cursor on a fresh line below the inline viewport.
    ///
    /// This must happen before the caller prints warnings or returns an error,
    /// otherwise normal stdout/stderr output can overwrite the final task row.
    pub fn finish(&mut self) -> io::Result<()> {
        if self.finished {
            return Ok(());
        }

        finish_inline_terminal(&mut self.terminal)?;
        self.finished = true;
        Ok(())
    }
}

impl Drop for TerminalReporter {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

fn finish_inline_terminal<B>(terminal: &mut Terminal<B>) -> Result<(), B::Error>
where
    B: Backend,
{
    let area = terminal.get_frame().area();
    let last_row = area.bottom().saturating_sub(1);
    terminal.set_cursor_position(Position::new(0, last_row))?;
    terminal.backend_mut().append_lines(1)?;
    terminal.show_cursor()?;
    terminal.backend_mut().flush()
}

fn insert_warning_block<B>(terminal: &mut Terminal<B>, warning: &str) -> Result<(), B::Error>
where
    B: Backend,
{
    let height = u16::try_from(warning.lines().count().saturating_add(1)).unwrap_or(u16::MAX);
    terminal.insert_before(height, |buffer| {
        Paragraph::new(warning)
            .style(Style::default().fg(Color::Yellow))
            .render(buffer.area, buffer);
    })
}

pub fn render_warnings(tasks: &[TaskSnapshot]) -> String {
    tasks
        .iter()
        .flat_map(|task| task.warnings.iter())
        .map(AsRef::as_ref)
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Plain-text fallback with the same stable columns as the interactive table.
pub fn render_text(tasks: &[TaskSnapshot]) -> String {
    let mut output = format!(
        " {:<9} {:<width$} {}\n{}",
        "ELAPSED",
        "TASK",
        "STATUS",
        "─".repeat(TEXT_TABLE_WIDTH),
        width = TEXT_LABEL_WIDTH,
    );
    for task in tasks {
        output.push_str(&format!(
            "\n {:<9} {:<width$} {}",
            format_elapsed(task.elapsed),
            truncate_label(&task.label, TEXT_LABEL_WIDTH),
            status_text(task),
            width = TEXT_LABEL_WIDTH,
        ));
    }
    output
}

pub fn format_elapsed(elapsed: Option<Duration>) -> String {
    let Some(elapsed) = elapsed else {
        return "--:--.-".to_owned();
    };

    let total_tenths = elapsed.as_millis() / 100;
    let minutes = (total_tenths / 600).min(99);
    let seconds = (total_tenths / 10) % 60;
    let tenths = total_tenths % 10;
    format!("{minutes:02}:{seconds:02}.{tenths}")
}

fn status_text(task: &TaskSnapshot) -> String {
    match task.phase {
        TaskPhase::Waiting => "◌ Waiting".to_owned(),
        TaskPhase::Ready => "◌ Ready".to_owned(),
        TaskPhase::Running => match task.progress {
            Some(progress) => format!("● Running {}/{}", progress.completed, progress.total),
            None => "● Running".to_owned(),
        },
        TaskPhase::Cached => "✓ Cached".to_owned(),
        TaskPhase::Finished => "✓ Finished".to_owned(),
        TaskPhase::Failed => "✗ Failed".to_owned(),
        TaskPhase::Cancelled => "⊘ Cancelled".to_owned(),
        TaskPhase::Skipped => "◌ Skipped".to_owned(),
        TaskPhase::RollingBack => "↶ Rolling back".to_owned(),
    }
}

fn status_style(phase: TaskPhase) -> Style {
    let color = match phase {
        TaskPhase::Cached | TaskPhase::Finished => Color::Green,
        TaskPhase::Failed => Color::Red,
        TaskPhase::Running => Color::Cyan,
        TaskPhase::RollingBack => Color::Yellow,
        TaskPhase::Waiting | TaskPhase::Ready | TaskPhase::Cancelled | TaskPhase::Skipped => {
            Color::DarkGray
        }
    };
    Style::default().fg(color)
}

fn truncate_label(label: &Arc<str>, width: usize) -> String {
    let count = label.chars().count();
    if count <= width {
        return label.to_string();
    }
    if width <= 1 {
        return "…".to_owned();
    }

    let mut truncated = label.chars().take(width - 1).collect::<String>();
    truncated.push('…');
    truncated
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;
    use std::time::Duration;

    use bitloom_core::{Task, TaskContext, TaskKind, TaskProgress, task};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;

    struct UiTask;

    impl Task for UiTask {
        type Output = ();
        type Error = Infallible;
        type Dependencies = ();

        const KIND: TaskKind = TaskKind::new("test.ui");

        fn label(&self) -> std::borrow::Cow<'static, str> {
            "UI task".into()
        }

        fn dependencies(&self) -> &Self::Dependencies {
            &()
        }

        async fn run(&self, _context: TaskContext, (): ()) -> Result<Self::Output, Self::Error> {
            Ok(())
        }
    }

    fn snapshot(
        _id: u64,
        label: &'static str,
        phase: TaskPhase,
        elapsed: Option<Duration>,
        progress: Option<TaskProgress>,
    ) -> TaskSnapshot {
        let mut snapshot = task(UiTask).snapshot();
        snapshot.label = Arc::from(label);
        snapshot.phase = phase;
        snapshot.progress = progress;
        snapshot.elapsed = elapsed;
        snapshot
    }

    #[test]
    fn plain_text_matches_progress_contract() {
        let tasks = [
            snapshot(
                1,
                "Load sources",
                TaskPhase::Finished,
                Some(Duration::from_millis(300)),
                None,
            ),
            snapshot(
                2,
                "Place & Route",
                TaskPhase::Running,
                Some(Duration::from_millis(24_300)),
                Some(TaskProgress { completed: 17, total: 64 }),
            ),
            snapshot(3, "Generate bitstream", TaskPhase::Waiting, None, None),
        ];

        assert_eq!(
            render_text(&tasks),
            concat!(
                " ELAPSED   TASK                         STATUS\n",
                "──────────────────────────────────────────────────────────────\n",
                " 00:00.3   Load sources                 ✓ Finished\n",
                " 00:24.3   Place & Route                ● Running 17/64\n",
                " --:--.-   Generate bitstream           ◌ Waiting",
            )
        );
    }

    #[test]
    fn ratatui_widget_renders_all_columns() {
        let tasks = [snapshot(
            1,
            "Place & Route",
            TaskPhase::Running,
            Some(Duration::from_millis(24_300)),
            Some(TaskProgress { completed: 17, total: 64 }),
        )];
        let mut terminal = Terminal::new(TestBackend::new(62, 3)).unwrap();
        terminal.draw(|frame| frame.render_widget(TaskTable::new(&tasks), frame.area())).unwrap();
        let buffer = terminal.backend().buffer();
        let rendered = (0..buffer.area.height)
            .map(|y| (0..buffer.area.width).map(|x| buffer[(x, y)].symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(rendered.contains("ELAPSED"));
        assert!(rendered.contains("Place & Route"));
        assert!(rendered.contains("Running 17/64"));
    }

    #[test]
    fn finishing_inline_output_moves_below_the_table() {
        let backend = TestBackend::new(20, 6);
        let mut terminal = Terminal::with_options(
            backend,
            TerminalOptions { viewport: Viewport::Fixed(Rect::new(0, 1, 20, 3)) },
        )
        .unwrap();

        finish_inline_terminal(&mut terminal).unwrap();

        let cursor = terminal.backend_mut().get_cursor_position().unwrap();
        assert_eq!(cursor.y, 4);
    }

    #[test]
    fn inserts_warnings_above_an_inline_table() {
        let backend = TestBackend::new(62, 12);
        let mut terminal =
            Terminal::with_options(backend, TerminalOptions { viewport: Viewport::Inline(3) })
                .unwrap();
        let tasks = [snapshot(
            1,
            "Parse partition layout",
            TaskPhase::Finished,
            Some(Duration::from_millis(100)),
            None,
        )];
        terminal.draw(|frame| frame.render_widget(TaskTable::new(&tasks), frame.area())).unwrap();

        insert_warning_block(&mut terminal, "Warning: directive declared after partitions")
            .unwrap();
        terminal.draw(|frame| frame.render_widget(TaskTable::new(&tasks), frame.area())).unwrap();

        let buffer = terminal.backend().buffer();
        let rows = (0..buffer.area.height)
            .map(|y| (0..buffer.area.width).map(|x| buffer[(x, y)].symbol()).collect::<String>())
            .collect::<Vec<_>>();
        let warning_row = rows.iter().position(|row| row.contains("Warning:")).unwrap();
        let table_row = rows.iter().position(|row| row.contains("ELAPSED")).unwrap();
        assert!(warning_row < table_row);
    }

    #[test]
    fn plain_warnings_follow_task_order() {
        let mut first = snapshot(1, "First", TaskPhase::Finished, None, None);
        first.warnings = vec![Arc::from("first warning"), Arc::from("second warning")];
        let mut second = snapshot(2, "Second", TaskPhase::Finished, None, None);
        second.warnings = vec![Arc::from("third warning")];

        assert_eq!(
            render_warnings(&[first, second]),
            "first warning\n\nsecond warning\n\nthird warning"
        );
    }

    #[test]
    fn elapsed_is_capped_to_two_digit_minutes() {
        assert_eq!(format_elapsed(Some(Duration::from_secs(7_200))), "99:00.0");
    }

    #[test]
    fn cancellation_has_a_distinct_status() {
        let cancelled = snapshot(1, "Synthesis", TaskPhase::Cancelled, None, None);

        assert_eq!(status_text(&cancelled), "⊘ Cancelled");
    }
}
