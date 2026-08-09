# Wipsaw Product Specification

Status: Approved baseline; implementation in progress
Date: 2026-08-08
Platform: Linux only

## Product summary

Wipsaw is a terminal workspace for people who operate several Codex sessions
and scheduled AI jobs. It combines a familiar tmux interaction model with
named Codex tabs, multiple accounts and Codex homes, WIP scheduling, execution
observation, remote Linux hosts, one top-level Codex manager named Lumbergh,
and a workspace-scoped Middle Manager for every workspace.

Wipsaw is both human-operated and agent-operable. Every important TUI action
must have a structured CLI or MCP equivalent so Lumbergh can manage the same
environment without simulating keystrokes.

## Product principles

1. **Tmux knowledge transfers.** Existing tmux navigation and window concepts
   should work with minimal relearning.
2. **The terminal remains real.** Shells, Codex, SSH, and developer tools run in
   genuine PTYs managed by tmux, not a partial terminal emulator.
3. **Names are human; IDs are durable.** Users work with names while Wipsaw
   stores stable identifiers for tabs, threads, WIPs, runs, executions, hosts,
   homes, and accounts.
4. **Observation is separate from interaction.** Opening a run shows progress,
   logs, artifacts, and Codex history without silently resuming or changing it.
5. **Configuration is explicit and layered.** The resolved account, model,
   permissions, tools, context, shell, and host must be inspectable before a
   launch.
6. **Credentials have owners.** Personal, employer, and API-funded accounts are
   distinct and never silently pooled.
7. **Wipsaw is extensible, not patchable.** Themes, tools, WIP templates,
   skills, MCPs, and integrations use documented extension points.

## Vocabulary

| Term | Meaning |
| --- | --- |
| Workspace | A named Wipsaw environment backed by one private tmux session. |
| Tab | A Wipsaw record backed by a tmux window. |
| Pane | A terminal PTY backed by a tmux pane. |
| Codex thread | A resumable Codex conversation/session ID. |
| Account | A ChatGPT/Codex login or API-funded identity. |
| Codex home | The isolated Codex config, auth, sessions, skills, plugins, and state directory for an account. |
| Profile | Reusable settings for model, permissions, context, tools, shell, or execution. |
| WIP | A recurring or on-demand schedule definition, repurposed from a Wiphand research request. |
| Run | One scheduled or manually triggered occurrence of a WIP. |
| Execution | The observable Docker, Codex, or process execution underlying a run. |
| Host | The local machine or a configured Linux SSH server. |
| Lumbergh | The single top-level manager thread embedded in the Home dashboard. |
| Middle Manager | A separate manager thread scoped to one workspace and opened from that workspace's manager tab. |

## Primary users

### Individual operator

Uses personal ChatGPT accounts, API keys, local repositories, and scheduled
jobs. Wants durable sessions and a fast overview of what is active.

### Company developer

Keeps an employer-provided ChatGPT account and Codex home separate from
personal accounts. Needs explicit account, model, permission, and directory
selection per workspace or tab.

### Multi-host operator

Runs Codex and WIPs on several Linux servers over SSH. Needs host health,
remote session persistence, path-aware profiles, and one local navigator.

## Core experience

Launching `wipsaw` opens a manager-first Home dashboard with Lumbergh embedded
directly in it, so a new user can type a request without leaving Wipsaw or
learning its information architecture first. Lumbergh is the one top manager
across the environment. Creating a workspace adds a separate Middle Manager
scoped to that workspace. Both manager types use the selected machine's
existing Codex authentication, resumable non-interactive Codex threads, and a
private Wipsaw-only tool surface. A branded tmux status line shows tabs, and
the navigator can also be opened as a popup or pinned drawer from an ordinary
shell or Codex TUI.

Navigator modes:

- Home: manager entry point, quick actions, active workspace, live system and
  identity summary, onboarding, and honest notices for unavailable runtimes.
- Sessions: workspaces, tabs, panes, Codex threads, activity, account, model.
- WIPs: schedules, next run, current run, health, last result.
- Runs: queued/running/completed/failed executions and artifacts.
- Hosts: connectivity, tmux, Codex version, Docker, and registered homes.
- Accounts: aliases, type, login status, usage observations, and ownership.
- Usage: provider-reported or locally observed usage and reset times. This is
  informational and account-management assistance; Wipsaw does not claim to
  reset provider credits.

