//! Park-run TUI (additive to headless `porch agent`).

use std::collections::{HashMap, HashSet};
use std::io::{self, Stdout};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, Result};
use porch_gate::{
    Event, RunSnapshot, get_finding_hunk, get_run, load_finding_notes, set_finding_note,
    subscribe_events,
};
use porch_run::{AgentResponse, agent_respond, sync_hint_for};
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
    pub message: String,
    pub start_line: Option<u32>,
    pub end_line: Option<u32>,
}

impl FindingRow {
    /// `path`, `path:line`, or `path:start-end` when line anchors are known.
    #[must_use]
    pub fn path_loc(&self) -> String {
        match (self.start_line, self.end_line) {
            (Some(start), Some(end)) if end > start => {
                format!("{}:{start}-{end}", self.path)
            }
            (Some(start), _) => format!("{}:{start}", self.path),
            _ => self.path.clone(),
        }
    }
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
    /// Show on-demand hunk/diff for the cursor finding.
    pub show_detail: bool,
    /// Cached hunk text for the detail panel.
    pub detail_hunk: String,
    /// Per-finding operator notes (also persisted under `$PORCH_HOME/runs/<id>/`).
    pub notes: HashMap<String, String>,
    /// When set, keystrokes edit the note for this finding id.
    pub note_editing: Option<String>,
    pub note_draft: String,
    list_state: ListState,
    home: PathBuf,
    work_tree: PathBuf,
    /// Completes with a footer message when the background respond thread finishes.
    respond_done: Option<Receiver<String>>,
}

impl App {
    /// Build an app from a `get_run` snapshot.
    #[must_use]
    pub fn from_snapshot(snapshot: RunSnapshot, home: &Path, work_tree: &Path) -> Self {
        let findings = parse_findings(&snapshot.findings);
        let notes = load_finding_notes(home, &snapshot.run_id).unwrap_or_default();
        let mut list_state = ListState::default();
        if !findings.is_empty() {
            list_state.select(Some(0));
        }
        let sync_msg = sync_hint_for(home, work_tree).unwrap_or_default();
        Self {
            snapshot,
            findings,
            selected: HashSet::new(),
            cursor: 0,
            activity: Vec::new(),
            abort_armed: false,
            working: false,
            message: sync_msg,
            show_detail: false,
            detail_hunk: String::new(),
            notes: notes.into_iter().collect(),
            note_editing: None,
            note_draft: String::new(),
            list_state,
            home: home.to_path_buf(),
            work_tree: work_tree.to_path_buf(),
            respond_done: None,
        }
    }

    /// Whether action keys (approve/fix/skip/abort) are enabled.
    #[must_use]
    pub fn actions_enabled(&self) -> bool {
        self.snapshot.status == "parked" && !self.working && self.note_editing.is_none()
    }

    /// Apply a fresh snapshot (e.g. after `stream_gap` + `get_run`).
    pub fn apply_snapshot(&mut self, snapshot: RunSnapshot) {
        let findings = parse_findings(&snapshot.findings);
        if let Ok(notes) = load_finding_notes(&self.home, &snapshot.run_id) {
            self.notes = notes.into_iter().collect();
        }
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
        if self.show_detail {
            self.refresh_detail_hunk();
        }
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
        let done_msg = match &self.respond_done {
            Some(rx) => rx.try_recv().ok(),
            None => None,
        };
        if let Some(msg) = done_msg {
            self.working = false;
            self.message = msg;
            self.respond_done = None;
        }
        let just_finished = was_working && !self.working;
        self.working || just_finished
    }

