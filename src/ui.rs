use crossterm::cursor::Show;
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
        if let Some(pkg) = kv.get("package") {
            self.seq = self.seq.saturating_add(1);
            let status = kv
                .get("status")
                .cloned()
                .unwrap_or_else(|| "unknown".to_string());
            let detail = kv
                .get("reason")
                .or_else(|| kv.get("elapsed"))
                .or_else(|| kv.get("phase"))
                .cloned()
                .unwrap_or_default();
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
            Constraint::Min(8),
            Constraint::Length(8),
            Constraint::Length(3),
        ])
        .split(frame.area());

    let elapsed = state.started.elapsed().as_secs();
    let header = Paragraph::new(format!(
        "{} | elapsed={}m{:02}s",
        state.title,
        elapsed / 60,
        elapsed % 60
    ))
    .block(Block::default().borders(Borders::ALL).title("Build"));
    frame.render_widget(header, chunks[0]);

    let status_body = if state.queue_line.is_empty() {
        format!("phase={}", state.last_phase)
    } else {
        format!("phase={} | {}", state.last_phase, state.queue_line)
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
    rows.sort_by(|a, b| b.1.seq.cmp(&a.1.seq).then_with(|| a.0.cmp(&b.0)));
    rows.truncate(12);
    let table_rows = rows.into_iter().map(|(pkg, ps)| {
        let style = match ps.status.as_str() {
            "generated" => Style::default().fg(Color::Green),
            "up-to-date" => Style::default().fg(Color::LightGreen),
            "quarantined" => Style::default().fg(Color::Red),
            "skipped" => Style::default().fg(Color::Yellow),
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
