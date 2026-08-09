use std::collections::VecDeque;
use std::fs;
use std::io::{self, IsTerminal, Stdout, Write};
use std::path::Path;
use std::sync::mpsc::TryRecvError;
use std::time::Duration;

use crossterm::cursor::{Hide, Show};
use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    Clear as ClearTerminal, ClearType, EnterAlternateScreen, LeaveAlternateScreen,
    disable_raw_mode, enable_raw_mode,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph, Row, Table, Wrap,
};
use ratatui::{Frame, Terminal};

use crate::app::{TabLaunchSettings, WipsawApp};
use crate::doctor::DoctorReport;
use crate::error::{Result, WipsawError};
use crate::manager::{
    MANAGER_MODEL, MANAGER_SKILL_NAMES, MANAGER_TRANSCRIPT_MESSAGE_LIMIT, ManagerContextScope,
    ManagerEvent, ManagerProgress, ManagerProgressStatus, list_manager_directory,
    sensitive_reference,
};
use crate::model::{
    Account, CodexHome, CodexThread, ManagerMessage, ManagerSession, ModelProfile, Tab, Workspace,
};
use crate::tmux::TmuxWindow;

const CYAN: Color = Color::Rgb(56, 220, 232);
const AMBER: Color = Color::Rgb(251, 191, 36);
const GREEN: Color = Color::Rgb(74, 222, 128);
const RED: Color = Color::Rgb(248, 113, 113);
const INK: Color = Color::Rgb(238, 238, 238);
const MUTED: Color = Color::Rgb(128, 128, 128);
const STEEL: Color = Color::Rgb(184, 184, 184);
const PANEL: Color = Color::Rgb(20, 20, 20);
const DEEP: Color = Color::Rgb(10, 10, 10);
const BORDER: Color = Color::Rgb(72, 72, 72);
const WIPSAW_MARK: [&str; 9] = [
    "████               ████",
    " ████             ████ ",
    "  ████     █     ████  ",
    "   ████   ███   ████   ",
    "    ████ █████ ████    ",
    "     █████████████     ",
    "      ███▀▄▀▄▀███      ",
    "       ██ ▀ ▀ ██       ",
    "        ▀     ▀        ",
];

pub fn run(app: &mut WipsawApp) -> Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(WipsawError::InvalidInput {
            field: "terminal",
            message: "the navigator needs an interactive terminal; use a subcommand for scripts"
                .to_string(),
        });
    }

    let _popup_guard = NavigatorPopupGuard::new(app);
    let mut session = TerminalSession::enter()?;
    let mut navigator = Navigator::load(app)?;

    loop {
        navigator.poll_manager(app);
        // Copy mode performs its own text selection so it works consistently
        // inside tmux, popups, and alternate-screen terminals.
        session.set_mouse_capture(true)?;
        session.terminal.draw(|frame| navigator.render(frame))?;

        let input = if navigator.manager.turn.is_some() && !navigator.manager.copy_view {
            if !event::poll(Duration::from_millis(250))? {
                continue;
            }
            event::read()?
        } else {
            // Avoid repainting an idle dashboard. Copy mode keeps its own exact
            // selection offsets, so redraws are needed only after user input.
            event::read()?
        };
        let old_workspace = navigator.selected_workspace().map(|item| item.id.clone());
        let action = match input {
            Event::Key(key) if key.kind == KeyEventKind::Press => navigator.handle_key(key),
            Event::Paste(value) => {
                navigator.handle_paste(&value);
                Action::None
            }
            Event::Mouse(mouse) => navigator.handle_mouse(mouse),
            _ => continue,
        };
        let new_workspace = navigator.selected_workspace().map(|item| item.id.clone());
        if old_workspace != new_workspace
            && let Err(error) = navigator.refresh_tabs(app, None)
        {
            navigator.error(error);
        }

        match action {
            Action::None => {}
            Action::Quit => break,
            Action::Refresh => match navigator.refresh(app) {
                Ok(()) => navigator.notice("table of contents refreshed"),
                Err(error) => navigator.error(error),
            },
            Action::CreateWorkspace { name } => {
                let result = std::env::current_dir()
                    .map_err(WipsawError::from)
                    .and_then(|cwd| app.create_workspace(&name, &cwd));
                match result {
                    Ok(workspace) => {
                        navigator.notice(format!("created workspace '{}'", workspace.name));
                        if let Err(error) = navigator.refresh_selecting(app, Some(&workspace.id)) {
                            navigator.error(error);
                        }
                        if let Err(error) = navigator.open_middle_manager(app, &workspace.id) {
                            navigator.error(error);
                        }
                    }
                    Err(error) => navigator.error(error),
                }
            }
            Action::CreateTab {
                workspace_id,
                name,
                start_codex,
            } => {
                let result =
                    app.create_tab(&workspace_id, &name, None, TabLaunchSettings::default());
                match result {
                    Ok(tab) if start_codex => {
                        session.suspend()?;
                        let result = app
                            .start_tab_codex(&workspace_id, &tab.id)
                            .and_then(|_| app.activate_tab(&workspace_id, &tab.id));
                        session.resume()?;
                        match result {
                            Ok(true) => break,
                            Ok(false) => {
                                navigator.notice(format!("detached from '{}'", tab.name));
                                if let Err(error) = navigator.refresh(app) {
                                    navigator.error(error);
                                }
                            }
                            Err(error) => navigator.error(error),
                        }
                    }
                    Ok(tab) => {
                        navigator.notice(format!("created tab '{}'", tab.name));
                        if let Err(error) = navigator.refresh_tabs(app, Some(&tab.id)) {
                            navigator.error(error);
                        }
                    }
                    Err(error) => navigator.error(error),
                }
            }
            Action::RenameTab {
                workspace_id,
                tab_id,
                name,
            } => match app.rename_tab(&workspace_id, &tab_id, &name) {
                Ok(tab) => {
                    navigator.notice(format!("renamed tab to '{}'", tab.name));
                    if let Err(error) = navigator.refresh_tabs(app, Some(&tab.id)) {
                        navigator.error(error);
                    }
                }
                Err(error) => navigator.error(error),
            },
            Action::ActivateWorkspace(workspace_id) => {
                session.suspend()?;
                let result = app.activate_workspace(&workspace_id);
                session.resume()?;
                match result {
                    Ok(true) => break,
                    Ok(false) => {
                        navigator.notice("detached; tmux kept the workspace running");
                        if let Err(error) = navigator.refresh(app) {
                            navigator.error(error);
                        }
                    }
                    Err(error) => navigator.error(error),
                }
            }
            Action::ActivateTab {
                workspace_id,
                tab_id,
            } => {
                session.suspend()?;
                let result = app.activate_tab(&workspace_id, &tab_id);
                session.resume()?;
                match result {
                    Ok(true) => break,
                    Ok(false) => {
                        navigator.notice("detached; the tab is still running");
                        if let Err(error) = navigator.refresh(app) {
                            navigator.error(error);
                        }
                    }
                    Err(error) => navigator.error(error),
                }
            }
            Action::OpenThread {
                thread_id,
                workspace_id,
            } => {
                session.suspend()?;
                let result = app.open_codex_thread(&thread_id, workspace_id.as_deref());
                session.resume()?;
                match result {
                    Ok(true) => break,
                    Ok(false) => {
                        navigator.notice("detached; the Codex thread is still running");
                        if let Err(error) = navigator.refresh(app) {
                            navigator.error(error);
                        }
                    }
                    Err(error) => navigator.error(error),
                }
            }
            Action::OpenLumbergh => {
                if let Err(error) = navigator.open_lumbergh(app) {
                    navigator.error(error);
                }
            }
            Action::OpenMiddleManager { workspace_id } => {
                if let Err(error) = navigator.open_middle_manager(app, &workspace_id) {
                    navigator.error(error);
                }
            }
            Action::CloseManager => {
                if let Err(error) = navigator.close_middle_manager(app) {
                    navigator.error(error);
                }
            }
            Action::SendManagerMessage {
                workspace_id,
                prompt,
            } => match app.start_manager_turn(workspace_id.as_deref(), &prompt) {
                Ok(handle) => navigator.manager_turn_started(app, handle),
                Err(error) => navigator.error(error),
            },
            Action::FocusManager => {
                navigator.manager.focused = true;
                navigator.message = None;
            }
            Action::CopyManager { text, description } => {
                let tmux_copy = app.tmux.copy_to_clipboard(&text);
                let terminal_copy = session.copy_to_clipboard(&text);
                if tmux_copy.is_ok() || terminal_copy.is_ok() {
                    navigator.notice(format!("copied {description} to the clipboard"));
                } else if let Err(error) = tmux_copy {
                    navigator.error(error);
                }
            }
        }
    }

    Ok(())
}

struct NavigatorPopupGuard {
    tmux: Option<crate::tmux::TmuxBackend>,
    session: Option<String>,
}

impl NavigatorPopupGuard {
    fn new(app: &WipsawApp) -> Self {
        Self {
            tmux: std::env::var_os("WIPSAW_PARENT_SESSION").map(|_| app.tmux.clone()),
            session: std::env::var("WIPSAW_PARENT_SESSION").ok(),
        }
    }
}

impl Drop for NavigatorPopupGuard {
    fn drop(&mut self) {
        if let (Some(tmux), Some(session)) = (&self.tmux, &self.session) {
            let _ = tmux.clear_navigator_guard(session);
        }
    }
}

struct TerminalSession {
    terminal: Terminal<CrosstermBackend<Stdout>>,
    active: bool,
    mouse_capture: bool,
}