## Tmux-compatible interaction

The initial prefix remains `Ctrl-b`. Wipsaw preserves common tmux meanings:

| Binding | Behavior |
| --- | --- |
| `Ctrl-b c` | Create a managed tab. |
| `Ctrl-b ,` | Rename the current tab and linked Codex thread when supported. |
| `Ctrl-b n` / `p` | Next or previous tab. |
| `Ctrl-b 0..9` | Select tab by index. |
| `Ctrl-b %` / `"` | Split the current tab. |
| `Ctrl-b &` | Close a tab after showing what will remain resumable. |
| `Ctrl-b w` | Toggle the Wipsaw navigator. |
| `Ctrl-b m` | Open the current workspace's Middle Manager. |
| `Ctrl-b e` | Open settings for the current tab or WIP. |
| `Ctrl-b g` | Open the WIP schedule view. |

Bindings are configurable. Destructive operations must retain confirmation or
policy checks even when invoked by Lumbergh.

## Functional requirements

### Workspaces, tabs, and terminals

- Create, rename, reorder, move, detach, reattach, archive, and delete tabs.
- Delete a workspace only through an exact resolved target and explicit
  confirmation; stop its private tmux session, permanently delete native Codex
  threads owned by its tabs and Middle Manager, and cascade its tabs, context,
  and scoped manager history without touching Lumbergh. If native deletion
  fails, keep the durable workspace graph available for a safe retry.
- Restore Wipsaw after its TUI or daemon restarts; tmux remains the durable PTY
  owner. If the private tmux session itself is gone, opening the workspace must
  reconstruct registered tabs, reconcile ephemeral window IDs, and restore the
  Middle Manager entry point and native thread mapping without requiring manual
  database repair.
- Support bash and zsh initially, including user-selected rc files and
  Oh My Zsh configuration.
- Inject Wipsaw shortcuts without editing the user's global shell files.
- Provide `codex`, `manager`, `lumberg`, and `lumbergh` commands inside managed
  shells. `manager` targets the current Middle Manager; `lumberg` and
  `lumbergh` target the one dashboard Lumbergh.
- Resolve and validate a working directory on the selected host.
- Allow custom status-line themes, navigator themes, and key maps.

### Codex sessions

- Create, name, list, search, resume, fork, archive, and inspect threads.
- Track thread ID together with host, Codex home, account, working directory,
  Codex version, model profile, and originating tab or run.
- Use the Codex app-server protocol when compatible and a versioned CLI adapter
  otherwise.
- Never infer a thread by scanning for the newest rollout when a supported
  thread-returning interface is available.
- Distinguish read-only inspection from interactive resume.

### Accounts and credentials

- Register multiple personal, employer, or service accounts with user-defined
  aliases.
- On first local startup, adopt the current machine's active Codex home and
  authentication as `current`; do not require users to register what Codex
  already uses.
- Support ChatGPT/Codex session authentication, Codex access tokens, and OpenAI
  API keys.
- Give every ChatGPT/Codex account a separate Codex home.
- Allow more than one API-funded account and select one per profile, tab, WIP,
  or run override.
- Read secrets from stdin, Linux secret storage, or explicit external secret
  references. Do not place secret values in CLI arguments, tmux formats,
  SQLite/Postgres rows, logs, or generated manifests.
- Display account identity and login health without displaying credentials.
- Record which account funded an execution for audit and usage attribution.

### Models and launch settings

Both tabs and WIPs must support:

- model provider and model;
- reasoning effort where supported;
- Codex profile and feature flags;
- web search;
- sandbox and approval policy;
- writable and read-only context directories;
- context files;
- skills and plugins;
- MCP servers;
- CLI tools and PATH additions;
- account/Codex home;
- local or SSH host;
- shell, rc files, theme, environment references, and working directory.

The UI must show the final resolved value and the layer that supplied it.

Resolution order is:

```text
global < host < account/home < named profile < workspace < tab or WIP < run override
```

