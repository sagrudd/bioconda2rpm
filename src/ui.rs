use crossterm::cursor::Show;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, Wrap};
use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

enum UiEvent {
    Log(String),
    Finish(String),
}

#[derive(Clone)]
struct PackageState {
    status: String,
    detail: String,
    seq: u64,
}

struct UiState {
    title: String,
    started: Instant,
    last_phase: String,
    last_status_line: String,
    queue_line: String,
    logs: VecDeque<String>,
    packages: BTreeMap<String, PackageState>,
    seq: u64,
    summary: Option<String>,
}

impl UiState {
    fn new(title: String) -> Self {
        Self {
            title,
            started: Instant::now(),
            last_phase: "starting".to_string(),
            last_status_line: "status=starting".to_string(),
            queue_line: String::new(),
            logs: VecDeque::new(),
            packages: BTreeMap::new(),
            seq: 0,
            summary: None,
        }
    }

    fn ingest_log(&mut self, line: String) {
        let cleaned = line
            .strip_prefix("progress ")
            .unwrap_or(line.as_str())
            .to_string();
        self.logs.push_back(cleaned.clone());
        while self.logs.len() > 16 {
            let _ = self.logs.pop_front();
        }

        if !cleaned.is_empty() {
            self.last_status_line = cleaned.clone();
        }

        let kv = parse_progress_kv(&cleaned);
        if let Some(phase) = kv.get("phase") {
            self.last_phase = phase.clone();
        }
        if kv.get("phase").map(|v| v.as_str()) == Some("batch-queue") {
            let status = kv.get("status").cloned().unwrap_or_default();
            let running = kv.get("running").cloned().unwrap_or_default();
            let queued = kv.get("queued").cloned().unwrap_or_default();
            let workers = kv.get("queue_workers").cloned().unwrap_or_default();
            self.queue_line = format!(
                "queue status={} running={} queued={} workers={}",
                status, running, queued, workers
            );
        }
        if kv.get("phase").map(|v| v.as_str()) == Some("container-build")
            && let Some(label) = kv.get("label")
        {
            let status = kv
                .get("status")
                .cloned()
                .unwrap_or_else(|| "running".to_string());
            let mapped_status = match status.as_str() {
                "running" | "started" => "running",
                "completed" => "generated",
                "failed" => "quarantined",
                other => other,
            };
            let detail = kv
                .get("elapsed")
                .cloned()
                .unwrap_or_else(|| "container-build".to_string());
            self.seq = self.seq.saturating_add(1);
            self.packages.insert(
                label.clone(),
                PackageState {
                    status: mapped_status.to_string(),
                    detail,
                    seq: self.seq,
                },
            );
        }
        if kv.get("phase").map(|v| v.as_str()) == Some("dependency-plan")
            && kv.get("status").map(|v| v.as_str()) == Some("completed")
        {
            if let Some(order) = kv.get("order") {
                for pkg in order.split("->").filter(|entry| !entry.is_empty()) {
                    if self.packages.contains_key(pkg) {
                        continue;
                    }
                    self.seq = self.seq.saturating_add(1);
                    self.packages.insert(
                        pkg.to_string(),
                        PackageState {
                            status: "pending".to_string(),
                            detail: "dependency-plan".to_string(),
                            seq: self.seq,
                        },
                    );
                }
            }
        }
        if kv.get("phase").map(|v| v.as_str()) == Some("dependency") {
            if let Some(dep) = kv.get("to") {
                let action = kv.get("action").map(|s| s.as_str()).unwrap_or_default();
                let dep_status = match action {
                    "follow" => "waiting",
                    "scan" => "queued",
                    "unresolved" => "blocked",
                    "skip" => "skipped",
                    _ => "queued",
                };
                self.seq = self.seq.saturating_add(1);
                self.packages.entry(dep.clone()).or_insert(PackageState {
                    status: dep_status.to_string(),
                    detail: "dependency-edge".to_string(),
                    seq: self.seq,
                });
            }
        }
        if let Some(pkg) = kv.get("package") {
            self.seq = self.seq.saturating_add(1);
            let phase = kv.get("phase").map(|s| s.as_str()).unwrap_or_default();
            let action = kv.get("action").map(|s| s.as_str()).unwrap_or_default();
            let inferred_status = match (phase, action) {
                ("dependency", "scan") | ("dependency", "follow") => "queued",
                ("dependency", "unresolved") => "blocked",
                ("dependency", "skip") => "skipped",
                ("dependency-plan", _) => "planned",
                _ => "queued",
            };
            let mut status = kv
                .get("status")
                .cloned()
                .unwrap_or_else(|| inferred_status.to_string());
            let mut detail = kv
                .get("reason")
                .or_else(|| kv.get("elapsed"))
                .cloned()
                .unwrap_or_else(|| match (phase, action) {
                    ("dependency", "scan") => "dependency-queue".to_string(),
                    ("dependency", "follow") => "dependency-follow".to_string(),
                    ("dependency", "unresolved") => "dependency-unresolved".to_string(),
                    ("dependency", "skip") => "dependency-skip".to_string(),
                    ("dependency-plan", _) => "dependency-plan".to_string(),
                    _ => phase.to_string(),
                });

            // Normalize scheduler events into user-facing package lifecycle states.
            if phase == "batch-queue" {
                status = match status.as_str() {
                    "dispatch" => "running".to_string(),
                    "completed" => kv
                        .get("result")
                        .cloned()
                        .unwrap_or_else(|| "completed".to_string()),
                    "cancelled" => "skipped".to_string(),
                    other => other.to_string(),
                };
                if status == "running" {
                    detail = format!(
                        "worker-dispatch running={} queued={}",
                        kv.get("running").cloned().unwrap_or_default(),
                        kv.get("queued").cloned().unwrap_or_default()
                    );
                } else if let Some(elapsed) = kv.get("elapsed") {
                    detail = elapsed.clone();
                }
            }
            self.packages.insert(
                pkg.clone(),
                PackageState {
                    status,
                    detail,
                    seq: self.seq,
                },
            );
        }
    }