impl TerminalSession {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(error) = execute!(
            stdout,
            EnterAlternateScreen,
            EnableBracketedPaste,
            EnableMouseCapture,
            Hide,
            ClearTerminal(ClearType::All)
        ) {
            let _ = disable_raw_mode();
            return Err(error);
        }
        let terminal = Terminal::new(CrosstermBackend::new(stdout))?;
        Ok(Self {
            terminal,
            active: true,
            mouse_capture: true,
        })
    }

    fn set_mouse_capture(&mut self, enabled: bool) -> io::Result<()> {
        if !self.active || self.mouse_capture == enabled {
            return Ok(());
        }
        if enabled {
            execute!(self.terminal.backend_mut(), EnableMouseCapture)?;
        } else {
            execute!(self.terminal.backend_mut(), DisableMouseCapture)?;
        }
        self.mouse_capture = enabled;
        Ok(())
    }

    fn suspend(&mut self) -> io::Result<()> {
        if self.active {
            disable_raw_mode()?;
            execute!(
                self.terminal.backend_mut(),
                DisableBracketedPaste,
                DisableMouseCapture,
                Show,
                LeaveAlternateScreen
            )?;
            self.active = false;
            self.mouse_capture = false;
        }
        Ok(())
    }

    fn resume(&mut self) -> io::Result<()> {
        if !self.active {
            enable_raw_mode()?;
            if let Err(error) = execute!(
                self.terminal.backend_mut(),
                EnterAlternateScreen,
                EnableBracketedPaste,
                EnableMouseCapture,
                Hide,
                ClearTerminal(ClearType::All)
            ) {
                let _ = disable_raw_mode();
                return Err(error);
            }
            self.active = true;
            self.mouse_capture = true;
            self.terminal.clear()?;
        }
        Ok(())
    }

    fn copy_to_clipboard(&mut self, text: &str) -> io::Result<()> {
        let payload = base64_encode(text.as_bytes());
        self.terminal
            .backend_mut()
            .write_all(format!("\x1b]52;c;{payload}\x07").as_bytes())?;
        self.terminal.backend_mut().flush()
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        if self.active {
            let _ = disable_raw_mode();
            let _ = execute!(
                self.terminal.backend_mut(),
                DisableBracketedPaste,
                DisableMouseCapture,
                Show,
                LeaveAlternateScreen
            );
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Panel {
    Workspaces,
    Tabs,
    Threads,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum View {
    Home,
    Sessions,
    Threads,
    Wips,
}

impl View {
    const fn next(self) -> Self {
        match self {
            Self::Home => Self::Sessions,
            Self::Sessions => Self::Threads,
            Self::Threads => Self::Wips,
            Self::Wips => Self::Home,
        }
    }

    const fn previous(self) -> Self {
        match self {
            Self::Home => Self::Wips,
            Self::Sessions => Self::Home,
            Self::Threads => Self::Sessions,
            Self::Wips => Self::Threads,
        }
    }
}

#[derive(Default)]
struct SystemHealth {
    codex: bool,
    tmux: bool,
    docker: bool,
}

impl SystemHealth {
    fn collect(app: &WipsawApp) -> Self {
        let report = DoctorReport::collect(app);
        let ok = |name: &str| {
            report
                .checks
                .iter()
                .find(|check| check.name == name)
                .is_some_and(|check| check.ok)
        };
        Self {
            codex: ok("codex"),
            tmux: ok("tmux"),
            docker: ok("docker"),
        }
    }
}

#[derive(Default)]
struct ManagerChat {
    session: Option<ManagerSession>,
    messages: Vec<ManagerMessage>,
    composer: String,
    composer_cursor: usize,
    composer_area: Option<Rect>,
    composer_view_start: usize,
    composer_width: usize,
    transcript_area: Option<Rect>,
    focused: bool,
    overlay: bool,
    activity: Option<String>,
    turn: Option<ActiveManagerTurn>,
    scroll: u16,
    references: Vec<ManagerReference>,
    scope: Option<ManagerContextScope>,
    reference_index: usize,
    reference_dismissed: bool,
    progress: Vec<ManagerProgress>,
    copy_view: bool,
    copy_cursor: usize,
    copy_anchor: Option<usize>,
    copy_dragging: bool,
    copy_drag_origin: usize,
    copy_drag_origin_end: usize,
    copy_view_top: usize,
    copy_view_height: usize,
    copy_view_width: usize,
}

#[derive(Debug, Clone, Copy)]
struct ManagerCopyLine {
    start: usize,
    end: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ManagerReferenceKind {
    File,
    Skill,
}

#[derive(Debug, Clone)]
struct ManagerReference {
    kind: ManagerReferenceKind,
    label: String,
    hint: Option<String>,
    directory: bool,
}

struct ActiveManagerTurn {
    session_id: String,
    receiver: std::sync::mpsc::Receiver<ManagerEvent>,
}

struct Navigator {
    workspaces: Vec<(Workspace, bool)>,
    tabs: Vec<Tab>,
    windows: Vec<TmuxWindow>,
    threads: Vec<CodexThread>,
    accounts: Vec<Account>,
    homes: Vec<CodexHome>,
    profiles: Vec<ModelProfile>,
    managers: Vec<ManagerSession>,
    total_tabs: usize,
    workspace_state: ListState,
    tab_state: ListState,
    thread_state: ListState,
    view: View,
    active: Panel,
    health: SystemHealth,
    is_popup: bool,
    prefix_pending: bool,
    show_help: bool,
    prompt: Option<Prompt>,
    message: Option<(String, bool)>,
    manager: ManagerChat,
}

impl Navigator {
    fn load(app: &WipsawApp) -> Result<Self> {
        let mut navigator = Self::empty();
        navigator.is_popup = std::env::var_os("WIPSAW_PARENT_SESSION").is_some();
        navigator.refresh(app)?;
        if let Err(error) = navigator.load_manager(app, None) {
            navigator.error(error);
        }
        if let Some((workspace, tab)) = app.current_managed_context()?
            && let Some(index) = navigator
                .workspaces
                .iter()
                .position(|(item, _)| item.id == workspace.id)
        {
            navigator.workspace_state.select(Some(index));
            navigator.refresh_tabs(app, Some(&tab.id))?;
        }
        match std::env::var("WIPSAW_TUI_START").as_deref() {
            Ok("new-tab") => {
                navigator.view = View::Sessions;
                navigator.active = Panel::Tabs;
                navigator.begin_create();
            }
            Ok("rename-tab") => {
                navigator.view = View::Sessions;
                navigator.active = Panel::Tabs;
                navigator.begin_rename();
            }
            Ok("middle-manager") => {
                let requested = std::env::var("WIPSAW_MANAGER_WORKSPACE").ok();
                let workspace_id = requested
                    .as_deref()
                    .and_then(|value| {
                        navigator
                            .workspaces
                            .iter()
                            .find(|(workspace, _)| {
                                workspace.id == value
                                    || workspace.name.eq_ignore_ascii_case(value)
                                    || workspace.tmux_session == value
                            })
                            .map(|(workspace, _)| workspace.id.clone())
                    })
                    .or_else(|| {
                        navigator
                            .selected_workspace()
                            .map(|workspace| workspace.id.clone())
                    });
                if let Some(workspace_id) = workspace_id {
                    if let Err(error) = navigator.open_middle_manager(app, &workspace_id) {
                        navigator.error(error);
                    }
                } else {
                    navigator.error("create or select a workspace before opening a Middle Manager");
                }
            }
            Ok("lumbergh") => {
                navigator.view = View::Home;
                navigator.manager.focused = true;
            }
            _ => {}
        }
        Ok(navigator)
    }

    fn empty() -> Self {
        Self {
            workspaces: Vec::new(),
            tabs: Vec::new(),
            windows: Vec::new(),
            threads: Vec::new(),
            accounts: Vec::new(),
            homes: Vec::new(),
            profiles: Vec::new(),
            managers: Vec::new(),
            total_tabs: 0,
            workspace_state: ListState::default(),
            tab_state: ListState::default(),
            thread_state: ListState::default(),
            view: View::Home,
            active: Panel::Workspaces,
            health: SystemHealth::default(),
            is_popup: false,
            prefix_pending: false,
            show_help: false,
            prompt: None,
            message: None,
            manager: ManagerChat::default(),
        }
    }

    fn load_manager(&mut self, app: &WipsawApp, workspace_ref: Option<&str>) -> Result<()> {
        let (session, messages) = app.manager_messages(workspace_ref)?;
        let scope = app.manager_context_scope(&session)?;
        let references = manager_reference_catalog(&scope);
        if let Some(existing) = self
            .managers
            .iter_mut()
            .find(|manager| manager.id == session.id)
        {
            *existing = session.clone();
        } else {
            self.managers.push(session.clone());
        }
        self.manager.session = Some(session);
        self.manager.messages = messages;
        self.manager.references = references;
        self.manager.scope = Some(scope);
        self.manager.reference_index = 0;
        self.manager.reference_dismissed = false;
        self.manager.scroll = 0;
        self.manager.activity = None;
        self.manager.progress.clear();
        self.manager.copy_view = false;
        Ok(())
    }

    fn open_lumbergh(&mut self, app: &WipsawApp) -> Result<()> {
        if self.manager.turn.is_some()
            && self
                .manager
                .session
                .as_ref()
                .is_some_and(|session| session.workspace_id.is_some())
        {
            self.notice("the Middle Manager is still working; wait for this turn to finish");
            return Ok(());
        }
        self.load_manager(app, None)?;
        self.manager.overlay = false;
        self.manager.focused = true;
        self.view = View::Home;
        self.message = None;
        Ok(())
    }

    fn open_middle_manager(&mut self, app: &WipsawApp, workspace_ref: &str) -> Result<()> {
        if self.manager.turn.is_some()
            && self
                .manager
                .session
                .as_ref()
                .is_some_and(|session| session.workspace_id.as_deref() != Some(workspace_ref))
        {
            self.notice("another manager is still working; wait for this turn to finish");
            return Ok(());
        }
        self.load_manager(app, Some(workspace_ref))?;
        self.manager.overlay = true;
        self.manager.focused = true;
        self.manager.composer.clear();
        self.manager.composer_cursor = 0;
        self.message = None;
        Ok(())
    }

    fn close_middle_manager(&mut self, app: &WipsawApp) -> Result<()> {
        if self.manager.turn.is_some() {
            self.notice("the Middle Manager is still working; wait for this turn to finish");
            return Ok(());
        }
        self.manager.overlay = false;
        self.manager.focused = false;
        self.manager.composer.clear();
        self.manager.composer_cursor = 0;
        self.load_manager(app, None)?;
        Ok(())
    }

    fn manager_turn_started(&mut self, app: &WipsawApp, handle: crate::app::ManagerTurnHandle) {
        let session_id = handle.session.id.clone();
        self.manager.session = Some(handle.session);
        self.manager.turn = Some(ActiveManagerTurn {
            session_id,
            receiver: handle.receiver,
        });
        self.manager.activity = Some("working".to_string());
        self.manager.progress.clear();
        self.upsert_manager_progress(ManagerProgress {
            id: "turn".to_string(),
            label: "Thinking with Terra · medium".to_string(),
            detail: None,
            status: ManagerProgressStatus::Running,
        });
        self.manager.scroll = 0;
        if let Some(session) = &self.manager.session {
            match app
                .registry
                .list_manager_messages(&session.id, MANAGER_TRANSCRIPT_MESSAGE_LIMIT)
            {
                Ok(messages) => self.manager.messages = messages,
                Err(error) => self.error(error),
            }
        }
    }

    fn poll_manager(&mut self, app: &WipsawApp) {
        let mut events = Vec::new();
        let mut disconnected = false;
        let mut refresh_inventory = false;
        if let Some(turn) = self.manager.turn.as_ref() {
            loop {
                match turn.receiver.try_recv() {
                    Ok(event) => events.push(event),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
        }
        if disconnected
            && !events
                .iter()
                .any(|event| matches!(event, ManagerEvent::Finished(_)))
        {
            events.push(ManagerEvent::Finished(Err(
                "manager worker stopped before reporting a result".to_string(),
            )));
        }
        for event in events {
            match event {
                ManagerEvent::Started => {
                    self.manager.activity = Some("working".to_string());
                    self.upsert_manager_progress(ManagerProgress {
                        id: "turn".to_string(),
                        label: "Thinking with Terra · medium".to_string(),
                        detail: None,
                        status: ManagerProgressStatus::Running,
                    });
                }
                ManagerEvent::Progress(progress) => {
                    refresh_inventory |= manager_progress_changes_inventory(&progress);
                    self.upsert_manager_progress(progress);
                }
                ManagerEvent::Finished(result) => {
                    refresh_inventory = true;
                    let session_id = self
                        .manager
                        .turn
                        .as_ref()
                        .map(|turn| turn.session_id.clone())
                        .unwrap_or_default();
                    self.manager.turn = None;
                    match result {
                        Ok(result) => {
                            if let Err(error) = app.complete_manager_turn(&result) {
                                self.manager.progress.retain(|item| item.id != "turn");
                                self.upsert_manager_progress(ManagerProgress {
                                    id: "turn".to_string(),
                                    label: "Turn failed".to_string(),
                                    detail: Some(error.to_string()),
                                    status: ManagerProgressStatus::Failed,
                                });
                                self.manager.activity = Some("turn failed".to_string());
                                self.error(error);
                            } else {
                                let usage = result.usage.as_ref().map(|usage| {
                                    format!(
                                        "{} input · {} cached · {} output tokens",
                                        usage.input_tokens,
                                        usage.cached_input_tokens,
                                        usage.output_tokens
                                    )
                                });
                                self.manager.progress.retain(|item| item.id != "turn");
                                self.upsert_manager_progress(ManagerProgress {
                                    id: "turn".to_string(),
                                    label: "Turn complete".to_string(),
                                    detail: usage,
                                    status: ManagerProgressStatus::Completed,
                                });
                                self.manager.activity = Some("complete".to_string());
                                self.notice("manager turn complete");
                            }
                        }
                        Err(error) => {
                            if let Err(registry_error) = app.fail_manager_turn(&session_id, &error)
                            {
                                self.manager.progress.retain(|item| item.id != "turn");
                                self.upsert_manager_progress(ManagerProgress {
                                    id: "turn".to_string(),
                                    label: "Turn failed".to_string(),
                                    detail: Some(error.clone()),
                                    status: ManagerProgressStatus::Failed,
                                });
                                self.manager.activity = Some("turn failed".to_string());
                                self.error(registry_error);
                            } else {
                                self.manager.progress.retain(|item| item.id != "turn");
                                self.upsert_manager_progress(ManagerProgress {
                                    id: "turn".to_string(),
                                    label: "Turn failed".to_string(),
                                    detail: Some(error.clone()),
                                    status: ManagerProgressStatus::Failed,
                                });
                                self.manager.activity = Some("turn failed".to_string());
                                self.error(error);
                            }
                        }
                    }
                    if let Ok(Some(session)) = app.registry.manager_session_by_id(&session_id) {
                        if let Some(existing) = self
                            .managers
                            .iter_mut()
                            .find(|manager| manager.id == session.id)
                        {
                            *existing = session.clone();
                        }
                        if let Ok(scope) = app.manager_context_scope(&session) {
                            self.manager.references = manager_reference_catalog(&scope);
                            self.manager.scope = Some(scope);
                        }
                        self.manager.session = Some(session);
                    }
                    if let Ok(messages) = app
                        .registry
                        .list_manager_messages(&session_id, MANAGER_TRANSCRIPT_MESSAGE_LIMIT)
                    {
                        self.manager.messages = messages;
                    }
                }
            }
        }
        if refresh_inventory && let Err(error) = self.refresh_inventory(app) {
            self.error(error);
        }
    }

    fn upsert_manager_progress(&mut self, progress: ManagerProgress) {
        if let Some(existing) = self
            .manager
            .progress
            .iter_mut()
            .find(|existing| existing.id == progress.id)
        {
            *existing = progress;
        } else {
            self.manager.progress.push(progress);
            if self.manager.progress.len() > 64 {
                let removable = self
                    .manager
                    .progress
                    .iter()
                    .position(|progress| progress.id != "turn")
                    .unwrap_or(0);
                self.manager.progress.remove(removable);
            }
        }
    }

    fn refresh(&mut self, app: &WipsawApp) -> Result<()> {
        self.refresh_inventory(app)?;
        self.health = SystemHealth::collect(app);
        Ok(())
    }

    fn refresh_inventory(&mut self, app: &WipsawApp) -> Result<()> {
        let workspace_id = self.selected_workspace().map(|item| item.id.clone());
        let tab_id = self.selected_tab().map(|item| item.id.clone());
        let thread_id = self.selected_thread().map(|item| item.id.clone());
        self.workspaces = app.list_workspaces()?;
        self.total_tabs = self
            .workspaces
            .iter()
            .map(|(workspace, _)| app.registry.list_tabs(&workspace.id).map(|tabs| tabs.len()))
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .sum();
        select_id(
            &mut self.workspace_state,
            self.workspaces
                .iter()
                .position(|(workspace, _)| Some(&workspace.id) == workspace_id.as_ref()),
            self.workspaces.len(),
        );
        self.refresh_tabs(app, tab_id.as_deref())?;
        self.threads = app.list_codex_threads(None)?;
        self.accounts = app.registry.list_accounts()?;
        self.homes = app.registry.list_codex_homes()?;
        self.profiles = app.registry.list_model_profiles()?;
        self.managers = app.registry.list_manager_sessions()?;
        if let Some(session) = &self.manager.session {
            let scope = app.manager_context_scope(session)?;
            self.manager.references = manager_reference_catalog(&scope);
            self.manager.scope = Some(scope);
        }
        select_id(
            &mut self.thread_state,
            self.threads
                .iter()
                .position(|thread| Some(&thread.id) == thread_id.as_ref()),
            self.threads.len(),
        );
        Ok(())
    }

    fn refresh_selecting(&mut self, app: &WipsawApp, workspace_id: Option<&str>) -> Result<()> {
        self.refresh(app)?;
        select_id(
            &mut self.workspace_state,
            self.workspaces
                .iter()
                .position(|(workspace, _)| Some(workspace.id.as_str()) == workspace_id),
            self.workspaces.len(),
        );
        self.refresh_tabs(app, None)?;
        ensure_selection(&mut self.thread_state, self.threads.len());
        Ok(())
    }

    fn refresh_tabs(&mut self, app: &WipsawApp, tab_id: Option<&str>) -> Result<()> {
        let Some(workspace_id) = self.selected_workspace().map(|item| item.id.clone()) else {
            self.tabs.clear();
            self.windows.clear();
            self.tab_state.select(None);
            return Ok(());
        };
        let (_, tabs, windows) = app.list_tabs(&workspace_id)?;
        self.tabs = tabs;
        self.windows = windows;
        select_id(
            &mut self.tab_state,
            self.tabs
                .iter()
                .position(|tab| Some(tab.id.as_str()) == tab_id),
            self.tabs.len(),
        );
        Ok(())
    }

    fn selected_workspace(&self) -> Option<&Workspace> {
        self.workspace_state
            .selected()
            .and_then(|index| self.workspaces.get(index))
            .map(|(workspace, _)| workspace)
    }

    fn selected_tab(&self) -> Option<&Tab> {
        self.tab_state
            .selected()
            .and_then(|index| self.tabs.get(index))
    }

    fn selected_thread(&self) -> Option<&CodexThread> {
        self.thread_state
            .selected()
            .and_then(|index| self.threads.get(index))
    }

    fn notice(&mut self, message: impl Into<String>) {
        self.message = Some((message.into(), false));
    }

    fn error(&mut self, error: impl std::fmt::Display) {
        self.message = Some((error.to_string(), true));
    }

    fn handle_key(&mut self, key: KeyEvent) -> Action {
        if self.manager.copy_view {
            return self.handle_manager_copy_view_key(key);
        }
        if self.prompt.is_some() {
            return self.handle_prompt_key(key);
        }
        if self.show_help {
            if matches!(
                key.code,
                KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q')
            ) {
                self.show_help = false;
            }
            return Action::None;
        }
        if self.manager.focused {
            return self.handle_manager_key(key);
        }
        if self.manager.overlay {
            return match key.code {
                KeyCode::Esc | KeyCode::Char('q') => Action::CloseManager,
                KeyCode::Enter => Action::FocusManager,
                KeyCode::Char('y') => self.copy_latest_manager_message(),
                KeyCode::Char('Y') => self.copy_manager_transcript(),
                KeyCode::Char('v') => {
                    self.open_manager_copy_view();
                    Action::None
                }
                KeyCode::PageUp | KeyCode::Up | KeyCode::Char('k') => {
                    self.manager.scroll = self.manager.scroll.saturating_add(3);
                    Action::None
                }
                KeyCode::PageDown | KeyCode::Down | KeyCode::Char('j') => {
                    self.manager.scroll = self.manager.scroll.saturating_sub(3);
                    Action::None
                }
                _ => Action::None,
            };
        }
        if self.prefix_pending {
            self.prefix_pending = false;
            return self.handle_prefix_key(key.code);
        }
        if key.code == KeyCode::Char('b') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.prefix_pending = true;
            self.message = None;
            return Action::None;
        }

        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => Action::Quit,
            KeyCode::Char('?') | KeyCode::Char('/') => {
                self.show_help = true;
                Action::None
            }
            KeyCode::Tab => {
                self.set_view(self.view.next());
                Action::None
            }
            KeyCode::BackTab => {
                self.set_view(self.view.previous());
                Action::None
            }
            KeyCode::Char('1') => {
                self.set_view(View::Home);
                Action::None
            }
            KeyCode::Char('2') => {
                self.set_view(View::Sessions);
                Action::None
            }
            KeyCode::Char('3') => {
                self.set_view(View::Threads);
                Action::None
            }
            KeyCode::Char('4') | KeyCode::Char('g') => {
                self.set_view(View::Wips);
                Action::None
            }
            KeyCode::Right | KeyCode::Char('l') if self.view == View::Sessions => {
                self.active = Panel::Tabs;
                Action::None
            }
            KeyCode::Left | KeyCode::Char('h') if self.view == View::Sessions => {
                self.active = Panel::Workspaces;
                Action::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_selection(1);
                Action::None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_selection(-1);
                Action::None
            }
            KeyCode::Home => {
                self.select_edge(false);
                Action::None
            }
            KeyCode::End | KeyCode::Char('G') => {
                self.select_edge(true);
                Action::None
            }
            KeyCode::Char('r') => Action::Refresh,
            KeyCode::Char('c') => {
                self.begin_create();
                Action::None
            }
            KeyCode::Char('n') => {
                self.begin_tab(true);
                Action::None
            }
            KeyCode::Char('t') => {
                self.begin_tab(false);
                Action::None
            }
            KeyCode::Char('m') => Action::OpenLumbergh,
            KeyCode::Char('y') => self.copy_latest_manager_message(),
            KeyCode::Char('Y') => self.copy_manager_transcript(),
            KeyCode::Char('v') if self.view == View::Home => {
                self.open_manager_copy_view();
                Action::None
            }
            KeyCode::Char(',') => {
                self.begin_rename();
                Action::None
            }
            KeyCode::Enter => self.enter_action(),
            _ => Action::None,
        }
    }

    fn handle_prefix_key(&mut self, code: KeyCode) -> Action {
        match code {
            KeyCode::Char('w') if self.is_popup => return Action::Quit,
            KeyCode::Char('w') => self.set_view(View::Home),
            KeyCode::Char('s') => self.set_view(View::Sessions),
            KeyCode::Char('t') => self.set_view(View::Threads),
            KeyCode::Char('g') => self.set_view(View::Wips),
            KeyCode::Char('n') => self.set_view(self.view.next()),
            KeyCode::Char('p') => self.set_view(self.view.previous()),
            KeyCode::Char('c') => self.begin_create(),
            KeyCode::Char('m') => return Action::OpenLumbergh,
            KeyCode::Char('?') => self.show_help = true,
            KeyCode::Char('q') => return Action::Quit,
            _ => self.notice("unknown prefix key; press ? for bindings"),
        }
        Action::None
    }

    fn handle_manager_key(&mut self, key: KeyEvent) -> Action {
        let matches = self.manager_reference_matches();
        if !matches.is_empty() && !self.manager.reference_dismissed {
            match key.code {
                KeyCode::Up
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    self.manager.reference_index = self.manager.reference_index.saturating_sub(1);
                    return Action::None;
                }
                KeyCode::Down
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    self.manager.reference_index =
                        (self.manager.reference_index + 1).min(matches.len() - 1);
                    return Action::None;
                }
                KeyCode::Tab => {
                    self.insert_manager_reference();
                    return Action::None;
                }
                KeyCode::Enter if key.modifiers.is_empty() => {
                    self.insert_manager_reference();
                    return Action::None;
                }
                KeyCode::Esc => {
                    self.manager.reference_dismissed = true;
                    return Action::None;
                }
                _ => {}
            }
        }
        match key.code {
            KeyCode::Char('o') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.open_manager_copy_view();
                Action::None
            }
            KeyCode::Esc => {
                self.manager.focused = false;
                Action::None
            }
            KeyCode::Backspace => {
                self.delete_manager_character(true);
                Action::None
            }
            KeyCode::Delete => {
                self.delete_manager_character(false);
                Action::None
            }
            KeyCode::Left
                if key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.move_manager_word(false);
                Action::None
            }
            KeyCode::Right
                if key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.move_manager_word(true);
                Action::None
            }
            KeyCode::Left => {
                self.manager.composer_cursor =
                    previous_char_boundary(&self.manager.composer, self.manager.composer_cursor);
                self.manager_reference_cursor_moved();
                Action::None
            }
            KeyCode::Right => {
                self.manager.composer_cursor =
                    next_char_boundary(&self.manager.composer, self.manager.composer_cursor);
                self.manager_reference_cursor_moved();
                Action::None
            }
            KeyCode::Up
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.manager.composer_cursor = move_cursor_vertically(
                    &self.manager.composer,
                    self.manager.composer_cursor,
                    self.manager.composer_width,
                    false,
                );
                self.manager_reference_cursor_moved();
                Action::None
            }
            KeyCode::Down
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.manager.composer_cursor = move_cursor_vertically(
                    &self.manager.composer,
                    self.manager.composer_cursor,
                    self.manager.composer_width,
                    true,
                );
                self.manager_reference_cursor_moved();
                Action::None
            }
            KeyCode::Home => {
                self.manager.composer_cursor =
                    current_line_start(&self.manager.composer, self.manager.composer_cursor);
                self.manager_reference_cursor_moved();
                Action::None
            }
            KeyCode::End => {
                self.manager.composer_cursor =
                    current_line_end(&self.manager.composer, self.manager.composer_cursor);
                self.manager_reference_cursor_moved();
                Action::None
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.manager.composer.clear();
                self.manager.composer_cursor = 0;
                self.manager.reference_index = 0;
                self.manager.reference_dismissed = false;
                Action::None
            }
            KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.manager.composer_cursor =
                    current_line_start(&self.manager.composer, self.manager.composer_cursor);
                self.manager_reference_cursor_moved();
                Action::None
            }
            KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.manager.composer_cursor =
                    current_line_end(&self.manager.composer, self.manager.composer_cursor);
                self.manager_reference_cursor_moved();
                Action::None
            }
            KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.delete_manager_word();
                Action::None
            }
            KeyCode::Char('y') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.copy_latest_manager_message()
            }
            KeyCode::PageUp => {
                self.manager.scroll = self.manager.scroll.saturating_add(12);
                Action::None
            }
            KeyCode::PageDown => {
                self.manager.scroll = self.manager.scroll.saturating_sub(12);
                Action::None
            }
            KeyCode::Up if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.manager.scroll = self.manager.scroll.saturating_add(3);
                Action::None
            }
            KeyCode::Down if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.manager.scroll = self.manager.scroll.saturating_sub(3);
                Action::None
            }
            KeyCode::Enter
                if key
                    .modifiers
                    .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) =>
            {
                self.insert_manager_text("\n");
                Action::None
            }
            KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.insert_manager_text("\n");
                Action::None
            }
            KeyCode::Enter => {
                if self.manager.turn.is_some() {
                    self.error("this manager is still working");
                    return Action::None;
                }
                let prompt = self.manager.composer.trim().to_string();
                if prompt.is_empty() {
                    self.notice("type a message for the manager first");
                    return Action::None;
                }
                self.manager.composer.clear();
                self.manager.composer_cursor = 0;
                self.manager.reference_index = 0;
                self.manager.reference_dismissed = false;
                let workspace_id = self
                    .manager
                    .session
                    .as_ref()
                    .and_then(|session| session.workspace_id.clone());
                Action::SendManagerMessage {
                    workspace_id,
                    prompt,
                }
            }
            KeyCode::Char(character)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.insert_manager_text(&character.to_string());
                Action::None
            }
            _ => Action::None,
        }
    }

    fn open_manager_copy_view(&mut self) {
        self.manager.copy_view = true;
        self.manager.focused = false;
        self.manager.scroll = 0;
        let document = self.manager_copy_document();
        self.manager.copy_cursor = document.len();
        self.manager.copy_anchor = None;
        self.manager.copy_dragging = false;
        self.manager.copy_drag_origin = self.manager.copy_cursor;
        self.manager.copy_drag_origin_end = self.manager.copy_cursor;
        self.manager.copy_view_top = usize::MAX;
        self.message = None;
    }

    fn handle_manager_copy_view_key(&mut self, key: KeyEvent) -> Action {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.manager.copy_view = false;
                self.manager.copy_dragging = false;
                Action::None
            }
            KeyCode::Char('v') => {
                self.manager.copy_anchor = if self.manager.copy_anchor.is_some() {
                    None
                } else {
                    Some(self.manager.copy_cursor)
                };
                Action::None
            }
            KeyCode::Char('y') | KeyCode::Enter => self.copy_manager_selection(),
            KeyCode::Char('Y') => self.copy_manager_transcript(),
            KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                let document = self.manager_copy_document();
                self.manager.copy_anchor = Some(0);
                self.manager.copy_cursor = document.len();
                self.ensure_manager_copy_cursor_visible();
                Action::None
            }
            KeyCode::Left | KeyCode::Char('h') => {
                self.move_manager_copy_horizontal(
                    false,
                    key.modifiers.contains(KeyModifiers::SHIFT),
                );
                Action::None
            }
            KeyCode::Right | KeyCode::Char('l') => {
                self.move_manager_copy_horizontal(
                    true,
                    key.modifiers.contains(KeyModifiers::SHIFT),
                );
                Action::None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_manager_copy_vertical(-1, key.modifiers.contains(KeyModifiers::SHIFT));
                Action::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_manager_copy_vertical(1, key.modifiers.contains(KeyModifiers::SHIFT));
                Action::None
            }
            KeyCode::PageUp => {
                let amount = self.manager.copy_view_height.max(1) as isize;
                self.move_manager_copy_vertical(
                    -amount,
                    key.modifiers.contains(KeyModifiers::SHIFT),
                );
                Action::None
            }
            KeyCode::PageDown => {
                let amount = self.manager.copy_view_height.max(1) as isize;
                self.move_manager_copy_vertical(
                    amount,
                    key.modifiers.contains(KeyModifiers::SHIFT),
                );
                Action::None
            }
            KeyCode::Home => {
                self.set_manager_copy_cursor(0, key.modifiers.contains(KeyModifiers::SHIFT));
                Action::None
            }
            KeyCode::End | KeyCode::Char('G') => {
                let end = self.manager_copy_document().len();
                self.set_manager_copy_cursor(end, key.modifiers.contains(KeyModifiers::SHIFT));
                Action::None
            }
            _ => Action::None,
        }
    }

    fn manager_copy_document(&self) -> String {
        manager_transcript_text(
            &self.manager.messages,
            &self.manager.progress,
            self.manager_label(),
        )
    }

    fn set_manager_copy_cursor(&mut self, cursor: usize, extend: bool) {
        let document = self.manager_copy_document();
        let cursor = clamp_char_boundary(&document, cursor.min(document.len()));
        if extend {
            self.manager
                .copy_anchor
                .get_or_insert(self.manager.copy_cursor.min(document.len()));
        } else {
            self.manager.copy_anchor = None;
        }
        self.manager.copy_cursor = cursor;
        self.ensure_manager_copy_cursor_visible();
    }

    fn move_manager_copy_horizontal(&mut self, forwards: bool, extend: bool) {
        let document = self.manager_copy_document();
        let cursor = clamp_char_boundary(&document, self.manager.copy_cursor.min(document.len()));
        let next = if forwards {
            next_char_boundary(&document, cursor)
        } else {
            previous_char_boundary(&document, cursor)
        };
        self.set_manager_copy_cursor(next, extend);
    }

    fn move_manager_copy_vertical(&mut self, rows: isize, extend: bool) {
        let document = self.manager_copy_document();
        let width = self.manager.copy_view_width.max(1);
        let lines = manager_copy_lines(&document, width);
        let current = manager_copy_line_at(&lines, self.manager.copy_cursor.min(document.len()));
        let column = document
            [lines[current].start..self.manager.copy_cursor.min(lines[current].end)]
            .chars()
            .count();
        let target = current
            .saturating_add_signed(rows)
            .min(lines.len().saturating_sub(1));
        let cursor =
            byte_at_character_column(&document, lines[target].start, lines[target].end, column);
        self.set_manager_copy_cursor(cursor, extend);
    }

    fn ensure_manager_copy_cursor_visible(&mut self) {
        let document = self.manager_copy_document();
        let lines = manager_copy_lines(&document, self.manager.copy_view_width.max(1));
        let cursor_line =
            manager_copy_line_at(&lines, self.manager.copy_cursor.min(document.len()));
        let height = self.manager.copy_view_height.max(1);
        let max_top = lines.len().saturating_sub(height);
        if self.manager.copy_view_top == usize::MAX {
            self.manager.copy_view_top = max_top;
        }
        if cursor_line < self.manager.copy_view_top {
            self.manager.copy_view_top = cursor_line;
        } else if cursor_line >= self.manager.copy_view_top.saturating_add(height) {
            self.manager.copy_view_top = cursor_line.saturating_sub(height - 1);
        }
        self.manager.copy_view_top = self.manager.copy_view_top.min(max_top);
    }

    fn copy_manager_selection(&self) -> Action {
        let document = self.manager_copy_document();
        if document.is_empty() {
            return Action::None;
        }
        let cursor = clamp_char_boundary(&document, self.manager.copy_cursor.min(document.len()));
        let selected = self
            .manager
            .copy_anchor
            .map(|anchor| clamp_char_boundary(&document, anchor.min(document.len())))
            .filter(|anchor| *anchor != cursor)
            .map(|anchor| {
                let (start, end) = if anchor < cursor {
                    (anchor, cursor)
                } else {
                    (cursor, anchor)
                };
                document[start..end].to_string()
            })
            .unwrap_or_else(|| {
                let lines = manager_copy_lines(&document, self.manager.copy_view_width.max(1));
                let line = lines[manager_copy_line_at(&lines, cursor)];
                document[line.start..line.end].to_string()
            });
        if selected.is_empty() {
            return Action::None;
        }
        Action::CopyManager {
            text: selected,
            description: "the selected manager text",
        }
    }

    fn handle_paste(&mut self, value: &str) {
        if !self.manager.focused {
            return;
        }
        let normalized = value.replace("\r\n", "\n").replace('\r', "\n");
        self.insert_manager_text(&normalized);
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) -> Action {
        if self.manager.copy_view {
            return self.handle_manager_copy_view_mouse(mouse);
        }
        let over_transcript = self
            .manager
            .transcript_area
            .is_some_and(|area| rect_contains(area, mouse.column, mouse.row));
        match mouse.kind {
            MouseEventKind::ScrollUp if over_transcript => {
                self.manager.scroll = self.manager.scroll.saturating_add(6);
            }
            MouseEventKind::ScrollDown if over_transcript => {
                self.manager.scroll = self.manager.scroll.saturating_sub(6);
            }
            MouseEventKind::Down(MouseButton::Left) if over_transcript => {
                self.manager.focused = false;
            }
            MouseEventKind::Down(MouseButton::Left)
                if self
                    .manager
                    .composer_area
                    .is_some_and(|area| rect_contains(area, mouse.column, mouse.row)) =>
            {
                let area = self.manager.composer_area.expect("composer area checked");
                let visual_row =
                    self.manager.composer_view_start + mouse.row.saturating_sub(area.y) as usize;
                let visual_column = mouse.column.saturating_sub(area.x).saturating_sub(2) as usize;
                self.manager.composer_cursor = editor_cursor_from_position(
                    &self.manager.composer,
                    self.manager.composer_width,
                    visual_row,
                    visual_column,
                );
                self.manager.focused = true;
                self.manager_reference_cursor_moved();
                self.message = None;
            }
            _ => {}
        }
        Action::None
    }

    fn handle_manager_copy_view_mouse(&mut self, mouse: MouseEvent) -> Action {
        let Some(area) = self.manager.transcript_area else {
            return Action::None;
        };
        let document = self.manager_copy_document();
        let lines = manager_copy_lines(&document, self.manager.copy_view_width.max(1));
        let max_top = lines
            .len()
            .saturating_sub(self.manager.copy_view_height.max(1));
        match mouse.kind {
            MouseEventKind::ScrollUp if rect_contains(area, mouse.column, mouse.row) => {
                self.manager.copy_view_top =
                    self.manager.copy_view_top.min(max_top).saturating_sub(3);
            }
            MouseEventKind::ScrollDown if rect_contains(area, mouse.column, mouse.row) => {
                self.manager.copy_view_top = self
                    .manager
                    .copy_view_top
                    .min(max_top)
                    .saturating_add(3)
                    .min(max_top);
            }
            MouseEventKind::Down(MouseButton::Left)
                if rect_contains(area, mouse.column, mouse.row) =>
            {
                let (start, end) = manager_copy_cell_from_mouse(
                    &document,
                    &lines,
                    self.manager.copy_view_top.min(max_top),
                    area,
                    mouse.column,
                    mouse.row,
                );
                self.manager.copy_drag_origin = start;
                self.manager.copy_drag_origin_end = end;
                self.manager.copy_anchor = Some(start);
                self.manager.copy_cursor = end;
                self.manager.copy_dragging = true;
            }
            MouseEventKind::Drag(MouseButton::Left) if self.manager.copy_dragging => {
                let (start, end) = manager_copy_cell_from_mouse(
                    &document,
                    &lines,
                    self.manager.copy_view_top.min(max_top),
                    area,
                    mouse.column,
                    mouse.row,
                );
                self.set_manager_mouse_selection(start, end, &document);
            }
            MouseEventKind::Up(MouseButton::Left) if self.manager.copy_dragging => {
                let (start, end) = manager_copy_cell_from_mouse(
                    &document,
                    &lines,
                    self.manager.copy_view_top.min(max_top),
                    area,
                    mouse.column,
                    mouse.row,
                );
                self.set_manager_mouse_selection(start, end, &document);
                self.manager.copy_dragging = false;
            }
            _ => {}
        }
        Action::None
    }

    fn set_manager_mouse_selection(&mut self, start: usize, end: usize, document: &str) {
        let origin = self.manager.copy_drag_origin.min(document.len());
        if start < origin {
            self.manager.copy_anchor = Some(self.manager.copy_drag_origin_end.min(document.len()));
            self.manager.copy_cursor = start;
        } else {
            self.manager.copy_anchor = Some(origin);
            self.manager.copy_cursor = end;
        }
    }

    fn insert_manager_text(&mut self, value: &str) {
        const LIMIT: usize = 12_000;
        let remaining = LIMIT.saturating_sub(self.manager.composer.chars().count());
        if remaining == 0 {
            return;
        }
        let inserted = value.chars().take(remaining).collect::<String>();
        let cursor = clamp_char_boundary(&self.manager.composer, self.manager.composer_cursor);
        self.manager.composer.insert_str(cursor, &inserted);
        self.manager.composer_cursor = cursor + inserted.len();
        self.manager_reference_cursor_moved();
    }

    fn delete_manager_character(&mut self, backwards: bool) {
        let cursor = clamp_char_boundary(&self.manager.composer, self.manager.composer_cursor);
        let (start, end) = if backwards {
            (
                previous_char_boundary(&self.manager.composer, cursor),
                cursor,
            )
        } else {
            (cursor, next_char_boundary(&self.manager.composer, cursor))
        };
        if start != end {
            self.manager.composer.replace_range(start..end, "");
            self.manager.composer_cursor = start;
        }
        self.manager_reference_cursor_moved();
    }

    fn move_manager_word(&mut self, forwards: bool) {
        self.manager.composer_cursor = if forwards {
            next_word_boundary(&self.manager.composer, self.manager.composer_cursor)
        } else {
            previous_word_boundary(&self.manager.composer, self.manager.composer_cursor)
        };
        self.manager_reference_cursor_moved();
    }

    fn delete_manager_word(&mut self) {
        let cursor = clamp_char_boundary(&self.manager.composer, self.manager.composer_cursor);
        let start = previous_word_boundary(&self.manager.composer, cursor);
        if start != cursor {
            self.manager.composer.replace_range(start..cursor, "");
            self.manager.composer_cursor = start;
        }
        self.manager_reference_cursor_moved();
    }

    fn manager_reference_cursor_moved(&mut self) {
        self.manager.reference_index = 0;
        self.manager.reference_dismissed = false;
    }

    fn manager_reference_matches(&self) -> Vec<ManagerReference> {
        let cursor = clamp_char_boundary(&self.manager.composer, self.manager.composer_cursor);
        let Some((kind, _, query)) = active_manager_reference(&self.manager.composer[..cursor])
        else {
            return Vec::new();
        };
        let normalized_query = query.to_ascii_lowercase();
        let mut matches = self
            .manager
            .references
            .iter()
            .filter(|reference| {
                reference.kind == kind
                    && reference
                        .label
                        .to_ascii_lowercase()
                        .contains(&normalized_query)
            })
            .cloned()
            .collect::<Vec<_>>();
        if kind == ManagerReferenceKind::File
            && let Some(scope) = &self.manager.scope
        {
            matches.extend(manager_path_reference_matches(scope, query));
        }
        matches.sort_by_key(|reference| {
            let label = reference.label.to_ascii_lowercase();
            (!label.starts_with(&normalized_query), label.len(), label)
        });
        matches.dedup_by(|left, right| left.kind == right.kind && left.label == right.label);
        matches.truncate(8);
        matches
    }

    fn insert_manager_reference(&mut self) {
        let cursor = clamp_char_boundary(&self.manager.composer, self.manager.composer_cursor);
        let Some((kind, start, _)) = active_manager_reference(&self.manager.composer[..cursor])
        else {
            return;
        };
        let matches = self.manager_reference_matches();
        let Some(reference) = matches.get(
            self.manager
                .reference_index
                .min(matches.len().saturating_sub(1)),
        ) else {
            return;
        };
        let label = reference.label.clone();
        let directory = reference.directory;
        let sigil = match kind {
            ManagerReferenceKind::File => '@',
            ManagerReferenceKind::Skill => '$',
        };
        let replacement =
            if kind == ManagerReferenceKind::File && label.contains(char::is_whitespace) {
                if directory {
                    format!("{sigil}{{{label}")
                } else {
                    format!("{sigil}{{{label}}} ")
                }
            } else if directory {
                format!("{sigil}{label}")
            } else {
                format!("{sigil}{label} ")
            };
        self.manager
            .composer
            .replace_range(start..cursor, &replacement);
        self.manager.composer_cursor = start + replacement.len();
        self.manager.reference_index = 0;
        self.manager.reference_dismissed = false;
    }

    fn copy_latest_manager_message(&self) -> Action {
        self.manager
            .messages
            .iter()
            .rev()
            .find(|message| message.role == "assistant")
            .map(|message| Action::CopyManager {
                text: message.content.clone(),
                description: "the latest manager response",
            })
            .unwrap_or(Action::None)
    }

    fn copy_manager_transcript(&self) -> Action {
        if self.manager.messages.is_empty() && self.manager.progress.is_empty() {
            return Action::None;
        }
        let label = self.manager_label();
        let text = manager_transcript_text(&self.manager.messages, &self.manager.progress, label);
        Action::CopyManager {
            text,
            description: "the manager transcript",
        }
    }

    fn handle_prompt_key(&mut self, key: KeyEvent) -> Action {
        match key.code {
            KeyCode::Esc => {
                self.prompt = None;
                Action::None
            }
            KeyCode::Backspace => {
                if let Some(prompt) = &mut self.prompt {
                    prompt.value.pop();
                }
                Action::None
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(prompt) = &mut self.prompt {
                    prompt.value.clear();
                }
                Action::None
            }
            KeyCode::Char(character)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                if let Some(prompt) = &mut self.prompt
                    && prompt.value.chars().count() < 96
                {
                    prompt.value.push(character);
                }
                Action::None
            }
            KeyCode::Enter => {
                let Some(prompt) = self.prompt.take() else {
                    return Action::None;
                };
                let name = prompt.value.trim().to_string();
                if name.is_empty() {
                    self.error("name must not be empty");
                    return Action::None;
                }
                match prompt.kind {
                    PromptKind::Workspace => Action::CreateWorkspace { name },
                    PromptKind::Tab {
                        workspace_id,
                        start_codex,
                    } => Action::CreateTab {
                        workspace_id,
                        name,
                        start_codex,
                    },
                    PromptKind::RenameTab {
                        workspace_id,
                        tab_id,
                    } => Action::RenameTab {
                        workspace_id,
                        tab_id,
                        name,
                    },
                }
            }
            _ => Action::None,
        }
    }

    fn begin_create(&mut self) {
        match self.view {
            View::Home => self.begin_workspace(),
            View::Sessions if self.active == Panel::Tabs => self.begin_tab(false),
            View::Sessions => self.begin_workspace(),
            View::Threads => self.begin_tab(true),
            View::Wips => self.notice("WIP creation arrives with the scheduler runtime next slice"),
        }
    }

    fn begin_workspace(&mut self) {
        let suggested_name = std::env::current_dir()
            .ok()
            .and_then(|cwd| {
                cwd.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            })
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| "workspace".to_string());
        self.prompt = Some(Prompt {
            kind: PromptKind::Workspace,
            value: suggested_name,
        });
        self.message = None;
    }

    fn begin_tab(&mut self, start_codex: bool) {
        let Some(workspace) = self.selected_workspace() else {
            self.error("create a workspace first with c, or ask Lumbergh from Home");
            return;
        };
        self.prompt = Some(Prompt::new(PromptKind::Tab {
            workspace_id: workspace.id.clone(),
            start_codex,
        }));
        self.message = None;
    }

    fn begin_rename(&mut self) {
        if self.view != View::Sessions || self.active != Panel::Tabs {
            self.notice("open Sessions and focus its tabs to rename one");
            return;
        }
        let (Some(workspace), Some(tab)) = (self.selected_workspace(), self.selected_tab()) else {
            self.error("select a tab to rename");
            return;
        };
        self.prompt = Some(Prompt {
            kind: PromptKind::RenameTab {
                workspace_id: workspace.id.clone(),
                tab_id: tab.id.clone(),
            },
            value: tab.name.clone(),
        });
        self.message = None;
    }

    fn enter_action(&mut self) -> Action {
        match self.view {
            View::Home => Action::FocusManager,
            View::Sessions if self.active == Panel::Workspaces => self
                .selected_workspace()
                .map(|workspace| Action::ActivateWorkspace(workspace.id.clone()))
                .unwrap_or(Action::None),
            View::Sessions => self
                .selected_workspace()
                .zip(self.selected_tab())
                .map(|(workspace, tab)| {
                    if is_manager_tab(tab) {
                        Action::OpenMiddleManager {
                            workspace_id: workspace.id.clone(),
                        }
                    } else {
                        Action::ActivateTab {
                            workspace_id: workspace.id.clone(),
                            tab_id: tab.id.clone(),
                        }
                    }
                })
                .unwrap_or(Action::None),
            View::Threads => self
                .selected_thread()
                .map(|thread| Action::OpenThread {
                    thread_id: thread.id.clone(),
                    workspace_id: self.selected_workspace().map(|item| item.id.clone()),
                })
                .unwrap_or(Action::None),
            View::Wips => {
                self.notice("WIP schedules are not connected yet · press m to ask Lumbergh");
                Action::None
            }
        }
    }

    fn move_selection(&mut self, delta: isize) {
        match self.view {
            View::Home => move_state(&mut self.workspace_state, self.workspaces.len(), delta),
            View::Sessions if self.active == Panel::Workspaces => {
                move_state(&mut self.workspace_state, self.workspaces.len(), delta)
            }
            View::Sessions => move_state(&mut self.tab_state, self.tabs.len(), delta),
            View::Threads => move_state(&mut self.thread_state, self.threads.len(), delta),
            View::Wips => {}
        }
    }

    fn select_edge(&mut self, last: bool) {
        let (state, len) = match self.view {
            View::Home => (&mut self.workspace_state, self.workspaces.len()),
            View::Sessions if self.active == Panel::Workspaces => {
                (&mut self.workspace_state, self.workspaces.len())
            }
            View::Sessions => (&mut self.tab_state, self.tabs.len()),
            View::Threads => (&mut self.thread_state, self.threads.len()),
            View::Wips => return,
        };
        state.select((len > 0).then_some(if last { len - 1 } else { 0 }));
    }

    fn set_view(&mut self, view: View) {
        self.view = view;
        self.active = match view {
            View::Home | View::Sessions => {
                if self.active == Panel::Threads {
                    Panel::Workspaces
                } else {
                    self.active
                }
            }
            View::Threads => Panel::Threads,
            View::Wips => Panel::Workspaces,
        };
        self.message = None;
    }

    fn render(&mut self, frame: &mut Frame<'_>) {
        let area = frame.area();
        self.manager.transcript_area = None;
        self.manager.composer_area = None;
        frame.render_widget(Block::default().style(Style::default().bg(DEEP)), area);
        if self.manager.copy_view {
            self.render_manager_copy_view(frame, area);
            return;
        }
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(8), Constraint::Length(2)])
            .split(area);

        if rows[0].width >= 126 {
            let columns = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Length(25),
                    Constraint::Min(58),
                    Constraint::Length(29),
                ])
                .split(rows[0]);
            self.render_sidebar(frame, columns[0]);
            self.render_main(frame, inset(columns[1], 2, 1));
            self.render_status_rail(frame, columns[2]);
        } else if rows[0].width >= 88 {
            let columns = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Length(22), Constraint::Min(56)])
                .split(rows[0]);
            self.render_sidebar(frame, columns[0]);
            self.render_main(frame, inset(columns[1], 2, 1));
        } else {
            let compact = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(3), Constraint::Min(5)])
                .split(rows[0]);
            self.render_compact_header(frame, compact[0]);
            self.render_main(frame, inset(compact[1], 1, 1));
        }

        self.render_footer(frame, rows[1]);
        if self.manager.overlay {
            self.render_manager_overlay(frame, area);
        }
        if self.show_help {
            self.render_help(frame, area);
        }
        if self.prompt.is_some() {
            self.render_prompt(frame, area);
        }
    }

    fn render_sidebar(&self, frame: &mut Frame<'_>, area: Rect) {
        let block = Block::default()
            .borders(Borders::RIGHT)
            .border_style(Style::default().fg(BORDER))
            .style(Style::default().bg(DEEP));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let logo_height = if inner.height >= 30 { 11 } else { 2 };
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(logo_height),
                Constraint::Length(11),
                Constraint::Min(1),
                Constraint::Length(4),
            ])
            .split(inner);

        if logo_height > 2 {
            let mut logo = WIPSAW_MARK
                .iter()
                .map(|line| {
                    Line::styled(
                        *line,
                        Style::default().fg(STEEL).add_modifier(Modifier::BOLD),
                    )
                })
                .collect::<Vec<_>>();
            logo.push(Line::raw(""));
            logo.push(Line::styled(
                "WIPSAW",
                Style::default().fg(INK).add_modifier(Modifier::BOLD),
            ));
            frame.render_widget(Paragraph::new(logo).alignment(Alignment::Center), rows[0]);
        } else {
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(" W ", Style::default().fg(DEEP).bg(STEEL)),
                    Span::styled(
                        " WIPSAW",
                        Style::default().fg(INK).add_modifier(Modifier::BOLD),
                    ),
                ])),
                rows[0],
            );
        }

        let navigation = [
            (Some(View::Home), "1", "HOME"),
            (Some(View::Sessions), "2", "SESSIONS"),
            (Some(View::Threads), "3", "THREADS"),
            (Some(View::Wips), "4", "WIPs"),
            (None, " ", "RUNS      · next"),
            (None, " ", "HOSTS     · next"),
            (None, " ", "ACCOUNTS  · next"),
            (None, " ", "USAGE     · next"),
        ];
        let navigation = navigation
            .into_iter()
            .map(|(view, key, label)| {
                let active = view == Some(self.view);
                Line::from(vec![
                    Span::styled(
                        if active { "› " } else { "  " },
                        Style::default().fg(CYAN).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!("{key} "),
                        Style::default().fg(if active { CYAN } else { MUTED }),
                    ),
                    Span::styled(
                        label,
                        Style::default()
                            .fg(if active { INK } else { MUTED })
                            .add_modifier(if active {
                                Modifier::BOLD
                            } else {
                                Modifier::empty()
                            }),
                    ),
                ])
            })
            .collect::<Vec<_>>();
        frame.render_widget(Paragraph::new(navigation), rows[1]);

        frame.render_widget(
            Paragraph::new(vec![
                Line::from(vec![
                    Span::styled(" Ctrl-b w", Style::default().fg(CYAN)),
                    Span::styled("  navigator", Style::default().fg(MUTED)),
                ]),
                Line::from(vec![
                    Span::styled(" m", Style::default().fg(CYAN)),
                    Span::styled("         Lumbergh", Style::default().fg(MUTED)),
                ]),
                Line::from(vec![
                    Span::styled(" ?", Style::default().fg(CYAN)),
                    Span::styled("         guide", Style::default().fg(MUTED)),
                ]),
            ]),
            rows[3],
        );
    }

    fn render_compact_header(&self, frame: &mut Frame<'_>, area: Rect) {
        let view = match self.view {
            View::Home => "HOME",
            View::Sessions => "SESSIONS",
            View::Threads => "THREADS",
            View::Wips => "WIPs",
        };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    " W ",
                    Style::default()
                        .fg(DEEP)
                        .bg(STEEL)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    " WIPSAW  ",
                    Style::default().fg(INK).add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!("{view}  "), Style::default().fg(CYAN)),
                Span::styled(
                    "1 Home  2 Sessions  3 Threads  4 WIPs",
                    Style::default().fg(MUTED),
                ),
            ]))
            .block(
                Block::default()
                    .borders(Borders::BOTTOM)
                    .border_style(Style::default().fg(BORDER)),
            ),
            area,
        );
    }

    fn render_status_rail(&self, frame: &mut Frame<'_>, area: Rect) {
        let live = self.workspaces.iter().filter(|(_, live)| *live).count();
        let current_account = self
            .accounts
            .first()
            .map(|account| account.alias.as_str())
            .unwrap_or("none");
        let block = Block::default()
            .borders(Borders::LEFT)
            .border_style(Style::default().fg(BORDER));
        let inner = inset(block.inner(area), 2, 1);
        frame.render_widget(block, area);
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(15),
                Constraint::Length(6),
                Constraint::Length(5),
                Constraint::Min(5),
            ])
            .split(inner);

        frame.render_widget(
            Paragraph::new(vec![
                metric_line(self.workspaces.len(), "WORKSPACES"),
                metric_line(live, "LIVE"),
                metric_line(self.total_tabs, "TABS"),
                metric_line(self.threads.len(), "THREADS"),
                metric_line(0, "RUNNING WIPs"),
            ])
            .block(
                Block::default()
                    .title(section_title("OVERVIEW"))
                    .borders(Borders::BOTTOM)
                    .border_style(Style::default().fg(BORDER)),
            ),
            rows[0],
        );
        frame.render_widget(
            Paragraph::new(vec![
                health_line("Codex", self.health.codex),
                health_line("tmux", self.health.tmux),
                health_line("Docker", self.health.docker),
            ])
            .block(Block::default().title(section_title("SYSTEM"))),
            rows[1],
        );
        frame.render_widget(
            Paragraph::new(vec![
                Line::styled(current_account.to_string(), Style::default().fg(INK)),
                Line::styled(
                    format!(
                        "{} homes · {} profiles",
                        self.homes.len(),
                        self.profiles.len()
                    ),
                    Style::default().fg(MUTED),
                ),
            ])
            .block(
                Block::default()
                    .title(section_title("IDENTITY"))
                    .borders(Borders::BOTTOM)
                    .border_style(Style::default().fg(BORDER)),
            ),
            rows[2],
        );
        frame.render_widget(
            Paragraph::new(vec![
                Line::styled("WIP runtime", Style::default().fg(INK)),
                Line::styled(
                    "scheduler transplant · next slice",
                    Style::default().fg(MUTED),
                ),
                Line::raw(""),
                Line::styled("m  ask Lumbergh", Style::default().fg(CYAN)),
            ])
            .block(Block::default().title(section_title("UP NEXT")))
            .wrap(Wrap { trim: true }),
            rows[3],
        );
    }

    fn render_main(&mut self, frame: &mut Frame<'_>, area: Rect) {
        match self.view {
            View::Home => self.render_home(frame, area),
            View::Sessions => self.render_sessions(frame, area),
            View::Threads => self.render_threads_view(frame, area),
            View::Wips => self.render_wips(frame, area),
        }
    }

    fn render_home(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let compact = area.height < 26;
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints(if compact {
                [
                    Constraint::Length(3),
                    Constraint::Min(5),
                    Constraint::Length(5),
                    Constraint::Length(3),
                    Constraint::Length(5),
                ]
            } else {
                [
                    Constraint::Length(4),
                    Constraint::Min(10),
                    Constraint::Length(5),
                    Constraint::Length(3),
                    Constraint::Length(8),
                ]
            })
            .split(area);

        frame.render_widget(
            Paragraph::new(vec![
                Line::styled(
                    "Ready to cut?",
                    Style::default().fg(INK).add_modifier(Modifier::BOLD),
                ),
                Line::styled(
                    "Run your Codex workspace from one place.",
                    Style::default().fg(MUTED),
                ),
            ]),
            rows[0],
        );
        self.render_manager_transcript(frame, rows[1]);
        self.render_manager_composer(frame, rows[2]);
        self.render_manager_reference_menu(frame, rows[2]);
        self.render_quick_actions(frame, rows[3]);
        self.render_home_activity(frame, rows[4]);
    }

    fn render_manager_transcript(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let label = self.manager_label();
        let activity = self.manager.activity.as_deref().unwrap_or("ready");
        let scope = self
            .manager
            .scope
            .as_ref()
            .map(|scope| {
                if scope.machine_wide {
                    "MACHINE READ"
                } else {
                    "WORKSPACE READ"
                }
            })
            .unwrap_or("READ SCOPE");
        let block = Block::default()
            .title(Line::from(vec![
                Span::styled(
                    format!(" {label} "),
                    Style::default().fg(CYAN).add_modifier(Modifier::BOLD),
                ),
                Span::styled("CODEX MANAGER · TERRA MEDIUM", Style::default().fg(MUTED)),
                Span::styled(format!(" · {scope}"), Style::default().fg(MUTED)),
                Span::styled(format!(" · {activity} "), Style::default().fg(AMBER)),
                Span::styled(
                    if self.manager.scroll > 0 {
                        format!(" · HISTORY ↑{} ", self.manager.scroll)
                    } else {
                        " · LIVE ".to_string()
                    },
                    Style::default().fg(if self.manager.scroll > 0 {
                        AMBER
                    } else {
                        GREEN
                    }),
                ),
            ]))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(if self.manager.turn.is_some() {
                AMBER
            } else {
                CYAN
            }))
            .style(Style::default().bg(PANEL));
        let inner = block.inner(area);
        self.manager.transcript_area = Some(inner);
        let lines = manager_transcript_lines(
            &self.manager.messages,
            &self.manager.progress,
            label,
            inner.width.saturating_sub(1).max(12) as usize,
        );
        let scroll = manager_scroll_position(lines.len(), inner.height, self.manager.scroll);
        frame.render_widget(
            Paragraph::new(lines)
                .block(block)
                .scroll((scroll, 0))
                .wrap(Wrap { trim: false }),
            area,
        );
    }

    fn render_manager_composer(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let block = Block::default()
            .title(if area.width >= 76 {
                " COMPOSER · Enter send · Ctrl+J newline · arrows edit · PgUp/PgDn history · @ files · $ skills "
            } else {
                " COMPOSER · Enter send · arrows edit "
            })
            .borders(Borders::ALL)
            .border_style(Style::default().fg(if self.manager.focused { CYAN } else { BORDER }))
            .style(Style::default().bg(DEEP));
        let inner = block.inner(area);
        self.manager.composer_area = Some(inner);
        self.manager.composer_width = inner.width.saturating_sub(2).max(1) as usize;
        let (lines, cursor_row) = manager_editor_lines(
            &self.manager.composer,
            self.manager.composer_cursor,
            inner.width.max(4) as usize,
            self.manager.focused,
        );
        let visible = (inner.height as usize).max(1);
        let view_start = if lines.len() > visible {
            cursor_row
                .saturating_sub(visible - 1)
                .min(lines.len() - visible)
        } else {
            0
        };
        self.manager.composer_view_start = view_start;
        let visible_lines = lines
            .into_iter()
            .skip(view_start)
            .take(visible)
            .collect::<Vec<_>>();
        frame.render_widget(Paragraph::new(visible_lines).block(block), area);
    }

    fn render_manager_reference_menu(&self, frame: &mut Frame<'_>, composer: Rect) {
        if !self.manager.focused || self.manager.reference_dismissed {
            return;
        }
        let matches = self.manager_reference_matches();
        if matches.is_empty() {
            return;
        }
        let kind = matches[0].kind;
        let height = (matches.len() as u16 + 2).min(10);
        let width = composer.width.saturating_sub(2).min(76);
        if width < 24 {
            return;
        }
        let area = Rect {
            x: composer.x.saturating_add(1),
            y: composer.y.saturating_sub(height),
            width,
            height,
        };
        frame.render_widget(Clear, area);
        let selected = self.manager.reference_index.min(matches.len() - 1);
        let items = matches
            .iter()
            .enumerate()
            .map(|(index, reference)| {
                let sigil = if reference.kind == ManagerReferenceKind::File {
                    '@'
                } else {
                    '$'
                };
                let mut spans = vec![Span::raw(format!(" {sigil}{}", reference.label))];
                if let Some(hint) = &reference.hint {
                    spans.push(Span::styled(
                        format!("  ·  {hint}"),
                        Style::default().fg(MUTED),
                    ));
                }
                ListItem::new(Line::from(spans)).style(if index == selected {
                    Style::default()
                        .bg(PANEL)
                        .fg(CYAN)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(INK)
                })
            })
            .collect::<Vec<_>>();
        frame.render_widget(
            List::new(items).block(
                Block::default()
                    .title(if kind == ManagerReferenceKind::File {
                        " @ FILES · ↑↓ choose · Enter/Tab insert "
                    } else {
                        " $ SKILLS · ↑↓ choose · Enter/Tab insert "
                    })
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(CYAN))
                    .style(Style::default().bg(DEEP)),
            ),
            area,
        );
    }

    fn render_manager_overlay(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let popup = centered_rect(area, 104, area.height.saturating_sub(4));
        frame.render_widget(Clear, popup);
        frame.render_widget(Block::default().style(Style::default().bg(DEEP)), popup);
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(6),
                Constraint::Length(6),
                Constraint::Length(2),
            ])
            .split(inset(popup, 1, 1));
        let workspace = self
            .manager
            .session
            .as_ref()
            .and_then(|session| session.workspace_name.as_deref())
            .unwrap_or("workspace");
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(vec![
                    Span::styled(
                        "MIDDLE MANAGER",
                        Style::default().fg(CYAN).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(format!("  //  {workspace}"), Style::default().fg(INK)),
                ]),
                Line::styled(
                    "Workspace-scoped · private Wipsaw MCP tools · escalates cross-workspace work to Lumbergh",
                    Style::default().fg(MUTED),
                ),
            ]),
            rows[0],
        );
        self.render_manager_transcript(frame, rows[1]);
        self.render_manager_composer(frame, rows[2]);
        self.render_manager_reference_menu(frame, rows[2]);
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("Enter", Style::default().fg(CYAN)),
                Span::styled(
                    " send  ·  arrows edit  ·  PgUp/PgDn history  ·  ",
                    Style::default().fg(MUTED),
                ),
                Span::styled("Esc", Style::default().fg(CYAN)),
                Span::styled(
                    if self.manager.focused {
                        " leave composer"
                    } else {
                        " close Middle Manager"
                    },
                    Style::default().fg(MUTED),
                ),
                Span::styled(
                    "  ·  v copy mode / y latest / Y transcript",
                    Style::default().fg(MUTED),
                ),
            ])),
            rows[3],
        );
    }

    fn render_manager_copy_view(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(3),
                Constraint::Length(2),
            ])
            .split(inset(area, 2, 1));
        let label = self.manager_label();
        frame.render_widget(
            Paragraph::new(vec![
                Line::styled(
                    format!("{label} SELECT & COPY"),
                    Style::default().fg(CYAN).add_modifier(Modifier::BOLD),
                ),
                Line::styled(
                    "Mouse-drag or use Shift+arrows to select exact transcript text; press y to copy it.",
                    Style::default().fg(MUTED),
                ),
            ]),
            rows[0],
        );
        self.manager.transcript_area = Some(rows[1]);
        self.manager.copy_view_width = rows[1].width.max(1) as usize;
        self.manager.copy_view_height = rows[1].height.max(1) as usize;
        let document = self.manager_copy_document();
        self.manager.copy_cursor =
            clamp_char_boundary(&document, self.manager.copy_cursor.min(document.len()));
        if let Some(anchor) = self.manager.copy_anchor.as_mut() {
            *anchor = clamp_char_boundary(&document, (*anchor).min(document.len()));
        }
        let copy_lines = manager_copy_lines(&document, self.manager.copy_view_width);
        let max_top = copy_lines
            .len()
            .saturating_sub(self.manager.copy_view_height);
        if self.manager.copy_view_top == usize::MAX {
            self.manager.copy_view_top = max_top;
        }
        self.manager.copy_view_top = self.manager.copy_view_top.min(max_top);
        let cursor_line = manager_copy_line_at(&copy_lines, self.manager.copy_cursor);
        let selection = self
            .manager
            .copy_anchor
            .filter(|anchor| *anchor != self.manager.copy_cursor)
            .map(|anchor| {
                if anchor < self.manager.copy_cursor {
                    (anchor, self.manager.copy_cursor)
                } else {
                    (self.manager.copy_cursor, anchor)
                }
            });
        let rendered = copy_lines
            .iter()
            .enumerate()
            .skip(self.manager.copy_view_top)
            .take(self.manager.copy_view_height)
            .map(|(index, line)| {
                render_manager_copy_line(
                    &document,
                    *line,
                    index == cursor_line,
                    self.manager.copy_cursor,
                    selection,
                )
            })
            .collect::<Vec<_>>();
        frame.render_widget(Paragraph::new(rendered), rows[1]);
        let selected_chars = selection
            .map(|(start, end)| document[start..end].chars().count())
            .unwrap_or(0);
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    if selected_chars > 0 {
                        format!(" {selected_chars} selected ")
                    } else {
                        " cursor ".to_string()
                    },
                    Style::default().fg(if selected_chars > 0 { CYAN } else { MUTED }),
                ),
                Span::styled(
                    "· drag/Shift+arrows select · y/Enter copy · Ctrl+A all · Y transcript · Esc return",
                    Style::default().fg(MUTED),
                ),
            ]))
            .block(
                Block::default()
                    .borders(Borders::TOP)
                    .border_style(Style::default().fg(BORDER)),
            ),
            rows[2],
        );
    }

    fn manager_label(&self) -> &'static str {
        if self
            .manager
            .session
            .as_ref()
            .is_some_and(|session| session.workspace_id.is_some())
        {
            "MIDDLE MANAGER"
        } else {
            "LUMBERGH"
        }
    }

    fn render_quick_actions(&self, frame: &mut Frame<'_>, area: Rect) {
        let shortcuts = if area.width < 92 {
            Line::from(vec![
                shortcut_span("c", "New workspace"),
                Span::raw("   "),
                shortcut_span("n", "New Codex session"),
                Span::raw("   "),
                shortcut_span("t", "New shell tab"),
            ])
        } else {
            Line::from(vec![
                shortcut_span("c", "New workspace"),
                Span::raw("   "),
                shortcut_span("n", "New Codex session"),
                Span::raw("   "),
                shortcut_span("t", "New shell tab"),
                Span::raw("   "),
                shortcut_span("g", "Schedule a WIP"),
            ])
        };
        frame.render_widget(
            Paragraph::new(shortcuts).block(
                Block::default()
                    .borders(Borders::TOP | Borders::BOTTOM)
                    .border_style(Style::default().fg(BORDER)),
            ),
            area,
        );
    }

    fn render_home_activity(&self, frame: &mut Frame<'_>, area: Rect) {
        let Some(workspace) = self.selected_workspace() else {
            frame.render_widget(
                Paragraph::new(vec![
                    Line::styled(
                        "NO WORKSPACE YET",
                        Style::default().fg(AMBER).add_modifier(Modifier::BOLD),
                    ),
                    Line::raw(""),
                    Line::styled(
                        "Press c to create one here, or tell Lumbergh what you want built.",
                        Style::default().fg(INK),
                    ),
                    Line::styled(
                        "The top manager is already available above with your current Codex auth.",
                        Style::default().fg(MUTED),
                    ),
                ])
                .block(Block::default().title(section_title("START HERE")))
                .wrap(Wrap { trim: true }),
                area,
            );
            return;
        };

        let is_live = self
            .workspaces
            .iter()
            .find(|(item, _)| item.id == workspace.id)
            .is_some_and(|(_, live)| *live);
        let title = Line::from(vec![
            Span::styled("ACTIVE WORKSPACE  ", Style::default().fg(MUTED)),
            Span::styled(
                &workspace.name,
                Style::default().fg(CYAN).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                if is_live {
                    "  ● LIVE"
                } else {
                    "  ○ STOPPED"
                },
                Style::default().fg(if is_live { GREEN } else { AMBER }),
            ),
            Span::styled("  ·  j/k switch", Style::default().fg(MUTED)),
        ]);
        let row_limit = area.height.saturating_sub(3) as usize;
        let rows = self
            .tabs
            .iter()
            .take(row_limit)
            .map(|tab| {
                let thread = self.thread_for_tab(tab);
                let profile = tab
                    .model_profile_id
                    .as_deref()
                    .and_then(|id| self.profiles.iter().find(|profile| profile.id == id));
                let home = tab
                    .codex_home_id
                    .as_deref()
                    .and_then(|id| self.homes.iter().find(|home| home.id == id));
                let kind = if is_manager_tab(tab) {
                    "middle manager"
                } else if tab.codex_thread_id.is_some() {
                    "codex"
                } else {
                    "shell"
                };
                let model = if is_manager_tab(tab) {
                    MANAGER_MODEL
                } else {
                    thread
                        .map(|thread| thread.model.as_str())
                        .or_else(|| profile.map(|profile| profile.model.as_str()))
                        .unwrap_or("default")
                };
                let account = thread
                    .map(|thread| thread.account_alias.as_str())
                    .or_else(|| home.map(|home| home.account_alias.as_str()))
                    .unwrap_or("current");
                let state = self.tab_live_state(tab);
                Row::new(vec![
                    format!("{}  {}", tab.tmux_window_index, tab.name),
                    kind.to_string(),
                    model.to_string(),
                    account.to_string(),
                    state.to_string(),
                ])
                .style(Style::default().fg(if state == "active" {
                    GREEN
                } else {
                    INK
                }))
            })
            .collect::<Vec<_>>();
        let table = Table::new(
            rows,
            [
                Constraint::Percentage(26),
                Constraint::Length(15),
                Constraint::Percentage(20),
                Constraint::Percentage(16),
                Constraint::Length(9),
            ],
        )
        .header(
            Row::new(["TAB", "TYPE", "MODEL", "ACCOUNT", "STATE"])
                .style(Style::default().fg(MUTED).add_modifier(Modifier::BOLD)),
        )
        .column_spacing(1)
        .block(
            Block::default()
                .title(title)
                .borders(Borders::TOP)
                .border_style(Style::default().fg(BORDER)),
        );
        frame.render_widget(table, area);
    }

    fn render_sessions(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(3), Constraint::Min(5)])
            .split(area);
        frame.render_widget(
            view_heading(
                "Sessions",
                "Workspaces own durable tmux sessions; tabs own shells and Codex threads.",
            ),
            rows[0],
        );
        if rows[1].width >= 72 {
            let columns = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(38), Constraint::Percentage(62)])
                .split(rows[1]);
            self.render_session_workspaces(frame, columns[0]);
            self.render_session_tabs(frame, inset(columns[1], 2, 0));
        } else {
            let compact = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(8), Constraint::Min(6)])
                .split(rows[1]);
            self.render_session_workspaces(frame, compact[0]);
            self.render_session_tabs(frame, compact[1]);
        }
    }

    fn render_session_workspaces(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let items = if self.workspaces.is_empty() {
            vec![ListItem::new(Text::from(vec![
                Line::styled("No workspaces yet", Style::default().fg(INK)),
                Line::styled("press c to create one", Style::default().fg(MUTED)),
            ]))]
        } else {
            self.workspaces
                .iter()
                .map(|(workspace, live)| {
                    ListItem::new(Text::from(vec![
                        Line::from(vec![
                            Span::styled(
                                if *live { "● " } else { "○ " },
                                Style::default().fg(if *live { GREEN } else { MUTED }),
                            ),
                            Span::styled(&workspace.name, Style::default().fg(INK)),
                            Span::styled(
                                if *live { "  live" } else { "  stopped" },
                                Style::default().fg(if *live { GREEN } else { AMBER }),
                            ),
                        ]),
                        Line::styled(
                            format!("  {}", workspace.cwd.display()),
                            Style::default().fg(MUTED),
                        ),
                    ]))
                })
                .collect()
        };
        render_section_list(
            frame,
            area,
            format!("WORKSPACES  {}", self.workspaces.len()),
            items,
            &mut self.workspace_state,
            self.active == Panel::Workspaces,
            Borders::RIGHT,
        );
    }

    fn render_session_tabs(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(5), Constraint::Length(8)])
            .split(area);
        let items = if self.tabs.is_empty() {
            vec![ListItem::new(Text::from(vec![
                Line::styled("No tabs in this workspace", Style::default().fg(INK)),
                Line::styled(
                    "t creates a shell · n creates Codex",
                    Style::default().fg(MUTED),
                ),
            ]))]
        } else {
            self.tabs
                .iter()
                .map(|tab| {
                    let kind = if is_manager_tab(tab) {
                        "MIDDLE MANAGER"
                    } else if tab.codex_thread_id.is_some() {
                        "CODEX"
                    } else {
                        "SHELL"
                    };
                    ListItem::new(Text::from(vec![
                        Line::from(vec![
                            Span::styled(
                                format!("{:>2}  ", tab.tmux_window_index),
                                Style::default().fg(MUTED),
                            ),
                            Span::styled(&tab.name, Style::default().fg(INK)),
                            Span::styled(format!("  {kind}"), Style::default().fg(CYAN)),
                            Span::styled(
                                format!("  {}", self.tab_live_state(tab)),
                                Style::default().fg(if self.tab_live_state(tab) == "active" {
                                    GREEN
                                } else {
                                    MUTED
                                }),
                            ),
                        ]),
                        Line::styled(
                            format!("    {}", tab.cwd.display()),
                            Style::default().fg(MUTED),
                        ),
                    ]))
                })
                .collect()
        };
        render_section_list(
            frame,
            rows[0],
            format!("TABS  {}", self.tabs.len()),
            items,
            &mut self.tab_state,
            self.active == Panel::Tabs,
            Borders::NONE,
        );
        self.render_tab_detail(frame, rows[1]);
    }

    fn render_tab_detail(&self, frame: &mut Frame<'_>, area: Rect) {
        let Some(tab) = self.selected_tab() else {
            frame.render_widget(
                Paragraph::new("Select a tab to inspect its resolved launch context")
                    .style(Style::default().fg(MUTED))
                    .block(
                        Block::default()
                            .title(section_title("RESOLVED CONTEXT"))
                            .borders(Borders::TOP)
                            .border_style(Style::default().fg(BORDER)),
                    ),
                area,
            );
            return;
        };
        let thread = self.thread_for_tab(tab);
        let home = tab
            .codex_home_id
            .as_deref()
            .and_then(|id| self.homes.iter().find(|home| home.id == id));
        let profile = tab
            .model_profile_id
            .as_deref()
            .and_then(|id| self.profiles.iter().find(|profile| profile.id == id));
        if is_manager_tab(tab) {
            let manager = self
                .managers
                .iter()
                .find(|manager| manager.workspace_id.as_deref() == Some(&tab.workspace_id));
            let manager_thread = manager
                .and_then(|manager| manager.native_thread_id.as_deref())
                .map(short_id)
                .unwrap_or_else(|| "starts on first message".to_string());
            let detail = vec![
                detail_line("type", "Middle Manager"),
                detail_line("model", "gpt-5.6-terra · medium"),
                detail_line(
                    "account",
                    home.map(|home| home.account_alias.as_str())
                        .unwrap_or("current"),
                ),
                detail_line("home", "Wipsaw isolated manager home"),
                detail_line("thread", &manager_thread),
                detail_line("scope", &tab.workspace_id),
            ];
            frame.render_widget(
                Paragraph::new(detail)
                    .block(
                        Block::default()
                            .title(section_title("RESOLVED CONTEXT"))
                            .borders(Borders::TOP)
                            .border_style(Style::default().fg(BORDER)),
                    )
                    .wrap(Wrap { trim: true }),
                area,
            );
            return;
        }
        let detail = vec![
            detail_line("type", if thread.is_some() { "Codex" } else { "shell" }),
            detail_line(
                "model",
                thread
                    .map(|thread| thread.model.as_str())
                    .or_else(|| profile.map(|profile| profile.model.as_str()))
                    .unwrap_or("inherited default"),
            ),
            detail_line(
                "account",
                thread
                    .map(|thread| thread.account_alias.as_str())
                    .or_else(|| home.map(|home| home.account_alias.as_str()))
                    .unwrap_or("current"),
            ),
            detail_line(
                "home",
                home.map(|home| home.name.as_str()).unwrap_or("current"),
            ),
            detail_line(
                "thread",
                thread
                    .map(|thread| short_id(&thread.id))
                    .as_deref()
                    .unwrap_or("not started"),
            ),
            detail_line("cwd", &tab.cwd.to_string_lossy()),
        ];
        frame.render_widget(
            Paragraph::new(detail)
                .block(
                    Block::default()
                        .title(section_title("RESOLVED CONTEXT"))
                        .borders(Borders::TOP)
                        .border_style(Style::default().fg(BORDER)),
                )
                .wrap(Wrap { trim: true }),
            area,
        );
    }

    fn render_threads_view(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(3), Constraint::Min(5)])
            .split(area);
        frame.render_widget(
            view_heading(
                "Codex threads",
                "Named, resumable conversations tracked by stable Wipsaw IDs.",
            ),
            rows[0],
        );
        let (list_area, detail_area) = if rows[1].width >= 76 {
            let columns = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
                .split(rows[1]);
            (columns[0], inset(columns[1], 2, 0))
        } else {
            let compact = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(6), Constraint::Length(9)])
                .split(rows[1]);
            (compact[0], compact[1])
        };
        let items = if self.threads.is_empty() {
            vec![ListItem::new(Text::from(vec![
                Line::styled("No managed Codex threads", Style::default().fg(INK)),
                Line::styled(
                    "press n or open Lumbergh from Home",
                    Style::default().fg(MUTED),
                ),
            ]))]
        } else {
            self.threads
                .iter()
                .map(|thread| {
                    ListItem::new(Text::from(vec![
                        Line::from(vec![
                            Span::styled("◆ ", Style::default().fg(CYAN)),
                            Span::styled(&thread.name, Style::default().fg(INK)),
                            Span::styled(
                                format!("  {}", thread.status.to_uppercase()),
                                Style::default().fg(GREEN),
                            ),
                        ]),
                        Line::from(vec![
                            Span::styled("  ", Style::default()),
                            Span::styled(&thread.account_alias, Style::default().fg(AMBER)),
                            Span::styled(" · ", Style::default().fg(MUTED)),
                            Span::styled(&thread.model, Style::default().fg(MUTED)),
                            Span::styled(" · ", Style::default().fg(MUTED)),
                            Span::styled(short_id(&thread.id), Style::default().fg(MUTED)),
                        ]),
                    ]))
                })
                .collect()
        };
        render_section_list(
            frame,
            list_area,
            format!("THREADS  {}", self.threads.len()),
            items,
            &mut self.thread_state,
            true,
            Borders::RIGHT,
        );
        let detail = self.selected_thread().map_or_else(
            || {
                vec![Line::styled(
                    "Select a thread to inspect it",
                    Style::default().fg(MUTED),
                )]
            },
            |thread| {
                vec![
                    detail_line("status", &thread.status),
                    detail_line("model", &thread.model),
                    detail_line("provider", &thread.model_provider),
                    detail_line(
                        "reasoning",
                        thread.reasoning_effort.as_deref().unwrap_or("default"),
                    ),
                    detail_line("account", &thread.account_alias),
                    detail_line("cwd", &thread.cwd.to_string_lossy()),
                    detail_line("native", &short_id(&thread.native_thread_id)),
                    Line::styled("", Style::default()),
                    Line::styled("Enter  open or resume", Style::default().fg(CYAN)),
                ]
            },
        );
        frame.render_widget(
            Paragraph::new(detail)
                .block(Block::default().title(section_title("THREAD DETAIL")))
                .wrap(Wrap { trim: true }),
            detail_area,
        );
    }

    fn render_wips(&self, frame: &mut Frame<'_>, area: Rect) {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(3), Constraint::Min(5)])
            .split(area);
        frame.render_widget(
            view_heading(
                "WIPs & schedules",
                "Recurring and on-demand jobs, runs, logs, and artifacts.",
            ),
            rows[0],
        );
        let content = inset(rows[1], 2, 1);
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(vec![
                    Span::styled(
                        "RUNTIME NOT CONNECTED",
                        Style::default().fg(AMBER).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled("  ·  next implementation slice", Style::default().fg(MUTED)),
                ]),
                Line::raw(""),
                Line::styled(
                    "This view is reserved for the Wiphand scheduler transplant; it is not fake sample data.",
                    Style::default().fg(INK),
                ),
                Line::raw(""),
                Line::styled("What lands here", Style::default().fg(CYAN).add_modifier(Modifier::BOLD)),
                Line::styled("  • schedules, next run, overlap warnings, pause/resume", Style::default().fg(INK)),
                Line::styled("  • running/failed executions with stable Wipsaw IDs", Style::default().fg(INK)),
                Line::styled("  • Docker and Codex logs without silently resuming a session", Style::default().fg(INK)),
                Line::styled("  • model, account, host, artifacts, and run overrides", Style::default().fg(INK)),
                Line::raw(""),
                Line::styled(
                    "m  Talk to Lumbergh now about preparing a WIP or organizing the workspace.",
                    Style::default().fg(CYAN),
                ),
            ])
            .block(
                Block::default()
                    .borders(Borders::LEFT)
                    .border_style(Style::default().fg(AMBER)),
            )
            .wrap(Wrap { trim: true }),
            content,
        );
    }

    fn thread_for_tab(&self, tab: &Tab) -> Option<&CodexThread> {
        tab.codex_thread_id
            .as_deref()
            .and_then(|id| self.threads.iter().find(|thread| thread.id == id))
    }

    fn tab_live_state(&self, tab: &Tab) -> &'static str {
        match self
            .windows
            .iter()
            .find(|window| window.id == tab.tmux_window_id)
        {
            Some(window) if window.active => "active",
            Some(_) => "idle",
            None => "stopped",
        }
    }

    fn render_footer(&self, frame: &mut Frame<'_>, area: Rect) {
        let (status, is_error) = self.message.clone().unwrap_or_else(|| {
            let text = match self.view {
                View::Home => "Enter writes to Lumbergh · c creates a workspace".to_string(),
                View::Sessions if self.active == Panel::Workspaces => self
                    .selected_workspace()
                    .map(|workspace| {
                        format!("Enter attaches '{}' · l focuses its tabs", workspace.name)
                    })
                    .unwrap_or_else(|| "c creates a workspace".to_string()),
                View::Sessions => self
                    .selected_tab()
                    .map(|tab| {
                        if is_manager_tab(tab) {
                            "Enter talks to this workspace's Middle Manager".to_string()
                        } else {
                            format!("Enter opens '{}' · , renames it", tab.name)
                        }
                    })
                    .unwrap_or_else(|| "t creates a shell · n creates Codex".to_string()),
                View::Threads => self
                    .selected_thread()
                    .map(|thread| format!("Enter opens or resumes '{}'", thread.name))
                    .unwrap_or_else(|| "n creates a named Codex session".to_string()),
                View::Wips => "WIP runtime is next · m opens Lumbergh now".to_string(),
            };
            (text, false)
        });
        let keys = if self.manager.focused {
            "←→↑↓ edit   Enter send   Ctrl+J newline   PgUp/PgDn history   @ files   $ skills"
        } else if self.manager.overlay || self.view == View::Home {
            "Enter compose   v copy mode   y latest   Y transcript   ↑↓ scroll   ? guide"
        } else if self.prefix_pending {
            "PREFIX C-b · w home  s sessions  t threads  g WIPs  m Lumbergh  n/p views"
        } else if area.width < 92 {
            "↑↓ move  Enter open  c workspace  n Codex  m Lumbergh  ? guide"
        } else {
            "↑↓ move   Enter open   c workspace   n Codex   t shell   m Lumbergh   / commands   ? guide"
        };
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(vec![
                    Span::styled(
                        "STATUS › ",
                        Style::default()
                            .fg(if is_error { RED } else { CYAN })
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        status,
                        Style::default().fg(if is_error { RED } else { INK }),
                    ),
                ]),
                Line::styled(
                    keys,
                    Style::default().fg(if self.prefix_pending { AMBER } else { MUTED }),
                ),
            ])
            .style(Style::default().bg(DEEP)),
            area,
        );
    }

    fn render_help(&self, frame: &mut Frame<'_>, area: Rect) {
        let popup = centered_rect(area, 88, 34);
        frame.render_widget(Clear, popup);
        let help = Text::from(vec![
            Line::styled(
                "START HERE",
                Style::default().fg(CYAN).add_modifier(Modifier::BOLD),
            ),
            Line::from(
                "  1. Lumbergh is always embedded on Home. Press Enter and describe what you need.",
            ),
            Line::from(
                "  2. Each workspace also has a Middle Manager; open its manager tab from Sessions.",
            ),
            Line::from("  3. From any managed tab, press Ctrl-b w to return to this dashboard."),
            Line::from("  Stopped workspaces are rebuilt automatically when you open them."),
            Line::raw(""),
            Line::styled("DASHBOARD", Style::default().fg(AMBER)),
            Line::from("  1 / 2 / 3 / 4  Home / Sessions / Threads / WIPs"),
            Line::from("  Tab / Shift-Tab next / previous view"),
            Line::from("  j k / arrows   move selection · h l changes workspace/tab focus"),
            Line::from("  Enter          primary action for the current view"),
            Line::from("  c              new workspace (or create in focused Sessions list)"),
            Line::from("  n / t          new Codex session / shell tab"),
            Line::from("  m              talk to Lumbergh from anywhere"),
            Line::from("  v              manager-only selection view for native terminal copy"),
            Line::from("  y / Y          copy latest manager response / full transcript"),
            Line::from("  ,              rename a focused tab · r refreshes live state"),
            Line::raw(""),
            Line::styled("MANAGER COMPOSER", Style::default().fg(AMBER)),
            Line::from("  Enter          send · Shift+Enter or Ctrl+J inserts a newline"),
            Line::from("  arrows         move the caret left/right and between prompt lines"),
            Line::from("  Home/End       start/end of line · Ctrl-Left/Right moves by word"),
            Line::from("  PgUp/PgDn      scroll conversation while keeping the prompt editable"),
            Line::from("  mouse wheel    scroll conversation · click composer to place caret"),
            Line::from("  @ / $          find a scoped file / available manager skill"),
            Line::from("  Ctrl+O         open selection view · Ctrl+Y copies latest response"),
            Line::from("  paste          preserves multiple lines"),
            Line::raw(""),
            Line::styled("TMUX-FAMILIAR PREFIX", Style::default().fg(AMBER)),
            Line::from("  C-b w          toggle this navigator"),
            Line::from("  C-b c / ,      create / rename a managed tab"),
            Line::from("  C-b m          open this workspace's Middle Manager from a terminal"),
            Line::from("  C-b n / p      next / previous tmux tab outside this popup"),
            Line::raw(""),
            Line::styled(
                "SHELL SHORTCUTS  codex · manager · lumberg · lumbergh",
                Style::default().fg(MUTED),
            ),
        ]);
        frame.render_widget(
            Paragraph::new(help)
                .block(
                    Block::default()
                        .title(" ? HELP · Esc closes ")
                        .borders(Borders::ALL)
                        .border_type(BorderType::Double)
                        .border_style(Style::default().fg(CYAN))
                        .style(Style::default().bg(PANEL).fg(INK)),
                )
                .wrap(Wrap { trim: false }),
            popup,
        );
    }

    fn render_prompt(&self, frame: &mut Frame<'_>, area: Rect) {
        let Some(prompt) = &self.prompt else {
            return;
        };
        let popup = centered_rect(area, 72, 7);
        frame.render_widget(Clear, popup);
        let text = vec![
            Line::styled(prompt.label(), Style::default().fg(MUTED)),
            Line::raw(""),
            Line::from(vec![
                Span::styled("› ", Style::default().fg(AMBER)),
                Span::styled(&prompt.value, Style::default().fg(INK)),
                Span::styled("█", Style::default().fg(CYAN)),
            ]),
        ];
        frame.render_widget(
            Paragraph::new(text).block(
                Block::default()
                    .title(" COMMAND · Enter confirms · Esc cancels ")
                    .borders(Borders::ALL)
                    .border_type(BorderType::Double)
                    .border_style(Style::default().fg(AMBER))
                    .style(Style::default().bg(PANEL)),
            ),
            popup,
        );
    }
}

