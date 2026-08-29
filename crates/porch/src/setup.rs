//! `porch setup` — headless JSON + easy one-screen TUI.

use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Result, bail};
use porch_review::{
    DetectedEngine, EngineKind, SetupResult, default_engine, detect_engines, detect_optional_tools,
    review_setup_ok, setup_apply, setup_verify, setup_yes,
};
use ratatui::Frame;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{self, Event as CEvent, KeyCode, KeyEventKind};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

/// CLI flags for `porch setup`.
#[derive(Debug, Clone)]
pub struct SetupArgs {
    pub yes: bool,
    pub verify: bool,
    pub apply: bool,
    pub engine: Option<String>,
}

/// Run `porch setup` (TTY wizard or headless JSON).
pub fn run(home: &Path, args: &SetupArgs) -> Result<ExitCode> {
    if args.yes || args.verify || args.apply || !io::stdin().is_terminal() {
        return run_headless(home, args);
    }
    run_wizard(home)
}

fn run_headless(home: &Path, args: &SetupArgs) -> Result<ExitCode> {
    if args.verify && (args.yes || args.apply) {
        bail!("--verify cannot combine with --yes / --apply");
    }
    if args.apply && args.yes {
        bail!("--apply cannot combine with --yes");
    }
    let engine = match args.engine.as_deref() {
        None => None,
        Some(s) => {
            if let Some(k) = EngineKind::parse(s) {
                Some(k)
            } else {
                let result = SetupResult {
                    ok: false,
                    engine: None,
                    wrapper: None,
                    verified: false,
                    warnings: Vec::new(),
                    error: Some(format!("unknown engine `{s}` (expected ocr|generic)")),
                };
                println!("{}", serde_json::to_string_pretty(&result)?);
                return Ok(ExitCode::from(2));
            }
        }
    };

    let result = if args.verify {
        setup_verify(home)?
    } else if args.apply {
        setup_apply(home)?
    } else {
        // `--yes`, forced `--engine`, or non-TTY bare `porch setup`.
        setup_yes(home, engine)?
    };
    emit_setup_json(&result)
}

fn emit_setup_json(result: &SetupResult) -> Result<ExitCode> {
    println!("{}", serde_json::to_string_pretty(result)?);
    Ok(if result.ok {
        ExitCode::SUCCESS
    } else if result
        .error
        .as_deref()
        .is_some_and(|e| e.contains("unknown"))
    {
        ExitCode::from(2)
    } else {
        ExitCode::from(1)
    })
}

// --- Easy one-screen wizard -------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WizardScreen {
    Select,
    Verifying,
    Success,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WizardAction {
    None,
    Quit,
    /// Operator asked to apply the focused engine.
    Apply,
    /// Leave without writing.
    Skip,
}

#[cfg(test)]
type ApplyHook = Box<dyn FnMut(EngineKind) -> Result<SetupResult, String>>;

/// Unit-testable setup wizard model (one screen + result).
pub struct SetupWizard {
    pub screen: WizardScreen,
    pub engines: Vec<DetectedEngine>,
    pub focus: usize,
    pub gh: Option<PathBuf>,
    pub fixer: Option<PathBuf>,
    pub tools_line: String,
    pub error: Option<String>,
    pub success_wrapper: Option<PathBuf>,
    pub success_engine: Option<String>,
    /// When set, Enter on Select invokes this instead of real setup (tests).
    #[cfg(test)]
    apply_hook: Option<ApplyHook>,
}

impl SetupWizard {
    /// Build wizard from PATH detection.
    #[must_use]
    pub fn detect() -> Self {
        let engines = detect_engines();
        let (fixer, gh, tools) = detect_optional_tools();
        let focus = default_focus(&engines);
        let tools_line = format_tools_line(&tools);
        Self {
            screen: WizardScreen::Select,
            engines,
            focus,
            gh,
            fixer,
            tools_line,
            error: None,
            success_wrapper: None,
            success_engine: None,
            #[cfg(test)]
            apply_hook: None,
        }
    }

    /// Inject an apply hook (unit tests).
    #[cfg(test)]
    pub fn with_apply_hook<F>(mut self, hook: F) -> Self
    where
        F: FnMut(EngineKind) -> Result<SetupResult, String> + 'static,
    {
        self.apply_hook = Some(Box::new(hook));
        self
    }

