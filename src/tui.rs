use std::io::{self, IsTerminal, Stdout};
use std::time::Duration;

use crossterm::cursor::{Hide, Show};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
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
use crate::model::{Account, CodexHome, CodexThread, ModelProfile, Tab, Workspace};
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
        session.terminal.draw(|frame| navigator.render(frame))?;

        if !event::poll(Duration::from_millis(250))? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }

        let old_workspace = navigator.selected_workspace().map(|item| item.id.clone());
        let action = navigator.handle_key(key);
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
                        session.suspend()?;
                        let result = app.activate_tab(&workspace.id, "manager");
                        session.resume()?;
                        match result {
                            Ok(true) => break,
                            Ok(false) => navigator.notice("detached; Lumbergh is still running"),
                            Err(error) => navigator.error(error),
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
            Action::OpenManager { workspace_id } => {
                session.suspend()?;
                let result = app.activate_tab(&workspace_id, "manager");
                session.resume()?;
                match result {
                    Ok(true) => break,
                    Ok(false) => navigator.notice("detached; Lumbergh is still running"),
                    Err(error) => navigator.error(error),
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
}

impl TerminalSession {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(error) = execute!(
            stdout,
            EnterAlternateScreen,
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
        })
    }

    fn suspend(&mut self) -> io::Result<()> {
        if self.active {
            disable_raw_mode()?;
            execute!(self.terminal.backend_mut(), Show, LeaveAlternateScreen)?;
            self.active = false;
        }
        Ok(())
    }

    fn resume(&mut self) -> io::Result<()> {
        if !self.active {
            enable_raw_mode()?;
            if let Err(error) = execute!(
                self.terminal.backend_mut(),
                EnterAlternateScreen,
                Hide,
                ClearTerminal(ClearType::All)
            ) {
                let _ = disable_raw_mode();
                return Err(error);
            }
            self.active = true;
            self.terminal.clear()?;
        }
        Ok(())
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        if self.active {
            let _ = disable_raw_mode();
            let _ = execute!(self.terminal.backend_mut(), Show, LeaveAlternateScreen);
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

struct Navigator {
    workspaces: Vec<(Workspace, bool)>,
    tabs: Vec<Tab>,
    windows: Vec<TmuxWindow>,
    threads: Vec<CodexThread>,
    accounts: Vec<Account>,
    homes: Vec<CodexHome>,
    profiles: Vec<ModelProfile>,
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
}

impl Navigator {
    fn load(app: &WipsawApp) -> Result<Self> {
        let mut navigator = Self::empty();
        navigator.is_popup = std::env::var_os("WIPSAW_PARENT_SESSION").is_some();
        navigator.refresh(app)?;
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
        }
    }

    fn refresh(&mut self, app: &WipsawApp) -> Result<()> {
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
        self.health = SystemHealth::collect(app);
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
            KeyCode::Char('m') => self.open_manager_action(),
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
            KeyCode::Char('m') => return self.open_manager_action(),
            KeyCode::Char('?') => self.show_help = true,
            KeyCode::Char('q') => return Action::Quit,
            _ => self.notice("unknown prefix key; press ? for bindings"),
        }
        Action::None
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
            self.error("create a workspace first · Enter on Home starts with Lumbergh");
            return;
        };
        self.prompt = Some(Prompt::new(PromptKind::Tab {
            workspace_id: workspace.id.clone(),
            start_codex,
        }));
        self.message = None;
    }

    fn open_manager_action(&mut self) -> Action {
        let Some(workspace_id) = self
            .selected_workspace()
            .map(|workspace| workspace.id.clone())
        else {
            self.begin_workspace();
            return Action::None;
        };
        Action::OpenManager { workspace_id }
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
            View::Home => self.open_manager_action(),
            View::Sessions if self.active == Panel::Workspaces => self
                .selected_workspace()
                .map(|workspace| Action::ActivateWorkspace(workspace.id.clone()))
                .unwrap_or(Action::None),
            View::Sessions => self
                .selected_workspace()
                .zip(self.selected_tab())
                .map(|(workspace, tab)| Action::ActivateTab {
                    workspace_id: workspace.id.clone(),
                    tab_id: tab.id.clone(),
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
        frame.render_widget(Block::default().style(Style::default().bg(DEEP)), area);
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
                    Span::styled("         manager", Style::default().fg(MUTED)),
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
                Constraint::Length(9),
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
                    Constraint::Length(7),
                    Constraint::Length(3),
                    Constraint::Min(5),
                ]
            } else {
                [
                    Constraint::Length(4),
                    Constraint::Length(8),
                    Constraint::Length(3),
                    Constraint::Min(8),
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
        self.render_manager_entry(frame, rows[1]);
        self.render_quick_actions(frame, rows[2]);
        self.render_home_activity(frame, rows[3]);
    }

    fn render_manager_entry(&self, frame: &mut Frame<'_>, area: Rect) {
        let ready = self.selected_workspace().is_some();
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2),
                Constraint::Length(3),
                Constraint::Min(1),
            ])
            .split(area);
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(vec![
                    Span::styled(
                        "LUMBERGH",
                        Style::default().fg(CYAN).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(" // CODEX MANAGER", Style::default().fg(INK)),
                ]),
                Line::styled(
                    "Tell me what you want to set up, run, or inspect.",
                    Style::default().fg(MUTED),
                ),
            ]),
            rows[0],
        );
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("› ", Style::default().fg(CYAN).add_modifier(Modifier::BOLD)),
                Span::styled(
                    if ready {
                        "Ask Lumbergh anything…"
                    } else {
                        "Create a workspace and meet Lumbergh…"
                    },
                    Style::default().fg(INK),
                ),
            ]))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(CYAN))
                    .style(Style::default().bg(PANEL)),
            ),
            rows[1],
        );
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    "Enter",
                    Style::default().fg(CYAN).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    if ready {
                        "  talk to manager"
                    } else {
                        "  name a workspace, then talk to manager"
                    },
                    Style::default().fg(MUTED),
                ),
            ])),
            rows[2],
        );
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
                        "Press Enter. Wipsaw will use this directory and your current Codex home/auth.",
                        Style::default().fg(INK),
                    ),
                    Line::styled(
                        "You can ask Lumbergh to organize everything else after it opens.",
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
                let kind = if tab.name.eq_ignore_ascii_case("manager") {
                    "manager"
                } else if tab.codex_thread_id.is_some() {
                    "codex"
                } else {
                    "shell"
                };
                let model = thread
                    .map(|thread| thread.model.as_str())
                    .or_else(|| profile.map(|profile| profile.model.as_str()))
                    .unwrap_or("default");
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
                Constraint::Percentage(28),
                Constraint::Length(9),
                Constraint::Percentage(24),
                Constraint::Percentage(20),
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
                    let kind = if tab.name.eq_ignore_ascii_case("manager") {
                        "MANAGER"
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
                View::Home => self
                    .selected_workspace()
                    .map(|workspace| {
                        format!(
                            "Enter opens Lumbergh for '{}' · j/k switches workspace",
                            workspace.name
                        )
                    })
                    .unwrap_or_else(|| {
                        "Enter creates your first workspace and opens Lumbergh".to_string()
                    }),
                View::Sessions if self.active == Panel::Workspaces => self
                    .selected_workspace()
                    .map(|workspace| {
                        format!("Enter attaches '{}' · l focuses its tabs", workspace.name)
                    })
                    .unwrap_or_else(|| "c creates a workspace".to_string()),
                View::Sessions => self
                    .selected_tab()
                    .map(|tab| format!("Enter opens '{}' · , renames it", tab.name))
                    .unwrap_or_else(|| "t creates a shell · n creates Codex".to_string()),
                View::Threads => self
                    .selected_thread()
                    .map(|thread| format!("Enter opens or resumes '{}'", thread.name))
                    .unwrap_or_else(|| "n creates a named Codex session".to_string()),
                View::Wips => "WIP runtime is next · m opens Lumbergh now".to_string(),
            };
            (text, false)
        });
        let keys = if self.prefix_pending {
            "PREFIX C-b · w home  s sessions  t threads  g WIPs  m manager  n/p views"
        } else if area.width < 92 {
            "↑↓ move  Enter open  c workspace  n Codex  m manager  ? guide"
        } else {
            "↑↓ move   Enter open   c workspace   n Codex   t shell   m manager   / commands   ? guide"
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
        let popup = centered_rect(area, 88, 27);
        frame.render_widget(Clear, popup);
        let help = Text::from(vec![
            Line::styled(
                "START HERE",
                Style::default().fg(CYAN).add_modifier(Modifier::BOLD),
            ),
            Line::from("  1. Press Enter on Home to open Lumbergh, your persistent Codex manager."),
            Line::from(
                "  2. Tell Lumbergh what you want: create tabs, choose models, or prepare a WIP.",
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
            Line::from("  ,              rename a focused tab · r refreshes live state"),
            Line::raw(""),
            Line::styled("TMUX-FAMILIAR PREFIX", Style::default().fg(AMBER)),
            Line::from("  C-b w          toggle this navigator"),
            Line::from("  C-b c / ,      create / rename a managed tab"),
            Line::from("  C-b m          open Lumbergh"),
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
                "Workspace name · Lumbergh starts next in the current directory"
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
    OpenManager {
        workspace_id: String,
    },
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
    use crossterm::event::KeyCode;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::{Action, Navigator, PromptKind};

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
    fn home_enter_starts_first_workspace_manager_flow() {
        let mut navigator = Navigator::empty();

        assert!(matches!(navigator.enter_action(), Action::None));
        assert!(matches!(
            navigator.prompt.as_ref().map(|prompt| &prompt.kind),
            Some(PromptKind::Workspace)
        ));
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
