use std::ffi::OsString;
use std::path::PathBuf;
use std::str::FromStr;

use clap::{Args, Parser, Subcommand};
use serde::Serialize;

use crate::app::{TabLaunchSettings, WipsawApp};
use crate::doctor::DoctorReport;
use crate::error::{Result, WipsawError};
use crate::model::{AccountAuthKind, AccountOwnerKind, ModelProfileSettings};

#[derive(Debug, Parser)]
#[command(
    name = "wipsaw",
    version,
    about = "Manage Wipsaw Codex workspaces and WIPs"
)]
pub struct Cli {
    /// Emit machine-readable JSON.
    #[arg(long, global = true)]
    pub json: bool,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Adopt and verify the active Codex home, authentication, and app-server.
    Init,
    /// Check local Wipsaw dependencies and state.
    Doctor,
    /// Manage durable tmux-backed workspaces.
    Workspace(WorkspaceArgs),
    /// Manage terminal tabs in a workspace.
    Tab(TabArgs),
    /// Register account identities without storing secret values.
    Account(AccountArgs),
    /// Register isolated Codex homes.
    Home(HomeArgs),
    /// Create and inspect managed Codex threads.
    Thread(ThreadArgs),
    /// Manage reusable model and Codex launch profiles.
    Profile(ProfileArgs),
    /// Internal entry points used by Wipsaw-managed shell shortcuts.
    #[command(hide = true)]
    Shortcut(ShortcutArgs),
    /// Private MCP server used by embedded Wipsaw managers.
    #[command(hide = true)]
    ManagerMcp,
}

#[derive(Debug, Args)]
pub struct ShortcutArgs {
    #[command(subcommand)]
    pub command: ShortcutCommand,
}

#[derive(Debug, Subcommand)]
pub enum ShortcutCommand {
    /// Resume or create the Codex thread assigned to the current tab.
    Codex {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<OsString>,
    },
    /// Open this workspace's embedded Middle Manager.
    Manager,
    /// Open the single top-level Lumbergh manager in the dashboard.
    Lumbergh,
}

#[derive(Debug, Args)]
pub struct WorkspaceArgs {
    #[command(subcommand)]
    pub command: WorkspaceCommand,
}

#[derive(Debug, Subcommand)]
pub enum WorkspaceCommand {
    /// Create a private tmux session with a Middle Manager tab.
    Create {
        name: String,
        #[arg(long, default_value = ".")]
        cwd: PathBuf,
        /// Attach after creating the workspace.
        #[arg(long)]
        attach: bool,
    },
    /// List registered workspaces.
    List,
    /// Start or reconstruct a stopped workspace and its Middle Manager.
    Start { workspace: String },
    /// Permanently remove a workspace and stop its private tmux session.
    Delete {
        workspace: String,
        /// Confirm this destructive operation.
        #[arg(long)]
        yes: bool,
    },
    /// Manage the exact file and directory allowlist for this workspace's Middle Manager.
    Context {
        #[command(subcommand)]
        command: WorkspaceContextCommand,
    },
    /// Attach to a workspace by name or ID.
    Attach { workspace: String },
}

#[derive(Debug, Subcommand)]
pub enum WorkspaceContextCommand {
    /// Add one existing file or directory to the Middle Manager's read scope.
    Add { workspace: String, path: PathBuf },
    /// List the Middle Manager's explicit read scope.
    List { workspace: String },
    /// Remove one file or directory from the Middle Manager's read scope.
    Remove { workspace: String, path: PathBuf },
}

#[derive(Debug, Args)]
pub struct TabArgs {
    #[command(subcommand)]
    pub command: TabCommand,
}

#[derive(Debug, Subcommand)]
pub enum TabCommand {
    /// Create a tab in a workspace.
    Create {
        workspace: String,
        name: String,
        #[arg(long)]
        cwd: Option<PathBuf>,
        #[arg(long)]
        account: Option<String>,
        #[arg(long)]
        home: Option<String>,
        #[arg(long)]
        profile: Option<String>,
    },
    /// List registered and live tabs.
    List { workspace: String },
    /// Rename a tab by name or ID.
    Rename {
        workspace: String,
        tab: String,
        name: String,
    },
}

