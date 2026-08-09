use std::collections::HashSet;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, params};

use crate::error::{Result, WipsawError};
use crate::model::{
    Account, AccountAuthKind, AccountOwnerKind, CodexHome, CodexThread, ManagerKind,
    ManagerMessage, ManagerSession, ModelProfile, ModelProfileSettings, Tab, Workspace,
    WorkspaceContext,
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
    pub manager_account_id: Option<&'a str>,
    pub manager_codex_home_id: Option<&'a str>,
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

pub struct NewManagerSession<'a> {
    pub id: &'a str,
    pub kind: ManagerKind,
    pub workspace_id: Option<&'a str>,
    pub source_codex_home_id: &'a str,
    pub cwd: &'a Path,
    pub model: &'a str,
    pub reasoning_effort: &'a str,
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

        if version < 4 {
            let transaction = self.connection.transaction()?;
            transaction.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS manager_sessions (
                    id TEXT PRIMARY KEY,
                    kind TEXT NOT NULL CHECK(kind IN ('lumbergh', 'middle-manager')),
                    workspace_id TEXT REFERENCES workspaces(id) ON DELETE CASCADE,
                    source_codex_home_id TEXT NOT NULL REFERENCES codex_homes(id) ON DELETE RESTRICT,
                    native_thread_id TEXT,
                    cwd TEXT NOT NULL,
                    model TEXT NOT NULL,
                    reasoning_effort TEXT NOT NULL,
                    status TEXT NOT NULL DEFAULT 'idle',
                    last_error TEXT,
                    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
                    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
                    CHECK (
                        (kind = 'lumbergh' AND workspace_id IS NULL) OR
                        (kind = 'middle-manager' AND workspace_id IS NOT NULL)
                    )
                );

                CREATE UNIQUE INDEX IF NOT EXISTS idx_manager_sessions_workspace
                    ON manager_sessions(workspace_id)
                    WHERE workspace_id IS NOT NULL;
                CREATE UNIQUE INDEX IF NOT EXISTS idx_manager_sessions_lumbergh
                    ON manager_sessions(kind)
                    WHERE kind = 'lumbergh';
                CREATE UNIQUE INDEX IF NOT EXISTS idx_manager_sessions_native_thread
                    ON manager_sessions(source_codex_home_id, native_thread_id)
                    WHERE native_thread_id IS NOT NULL;

                CREATE TABLE IF NOT EXISTS manager_messages (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    manager_session_id TEXT NOT NULL REFERENCES manager_sessions(id) ON DELETE CASCADE,
                    role TEXT NOT NULL CHECK(role IN ('user', 'assistant', 'system')),
                    content TEXT NOT NULL,
                    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
                );

                CREATE INDEX IF NOT EXISTS idx_manager_messages_session
                    ON manager_messages(manager_session_id, id);

                UPDATE tabs AS manager_tab
                SET name = 'middle-manager',
                    updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
                WHERE lower(manager_tab.name) = 'manager'
                  AND NOT EXISTS (
                      SELECT 1 FROM tabs AS existing
                      WHERE existing.workspace_id = manager_tab.workspace_id
                        AND lower(existing.name) = 'middle-manager'
                  );

                PRAGMA user_version = 4;
                "#,
            )?;
            transaction.commit()?;
        }

        if version < 5 {
            let transaction = self.connection.transaction()?;
            transaction.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS workspace_contexts (
                    workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
                    path TEXT NOT NULL,
                    kind TEXT NOT NULL CHECK(kind IN ('directory', 'file')),
                    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
                    PRIMARY KEY(workspace_id, path)
                );

                CREATE INDEX IF NOT EXISTS idx_workspace_contexts_workspace
                    ON workspace_contexts(workspace_id, kind, path);

                INSERT OR IGNORE INTO workspace_contexts (workspace_id, path, kind)
                    SELECT id, cwd, 'directory' FROM workspaces;

                PRAGMA user_version = 5;
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
            "INSERT INTO tabs (id, workspace_id, name, tmux_window_id, tmux_window_index, cwd, account_id, codex_home_id) VALUES (?1, ?2, 'middle-manager', ?3, ?4, ?5, ?6, ?7)",
            params![
                input.manager_tab_id,
                input.id,
                input.manager_window_id,
                input.manager_window_index,
                path_text(input.cwd),
                input.manager_account_id,
                input.manager_codex_home_id,
            ],
        )?;
        transaction.execute(
            "INSERT INTO workspace_contexts (workspace_id, path, kind) VALUES (?1, ?2, 'directory')",
            params![input.id, path_text(input.cwd)],
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

    pub fn workspace_by_tmux_session(&self, session: &str) -> Result<Option<Workspace>> {
        self.connection
            .query_row(
                "SELECT id, name, host_id, tmux_session, cwd, status, created_at, updated_at FROM workspaces WHERE tmux_session = ?1 LIMIT 1",
                [session],
                workspace_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn delete_workspace(&self, workspace_id: &str) -> Result<()> {
        let changed = self
            .connection
            .execute("DELETE FROM workspaces WHERE id = ?1", [workspace_id])?;
        if changed == 0 {
            return Err(WipsawError::NotFound {
                entity: "workspace",
                value: workspace_id.to_string(),
            });
        }
        Ok(())
    }

    pub fn list_workspace_contexts(&self, workspace_id: &str) -> Result<Vec<WorkspaceContext>> {
        let mut statement = self.connection.prepare(
            "SELECT workspace_id, path, kind, created_at FROM workspace_contexts WHERE workspace_id = ?1 ORDER BY kind, path COLLATE NOCASE",
        )?;
        let rows = statement.query_map([workspace_id], workspace_context_from_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn insert_workspace_context(
        &self,
        workspace_id: &str,
        path: &Path,
        kind: &str,
    ) -> Result<WorkspaceContext> {
        self.connection.execute(
            "INSERT INTO workspace_contexts (workspace_id, path, kind) VALUES (?1, ?2, ?3) ON CONFLICT(workspace_id, path) DO UPDATE SET kind = excluded.kind",
            params![workspace_id, path_text(path), kind],
        )?;
        self.workspace_context(workspace_id, path)?
            .ok_or_else(|| WipsawError::NotFound {
                entity: "workspace context",
                value: path.display().to_string(),
            })
    }

    pub fn delete_workspace_context(&self, workspace_id: &str, path: &Path) -> Result<()> {
        let changed = self.connection.execute(
            "DELETE FROM workspace_contexts WHERE workspace_id = ?1 AND path = ?2",
            params![workspace_id, path_text(path)],
        )?;
        if changed == 0 {
            return Err(WipsawError::NotFound {
                entity: "workspace context",
                value: path.display().to_string(),
            });
        }
        Ok(())
    }

    fn workspace_context(
        &self,
        workspace_id: &str,
        path: &Path,
    ) -> Result<Option<WorkspaceContext>> {
        self.connection
            .query_row(
                "SELECT workspace_id, path, kind, created_at FROM workspace_contexts WHERE workspace_id = ?1 AND path = ?2 LIMIT 1",
                params![workspace_id, path_text(path)],
                workspace_context_from_row,
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

    pub fn tab_by_tmux_window(&self, workspace_id: &str, window_id: &str) -> Result<Option<Tab>> {
        self.connection
            .query_row(
                &format!("{TAB_SELECT} WHERE workspace_id = ?1 AND tmux_window_id = ?2 LIMIT 1"),
                params![workspace_id, window_id],
                tab_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn tab_by_codex_thread(&self, thread_id: &str) -> Result<Option<Tab>> {
        self.connection
            .query_row(
                &format!("{TAB_SELECT} WHERE codex_thread_id = ?1 LIMIT 1"),
                [thread_id],
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

    /// Replace every persisted tmux target for a workspace in one transaction.
    ///
    /// Window IDs belong to a particular tmux server lifetime. When that
    /// server disappears, restored windows can reuse the same IDs in a
    /// different order. Moving all rows through unique temporary IDs avoids
    /// collisions while swapping the stale targets for the restored ones.
    pub fn replace_workspace_tab_targets(
        &self,
        workspace_id: &str,
        targets: &[(String, String, i64)],
    ) -> Result<()> {
        let registered_count: i64 = self.connection.query_row(
            "SELECT COUNT(*) FROM tabs WHERE workspace_id = ?1",
            [workspace_id],
            |row| row.get(0),
        )?;
        let tab_ids = targets
            .iter()
            .map(|(tab_id, _, _)| tab_id.as_str())
            .collect::<HashSet<_>>();
        let window_ids = targets
            .iter()
            .map(|(_, window_id, _)| window_id.as_str())
            .collect::<HashSet<_>>();
        if registered_count != targets.len() as i64
            || tab_ids.len() != targets.len()
            || window_ids.len() != targets.len()
        {
            return Err(WipsawError::InvalidInput {
                field: "workspace recovery",
                message: "restored tmux targets must cover every tab exactly once".to_string(),
            });
        }

        let transaction = self.connection.unchecked_transaction()?;
        transaction.execute(
            "UPDATE tabs SET tmux_window_id = 'wipsaw-recovering:' || id WHERE workspace_id = ?1",
            [workspace_id],
        )?;
        for (tab_id, window_id, window_index) in targets {
            let changed = transaction.execute(
                "UPDATE tabs SET tmux_window_id = ?3, tmux_window_index = ?4, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?1 AND workspace_id = ?2",
                params![tab_id, workspace_id, window_id, window_index],
            )?;
            if changed != 1 {
                return Err(WipsawError::NotFound {
                    entity: "tab",
                    value: tab_id.clone(),
                });
            }
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn bind_tab_to_codex_thread(&self, tab_id: &str, thread: &CodexThread) -> Result<Tab> {
        let changed = self.connection.execute(
            "UPDATE tabs SET account_id = ?2, codex_home_id = ?3, model_profile_id = ?4, codex_thread_id = ?5, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?1",
            params![
                tab_id,
                thread.account_id,
                thread.codex_home_id,
                thread.model_profile_id,
                thread.id,
            ],
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

    pub fn list_workspace_codex_threads(&self, workspace_id: &str) -> Result<Vec<CodexThread>> {
        let mut statement = self.connection.prepare(&format!(
            "{CODEX_THREAD_SELECT} JOIN tabs tab ON tab.codex_thread_id = t.id WHERE tab.workspace_id = ?1 GROUP BY t.id ORDER BY t.updated_at DESC"
        ))?;
        let rows = statement.query_map([workspace_id], codex_thread_from_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn codex_thread_workspace_ids(&self, thread_id: &str) -> Result<Vec<String>> {
        let mut statement = self.connection.prepare(
            "SELECT DISTINCT workspace_id FROM tabs WHERE codex_thread_id = ?1 ORDER BY workspace_id",
        )?;
        let rows = statement.query_map([thread_id], |row| row.get(0))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn delete_codex_thread(&self, thread_id: &str) -> Result<()> {
        let changed = self
            .connection
            .execute("DELETE FROM codex_threads WHERE id = ?1", [thread_id])?;
        if changed == 0 {
            return Err(WipsawError::NotFound {
                entity: "Codex thread",
                value: thread_id.to_string(),
            });
        }
        Ok(())
    }

    pub fn delete_workspace_and_threads(
        &mut self,
        workspace_id: &str,
        thread_ids: &[String],
    ) -> Result<()> {
        let transaction = self.connection.transaction()?;
        for thread_id in thread_ids {
            transaction.execute("DELETE FROM codex_threads WHERE id = ?1", [thread_id])?;
        }
        let changed =
            transaction.execute("DELETE FROM workspaces WHERE id = ?1", [workspace_id])?;
        if changed == 0 {
            return Err(WipsawError::NotFound {
                entity: "workspace",
                value: workspace_id.to_string(),
            });
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn insert_manager_session(&self, input: NewManagerSession<'_>) -> Result<ManagerSession> {
        self.connection.execute(
            "INSERT INTO manager_sessions (id, kind, workspace_id, source_codex_home_id, cwd, model, reasoning_effort) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                input.id,
                input.kind.to_string(),
                input.workspace_id,
                input.source_codex_home_id,
                path_text(input.cwd),
                input.model,
                input.reasoning_effort,
            ],
        )?;
        self.manager_session_by_id(input.id)?
            .ok_or_else(|| WipsawError::NotFound {
                entity: "manager session",
                value: input.id.to_string(),
            })
    }

    pub fn manager_session_for_workspace(
        &self,
        workspace_id: Option<&str>,
    ) -> Result<Option<ManagerSession>> {
        let sql = match workspace_id {
            Some(_) => format!("{MANAGER_SESSION_SELECT} WHERE m.workspace_id = ?1 LIMIT 1"),
            None => format!(
                "{MANAGER_SESSION_SELECT} WHERE m.kind = 'lumbergh' AND m.workspace_id IS NULL LIMIT 1"
            ),
        };
        if let Some(workspace_id) = workspace_id {
            self.connection
                .query_row(&sql, [workspace_id], manager_session_from_row)
                .optional()
                .map_err(Into::into)
        } else {
            self.connection
                .query_row(&sql, [], manager_session_from_row)
                .optional()
                .map_err(Into::into)
        }
    }

    pub fn manager_session_by_id(&self, id: &str) -> Result<Option<ManagerSession>> {
        self.connection
            .query_row(
                &format!("{MANAGER_SESSION_SELECT} WHERE m.id = ?1 LIMIT 1"),
                [id],
                manager_session_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn list_manager_sessions(&self) -> Result<Vec<ManagerSession>> {
        let mut statement = self.connection.prepare(&format!(
            "{MANAGER_SESSION_SELECT} ORDER BY CASE m.kind WHEN 'lumbergh' THEN 0 ELSE 1 END, w.name COLLATE NOCASE"
        ))?;
        let rows = statement.query_map([], manager_session_from_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn append_manager_message(
        &self,
        manager_session_id: &str,
        role: &str,
        content: &str,
    ) -> Result<ManagerMessage> {
        if !["user", "assistant", "system"].contains(&role) {
            return Err(WipsawError::InvalidInput {
                field: "manager message role",
                message: format!("'{role}' must be user, assistant, or system"),
            });
        }
        if content.trim().is_empty() {
            return Err(WipsawError::InvalidInput {
                field: "manager message",
                message: "must not be empty".to_string(),
            });
        }
        self.connection.execute(
            "INSERT INTO manager_messages (manager_session_id, role, content) VALUES (?1, ?2, ?3)",
            params![manager_session_id, role, content],
        )?;
        let id = self.connection.last_insert_rowid();
        self.connection
            .query_row(
                &format!("{MANAGER_MESSAGE_SELECT} WHERE id = ?1"),
                [id],
                manager_message_from_row,
            )
            .map_err(Into::into)
    }

    pub fn list_manager_messages(
        &self,
        manager_session_id: &str,
        limit: usize,
    ) -> Result<Vec<ManagerMessage>> {
        let mut statement = self.connection.prepare(&format!(
            "SELECT * FROM ({MANAGER_MESSAGE_SELECT} WHERE manager_session_id = ?1 ORDER BY id DESC LIMIT ?2) ORDER BY id"
        ))?;
        let rows = statement.query_map(
            params![manager_session_id, i64::try_from(limit).unwrap_or(i64::MAX)],
            manager_message_from_row,
        )?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn mark_manager_working(&self, manager_session_id: &str) -> Result<()> {
        let changed = self.connection.execute(
            "UPDATE manager_sessions SET status = 'working', last_error = NULL, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?1",
            [manager_session_id],
        )?;
        if changed == 0 {
            return Err(WipsawError::NotFound {
                entity: "manager session",
                value: manager_session_id.to_string(),
            });
        }
        Ok(())
    }

    pub fn complete_manager_turn(
        &self,
        manager_session_id: &str,
        native_thread_id: &str,
        message: &str,
    ) -> Result<ManagerMessage> {
        let transaction = self.connection.unchecked_transaction()?;
        let changed = transaction.execute(
            "UPDATE manager_sessions SET native_thread_id = ?2, status = 'idle', last_error = NULL, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?1",
            params![manager_session_id, native_thread_id],
        )?;
        if changed == 0 {
            return Err(WipsawError::NotFound {
                entity: "manager session",
                value: manager_session_id.to_string(),
            });
        }
        transaction.execute(
            "INSERT INTO manager_messages (manager_session_id, role, content) VALUES (?1, 'assistant', ?2)",
            params![manager_session_id, message],
        )?;
        let id = transaction.last_insert_rowid();
        transaction.commit()?;
        self.connection
            .query_row(
                &format!("{MANAGER_MESSAGE_SELECT} WHERE id = ?1"),
                [id],
                manager_message_from_row,
            )
            .map_err(Into::into)
    }

    pub fn fail_manager_turn(
        &self,
        manager_session_id: &str,
        error: &str,
    ) -> Result<ManagerMessage> {
        let transaction = self.connection.unchecked_transaction()?;
        let changed = transaction.execute(
            "UPDATE manager_sessions SET status = 'error', last_error = ?2, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?1",
            params![manager_session_id, error],
        )?;
        if changed == 0 {
            return Err(WipsawError::NotFound {
                entity: "manager session",
                value: manager_session_id.to_string(),
            });
        }
        transaction.execute(
            "INSERT INTO manager_messages (manager_session_id, role, content) VALUES (?1, 'system', ?2)",
            params![manager_session_id, error],
        )?;
        let id = transaction.last_insert_rowid();
        transaction.commit()?;
        self.connection
            .query_row(
                &format!("{MANAGER_MESSAGE_SELECT} WHERE id = ?1"),
                [id],
                manager_message_from_row,
            )
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

    pub fn preferred_codex_home(&self, account_id: Option<&str>) -> Result<Option<CodexHome>> {
        let account_clause = if account_id.is_some() {
            "WHERE h.account_id = ?1"
        } else {
            ""
        };
        let sql = format!(
            "{CODEX_HOME_SELECT} {account_clause} ORDER BY CASE WHEN h.name = 'current' COLLATE NOCASE THEN 0 ELSE 1 END, h.created_at LIMIT 1"
        );
        if let Some(account_id) = account_id {
            self.connection
                .query_row(&sql, [account_id], codex_home_from_row)
                .optional()
                .map_err(Into::into)
        } else {
            self.connection
                .query_row(&sql, [], codex_home_from_row)
                .optional()
                .map_err(Into::into)
        }
    }

    pub fn apply_default_codex_home_to_unconfigured_tabs(&self, home: &CodexHome) -> Result<usize> {
        self.connection
            .execute(
                "UPDATE tabs SET account_id = ?1, codex_home_id = ?2, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE account_id IS NULL AND codex_home_id IS NULL",
                params![home.account_id, home.id],
            )
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
const MANAGER_SESSION_SELECT: &str = "SELECT m.id, m.kind, m.workspace_id, w.name, m.source_codex_home_id, m.native_thread_id, m.cwd, m.model, m.reasoning_effort, m.status, m.last_error, m.created_at, m.updated_at FROM manager_sessions m LEFT JOIN workspaces w ON w.id = m.workspace_id";
const MANAGER_MESSAGE_SELECT: &str =
    "SELECT id, manager_session_id, role, content, created_at FROM manager_messages";

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

fn workspace_context_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkspaceContext> {
    Ok(WorkspaceContext {
        workspace_id: row.get(0)?,
        path: PathBuf::from(row.get::<_, String>(1)?),
        kind: row.get(2)?,
        created_at: row.get(3)?,
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

fn manager_session_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ManagerSession> {
    let kind = row.get::<_, String>(1)?.parse().map_err(conversion_error)?;
    Ok(ManagerSession {
        id: row.get(0)?,
        kind,
        workspace_id: row.get(2)?,
        workspace_name: row.get(3)?,
        source_codex_home_id: row.get(4)?,
        native_thread_id: row.get(5)?,
        cwd: PathBuf::from(row.get::<_, String>(6)?),
        model: row.get(7)?,
        reasoning_effort: row.get(8)?,
        status: row.get(9)?,
        last_error: row.get(10)?,
        created_at: row.get(11)?,
        updated_at: row.get(12)?,
    })
}

fn manager_message_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ManagerMessage> {
    Ok(ManagerMessage {
        id: row.get(0)?,
        manager_session_id: row.get(1)?,
        role: row.get(2)?,
        content: row.get(3)?,
        created_at: row.get(4)?,
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
        NewAccount, NewCodexHome, NewCodexThread, NewCurrentCodex, NewManagerSession,
        NewModelProfile, NewTab, NewWorkspace, Registry,
    };
    use crate::model::{AccountAuthKind, AccountOwnerKind, ManagerKind, ModelProfileSettings};

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
                manager_account_id: None,
                manager_codex_home_id: None,
            })
            .unwrap();
        assert_eq!(workspace.name, "development");
        let contexts = registry.list_workspace_contexts(&workspace.id).unwrap();
        assert_eq!(contexts.len(), 1);
        assert_eq!(contexts[0].kind, "directory");
        assert_eq!(contexts[0].path, Path::new("/tmp"));
        registry
            .insert_workspace_context(&workspace.id, Path::new("/tmp/README.md"), "file")
            .unwrap();
        assert_eq!(
            registry
                .list_workspace_contexts(&workspace.id)
                .unwrap()
                .len(),
            2
        );
        let tabs = registry.list_tabs(&workspace.id).unwrap();
        assert_eq!(tabs.len(), 1);
        assert_eq!(tabs[0].name, "middle-manager");
        assert_eq!(
            registry
                .workspace_by_tmux_session("wipsaw-01900000")
                .unwrap()
                .unwrap()
                .id,
            workspace.id
        );
        assert_eq!(
            registry
                .tab_by_tmux_window(&workspace.id, "@1")
                .unwrap()
                .unwrap()
                .name,
            "middle-manager"
        );
        registry.delete_workspace(&workspace.id).unwrap();
        assert!(registry.workspace_by_ref(&workspace.id).unwrap().is_none());
        assert!(registry.list_tabs(&workspace.id).unwrap().is_empty());
        assert!(
            registry
                .list_workspace_contexts(&workspace.id)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn lumbergh_and_middle_managers_keep_separate_threads_and_history() {
        let (_root, mut registry) = registry();
        registry
            .bootstrap_current_codex(NewCurrentCodex {
                account_id: "acct_01900000000070008000000000000000",
                home_id: "home_01900000000070008000000000000000",
                path: Path::new("/tmp/current-codex-home"),
                codex_binary: Path::new("/usr/bin/codex"),
            })
            .unwrap();
        let workspace = registry
            .insert_workspace_with_manager(NewWorkspace {
                id: "ws_01900000000070008000000000000000",
                name: "development",
                tmux_session: "wipsaw-01900000",
                cwd: Path::new("/tmp/development"),
                manager_tab_id: "tab_01900000000070008000000000000000",
                manager_window_id: "@1",
                manager_window_index: 0,
                manager_account_id: Some("acct_01900000000070008000000000000000"),
                manager_codex_home_id: Some("home_01900000000070008000000000000000"),
            })
            .unwrap();
        let lumbergh = registry
            .insert_manager_session(NewManagerSession {
                id: "manager_01900000000070008000000000000000",
                kind: ManagerKind::Lumbergh,
                workspace_id: None,
                source_codex_home_id: "home_01900000000070008000000000000000",
                cwd: Path::new("/tmp"),
                model: "gpt-5.6-terra",
                reasoning_effort: "medium",
            })
            .unwrap();
        let middle = registry
            .insert_manager_session(NewManagerSession {
                id: "manager_01900000000070008000000000000001",
                kind: ManagerKind::MiddleManager,
                workspace_id: Some(&workspace.id),
                source_codex_home_id: "home_01900000000070008000000000000000",
                cwd: &workspace.cwd,
                model: "gpt-5.6-terra",
                reasoning_effort: "medium",
            })
            .unwrap();
        registry
            .append_manager_message(&lumbergh.id, "user", "list everything")
            .unwrap();
        registry.mark_manager_working(&lumbergh.id).unwrap();
        registry
            .complete_manager_turn(
                &lumbergh.id,
                "01900000-0000-7000-8000-000000000001",
                "Here is the overview.",
            )
            .unwrap();

        let messages = registry.list_manager_messages(&lumbergh.id, 10).unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, "user");
        assert_eq!(messages[1].role, "assistant");
        let updated = registry
            .manager_session_by_id(&lumbergh.id)
            .unwrap()
            .unwrap();
        assert_eq!(updated.status, "idle");
        assert_eq!(
            updated.native_thread_id.as_deref(),
            Some("01900000-0000-7000-8000-000000000001")
        );
        assert_eq!(
            registry
                .manager_session_for_workspace(Some(&workspace.id))
                .unwrap()
                .unwrap()
                .id,
            middle.id
        );
        assert!(
            registry
                .list_manager_messages(&middle.id, 10)
                .unwrap()
                .is_empty()
        );
        registry.delete_workspace(&workspace.id).unwrap();
        assert!(
            registry
                .manager_session_by_id(&middle.id)
                .unwrap()
                .is_none()
        );
        assert!(
            registry
                .manager_session_by_id(&lumbergh.id)
                .unwrap()
                .is_some()
        );
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
        assert_eq!(
            registry.preferred_codex_home(None).unwrap().unwrap().id,
            homes[0].id
        );

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
    fn preferred_home_backfills_only_unconfigured_tabs() {
        let (_root, mut registry) = registry();
        registry
            .insert_workspace_with_manager(NewWorkspace {
                id: "ws_01900000000070008000000000000000",
                name: "development",
                tmux_session: "wipsaw-01900000",
                cwd: Path::new("/tmp"),
                manager_tab_id: "tab_01900000000070008000000000000000",
                manager_window_id: "@1",
                manager_window_index: 0,
                manager_account_id: None,
                manager_codex_home_id: None,
            })
            .unwrap();
        registry
            .bootstrap_current_codex(NewCurrentCodex {
                account_id: "acct_01900000000070008000000000000000",
                home_id: "home_01900000000070008000000000000000",
                path: Path::new("/tmp/current-codex-home"),
                codex_binary: Path::new("/usr/bin/codex"),
            })
            .unwrap();
        let home = registry.preferred_codex_home(None).unwrap().unwrap();
        assert_eq!(
            registry
                .apply_default_codex_home_to_unconfigured_tabs(&home)
                .unwrap(),
            1
        );
        let manager = registry
            .tab_by_ref("ws_01900000000070008000000000000000", "middle-manager")
            .unwrap()
            .unwrap();
        assert_eq!(
            manager.account_id.as_deref(),
            Some(home.account_id.as_str())
        );
        assert_eq!(manager.codex_home_id.as_deref(), Some(home.id.as_str()));
        assert_eq!(
            registry
                .apply_default_codex_home_to_unconfigured_tabs(&home)
                .unwrap(),
            0
        );
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
                manager_account_id: None,
                manager_codex_home_id: None,
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
            ["middle-manager", "api"]
        );
    }

    #[test]
    fn workspace_recovery_can_swap_reused_tmux_window_ids() {
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
                manager_account_id: None,
                manager_codex_home_id: None,
            })
            .unwrap();
        let api = registry
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

        registry
            .replace_workspace_tab_targets(
                &workspace.id,
                &[
                    (
                        "tab_01900000000070008000000000000000".to_string(),
                        "@2".to_string(),
                        1,
                    ),
                    (api.id.clone(), "@1".to_string(), 0),
                ],
            )
            .unwrap();

        let tabs = registry.list_tabs(&workspace.id).unwrap();
        assert_eq!(tabs[0].name, "api");
        assert_eq!(tabs[0].tmux_window_id, "@1");
        assert_eq!(tabs[1].name, "middle-manager");
        assert_eq!(tabs[1].tmux_window_id, "@2");
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
                manager_account_id: None,
                manager_codex_home_id: None,
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
                manager_account_id: None,
                manager_codex_home_id: None,
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

        let second_tab = registry
            .insert_tab(NewTab {
                id: "tab_01900000000070008000000000000002",
                workspace_id: &workspace.id,
                name: "review",
                tmux_window_id: "@3",
                tmux_window_index: 2,
                cwd: Path::new("/tmp"),
                account_id: None,
                codex_home_id: None,
                model_profile_id: None,
                codex_thread_id: None,
            })
            .unwrap();
        let second_binding = registry
            .bind_tab_to_codex_thread(&second_tab.id, &thread)
            .unwrap();
        assert_eq!(
            second_binding.codex_thread_id.as_deref(),
            Some(thread.id.as_str())
        );
        assert_eq!(
            second_binding.model_profile_id.as_deref(),
            Some(profile.id.as_str())
        );
        assert_eq!(
            registry
                .list_workspace_codex_threads(&workspace.id)
                .unwrap()
                .len(),
            1
        );
        registry
            .delete_workspace_and_threads(&workspace.id, std::slice::from_ref(&thread.id))
            .unwrap();
        assert!(registry.workspace_by_ref(&workspace.id).unwrap().is_none());
        assert!(registry.codex_thread_by_ref(&thread.id).unwrap().is_none());
    }
}