    /// Handle a key press. Returns [`KeyAction::Detach`] on `q`.
    pub fn handle_key(&mut self, code: KeyCode) -> KeyAction {
        if let Some(finding_id) = self.note_editing.clone() {
            return self.handle_note_key(code, &finding_id);
        }

        match code {
            KeyCode::Char('q') => return KeyAction::Detach,
            KeyCode::Char('j') | KeyCode::Down => {
                self.move_cursor(1);
                if self.show_detail {
                    self.refresh_detail_hunk();
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.move_cursor(-1);
                if self.show_detail {
                    self.refresh_detail_hunk();
                }
            }
            KeyCode::Char(' ') => self.toggle_selection(),
            KeyCode::Char('d') => self.toggle_detail(),
            KeyCode::Char('n') if self.actions_enabled() => self.begin_note_edit(),
            KeyCode::Char('a') if self.actions_enabled() => {
                self.spawn_respond(AgentResponse::Approve);
            }
            KeyCode::Char('s') if self.actions_enabled() => {
                self.spawn_respond(AgentResponse::Skip);
            }
            KeyCode::Char('f') if self.actions_enabled() => {
                self.spawn_fix(false);
            }
            KeyCode::Char('y') if self.actions_enabled() => {
                self.spawn_fix(true);
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

    fn handle_note_key(&mut self, code: KeyCode, finding_id: &str) -> KeyAction {
        match code {
            KeyCode::Esc => {
                self.note_editing = None;
                self.note_draft.clear();
                self.message.clear();
            }
            KeyCode::Enter => {
                let draft = self.note_draft.clone();
                match set_finding_note(&self.home, &self.snapshot.run_id, finding_id, &draft) {
                    Ok(()) => {
                        if draft.is_empty() {
                            self.notes.remove(finding_id);
                        } else {
                            self.notes.insert(finding_id.to_string(), draft);
                        }
                        self.message = format!("note saved for {finding_id}");
                    }
                    Err(e) => {
                        self.message = format!("note error: {e}");
                    }
                }
                self.note_editing = None;
                self.note_draft.clear();
            }
            KeyCode::Backspace => {
                self.note_draft.pop();
            }
            KeyCode::Char(c) => {
                self.note_draft.push(c);
            }
            _ => {}
        }
        KeyAction::None
    }

    fn begin_note_edit(&mut self) {
        let Some(f) = self.findings.get(self.cursor) else {
            return;
        };
        self.note_draft = self.notes.get(&f.id).cloned().unwrap_or_default();
        self.note_editing = Some(f.id.clone());
        self.message = format!("editing note for {} (Enter save, Esc cancel)", f.id);
        self.abort_armed = false;
    }

    fn toggle_detail(&mut self) {
        self.show_detail = !self.show_detail;
        if self.show_detail {
            self.refresh_detail_hunk();
        } else {
            self.detail_hunk.clear();
        }
    }

    /// Fetch hunk/diff for the cursor finding via daemon RPC.
    pub fn refresh_detail_hunk(&mut self) {
        let Some(f) = self.findings.get(self.cursor) else {
            self.detail_hunk = String::new();
            return;
        };
        match get_finding_hunk(&self.home, &self.snapshot.run_id, &f.id) {
            Ok(v) => {
                if let Some(err) = v.get("error").and_then(|e| e.as_str()) {
                    self.detail_hunk = format!("error: {err}");
                } else {
                    let hunk = v.get("hunk").and_then(|h| h.as_str()).unwrap_or("");
                    let truncated = v
                        .get("truncated")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false);
                    let source = v.get("source").and_then(|s| s.as_str()).unwrap_or("hunk");
                    let mut body = format!("[{source}] {}\n", f.path_loc());
                    if !f.message.is_empty() {
                        body.push_str(&f.message);
                        body.push('\n');
                    }
                    body.push_str(hunk);
                    if truncated {
                        body.push_str("\n… truncated");
                    }
                    self.detail_hunk = body;
                }
            }
            Err(e) => {
                self.detail_hunk = format!("rpc error: {e}");
            }
        }
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

    fn spawn_fix(&mut self, yes: bool) {
        let ids: Vec<String> = if self.selected.is_empty() {
            Vec::new()
        } else {
            self.selected.iter().cloned().collect()
        };
        let finding_ids = if ids.is_empty() { None } else { Some(ids) };
        self.spawn_respond(AgentResponse::Fix { finding_ids, yes });
    }

    fn spawn_respond(&mut self, response: AgentResponse) {
        let home = self.home.clone();
        let work = self.work_tree.clone();
        let run_id = self.snapshot.run_id.clone();
        let label = match &response {
            AgentResponse::Approve => "approve",
            AgentResponse::Skip => "skip",
            AgentResponse::Abort => "abort",
            AgentResponse::Fix { yes: true, .. } => "fix --yes",
            AgentResponse::Fix { yes: false, .. } => "fix",
            AgentResponse::Compose { .. } => "compose respond",
        }
        .to_string();
        self.spawn_respond_job(move || {
            let result = agent_respond(&home, Some(&run_id), &work, response);
            respond_footer_message(&label, &result)
        });
    }

    /// Run `job` on a background thread; clear `working` when it finishes.
    fn spawn_respond_job<F>(&mut self, job: F)
    where
        F: FnOnce() -> String + Send + 'static,
    {
        self.working = true;
        self.message = "working…".into();
        self.abort_armed = false;
        let (tx, rx) = mpsc::channel();
        self.respond_done = Some(rx);
        let _ = std::thread::spawn(move || {
            let msg = job();
            let _ = tx.send(msg);
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
        let detail_h = if self.show_detail { 8u16 } else { 0 };
        let note_h = if self.note_editing.is_some() { 3u16 } else { 0 };
        let [pipeline, findings, detail, note_bar, activity, footer] = Layout::vertical([
            Constraint::Length(5),
            Constraint::Percentage(if self.show_detail { 30 } else { 45 }),
            Constraint::Length(detail_h),
            Constraint::Length(note_h),
            Constraint::Fill(1),
            Constraint::Length(2),
        ])
        .areas(area);

        self.render_pipeline(frame, pipeline);
        self.render_findings(frame, findings);
        if self.show_detail {
            frame.render_widget(
                Paragraph::new(self.detail_hunk.clone()).block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title("detail (d toggle)"),
                ),
                detail,
            );
        }
        if self.note_editing.is_some() {
            frame.render_widget(
                Paragraph::new(format!("note> {}", self.note_draft)).block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title("note (Enter save Esc cancel)"),
                ),
                note_bar,
            );
        }
        self.render_activity(frame, activity);
        let keys = footer_keys(self);
        let footer_text = if self.message.is_empty() {
            keys.to_string()
        } else {
            format!("{keys}  |  {}", self.message)
        };
        frame.render_widget(Paragraph::new(footer_text), footer);
    }

    fn render_pipeline(&self, frame: &mut Frame<'_>, area: ratatui::layout::Rect) {
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
            area,
        );
    }

    fn render_findings(&mut self, frame: &mut Frame<'_>, area: ratatui::layout::Rect) {
        let items: Vec<ListItem> = self
            .findings
            .iter()
            .map(|f| {
                let mark = if self.selected.contains(&f.id) {
                    "[x]"
                } else {
                    "[ ]"
                };
                let note_mark = if self.notes.contains_key(&f.id) {
                    "*"
                } else {
                    " "
                };
                let msg = truncate_chars(&f.message, 48);
                ListItem::new(format!(
                    "{mark}{note_mark} {} {} {}  {msg}",
                    f.id,
                    f.severity,
                    f.path_loc()
                ))
            })
            .collect();
        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title("findings"))
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
            .highlight_symbol("> ");
        frame.render_stateful_widget(list, area, &mut self.list_state);
    }

    fn render_activity(&self, frame: &mut Frame<'_>, area: ratatui::layout::Rect) {
        let log_lines: Vec<Line> = self
            .activity
            .iter()
            .rev()
            .take(area.height.saturating_sub(2) as usize)
            .rev()
            .map(|t| Line::from(Span::raw(t.clone())))
            .collect();
        frame.render_widget(
            Paragraph::new(log_lines)
                .block(Block::default().borders(Borders::ALL).title("activity")),
            area,
        );
    }
}

fn footer_keys(app: &App) -> &'static str {
    if app.note_editing.is_some() {
        "Enter save  Esc cancel"
    } else if app.actions_enabled() {
        "a approve  f fix  y fix--yes  s skip  x abort  d detail  n note  q detach"
    } else if app.working {
        "working…  q detach"
    } else {
        "d detail  q detach (actions when parked)"
    }
}

fn respond_footer_message(label: &str, result: &porch_run::AgentCliResult) -> String {
    if result.exit_code == 0 {
        let status = serde_json::from_str::<serde_json::Value>(&result.json)
            .ok()
            .and_then(|v| v.get("status").and_then(|s| s.as_str()).map(str::to_string))
            .unwrap_or_else(|| "ok".into());
        format!("{label} ok ({status})")
    } else {
        let err = serde_json::from_str::<serde_json::Value>(&result.json)
            .ok()
            .and_then(|v| v.get("error").and_then(|s| s.as_str()).map(str::to_string))
            .unwrap_or_else(|| format!("exit {}", result.exit_code));
        format!("{label} error: {err}")
    }
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
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
                message: v
                    .get("message")
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string(),
                start_line: v
                    .get("start_line")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|n| u32::try_from(n).ok()),
                end_line: v
                    .get("end_line")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|n| u32::try_from(n).ok()),
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
    use porch_gate::{Db, db_path, set_finding_note, wait_for_health};
    use std::sync::atomic::AtomicBool;
    use tempfile::TempDir;

    struct NoopExecutor;

    impl porch_gate::RunExecutor for NoopExecutor {
        fn execute(&self, _home: &Path, _run_id: &str, _cancel: &AtomicBool) {}

        fn recover_stale(&self, _home: &Path) -> std::result::Result<(), String> {
            Ok(())
        }
    }

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
                {
                    "id":"f0",
                    "severity":"warning",
                    "path":"src/a.rs",
                    "message":"null check missing",
                    "start_line":10,
                    "end_line":12
                },
                {
                    "id":"f1",
                    "severity":"error",
                    "path":"src/b.rs",
                    "message":"unwrap on user input",
                    "start_line":3,
                    "end_line":3
                }
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
        let s = render_to_string(&mut app, 100, 24);
        assert!(s.contains("feat/demo"), "buffer={s}");
        assert!(s.contains("intent"), "buffer={s}");
        assert!(s.contains("rebase"), "buffer={s}");
        assert!(s.contains("review"), "buffer={s}");
        assert!(s.contains("certify"), "buffer={s}");
        assert!(s.contains("deliver"), "buffer={s}");
        assert!(s.contains("parked"), "buffer={s}");
    }

    #[test]
    fn render_shows_finding_message_and_path_line() {
        let home = PathBuf::from("/tmp");
        let mut app = App::from_snapshot(parked_snapshot(), &home, &home);
        let s = render_to_string(&mut app, 120, 24);
        assert!(s.contains("null check missing"), "buffer={s}");
        assert!(s.contains("src/a.rs:10-12"), "buffer={s}");
        assert!(
            s.contains("a approve") && s.contains("y fix--yes") && s.contains("n note"),
            "footer keys missing: {s}"
        );
    }

    #[test]
    fn y_spawns_fix_yes_and_f_spawns_fix_without_yes() {
        let home = PathBuf::from("/tmp");
        let mut app = App::from_snapshot(parked_snapshot(), &home, &home);
        // Capture via channel by overriding spawn: press y and observe working + message.
        let _ = app.handle_key(KeyCode::Char('y'));
        assert!(app.working);
        assert_eq!(app.message, "working…");
        // Let the background respond fail closed (no daemon/db) and settle message.
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            if app.poll_respond_done() && !app.working {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "y respond hung");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            app.message.contains("fix --yes"),
            "expected fix --yes label, got {}",
            app.message
        );

        let mut app = App::from_snapshot(parked_snapshot(), &home, &home);
        let _ = app.handle_key(KeyCode::Char('f'));
        assert!(app.working);
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            if app.poll_respond_done() && !app.working {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "f respond hung");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            app.message.contains("fix ") || app.message.starts_with("fix "),
            "expected fix label, got {}",
            app.message
        );
        assert!(
            !app.message.contains("fix --yes"),
            "f must not use --yes: {}",
            app.message
        );
    }

    #[test]
    fn note_persists_under_run_artifact_dir() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().canonicalize().unwrap();
        let mut snap = parked_snapshot();
        snap.run_id = "note-run".into();
        let mut app = App::from_snapshot(snap, &home, &home);
        let _ = app.handle_key(KeyCode::Char('n'));
        assert_eq!(app.note_editing.as_deref(), Some("f0"));
        for c in "use Option".chars() {
            let _ = app.handle_key(KeyCode::Char(c));
        }
        let _ = app.handle_key(KeyCode::Enter);
        assert!(app.note_editing.is_none());
        assert_eq!(app.notes.get("f0").map(String::as_str), Some("use Option"));
        let loaded = load_finding_notes(&home, "note-run").unwrap();
        assert_eq!(loaded.get("f0").map(String::as_str), Some("use Option"));
        assert!(app.message.contains("note saved"), "{}", app.message);
    }

    #[test]
    fn detail_panel_fetches_hunk_via_rpc() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().canonicalize().unwrap();
        std::fs::create_dir_all(&home).unwrap();
        let wt = home.join("wt");
        std::fs::create_dir_all(wt.join("src")).unwrap();
        std::fs::write(
            wt.join("src/a.rs"),
            "fn a() {}\nfn b() {}\nfn c() { todo!() }\nfn d() {}\n",
        )
        .unwrap();

        let db = Db::open(&db_path(&home)).unwrap();
        db.upsert_repo("repo1", &home, &home.join("bare.git"), "main")
            .unwrap();
        let run = db
            .insert_run("repo1", "feat/demo", "abc123", None, None)
            .unwrap();
        db.set_run_status(&run.id, "parked", None).unwrap();
        db.set_worktree_dir(&run.id, &wt).unwrap();
        db.set_findings_json(
            &run.id,
            Some(
                r#"[{"id":"f0","severity":"warning","path":"src/a.rs","message":"todo left","action":"ask-user","start_line":3,"end_line":3}]"#,
            ),
        )
        .unwrap();

        let exec: Arc<dyn porch_gate::RunExecutor> = Arc::new(NoopExecutor);
        // ensure_daemon wants a concrete executor type via PipelineExecutor in prod;
        // use wait + thread like gate tests.
        let home_d = home.clone();
        // Keep JoinHandle alive for the test body; do not killpg the test binary pid.
        let daemon = std::thread::spawn(move || {
            let _ = porch_gate::run_daemon(&home_d, &exec);
        });
        wait_for_health(&home, Duration::from_secs(5)).unwrap();

        let snap = get_run(&home, &run.id).unwrap();
        let mut app = App::from_snapshot(snap, &home, &wt);
        let _ = app.handle_key(KeyCode::Char('d'));
        assert!(app.show_detail);
        assert!(
            app.detail_hunk.contains("todo left"),
            "detail should show full finding message: {}",
            app.detail_hunk
        );
        assert!(
            app.detail_hunk.contains("todo!()") || app.detail_hunk.contains("fn c"),
            "detail={}",
            app.detail_hunk
        );
        let s = render_to_string(&mut app, 100, 30);
        assert!(s.contains("detail"), "buffer={s}");
        std::mem::forget(daemon);
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
            "approve ok (completed)".into()
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
        assert_eq!(app.message, "approve ok (completed)");
        assert_eq!(app.snapshot.status, "completed");
        assert!(!app.actions_enabled());
    }

    #[test]
    fn set_finding_note_round_trip_helper() {
        let tmp = TempDir::new().unwrap();
        set_finding_note(tmp.path(), "r1", "f0", "hi").unwrap();
        let map = load_finding_notes(tmp.path(), "r1").unwrap();
        assert_eq!(map.get("f0").map(String::as_str), Some("hi"));
    }
}