#[derive(Debug, Args)]
pub struct ProfileArgs {
    #[command(subcommand)]
    pub command: ProfileCommand,
}

#[derive(Debug, Subcommand)]
pub enum ProfileCommand {
    /// Create a reusable model and permission profile.
    Add {
        name: String,
        #[arg(long)]
        model: String,
        #[arg(long)]
        provider: Option<String>,
        #[arg(long)]
        reasoning_effort: Option<String>,
        #[arg(long)]
        search: Option<bool>,
        #[arg(long)]
        sandbox: Option<String>,
        #[arg(long)]
        approval_policy: Option<String>,
    },
    /// List model profiles.
    List,
}

#[derive(Debug, Args)]
pub struct AccountArgs {
    #[command(subcommand)]
    pub command: AccountCommand,
}

#[derive(Debug, Subcommand)]
pub enum AccountCommand {
    /// Register an account alias and a secret reference.
    Add {
        alias: String,
        #[arg(long, default_value = "chatgpt-session")]
        auth: String,
        #[arg(long, default_value = "personal")]
        owner: String,
        /// A secret URI, never a raw token. Supported: secret://, command://, stdin://, codex-home://.
        #[arg(long)]
        credential_ref: Option<String>,
    },
    /// List account metadata. Secret values are never returned.
    List,
}

#[derive(Debug, Args)]
pub struct HomeArgs {
    #[command(subcommand)]
    pub command: HomeCommand,
}

#[derive(Debug, Subcommand)]
pub enum HomeCommand {
    /// Register a Codex home to exactly one account.
    Add {
        name: String,
        #[arg(long)]
        account: String,
        #[arg(long)]
        path: PathBuf,
        #[arg(long, default_value = "codex")]
        codex_binary: PathBuf,
        #[arg(long)]
        create: bool,
    },
    /// List Codex homes and their owning account aliases.
    List,
    /// Verify an isolated Codex app-server can read this home.
    Probe { home: String },
}

#[derive(Debug, Args)]
pub struct ThreadArgs {
    #[command(subcommand)]
    pub command: ThreadCommand,
}

#[derive(Debug, Subcommand)]
pub enum ThreadCommand {
    /// Start and name a native Codex thread, then assign a Wipsaw ID.
    Create {
        name: String,
        #[arg(long)]
        home: String,
        #[arg(long)]
        cwd: Option<PathBuf>,
        #[arg(long)]
        profile: Option<String>,
        /// Workspace containing the optional tab binding.
        #[arg(long)]
        workspace: Option<String>,
        /// Existing tab to bind. Requires --workspace.
        #[arg(long)]
        tab: Option<String>,
    },
    /// Adopt and open an exact existing native Codex session without creating a new conversation.
    Import {
        native_thread_id: String,
        #[arg(long)]
        home: String,
        #[arg(long)]
        workspace: String,
        /// Existing destination tab. Omit to create a durable tab.
        #[arg(long)]
        tab: Option<String>,
        /// Name for a newly created tab. Defaults to the native session name.
        #[arg(long)]
        name: Option<String>,
        /// Replace a different thread currently bound to --tab without deleting its history.
        #[arg(long)]
        replace_existing: bool,
        /// Attach to the imported session after opening it.
        #[arg(long)]
        attach: bool,
    },
    /// List Wipsaw-managed Codex threads, optionally for one home.
    List {
        #[arg(long)]
        home: Option<String>,
    },
    /// Read current metadata from the owning Codex home by exact native ID.
    Inspect { thread: String },
    /// Permanently delete a native Codex thread and its Wipsaw record.
    Delete {
        thread: String,
        /// Confirm this destructive operation.
        #[arg(long)]
        yes: bool,
    },
    /// Replace a tab's shell with the mapped Codex TUI, then return to the shell on exit.
    Resume {
        thread: String,
        #[arg(long)]
        workspace: String,
        #[arg(long)]
        tab: String,
        /// Attach to the workspace after launching Codex.
        #[arg(long)]
        attach: bool,
    },
}