### WIPs and schedules

- Retain Wiphand's recurring scheduling, schedule preview, overlap warnings,
  manual triggering, expected duration, time zones, execution profiles,
  dynamic context, memory, result sinks, artifacts, and run history.
- Rename the public API and product vocabulary from research requests to WIPs.
- Create, read, update, pause, resume, trigger, clone, archive, and delete WIPs.
- Provide both cron entry and a human-readable schedule editor.
- Link a WIP optionally to a separate interactive supervision thread.
- Allow model and account selection at WIP level with a one-run override.
- Preserve reusable Wiplash, Bottube, Moltbook, outreach, media, and research
  job definitions as bundled extension packs rather than hard-coding their
  names into the generic scheduler.

### Runs, executions, and logs

Every trigger returns three Wipsaw-owned identifiers:

- `wip_id`: stable schedule identity;
- `run_id`: one occurrence and its result state;
- `execution_id`: the observable runtime handle.

The execution record may resolve internally to a Docker container ID, Codex
thread ID, process ID, remote host, or a combination. Those backend IDs are not
the public identity.

Users and Lumbergh can:

- follow or page persisted stdout/stderr;
- inspect the current container state;
- inspect the Codex thread and turn status without resuming it;
- open artifacts and result metadata;
- see model, account, host, image, command summary, timing, and exit status;
- explicitly resume an interactive thread when the execution supports it;
- retain logs after a container is removed according to a configurable policy.

### Wiphand capability migration

The initial source transplant retains:

- FastAPI API and migrations;
- Postgres, Redis, Celery beat, dispatcher, and workers;
- generic Codex and Docker executors;
- schedule previews and calendar feed;
- run history, logs, artifacts, review state, and completion policies;
- execution profiles and resource bindings;
- optional Mem0 and pgvector services;
- current web frontend as an optional rebranded schedule/artifact dashboard;
- current scripts and domain-specific jobs, later sorted into extension packs.

The Wiphand skill becomes a Wipsaw skill. No Wiphand MCP server exists in the
current source inventory; Wipsaw will introduce one rather than rename a
nonexistent implementation.

### Agent management

- One Lumbergh coordinates the whole environment; every workspace owns one
  Middle Manager that escalates cross-workspace decisions to Lumbergh.
- Managers run through resumable `codex exec --json` threads at the configured
  manager model/effort default, initially `gpt-5.6-terra` with medium effort.
- Manager Codex homes reuse the selected account's authentication by reference
  while ignoring inherited user configuration.
- Manager sessions expose only the Wipsaw manager, `skill-creator`, and
  `skill-installer` skills plus private validated Wipsaw MCP tools. The latter
  is presented as the built-in skill lookup/installer. Managers do not inherit
  personal MCPs, plugins, apps, unrelated skills, shell execution, image tools,
  or multi-agent tools.
- Typed manager tools list workspaces with their tabs, create a new tab with an
  immediately bound and running Codex session, start or repair a session in an
  existing tab, search and read bounded native Codex history, and atomically
  create a launched tab whose new thread begins with a curated handoff summary.
  Raw tab creation is shell-only and cannot be reported as a Codex session. The
  standard local Codex home remains searchable read-only even when it is not
  registered as a destination home.
- Historical excerpts contain only user and final-assistant text; tool output,
  reasoning, likely credential assignments, and excess context are excluded.
- Lumbergh has machine-wide read access through credential-filtered private
  file tools and lazy absolute-path `@` browsing. Every Middle Manager is
  technically restricted to explicit file/directory context rows owned by its
  workspace; workspace creation seeds the selected working directory, and the
  user or Lumbergh can add/remove roots.
- A Middle Manager's workspace ID also constrains Wipsaw operations: it cannot
  target another workspace, create/delete workspaces, change global identity
  homes, or mutate its own scope. It escalates those operations to Lumbergh.
- The embedded composer supports UTF-8-safe caret movement and insertion across
  lines, Home/End and word navigation, deletion, multiline paste, mouse caret
  placement, scoped `@` file completion, `$` completion over the skills allowed
  in that manager home, and a blank `λ` prompt instead of placeholder content.
  Referenced file contents
  are bounded, credential-filtered, and confined to the same enforced scope as
  the private file tools.