struct Prompt {
    kind: PromptKind,
    value: String,
}

impl Prompt {
    fn new(kind: PromptKind) -> Self {
        Self {
            kind,
            value: String::new(),
        }
    }

    fn label(&self) -> &'static str {
        match self.kind {
            PromptKind::Workspace => {
                "Workspace name · its Middle Manager starts in the current directory"
            }
            PromptKind::Tab {
                start_codex: false, ..
            } => "Shell tab name · inherits the selected workspace directory",
            PromptKind::Tab {
                start_codex: true, ..
            } => "Codex tab/thread name · uses the preferred Codex home",
            PromptKind::RenameTab { .. } => "New tab name",
        }
    }
}

enum PromptKind {
    Workspace,
    Tab {
        workspace_id: String,
        start_codex: bool,
    },
    RenameTab {
        workspace_id: String,
        tab_id: String,
    },
}

enum Action {
    None,
    Quit,
    Refresh,
    CreateWorkspace {
        name: String,
    },
    CreateTab {
        workspace_id: String,
        name: String,
        start_codex: bool,
    },
    RenameTab {
        workspace_id: String,
        tab_id: String,
        name: String,
    },
    ActivateWorkspace(String),
    ActivateTab {
        workspace_id: String,
        tab_id: String,
    },
    OpenThread {
        thread_id: String,
        workspace_id: Option<String>,
    },
    OpenLumbergh,
    OpenMiddleManager {
        workspace_id: String,
    },
    CloseManager,
    FocusManager,
    SendManagerMessage {
        workspace_id: Option<String>,
        prompt: String,
    },
    CopyManager {
        text: String,
        description: &'static str,
    },
}