    fn scheduler_counters(&self) -> (usize, usize, usize, usize) {
        let mut ready = 0usize;
        let mut running = 0usize;
        let mut completed = 0usize;
        let mut blocked = 0usize;

        for ps in self.packages.values() {
            match ps.status.as_str() {
                "running" | "started" => running += 1,
                "queued" | "waiting" | "pending" | "planned" => ready += 1,
                "generated" | "up-to-date" | "skipped" => completed += 1,
                "blocked" | "quarantined" => blocked += 1,
                _ => {}
            }
        }

        (ready, running, completed, blocked)
    }
}

pub struct ProgressUi {
    tx: Sender<UiEvent>,
    join: Option<JoinHandle<()>>,
}

impl ProgressUi {
    pub fn start(title: String) -> Self {
        let (tx, rx) = mpsc::channel::<UiEvent>();
        let join = thread::spawn(move || run_ui_loop(title, rx));
        Self {
            tx,
            join: Some(join),
        }
    }

    pub fn sink(&self) -> Arc<dyn Fn(String) + Send + Sync + 'static> {
        let tx = self.tx.clone();
        Arc::new(move |line: String| {
            let _ = tx.send(UiEvent::Log(line));
        })
    }

    pub fn finish(mut self, summary: String) {
        let _ = self.tx.send(UiEvent::Finish(summary));
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl Drop for ProgressUi {
    fn drop(&mut self) {
        if self.join.is_some() {
            let _ = self.tx.send(UiEvent::Finish(String::new()));
            if let Some(join) = self.join.take() {
                let _ = join.join();
            }
        }
    }
}

fn run_ui_loop(title: String, rx: Receiver<UiEvent>) {
    let mut state = UiState::new(title);
    let mut terminal = init_terminal().ok();
    let mut done = false;

    while !done {
        match rx.recv_timeout(Duration::from_millis(120)) {
            Ok(UiEvent::Log(line)) => {
                state.ingest_log(line);
            }
            Ok(UiEvent::Finish(summary)) => {
                if !summary.is_empty() {
                    state.summary = Some(summary);
                }
                done = true;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                done = true;
            }
        }

        while let Ok(evt) = rx.try_recv() {
            match evt {
                UiEvent::Log(line) => state.ingest_log(line),
                UiEvent::Finish(summary) => {
                    if !summary.is_empty() {
                        state.summary = Some(summary);
                    }
                    done = true;
                }
            }
        }

        if terminal.is_some() {
            while event::poll(Duration::from_millis(0)).unwrap_or(false) {
                if let Ok(Event::Key(key)) = event::read() {
                    if key.kind == KeyEventKind::Press
                        && key.code == KeyCode::Char('c')
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        crate::priority_specs::request_cancellation(
                            "cancelled by user (Ctrl-C in ratatui)",
                        );
                        state.summary =
                            Some("cancelling build and clearing queued work...".to_string());
                        done = true;
                        break;
                    }
                }
            }
        }

        if let Some(term) = terminal.as_mut() {
            let _ = term.draw(|f| draw_ui(f, &state));
        }
    }

    if let Some(mut term) = terminal {
        let _ = term.draw(|f| draw_ui(f, &state));
        restore_terminal(&mut term);
    }
}

fn init_terminal() -> Result<Terminal<CrosstermBackend<std::io::Stdout>>, ()> {
    let mut stdout = std::io::stdout();
    enable_raw_mode().map_err(|_| ())?;
    if execute!(stdout, EnterAlternateScreen).is_err() {
        let _ = disable_raw_mode();
        return Err(());
    }
    Terminal::new(CrosstermBackend::new(stdout)).map_err(|_| {
        let _ = disable_raw_mode();
    })
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>) {
    let _ = disable_raw_mode();
    let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen, Show);
    let _ = terminal.show_cursor();
}