    /// Focused engine kind, if any selectable.
    #[must_use]
    pub fn focused_engine(&self) -> Option<EngineKind> {
        self.engines.get(self.focus).map(|e| e.kind)
    }

    /// Handle a key. Returns [`WizardAction::Apply`] when Enter should run setup.
    pub fn handle_key(&mut self, code: KeyCode) -> WizardAction {
        match self.screen {
            WizardScreen::Success => match code {
                KeyCode::Enter | KeyCode::Char('q') | KeyCode::Esc => WizardAction::Quit,
                _ => WizardAction::None,
            },
            WizardScreen::Verifying => WizardAction::None,
            WizardScreen::Select => match code {
                KeyCode::Char('q') | KeyCode::Esc => WizardAction::Quit,
                KeyCode::Char('s') => WizardAction::Skip,
                KeyCode::Up | KeyCode::Char('k') => {
                    if !self.engines.is_empty() {
                        self.focus = self.focus.saturating_sub(1);
                    }
                    WizardAction::None
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    if !self.engines.is_empty() {
                        self.focus = (self.focus + 1).min(self.engines.len() - 1);
                    }
                    WizardAction::None
                }
                KeyCode::Enter => {
                    if self.engines.is_empty() {
                        self.error = Some(
                            "no review engine on PATH — install ocr (brew/cargo) or a binary named review"
                                .into(),
                        );
                        return WizardAction::None;
                    }
                    self.error = None;
                    self.screen = WizardScreen::Verifying;
                    WizardAction::Apply
                }
                _ => WizardAction::None,
            },
        }
    }

    /// Apply focused engine via hook or real `setup_yes`.
    pub fn apply(&mut self, home: &Path) -> WizardAction {
        let Some(kind) = self.focused_engine() else {
            self.screen = WizardScreen::Select;
            self.error = Some("no engine selected".into());
            return WizardAction::None;
        };
        let result = self.run_apply(home, kind);
        if result.ok {
            self.screen = WizardScreen::Success;
            self.success_engine.clone_from(&result.engine);
            self.success_wrapper = result.wrapper.map(PathBuf::from);
            self.error = None;
        } else {
            self.screen = WizardScreen::Select;
            self.error = result.error.or_else(|| Some("setup failed".into()));
        }
        WizardAction::None
    }

    #[allow(clippy::unused_self)] // `self` used only when `cfg(test)` apply_hook is set
    fn run_apply(&mut self, home: &Path, kind: EngineKind) -> SetupResult {
        #[cfg(test)]
        if let Some(hook) = self.apply_hook.as_mut() {
            return match hook(kind) {
                Ok(r) => r,
                Err(e) => SetupResult {
                    ok: false,
                    engine: Some(kind.as_str().into()),
                    wrapper: None,
                    verified: false,
                    warnings: Vec::new(),
                    error: Some(e),
                },
            };
        }
        match setup_yes(home, Some(kind)) {
            Ok(r) => r,
            Err(e) => SetupResult {
                ok: false,
                engine: Some(kind.as_str().into()),
                wrapper: None,
                verified: false,
                warnings: Vec::new(),
                error: Some(e.to_string()),
            },
        }
    }

    /// Draw the current screen.
    pub fn render(&self, frame: &mut Frame<'_>) {
        let area = frame.area();
        let block = Block::default()
            .title(" porch setup ")
            .borders(Borders::ALL);
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let text = match self.screen {
            WizardScreen::Select => self.select_lines(),
            WizardScreen::Verifying => vec![
                Line::from("verifying…"),
                Line::from("writing wrapper + config, then checking --help / preview"),
            ],
            WizardScreen::Success => self.success_lines(),
        };
        let para = Paragraph::new(text);
        frame.render_widget(para, inner);
    }

