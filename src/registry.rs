use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, params};

use crate::error::{Result, WipsawError};
use crate::model::{
    Account, AccountAuthKind, AccountOwnerKind, CodexHome, CodexThread, ModelProfile,
    ModelProfileSettings, Tab, Workspace,
};

const LOCAL_HOST_ID: &str = "host_local";

pub struct Registry {
    connection: Connection,
}

pub struct NewWorkspace<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub tmux_session: &'a str,
    pub cwd: &'a Path,
    pub manager_tab_id: &'a str,
    pub manager_window_id: &'a str,
    pub manager_window_index: i64,
}

pub struct NewTab<'a> {
    pub id: &'a str,
    pub workspace_id: &'a str,
    pub name: &'a str,
    pub tmux_window_id: &'a str,
    pub tmux_window_index: i64,
    pub cwd: &'a Path,
    pub account_id: Option<&'a str>,
    pub codex_home_id: Option<&'a str>,
    pub model_profile_id: Option<&'a str>,
    pub codex_thread_id: Option<&'a str>,
}

pub struct NewCodexThread<'a> {
    pub id: &'a str,
    pub codex_home_id: &'a str,
    pub native_thread_id: &'a str,
    pub name: &'a str,
    pub cwd: &'a Path,
    pub model_profile_id: Option<&'a str>,
    pub model: &'a str,
    pub model_provider: &'a str,
    pub reasoning_effort: Option<&'a str>,
    pub status: &'a str,
    pub rollout_path: Option<&'a Path>,
    pub native_created_at: Option<i64>,
    /// If present, bind the thread and its resolved home/profile to this tab in
    /// the same transaction as the thread insert.
    pub bind_tab_id: Option<&'a str>,
}

pub struct NewModelProfile<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub model: &'a str,
    pub settings: &'a ModelProfileSettings,
}

pub struct NewAccount<'a> {
    pub id: &'a str,
    pub alias: &'a str,
    pub auth_kind: AccountAuthKind,
    pub owner_kind: AccountOwnerKind,
    pub credential_ref: Option<&'a str>,
}

pub struct NewCodexHome<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub account_id: &'a str,
    pub path: &'a Path,
    pub codex_binary: &'a Path,
}

pub struct NewCurrentCodex<'a> {
    pub account_id: &'a str,
    pub home_id: &'a str,
    pub path: &'a Path,
    pub codex_binary: &'a Path,
}

impl Registry {
    pub fn open(path: &Path) -> Result<Self> {
        let connection = Connection::open(path)?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        let mut registry = Self { connection };
        registry.migrate()?;
        if path.exists() {
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        }
        Ok(registry)
    }