fn draw_ui(frame: &mut ratatui::Frame<'_>, state: &UiState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Min(13),
            Constraint::Length(6),
            Constraint::Length(3),
        ])
        .split(frame.area());

    let elapsed = state.started.elapsed().as_secs();
    let header = Paragraph::new(format!(
        "{} | elapsed={}m{:02}s | Ctrl-C cancels",
        state.title,
        elapsed / 60,
        elapsed % 60
    ))
    .block(Block::default().borders(Borders::ALL).title("Build"));
    frame.render_widget(header, chunks[0]);

    let status_body = if state.queue_line.is_empty() {
        let (ready, running, completed, blocked) = state.scheduler_counters();
        format!(
            "phase={} | counters ready={} running={} completed={} blocked={} | {}",
            state.last_phase, ready, running, completed, blocked, state.last_status_line
        )
    } else {
        let (ready, running, completed, blocked) = state.scheduler_counters();
        format!(
            "phase={} | counters ready={} running={} completed={} blocked={} | {} | {}",
            state.last_phase,
            ready,
            running,
            completed,
            blocked,
            state.queue_line,
            state.last_status_line
        )
    };
    let status = Paragraph::new(status_body)
        .block(Block::default().borders(Borders::ALL).title("Status"))
        .wrap(Wrap { trim: true });
    frame.render_widget(status, chunks[1]);

    let mut rows = state
        .packages
        .iter()
        .map(|(pkg, ps)| (pkg.clone(), ps.clone()))
        .collect::<Vec<_>>();
    let rank = |status: &str| -> usize {
        match status {
            "running" | "started" => 0,
            "quarantined" | "blocked" => 1,
            "generated" | "up-to-date" => 2,
            "queued" | "waiting" => 3,
            "pending" | "planned" => 4,
            "skipped" => 5,
            _ => 6,
        }
    };
    rows.sort_by(|a, b| {
        rank(&a.1.status)
            .cmp(&rank(&b.1.status))
            .then_with(|| b.1.seq.cmp(&a.1.seq))
            .then_with(|| a.0.cmp(&b.0))
    });
    // Fit visible rows to current terminal height instead of a fixed cap.
    // Table has: top border + header + bottom border.
    let visible_capacity = chunks[2].height.saturating_sub(3) as usize;
    let visible_capacity = visible_capacity.max(1);
    let is_completed = |status: &str| matches!(status, "generated" | "up-to-date" | "skipped");
    let mut primary = rows
        .iter()
        .filter(|(_, ps)| !is_completed(&ps.status))
        .cloned()
        .collect::<Vec<_>>();
    let mut secondary = rows
        .into_iter()
        .filter(|(_, ps)| is_completed(&ps.status))
        .collect::<Vec<_>>();
    if primary.len() < visible_capacity {
        let remaining = visible_capacity - primary.len();
        secondary.truncate(remaining);
        primary.extend(secondary);
    } else {
        primary.truncate(visible_capacity);
    }
    let rows = primary;
    let table_rows = rows.into_iter().map(|(pkg, ps)| {
        let style = match ps.status.as_str() {
            "generated" => Style::default().fg(Color::Green),
            "up-to-date" => Style::default().fg(Color::LightGreen),
            "quarantined" => Style::default().fg(Color::Red),
            "skipped" => Style::default().fg(Color::Yellow),
            "queued" | "waiting" | "pending" | "planned" => Style::default().fg(Color::Blue),
            "blocked" => Style::default().fg(Color::LightRed),
            "started" | "running" => Style::default().fg(Color::Cyan),
            _ => Style::default(),
        };
        Row::new(vec![
            Cell::from(pkg),
            Cell::from(ps.status),
            Cell::from(ps.detail),
        ])
        .style(style)
    });
    let table = Table::new(
        table_rows,
        [
            Constraint::Length(28),
            Constraint::Length(14),
            Constraint::Min(20),
        ],
    )
    .header(Row::new(vec!["Package", "State", "Detail"]).style(Style::default().fg(Color::White)))
    .block(Block::default().borders(Borders::ALL).title("Packages"));
    frame.render_widget(table, chunks[2]);

    let log_text = state
        .logs
        .iter()
        .rev()
        .take(7)
        .cloned()
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n");
    let logs = Paragraph::new(log_text)
        .block(Block::default().borders(Borders::ALL).title("Recent Logs"))
        .wrap(Wrap { trim: true });
    frame.render_widget(logs, chunks[3]);

    let summary = Paragraph::new(
        state
            .summary
            .clone()
            .unwrap_or_else(|| "running...".to_string()),
    )
    .block(Block::default().borders(Borders::ALL).title("Summary"))
    .wrap(Wrap { trim: true });
    frame.render_widget(summary, chunks[4]);
}

fn parse_progress_kv(line: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for token in line.split_whitespace() {
        if let Some((key, value)) = token.split_once('=') {
            out.insert(key.to_string(), value.to_string());
        }
    }
    out
}