fn is_manager_tab(tab: &Tab) -> bool {
    tab.name.eq_ignore_ascii_case("middle-manager") || tab.name.eq_ignore_ascii_case("manager")
}

fn manager_reference_catalog(scope: &ManagerContextScope) -> Vec<ManagerReference> {
    const MAX_FILES: usize = 20_000;
    const MAX_DEPTH: usize = 8;
    const SKIPPED_DIRECTORIES: &[&str] = &[
        ".git",
        ".cache",
        ".aws",
        ".docker",
        ".gnupg",
        ".next",
        ".ssh",
        ".venv",
        "build",
        "coverage",
        "dist",
        "node_modules",
        "target",
        "venv",
    ];

    let mut references = MANAGER_SKILL_NAMES
        .into_iter()
        .map(|name| ManagerReference {
            kind: ManagerReferenceKind::Skill,
            label: name.to_string(),
            hint: Some(
                match name {
                    "wipsaw-manager" => "manage Wipsaw",
                    "skill-creator" => "create or revise a skill",
                    "skill-installer" => "look up or install skills",
                    _ => "manager skill",
                }
                .to_string(),
            ),
            directory: false,
        })
        .collect::<Vec<_>>();
    let mut files = Vec::new();
    let catalog_roots = if scope.machine_wide {
        vec![scope.cwd.clone()]
    } else {
        scope.roots.clone()
    };
    let mut queue = VecDeque::new();
    for root in &catalog_roots {
        if root.is_file() {
            let label = manager_reference_label(scope, root, false);
            if !sensitive_reference(&label) {
                files.push(label);
            }
        } else if root.is_dir() {
            queue.push_back((root.clone(), root.clone(), 0_usize));
        }
    }
    while let Some((root, directory, depth)) = queue.pop_front() {
        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };
        let mut entries = entries
            .filter_map(std::result::Result::ok)
            .collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                if depth < MAX_DEPTH && !SKIPPED_DIRECTORIES.contains(&name.as_ref()) {
                    queue.push_back((root.clone(), entry.path(), depth + 1));
                }
                continue;
            }
            if !file_type.is_file() || sensitive_reference(&name) {
                continue;
            }
            files.push(manager_reference_label(scope, &entry.path(), false));
            if files.len() >= MAX_FILES {
                queue.clear();
                break;
            }
        }
    }
    files.sort_by_key(|path| path.to_ascii_lowercase());
    references.extend(files.into_iter().map(|label| ManagerReference {
        kind: ManagerReferenceKind::File,
        label,
        hint: None,
        directory: false,
    }));
    references
}

