//! Park-run TUI (additive to headless `porch agent`).

use std::collections::HashSet;
use std::io::{self, Stdout};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, Result};
use porch_gate::{Event, RunSnapshot, get_run, subscribe_events};
use porch_run::{AgentResponse, agent_respond};
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{self, Event as CEvent, KeyCode, KeyEventKind};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::{Frame, Terminal};

const PHASES: &[&str] = &["intent", "rebase", "review", "certify", "deliver"];

/// Finding row shown in the parked findings panel.
#[derive(Debug, Clone)]
pub struct FindingRow {
    pub id: String,
    pub severity: String,
    pub path: String,
}

/// Detach / quit without aborting the run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyAction {
    None,
    Detach,
}

/// Unit-testable UI model for one attached run.
pub struct App {
    pub snapshot: RunSnapshot,
    pub findings: Vec<FindingRow>,
    pub selected: HashSet<String>,
    pub cursor: usize,
    pub activity: Vec<String>,
    pub abort_armed: bool,
    pub working: bool,
    pub message: String,
    list_state: ListState,
    home: PathBuf,
    work_tree: PathBuf,
    /// Completes when the background `agent_respond` thread finishes.
    respond_done: Option<Receiver<()>>,
}

impl App {
    /// Build an app from a `get_run` snapshot.
    #[must_use]
    pub fn from_snapshot(snapshot: RunSnapshot, home: &Path, work_tree: &Path) -> Self {
        let findings = parse_findings(&snapshot.findings);
        let mut list_state = ListState::default();
        if !findings.is_empty() {
            list_state.select(Some(0));
        }
        Self {
            snapshot,
            findings,
            selected: HashSet::new(),
            cursor: 0,
            activity: Vec::new(),
            abort_armed: false,
            working: false,
            message: String::new(),
            list_state,
            home: home.to_path_buf(),
            work_tree: work_tree.to_path_buf(),
            respond_done: None,
        }
    }

    /// Whether action keys (approve/fix/skip/abort) are enabled.
    #[must_use]
    pub fn actions_enabled(&self) -> bool {
        self.snapshot.status == "parked" && !self.working
    }

    /// Apply a fresh snapshot (e.g. after `stream_gap` + `get_run`).
    pub fn apply_snapshot(&mut self, snapshot: RunSnapshot) {
        let findings = parse_findings(&snapshot.findings);
        self.snapshot = snapshot;
        self.findings = findings;
        if self.cursor >= self.findings.len() {
            self.cursor = self.findings.len().saturating_sub(1);
        }
        if self.findings.is_empty() {
            self.list_state.select(None);
        } else {
            self.list_state.select(Some(self.cursor));
        }
        self.abort_armed = false;
    }

    /// Clear `working` when the background respond thread has finished.
    ///
    /// CLI-side `agent_respond` does not publish into the daemon `EventHub`, so
    /// completion must be observed here — not via subscribe status transitions.
    ///
    /// Returns true when the attach loop should refresh the run snapshot: still
    /// working, or `working` just transitioned true→false on this poll.
    pub fn poll_respond_done(&mut self) -> bool {
        let was_working = self.working;
        let done = match &self.respond_done {
            Some(rx) => rx.try_recv().is_ok(),
            None => false,
        };
        if done {
            self.working = false;
            self.message.clear();
            self.respond_done = None;
        }
        let just_finished = was_working && !self.working;
        self.working || just_finished
    }