    fn select_lines(&self) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        lines.push(Line::from("Review engine"));
        if self.engines.is_empty() {
            lines.push(Line::from(Span::styled(
                "  (none detected)",
                Style::default().add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(
                "  install ocr, or put a binary named `review` on PATH",
            ));
        } else {
            for (i, eng) in self.engines.iter().enumerate() {
                let marker = if i == self.focus { "●" } else { "○" };
                let rec = if eng.kind == EngineKind::Ocr {
                    "  ← recommended"
                } else {
                    ""
                };
                let label = match eng.kind {
                    EngineKind::Ocr => format!("  {marker} ocr   ({}){rec}", eng.bin.display()),
                    EngineKind::Generic => {
                        format!("  {marker} generic (binary already named review){rec}")
                    }
                };
                let style = if i == self.focus {
                    Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED)
                } else {
                    Style::default()
                };
                lines.push(Line::from(Span::styled(label, style)));
            }
        }
        lines.push(Line::from(""));
        lines.push(Line::from(format!(
            "gh              {}",
            self.gh
                .as_ref()
                .map_or_else(|| "(missing)".into(), |p| p.display().to_string())
        )));
        lines.push(Line::from(format!(
            "fixer           {}",
            self.fixer
                .as_ref()
                .map_or_else(|| "(none — optional)".into(), |p| p.display().to_string())
        )));
        lines.push(Line::from(self.tools_line.clone()));
        lines.push(Line::from(""));
        if let Some(err) = &self.error {
            lines.push(Line::from(Span::styled(
                format!("error: {err}"),
                Style::default().add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from("Enter  retry"));
        } else {
            lines.push(Line::from("Enter  write wrapper + config and verify"));
        }
        lines.push(Line::from("↑↓     change engine"));
        lines.push(Line::from("s      skip (leave nothing written)"));
        lines.push(Line::from("q      quit"));
        lines
    }

    fn success_lines(&self) -> Vec<Line<'static>> {
        let wrap = self
            .success_wrapper
            .as_ref()
            .map_or_else(|| "(unknown)".into(), |p| p.display().to_string());
        let eng = self
            .success_engine
            .clone()
            .unwrap_or_else(|| "review".into());
        vec![
            Line::from("setup ok"),
            Line::from(format!("engine    {eng}")),
            Line::from(format!("wrapper   {wrap}")),
            Line::from("doctor    review=ok (resolved wrapper)"),
            Line::from("next      porch init   or   git push porch"),
            Line::from(""),
            Line::from("Enter / q  dismiss"),
        ]
    }
}

fn default_focus(engines: &[DetectedEngine]) -> usize {
    if engines.is_empty() {
        return 0;
    }
    if let Some(kind) = default_engine(engines) {
        if let Some(i) = engines.iter().position(|e| e.kind == kind) {
            return i;
        }
    }
    0
}

fn format_tools_line(tools: &porch_review::ToolsConfig) -> String {
    let pairs = [
        ("biome", tools.biome.as_ref()),
        ("bun", tools.bun.as_ref()),
        ("cargo", tools.cargo.as_ref()),
        ("just", tools.just.as_ref()),
        ("moon", tools.moon.as_ref()),
    ];
    let mut detected = Vec::new();
    let mut missing = Vec::new();
    for (name, val) in pairs {
        if val.is_some() {
            detected.push(name);
        } else {
            missing.push(name);
        }
    }
    let det = if detected.is_empty() {
        "none detected".into()
    } else {
        format!("detected: {}", detected.join(" "))
    };
    let miss = if missing.is_empty() {
        "all present".into()
    } else {
        format!("missing: {}", missing.join(" "))
    };
    format!("biome bun cargo just moon   {det} / {miss}")
}

fn run_wizard(home: &Path) -> Result<ExitCode> {
    let mut wizard = SetupWizard::detect();
    if wizard.engines.is_empty() {
        wizard.error = Some(
            "no review engine on PATH — install ocr OR ensure `review` is on PATH, then re-open"
                .into(),
        );
    }
    let mut term = WizardTerminal::enter()?;
    loop {
        term.terminal.draw(|frame| {
            // Keep layout simple: full-frame paragraph.
            let _ = Layout::default()
                .constraints([Constraint::Percentage(100)])
                .split(frame.area());
            wizard.render(frame);
        })?;
        if event::poll(std::time::Duration::from_millis(200))? {
            if let CEvent::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                let action = wizard.handle_key(key.code);
                match action {
                    WizardAction::Quit | WizardAction::Skip => break,
                    WizardAction::Apply => {
                        term.terminal.draw(|frame| wizard.render(frame))?;
                        let _ = wizard.apply(home);
                    }
                    WizardAction::None => {}
                }
            }
        }
    }
    drop(term);
    let _ = writeln!(io::stderr(), "porch setup: done");
    Ok(ExitCode::SUCCESS)
}

struct WizardTerminal {
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
}

impl WizardTerminal {
    fn enter() -> Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend)?;
        Ok(Self { terminal })
    }
}