fn manager_path_reference_matches(
    scope: &ManagerContextScope,
    query: &str,
) -> Vec<ManagerReference> {
    let query = query.strip_prefix('{').unwrap_or(query);
    let (directory, prefix) = if query.is_empty() {
        (None, "")
    } else if query.ends_with('/') {
        (Some(query), "")
    } else {
        let path = Path::new(query);
        let parent = path
            .parent()
            .and_then(Path::to_str)
            .filter(|value| !value.is_empty());
        let prefix = path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or(query);
        (parent, prefix)
    };
    let Ok((entries, _)) = list_manager_directory(scope, directory, 20_000) else {
        return Vec::new();
    };
    let prefix = prefix.to_ascii_lowercase();
    entries
        .into_iter()
        .filter(|entry| {
            entry
                .path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.to_ascii_lowercase().contains(&prefix))
                || prefix.is_empty()
        })
        .map(|entry| {
            let directory = entry.kind == "directory";
            ManagerReference {
                kind: ManagerReferenceKind::File,
                label: manager_reference_label(scope, &entry.path, directory),
                hint: Some(entry.kind),
                directory,
            }
        })
        .collect()
}

fn manager_reference_label(scope: &ManagerContextScope, path: &Path, directory: bool) -> String {
    let path = if !scope.machine_wide {
        path.strip_prefix(&scope.cwd).unwrap_or(path)
    } else {
        path
    };
    let mut label = path.to_string_lossy().into_owned();
    if label.is_empty() {
        label.push('.');
    }
    if directory && !label.ends_with('/') {
        label.push('/');
    }
    label
}

