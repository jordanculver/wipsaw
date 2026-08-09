use std::collections::HashSet;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use directories::BaseDirs;
use serde::Serialize;

use crate::codex::{
    CodexHomeProbe, NativeCodexThread, NativeCodexThreadInspection, archive_thread,
    codex_launch_path, delete_thread, inspect_thread as inspect_native_thread,
    native_thread_has_active_writer, probe_home, resume_native_thread, start_named_thread,
    start_named_thread_with_handoff,
};
use crate::error::{Result, WipsawError};
use crate::id::{EntityKind, WipsawId};
use crate::manager::{
    MANAGER_MODEL, MANAGER_REASONING_EFFORT, MANAGER_TRANSCRIPT_MESSAGE_LIMIT, ManagerContextScope,
    ManagerEvent, ManagerTurnRequest, ManagerTurnResult, expand_prompt_references, prepare_runtime,
    spawn_turn,
};
use crate::model::{
    Account, AccountAuthKind, AccountOwnerKind, CodexHome, CodexThread, ManagerKind,
    ManagerMessage, ManagerSession, ModelProfile, ModelProfileSettings, Tab, Workspace,
    WorkspaceContext, validate_credential_ref, validate_display_name, validate_model_name,
    validate_profile_settings,
};
use crate::paths::AppPaths;
use crate::registry::{
    NewAccount, NewCodexHome, NewCodexThread, NewCurrentCodex, NewManagerSession, NewModelProfile,
    NewTab, NewWorkspace, Registry,
};
use crate::shell;
use crate::shortcuts;
use crate::tmux::{CodexTabLaunch, TmuxBackend, TmuxWindow};

pub struct WipsawApp {
    pub paths: AppPaths,
    pub registry: Registry,
    pub tmux: TmuxBackend,
}

#[derive(Debug, Clone, Serialize)]
pub struct CodexThreadInspection {
    pub managed: CodexThread,
    pub native: NativeCodexThreadInspection,
}