- Codex reasoning, tool calls, completion/failure state, and usage render in
  order inside the manager response box before the final answer.
- Copy actions target the latest manager response or its transcript. A
  redraw-stable, manager-only selection view also supports native terminal copy
  without selecting the surrounding dashboard.
- Long manager conversations scroll relative to the live bottom. Composer mode
  keeps prompt arrows for editing while Page Up/Down and the mouse wheel move
  history without progress snapping it back. The transcript-only view supports
  arrows, Page Up/Down, Home, and End with mouse capture disabled for selection.
- Manager mutations trigger inventory reconciliation so tabs, Codex bindings,
  thread details, and dashboard counts update without a manual refresh.
- Human TUI, CLI, and MCP actions call the same application services.
- The manager can organize tabs, create WIPs, inspect runs, change safe
  settings, and report health.
- High-risk changes such as enabling unrestricted execution, deleting durable
  state, changing secret ownership, or exposing new host paths require policy
  evaluation and an audit entry.

### SSH hosts

- Register Linux hosts using existing OpenSSH configuration and keys.
- Detect remote tmux, Codex, Docker, shells, homes, and working directories.
- Keep remote Codex homes and credentials on the remote host unless explicitly
  migrated.
- Treat local and remote paths as different namespaces.
- Preserve remote work through a hidden remote tmux backend when SSH drops.

### Extensibility

Extension types include themes, key maps, WIP templates, job packs, context
builders, result sinks, CLI tools, skills, MCP definitions, account providers,
and host adapters.

Extensions live under XDG configuration/data directories and use a versioned
manifest. Executable hooks are opt-in and must have visible trust and origin
metadata. A future browser companion must perform a browser-native workflow,
such as sending the current page or selection into a Wipsaw tab or WIP, rather
than acting only as a launcher.

## Non-functional requirements

- Reattach to an existing local workspace in under two seconds on a typical
  development machine, excluding account network checks.
- Do not lose live shells when the Wipsaw UI crashes.
- Do not expose credentials in process listings or logs.
- Use opaque, sortable Wipsaw IDs and idempotency keys for create/trigger APIs.
- Paginate all growing lists and logs.
- Record an audit event for credential selection, permission-profile changes,
  WIP mutations, execution lifecycle changes, and destructive actions.
- Degrade gracefully when Docker, an SSH host, Wiphand-derived services, or a
  particular Codex home is unavailable.
- Support light and dark terminal palettes and 16/256/true-color terminals.

## Packaging

- Linux-only release.
- Prefer a compatible system tmux when present; ship a pinned, tested tmux
  fallback so lack of tmux does not prevent startup.
- Launch tmux on a private socket with a generated Wipsaw config, never by
  rewriting the user's tmux configuration.
- Provide `wipsaw doctor` for tmux, terminal, shell, Codex, account, Docker,
  Compose, SSH, ports, storage, and service checks.
- Wipsaw runtime images are versioned together and managed through
  `wipsaw service` commands.

## Release slices

### Slice 0: architecture spikes

Validate private tmux operation, Codex thread control, account isolation,
Docker log handles, Wiphand migration, model overrides, and Linux packaging.

### Slice 1: local session workspace

Private tmux, Rust launcher/controller, tabs, navigator, one or more Codex
homes, model profiles, naming/resume, settings inspector, embedded Lumbergh,
workspace Middle Managers, and the private manager MCP surface.

### Slice 2: WIP runtime

Transplanted scheduler/container stack, renamed APIs, stable run/execution IDs,
logs, artifacts, model/account selection, Wipsaw skill, and MCP server.

### Slice 3: remote and extensions

SSH hosts, hidden remote tmux, extension manifests, themes, job packs, usage
views, and hardened secret-provider integrations.

## Explicitly deferred

- macOS and Windows support.
- Replacing tmux with a custom multiplexer.
- Claiming or automating unsupported provider credit resets.
- Sharing one mutable Codex home across multiple authenticated accounts.
- A public extension marketplace before signing, trust, and update policies are
  designed.
