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
    CodexHomeProbe, NativeCodexThreadInspection, archive_thread,
    inspect_thread as inspect_native_thread, probe_home, start_named_thread,
};
use crate::error::{Result, WipsawError};
use crate::id::{EntityKind, WipsawId};
use crate::model::{
    Account, AccountAuthKind, AccountOwnerKind, CodexHome, CodexThread, ModelProfile,
    ModelProfileSettings, Tab, Workspace, validate_credential_ref, validate_display_name,
    validate_model_name, validate_profile_settings,
};
use crate::paths::AppPaths;
use crate::registry::{
    NewAccount, NewCodexHome, NewCodexThread, NewCurrentCodex, NewModelProfile, NewTab,
    NewWorkspace, Registry,
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
    pub manager_thread_id: String,
    pub native_manager_thread_id: String,
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
        self.registry
            .list_workspaces()?
            .into_iter()
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
        self.registry
            .workspace_by_ref(reference)?
            .ok_or_else(|| WipsawError::NotFound {
                entity: "workspace",
                value: reference.to_string(),
            })
    }

    pub fn attach_workspace(&mut self, reference: &str) -> Result<()> {
        self.activate_workspace(reference).map(|_| ())
    }

    /// Ensure the workspace has a live tmux session, reconcile every durable
    /// tab with a real window, and keep its persistent manager Codex process
    /// running. A stopped workspace is reconstructed from registry metadata.
    pub fn start_workspace(&mut self, reference: &str) -> Result<WorkspaceStart> {
        let workspace = self.workspace(reference)?;
        let restored = self.ensure_workspace_runtime(&workspace)?;
        let manager = self.start_tab_codex(&workspace.id, "manager")?;
        Ok(WorkspaceStart {
            workspace,
            restored,
            manager_thread_id: manager.id,
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

    fn ensure_workspace_runtime(&self, workspace: &Workspace) -> Result<bool> {
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
        if !tabs
            .iter()
            .any(|tab| tab.name.eq_ignore_ascii_case("manager"))
        {
            return Err(WipsawError::NotFound {
                entity: "manager tab",
                value: workspace.name.clone(),
            });
        }
        tabs.sort_by_key(|tab| !tab.name.eq_ignore_ascii_case("manager"));

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
        Ok(restored)
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

        let native = start_named_thread(&home, &cwd, &name, profile.as_ref())?;
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
        self.registry.list_codex_threads(home_id.as_deref())
    }

    pub fn inspect_codex_thread(&self, reference: &str) -> Result<CodexThreadInspection> {
        let managed = self
            .registry
            .codex_thread_by_ref(reference)?
            .ok_or_else(|| WipsawError::NotFound {
                entity: "Codex thread",
                value: reference.to_string(),
            })?;
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
        let home = self
            .registry
            .codex_home_by_ref(&thread.codex_home_id)?
            .ok_or_else(|| WipsawError::NotFound {
                entity: "Codex home",
                value: thread.codex_home_id.clone(),
            })?;
        let workspace = self.workspace(workspace_ref)?;
        self.ensure_workspace_runtime(&workspace)?;
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

        self.registry.bind_tab_to_codex_thread(&tab.id, &thread)?;
        let return_shell = return_shell_path();
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
        Ok(CodexThreadLaunch {
            thread_id: thread.id,
            native_thread_id: thread.native_thread_id,
            workspace_id: workspace.id,
            tab_id: tab.id,
            tmux_session: workspace.tmux_session,
            tmux_window_id: tab.tmux_window_id,
            codex_home_id: home.id,
            cwd: thread.cwd,
            return_shell,
        })
    }

    /// Enter the Codex thread assigned to the shell's current managed tab,
    /// creating and binding one on first use. Successful execution replaces
    /// the shortcut process with the real Codex CLI.
    pub fn run_codex_shortcut(&mut self, args: &[OsString]) -> Result<()> {
        let (workspace, tab) = self.current_tab()?;
        let thread_name = if tab.name.eq_ignore_ascii_case("manager") {
            format!("Lumbergh - {}", workspace.name)
        } else {
            tab.name.clone()
        };
        let thread = self.ensure_tab_thread(&workspace, &tab, &thread_name)?;
        self.exec_codex_thread(&thread, args)
    }

    /// Select the persistent manager tab. Its named Codex thread is created on
    /// first use and resumed only when the tab is sitting at a shell prompt.
    pub fn run_manager_shortcut(&mut self) -> Result<()> {
        let (workspace, current_tab) = self.current_tab()?;
        let manager_tab = self
            .registry
            .tab_by_ref(&workspace.id, "manager")?
            .ok_or_else(|| WipsawError::NotFound {
                entity: "manager tab",
                value: workspace.name.clone(),
            })?;
        let name = format!("Lumbergh - {}", workspace.name);
        let thread = self.ensure_tab_thread(&workspace, &manager_tab, &name)?;

        if current_tab.id == manager_tab.id {
            return self.exec_codex_thread(&thread, &[]);
        }

        if self.tab_should_start_thread(&workspace, &manager_tab, &thread)? {
            self.resume_codex_thread(&thread.id, &workspace.id, &manager_tab.id)?;
        }
        self.tmux
            .select_tab(&workspace.tmux_session, &manager_tab.tmux_window_id)
    }

    /// Lazily create the named Codex thread for a tab and launch it when the
    /// pane is currently at a shell prompt.
    pub fn start_tab_codex(&mut self, workspace_ref: &str, tab_ref: &str) -> Result<CodexThread> {
        let workspace = self.workspace(workspace_ref)?;
        self.ensure_workspace_runtime(&workspace)?;
        let tab = self
            .registry
            .tab_by_ref(&workspace.id, tab_ref)?
            .ok_or_else(|| WipsawError::NotFound {
                entity: "tab",
                value: tab_ref.to_string(),
            })?;
        let name = if tab.name.eq_ignore_ascii_case("manager") {
            format!("Lumbergh - {}", workspace.name)
        } else {
            tab.name.clone()
        };
        let thread = self.ensure_tab_thread(&workspace, &tab, &name)?;
        if self.tab_should_start_thread(&workspace, &tab, &thread)? {
            self.resume_codex_thread(&thread.id, &workspace.id, &tab.id)?;
        }
        Ok(thread)
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
    let candidate = std::process::Command::new(&resolved)
        .arg("--version")
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