fn rect_contains(area: Rect, column: u16, row: u16) -> bool {
    column >= area.x
        && column < area.x.saturating_add(area.width)
        && row >= area.y
        && row < area.y.saturating_add(area.height)
}

fn clamp_char_boundary(value: &str, requested: usize) -> usize {
    let mut cursor = requested.min(value.len());
    while !value.is_char_boundary(cursor) {
        cursor = cursor.saturating_sub(1);
    }
    cursor
}

fn previous_char_boundary(value: &str, cursor: usize) -> usize {
    let cursor = clamp_char_boundary(value, cursor);
    value[..cursor]
        .char_indices()
        .next_back()
        .map(|(index, _)| index)
        .unwrap_or(0)
}

fn next_char_boundary(value: &str, cursor: usize) -> usize {
    let cursor = clamp_char_boundary(value, cursor);
    value[cursor..]
        .chars()
        .next()
        .map(|character| cursor + character.len_utf8())
        .unwrap_or(value.len())
}

fn current_line_start(value: &str, cursor: usize) -> usize {
    let cursor = clamp_char_boundary(value, cursor);
    value[..cursor]
        .rfind('\n')
        .map(|index| index + 1)
        .unwrap_or(0)
}

fn current_line_end(value: &str, cursor: usize) -> usize {
    let cursor = clamp_char_boundary(value, cursor);
    value[cursor..]
        .find('\n')
        .map(|offset| cursor + offset)
        .unwrap_or(value.len())
}

