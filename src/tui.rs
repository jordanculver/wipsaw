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
    Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap,
};
use ratatui::{Frame, Terminal};

use crate::app::{TabLaunchSettings, WipsawApp};
use crate::error::{Result, WipsawError};
use crate::model::{CodexThread, Tab, Workspace};
use crate::tmux::TmuxWindow;

const TEAL: Color = Color::Rgb(45, 212, 191);
const AMBER: Color = Color::Rgb(251, 191, 36);
const GREEN: Color = Color::Rgb(74, 222, 128);
const RED: Color = Color::Rgb(248, 113, 113);
const INK: Color = Color::Rgb(226, 232, 240);
const MUTED: Color = Color::Rgb(100, 116, 139);
const PANEL: Color = Color::Rgb(15, 23, 42);
const DEEP: Color = Color::Rgb(7, 12, 20);
const BORDER: Color = Color::Rgb(51, 65, 85);

pub fn run(app: &mut WipsawApp) -> Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(WipsawError::InvalidInput {
            field: "terminal",
            message: "the navigator needs an interactive terminal; use a subcommand for scripts"
                .to_string(),
        });
    }

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
        }
    }

    Ok(())
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

impl Panel {
    const fn next(self) -> Self {
        match self {
            Self::Workspaces => Self::Tabs,
            Self::Tabs => Self::Threads,
            Self::Threads => Self::Workspaces,
        }
    }

    const fn previous(self) -> Self {
        match self {
            Self::Workspaces => Self::Threads,
            Self::Tabs => Self::Workspaces,
            Self::Threads => Self::Tabs,
        }
    }
}

struct Navigator {
    workspaces: Vec<(Workspace, bool)>,
    tabs: Vec<Tab>,
    windows: Vec<TmuxWindow>,
    threads: Vec<CodexThread>,
    workspace_state: ListState,
    tab_state: ListState,
    thread_state: ListState,
    active: Panel,
    prefix_pending: bool,
    show_help: bool,
    prompt: Option<Prompt>,
    message: Option<(String, bool)>,
}