    /// Handle a key press. Returns [`KeyAction::Detach`] on `q`.
    pub fn handle_key(&mut self, code: KeyCode) -> KeyAction {
        match code {
            KeyCode::Char('q') => return KeyAction::Detach,
            KeyCode::Char('j') | KeyCode::Down => self.move_cursor(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_cursor(-1),
            KeyCode::Char(' ') => self.toggle_selection(),
            KeyCode::Char('a') if self.actions_enabled() => {
                self.spawn_respond(AgentResponse::Approve);
            }
            KeyCode::Char('s') if self.actions_enabled() => {
                self.spawn_respond(AgentResponse::Skip);
            }
            KeyCode::Char('f') if self.actions_enabled() => {
                let ids: Vec<String> = if self.selected.is_empty() {
                    Vec::new()
                } else {
                    self.selected.iter().cloned().collect()
                };
                let finding_ids = if ids.is_empty() { None } else { Some(ids) };
                self.spawn_respond(AgentResponse::Fix {
                    finding_ids,
                    yes: false,
                });
            }
            KeyCode::Char('x') if self.actions_enabled() => {
                if self.abort_armed {
                    self.abort_armed = false;
                    self.spawn_respond(AgentResponse::Abort);
                } else {
                    self.abort_armed = true;
                    self.message = "press x again to abort".into();
                }
            }
            _ => {
                if code != KeyCode::Char('x') {
                    self.abort_armed = false;
                }
            }
        }
        KeyAction::None
    }

    fn move_cursor(&mut self, delta: i32) {
        if self.findings.is_empty() {
            return;
        }
        let len = i64::try_from(self.findings.len()).unwrap_or(1);
        let cur = i64::try_from(self.cursor).unwrap_or(0);
        let next = (cur + i64::from(delta)).rem_euclid(len);
        self.cursor = usize::try_from(next).unwrap_or(0);
        self.list_state.select(Some(self.cursor));
    }

    fn toggle_selection(&mut self) {
        if let Some(f) = self.findings.get(self.cursor) {
            if !self.selected.remove(&f.id) {
                self.selected.insert(f.id.clone());
            }
        }
    }

    fn spawn_respond(&mut self, response: AgentResponse) {
        let home = self.home.clone();
        let work = self.work_tree.clone();
        let run_id = self.snapshot.run_id.clone();
        self.spawn_respond_job(move || {
            let _ = agent_respond(&home, Some(&run_id), &work, response);
        });
    }

    /// Run `job` on a background thread; clear `working` when it finishes.
    fn spawn_respond_job<F>(&mut self, job: F)
    where
        F: FnOnce() + Send + 'static,
    {
        self.working = true;
        self.message = "working…".into();
        self.abort_armed = false;
        let (tx, rx) = mpsc::channel();
        self.respond_done = Some(rx);
        let _ = std::thread::spawn(move || {
            job();
            let _ = tx.send(());
        });
    }

    /// Push an activity line (keeps last 200).
    pub fn push_activity(&mut self, text: impl Into<String>) {
        self.activity.push(text.into());
        if self.activity.len() > 200 {
            let drop_n = self.activity.len() - 200;
            self.activity.drain(0..drop_n);
        }
    }

    /// Render into a ratatui frame.
    pub fn render(&mut self, frame: &mut Frame<'_>) {
        let area = frame.area();
        let [pipeline, findings, activity, footer] = Layout::vertical([
            Constraint::Length(5),
            Constraint::Percentage(45),
            Constraint::Fill(1),
            Constraint::Length(2),
        ])
        .areas(area);

        let branch = &self.snapshot.branch;
        let status = &self.snapshot.status;
        let phase_line = PHASES
            .iter()
            .map(|p| {
                let st = self
                    .snapshot
                    .steps
                    .iter()
                    .rfind(|s| s.step == *p)
                    .map_or("-", |s| s.status.as_str());
                format!("{p}:{st}")
            })
            .collect::<Vec<_>>()
            .join("  ");
        let pipeline_text = format!("branch {branch}  status {status}\n{phase_line}");
        frame.render_widget(
            Paragraph::new(pipeline_text).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("pipeline (intent rebase review certify deliver)"),
            ),
            pipeline,
        );

        let items: Vec<ListItem> = self
            .findings
            .iter()
            .map(|f| {
                let mark = if self.selected.contains(&f.id) {
                    "[x]"
                } else {
                    "[ ]"
                };
                ListItem::new(format!("{mark} {} {} {}", f.id, f.severity, f.path))
            })
            .collect();
        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title("findings"))
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
            .highlight_symbol("> ");
        frame.render_stateful_widget(list, findings, &mut self.list_state);

        let log_lines: Vec<Line> = self
            .activity
            .iter()
            .rev()
            .take(activity.height.saturating_sub(2) as usize)
            .rev()
            .map(|t| Line::from(Span::raw(t.clone())))
            .collect();
        frame.render_widget(
            Paragraph::new(log_lines)
                .block(Block::default().borders(Borders::ALL).title("activity")),
            activity,
        );

        let keys = if self.actions_enabled() {
            "a approve  f fix  s skip  x abort  q detach"
        } else if self.working {
            "working…  q detach"
        } else {
            "q detach (actions when parked)"
        };
        let footer_text = if self.message.is_empty() {
            keys.to_string()
        } else {
            format!("{keys}  |  {}", self.message)
        };
        frame.render_widget(Paragraph::new(footer_text), footer);
    }
}