    fn migrate(&mut self) -> Result<()> {
        let version = self
            .connection
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))?;

        if version < 1 {
            let transaction = self.connection.transaction()?;
            transaction.execute_batch(
                r#"
            CREATE TABLE IF NOT EXISTS hosts (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL UNIQUE,
                kind TEXT NOT NULL,
                address TEXT,
                created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
                updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
            );

            CREATE TABLE IF NOT EXISTS accounts (
                id TEXT PRIMARY KEY,
                alias TEXT NOT NULL UNIQUE COLLATE NOCASE,
                auth_kind TEXT NOT NULL,
                owner_kind TEXT NOT NULL,
                credential_ref TEXT,
                created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
                updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
            );

            CREATE TABLE IF NOT EXISTS codex_homes (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL UNIQUE COLLATE NOCASE,
                host_id TEXT NOT NULL REFERENCES hosts(id) ON DELETE RESTRICT,
                account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE RESTRICT,
                path TEXT NOT NULL,
                codex_binary TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
                updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
                UNIQUE(host_id, path)
            );

            CREATE TABLE IF NOT EXISTS workspaces (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL UNIQUE COLLATE NOCASE,
                host_id TEXT NOT NULL REFERENCES hosts(id) ON DELETE RESTRICT,
                tmux_session TEXT NOT NULL UNIQUE,
                cwd TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'active',
                created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
                updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
            );

            CREATE TABLE IF NOT EXISTS tabs (
                id TEXT PRIMARY KEY,
                workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
                name TEXT NOT NULL COLLATE NOCASE,
                tmux_window_id TEXT NOT NULL,
                tmux_window_index INTEGER NOT NULL,
                cwd TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
                updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
                UNIQUE(workspace_id, name),
                UNIQUE(workspace_id, tmux_window_id)
            );

            CREATE INDEX IF NOT EXISTS idx_tabs_workspace_index
                ON tabs(workspace_id, tmux_window_index);
            CREATE INDEX IF NOT EXISTS idx_homes_account
                ON codex_homes(account_id);

                PRAGMA user_version = 1;
                "#,
            )?;
            transaction.commit()?;
        }

        if version < 2 {
            let transaction = self.connection.transaction()?;
            transaction.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS model_profiles (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL UNIQUE COLLATE NOCASE,
                    provider TEXT,
                    model TEXT NOT NULL,
                    reasoning_effort TEXT,
                    search INTEGER,
                    sandbox TEXT,
                    approval_policy TEXT,
                    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
                    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
                );

                ALTER TABLE tabs ADD COLUMN account_id TEXT REFERENCES accounts(id) ON DELETE RESTRICT;
                ALTER TABLE tabs ADD COLUMN codex_home_id TEXT REFERENCES codex_homes(id) ON DELETE RESTRICT;
                ALTER TABLE tabs ADD COLUMN model_profile_id TEXT REFERENCES model_profiles(id) ON DELETE RESTRICT;

                CREATE INDEX IF NOT EXISTS idx_tabs_account ON tabs(account_id);
                CREATE INDEX IF NOT EXISTS idx_tabs_home ON tabs(codex_home_id);
                CREATE INDEX IF NOT EXISTS idx_tabs_model_profile ON tabs(model_profile_id);

                PRAGMA user_version = 2;
                "#,
            )?;
            transaction.commit()?;
        }

        if version < 3 {
            let transaction = self.connection.transaction()?;
            transaction.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS codex_threads (
                    id TEXT PRIMARY KEY,
                    codex_home_id TEXT NOT NULL REFERENCES codex_homes(id) ON DELETE RESTRICT,
                    native_thread_id TEXT NOT NULL,
                    name TEXT NOT NULL,
                    cwd TEXT NOT NULL,
                    model_profile_id TEXT REFERENCES model_profiles(id) ON DELETE SET NULL,
                    model TEXT NOT NULL,
                    model_provider TEXT NOT NULL,
                    reasoning_effort TEXT,
                    status TEXT NOT NULL,
                    rollout_path TEXT,
                    native_created_at INTEGER,
                    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
                    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
                    UNIQUE(codex_home_id, native_thread_id)
                );

                ALTER TABLE tabs ADD COLUMN codex_thread_id TEXT
                    REFERENCES codex_threads(id) ON DELETE SET NULL;

                CREATE INDEX IF NOT EXISTS idx_codex_threads_home
                    ON codex_threads(codex_home_id);
                CREATE INDEX IF NOT EXISTS idx_codex_threads_profile
                    ON codex_threads(model_profile_id);
                CREATE INDEX IF NOT EXISTS idx_tabs_codex_thread
                    ON tabs(codex_thread_id);

                PRAGMA user_version = 3;
                "#,
            )?;
            transaction.commit()?;
        }

        self.connection.execute(
            "INSERT OR IGNORE INTO hosts (id, name, kind) VALUES (?1, 'local', 'local')",
            [LOCAL_HOST_ID],
        )?;
        Ok(())
    }

    pub fn insert_workspace_with_manager(&mut self, input: NewWorkspace<'_>) -> Result<Workspace> {
        if self.workspace_by_ref(input.name)?.is_some() {
            return Err(WipsawError::AlreadyExists {
                entity: "workspace",
                value: input.name.to_string(),
            });
        }
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "INSERT INTO workspaces (id, name, host_id, tmux_session, cwd) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                input.id,
                input.name,
                LOCAL_HOST_ID,
                input.tmux_session,
                path_text(input.cwd),
            ],
        )?;
        transaction.execute(
            "INSERT INTO tabs (id, workspace_id, name, tmux_window_id, tmux_window_index, cwd) VALUES (?1, ?2, 'manager', ?3, ?4, ?5)",
            params![
                input.manager_tab_id,
                input.id,
                input.manager_window_id,
                input.manager_window_index,
                path_text(input.cwd),
            ],
        )?;
        transaction.commit()?;
        self.workspace_by_ref(input.id)?
            .ok_or_else(|| WipsawError::NotFound {
                entity: "workspace",
                value: input.id.to_string(),
            })
    }

    pub fn list_workspaces(&self) -> Result<Vec<Workspace>> {
        let mut statement = self.connection.prepare(
            "SELECT id, name, host_id, tmux_session, cwd, status, created_at, updated_at FROM workspaces ORDER BY name COLLATE NOCASE",
        )?;
        let rows = statement.query_map([], workspace_from_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn workspace_by_ref(&self, value: &str) -> Result<Option<Workspace>> {
        self.connection
            .query_row(
                "SELECT id, name, host_id, tmux_session, cwd, status, created_at, updated_at FROM workspaces WHERE id = ?1 OR name = ?1 COLLATE NOCASE LIMIT 1",
                [value],
                workspace_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn insert_tab(&self, input: NewTab<'_>) -> Result<Tab> {
        if self.tab_by_ref(input.workspace_id, input.name)?.is_some() {
            return Err(WipsawError::AlreadyExists {
                entity: "tab",
                value: input.name.to_string(),
            });
        }
        self.connection.execute(
            "INSERT INTO tabs (id, workspace_id, name, tmux_window_id, tmux_window_index, cwd, account_id, codex_home_id, model_profile_id, codex_thread_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                input.id,
                input.workspace_id,
                input.name,
                input.tmux_window_id,
                input.tmux_window_index,
                path_text(input.cwd),
                input.account_id,
                input.codex_home_id,
                input.model_profile_id,
                input.codex_thread_id,
            ],
        )?;
        self.tab_by_ref(input.workspace_id, input.id)?
            .ok_or_else(|| WipsawError::NotFound {
                entity: "tab",
                value: input.id.to_string(),
            })
    }

    pub fn list_tabs(&self, workspace_id: &str) -> Result<Vec<Tab>> {
        let mut statement = self.connection.prepare(&format!(
            "{TAB_SELECT} WHERE workspace_id = ?1 ORDER BY tmux_window_index"
        ))?;
        let rows = statement.query_map([workspace_id], tab_from_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn tab_by_ref(&self, workspace_id: &str, value: &str) -> Result<Option<Tab>> {
        self.connection
            .query_row(
                &format!("{TAB_SELECT} WHERE workspace_id = ?1 AND (id = ?2 OR name = ?2 COLLATE NOCASE) LIMIT 1"),
                params![workspace_id, value],
                tab_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn rename_tab(&self, tab_id: &str, name: &str) -> Result<Tab> {
        let changed = self.connection.execute(
            "UPDATE tabs SET name = ?2, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?1",
            params![tab_id, name],
        )?;
        if changed == 0 {
            return Err(WipsawError::NotFound {
                entity: "tab",
                value: tab_id.to_string(),
            });
        }
        self.connection
            .query_row(
                &format!("{TAB_SELECT} WHERE id = ?1"),
                [tab_id],
                tab_from_row,
            )
            .map_err(Into::into)
    }

    pub fn insert_codex_thread(&mut self, input: NewCodexThread<'_>) -> Result<CodexThread> {
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "INSERT INTO codex_threads (id, codex_home_id, native_thread_id, name, cwd, model_profile_id, model, model_provider, reasoning_effort, status, rollout_path, native_created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                input.id,
                input.codex_home_id,
                input.native_thread_id,
                input.name,
                path_text(input.cwd),
                input.model_profile_id,
                input.model,
                input.model_provider,
                input.reasoning_effort,
                input.status,
                input.rollout_path.map(path_text),
                input.native_created_at,
            ],
        )?;
        if let Some(tab_id) = input.bind_tab_id {
            let changed = transaction.execute(
                "UPDATE tabs SET account_id = (SELECT account_id FROM codex_homes WHERE id = ?2), codex_home_id = ?2, model_profile_id = ?3, codex_thread_id = ?4, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?1",
                params![tab_id, input.codex_home_id, input.model_profile_id, input.id],
            )?;
            if changed == 0 {
                return Err(WipsawError::NotFound {
                    entity: "tab",
                    value: tab_id.to_string(),
                });
            }
        }
        transaction.commit()?;
        self.codex_thread_by_ref(input.id)?
            .ok_or_else(|| WipsawError::NotFound {
                entity: "Codex thread",
                value: input.id.to_string(),
            })
    }

    pub fn list_codex_threads(&self, home_id: Option<&str>) -> Result<Vec<CodexThread>> {
        let sql = match home_id {
            Some(_) => format!(
                "{CODEX_THREAD_SELECT} WHERE t.codex_home_id = ?1 ORDER BY t.updated_at DESC"
            ),
            None => format!("{CODEX_THREAD_SELECT} ORDER BY t.updated_at DESC"),
        };
        let mut statement = self.connection.prepare(&sql)?;
        if let Some(home_id) = home_id {
            let rows = statement.query_map([home_id], codex_thread_from_row)?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(Into::into)
        } else {
            let rows = statement.query_map([], codex_thread_from_row)?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(Into::into)
        }
    }

    pub fn codex_thread_by_ref(&self, value: &str) -> Result<Option<CodexThread>> {
        self.connection
            .query_row(
                &format!(
                    "{CODEX_THREAD_SELECT} WHERE t.id = ?1 OR t.native_thread_id = ?1 LIMIT 1"
                ),
                [value],
                codex_thread_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn insert_model_profile(&self, input: NewModelProfile<'_>) -> Result<ModelProfile> {
        if self.model_profile_by_ref(input.name)?.is_some() {
            return Err(WipsawError::AlreadyExists {
                entity: "model profile",
                value: input.name.to_string(),
            });
        }
        self.connection.execute(
            "INSERT INTO model_profiles (id, name, provider, model, reasoning_effort, search, sandbox, approval_policy) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                input.id,
                input.name,
                input.settings.provider,
                input.model,
                input.settings.reasoning_effort,
                input.settings.search,
                input.settings.sandbox,
                input.settings.approval_policy,
            ],
        )?;
        self.model_profile_by_ref(input.id)?
            .ok_or_else(|| WipsawError::NotFound {
                entity: "model profile",
                value: input.id.to_string(),
            })
    }

    pub fn list_model_profiles(&self) -> Result<Vec<ModelProfile>> {
        let mut statement = self.connection.prepare(&format!(
            "{MODEL_PROFILE_SELECT} ORDER BY name COLLATE NOCASE"
        ))?;
        let rows = statement.query_map([], model_profile_from_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn model_profile_by_ref(&self, value: &str) -> Result<Option<ModelProfile>> {
        self.connection
            .query_row(
                &format!(
                    "{MODEL_PROFILE_SELECT} WHERE id = ?1 OR name = ?1 COLLATE NOCASE LIMIT 1"
                ),
                [value],
                model_profile_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn insert_account(&self, input: NewAccount<'_>) -> Result<Account> {
        if self.account_by_ref(input.alias)?.is_some() {
            return Err(WipsawError::AlreadyExists {
                entity: "account",
                value: input.alias.to_string(),
            });
        }
        self.connection.execute(
            "INSERT INTO accounts (id, alias, auth_kind, owner_kind, credential_ref) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                input.id,
                input.alias,
                input.auth_kind.to_string(),
                input.owner_kind.to_string(),
                input.credential_ref,
            ],
        )?;
        self.account_by_ref(input.id)?
            .ok_or_else(|| WipsawError::NotFound {
                entity: "account",
                value: input.id.to_string(),
            })
    }

    /// Adopt the machine's active Codex home on a brand-new registry. The
    /// account and home are committed together so first-run setup cannot leave
    /// half of the default identity behind.
    pub fn bootstrap_current_codex(&mut self, input: NewCurrentCodex<'_>) -> Result<bool> {
        let identity_count = self.connection.query_row(
            "SELECT (SELECT COUNT(*) FROM accounts) + (SELECT COUNT(*) FROM codex_homes)",
            [],
            |row| row.get::<_, i64>(0),
        )?;
        if identity_count != 0 {
            return Ok(false);
        }

        let transaction = self.connection.transaction()?;
        transaction.execute(
            "INSERT INTO accounts (id, alias, auth_kind, owner_kind, credential_ref) VALUES (?1, 'current', 'existing-codex-home', 'personal', NULL)",
            [input.account_id],
        )?;
        transaction.execute(
            "INSERT INTO codex_homes (id, name, host_id, account_id, path, codex_binary) VALUES (?1, 'current', ?2, ?3, ?4, ?5)",
            params![
                input.home_id,
                LOCAL_HOST_ID,
                input.account_id,
                path_text(input.path),
                path_text(input.codex_binary),
            ],
        )?;
        transaction.commit()?;
        Ok(true)
    }

    pub fn list_accounts(&self) -> Result<Vec<Account>> {
        let mut statement = self.connection.prepare(
            "SELECT id, alias, auth_kind, owner_kind, credential_ref, created_at, updated_at FROM accounts ORDER BY alias COLLATE NOCASE",
        )?;
        let rows = statement.query_map([], account_from_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn account_by_ref(&self, value: &str) -> Result<Option<Account>> {
        self.connection
            .query_row(
                "SELECT id, alias, auth_kind, owner_kind, credential_ref, created_at, updated_at FROM accounts WHERE id = ?1 OR alias = ?1 COLLATE NOCASE LIMIT 1",
                [value],
                account_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn insert_codex_home(&self, input: NewCodexHome<'_>) -> Result<CodexHome> {
        if self.codex_home_by_ref(input.name)?.is_some() {
            return Err(WipsawError::AlreadyExists {
                entity: "Codex home",
                value: input.name.to_string(),
            });
        }
        if let Some(existing) = self.codex_home_by_path(input.path)? {
            return Err(WipsawError::CodexHomeConflict {
                path: input.path.to_path_buf(),
                account: existing.account_alias,
            });
        }
        self.connection.execute(
            "INSERT INTO codex_homes (id, name, host_id, account_id, path, codex_binary) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                input.id,
                input.name,
                LOCAL_HOST_ID,
                input.account_id,
                path_text(input.path),
                path_text(input.codex_binary),
            ],
        )?;
        self.codex_home_by_ref(input.id)?
            .ok_or_else(|| WipsawError::NotFound {
                entity: "Codex home",
                value: input.id.to_string(),
            })
    }

    pub fn list_codex_homes(&self) -> Result<Vec<CodexHome>> {
        let mut statement = self.connection.prepare(CODEX_HOME_SELECT)?;
        let rows = statement.query_map([], codex_home_from_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn codex_home_by_ref(&self, value: &str) -> Result<Option<CodexHome>> {
        self.connection
            .query_row(
                &format!(
                    "{CODEX_HOME_SELECT} WHERE h.id = ?1 OR h.name = ?1 COLLATE NOCASE LIMIT 1"
                ),
                [value],
                codex_home_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    fn codex_home_by_path(&self, path: &Path) -> Result<Option<CodexHome>> {
        self.connection
            .query_row(
                &format!("{CODEX_HOME_SELECT} WHERE h.host_id = ?1 AND h.path = ?2 LIMIT 1"),
                params![LOCAL_HOST_ID, path_text(path)],
                codex_home_from_row,
            )
            .optional()
            .map_err(Into::into)
    }
}

const CODEX_HOME_SELECT: &str = "SELECT h.id, h.name, h.host_id, h.account_id, a.alias, h.path, h.codex_binary, h.created_at, h.updated_at FROM codex_homes h JOIN accounts a ON a.id = h.account_id";
const TAB_SELECT: &str = "SELECT id, workspace_id, name, tmux_window_id, tmux_window_index, cwd, account_id, codex_home_id, model_profile_id, codex_thread_id, created_at, updated_at FROM tabs";
const MODEL_PROFILE_SELECT: &str = "SELECT id, name, provider, model, reasoning_effort, search, sandbox, approval_policy, created_at, updated_at FROM model_profiles";
const CODEX_THREAD_SELECT: &str = "SELECT t.id, t.codex_home_id, h.account_id, a.alias, t.native_thread_id, t.name, t.cwd, t.model_profile_id, t.model, t.model_provider, t.reasoning_effort, t.status, t.rollout_path, t.native_created_at, t.created_at, t.updated_at FROM codex_threads t JOIN codex_homes h ON h.id = t.codex_home_id JOIN accounts a ON a.id = h.account_id";

fn workspace_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Workspace> {
    Ok(Workspace {
        id: row.get(0)?,
        name: row.get(1)?,
        host_id: row.get(2)?,
        tmux_session: row.get(3)?,
        cwd: PathBuf::from(row.get::<_, String>(4)?),
        status: row.get(5)?,
        created_at: row.get(6)?,
        updated_at: row.get(7)?,
    })
}

fn tab_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Tab> {
    Ok(Tab {
        id: row.get(0)?,
        workspace_id: row.get(1)?,
        name: row.get(2)?,
        tmux_window_id: row.get(3)?,
        tmux_window_index: row.get(4)?,
        cwd: PathBuf::from(row.get::<_, String>(5)?),
        account_id: row.get(6)?,
        codex_home_id: row.get(7)?,
        model_profile_id: row.get(8)?,
        codex_thread_id: row.get(9)?,
        created_at: row.get(10)?,
        updated_at: row.get(11)?,
    })
}

fn codex_thread_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CodexThread> {
    Ok(CodexThread {
        id: row.get(0)?,
        codex_home_id: row.get(1)?,
        account_id: row.get(2)?,
        account_alias: row.get(3)?,
        native_thread_id: row.get(4)?,
        name: row.get(5)?,
        cwd: PathBuf::from(row.get::<_, String>(6)?),
        model_profile_id: row.get(7)?,
        model: row.get(8)?,
        model_provider: row.get(9)?,
        reasoning_effort: row.get(10)?,
        status: row.get(11)?,
        rollout_path: row.get::<_, Option<String>>(12)?.map(PathBuf::from),
        native_created_at: row.get(13)?,
        created_at: row.get(14)?,
        updated_at: row.get(15)?,
    })
}

fn model_profile_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ModelProfile> {
    Ok(ModelProfile {
        id: row.get(0)?,
        name: row.get(1)?,
        provider: row.get(2)?,
        model: row.get(3)?,
        reasoning_effort: row.get(4)?,
        search: row.get(5)?,
        sandbox: row.get(6)?,
        approval_policy: row.get(7)?,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
    })
}

fn account_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Account> {
    let auth_kind = row.get::<_, String>(2)?.parse().map_err(conversion_error)?;
    let owner_kind = row.get::<_, String>(3)?.parse().map_err(conversion_error)?;
    Ok(Account {
        id: row.get(0)?,
        alias: row.get(1)?,
        auth_kind,
        owner_kind,
        credential_ref: row.get(4)?,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
    })
}

fn codex_home_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CodexHome> {
    Ok(CodexHome {
        id: row.get(0)?,
        name: row.get(1)?,
        host_id: row.get(2)?,
        account_id: row.get(3)?,
        account_alias: row.get(4)?,
        path: PathBuf::from(row.get::<_, String>(5)?),
        codex_binary: PathBuf::from(row.get::<_, String>(6)?),
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
    })
}

fn conversion_error(error: WipsawError) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
}

fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use tempfile::tempdir;

    use super::{
        NewAccount, NewCodexHome, NewCodexThread, NewCurrentCodex, NewModelProfile, NewTab,
        NewWorkspace, Registry,
    };
    use crate::model::{AccountAuthKind, AccountOwnerKind, ModelProfileSettings};

    fn registry() -> (tempfile::TempDir, Registry) {
        let root = tempdir().unwrap();
        let registry = Registry::open(&root.path().join("registry.sqlite3")).unwrap();
        (root, registry)
    }

    #[test]
    fn workspace_and_manager_are_inserted_atomically() {
        let (_root, mut registry) = registry();
        let workspace = registry
            .insert_workspace_with_manager(NewWorkspace {
                id: "ws_01900000000070008000000000000000",
                name: "development",
                tmux_session: "wipsaw-01900000",
                cwd: Path::new("/tmp"),
                manager_tab_id: "tab_01900000000070008000000000000000",
                manager_window_id: "@1",
                manager_window_index: 0,
            })
            .unwrap();
        assert_eq!(workspace.name, "development");
        let tabs = registry.list_tabs(&workspace.id).unwrap();
        assert_eq!(tabs.len(), 1);
        assert_eq!(tabs[0].name, "manager");
    }

    #[test]
    fn account_homes_cannot_be_shared_across_accounts() {
        let (_root, registry) = registry();
        let personal = registry
            .insert_account(NewAccount {
                id: "acct_01900000000070008000000000000000",
                alias: "personal",
                auth_kind: AccountAuthKind::ChatgptSession,
                owner_kind: AccountOwnerKind::Personal,
                credential_ref: None,
            })
            .unwrap();
        let company = registry
            .insert_account(NewAccount {
                id: "acct_01900000000070008000000000000001",
                alias: "company",
                auth_kind: AccountAuthKind::ChatgptSession,
                owner_kind: AccountOwnerKind::Company,
                credential_ref: None,
            })
            .unwrap();
        registry
            .insert_codex_home(NewCodexHome {
                id: "home_01900000000070008000000000000000",
                name: "personal-home",
                account_id: &personal.id,
                path: Path::new("/tmp/personal-codex-home"),
                codex_binary: Path::new("/usr/bin/codex"),
            })
            .unwrap();
        let error = registry
            .insert_codex_home(NewCodexHome {
                id: "home_01900000000070008000000000000001",
                name: "company-home",
                account_id: &company.id,
                path: Path::new("/tmp/personal-codex-home"),
                codex_binary: Path::new("/usr/bin/codex"),
            })
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("already assigned to account 'personal'")
        );
    }

    #[test]
    fn empty_registry_adopts_the_current_codex_home_once() {
        let (_root, mut registry) = registry();
        let adopted = registry
            .bootstrap_current_codex(NewCurrentCodex {
                account_id: "acct_01900000000070008000000000000000",
                home_id: "home_01900000000070008000000000000000",
                path: Path::new("/tmp/current-codex-home"),
                codex_binary: Path::new("/usr/bin/codex"),
            })
            .unwrap();
        assert!(adopted);
        let accounts = registry.list_accounts().unwrap();
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].alias, "current");
        assert_eq!(accounts[0].auth_kind, AccountAuthKind::ExistingCodexHome);
        let homes = registry.list_codex_homes().unwrap();
        assert_eq!(homes.len(), 1);
        assert_eq!(homes[0].name, "current");

        let adopted_again = registry
            .bootstrap_current_codex(NewCurrentCodex {
                account_id: "acct_01900000000070008000000000000001",
                home_id: "home_01900000000070008000000000000001",
                path: Path::new("/tmp/another-codex-home"),
                codex_binary: Path::new("/usr/bin/codex"),
            })
            .unwrap();
        assert!(!adopted_again);
        assert_eq!(registry.list_codex_homes().unwrap().len(), 1);
    }

    #[test]
    fn tabs_are_ordered_by_tmux_index() {
        let (_root, mut registry) = registry();
        let workspace = registry
            .insert_workspace_with_manager(NewWorkspace {
                id: "ws_01900000000070008000000000000000",
                name: "development",
                tmux_session: "wipsaw-01900000",
                cwd: Path::new("/tmp"),
                manager_tab_id: "tab_01900000000070008000000000000000",
                manager_window_id: "@1",
                manager_window_index: 0,
            })
            .unwrap();
        registry
            .insert_tab(NewTab {
                id: "tab_01900000000070008000000000000001",
                workspace_id: &workspace.id,
                name: "api",
                tmux_window_id: "@2",
                tmux_window_index: 2,
                cwd: Path::new("/tmp"),
                account_id: None,
                codex_home_id: None,
                model_profile_id: None,
                codex_thread_id: None,
            })
            .unwrap();
        let tabs = registry.list_tabs(&workspace.id).unwrap();
        assert_eq!(
            tabs.iter().map(|tab| tab.name.as_str()).collect::<Vec<_>>(),
            ["manager", "api"]
        );
    }

    #[test]
    fn model_profile_is_bound_to_a_tab() {
        let (_root, mut registry) = registry();
        let profile = registry
            .insert_model_profile(NewModelProfile {
                id: "profile_01900000000070008000000000000000",
                name: "careful",
                model: "example-model",
                settings: &ModelProfileSettings {
                    reasoning_effort: Some("high".to_string()),
                    search: Some(true),
                    sandbox: Some("workspace-write".to_string()),
                    ..ModelProfileSettings::default()
                },
            })
            .unwrap();
        let workspace = registry
            .insert_workspace_with_manager(NewWorkspace {
                id: "ws_01900000000070008000000000000000",
                name: "development",
                tmux_session: "wipsaw-01900000",
                cwd: Path::new("/tmp"),
                manager_tab_id: "tab_01900000000070008000000000000000",
                manager_window_id: "@1",
                manager_window_index: 0,
            })
            .unwrap();
        let tab = registry
            .insert_tab(NewTab {
                id: "tab_01900000000070008000000000000001",
                workspace_id: &workspace.id,
                name: "api",
                tmux_window_id: "@2",
                tmux_window_index: 2,
                cwd: Path::new("/tmp"),
                account_id: None,
                codex_home_id: None,
                model_profile_id: Some(&profile.id),
                codex_thread_id: None,
            })
            .unwrap();
        assert_eq!(tab.model_profile_id.as_deref(), Some(profile.id.as_str()));
    }

    #[test]
    fn native_thread_and_tab_binding_are_inserted_atomically() {
        let (_root, mut registry) = registry();
        let account = registry
            .insert_account(NewAccount {
                id: "acct_01900000000070008000000000000000",
                alias: "company",
                auth_kind: AccountAuthKind::ChatgptSession,
                owner_kind: AccountOwnerKind::Company,
                credential_ref: None,
            })
            .unwrap();
        let home = registry
            .insert_codex_home(NewCodexHome {
                id: "home_01900000000070008000000000000000",
                name: "company-home",
                account_id: &account.id,
                path: Path::new("/tmp/company-codex-home"),
                codex_binary: Path::new("/usr/bin/codex"),
            })
            .unwrap();
        let profile = registry
            .insert_model_profile(NewModelProfile {
                id: "profile_01900000000070008000000000000000",
                name: "careful",
                model: "gpt-test",
                settings: &ModelProfileSettings::default(),
            })
            .unwrap();
        let workspace = registry
            .insert_workspace_with_manager(NewWorkspace {
                id: "ws_01900000000070008000000000000000",
                name: "development",
                tmux_session: "wipsaw-01900000",
                cwd: Path::new("/tmp"),
                manager_tab_id: "tab_01900000000070008000000000000000",
                manager_window_id: "@1",
                manager_window_index: 0,
            })
            .unwrap();
        let tab = registry
            .insert_tab(NewTab {
                id: "tab_01900000000070008000000000000001",
                workspace_id: &workspace.id,
                name: "api",
                tmux_window_id: "@2",
                tmux_window_index: 1,
                cwd: Path::new("/tmp"),
                account_id: None,
                codex_home_id: None,
                model_profile_id: None,
                codex_thread_id: None,
            })
            .unwrap();
        let thread = registry
            .insert_codex_thread(NewCodexThread {
                id: "thread_01900000000070008000000000000000",
                codex_home_id: &home.id,
                native_thread_id: "01900000-0000-7000-8000-000000000000",
                name: "API implementation",
                cwd: Path::new("/tmp"),
                model_profile_id: Some(&profile.id),
                model: "gpt-test",
                model_provider: "openai",
                reasoning_effort: Some("high"),
                status: "idle",
                rollout_path: Some(Path::new("/tmp/rollout.jsonl")),
                native_created_at: Some(1_700_000_000),
                bind_tab_id: Some(&tab.id),
            })
            .unwrap();

        assert_eq!(thread.account_alias, "company");
        assert_eq!(
            thread.model_profile_id.as_deref(),
            Some(profile.id.as_str())
        );
        let rebound = registry
            .tab_by_ref(&workspace.id, &tab.id)
            .unwrap()
            .unwrap();
        assert_eq!(rebound.account_id.as_deref(), Some(account.id.as_str()));
        assert_eq!(rebound.codex_home_id.as_deref(), Some(home.id.as_str()));
        assert_eq!(rebound.codex_thread_id.as_deref(), Some(thread.id.as_str()));
    }
}