impl Navigator {
    fn load(app: &WipsawApp) -> Result<Self> {
        let mut navigator = Self::empty();
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
                navigator.active = Panel::Tabs;
                navigator.begin_create();
            }
            Ok("rename-tab") => {
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
            workspace_state: ListState::default(),
            tab_state: ListState::default(),
            thread_state: ListState::default(),
            active: Panel::Workspaces,
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
        select_id(
            &mut self.workspace_state,
            self.workspaces
                .iter()
                .position(|(workspace, _)| Some(&workspace.id) == workspace_id.as_ref()),
            self.workspaces.len(),
        );
        self.refresh_tabs(app, tab_id.as_deref())?;
        self.threads = app.list_codex_threads(None)?;
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
        self.workspaces = app.list_workspaces()?;
        select_id(
            &mut self.workspace_state,
            self.workspaces
                .iter()
                .position(|(workspace, _)| Some(workspace.id.as_str()) == workspace_id),
            self.workspaces.len(),
        );
        self.refresh_tabs(app, None)?;
        self.threads = app.list_codex_threads(None)?;
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
            KeyCode::Char('?') => {
                self.show_help = true;
                Action::None
            }
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
                self.active = self.active.next();
                Action::None
            }
            KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
                self.active = self.active.previous();
                Action::None
            }
            KeyCode::Char('1') => {
                self.active = Panel::Workspaces;
                Action::None
            }
            KeyCode::Char('2') => {
                self.active = Panel::Tabs;
                Action::None
            }
            KeyCode::Char('3') => {
                self.active = Panel::Threads;
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
            KeyCode::Char('g') => {
                self.select_edge(false);
                Action::None
            }
            KeyCode::Char('G') => {
                self.select_edge(true);
                Action::None
            }
            KeyCode::Char('r') => Action::Refresh,
            KeyCode::Char('c') | KeyCode::Char('n') => {
                self.begin_create();
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
            KeyCode::Char('w') => self.active = Panel::Workspaces,
            KeyCode::Char('t') => self.active = Panel::Tabs,
            KeyCode::Char('s') => self.active = Panel::Threads,
            KeyCode::Char('n') => self.active = self.active.next(),
            KeyCode::Char('p') => self.active = self.active.previous(),
            KeyCode::Char('c') => self.begin_create(),
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
        let prompt = match self.active {
            Panel::Workspaces => Prompt::new(PromptKind::Workspace),
            Panel::Tabs | Panel::Threads => {
                let Some(workspace) = self.selected_workspace() else {
                    self.error("create a workspace first");
                    return;
                };
                Prompt::new(PromptKind::Tab {
                    workspace_id: workspace.id.clone(),
                    start_codex: self.active == Panel::Threads,
                })
            }
        };
        self.prompt = Some(prompt);
        self.message = None;
    }

    fn begin_rename(&mut self) {
        if self.active != Panel::Tabs {
            self.notice("rename is available from the tabs panel");
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

    fn enter_action(&self) -> Action {
        match self.active {
            Panel::Workspaces => self
                .selected_workspace()
                .map(|workspace| Action::ActivateWorkspace(workspace.id.clone()))
                .unwrap_or(Action::None),
            Panel::Tabs => self
                .selected_workspace()
                .zip(self.selected_tab())
                .map(|(workspace, tab)| Action::ActivateTab {
                    workspace_id: workspace.id.clone(),
                    tab_id: tab.id.clone(),
                })
                .unwrap_or(Action::None),
            Panel::Threads => self
                .selected_thread()
                .map(|thread| Action::OpenThread {
                    thread_id: thread.id.clone(),
                    workspace_id: self.selected_workspace().map(|item| item.id.clone()),
                })
                .unwrap_or(Action::None),
        }
    }

    fn move_selection(&mut self, delta: isize) {
        match self.active {
            Panel::Workspaces => {
                move_state(&mut self.workspace_state, self.workspaces.len(), delta)
            }
            Panel::Tabs => move_state(&mut self.tab_state, self.tabs.len(), delta),
            Panel::Threads => move_state(&mut self.thread_state, self.threads.len(), delta),
        }
    }

    fn select_edge(&mut self, last: bool) {
        let (state, len) = match self.active {
            Panel::Workspaces => (&mut self.workspace_state, self.workspaces.len()),
            Panel::Tabs => (&mut self.tab_state, self.tabs.len()),
            Panel::Threads => (&mut self.thread_state, self.threads.len()),
        };
        state.select((len > 0).then_some(if last { len - 1 } else { 0 }));
    }

    fn render(&mut self, frame: &mut Frame<'_>) {
        let area = frame.area();
        frame.render_widget(Block::default().style(Style::default().bg(DEEP)), area);
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(6),
                Constraint::Length(2),
            ])
            .split(area);
        self.render_header(frame, rows[0]);
        self.render_panels(frame, rows[1]);
        self.render_footer(frame, rows[2]);
        if self.show_help {
            self.render_help(frame, area);
        }
        if self.prompt.is_some() {
            self.render_prompt(frame, area);
        }
    }

    fn render_header(&self, frame: &mut Frame<'_>, area: Rect) {
        let live_count = self.workspaces.iter().filter(|(_, live)| *live).count();
        let header = Line::from(vec![
            Span::styled(
                " WIPSAW ",
                Style::default()
                    .fg(DEEP)
                    .bg(TEAL)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                " // CUT CONTROL  ",
                Style::default().fg(INK).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(
                    "{live_count}/{} workspaces  ·  {} tabs  ·  {} threads",
                    self.workspaces.len(),
                    self.tabs.len(),
                    self.threads.len()
                ),
                Style::default().fg(MUTED),
            ),
        ]);
        frame.render_widget(
            Paragraph::new(header)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(BORDER))
                        .style(Style::default().bg(PANEL)),
                )
                .alignment(Alignment::Left),
            area,
        );
    }

    fn render_panels(&mut self, frame: &mut Frame<'_>, area: Rect) {
        if area.width < 100 {
            match self.active {
                Panel::Workspaces => self.render_workspaces(frame, area),
                Panel::Tabs => self.render_tabs(frame, area),
                Panel::Threads => self.render_threads(frame, area),
            }
            return;
        }
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(28),
                Constraint::Percentage(34),
                Constraint::Percentage(38),
            ])
            .split(area);
        self.render_workspaces(frame, columns[0]);
        self.render_tabs(frame, columns[1]);
        self.render_threads(frame, columns[2]);
    }

    fn render_workspaces(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let items = if self.workspaces.is_empty() {
            vec![ListItem::new(Line::styled(
                "  No cuts yet · press c",
                Style::default().fg(MUTED),
            ))]
        } else {
            self.workspaces
                .iter()
                .map(|(workspace, live)| {
                    ListItem::new(Line::from(vec![
                        Span::styled(
                            if *live { "● " } else { "○ " },
                            Style::default().fg(if *live { GREEN } else { MUTED }),
                        ),
                        Span::styled(&workspace.name, Style::default().fg(INK)),
                        Span::styled(
                            if *live { "  LIVE" } else { "  STOPPED" },
                            Style::default().fg(if *live { GREEN } else { MUTED }),
                        ),
                    ]))
                })
                .collect()
        };
        render_list(
            frame,
            area,
            format!(" 1 WORKSPACES · {} ", self.workspaces.len()),
            items,
            &mut self.workspace_state,
            self.active == Panel::Workspaces,
        );
    }

    fn render_tabs(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let items = if self.tabs.is_empty() {
            vec![ListItem::new(Line::styled(
                "  Select a workspace · press c",
                Style::default().fg(MUTED),
            ))]
        } else {
            self.tabs
                .iter()
                .map(|tab| {
                    let live = self
                        .windows
                        .iter()
                        .find(|window| window.id == tab.tmux_window_id);
                    ListItem::new(Line::from(vec![
                        Span::styled(
                            format!("{:>2} ", tab.tmux_window_index),
                            Style::default().fg(MUTED),
                        ),
                        Span::styled(&tab.name, Style::default().fg(INK)),
                        Span::styled(
                            if tab.codex_thread_id.is_some() {
                                "  CODEX"
                            } else {
                                "  SHELL"
                            },
                            Style::default().fg(if tab.codex_thread_id.is_some() {
                                TEAL
                            } else {
                                MUTED
                            }),
                        ),
                        Span::styled(
                            if live.is_some_and(|window| window.active) {
                                "  ◀"
                            } else if live.is_none() {
                                "  ×"
                            } else {
                                ""
                            },
                            Style::default().fg(AMBER),
                        ),
                    ]))
                })
                .collect()
        };
        render_list(
            frame,
            area,
            format!(" 2 TABS · {} ", self.tabs.len()),
            items,
            &mut self.tab_state,
            self.active == Panel::Tabs,
        );
    }

    fn render_threads(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let items = if self.threads.is_empty() {
            vec![ListItem::new(Text::from(vec![
                Line::styled("  No managed Codex threads", Style::default().fg(MUTED)),
                Line::styled(
                    "  press c or run codex in a tab",
                    Style::default().fg(MUTED),
                ),
            ]))]
        } else {
            self.threads
                .iter()
                .map(|thread| {
                    ListItem::new(Text::from(vec![
                        Line::from(vec![
                            Span::styled("◆ ", Style::default().fg(TEAL)),
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
                        ]),
                    ]))
                })
                .collect()
        };
        render_list(
            frame,
            area,
            format!(" 3 CODEX THREADS · {} ", self.threads.len()),
            items,
            &mut self.thread_state,
            self.active == Panel::Threads,
        );
    }

    fn render_footer(&self, frame: &mut Frame<'_>, area: Rect) {
        let (cut_line, is_error) = self.message.clone().unwrap_or_else(|| {
            let text = match self.active {
                Panel::Workspaces => self
                    .selected_workspace()
                    .map(|workspace| format!("Enter attaches '{}'", workspace.name))
                    .unwrap_or_else(|| "c creates your first workspace here".to_string()),
                Panel::Tabs => self
                    .selected_tab()
                    .map(|tab| format!("Enter opens '{}' · , renames it", tab.name))
                    .unwrap_or_else(|| "c creates a shell tab here".to_string()),
                Panel::Threads => self
                    .selected_thread()
                    .map(|thread| format!("Enter resumes '{}'", thread.name))
                    .unwrap_or_else(|| "c creates and starts a Codex tab here".to_string()),
            };
            (text, false)
        });
        let prefix = if self.prefix_pending {
            Span::styled(
                "PREFIX C-b · choose w/t/s, n/p, c, ?, q",
                Style::default().fg(AMBER),
            )
        } else {
            Span::styled(
                "h/l panels  j/k move  Enter open  c create  , rename  r refresh  ? help  q quit",
                Style::default().fg(MUTED),
            )
        };
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(vec![
                    Span::styled(
                        "CUT LINE › ",
                        Style::default().fg(AMBER).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        cut_line,
                        Style::default().fg(if is_error { RED } else { INK }),
                    ),
                ]),
                Line::from(prefix),
            ])
            .style(Style::default().bg(DEEP)),
            area,
        );
    }

    fn render_help(&self, frame: &mut Frame<'_>, area: Rect) {
        let popup = centered_rect(area, 78, 20);
        frame.render_widget(Clear, popup);
        let help = Text::from(vec![
            Line::styled(
                "CUT CONTROL KEYS",
                Style::default().fg(TEAL).add_modifier(Modifier::BOLD),
            ),
            Line::raw(""),
            Line::from("  1 / 2 / 3      workspaces / tabs / Codex threads"),
            Line::from("  h l / arrows   move between panels"),
            Line::from("  j k / arrows   move selection"),
            Line::from("  g / G          first / last item"),
            Line::from("  Enter          attach, open, or resume"),
            Line::from("  c or n         create in the active panel"),
            Line::from("  ,              rename the selected tab"),
            Line::from("  r              refresh live state"),
            Line::raw(""),
            Line::styled("TMUX-FAMILIAR PREFIX", Style::default().fg(AMBER)),
            Line::from("  C-b w/t/s      focus a table of contents"),
            Line::from("  C-b n/p        next / previous panel"),
            Line::from("  C-b c          create in the active panel"),
            Line::raw(""),
            Line::styled(
                "Inside a managed shell: codex · manager · lumberg · lumbergh",
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
                        .border_style(Style::default().fg(TEAL))
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
                Span::styled("█", Style::default().fg(TEAL)),
            ]),
        ];
        frame.render_widget(
            Paragraph::new(text).block(
                Block::default()
                    .title(" NEW CUT · Enter confirms · Esc cancels ")
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
            PromptKind::Workspace => "Workspace name · working directory is the current directory",
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
}

fn render_list<'a>(
    frame: &mut Frame<'_>,
    area: Rect,
    title: String,
    items: Vec<ListItem<'a>>,
    state: &mut ListState,
    active: bool,
) {
    let border_color = if active { TEAL } else { BORDER };
    let block = Block::default()
        .title(Span::styled(
            title,
            Style::default()
                .fg(if active { TEAL } else { MUTED })
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color))
        .style(Style::default().bg(PANEL));
    let list = List::new(items)
        .block(block)
        .highlight_symbol("▌ ")
        .highlight_style(
            Style::default()
                .bg(Color::Rgb(19, 78, 74))
                .fg(Color::White)
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
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::Navigator;

    #[test]
    fn empty_navigator_renders_primary_table_of_contents() {
        let backend = TestBackend::new(120, 28);
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
        assert!(content.contains("WORKSPACES"));
        assert!(content.contains("TABS"));
        assert!(content.contains("CODEX THREADS"));
        assert!(content.contains("CUT LINE"));
    }
}