pub fn run(cli: Cli) -> Result<()> {
    if matches!(&cli.command, Some(Command::ManagerMcp)) {
        return crate::manager::run_manager_mcp_server();
    }
    let mut app = WipsawApp::from_env()?;
    match cli.command {
        None => crate::tui::run(&mut app)?,
        Some(Command::Init) => {
            let report = app.initialize()?;
            if cli.json {
                print_json(&report)?;
            } else {
                println!("Wipsaw initialization: {}", report.status);
                println!(
                    "  account: {} via home '{}'",
                    report.codex.account_alias, report.codex.home_name
                );
                println!("  Codex home: {}", report.codex.configured_path);
                println!("  Codex: {}", report.codex.codex_version);
                println!("  app-server: ready");
                println!("  shortcuts: {}", report.shortcut_bin.display());
                println!("  tmux socket: {}", report.tmux_socket);
                println!("  next: run `wipsaw`");
            }
        }
        Some(Command::Doctor) => {
            let report = DoctorReport::collect(&app);
            if cli.json {
                print_json(&report)?;
            } else {
                println!("Wipsaw doctor: {}", report.status);
                for check in report.checks {
                    let marker = if check.ok {
                        "ok"
                    } else if check.required {
                        "BLOCKED"
                    } else {
                        "optional"
                    };
                    println!("  {marker:>8}  {:<16} {}", check.name, check.detail);
                }
                println!("  state: {}", report.paths.state);
            }
        }
        Some(Command::Workspace(args)) => match args.command {
            WorkspaceCommand::Create { name, cwd, attach } => {
                let workspace = app.create_workspace(&name, &cwd)?;
                app.start_workspace(&workspace.id)?;
                if cli.json {
                    print_json(&workspace)?;
                } else {
                    println!(
                        "created workspace '{}' ({}) with its Middle Manager ready on tmux session {}",
                        workspace.name, workspace.id, workspace.tmux_session
                    );
                }
                if attach {
                    app.attach_workspace(&workspace.id)?;
                }
            }
            WorkspaceCommand::List => {
                let workspaces = app.list_workspaces()?;
                if cli.json {
                    let output = workspaces
                        .into_iter()
                        .map(|(workspace, live)| WorkspaceOutput { workspace, live })
                        .collect::<Vec<_>>();
                    print_json(&output)?;
                } else if workspaces.is_empty() {
                    println!("no workspaces");
                } else {
                    for (workspace, live) in workspaces {
                        println!(
                            "{}  {:<24} {:<7} {}",
                            workspace.id,
                            workspace.name,
                            if live { "live" } else { "stopped" },
                            workspace.cwd.display()
                        );
                    }
                }
            }
            WorkspaceCommand::Start { workspace } => {
                let started = app.start_workspace(&workspace)?;
                output(cli.json, &started, || {
                    let mut message = if started.restored {
                        format!(
                            "restored workspace '{}' with its Middle Manager ready",
                            started.workspace.name
                        )
                    } else {
                        format!(
                            "workspace '{}' is running with its Middle Manager ready",
                            started.workspace.name
                        )
                    };
                    if !started.reopened_threads.is_empty() {
                        message.push_str(&format!(
                            "; reopened {} Codex conversation(s)",
                            started.reopened_threads.len()
                        ));
                    }
                    if !started.reopen_failures.is_empty() {
                        message.push_str(&format!(
                            "; {} conversation(s) need attention",
                            started.reopen_failures.len()
                        ));
                    }
                    message
                })?;
            }
            WorkspaceCommand::Delete { workspace, yes } => {
                if !yes {
                    return Err(WipsawError::InvalidInput {
                        field: "workspace delete",
                        message: "requires --yes because this removes its tabs and manager history"
                            .to_string(),
                    });
                }
                let deleted = app.delete_workspace(&workspace)?;
                output(cli.json, &deleted, || {
                    format!(
                        "deleted workspace '{}' ({}) and {} native Codex thread(s)",
                        deleted.workspace.name,
                        deleted.workspace.id,
                        deleted.deleted_threads.len()
                            + usize::from(deleted.deleted_manager_thread_id.is_some())
                    )
                })?;
            }
            WorkspaceCommand::Context { command } => match command {
                WorkspaceContextCommand::Add { workspace, path } => {
                    let context = app.add_workspace_context(&workspace, &path)?;
                    output(cli.json, &context, || {
                        format!(
                            "added {} '{}' to the Middle Manager context",
                            context.kind,
                            context.path.display()
                        )
                    })?;
                }
                WorkspaceContextCommand::List { workspace } => {
                    let contexts = app.list_workspace_contexts(&workspace)?;
                    if cli.json {
                        print_json(&contexts)?;
                    } else if contexts.is_empty() {
                        println!("this workspace's Middle Manager has no file context");
                    } else {
                        for context in contexts {
                            println!("{:<9} {}", context.kind, context.path.display());
                        }
                    }
                }
                WorkspaceContextCommand::Remove { workspace, path } => {
                    let context = app.remove_workspace_context(&workspace, &path)?;
                    output(cli.json, &context, || {
                        format!(
                            "removed {} '{}' from the Middle Manager context",
                            context.kind,
                            context.path.display()
                        )
                    })?;
                }
            },
            WorkspaceCommand::Attach { workspace } => app.attach_workspace(&workspace)?,
        },
        Some(Command::Tab(args)) => match args.command {
            TabCommand::Create {
                workspace,
                name,
                cwd,
                account,
                home,
                profile,
            } => {
                let tab = app.create_tab(
                    &workspace,
                    &name,
                    cwd.as_deref(),
                    TabLaunchSettings {
                        account: account.as_deref(),
                        codex_home: home.as_deref(),
                        model_profile: profile.as_deref(),
                    },
                )?;
                output(cli.json, &tab, || {
                    format!("created tab '{}' ({})", tab.name, tab.id)
                })?;
            }
            TabCommand::List { workspace } => {
                let (workspace, tabs, windows) = app.list_tabs(&workspace)?;
                let output = tabs
                    .into_iter()
                    .map(|tab| {
                        let live = windows
                            .iter()
                            .find(|window| window.id == tab.tmux_window_id);
                        TabOutput {
                            tab,
                            live: live.is_some(),
                            active: live.is_some_and(|window| window.active),
                        }
                    })
                    .collect::<Vec<_>>();
                if cli.json {
                    print_json(&output)?;
                } else if output.is_empty() {
                    println!("no tabs in workspace '{}'", workspace.name);
                } else {
                    for item in output {
                        println!(
                            "{}  {:<24} index={:<3} {:<7}{}",
                            item.tab.id,
                            item.tab.name,
                            item.tab.tmux_window_index,
                            if item.live { "live" } else { "missing" },
                            if item.active { " active" } else { "" }
                        );
                    }
                }
            }
            TabCommand::Rename {
                workspace,
                tab,
                name,
            } => {
                let tab = app.rename_tab(&workspace, &tab, &name)?;
                output(cli.json, &tab, || format!("renamed tab to '{}'", tab.name))?;
            }
        },
        Some(Command::Account(args)) => match args.command {
            AccountCommand::Add {
                alias,
                auth,
                owner,
                credential_ref,
            } => {
                let account = app.add_account(
                    &alias,
                    AccountAuthKind::from_str(&auth)?,
                    AccountOwnerKind::from_str(&owner)?,
                    credential_ref.as_deref(),
                )?;
                output(cli.json, &account, || {
                    format!("registered account '{}' ({})", account.alias, account.id)
                })?;
            }
            AccountCommand::List => {
                let accounts = app.registry.list_accounts()?;
                if cli.json {
                    print_json(&accounts)?;
                } else if accounts.is_empty() {
                    println!("no accounts");
                } else {
                    for account in accounts {
                        println!(
                            "{}  {:<24} {:<20} {}",
                            account.id, account.alias, account.auth_kind, account.owner_kind
                        );
                    }
                }
            }
        },
        Some(Command::Home(args)) => match args.command {
            HomeCommand::Add {
                name,
                account,
                path,
                codex_binary,
                create,
            } => {
                let home = app.add_codex_home(&name, &account, &path, &codex_binary, create)?;
                output(cli.json, &home, || {
                    format!(
                        "registered Codex home '{}' ({}) for account '{}'",
                        home.name, home.id, home.account_alias
                    )
                })?;
            }
            HomeCommand::List => {
                let homes = app.registry.list_codex_homes()?;
                if cli.json {
                    print_json(&homes)?;
                } else if homes.is_empty() {
                    println!("no Codex homes");
                } else {
                    for home in homes {
                        println!(
                            "{}  {:<24} account={:<20} {}",
                            home.id,
                            home.name,
                            home.account_alias,
                            home.path.display()
                        );
                    }
                }
            }
            HomeCommand::Probe { home } => {
                let report = app.probe_codex_home(&home)?;
                if cli.json {
                    print_json(&report)?;
                } else {
                    println!(
                        "Codex home '{}' is app-server compatible ({} threads, {})",
                        report.home_name, report.thread_count, report.codex_version
                    );
                }
            }
        },
        Some(Command::Thread(args)) => match args.command {
            ThreadCommand::Create {
                name,
                home,
                cwd,
                profile,
                workspace,
                tab,
            } => {
                let binding = match (workspace.as_deref(), tab.as_deref()) {
                    (Some(workspace), Some(tab)) => Some((workspace, tab)),
                    (None, None) => None,
                    _ => {
                        return Err(WipsawError::InvalidInput {
                            field: "tab binding",
                            message: "--workspace and --tab must be supplied together".to_string(),
                        });
                    }
                };
                let thread = app.create_codex_thread(
                    &name,
                    &home,
                    cwd.as_deref(),
                    profile.as_deref(),
                    binding,
                )?;
                output(cli.json, &thread, || {
                    format!(
                        "created Codex thread '{}' ({}, native {})",
                        thread.name, thread.id, thread.native_thread_id
                    )
                })?;
            }
            ThreadCommand::Import {
                native_thread_id,
                home,
                workspace,
                tab,
                name,
                replace_existing,
                attach,
            } => {
                let imported = app.import_codex_session(
                    &workspace,
                    &home,
                    &native_thread_id,
                    tab.as_deref(),
                    name.as_deref(),
                    replace_existing,
                )?;
                output(cli.json, &imported, || {
                    let mut message = format!(
                        "imported native Codex session '{}' as thread '{}' in tab '{}'",
                        imported.thread.native_thread_id, imported.thread.id, imported.tab.name
                    );
                    if let Some(reason) = &imported.deferred_reason {
                        message.push_str(&format!("; launch deferred: {reason}"));
                    }
                    message
                })?;
                if attach && imported.launch.is_some() {
                    app.activate_tab(&imported.workspace_id, &imported.tab.id)?;
                }
            }
            ThreadCommand::List { home } => {
                let threads = app.list_codex_threads(home.as_deref())?;
                if cli.json {
                    print_json(&threads)?;
                } else if threads.is_empty() {
                    println!("no managed Codex threads");
                } else {
                    for thread in threads {
                        println!(
                            "{}  {:<24} native={:<36} home={:<20} model={:<20} {}",
                            thread.id,
                            thread.name,
                            thread.native_thread_id,
                            thread.codex_home_id,
                            thread.model,
                            thread.status
                        );
                    }
                }
            }
            ThreadCommand::Inspect { thread } => {
                let inspection = app.inspect_codex_thread(&thread)?;
                if cli.json {
                    print_json(&inspection)?;
                } else {
                    println!(
                        "Codex thread '{}' ({}) is {} in home '{}'",
                        inspection.managed.name,
                        inspection.managed.id,
                        inspection.native.status,
                        inspection.managed.codex_home_id
                    );
                    println!(
                        "  native={} model={} provider={} cwd={}",
                        inspection.native.native_thread_id,
                        inspection.managed.model,
                        inspection.native.model_provider,
                        inspection.native.cwd.display()
                    );
                    if let Some(path) = inspection.native.rollout_path {
                        println!("  rollout={}", path.display());
                    }
                }
            }
            ThreadCommand::Delete { thread, yes } => {
                if !yes {
                    return Err(WipsawError::InvalidInput {
                        field: "thread delete",
                        message:
                            "requires --yes because native Codex history is permanently removed"
                                .to_string(),
                    });
                }
                let deleted = app.delete_codex_thread(&thread)?;
                output(cli.json, &deleted, || {
                    format!(
                        "deleted Codex thread '{}' ({}, native {})",
                        deleted.name, deleted.thread_id, deleted.native_thread_id
                    )
                })?;
            }
            ThreadCommand::Resume {
                thread,
                workspace,
                tab,
                attach,
            } => {
                app.start_workspace(&workspace)?;
                let launch = app.resume_codex_thread(&thread, &workspace, &tab)?;
                output(cli.json, &launch, || {
                    format!(
                        "resumed Codex thread '{}' in tab '{}'",
                        launch.thread_id, launch.tab_id
                    )
                })?;
                if attach {
                    app.activate_tab(&launch.workspace_id, &launch.tab_id)?;
                }
            }
        },
        Some(Command::Profile(args)) => match args.command {
            ProfileCommand::Add {
                name,
                model,
                provider,
                reasoning_effort,
                search,
                sandbox,
                approval_policy,
            } => {
                let profile = app.add_model_profile(
                    &name,
                    &model,
                    ModelProfileSettings {
                        provider,
                        reasoning_effort,
                        search,
                        sandbox,
                        approval_policy,
                    },
                )?;
                output(cli.json, &profile, || {
                    format!(
                        "created model profile '{}' ({}) using model '{}'",
                        profile.name, profile.id, profile.model
                    )
                })?;
            }
            ProfileCommand::List => {
                let profiles = app.registry.list_model_profiles()?;
                if cli.json {
                    print_json(&profiles)?;
                } else if profiles.is_empty() {
                    println!("no model profiles");
                } else {
                    for profile in profiles {
                        println!(
                            "{}  {:<24} model={:<24} provider={}",
                            profile.id,
                            profile.name,
                            profile.model,
                            profile.provider.as_deref().unwrap_or("default")
                        );
                    }
                }
            }
        },
        Some(Command::Shortcut(args)) => match args.command {
            ShortcutCommand::Codex { args } => app.run_codex_shortcut(&args)?,
            ShortcutCommand::Manager => app.run_manager_shortcut()?,
            ShortcutCommand::Lumbergh => app.run_lumbergh_shortcut()?,
        },
        Some(Command::ManagerMcp) => unreachable!("manager MCP is handled before app startup"),
    }
    Ok(())
}

#[derive(Serialize)]
struct WorkspaceOutput {
    #[serde(flatten)]
    workspace: crate::model::Workspace,
    live: bool,
}

#[derive(Serialize)]
struct TabOutput {
    #[serde(flatten)]
    tab: crate::model::Tab,
    live: bool,
    active: bool,
}

fn output<T, F>(json: bool, value: &T, text: F) -> Result<()>
where
    T: Serialize,
    F: FnOnce() -> String,
{
    if json {
        print_json(value)
    } else {
        println!("{}", text());
        Ok(())
    }
}

fn print_json<T: Serialize>(value: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}