fn parse_findings(value: &serde_json::Value) -> Vec<FindingRow> {
    let Some(arr) = value.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|v| {
            Some(FindingRow {
                id: v.get("id")?.as_str()?.to_string(),
                severity: v
                    .get("severity")
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string(),
                path: v
                    .get("path")
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string(),
            })
        })
        .collect()
}

/// Draw `app` on a [`TestBackend`] and return the buffer as a string (for tests).
#[cfg(test)]
fn render_to_string(app: &mut App, width: u16, height: u16) -> String {
    use ratatui::backend::TestBackend;
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    terminal.draw(|frame| app.render(frame)).expect("draw");
    let buf = terminal.backend().buffer().clone();
    let mut out = String::new();
    for y in 0..height {
        for x in 0..width {
            out.push_str(buf[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

struct RawTerminal {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl RawTerminal {
    fn enter() -> Result<Self> {
        enable_raw_mode().context("enable raw mode")?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen).context("enter alt screen")?;
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend).context("terminal")?;
        Ok(Self { terminal })
    }
}

impl Drop for RawTerminal {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
        let _ = self.terminal.show_cursor();
    }
}

/// Live TTY attach loop for one run. Caller must ensure stdin is a TTY.
///
/// # Errors
///
/// Returns an error if the terminal cannot be initialized or RPC fails fatally.
pub fn run_attach(home: &Path, work_tree: &Path, run_id: &str) -> Result<()> {
    let snap = get_run(home, run_id).context("get_run")?;
    let app = Arc::new(Mutex::new(App::from_snapshot(snap, home, work_tree)));

    let home_sub = home.to_path_buf();
    let run_sub = run_id.to_string();
    let app_sub = Arc::clone(&app);
    let sub_thread: JoinHandle<()> = std::thread::spawn(move || {
        let _ = subscribe_events(&home_sub, Some(&run_sub), |ev| {
            if let Ok(mut guard) = app_sub.lock() {
                match ev {
                    Event::StreamGap { .. } => {
                        if let Ok(snap) = get_run(&home_sub, &run_sub) {
                            guard.apply_snapshot(snap);
                            guard.push_activity("stream_gap → refreshed get_run");
                        }
                    }
                    Event::State { run_id, .. } => {
                        if let Ok(snap) = get_run(&home_sub, &run_id) {
                            let status = snap.status.clone();
                            guard.apply_snapshot(snap);
                            // Do not clear `working` from status: approve stays parked,
                            // and CLI respond never publishes into this hub.
                            guard.push_activity(format!("state {status}"));
                        }
                    }
                    Event::Activity { text, .. } => {
                        guard.push_activity(text);
                    }
                }
            }
            true
        });
    });

    let mut term = RawTerminal::enter()?;
    loop {
        {
            let mut guard = app.lock().expect("app");
            term.terminal.draw(|frame| guard.render(frame))?;
        }
        if event::poll(Duration::from_millis(200))? {
            if let CEvent::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    let action = {
                        let mut guard = app.lock().expect("app");
                        guard.handle_key(key.code)
                    };
                    if action == KeyAction::Detach {
                        break;
                    }
                }
            }
        }
        // Clear working when respond thread finishes; refresh while busy or on
        // the completion tick (true→false) so terminal status replaces stale parked.
        if let Ok(mut guard) = app.lock() {
            if guard.poll_respond_done() {
                if let Ok(snap) = get_run(home, run_id) {
                    guard.apply_snapshot(snap);
                }
            }
        }
    }
    drop(term);
    // Subscriber ends when we drop... we can't easily cancel subscribe; process exit of
    // attach leaves the subscribe thread blocked until hangup — close by dropping app and
    // ignoring the join (daemon will see write fail when we exit... actually subscribe is
    // client-side reading. Detaching leaves the thread; we detach it.
    drop(sub_thread);
    Ok(())
}

/// Refresh helper used by gap-handling tests.
#[cfg(test)]
fn apply_gap_and_snapshot(app: &mut App, snapshot: RunSnapshot) {
    app.push_activity("stream_gap");
    app.apply_snapshot(snapshot);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parked_snapshot() -> RunSnapshot {
        RunSnapshot {
            run_id: "01TEST".into(),
            repo_id: "abcd".into(),
            branch: "feat/demo".into(),
            status: "parked".into(),
            sha: "abc".into(),
            head_sha: Some("abc".into()),
            base_sha: Some("def".into()),
            review_approved_head_sha: None,
            error: None,
            pr_url: None,
            worktree_dir: None,
            findings: serde_json::json!([
                {"id":"f0","severity":"high","path":"src/a.rs"},
                {"id":"f1","severity":"medium","path":"src/b.rs"}
            ]),
            steps: vec![
                porch_gate::StepSnapshot {
                    step: "intent".into(),
                    status: "completed".into(),
                    error: None,
                },
                porch_gate::StepSnapshot {
                    step: "rebase".into(),
                    status: "completed".into(),
                    error: None,
                },
                porch_gate::StepSnapshot {
                    step: "review".into(),
                    status: "parked".into(),
                    error: None,
                },
            ],
            state_rev: 1,
        }
    }

    #[test]
    fn parked_enables_actions_running_does_not() {
        let home = PathBuf::from("/tmp");
        let mut app = App::from_snapshot(parked_snapshot(), &home, &home);
        assert!(app.actions_enabled());
        app.snapshot.status = "running".into();
        assert!(!app.actions_enabled());
        // approve key ignored when not parked
        let _ = app.handle_key(KeyCode::Char('a'));
        assert!(!app.working);
    }

    #[test]
    fn abort_requires_two_x_presses() {
        let home = PathBuf::from("/tmp");
        let mut app = App::from_snapshot(parked_snapshot(), &home, &home);
        assert!(!app.abort_armed);
        let _ = app.handle_key(KeyCode::Char('x'));
        assert!(app.abort_armed);
        assert!(!app.working);
        // second x spawns abort (working)
        let _ = app.handle_key(KeyCode::Char('x'));
        assert!(app.working);
    }

    #[test]
    fn space_toggles_finding_selection() {
        let home = PathBuf::from("/tmp");
        let mut app = App::from_snapshot(parked_snapshot(), &home, &home);
        assert!(app.selected.is_empty());
        let _ = app.handle_key(KeyCode::Char(' '));
        assert!(app.selected.contains("f0"));
        let _ = app.handle_key(KeyCode::Char(' '));
        assert!(!app.selected.contains("f0"));
    }

    #[test]
    fn render_contains_branch_and_phase_names() {
        let home = PathBuf::from("/tmp");
        let mut app = App::from_snapshot(parked_snapshot(), &home, &home);
        let s = render_to_string(&mut app, 80, 24);
        assert!(s.contains("feat/demo"), "buffer={s}");
        assert!(s.contains("intent"), "buffer={s}");
        assert!(s.contains("rebase"), "buffer={s}");
        assert!(s.contains("review"), "buffer={s}");
        assert!(s.contains("certify"), "buffer={s}");
        assert!(s.contains("deliver"), "buffer={s}");
        assert!(s.contains("parked"), "buffer={s}");
    }

    #[test]
    fn gap_then_snapshot_replaces_stale_status() {
        let home = PathBuf::from("/tmp");
        let mut app = App::from_snapshot(parked_snapshot(), &home, &home);
        app.snapshot.status = "running".into();
        let mut next = parked_snapshot();
        next.status = "completed".into();
        apply_gap_and_snapshot(&mut app, next);
        assert_eq!(app.snapshot.status, "completed");
        assert!(app.activity.iter().any(|a| a.contains("stream_gap")));
    }

    #[test]
    fn working_clears_when_respond_finishes_while_parked() {
        let home = PathBuf::from("/tmp");
        let mut app = App::from_snapshot(parked_snapshot(), &home, &home);
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();

        app.spawn_respond_job(move || {
            let _ = started_tx.send(());
            let _ = release_rx.recv();
        });

        assert!(app.working);
        assert!(!app.actions_enabled());
        assert_eq!(app.snapshot.status, "parked");
        started_rx.recv().expect("respond thread started");

        // Still parked and still working until the job completes; refresh while busy.
        assert!(app.poll_respond_done());
        assert!(app.working);
        assert!(!app.actions_enabled());

        release_tx.send(()).expect("release respond");
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            let refresh = app.poll_respond_done();
            if refresh && !app.working {
                // Completion tick: attach loop refreshes even though working cleared.
                let mut done = parked_snapshot();
                done.status = "completed".into();
                app.apply_snapshot(done);
                break;
            }
            assert!(
                app.working,
                "respond completion must report refresh on true→false"
            );
            assert!(
                std::time::Instant::now() < deadline,
                "respond completion not observed"
            );
            std::thread::sleep(Duration::from_millis(10));
        }

        assert!(!app.working);
        assert_eq!(app.snapshot.status, "completed");
        assert!(!app.actions_enabled());
    }
}