fn byte_at_character_column(value: &str, start: usize, end: usize, column: usize) -> usize {
    value[start..end]
        .char_indices()
        .nth(column)
        .map(|(offset, _)| start + offset)
        .unwrap_or(end)
}

fn move_cursor_vertically(value: &str, cursor: usize, width: usize, down: bool) -> usize {
    let cursor = clamp_char_boundary(value, cursor);
    let width = if width == 0 { 80 } else { width };
    let rows = editor_rows(value, width);
    let row_index = rows
        .iter()
        .enumerate()
        .rev()
        .find(|(_, row)| cursor >= row.start && cursor <= row.end)
        .map(|(index, _)| index)
        .unwrap_or(0);
    let row = rows[row_index];
    let column = value[row.start..cursor.min(row.end)].chars().count();
    let target = if down {
        rows.get(row_index + 1)
    } else {
        row_index.checked_sub(1).and_then(|index| rows.get(index))
    };
    target
        .map(|target| byte_at_character_column(value, target.start, target.end, column))
        .unwrap_or(cursor)
}

fn is_word_character(character: char) -> bool {
    character.is_alphanumeric() || character == '_'
}

fn previous_word_boundary(value: &str, cursor: usize) -> usize {
    let mut cursor = clamp_char_boundary(value, cursor);
    while cursor > 0 {
        let previous = previous_char_boundary(value, cursor);
        let character = value[previous..cursor].chars().next().unwrap_or_default();
        if is_word_character(character) {
            break;
        }
        cursor = previous;
    }
    while cursor > 0 {
        let previous = previous_char_boundary(value, cursor);
        let character = value[previous..cursor].chars().next().unwrap_or_default();
        if !is_word_character(character) {
            break;
        }
        cursor = previous;
    }
    cursor
}

fn next_word_boundary(value: &str, cursor: usize) -> usize {
    let mut cursor = clamp_char_boundary(value, cursor);
    while cursor < value.len() {
        let next = next_char_boundary(value, cursor);
        let character = value[cursor..next].chars().next().unwrap_or_default();
        if !is_word_character(character) {
            break;
        }
        cursor = next;
    }
    while cursor < value.len() {
        let next = next_char_boundary(value, cursor);
        let character = value[cursor..next].chars().next().unwrap_or_default();
        if is_word_character(character) {
            break;
        }
        cursor = next;
    }
    cursor
}

#[derive(Debug, Clone, Copy)]
struct EditorRow {
    start: usize,
    end: usize,
}

fn editor_rows(value: &str, width: usize) -> Vec<EditorRow> {
    let width = width.max(1);
    let mut rows = Vec::new();
    let mut start = 0;
    let mut columns = 0;
    for (index, character) in value.char_indices() {
        if character == '\n' {
            rows.push(EditorRow { start, end: index });
            start = index + character.len_utf8();
            columns = 0;
            continue;
        }
        if columns == width {
            rows.push(EditorRow { start, end: index });
            start = index;
            columns = 0;
        }
        columns += 1;
    }
    rows.push(EditorRow {
        start,
        end: value.len(),
    });
    rows
}

fn editor_cursor_from_position(
    value: &str,
    width: usize,
    visual_row: usize,
    visual_column: usize,
) -> usize {
    let rows = editor_rows(value, width);
    let Some(row) = rows.get(visual_row) else {
        return value.len();
    };
    byte_at_character_column(value, row.start, row.end, visual_column)
}

fn active_manager_reference(value: &str) -> Option<(ManagerReferenceKind, usize, &str)> {
    if let Some(start) = value.rfind("@{")
        && !value[start + 2..].contains('}')
    {
        return Some((ManagerReferenceKind::File, start, &value[start + 2..]));
    }
    let start = value
        .rfind(char::is_whitespace)
        .map(|index| {
            index
                + value[index..]
                    .chars()
                    .next()
                    .map(char::len_utf8)
                    .unwrap_or(0)
        })
        .unwrap_or(0);
    let token = &value[start..];
    let (kind, query) = if let Some(query) = token.strip_prefix('@') {
        (ManagerReferenceKind::File, query)
    } else if let Some(query) = token.strip_prefix('$') {
        (ManagerReferenceKind::Skill, query)
    } else {
        return None;
    };
    if query.contains(['{', '}']) {
        return None;
    }
    Some((kind, start, query))
}

fn base64_encode(value: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::with_capacity(value.len().div_ceil(3) * 4);
    for chunk in value.chunks(3) {
        let first = chunk[0];
        let second = chunk.get(1).copied().unwrap_or(0);
        let third = chunk.get(2).copied().unwrap_or(0);
        encoded.push(ALPHABET[(first >> 2) as usize] as char);
        encoded.push(ALPHABET[(((first & 0x03) << 4) | (second >> 4)) as usize] as char);
        encoded.push(if chunk.len() > 1 {
            ALPHABET[(((second & 0x0f) << 2) | (third >> 6)) as usize] as char
        } else {
            '='
        });
        encoded.push(if chunk.len() > 2 {
            ALPHABET[(third & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    encoded
}

fn manager_transcript_lines(
    messages: &[ManagerMessage],
    progress: &[ManagerProgress],
    manager_label: &str,
    width: usize,
) -> Vec<Line<'static>> {
    if messages.is_empty() && progress.is_empty() {
        return vec![
            Line::styled(
                format!("{manager_label} IS READY"),
                Style::default().fg(CYAN).add_modifier(Modifier::BOLD),
            ),
            Line::raw(""),
            Line::styled(
                if manager_label == "LUMBERGH" {
                    "This is Wipsaw's one top manager. Ask for a workspace, Codex session, model setup, account, or WIP."
                } else {
                    "This manager owns the selected workspace. Ask it to organize sessions, choose models, inspect state, or prepare work."
                },
                Style::default().fg(INK),
            ),
            Line::raw(""),
            Line::styled(
                "Loaded skills: $wipsaw-manager, $skill-creator, and $skill-installer (skill lookup/install). Private Wipsaw MCP tools are enabled; shell, personal MCPs, plugins, apps, and unrelated skills stay outside this session.",
                Style::default().fg(MUTED),
            ),
            Line::raw(""),
            Line::styled(
                if manager_label == "LUMBERGH" {
                    "File scope: machine-wide read access (credential paths are blocked). Type @/ to browse from the filesystem root."
                } else {
                    "File scope: only this workspace's explicit files/directories. Ask Lumbergh to add context when needed."
                },
                Style::default().fg(MUTED),
            ),
        ];
    }
    let mut lines = Vec::new();
    let progress_after = messages.iter().rposition(|message| message.role == "user");
    for (index, message) in messages.iter().enumerate() {
        let (label, color) = match message.role.as_str() {
            "user" => ("YOU", AMBER),
            "assistant" => (manager_label, CYAN),
            _ => ("WIPSAW", RED),
        };
        lines.push(Line::styled(
            label.to_string(),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ));
        for line in wrap_text(&message.content, width.saturating_sub(2).max(8)) {
            lines.push(Line::styled(format!("  {line}"), Style::default().fg(INK)));
        }
        lines.push(Line::raw(""));
        if progress_after == Some(index) {
            append_manager_progress_lines(&mut lines, progress, width);
        }
    }
    if progress_after.is_none() {
        append_manager_progress_lines(&mut lines, progress, width);
    }
    lines
}

fn manager_scroll_position(line_count: usize, viewport_height: u16, scroll_back: u16) -> u16 {
    let max_scroll = line_count
        .saturating_sub(viewport_height as usize)
        .min(u16::MAX as usize) as u16;
    max_scroll.saturating_sub(scroll_back.min(max_scroll))
}

fn manager_progress_changes_inventory(progress: &ManagerProgress) -> bool {
    progress.status == ManagerProgressStatus::Completed
        && [
            "wipsaw/run_wipsaw",
            "wipsaw/create_codex_tab",
            "wipsaw/start_codex_session",
            "wipsaw/import_codex_session",
            "wipsaw/create_handoff_tab",
        ]
        .iter()
        .any(|tool| progress.label.contains(tool))
}

fn append_manager_progress_lines(
    lines: &mut Vec<Line<'static>>,
    progress: &[ManagerProgress],
    width: usize,
) {
    if progress.is_empty() {
        return;
    }
    lines.push(Line::styled(
        "CODEX",
        Style::default().fg(STEEL).add_modifier(Modifier::BOLD),
    ));
    for item in progress {
        let (symbol, color) = match item.status {
            ManagerProgressStatus::Running => ("•", AMBER),
            ManagerProgressStatus::Completed => ("✓", GREEN),
            ManagerProgressStatus::Failed => ("×", RED),
        };
        lines.push(Line::from(vec![
            Span::styled(format!("  {symbol} "), Style::default().fg(color)),
            Span::styled(item.label.clone(), Style::default().fg(INK)),
        ]));
        if let Some(detail) = &item.detail {
            for detail_line in wrap_text(detail, width.saturating_sub(6).max(8)) {
                lines.push(Line::styled(
                    format!("      {detail_line}"),
                    Style::default().fg(MUTED),
                ));
            }
        }
    }
    lines.push(Line::raw(""));
}

fn manager_transcript_text(
    messages: &[ManagerMessage],
    progress: &[ManagerProgress],
    manager_label: &str,
) -> String {
    let progress_after = messages.iter().rposition(|message| message.role == "user");
    let progress_text = || {
        let items = progress
            .iter()
            .map(|item| {
                let symbol = match item.status {
                    ManagerProgressStatus::Running => "•",
                    ManagerProgressStatus::Completed => "✓",
                    ManagerProgressStatus::Failed => "×",
                };
                item.detail.as_ref().map_or_else(
                    || format!("{symbol} {}", item.label),
                    |detail| format!("{symbol} {}\n  {detail}", item.label),
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        (!items.is_empty()).then(|| format!("CODEX\n{items}"))
    };
    let mut blocks = Vec::new();
    for (index, message) in messages.iter().enumerate() {
        let speaker = match message.role.as_str() {
            "user" => "YOU",
            "assistant" => manager_label,
            _ => "WIPSAW",
        };
        blocks.push(format!("{speaker}\n{}", message.content));
        if progress_after == Some(index)
            && let Some(progress) = progress_text()
        {
            blocks.push(progress);
        }
    }
    if progress_after.is_none()
        && let Some(progress) = progress_text()
    {
        blocks.push(progress);
    }
    blocks.join("\n\n")
}

fn manager_copy_lines(value: &str, width: usize) -> Vec<ManagerCopyLine> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut logical_start = 0;
    loop {
        let newline = value[logical_start..]
            .find('\n')
            .map(|offset| logical_start + offset);
        let logical_end = newline.unwrap_or(value.len());
        let logical = &value[logical_start..logical_end];
        lines.extend(
            editor_rows(logical, width)
                .into_iter()
                .map(|row| ManagerCopyLine {
                    start: logical_start + row.start,
                    end: logical_start + row.end,
                }),
        );
        let Some(newline) = newline else {
            break;
        };
        logical_start = newline + 1;
        if logical_start > value.len() {
            break;
        }
    }
    if lines.is_empty() {
        lines.push(ManagerCopyLine { start: 0, end: 0 });
    }
    lines
}

fn manager_copy_line_at(lines: &[ManagerCopyLine], offset: usize) -> usize {
    lines
        .iter()
        .enumerate()
        .rev()
        .find(|(_, line)| offset >= line.start && offset <= line.end)
        .map(|(index, _)| index)
        .unwrap_or_else(|| lines.len().saturating_sub(1))
}

fn manager_copy_cell_from_mouse(
    document: &str,
    lines: &[ManagerCopyLine],
    top: usize,
    area: Rect,
    column: u16,
    row: u16,
) -> (usize, usize) {
    let visual_row = row
        .saturating_sub(area.y)
        .min(area.height.saturating_sub(1)) as usize;
    let line = lines[(top + visual_row).min(lines.len().saturating_sub(1))];
    let visual_column = column
        .saturating_sub(area.x)
        .min(area.width.saturating_sub(1)) as usize;
    let start = byte_at_character_column(document, line.start, line.end, visual_column);
    let end = next_char_boundary(document, start).min(line.end);
    (start, end)
}

fn render_manager_copy_line(
    document: &str,
    line: ManagerCopyLine,
    cursor_line: bool,
    cursor: usize,
    selection: Option<(usize, usize)>,
) -> Line<'static> {
    let source = &document[line.start..line.end];
    let color = match source {
        "YOU" => AMBER,
        "LUMBERGH" | "MIDDLE MANAGER" => CYAN,
        "CODEX" => STEEL,
        "WIPSAW" => RED,
        _ => INK,
    };
    let normal = Style::default().fg(color);
    let selected = Style::default()
        .fg(DEEP)
        .bg(CYAN)
        .add_modifier(Modifier::BOLD);
    let cursor_style = Style::default().fg(DEEP).bg(AMBER);

    if let Some((selection_start, selection_end)) = selection {
        let start = selection_start.max(line.start).min(line.end);
        let end = selection_end.max(line.start).min(line.end);
        if start < end {
            return Line::from(vec![
                Span::styled(document[line.start..start].to_string(), normal),
                Span::styled(document[start..end].to_string(), selected),
                Span::styled(document[end..line.end].to_string(), normal),
            ]);
        }
        if source.is_empty() && selection_start <= line.start && selection_end > line.start {
            return Line::from(Span::styled(" ".to_string(), selected));
        }
    }

    if cursor_line {
        let cursor = clamp_char_boundary(document, cursor.clamp(line.start, line.end));
        if cursor < line.end {
            let next = next_char_boundary(document, cursor).min(line.end);
            return Line::from(vec![
                Span::styled(document[line.start..cursor].to_string(), normal),
                Span::styled(document[cursor..next].to_string(), cursor_style),
                Span::styled(document[next..line.end].to_string(), normal),
            ]);
        }
        return Line::from(vec![
            Span::styled(source.to_string(), normal),
            Span::styled(" ".to_string(), cursor_style),
        ]);
    }
    Line::from(Span::styled(source.to_string(), normal))
}

fn manager_editor_lines(
    value: &str,
    cursor: usize,
    width: usize,
    show_cursor: bool,
) -> (Vec<Line<'static>>, usize) {
    let content_width = width.saturating_sub(2).max(1);
    let cursor = clamp_char_boundary(value, cursor);
    let mut rows = editor_rows(value, content_width);
    if cursor == value.len()
        && rows
            .last()
            .is_some_and(|row| value[row.start..row.end].chars().count() == content_width)
    {
        rows.push(EditorRow {
            start: value.len(),
            end: value.len(),
        });
    }
    let cursor_row = rows
        .iter()
        .enumerate()
        .rev()
        .find(|(_, row)| cursor >= row.start && cursor <= row.end)
        .map(|(index, _)| index)
        .unwrap_or(0);
    let lines = rows
        .into_iter()
        .enumerate()
        .map(|(index, row)| {
            let mut spans = vec![Span::styled(
                if index == 0 { "λ " } else { "  " },
                Style::default().fg(CYAN).add_modifier(Modifier::BOLD),
            )];
            if show_cursor && index == cursor_row {
                let split = cursor.clamp(row.start, row.end);
                spans.push(Span::styled(
                    value[row.start..split].to_string(),
                    Style::default().fg(INK),
                ));
                if split < row.end {
                    let next = next_char_boundary(value, split).min(row.end);
                    spans.push(Span::styled(
                        value[split..next].to_string(),
                        Style::default()
                            .fg(DEEP)
                            .bg(CYAN)
                            .add_modifier(Modifier::BOLD),
                    ));
                    spans.push(Span::styled(
                        value[next..row.end].to_string(),
                        Style::default().fg(INK),
                    ));
                } else {
                    spans.push(Span::styled(
                        " ",
                        Style::default()
                            .fg(DEEP)
                            .bg(CYAN)
                            .add_modifier(Modifier::BOLD),
                    ));
                }
            } else {
                spans.push(Span::styled(
                    value[row.start..row.end].to_string(),
                    Style::default().fg(INK),
                ));
            }
            Line::from(spans)
        })
        .collect();
    (lines, cursor_row)
}

#[cfg(test)]
fn wrap_editor_text(value: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut wrapped = Vec::new();
    for source in value.split('\n') {
        if source.is_empty() {
            wrapped.push(String::new());
            continue;
        }
        let characters = source.chars().collect::<Vec<_>>();
        wrapped.extend(
            characters
                .chunks(width)
                .map(|chunk| chunk.iter().collect::<String>()),
        );
    }
    if wrapped.is_empty() {
        wrapped.push(String::new());
    }
    wrapped
}

fn wrap_text(value: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    for source_line in value.lines() {
        if source_line.is_empty() {
            lines.push(String::new());
            continue;
        }
        let mut line = String::new();
        for source_word in source_line.split_whitespace() {
            let mut word = source_word.to_string();
            loop {
                let separator = usize::from(!line.is_empty());
                let available = width.saturating_sub(line.chars().count() + separator);
                let word_len = word.chars().count();
                if word_len <= available {
                    if !line.is_empty() {
                        line.push(' ');
                    }
                    line.push_str(&word);
                    break;
                }
                if !line.is_empty() {
                    lines.push(std::mem::take(&mut line));
                    continue;
                }
                let chunk = word.chars().take(width).collect::<String>();
                word = word.chars().skip(width).collect();
                lines.push(chunk);
                if word.is_empty() {
                    break;
                }
            }
        }
        if !line.is_empty() {
            lines.push(line);
        }
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

fn inset(area: Rect, horizontal: u16, vertical: u16) -> Rect {
    Rect {
        x: area.x.saturating_add(horizontal.min(area.width / 2)),
        y: area.y.saturating_add(vertical.min(area.height / 2)),
        width: area.width.saturating_sub(horizontal.saturating_mul(2)),
        height: area.height.saturating_sub(vertical.saturating_mul(2)),
    }
}

fn section_title(title: &str) -> Span<'static> {
    Span::styled(
        format!(" {title} "),
        Style::default().fg(MUTED).add_modifier(Modifier::BOLD),
    )
}

fn metric_line(value: usize, label: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{value:>3} "),
            Style::default().fg(INK).add_modifier(Modifier::BOLD),
        ),
        Span::styled(label.to_string(), Style::default().fg(MUTED)),
    ])
}

fn health_line(label: &str, healthy: bool) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            if healthy { "● " } else { "○ " },
            Style::default().fg(if healthy { GREEN } else { RED }),
        ),
        Span::styled(label.to_string(), Style::default().fg(INK)),
        Span::styled(
            if healthy { "  ready" } else { "  unavailable" },
            Style::default().fg(if healthy { GREEN } else { MUTED }),
        ),
    ])
}