impl Drop for WizardTerminal {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
        let _ = self.terminal.show_cursor();
    }
}

/// True when operator has not completed review setup (and env override absent).
#[must_use]
pub fn setup_incomplete(home: &Path) -> bool {
    !review_setup_ok(home)
}

/// Draw wizard on a [`ratatui::backend::TestBackend`] and return buffer text.
#[cfg(test)]
pub fn render_to_string(wizard: &SetupWizard, width: u16, height: u16) -> String {
    use ratatui::backend::TestBackend;
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| wizard.render(frame)).unwrap();
    let buffer = terminal.backend().buffer().clone();
    let mut out = String::new();
    for y in 0..height {
        for x in 0..width {
            out.push_str(buffer[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use porch_review::SetupResult;

    fn fake_ocr_engine() -> DetectedEngine {
        DetectedEngine {
            kind: EngineKind::Ocr,
            bin: PathBuf::from("/opt/homebrew/bin/ocr"),
        }
    }

    #[test]
    fn ocr_is_focused_by_default_when_only_engine() {
        let mut w = SetupWizard {
            screen: WizardScreen::Select,
            engines: vec![fake_ocr_engine()],
            focus: 0,
            gh: None,
            fixer: None,
            tools_line: String::new(),
            error: None,
            success_wrapper: None,
            success_engine: None,
            apply_hook: None,
        };
        assert_eq!(w.focused_engine(), Some(EngineKind::Ocr));
        let s = render_to_string(&w, 80, 24);
        assert!(s.contains("ocr"), "buffer={s}");
        assert!(s.contains("recommended") || s.contains('●'), "buffer={s}");
        let _ = &mut w;
    }

    #[test]
    fn enter_triggers_apply_hook_and_success_shows_wrapper() {
        let mut w = SetupWizard {
            screen: WizardScreen::Select,
            engines: vec![fake_ocr_engine()],
            focus: 0,
            gh: Some(PathBuf::from("/opt/homebrew/bin/gh")),
            fixer: None,
            tools_line: "tools".into(),
            error: None,
            success_wrapper: None,
            success_engine: None,
            apply_hook: None,
        }
        .with_apply_hook(|_| {
            Ok(SetupResult {
                ok: true,
                engine: Some("ocr".into()),
                wrapper: Some("/tmp/home/bin/review".into()),
                verified: true,
                warnings: Vec::new(),
                error: None,
            })
        });
        assert_eq!(w.handle_key(KeyCode::Enter), WizardAction::Apply);
        assert_eq!(w.screen, WizardScreen::Verifying);
        let _ = w.apply(Path::new("/tmp"));
        assert_eq!(w.screen, WizardScreen::Success);
        let s = render_to_string(&w, 80, 24);
        assert!(s.contains("/tmp/home/bin/review"), "buffer={s}");
    }

    #[test]
    fn skip_does_not_write() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        let wrote = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&wrote);
        let mut w = SetupWizard {
            screen: WizardScreen::Select,
            engines: vec![fake_ocr_engine()],
            focus: 0,
            gh: None,
            fixer: None,
            tools_line: String::new(),
            error: None,
            success_wrapper: None,
            success_engine: None,
            apply_hook: None,
        }
        .with_apply_hook(move |_| {
            flag.store(true, Ordering::SeqCst);
            Err("should not run".into())
        });
        assert_eq!(w.handle_key(KeyCode::Char('s')), WizardAction::Skip);
        assert!(!wrote.load(Ordering::SeqCst));
    }

    #[test]
    fn apply_error_stays_on_select_inline() {
        let mut w = SetupWizard {
            screen: WizardScreen::Select,
            engines: vec![fake_ocr_engine()],
            focus: 0,
            gh: None,
            fixer: None,
            tools_line: String::new(),
            error: None,
            success_wrapper: None,
            success_engine: None,
            apply_hook: None,
        }
        .with_apply_hook(|_| Err("tampered wrapper".into()));
        assert_eq!(w.handle_key(KeyCode::Enter), WizardAction::Apply);
        let _ = w.apply(Path::new("/tmp"));
        assert_eq!(w.screen, WizardScreen::Select);
        assert!(w.error.as_deref().is_some_and(|e| e.contains("tampered")));
        let s = render_to_string(&w, 80, 24);
        assert!(s.contains("tampered"), "buffer={s}");
    }
}