#[derive(Debug, Clone, Serialize)]
pub struct CodexThreadLaunch {
    pub thread_id: String,
    pub native_thread_id: String,
    pub workspace_id: String,
    pub tab_id: String,
    pub tmux_session: String,
    pub tmux_window_id: String,
    pub codex_home_id: String,
    pub cwd: PathBuf,
    pub return_shell: PathBuf,
    /// True when this call started the Codex process. False means the exact
    /// managed/native thread was already running in the tab.
    pub launched: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct CodexTabSessionLaunch {
    pub workspace_id: String,
    pub workspace_name: String,
    pub tab: Tab,
    pub thread: CodexThread,
    pub launch: CodexThreadLaunch,
}

#[derive(Debug, Clone, Serialize)]
pub struct CodexHandoffLaunch {
    pub workspace_id: String,
    pub workspace_name: String,
    pub tab: Tab,
    pub thread: CodexThread,
    pub launch: CodexThreadLaunch,
    pub source_codex_home_id: String,
    pub source_native_thread_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CodexSessionImport {
    pub workspace_id: String,
    pub workspace_name: String,
    pub tab: Tab,
    pub thread: CodexThread,
    pub launch: Option<CodexThreadLaunch>,
    /// Present when the exact conversation was adopted but Codex would not
    /// allow a second writer while it remained open in another terminal.
    pub deferred_reason: Option<String>,
    pub thread_imported: bool,
    pub tab_created: bool,
    pub replaced_thread_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WipsawInitialization {
    pub status: String,
    pub codex: CodexHomeProbe,
    pub shortcut_bin: PathBuf,
    pub tmux_socket: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceStart {
    pub workspace: Workspace,
    pub restored: bool,
    pub reopened_threads: Vec<CodexThreadLaunch>,
    pub reopen_failures: Vec<WorkspaceThreadReopenFailure>,
    pub manager_session_id: String,
    pub native_manager_thread_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceThreadReopenFailure {
    pub tab_id: String,
    pub tab_name: String,
    pub thread_id: String,
    pub error: String,
}

#[derive(Debug, Default)]
struct WorkspaceRuntime {
    restored: bool,
    reopened_threads: Vec<CodexThreadLaunch>,
    reopen_failures: Vec<WorkspaceThreadReopenFailure>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceDeletion {
    pub workspace: Workspace,
    pub tmux_session_stopped: bool,
    pub deleted_threads: Vec<CodexThreadDeletion>,
    pub deleted_manager_thread_id: Option<String>,
    pub retained_shared_thread_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CodexThreadDeletion {
    pub thread_id: String,
    pub native_thread_id: String,
    pub name: String,
}

pub struct ManagerTurnHandle {
    pub session: ManagerSession,
    pub receiver: std::sync::mpsc::Receiver<ManagerEvent>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct TabLaunchSettings<'a> {
    pub account: Option<&'a str>,
    pub codex_home: Option<&'a str>,
    pub model_profile: Option<&'a str>,
}

impl WipsawApp {
    pub fn from_env() -> Result<Self> {
        let paths = AppPaths::from_env()?;
        Self::open(paths)
    }

    pub fn open(paths: AppPaths) -> Result<Self> {
        paths.ensure()?;
        shortcuts::install(&paths, &env::current_exe()?)?;
        shell::install(&paths)?;
        let registry = Registry::open(&paths.registry_path())?;
        let tmux = TmuxBackend::from_env(&paths);
        let mut app = Self {
            paths,
            registry,
            tmux,
        };
        app.adopt_current_codex_home()?;
        app.tmux.write_config()?;
        let has_live_workspace = app.registry.list_workspaces()?.iter().any(|workspace| {
            app.tmux
                .session_exists(&workspace.tmux_session)
                .unwrap_or(false)
        });
        if has_live_workspace {
            app.tmux.reload_config()?;
        }
        if let Some(home) = app.registry.preferred_codex_home(None)? {
            app.registry
                .apply_default_codex_home_to_unconfigured_tabs(&home)?;
            if let Ok(workspace_id) = env::var("WIPSAW_MANAGER_WORKSPACE_ID") {
                app.ensure_middle_manager(&workspace_id)?;
            } else {
                app.ensure_lumbergh()?;
                for workspace in app.registry.list_workspaces()? {
                    app.ensure_middle_manager(&workspace.id)?;
                }
            }
        }
        Ok(app)
    }

    pub fn initialize(&self) -> Result<WipsawInitialization> {
        let home = self
            .registry
            .preferred_codex_home(None)?
            .ok_or_else(|| WipsawError::InvalidInput {
                field: "Codex home",
                message: "no Codex home is available; set CODEX_HOME before first startup or register one with `wipsaw home add`"
                    .to_string(),
            })?;
        let codex = probe_home(&home)?;
        Ok(WipsawInitialization {
            status: "ready".to_string(),
            codex,
            shortcut_bin: self.paths.shortcut_bin_dir(),
            tmux_socket: self.tmux.socket_name().to_string(),
        })
    }

    pub fn create_workspace(&mut self, name: &str, cwd: &Path) -> Result<Workspace> {
        if env::var_os("WIPSAW_MANAGER_WORKSPACE_ID").is_some() {
            return Err(WipsawError::InvalidInput {
                field: "workspace create",
                message: "a Middle Manager cannot create another workspace; ask Lumbergh"
                    .to_string(),
            });
        }
        let name = validate_display_name("workspace name", name)?;
        let cwd = existing_directory(cwd, "working directory")?;
        if self.registry.workspace_by_ref(&name)?.is_some() {
            return Err(WipsawError::AlreadyExists {
                entity: "workspace",
                value: name,
            });
        }

        let workspace_id = WipsawId::new(EntityKind::Workspace);
        let manager_tab_id = WipsawId::new(EntityKind::Tab);
        let tmux_session = format!("wipsaw-{}", workspace_id.tmux_safe_suffix());
        let default_home = self.registry.preferred_codex_home(None)?;
        let manager = self.tmux.create_workspace(&tmux_session, &cwd)?;

        let inserted = self.registry.insert_workspace_with_manager(NewWorkspace {
            id: workspace_id.as_str(),
            name: &name,
            tmux_session: &tmux_session,
            cwd: &cwd,
            manager_tab_id: manager_tab_id.as_str(),
            manager_window_id: &manager.id,
            manager_window_index: manager.index,
            manager_account_id: default_home.as_ref().map(|home| home.account_id.as_str()),
            manager_codex_home_id: default_home.as_ref().map(|home| home.id.as_str()),
        });
        if inserted.is_err() {
            let _ = self.tmux.kill_workspace(&tmux_session);
        }
        inserted
    }

    pub fn list_workspaces(&self) -> Result<Vec<(Workspace, bool)>> {
        let manager_workspace_id = env::var("WIPSAW_MANAGER_WORKSPACE_ID").ok();
        self.registry
            .list_workspaces()?
            .into_iter()
            .filter(|workspace| {
                manager_workspace_id
                    .as_ref()
                    .is_none_or(|workspace_id| workspace_id == &workspace.id)
            })
            .map(|workspace| {
                let live = self
                    .tmux
                    .session_exists(&workspace.tmux_session)
                    .unwrap_or(false);
                Ok((workspace, live))
            })
            .collect()
    }

    pub fn workspace(&self, reference: &str) -> Result<Workspace> {
        let workspace =
            self.registry
                .workspace_by_ref(reference)?
                .ok_or_else(|| WipsawError::NotFound {
                    entity: "workspace",
                    value: reference.to_string(),
                })?;
        self.ensure_manager_workspace_scope(&workspace)?;
        Ok(workspace)
    }

    pub fn list_workspace_contexts(&self, reference: &str) -> Result<Vec<WorkspaceContext>> {
        let workspace = self.workspace(reference)?;
        self.ensure_manager_workspace_scope(&workspace)?;
        self.registry.list_workspace_contexts(&workspace.id)
    }

    pub fn add_workspace_context(&self, reference: &str, path: &Path) -> Result<WorkspaceContext> {
        if env::var_os("WIPSAW_MANAGER_WORKSPACE_ID").is_some() {
            return Err(WipsawError::InvalidInput {
                field: "workspace context",
                message: "a Middle Manager cannot broaden its own file scope; ask Lumbergh"
                    .to_string(),
            });
        }
        let workspace = self.workspace(reference)?;
        self.ensure_manager_workspace_scope(&workspace)?;
        let path = absolute_path(path)?;
        let path = fs::canonicalize(&path).map_err(|error| WipsawError::InvalidInput {
            field: "workspace context",
            message: format!("'{}' is unavailable: {error}", path.display()),
        })?;
        let kind = if path.is_dir() {
            "directory"
        } else if path.is_file() {
            "file"
        } else {
            return Err(WipsawError::InvalidInput {
                field: "workspace context",
                message: format!("'{}' must be a regular file or directory", path.display()),
            });
        };
        if crate::manager::sensitive_path(&path) {
            return Err(WipsawError::InvalidInput {
                field: "workspace context",
                message: format!("'{}' looks credential-bearing", path.display()),
            });
        }
        self.registry
            .insert_workspace_context(&workspace.id, &path, kind)
    }

    pub fn remove_workspace_context(
        &self,
        reference: &str,
        path: &Path,
    ) -> Result<WorkspaceContext> {
        if env::var_os("WIPSAW_MANAGER_WORKSPACE_ID").is_some() {
            return Err(WipsawError::InvalidInput {
                field: "workspace context",
                message: "a Middle Manager cannot change its own file scope; ask Lumbergh"
                    .to_string(),
            });
        }
        let workspace = self.workspace(reference)?;
        self.ensure_manager_workspace_scope(&workspace)?;
        let contexts = self.registry.list_workspace_contexts(&workspace.id)?;
        let requested = absolute_path(path)?;
        let canonical = fs::canonicalize(&requested).unwrap_or(requested);
        let context = contexts
            .into_iter()
            .find(|context| context.path == canonical || context.path == path)
            .ok_or_else(|| WipsawError::NotFound {
                entity: "workspace context",
                value: path.display().to_string(),
            })?;
        self.registry
            .delete_workspace_context(&workspace.id, &context.path)?;
        Ok(context)
    }

    fn ensure_manager_workspace_scope(&self, workspace: &Workspace) -> Result<()> {
        if let Ok(manager_workspace_id) = env::var("WIPSAW_MANAGER_WORKSPACE_ID")
            && manager_workspace_id != workspace.id
        {
            return Err(WipsawError::InvalidInput {
                field: "workspace context",
                message: "a Middle Manager can manage context only for its own workspace"
                    .to_string(),
            });
        }
        Ok(())
    }

    fn ensure_manager_path_scope(
        &self,
        workspace: &Workspace,
        path: &Path,
        field: &'static str,
    ) -> Result<()> {
        if env::var_os("WIPSAW_MANAGER_WORKSPACE_ID").is_none() {
            return Ok(());
        }
        let roots = self
            .registry
            .list_workspace_contexts(&workspace.id)?
            .into_iter()
            .map(|context| context.path)
            .collect();
        let scope = ManagerContextScope::workspace(workspace.cwd.clone(), roots);
        let canonical = fs::canonicalize(path)?;
        if !scope.permits(&canonical) {
            return Err(WipsawError::InvalidInput {
                field,
                message: format!(
                    "'{}' is outside this Middle Manager's explicit workspace context",
                    path.display()
                ),
            });
        }
        Ok(())
    }

    fn ensure_manager_thread_scope(&self, thread: &CodexThread) -> Result<()> {
        let Ok(manager_workspace_id) = env::var("WIPSAW_MANAGER_WORKSPACE_ID") else {
            return Ok(());
        };
        let bindings = self.registry.codex_thread_workspace_ids(&thread.id)?;
        if bindings.is_empty()
            || bindings
                .iter()
                .any(|workspace_id| workspace_id != &manager_workspace_id)
        {
            return Err(WipsawError::InvalidInput {
                field: "Codex thread",
                message: "is outside this Middle Manager's workspace".to_string(),
            });
        }
        Ok(())
    }

    pub fn delete_workspace(&mut self, reference: &str) -> Result<WorkspaceDeletion> {
        let workspace = self.workspace(reference)?;
        if env::var("WIPSAW_MANAGER_WORKSPACE_ID").as_deref() == Ok(workspace.id.as_str()) {
            return Err(WipsawError::InvalidInput {
                field: "workspace delete",
                message: "a Middle Manager cannot delete its own workspace; ask Lumbergh"
                    .to_string(),
            });
        }
        let manager = self
            .registry
            .manager_session_for_workspace(Some(&workspace.id))?;
        if manager
            .as_ref()
            .is_some_and(|manager| manager.status == "working")
        {
            return Err(WipsawError::InvalidInput {
                field: "workspace delete",
                message: format!(
                    "workspace '{}' cannot be deleted while its Middle Manager is working",
                    workspace.name
                ),
            });
        }
        let workspace_threads = self.registry.list_workspace_codex_threads(&workspace.id)?;
        let mut owned_threads = Vec::new();
        let mut retained_shared_thread_ids = Vec::new();
        for thread in workspace_threads {
            let bindings = self.registry.codex_thread_workspace_ids(&thread.id)?;
            if bindings.iter().all(|binding| binding == &workspace.id) {
                owned_threads.push(thread);
            } else {
                retained_shared_thread_ids.push(thread.id);
            }
        }
        let live = self.tmux.session_exists(&workspace.tmux_session)?;
        if live {
            self.tmux.kill_workspace(&workspace.tmux_session)?;
        }

        let mut deleted_native = HashSet::new();
        let mut deleted_manager_thread_id = None;
        if let Some(manager) = manager
            && let Some(native_thread_id) = manager.native_thread_id
        {
            let home = self
                .registry
                .codex_home_by_ref(&manager.source_codex_home_id)?
                .ok_or_else(|| WipsawError::NotFound {
                    entity: "Codex home",
                    value: manager.source_codex_home_id.clone(),
                })?;
            delete_thread(&home, &native_thread_id)?;
            deleted_native.insert((home.id, native_thread_id.clone()));
            deleted_manager_thread_id = Some(native_thread_id);
        }

        let mut deleted_threads = Vec::new();
        for thread in &owned_threads {
            let home = self
                .registry
                .codex_home_by_ref(&thread.codex_home_id)?
                .ok_or_else(|| WipsawError::NotFound {
                    entity: "Codex home",
                    value: thread.codex_home_id.clone(),
                })?;
            if deleted_native.insert((home.id.clone(), thread.native_thread_id.clone())) {
                delete_thread(&home, &thread.native_thread_id)?;
            }
            deleted_threads.push(CodexThreadDeletion {
                thread_id: thread.id.clone(),
                native_thread_id: thread.native_thread_id.clone(),
                name: thread.name.clone(),
            });
        }
        let thread_ids = owned_threads
            .iter()
            .map(|thread| thread.id.clone())
            .collect::<Vec<_>>();
        self.registry
            .delete_workspace_and_threads(&workspace.id, &thread_ids)?;
        Ok(WorkspaceDeletion {
            workspace,
            tmux_session_stopped: live,
            deleted_threads,
            deleted_manager_thread_id,
            retained_shared_thread_ids,
        })
    }

    pub fn delete_codex_thread(&self, reference: &str) -> Result<CodexThreadDeletion> {
        let thread = self
            .registry
            .codex_thread_by_ref(reference)?
            .ok_or_else(|| WipsawError::NotFound {
                entity: "Codex thread",
                value: reference.to_string(),
            })?;
        let bindings = self.registry.codex_thread_workspace_ids(&thread.id)?;
        if let Ok(manager_workspace_id) = env::var("WIPSAW_MANAGER_WORKSPACE_ID")
            && (bindings.is_empty()
                || bindings
                    .iter()
                    .any(|workspace_id| workspace_id != &manager_workspace_id))
        {
            return Err(WipsawError::InvalidInput {
                field: "thread delete",
                message: "a Middle Manager can delete only a thread bound exclusively to its own workspace"
                    .to_string(),
            });
        }
        let home = self
            .registry
            .codex_home_by_ref(&thread.codex_home_id)?
            .ok_or_else(|| WipsawError::NotFound {
                entity: "Codex home",
                value: thread.codex_home_id.clone(),
            })?;
        delete_thread(&home, &thread.native_thread_id)?;
        self.registry.delete_codex_thread(&thread.id)?;
        Ok(CodexThreadDeletion {
            thread_id: thread.id,
            native_thread_id: thread.native_thread_id,
            name: thread.name,
        })
    }

    pub fn attach_workspace(&mut self, reference: &str) -> Result<()> {
        self.activate_workspace(reference).map(|_| ())
    }

    /// Ensure the workspace has a live tmux session, reconcile every durable
    /// tab with a real window, and make its embedded Middle Manager available.
    /// A stopped workspace is reconstructed from registry metadata.
    pub fn start_workspace(&mut self, reference: &str) -> Result<WorkspaceStart> {
        let workspace = self.workspace(reference)?;
        let runtime = self.ensure_workspace_runtime(&workspace, true)?;
        let manager = self.ensure_middle_manager(&workspace.id)?;
        Ok(WorkspaceStart {
            workspace,
            restored: runtime.restored,
            reopened_threads: runtime.reopened_threads,
            reopen_failures: runtime.reopen_failures,
            manager_session_id: manager.id,
            native_manager_thread_id: manager.native_thread_id,
        })
    }

    /// Attach from a standalone terminal or switch the current Wipsaw client.
    /// Returns `true` when a navigator running inside Wipsaw should close.
    pub fn activate_workspace(&mut self, reference: &str) -> Result<bool> {
        let workspace = self.start_workspace(reference)?.workspace;
        if let Some(current) = self.current_workspace()? {
            if current.id != workspace.id {
                self.tmux.switch_client(&workspace.tmux_session)?;
            }
            return Ok(true);
        }
        self.tmux.attach(&workspace.tmux_session)?;
        Ok(false)
    }

    /// Select a tab and either switch the current Wipsaw client or attach a
    /// standalone terminal. Returns `true` when the caller is already inside
    /// Wipsaw and should close a navigator popup after switching.
    pub fn activate_tab(&mut self, workspace_ref: &str, tab_ref: &str) -> Result<bool> {
        let workspace = self.start_workspace(workspace_ref)?.workspace;
        let tab = self
            .registry
            .tab_by_ref(&workspace.id, tab_ref)?
            .ok_or_else(|| WipsawError::NotFound {
                entity: "tab",
                value: tab_ref.to_string(),
            })?;
        self.tmux
            .select_tab(&workspace.tmux_session, &tab.tmux_window_id)?;
        if let Some(current) = self.current_workspace()? {
            if current.id != workspace.id {
                self.tmux.switch_client(&workspace.tmux_session)?;
            }
            return Ok(true);
        }
        self.tmux.attach(&workspace.tmux_session)?;
        Ok(false)
    }

    pub fn create_tab(
        &mut self,
        workspace_ref: &str,
        name: &str,
        cwd: Option<&Path>,
        settings: TabLaunchSettings<'_>,
    ) -> Result<Tab> {
        let workspace = self.start_workspace(workspace_ref)?.workspace;
        let name = validate_display_name("tab name", name)?;
        if self.registry.tab_by_ref(&workspace.id, &name)?.is_some() {
            return Err(WipsawError::AlreadyExists {
                entity: "tab",
                value: name,
            });
        }
        let cwd = existing_directory(cwd.unwrap_or(&workspace.cwd), "working directory")?;
        self.ensure_manager_path_scope(&workspace, &cwd, "tab working directory")?;
        let (account_id, home_id, profile_id) = self.resolve_tab_settings(settings)?;
        let tab_id = WipsawId::new(EntityKind::Tab);
        let window = self.tmux.create_tab(&workspace.tmux_session, &name, &cwd)?;
        let inserted = self.registry.insert_tab(NewTab {
            id: tab_id.as_str(),
            workspace_id: &workspace.id,
            name: &name,
            tmux_window_id: &window.id,
            tmux_window_index: window.index,
            cwd: &cwd,
            account_id: account_id.as_deref(),
            codex_home_id: home_id.as_deref(),
            model_profile_id: profile_id.as_deref(),
            codex_thread_id: None,
        });
        if inserted.is_err() {
            let _ = self.tmux.kill_tab(&workspace.tmux_session, &window.id);
        }
        inserted
    }

    pub fn list_tabs(&self, workspace_ref: &str) -> Result<(Workspace, Vec<Tab>, Vec<TmuxWindow>)> {
        let workspace = self.workspace(workspace_ref)?;
        let tabs = self.registry.list_tabs(&workspace.id)?;
        let live_windows = if self
            .tmux
            .session_exists(&workspace.tmux_session)
            .unwrap_or(false)
        {
            self.tmux.list_windows(&workspace.tmux_session)?
        } else {
            Vec::new()
        };
        Ok((workspace, tabs, live_windows))
    }

    fn ensure_workspace_runtime(
        &self,
        workspace: &Workspace,
        reopen_bound_threads: bool,
    ) -> Result<WorkspaceRuntime> {
        if !workspace.cwd.is_dir() {
            return Err(WipsawError::InvalidInput {
                field: "workspace working directory",
                message: format!(
                    "'{}' no longer exists; restore it before starting workspace '{}'",
                    workspace.cwd.display(),
                    workspace.name
                ),
            });
        }
        let mut tabs = self.registry.list_tabs(&workspace.id)?;
        if !tabs.iter().any(is_manager_tab) {
            return Err(WipsawError::NotFound {
                entity: "manager tab",
                value: workspace.name.clone(),
            });
        }
        tabs.sort_by_key(|tab| !is_manager_tab(tab));

        let was_running = self.tmux.session_exists(&workspace.tmux_session)?;
        let mut restored = !was_running;
        let mut windows = if was_running {
            match self.tmux.list_windows(&workspace.tmux_session) {
                Ok(windows) => windows,
                Err(_error) if !self.tmux.session_exists(&workspace.tmux_session)? => {
                    restored = true;
                    vec![
                        self.tmux
                            .create_workspace(&workspace.tmux_session, &workspace.cwd)?,
                    ]
                }
                Err(error) => return Err(error),
            }
        } else {
            vec![
                self.tmux
                    .create_workspace(&workspace.tmux_session, &workspace.cwd)?,
            ]
        };

        let mut used_windows = HashSet::new();
        let mut targets = Vec::with_capacity(tabs.len());
        for tab in tabs {
            let window_position = windows
                .iter()
                .position(|window| {
                    !used_windows.contains(&window.id)
                        && window.id == tab.tmux_window_id
                        && window.name.eq_ignore_ascii_case(&tab.name)
                })
                .or_else(|| {
                    windows.iter().position(|window| {
                        !used_windows.contains(&window.id)
                            && window.name.eq_ignore_ascii_case(&tab.name)
                    })
                })
                .or_else(|| {
                    windows.iter().position(|window| {
                        !used_windows.contains(&window.id) && window.id == tab.tmux_window_id
                    })
                });
            let window = if let Some(position) = window_position {
                let window = windows[position].clone();
                if window.name != tab.name {
                    self.tmux
                        .rename_tab(&workspace.tmux_session, &window.id, &tab.name)?;
                }
                window
            } else {
                let cwd = if tab.cwd.is_dir() {
                    tab.cwd.as_path()
                } else {
                    workspace.cwd.as_path()
                };
                let window = self
                    .tmux
                    .create_tab(&workspace.tmux_session, &tab.name, cwd)?;
                windows.push(window.clone());
                window
            };
            used_windows.insert(window.id.clone());
            targets.push((tab.id, window.id, window.index));
        }
        self.registry
            .replace_workspace_tab_targets(&workspace.id, &targets)?;
        let (reopened_threads, reopen_failures) = if reopen_bound_threads {
            self.reopen_workspace_threads(workspace)?
        } else {
            (Vec::new(), Vec::new())
        };
        Ok(WorkspaceRuntime {
            restored,
            reopened_threads,
            reopen_failures,
        })
    }

    /// Reopen every durable, bound Codex conversation whose pane is back at a
    /// shell. One broken historical session must not prevent the workspace or
    /// its other tabs from recovering after a reboot.
    fn reopen_workspace_threads(
        &self,
        workspace: &Workspace,
    ) -> Result<(Vec<CodexThreadLaunch>, Vec<WorkspaceThreadReopenFailure>)> {
        let mut reopened = Vec::new();
        let mut failures = Vec::new();
        for tab in self.registry.list_tabs(&workspace.id)? {
            let Some(thread_id) = tab.codex_thread_id.as_deref() else {
                continue;
            };
            if is_manager_tab(&tab) {
                continue;
            }
            let result = (|| -> Result<Option<CodexThreadLaunch>> {
                let thread = self
                    .registry
                    .codex_thread_by_ref(thread_id)?
                    .ok_or_else(|| WipsawError::NotFound {
                        entity: "Codex thread",
                        value: thread_id.to_string(),
                    })?;
                let home = self
                    .registry
                    .codex_home_by_ref(&thread.codex_home_id)?
                    .ok_or_else(|| WipsawError::NotFound {
                        entity: "Codex home",
                        value: thread.codex_home_id.clone(),
                    })?;
                if self.tmux.window_has_managed_thread(
                    &workspace.tmux_session,
                    &tab.tmux_window_id,
                    &thread.id,
                )? {
                    return Ok(None);
                }
                if !self.tab_is_at_shell(workspace, &tab)? {
                    return Err(WipsawError::InvalidInput {
                        field: "workspace recovery",
                        message: "the tab pane is busy with another foreground process".to_string(),
                    });
                }
                self.launch_codex_thread(&thread, &home, workspace, &tab, false)
                    .map(Some)
            })();
            match result {
                Ok(Some(launch)) => reopened.push(launch),
                Ok(None) => {}
                Err(error) => failures.push(WorkspaceThreadReopenFailure {
                    tab_id: tab.id,
                    tab_name: tab.name,
                    thread_id: thread_id.to_string(),
                    error: error.to_string(),
                }),
            }
        }
        Ok((reopened, failures))
    }

    pub fn rename_tab(&mut self, workspace_ref: &str, tab_ref: &str, name: &str) -> Result<Tab> {
        let workspace = self.start_workspace(workspace_ref)?.workspace;
        let tab = self
            .registry
            .tab_by_ref(&workspace.id, tab_ref)?
            .ok_or_else(|| WipsawError::NotFound {
                entity: "tab",
                value: tab_ref.to_string(),
            })?;
        let name = validate_display_name("tab name", name)?;
        if let Some(existing) = self.registry.tab_by_ref(&workspace.id, &name)?
            && existing.id != tab.id
        {
            return Err(WipsawError::AlreadyExists {
                entity: "tab",
                value: name,
            });
        }
        self.tmux
            .rename_tab(&workspace.tmux_session, &tab.tmux_window_id, &name)?;
        match self.registry.rename_tab(&tab.id, &name) {
            Ok(updated) => Ok(updated),
            Err(error) => {
                let _ =
                    self.tmux
                        .rename_tab(&workspace.tmux_session, &tab.tmux_window_id, &tab.name);
                Err(error)
            }
        }
    }

    pub fn add_account(
        &self,
        alias: &str,
        auth_kind: AccountAuthKind,
        owner_kind: AccountOwnerKind,
        credential_ref: Option<&str>,
    ) -> Result<Account> {
        if env::var_os("WIPSAW_MANAGER_WORKSPACE_ID").is_some() {
            return Err(WipsawError::InvalidInput {
                field: "account add",
                message: "a Middle Manager cannot change global accounts; ask Lumbergh".to_string(),
            });
        }
        let alias = validate_display_name("account alias", alias)?;
        let credential_ref =
            validate_credential_ref(credential_ref, auth_kind.requires_credential_ref())?;
        let id = WipsawId::new(EntityKind::Account);
        self.registry.insert_account(NewAccount {
            id: id.as_str(),
            alias: &alias,
            auth_kind,
            owner_kind,
            credential_ref: credential_ref.as_deref(),
        })
    }

    pub fn add_model_profile(
        &self,
        name: &str,
        model: &str,
        settings: ModelProfileSettings,
    ) -> Result<ModelProfile> {
        let name = validate_display_name("model profile name", name)?;
        let model = validate_model_name(model)?;
        let settings = validate_profile_settings(&settings)?;
        let id = WipsawId::new(EntityKind::ModelProfile);
        self.registry.insert_model_profile(NewModelProfile {
            id: id.as_str(),
            name: &name,
            model: &model,
            settings: &settings,
        })
    }

    pub fn add_codex_home(
        &self,
        name: &str,
        account_ref: &str,
        path: &Path,
        codex_binary: &Path,
        create: bool,
    ) -> Result<CodexHome> {
        if env::var_os("WIPSAW_MANAGER_WORKSPACE_ID").is_some() {
            return Err(WipsawError::InvalidInput {
                field: "Codex home add",
                message: "a Middle Manager cannot change global Codex homes; ask Lumbergh"
                    .to_string(),
            });
        }
        let name = validate_display_name("Codex home name", name)?;
        let account =
            self.registry
                .account_by_ref(account_ref)?
                .ok_or_else(|| WipsawError::NotFound {
                    entity: "account",
                    value: account_ref.to_string(),
                })?;
        let path = absolute_path(path)?;
        if create {
            fs::create_dir_all(&path)?;
        }
        if !path.is_dir() {
            return Err(WipsawError::InvalidInput {
                field: "Codex home path",
                message: format!(
                    "'{}' is not a directory; use --create to create it",
                    path.display()
                ),
            });
        }
        let path = fs::canonicalize(path)?;
        let codex_binary =
            resolve_executable_path(codex_binary, Some(&self.paths.shortcut_bin_dir()))?;
        let id = WipsawId::new(EntityKind::CodexHome);
        self.registry.insert_codex_home(NewCodexHome {
            id: id.as_str(),
            name: &name,
            account_id: &account.id,
            path: &path,
            codex_binary: &codex_binary,
        })
    }

    pub fn probe_codex_home(&self, reference: &str) -> Result<CodexHomeProbe> {
        let home =
            self.registry
                .codex_home_by_ref(reference)?
                .ok_or_else(|| WipsawError::NotFound {
                    entity: "Codex home",
                    value: reference.to_string(),
                })?;
        probe_home(&home)
    }

    fn adopt_current_codex_home(&mut self) -> Result<()> {
        if auto_adopt_disabled()
            || !self.registry.list_accounts()?.is_empty()
            || !self.registry.list_codex_homes()?.is_empty()
        {
            return Ok(());
        }
        let Some(path) = active_codex_home() else {
            return Ok(());
        };
        if !path.is_dir() {
            return Ok(());
        }
        let path = fs::canonicalize(path)?;
        let requested_binary = env::var_os("WIPSAW_CODEX_BIN")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("codex"));
        let Ok(codex_binary) =
            resolve_executable_path(&requested_binary, Some(&self.paths.shortcut_bin_dir()))
        else {
            return Ok(());
        };
        let account_id = WipsawId::new(EntityKind::Account);
        let home_id = WipsawId::new(EntityKind::CodexHome);
        self.registry.bootstrap_current_codex(NewCurrentCodex {
            account_id: account_id.as_str(),
            home_id: home_id.as_str(),
            path: &path,
            codex_binary: &codex_binary,
        })?;
        Ok(())
    }

    pub fn create_codex_thread(
        &mut self,
        name: &str,
        home_ref: &str,
        cwd: Option<&Path>,
        profile_ref: Option<&str>,
        tab_binding: Option<(&str, &str)>,
    ) -> Result<CodexThread> {
        self.create_codex_thread_seeded(name, home_ref, cwd, profile_ref, tab_binding, None)
    }

    fn create_codex_thread_seeded(
        &mut self,
        name: &str,
        home_ref: &str,
        cwd: Option<&Path>,
        profile_ref: Option<&str>,
        tab_binding: Option<(&str, &str)>,
        handoff: Option<&str>,
    ) -> Result<CodexThread> {
        if env::var_os("WIPSAW_MANAGER_WORKSPACE_ID").is_some() && tab_binding.is_none() {
            return Err(WipsawError::InvalidInput {
                field: "thread create",
                message: "a Middle Manager must bind a new thread to a tab in its own workspace"
                    .to_string(),
            });
        }
        let name = validate_display_name("Codex thread name", name)?;
        let home =
            self.registry
                .codex_home_by_ref(home_ref)?
                .ok_or_else(|| WipsawError::NotFound {
                    entity: "Codex home",
                    value: home_ref.to_string(),
                })?;

        let bound_tab = tab_binding
            .map(|(workspace_ref, tab_ref)| {
                let workspace = self.workspace(workspace_ref)?;
                self.registry
                    .tab_by_ref(&workspace.id, tab_ref)?
                    .ok_or_else(|| WipsawError::NotFound {
                        entity: "tab",
                        value: tab_ref.to_string(),
                    })
            })
            .transpose()?;
        if let Some(tab) = &bound_tab {
            if let Some(thread_id) = &tab.codex_thread_id {
                return Err(WipsawError::InvalidInput {
                    field: "tab binding",
                    message: format!(
                        "tab '{}' is already bound to Codex thread '{}'; explicit rebinding is not implemented yet",
                        tab.name, thread_id
                    ),
                });
            }
            if let Some(tab_home_id) = &tab.codex_home_id
                && tab_home_id != &home.id
            {
                return Err(WipsawError::InvalidInput {
                    field: "tab binding",
                    message: format!(
                        "tab '{}' is configured for Codex home '{}', not '{}'",
                        tab.name, tab_home_id, home.name
                    ),
                });
            }
            if let Some(tab_account_id) = &tab.account_id
                && tab_account_id != &home.account_id
            {
                return Err(WipsawError::InvalidInput {
                    field: "tab binding",
                    message: format!(
                        "tab '{}' is configured for a different account than Codex home '{}'",
                        tab.name, home.name
                    ),
                });
            }
        }

        let effective_profile_ref = profile_ref.or_else(|| {
            bound_tab
                .as_ref()
                .and_then(|tab| tab.model_profile_id.as_deref())
        });
        let profile = effective_profile_ref
            .map(|reference| {
                self.registry
                    .model_profile_by_ref(reference)?
                    .ok_or_else(|| WipsawError::NotFound {
                        entity: "model profile",
                        value: reference.to_string(),
                    })
            })
            .transpose()?;
        let cwd = existing_directory(
            cwd.or_else(|| bound_tab.as_ref().map(|tab| tab.cwd.as_path()))
                .unwrap_or(Path::new(".")),
            "working directory",
        )?;
        if let Ok(workspace_id) = env::var("WIPSAW_MANAGER_WORKSPACE_ID") {
            let workspace = self.workspace(&workspace_id)?;
            self.ensure_manager_path_scope(&workspace, &cwd, "thread working directory")?;
        }

        let native = if handoff.is_some() {
            start_named_thread_with_handoff(&home, &cwd, &name, profile.as_ref(), handoff)?
        } else {
            start_named_thread(&home, &cwd, &name, profile.as_ref())?
        };
        let id = WipsawId::new(EntityKind::CodexThread);
        let inserted = self.registry.insert_codex_thread(NewCodexThread {
            id: id.as_str(),
            codex_home_id: &home.id,
            native_thread_id: &native.native_thread_id,
            name: &native.name,
            cwd: &native.cwd,
            model_profile_id: profile.as_ref().map(|profile| profile.id.as_str()),
            model: &native.model,
            model_provider: &native.model_provider,
            reasoning_effort: native.reasoning_effort.as_deref(),
            status: &native.status,
            rollout_path: native.rollout_path.as_deref(),
            native_created_at: native.native_created_at,
            bind_tab_id: bound_tab.as_ref().map(|tab| tab.id.as_str()),
        });
        if inserted.is_err() {
            let _ = archive_thread(&home, &native.native_thread_id);
        }
        inserted
    }

    /// Create, bind, and launch a fresh Codex session in one tab operation.
    /// Unlike `create_tab`, this never returns a shell-only tab.
    pub fn create_codex_tab(
        &mut self,
        workspace_ref: &str,
        name: &str,
        cwd: &Path,
        home_ref: Option<&str>,
        profile_ref: Option<&str>,
    ) -> Result<CodexTabSessionLaunch> {
        self.create_launched_codex_tab(workspace_ref, name, cwd, home_ref, profile_ref, None)
    }

    fn create_launched_codex_tab(
        &mut self,
        workspace_ref: &str,
        name: &str,
        cwd: &Path,
        home_ref: Option<&str>,
        profile_ref: Option<&str>,
        handoff: Option<&str>,
    ) -> Result<CodexTabSessionLaunch> {
        let workspace = self.workspace(workspace_ref)?;
        let tab = self.create_tab(
            &workspace.id,
            name,
            Some(cwd),
            TabLaunchSettings {
                account: None,
                codex_home: home_ref,
                model_profile: profile_ref,
            },
        )?;
        let home_id = tab
            .codex_home_id
            .clone()
            .ok_or_else(|| WipsawError::InvalidInput {
                field: "Codex home",
                message: "no Codex home is available for the new Codex tab".to_string(),
            });
        let home_id = match home_id {
            Ok(home_id) => home_id,
            Err(error) => {
                self.rollback_new_tab(&workspace, &tab);
                return Err(error);
            }
        };
        let thread = self.create_codex_thread_seeded(
            name,
            &home_id,
            Some(&tab.cwd),
            profile_ref,
            Some((&workspace.id, &tab.id)),
            handoff,
        );
        let thread = match thread {
            Ok(thread) => thread,
            Err(error) => {
                self.rollback_new_tab(&workspace, &tab);
                return Err(error);
            }
        };
        let launch = self.resume_codex_thread(&thread.id, &workspace.id, &tab.id);
        let launch = match launch {
            Ok(launch) => launch,
            Err(error) => {
                self.rollback_codex_tab(&workspace, &tab, &thread);
                return Err(error);
            }
        };
        let tab = self
            .registry
            .tab_by_ref(&workspace.id, &tab.id)?
            .ok_or_else(|| WipsawError::NotFound {
                entity: "tab",
                value: tab.id.clone(),
            })?;
        Ok(CodexTabSessionLaunch {
            workspace_id: workspace.id,
            workspace_name: workspace.name,
            tab,
            thread,
            launch,
        })
    }

    /// Create, seed, bind, and launch a Codex tab as one manager operation.
    /// Any resource created before a failure is removed on a best-effort basis
    /// so a manager retry does not multiply empty tabs or orphan threads.
    #[allow(clippy::too_many_arguments)]
    pub fn create_handoff_tab(
        &mut self,
        workspace_ref: &str,
        name: &str,
        cwd: &Path,
        home_ref: Option<&str>,
        profile_ref: Option<&str>,
        handoff: &str,
        source_codex_home_id: &str,
        source_native_thread_id: &str,
    ) -> Result<CodexHandoffLaunch> {
        let handoff = handoff.trim();
        if handoff.is_empty() || handoff.chars().count() > 48_000 {
            return Err(WipsawError::InvalidInput {
                field: "Codex handoff summary",
                message: "must contain between 1 and 48,000 characters".to_string(),
            });
        }
        let created = self.create_launched_codex_tab(
            workspace_ref,
            name,
            cwd,
            home_ref,
            profile_ref,
            Some(handoff),
        )?;
        Ok(CodexHandoffLaunch {
            workspace_id: created.workspace_id,
            workspace_name: created.workspace_name,
            tab: created.tab,
            thread: created.thread,
            launch: created.launch,
            source_codex_home_id: source_codex_home_id.to_string(),
            source_native_thread_id: source_native_thread_id.to_string(),
        })
    }

    /// Adopt an existing native Codex conversation into Wipsaw and open that
    /// exact session in a durable tab. No new Codex thread is created and no
    /// history is summarized or copied.
    #[allow(clippy::too_many_arguments)]
    pub fn import_codex_session(
        &mut self,
        workspace_ref: &str,
        source_home_ref: &str,
        native_thread_id: &str,
        target_tab_ref: Option<&str>,
        new_tab_name: Option<&str>,
        replace_existing: bool,
    ) -> Result<CodexSessionImport> {
        let native_thread_id = native_thread_id.trim();
        if native_thread_id.is_empty() || native_thread_id.len() > 128 {
            return Err(WipsawError::InvalidInput {
                field: "native Codex thread ID",
                message: "must contain between 1 and 128 characters".to_string(),
            });
        }
        let workspace = self.workspace(workspace_ref)?;
        self.ensure_workspace_runtime(&workspace, false)?;
        let home = self
            .registry
            .codex_home_by_ref(source_home_ref)?
            .ok_or_else(|| WipsawError::NotFound {
                entity: "Codex home",
                value: source_home_ref.to_string(),
            })?;
        let active_elsewhere = native_thread_has_active_writer(&home, native_thread_id)?;
        let mut native = if active_elsewhere {
            imported_native_metadata(inspect_native_thread(&home, native_thread_id)?)
        } else {
            resume_native_thread(&home, native_thread_id)?
        };
        if !native.cwd.is_dir() {
            return Err(WipsawError::InvalidInput {
                field: "imported session working directory",
                message: format!(
                    "'{}' no longer exists; restore it before importing session '{}'",
                    native.cwd.display(),
                    native_thread_id
                ),
            });
        }
        self.ensure_manager_path_scope(
            &workspace,
            &native.cwd,
            "imported session working directory",
        )?;
        native.name = validate_display_name(
            "imported Codex thread name",
            &truncate_display_name(&native.name, 96),
        )?;

        let existing_thread = self
            .registry
            .codex_thread_by_native_id(&home.id, native_thread_id)?;
        let existing_binding = existing_thread
            .as_ref()
            .map(|thread| self.registry.tab_by_codex_thread(&thread.id))
            .transpose()?
            .flatten();

        let (tab, tab_created) = if let Some(tab_ref) = target_tab_ref {
            let tab = self
                .registry
                .tab_by_ref(&workspace.id, tab_ref)?
                .ok_or_else(|| WipsawError::NotFound {
                    entity: "tab",
                    value: tab_ref.to_string(),
                })?;
            (tab, false)
        } else if let Some(tab) = existing_binding.clone() {
            if tab.workspace_id != workspace.id {
                let existing_workspace = self.workspace(&tab.workspace_id)?;
                return Err(WipsawError::InvalidInput {
                    field: "session import",
                    message: format!(
                        "native session '{}' is already tracked in workspace '{}' tab '{}' ({}); open that tab or explicitly choose a replacement target",
                        native_thread_id, existing_workspace.name, tab.name, tab.id
                    ),
                });
            }
            (tab, false)
        } else {
            let requested_name = new_tab_name.unwrap_or(&native.name);
            let requested_name = validate_display_name("imported tab name", requested_name)?;
            let tab_name = if new_tab_name.is_some() {
                requested_name
            } else {
                self.available_tab_name(&workspace.id, &requested_name)?
            };
            let tab = self.create_tab(
                &workspace.id,
                &tab_name,
                Some(&native.cwd),
                TabLaunchSettings {
                    account: Some(&home.account_id),
                    codex_home: Some(&home.id),
                    model_profile: None,
                },
            )?;
            (tab, true)
        };

        if is_manager_tab(&tab) {
            if tab_created {
                self.rollback_new_tab(&workspace, &tab);
            }
            return Err(WipsawError::InvalidInput {
                field: "session import tab",
                message: "manager tabs cannot host an interactive Codex session".to_string(),
            });
        }
        if let Some(bound) = &existing_binding
            && bound.id != tab.id
        {
            if tab_created {
                self.rollback_new_tab(&workspace, &tab);
            }
            return Err(WipsawError::InvalidInput {
                field: "session import",
                message: format!(
                    "native session '{}' is already bound to tab '{}' ({}); Wipsaw will not run one conversation in two tabs",
                    native_thread_id, bound.name, bound.id
                ),
            });
        }

        let replaced_thread_id = tab
            .codex_thread_id
            .as_ref()
            .filter(|thread_id| {
                existing_thread
                    .as_ref()
                    .is_none_or(|thread| thread_id.as_str() != thread.id)
            })
            .cloned();
        if replaced_thread_id.is_some() && !replace_existing {
            if tab_created {
                self.rollback_new_tab(&workspace, &tab);
            }
            return Err(WipsawError::InvalidInput {
                field: "session import tab",
                message: format!(
                    "tab '{}' already has a different Codex thread; pass replaceExisting=true to retain that old thread unbound and open the imported session here",
                    tab.name
                ),
            });
        }

        let previous_thread_name = existing_thread
            .as_ref()
            .filter(|thread| thread.name != tab.name)
            .map(|thread| thread.name.clone());
        let thread_result = (|| -> Result<(CodexThread, bool)> {
            if let Some(thread) = existing_thread {
                let thread = if thread.name == tab.name {
                    thread
                } else {
                    self.registry.rename_codex_thread(&thread.id, &tab.name)?
                };
                Ok((thread, false))
            } else {
                let id = WipsawId::new(EntityKind::CodexThread);
                let thread = self.registry.insert_codex_thread(NewCodexThread {
                    id: id.as_str(),
                    codex_home_id: &home.id,
                    native_thread_id: &native.native_thread_id,
                    // The tab is Wipsaw's durable, user-controlled label. Native
                    // sessions often have no name and expose an entire first prompt
                    // as their preview, which is poor navigator chrome.
                    name: &tab.name,
                    cwd: &native.cwd,
                    model_profile_id: None,
                    model: &native.model,
                    model_provider: &native.model_provider,
                    reasoning_effort: native.reasoning_effort.as_deref(),
                    status: &native.status,
                    rollout_path: native.rollout_path.as_deref(),
                    native_created_at: native.native_created_at,
                    bind_tab_id: None,
                })?;
                Ok((thread, true))
            }
        })();
        let (thread, thread_imported) = match thread_result {
            Ok(result) => result,
            Err(error) => {
                if tab_created {
                    self.rollback_new_tab(&workspace, &tab);
                }
                return Err(error);
            }
        };

        let original_tab = tab.clone();
        let result = (|| -> Result<(Tab, Option<CodexThreadLaunch>, Option<String>)> {
            let tab = self.registry.bind_tab_to_codex_thread(&tab.id, &thread)?;
            let already_running_here = self.tmux.window_has_managed_thread(
                &workspace.tmux_session,
                &tab.tmux_window_id,
                &thread.id,
            )?;
            if active_elsewhere && !already_running_here {
                if replaced_thread_id.is_some() {
                    self.tmux.reset_tab_to_shell(
                        &workspace.tmux_session,
                        &tab.tmux_window_id,
                        &tab.cwd,
                    )?;
                } else if !self.tab_is_at_shell(&workspace, &tab)? {
                    return Err(WipsawError::InvalidInput {
                        field: "session import tab",
                        message: format!(
                            "tab '{}' is busy and the imported conversation already has an active writer elsewhere",
                            tab.name
                        ),
                    });
                }
                let reason = format!(
                    "native session '{}' is currently open in another Codex process; its exact mapping is saved and Wipsaw will open it automatically the next time this workspace or tab is opened after that writer closes",
                    thread.native_thread_id
                );
                Ok((tab, None, Some(reason)))
            } else {
                let launch = self.launch_codex_thread(
                    &thread,
                    &home,
                    &workspace,
                    &tab,
                    replaced_thread_id.is_some(),
                )?;
                Ok((tab, Some(launch), None))
            }
        })();
        let (tab, launch, deferred_reason) = match result {
            Ok(result) => result,
            Err(error) => {
                if tab_created {
                    self.rollback_new_tab(&workspace, &original_tab);
                } else {
                    let _ = self.registry.restore_tab_binding(&original_tab);
                }
                if thread_imported {
                    let _ = self.registry.delete_codex_thread(&thread.id);
                } else if let Some(previous_name) = &previous_thread_name {
                    let _ = self.registry.rename_codex_thread(&thread.id, previous_name);
                }
                return Err(error);
            }
        };

        Ok(CodexSessionImport {
            workspace_id: workspace.id,
            workspace_name: workspace.name,
            tab,
            thread,
            launch,
            deferred_reason,
            thread_imported,
            tab_created,
            replaced_thread_id,
        })
    }

    fn rollback_new_tab(&mut self, workspace: &Workspace, tab: &Tab) {
        let _ = self
            .tmux
            .kill_tab(&workspace.tmux_session, &tab.tmux_window_id);
        let _ = self.registry.delete_tab(&tab.id);
    }

    fn rollback_codex_tab(&mut self, workspace: &Workspace, tab: &Tab, thread: &CodexThread) {
        self.rollback_new_tab(workspace, tab);
        if let Ok(Some(home)) = self.registry.codex_home_by_ref(&thread.codex_home_id) {
            let _ = delete_thread(&home, &thread.native_thread_id);
        }
        let _ = self.registry.delete_codex_thread(&thread.id);
    }

    pub fn list_codex_threads(&self, home_ref: Option<&str>) -> Result<Vec<CodexThread>> {
        let home_id = home_ref
            .map(|reference| {
                self.registry
                    .codex_home_by_ref(reference)?
                    .map(|home| home.id)
                    .ok_or_else(|| WipsawError::NotFound {
                        entity: "Codex home",
                        value: reference.to_string(),
                    })
            })
            .transpose()?;
        if let Ok(workspace_id) = env::var("WIPSAW_MANAGER_WORKSPACE_ID") {
            let mut threads = self.registry.list_workspace_codex_threads(&workspace_id)?;
            if let Some(home_id) = home_id {
                threads.retain(|thread| thread.codex_home_id == home_id);
            }
            Ok(threads)
        } else {
            self.registry.list_codex_threads(home_id.as_deref())
        }
    }

    pub fn ensure_lumbergh(&self) -> Result<ManagerSession> {
        if let Some(session) = self.registry.manager_session_for_workspace(None)? {
            return Ok(session);
        }
        let home = self.manager_source_home(None)?;
        let cwd = env::current_dir()?;
        let id = WipsawId::new(EntityKind::ManagerSession);
        self.registry.insert_manager_session(NewManagerSession {
            id: id.as_str(),
            kind: ManagerKind::Lumbergh,
            workspace_id: None,
            source_codex_home_id: &home.id,
            cwd: &cwd,
            model: MANAGER_MODEL,
            reasoning_effort: MANAGER_REASONING_EFFORT,
        })
    }

    pub fn ensure_middle_manager(&self, workspace_ref: &str) -> Result<ManagerSession> {
        let workspace = self.workspace(workspace_ref)?;
        if let Some(session) = self
            .registry
            .manager_session_for_workspace(Some(&workspace.id))?
        {
            return Ok(session);
        }
        let home = self.manager_source_home(Some(&workspace))?;
        let id = WipsawId::new(EntityKind::ManagerSession);
        self.registry.insert_manager_session(NewManagerSession {
            id: id.as_str(),
            kind: ManagerKind::MiddleManager,
            workspace_id: Some(&workspace.id),
            source_codex_home_id: &home.id,
            cwd: &workspace.cwd,
            model: MANAGER_MODEL,
            reasoning_effort: MANAGER_REASONING_EFFORT,
        })
    }

    pub fn manager_messages(
        &self,
        workspace_ref: Option<&str>,
    ) -> Result<(ManagerSession, Vec<ManagerMessage>)> {
        let session = match workspace_ref {
            Some(workspace) => self.ensure_middle_manager(workspace)?,
            None => self.ensure_lumbergh()?,
        };
        let messages = self
            .registry
            .list_manager_messages(&session.id, MANAGER_TRANSCRIPT_MESSAGE_LIMIT)?;
        Ok((session, messages))
    }

    pub fn manager_context_scope(&self, session: &ManagerSession) -> Result<ManagerContextScope> {
        match session.kind {
            ManagerKind::Lumbergh => Ok(ManagerContextScope::machine_wide(session.cwd.clone())),
            ManagerKind::MiddleManager => {
                let workspace_id =
                    session
                        .workspace_id
                        .as_deref()
                        .ok_or_else(|| WipsawError::InvalidInput {
                            field: "Middle Manager",
                            message: "has no workspace".to_string(),
                        })?;
                let roots = self
                    .registry
                    .list_workspace_contexts(workspace_id)?
                    .into_iter()
                    .map(|context| context.path)
                    .collect();
                Ok(ManagerContextScope::workspace(session.cwd.clone(), roots))
            }
        }
    }

    pub fn start_manager_turn(
        &self,
        workspace_ref: Option<&str>,
        prompt: &str,
    ) -> Result<ManagerTurnHandle> {
        let prompt = prompt.trim();
        if prompt.is_empty() {
            return Err(WipsawError::InvalidInput {
                field: "manager message",
                message: "must not be empty".to_string(),
            });
        }
        if prompt.chars().count() > 12_000 {
            return Err(WipsawError::InvalidInput {
                field: "manager message",
                message: "must be 12,000 characters or fewer".to_string(),
            });
        }
        let session = match workspace_ref {
            Some(workspace) => self.ensure_middle_manager(workspace)?,
            None => self.ensure_lumbergh()?,
        };
        let source_home = self
            .registry
            .codex_home_by_ref(&session.source_codex_home_id)?
            .ok_or_else(|| WipsawError::NotFound {
                entity: "Codex home",
                value: session.source_codex_home_id.clone(),
            })?;
        let launcher_home = BaseDirs::new()
            .map(|base| base.home_dir().to_path_buf())
            .ok_or_else(|| WipsawError::InvalidInput {
                field: "home directory",
                message: "could not resolve the Codex launcher home".to_string(),
            })?;
        let scope = self.manager_context_scope(&session)?;
        let runtime = prepare_runtime(&self.paths, &source_home, &session, &scope, &launcher_home)?;
        let model_prompt = expand_prompt_references(prompt, &scope)?;
        let executable = env::current_exe()?;
        let mut environment = vec![
            (
                OsString::from("WIPSAW_MANAGER_EXECUTABLE"),
                executable.as_os_str().to_owned(),
            ),
            (
                OsString::from("WIPSAW_CONFIG_DIR"),
                self.paths.config_dir.as_os_str().to_owned(),
            ),
            (
                OsString::from("WIPSAW_STATE_DIR"),
                self.paths.state_dir.as_os_str().to_owned(),
            ),
            (
                OsString::from("WIPSAW_DATA_DIR"),
                self.paths.data_dir.as_os_str().to_owned(),
            ),
            (
                OsString::from("WIPSAW_RUNTIME_DIR"),
                self.paths.runtime_dir.as_os_str().to_owned(),
            ),
            (
                OsString::from("WIPSAW_TMUX_SOCKET"),
                OsString::from(self.tmux.socket_name()),
            ),
            (
                OsString::from("WIPSAW_TMUX_BIN"),
                self.tmux.binary().as_os_str().to_owned(),
            ),
            (
                OsString::from("WIPSAW_MANAGER_CONTEXT_SCOPE"),
                OsString::from(serde_json::to_string(&scope)?),
            ),
        ];
        if let Some(workspace_id) = &session.workspace_id {
            environment.push((
                OsString::from("WIPSAW_MANAGER_WORKSPACE_ID"),
                OsString::from(workspace_id),
            ));
        }
        self.registry
            .append_manager_message(&session.id, "user", prompt)?;
        self.registry.mark_manager_working(&session.id)?;
        let receiver = spawn_turn(ManagerTurnRequest {
            session_id: session.id.clone(),
            native_thread_id: session.native_thread_id.clone(),
            codex_binary: source_home.codex_binary,
            runtime,
            prompt: model_prompt,
            environment,
        });
        Ok(ManagerTurnHandle { session, receiver })
    }

    pub fn complete_manager_turn(&self, result: &ManagerTurnResult) -> Result<()> {
        self.registry.complete_manager_turn(
            &result.session_id,
            &result.native_thread_id,
            &result.message,
        )?;
        Ok(())
    }

    pub fn fail_manager_turn(&self, session_id: &str, error: &str) -> Result<()> {
        self.registry.fail_manager_turn(session_id, error)?;
        Ok(())
    }

    fn manager_source_home(&self, workspace: Option<&Workspace>) -> Result<CodexHome> {
        if let Some(workspace) = workspace
            && let Some(tab) = self.manager_tab(&workspace.id)?
            && let Some(home_id) = tab.codex_home_id
            && let Some(home) = self.registry.codex_home_by_ref(&home_id)?
        {
            return Ok(home);
        }
        self.registry
            .preferred_codex_home(None)?
            .ok_or_else(|| WipsawError::InvalidInput {
                field: "Codex home",
                message: "no Codex home is available for the manager".to_string(),
            })
    }

    fn manager_tab(&self, workspace_id: &str) -> Result<Option<Tab>> {
        if let Some(tab) = self.registry.tab_by_ref(workspace_id, "middle-manager")? {
            return Ok(Some(tab));
        }
        self.registry.tab_by_ref(workspace_id, "manager")
    }

    pub fn inspect_codex_thread(&self, reference: &str) -> Result<CodexThreadInspection> {
        let managed = self
            .registry
            .codex_thread_by_ref(reference)?
            .ok_or_else(|| WipsawError::NotFound {
                entity: "Codex thread",
                value: reference.to_string(),
            })?;
        self.ensure_manager_thread_scope(&managed)?;
        let home = self
            .registry
            .codex_home_by_ref(&managed.codex_home_id)?
            .ok_or_else(|| WipsawError::NotFound {
                entity: "Codex home",
                value: managed.codex_home_id.clone(),
            })?;
        let native = inspect_native_thread(&home, &managed.native_thread_id)?;
        Ok(CodexThreadInspection { managed, native })
    }

    pub fn resume_codex_thread(
        &mut self,
        thread_ref: &str,
        workspace_ref: &str,
        tab_ref: &str,
    ) -> Result<CodexThreadLaunch> {
        let thread = self
            .registry
            .codex_thread_by_ref(thread_ref)?
            .ok_or_else(|| WipsawError::NotFound {
                entity: "Codex thread",
                value: thread_ref.to_string(),
            })?;
        self.ensure_manager_thread_scope(&thread)?;
        let home = self
            .registry
            .codex_home_by_ref(&thread.codex_home_id)?
            .ok_or_else(|| WipsawError::NotFound {
                entity: "Codex home",
                value: thread.codex_home_id.clone(),
            })?;
        let workspace = self.workspace(workspace_ref)?;
        self.ensure_workspace_runtime(&workspace, false)?;
        let tab = self
            .registry
            .tab_by_ref(&workspace.id, tab_ref)?
            .ok_or_else(|| WipsawError::NotFound {
                entity: "tab",
                value: tab_ref.to_string(),
            })?;

        if let Some(bound_thread_id) = &tab.codex_thread_id
            && bound_thread_id != &thread.id
        {
            return Err(WipsawError::InvalidInput {
                field: "tab binding",
                message: format!(
                    "tab '{}' is already bound to Codex thread '{}'",
                    tab.name, bound_thread_id
                ),
            });
        }
        if let Some(home_id) = &tab.codex_home_id
            && home_id != &thread.codex_home_id
        {
            return Err(WipsawError::InvalidInput {
                field: "tab binding",
                message: format!(
                    "tab '{}' uses Codex home '{}', while thread '{}' uses '{}'",
                    tab.name, home_id, thread.name, thread.codex_home_id
                ),
            });
        }
        if let Some(account_id) = &tab.account_id
            && account_id != &thread.account_id
        {
            return Err(WipsawError::InvalidInput {
                field: "tab binding",
                message: format!(
                    "tab '{}' uses a different account than thread '{}'",
                    tab.name, thread.name
                ),
            });
        }
        if let Some(profile_id) = &tab.model_profile_id
            && Some(profile_id.as_str()) != thread.model_profile_id.as_deref()
        {
            return Err(WipsawError::InvalidInput {
                field: "tab binding",
                message: format!(
                    "tab '{}' uses model profile '{}', while thread '{}' uses '{}'",
                    tab.name,
                    profile_id,
                    thread.name,
                    thread.model_profile_id.as_deref().unwrap_or("no profile")
                ),
            });
        }
        let tab = self.registry.bind_tab_to_codex_thread(&tab.id, &thread)?;
        self.launch_codex_thread(&thread, &home, &workspace, &tab, false)
    }

    fn launch_codex_thread(
        &self,
        thread: &CodexThread,
        home: &CodexHome,
        workspace: &Workspace,
        tab: &Tab,
        replace_foreground: bool,
    ) -> Result<CodexThreadLaunch> {
        if !thread.cwd.is_dir() {
            return Err(WipsawError::InvalidInput {
                field: "thread working directory",
                message: format!("'{}' is no longer a directory", thread.cwd.display()),
            });
        }
        if !home.path.is_dir() {
            return Err(WipsawError::InvalidInput {
                field: "Codex home path",
                message: format!("'{}' is no longer a directory", home.path.display()),
            });
        }
        if !self.tmux.session_exists(&workspace.tmux_session)? {
            return Err(WipsawError::InvalidInput {
                field: "workspace",
                message: format!(
                    "workspace '{}' is registered but its tmux session is not running",
                    workspace.name
                ),
            });
        }

        let return_shell = return_shell_path();
        let already_running = self.tmux.window_has_managed_thread(
            &workspace.tmux_session,
            &tab.tmux_window_id,
            &thread.id,
        )?;
        if !already_running {
            if native_thread_has_active_writer(home, &thread.native_thread_id)? {
                return Err(WipsawError::InvalidInput {
                    field: "Codex session writer",
                    message: format!(
                        "native session '{}' is already open in another Codex process; close that process and reopen this tab so Wipsaw can resume it safely",
                        thread.native_thread_id
                    ),
                });
            }
            if !replace_foreground && !self.tab_is_at_shell(workspace, tab)? {
                return Err(WipsawError::InvalidInput {
                    field: "Codex tab",
                    message: format!(
                        "tab '{}' is busy; stop its foreground process before reopening thread '{}'",
                        tab.name, thread.name
                    ),
                });
            }
            self.tmux.launch_codex_in_tab(CodexTabLaunch {
                session: &workspace.tmux_session,
                window_id: &tab.tmux_window_id,
                cwd: &thread.cwd,
                codex_home: &home.path,
                codex_binary: &home.codex_binary,
                managed_thread_id: &thread.id,
                native_thread_id: &thread.native_thread_id,
                return_shell: &return_shell,
            })?;
        }
        Ok(CodexThreadLaunch {
            thread_id: thread.id.clone(),
            native_thread_id: thread.native_thread_id.clone(),
            workspace_id: workspace.id.clone(),
            tab_id: tab.id.clone(),
            tmux_session: workspace.tmux_session.clone(),
            tmux_window_id: tab.tmux_window_id.clone(),
            codex_home_id: home.id.clone(),
            cwd: thread.cwd.clone(),
            return_shell,
            launched: !already_running,
        })
    }

    /// Enter the Codex thread assigned to the shell's current managed tab,
    /// creating and binding one on first use. Successful execution replaces
    /// the shortcut process with the real Codex CLI.
    pub fn run_codex_shortcut(&mut self, args: &[OsString]) -> Result<()> {
        let (workspace, tab) = self.current_tab()?;
        if is_manager_tab(&tab) {
            if !args.is_empty() {
                return Err(WipsawError::InvalidInput {
                    field: "Middle Manager command",
                    message: "manager tabs use the embedded Wipsaw composer and do not accept raw Codex arguments"
                        .to_string(),
                });
            }
            return self.exec_navigator("middle-manager", Some(&workspace.id));
        }
        let thread_name = tab.name.clone();
        let thread = self.ensure_tab_thread(&workspace, &tab, &thread_name)?;
        self.exec_codex_thread(&thread, args)
    }

    /// Open the current workspace's embedded Middle Manager.
    pub fn run_manager_shortcut(&mut self) -> Result<()> {
        let (workspace, _) = self.current_tab()?;
        self.exec_navigator("middle-manager", Some(&workspace.id))
    }

    /// Open the single top-level Lumbergh manager on the dashboard.
    pub fn run_lumbergh_shortcut(&self) -> Result<()> {
        self.exec_navigator("lumbergh", None)
    }

    /// Lazily create the named Codex thread for a tab and launch it when the
    /// pane is currently at a shell prompt.
    pub fn start_tab_codex(&mut self, workspace_ref: &str, tab_ref: &str) -> Result<CodexThread> {
        let workspace = self.workspace(workspace_ref)?;
        self.ensure_workspace_runtime(&workspace, true)?;
        let tab = self
            .registry
            .tab_by_ref(&workspace.id, tab_ref)?
            .ok_or_else(|| WipsawError::NotFound {
                entity: "tab",
                value: tab_ref.to_string(),
            })?;
        if is_manager_tab(&tab) {
            return Err(WipsawError::InvalidInput {
                field: "Middle Manager tab",
                message:
                    "open it inside Wipsaw; manager turns run through resumable `codex exec --json`"
                        .to_string(),
            });
        }
        let name = tab.name.clone();
        let thread = self.ensure_tab_thread(&workspace, &tab, &name)?;
        if self.tab_should_start_thread(&workspace, &tab, &thread)? {
            self.resume_codex_thread(&thread.id, &workspace.id, &tab.id)?;
        }
        Ok(thread)
    }

    fn exec_navigator(&self, start: &str, workspace_id: Option<&str>) -> Result<()> {
        let executable = env::current_exe()?;
        let mut command = Command::new(&executable);
        command.env("WIPSAW_TUI_START", start);
        if let Some(workspace_id) = workspace_id {
            command.env("WIPSAW_MANAGER_WORKSPACE", workspace_id);
        }
        let error = command.exec();
        Err(error.into())
    }

    /// Open a managed thread from the navigator. An unbound thread receives a
    /// new tab in the preferred workspace; an existing live Codex process is
    /// selected without being killed and respawned.
    pub fn open_codex_thread(
        &mut self,
        thread_ref: &str,
        preferred_workspace_ref: Option<&str>,
    ) -> Result<bool> {
        let thread = self
            .registry
            .codex_thread_by_ref(thread_ref)?
            .ok_or_else(|| WipsawError::NotFound {
                entity: "Codex thread",
                value: thread_ref.to_string(),
            })?;

        let (workspace, tab) = if let Some(tab) = self.registry.tab_by_codex_thread(&thread.id)? {
            let workspace = self.workspace(&tab.workspace_id)?;
            (workspace, tab)
        } else {
            let workspace = match preferred_workspace_ref {
                Some(reference) => self.workspace(reference)?,
                None => self
                    .list_workspaces()?
                    .into_iter()
                    .find_map(|(workspace, live)| live.then_some(workspace))
                    .ok_or_else(|| WipsawError::InvalidInput {
                        field: "workspace",
                        message: "create a live workspace before opening an unbound Codex thread"
                            .to_string(),
                    })?,
            };
            let tab_name = self.available_tab_name(&workspace.id, &thread.name)?;
            let tab = self.create_tab(
                &workspace.id,
                &tab_name,
                Some(&thread.cwd),
                TabLaunchSettings {
                    account: Some(&thread.account_id),
                    codex_home: Some(&thread.codex_home_id),
                    model_profile: thread.model_profile_id.as_deref(),
                },
            )?;
            (workspace, tab)
        };

        self.start_workspace(&workspace.id)?;
        let tab = self
            .registry
            .tab_by_ref(&workspace.id, &tab.id)?
            .ok_or_else(|| WipsawError::NotFound {
                entity: "tab",
                value: tab.id.clone(),
            })?;
        if self.tab_should_start_thread(&workspace, &tab, &thread)? {
            self.resume_codex_thread(&thread.id, &workspace.id, &tab.id)?;
        }
        self.activate_tab(&workspace.id, &tab.id)
    }

    pub fn current_managed_context(&self) -> Result<Option<(Workspace, Tab)>> {
        if let (Ok(session), Ok(window_id)) = (
            env::var("WIPSAW_PARENT_SESSION"),
            env::var("WIPSAW_PARENT_WINDOW"),
        ) && let Some(workspace) = self.registry.workspace_by_tmux_session(&session)?
        {
            let tab = self
                .registry
                .tab_by_tmux_window(&workspace.id, &window_id)?;
            return Ok(tab.map(|tab| (workspace, tab)));
        }

        if env::var_os("TMUX_PANE").is_none() {
            return Ok(None);
        }
        let context = match self.tmux.current_context() {
            Ok(context) => context,
            Err(_) => return Ok(None),
        };
        let Some(workspace) = self.registry.workspace_by_tmux_session(&context.session)? else {
            return Ok(None);
        };
        let tab = self
            .registry
            .tab_by_tmux_window(&workspace.id, &context.window_id)?;
        Ok(tab.map(|tab| (workspace, tab)))
    }

    fn current_workspace(&self) -> Result<Option<Workspace>> {
        Ok(self
            .current_managed_context()?
            .map(|(workspace, _)| workspace))
    }

    fn current_tab(&self) -> Result<(Workspace, Tab)> {
        if let Some(context) = self.current_managed_context()? {
            return Ok(context);
        }
        let context = self.tmux.current_context()?;
        let workspace = self
            .registry
            .workspace_by_tmux_session(&context.session)?
            .ok_or_else(|| WipsawError::InvalidInput {
                field: "terminal context",
                message: format!(
                    "tmux session '{}' is not managed by Wipsaw",
                    context.session
                ),
            })?;
        let tab = self
            .registry
            .tab_by_tmux_window(&workspace.id, &context.window_id)?
            .ok_or_else(|| WipsawError::InvalidInput {
                field: "terminal context",
                message: format!(
                    "tmux window '{}' is not registered in workspace '{}'",
                    context.window_name, workspace.name
                ),
            })?;
        Ok((workspace, tab))
    }

    fn ensure_tab_thread(
        &mut self,
        workspace: &Workspace,
        tab: &Tab,
        name: &str,
    ) -> Result<CodexThread> {
        if let Some(thread_id) = &tab.codex_thread_id {
            return self
                .registry
                .codex_thread_by_ref(thread_id)?
                .ok_or_else(|| WipsawError::NotFound {
                    entity: "Codex thread",
                    value: thread_id.clone(),
                });
        }

        let home = if let Some(home_id) = &tab.codex_home_id {
            self.registry.codex_home_by_ref(home_id)?
        } else {
            self.registry
                .preferred_codex_home(tab.account_id.as_deref())?
        }
        .ok_or_else(|| WipsawError::InvalidInput {
            field: "Codex home",
            message: "no Codex home is available; run `wipsaw home add` first".to_string(),
        })?;

        self.create_codex_thread(
            name,
            &home.id,
            Some(&tab.cwd),
            tab.model_profile_id.as_deref(),
            Some((&workspace.id, &tab.id)),
        )
    }

    fn exec_codex_thread(&self, thread: &CodexThread, args: &[OsString]) -> Result<()> {
        let home = self
            .registry
            .codex_home_by_ref(&thread.codex_home_id)?
            .ok_or_else(|| WipsawError::NotFound {
                entity: "Codex home",
                value: thread.codex_home_id.clone(),
            })?;
        let error = Command::new(&home.codex_binary)
            .arg("resume")
            .arg(&thread.native_thread_id)
            .args(args)
            .current_dir(&thread.cwd)
            .env("CODEX_HOME", &home.path)
            .env("WIPSAW_THREAD_ID", &thread.id)
            .env("WIPSAW_NATIVE_THREAD_ID", &thread.native_thread_id)
            .exec();
        Err(error.into())
    }

    fn tab_is_at_shell(&self, workspace: &Workspace, tab: &Tab) -> Result<bool> {
        let command = self
            .tmux
            .window_command(&workspace.tmux_session, &tab.tmux_window_id)?;
        let configured_shell_path = return_shell_path();
        let configured_shell = configured_shell_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("sh");
        Ok(command == configured_shell
            || matches!(
                command.as_str(),
                "bash" | "dash" | "fish" | "nu" | "sh" | "zsh"
            ))
    }

    fn tab_should_start_thread(
        &self,
        workspace: &Workspace,
        tab: &Tab,
        thread: &CodexThread,
    ) -> Result<bool> {
        if self.tmux.window_has_managed_thread(
            &workspace.tmux_session,
            &tab.tmux_window_id,
            &thread.id,
        )? {
            return Ok(false);
        }
        self.tab_is_at_shell(workspace, tab)
    }

    fn available_tab_name(&self, workspace_id: &str, requested: &str) -> Result<String> {
        let base = truncate_display_name(requested, 88);
        if self.registry.tab_by_ref(workspace_id, &base)?.is_none() {
            return Ok(base);
        }
        for suffix in 2..=999 {
            let candidate = format!("{} {suffix}", truncate_display_name(&base, 91));
            if self
                .registry
                .tab_by_ref(workspace_id, &candidate)?
                .is_none()
            {
                return Ok(candidate);
            }
        }
        Err(WipsawError::InvalidInput {
            field: "tab name",
            message: "could not derive a unique tab name".to_string(),
        })
    }

    fn resolve_tab_settings(
        &self,
        settings: TabLaunchSettings<'_>,
    ) -> Result<(Option<String>, Option<String>, Option<String>)> {
        let account = settings
            .account
            .map(|reference| {
                self.registry
                    .account_by_ref(reference)?
                    .ok_or_else(|| WipsawError::NotFound {
                        entity: "account",
                        value: reference.to_string(),
                    })
            })
            .transpose()?;
        let explicit_home = settings
            .codex_home
            .map(|reference| {
                self.registry
                    .codex_home_by_ref(reference)?
                    .ok_or_else(|| WipsawError::NotFound {
                        entity: "Codex home",
                        value: reference.to_string(),
                    })
            })
            .transpose()?;
        let home = match explicit_home {
            Some(home) => Some(home),
            None => self
                .registry
                .preferred_codex_home(account.as_ref().map(|account| account.id.as_str()))?,
        };
        if let (Some(account), Some(home)) = (&account, &home)
            && account.id != home.account_id
        {
            return Err(WipsawError::InvalidInput {
                field: "tab account/home",
                message: format!(
                    "Codex home '{}' belongs to account '{}', not '{}'",
                    home.name, home.account_alias, account.alias
                ),
            });
        }
        let profile = settings
            .model_profile
            .map(|reference| {
                self.registry
                    .model_profile_by_ref(reference)?
                    .ok_or_else(|| WipsawError::NotFound {
                        entity: "model profile",
                        value: reference.to_string(),
                    })
            })
            .transpose()?;
        let account_id = account
            .map(|account| account.id)
            .or_else(|| home.as_ref().map(|home| home.account_id.clone()));
        Ok((
            account_id,
            home.map(|home| home.id),
            profile.map(|profile| profile.id),
        ))
    }
}

fn existing_directory(path: &Path, field: &'static str) -> Result<PathBuf> {
    let path = absolute_path(path)?;
    if !path.is_dir() {
        return Err(WipsawError::InvalidInput {
            field,
            message: format!("'{}' is not a directory", path.display()),
        });
    }
    Ok(fs::canonicalize(path)?)
}

fn absolute_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    Ok(std::env::current_dir()?.join(path))
}

fn resolve_executable_path(path: &Path, excluded_dir: Option<&Path>) -> Result<PathBuf> {
    let resolved = if path.components().count() > 1 || path.is_absolute() {
        if !path.is_file() {
            return Err(WipsawError::ExecutableUnavailable {
                program: path.display().to_string(),
                detail: "path is not a file".to_string(),
            });
        }
        fs::canonicalize(path)?
    } else {
        find_executable(path, excluded_dir).ok_or_else(|| WipsawError::ExecutableUnavailable {
            program: path.display().to_string(),
            detail: "executable was not found on PATH".to_string(),
        })?
    };
    let launcher_home = BaseDirs::new()
        .map(|base| base.home_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("/"));
    let candidate = std::process::Command::new(&resolved)
        .arg("--version")
        .env("PATH", codex_launch_path(&resolved, &launcher_home))
        .output();
    match candidate {
        Ok(output) if output.status.success() => Ok(resolved),
        Ok(output) => Err(WipsawError::ExecutableUnavailable {
            program: resolved.display().to_string(),
            detail: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        }),
        Err(error) => Err(WipsawError::ExecutableUnavailable {
            program: resolved.display().to_string(),
            detail: error.to_string(),
        }),
    }
}

fn find_executable(name: &Path, excluded_dir: Option<&Path>) -> Option<PathBuf> {
    let excluded = excluded_dir.and_then(|path| fs::canonicalize(path).ok());
    env::var_os("PATH")
        .into_iter()
        .flat_map(|value| env::split_paths(&value).collect::<Vec<_>>())
        .map(|directory| directory.join(name))
        .find(|candidate| {
            candidate.is_file()
                && candidate
                    .metadata()
                    .is_ok_and(|metadata| metadata.permissions().mode() & 0o111 != 0)
                && excluded.as_ref().is_none_or(|excluded| {
                    candidate
                        .parent()
                        .and_then(|parent| fs::canonicalize(parent).ok())
                        .as_ref()
                        != Some(excluded)
                })
        })
        .and_then(|candidate| fs::canonicalize(candidate).ok())
}

fn active_codex_home() -> Option<PathBuf> {
    env::var_os("CODEX_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| BaseDirs::new().map(|base| base.home_dir().join(".codex")))
}

fn auto_adopt_disabled() -> bool {
    env::var("WIPSAW_AUTO_ADOPT_CODEX").is_ok_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "no"
        )
    })
}

fn return_shell_path() -> PathBuf {
    env::var_os("WIPSAW_SHELL")
        .filter(|value| !value.is_empty())
        .or_else(|| env::var_os("SHELL").filter(|value| !value.is_empty()))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/bin/sh"))
}

fn imported_native_metadata(inspection: NativeCodexThreadInspection) -> NativeCodexThread {
    let name = inspection
        .name
        .filter(|name| !name.trim().is_empty())
        .or_else(|| {
            inspection
                .preview
                .lines()
                .find(|line| !line.trim().is_empty())
                .map(str::to_string)
        })
        .unwrap_or_else(|| {
            format!(
                "Codex {}",
                inspection
                    .native_thread_id
                    .get(..8)
                    .unwrap_or(&inspection.native_thread_id)
            )
        });
    NativeCodexThread {
        native_thread_id: inspection.native_thread_id,
        name,
        cwd: inspection.cwd,
        // `thread/read` intentionally omits resolved model settings. The CLI
        // restores them from the original rollout when Wipsaw opens the tab.
        model: "saved-session".to_string(),
        model_provider: inspection.model_provider,
        reasoning_effort: None,
        status: inspection.status,
        rollout_path: inspection.rollout_path,
        native_created_at: inspection.native_created_at,
    }
}

fn is_manager_tab(tab: &Tab) -> bool {
    tab.name.eq_ignore_ascii_case("middle-manager") || tab.name.eq_ignore_ascii_case("manager")
}

fn truncate_display_name(value: &str, max_chars: usize) -> String {
    let mut result = value.chars().take(max_chars).collect::<String>();
    while result.ends_with(char::is_whitespace) {
        result.pop();
    }
    if result.is_empty() {
        "codex".to_string()
    } else {
        result
    }
}