fn shortcut_span(key: &str, label: &str) -> Span<'static> {
    Span::styled(
        format!("[{key}] {label}"),
        Style::default().fg(CYAN).add_modifier(Modifier::BOLD),
    )
}

fn view_heading(title: &str, subtitle: &str) -> Paragraph<'static> {
    Paragraph::new(vec![
        Line::styled(
            title.to_string(),
            Style::default().fg(INK).add_modifier(Modifier::BOLD),
        ),
        Line::styled(subtitle.to_string(), Style::default().fg(MUTED)),
    ])
}

fn detail_line(label: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label:<10}"), Style::default().fg(MUTED)),
        Span::styled(value.to_string(), Style::default().fg(INK)),
    ])
}

fn short_id(value: &str) -> String {
    const LIMIT: usize = 18;
    if value.chars().count() <= LIMIT {
        value.to_string()
    } else {
        format!("{}…", value.chars().take(LIMIT).collect::<String>())
    }
}

fn render_section_list<'a>(
    frame: &mut Frame<'_>,
    area: Rect,
    title: String,
    items: Vec<ListItem<'a>>,
    state: &mut ListState,
    active: bool,
    extra_borders: Borders,
) {
    let border_color = if active { CYAN } else { BORDER };
    let block = Block::default()
        .title(Span::styled(
            title,
            Style::default()
                .fg(if active { CYAN } else { MUTED })
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::TOP | extra_borders)
        .border_style(Style::default().fg(border_color))
        .style(Style::default().bg(DEEP));
    let list = List::new(items)
        .block(block)
        .highlight_symbol("› ")
        .highlight_style(
            Style::default()
                .bg(PANEL)
                .fg(INK)
                .add_modifier(Modifier::BOLD),
        );
    frame.render_stateful_widget(list, area, state);
}

fn move_state(state: &mut ListState, len: usize, delta: isize) {
    if len == 0 {
        state.select(None);
        return;
    }
    let current = state.selected().unwrap_or(0);
    let next = if delta.is_negative() {
        current.saturating_sub(delta.unsigned_abs())
    } else {
        current.saturating_add(delta as usize).min(len - 1)
    };
    state.select(Some(next));
}

fn select_id(state: &mut ListState, selected: Option<usize>, len: usize) {
    state.select(selected.or_else(|| (len > 0).then_some(0)));
}

fn ensure_selection(state: &mut ListState, len: usize) {
    if state.selected().is_none_or(|index| index >= len) {
        state.select((len > 0).then_some(0));
    }
}

fn centered_rect(area: Rect, requested_width: u16, requested_height: u16) -> Rect {
    let width = requested_width.min(area.width.saturating_sub(2)).max(1);
    let height = requested_height.min(area.height.saturating_sub(2)).max(1);
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use tempfile::tempdir;

    use super::{
        Action, Navigator, active_manager_reference, base64_encode, editor_cursor_from_position,
        manager_copy_lines, manager_progress_changes_inventory, manager_reference_catalog,
        manager_scroll_position, wrap_editor_text, wrap_text,
    };
    use crate::manager::{ManagerContextScope, ManagerProgress, ManagerProgressStatus};
    use crate::model::ManagerMessage;

    #[test]
    fn transcript_wraps_unbroken_ids_without_overflowing() {
        let lines = wrap_text("manager_019fe42934a67d13b2dd6e8cb949d034 ready", 12);
        assert!(lines.iter().all(|line| line.chars().count() <= 12));
        assert!(lines.last().is_some_and(|line| line.ends_with("ready")));
    }

    #[test]
    fn manager_composer_preserves_multiline_input_and_paste() {
        let mut navigator = Navigator::empty();
        navigator.manager.focused = true;
        navigator.insert_manager_text("first");
        navigator.handle_manager_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL));
        navigator.handle_paste("second\r\nthird");
        assert_eq!(navigator.manager.composer, "first\nsecond\nthird");
        assert_eq!(
            navigator.manager.composer_cursor,
            navigator.manager.composer.len()
        );
        assert_eq!(
            wrap_editor_text(&navigator.manager.composer, 20),
            ["first", "second", "third"]
        );
    }

    #[test]
    fn manager_composer_supports_full_cursor_editing() {
        let mut navigator = Navigator::empty();
        navigator.manager.focused = true;
        navigator.insert_manager_text("abc\ndef");

        navigator.handle_manager_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        navigator.handle_manager_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        navigator.insert_manager_text("X");
        assert_eq!(navigator.manager.composer, "abc\ndXef");

        navigator.handle_manager_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        navigator.handle_manager_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(navigator.manager.composer, "ac\ndXef");
        assert!(
            navigator
                .manager
                .composer
                .is_char_boundary(navigator.manager.composer_cursor)
        );

        navigator.handle_manager_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        navigator.insert_manager_text("!");
        assert_eq!(navigator.manager.composer, "ac!\ndXef");
    }

    #[test]
    fn manager_composer_moves_between_visually_wrapped_rows() {
        let mut navigator = Navigator::empty();
        navigator.manager.focused = true;
        navigator.manager.composer_width = 3;
        navigator.insert_manager_text("abcdef");
        navigator.handle_manager_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        navigator.insert_manager_text("X");
        assert_eq!(navigator.manager.composer, "abcXdef");
    }

    #[test]
    fn manager_transcript_scrolls_back_from_the_live_bottom() {
        assert_eq!(manager_scroll_position(100, 20, 0), 80);
        assert_eq!(manager_scroll_position(100, 20, 3), 77);
        assert_eq!(manager_scroll_position(100, 20, u16::MAX), 0);

        let mut navigator = Navigator::empty();
        navigator.manager.focused = true;
        navigator.handle_manager_key(KeyEvent::new(KeyCode::Up, KeyModifiers::CONTROL));
        assert_eq!(navigator.manager.scroll, 3);
        navigator.handle_manager_key(KeyEvent::new(KeyCode::Down, KeyModifiers::CONTROL));
        assert_eq!(navigator.manager.scroll, 0);
        navigator.handle_manager_key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE));
        assert_eq!(navigator.manager.scroll, 12);
        navigator.upsert_manager_progress(ManagerProgress {
            id: "call".to_string(),
            label: "MCP · wipsaw/search_files".to_string(),
            detail: None,
            status: ManagerProgressStatus::Completed,
        });
        assert_eq!(navigator.manager.scroll, 12);
    }

    #[test]
    fn editor_clicks_map_to_a_real_character_boundary() {
        let text = "alpha\nbéta";
        let cursor = editor_cursor_from_position(text, 20, 1, 2);
        assert_eq!(&text[cursor..], "ta");
        assert!(text.is_char_boundary(cursor));
    }

    #[test]
    fn completed_wipsaw_mutations_request_inventory_refresh() {
        let progress = ManagerProgress {
            id: "call".to_string(),
            label: "MCP · wipsaw/create_codex_tab".to_string(),
            detail: None,
            status: ManagerProgressStatus::Completed,
        };
        assert!(manager_progress_changes_inventory(&progress));
    }

    #[test]
    fn at_and_dollar_references_offer_files_and_all_manager_skills() {
        let root = tempdir().unwrap();
        fs::write(root.path().join("README.md"), "read me").unwrap();
        fs::write(root.path().join("notes with spaces.md"), "notes").unwrap();
        let mut navigator = Navigator::empty();
        navigator.manager.focused = true;
        let scope = ManagerContextScope::workspace(
            root.path().to_path_buf(),
            vec![root.path().to_path_buf()],
        );
        navigator.manager.references = manager_reference_catalog(&scope);
        navigator.manager.scope = Some(scope);
        let skill_names = navigator
            .manager
            .references
            .iter()
            .filter(|reference| reference.kind == super::ManagerReferenceKind::Skill)
            .map(|reference| reference.label.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            skill_names,
            ["wipsaw-manager", "skill-creator", "skill-installer"]
        );
        navigator.manager.composer = "Review @READ".to_string();
        navigator.manager.composer_cursor = navigator.manager.composer.len();
        navigator.insert_manager_reference();
        assert_eq!(navigator.manager.composer, "Review @README.md ");
        navigator.insert_manager_text("$skill-c");
        navigator.insert_manager_reference();
        assert!(navigator.manager.composer.ends_with("$skill-creator "));
        assert!(active_manager_reference("Use @notes").is_some());
    }

    #[test]
    fn osc52_payload_uses_standard_base64() {
        assert_eq!(base64_encode(b"hello"), "aGVsbG8=");
        assert_eq!(base64_encode(b"Wipsaw"), "V2lwc2F3");
    }

    #[test]
    fn manager_copy_selection_preserves_an_exact_id_across_visual_wraps() {
        let native_id = "019fab4e-fc47-75c1-9be8-66050a58add7";
        let mut navigator = Navigator::empty();
        navigator.manager.messages = vec![ManagerMessage {
            id: 1,
            manager_session_id: "manager_test".to_string(),
            role: "assistant".to_string(),
            content: format!("Resume native session {native_id} now."),
            created_at: String::new(),
        }];
        navigator.open_manager_copy_view();
        navigator.manager.copy_view_width = 12;
        let document = navigator.manager_copy_document();
        let start = document.find(native_id).unwrap();
        let end = start + native_id.len();
        assert!(manager_copy_lines(&document, 12).len() > document.lines().count());

        navigator.manager.copy_drag_origin = start;
        navigator.manager.copy_drag_origin_end = super::next_char_boundary(&document, start);
        navigator.set_manager_mouse_selection(end - 1, end, &document);
        match navigator.copy_manager_selection() {
            Action::CopyManager { text, .. } => assert_eq!(text, native_id),
            _ => panic!("expected selected text to be copied"),
        }

        navigator.manager.copy_drag_origin = end - 1;
        navigator.manager.copy_drag_origin_end = end;
        navigator.set_manager_mouse_selection(
            start,
            super::next_char_boundary(&document, start),
            &document,
        );
        match navigator.copy_manager_selection() {
            Action::CopyManager { text, .. } => assert_eq!(text, native_id),
            _ => panic!("expected reverse-selected text to be copied"),
        }
    }

    #[test]
    fn empty_navigator_renders_manager_first_dashboard() {
        let backend = TestBackend::new(144, 42);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut navigator = Navigator::empty();
        terminal
            .draw(|frame| navigator.render(frame))
            .expect("render navigator");
        let content = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(content.contains("WIPSAW"));
        assert!(content.contains("Ready to cut?"));
        assert!(content.contains("LUMBERGH"));
        assert!(content.contains("CODEX MANAGER"));
        assert!(content.contains('λ'));
        assert!(!content.contains("Ask LUMBERGH anything"));
        assert!(content.contains("New workspace"));
        assert!(content.contains("NO WORKSPACE YET"));
        assert!(content.contains("OVERVIEW"));
    }

    #[test]
    fn compact_dashboard_keeps_manager_and_navigation_visible() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut navigator = Navigator::empty();
        terminal
            .draw(|frame| navigator.render(frame))
            .expect("render compact navigator");
        let content = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(content.contains("WIPSAW"));
        assert!(content.contains("Ready to cut?"));
        assert!(content.contains("LUMBERGH"));
        assert!(content.contains("1 Home"));
    }

    #[test]
    fn home_enter_focuses_the_embedded_lumbergh_composer() {
        let mut navigator = Navigator::empty();

        assert!(matches!(navigator.enter_action(), Action::FocusManager));
        assert!(navigator.prompt.is_none());
    }

    #[test]
    fn manager_copy_view_contains_progress_without_dashboard_chrome() {
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut navigator = Navigator::empty();
        navigator.manager.messages = vec![
            ManagerMessage {
                id: 1,
                manager_session_id: "manager_test".to_string(),
                role: "user".to_string(),
                content: "List the workspaces".to_string(),
                created_at: String::new(),
            },
            ManagerMessage {
                id: 2,
                manager_session_id: "manager_test".to_string(),
                role: "assistant".to_string(),
                content: "There are two workspaces.".to_string(),
                created_at: String::new(),
            },
        ];
        navigator.manager.progress = vec![ManagerProgress {
            id: "tool_1".to_string(),
            label: "MCP · wipsaw/run_wipsaw".to_string(),
            detail: None,
            status: ManagerProgressStatus::Completed,
        }];
        navigator.manager.copy_view = true;

        terminal
            .draw(|frame| navigator.render(frame))
            .expect("render copy view");
        let content = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(content.contains("LUMBERGH SELECT & COPY"));
        assert!(content.contains("MCP · wipsaw/run_wipsaw"));
        assert!(content.contains("There are two workspaces."));
        assert!(!content.contains("Ready to cut?"));
        assert!(!content.contains("OVERVIEW"));
    }

    #[test]
    fn prefix_w_closes_an_active_navigator_popup() {
        let mut navigator = Navigator::empty();
        navigator.is_popup = true;

        assert!(matches!(
            navigator.handle_prefix_key(KeyCode::Char('w')),
            Action::Quit
        ));
    }
}
