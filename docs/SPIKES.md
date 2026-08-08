# Wipsaw Architecture Spikes

Status: Active
Date opened: 2026-08-08

## Purpose

Spikes retire architecture risk. They are not production features. Each spike
must record the environment, exact interface tested, result, limitations, and
the ADR or backlog decision it changes.

## Completed spikes

### SPIKE-001: Private tmux backend

Status: Passed
Decision: Supports ADR-001

Tested locally with tmux 3.4:

- created an isolated server with `tmux -L <socket> -f /dev/null`;
- created and named manager and Codex windows;
- set Wipsaw-specific status styles and formats;
- enumerated windows and active paths;
- confirmed `display-popup`, `display-menu`, `capture-pane`, and `pipe-pane`;
- destroyed only the temporary spike server.

Result: tmux can provide the durable PTY/window layer while Wipsaw owns its
look, navigation surfaces, metadata, and key table. This does not yet prove
cross-distribution packaging or nested SSH behavior.

Implementation update, 2026-08-08: the Rust adapter now creates a real private
workspace and manager window, creates and renames tabs, lists live state, and
generates the isolated Wipsaw theme. A disposable end-to-end CLI smoke test
passed against tmux 3.4. The smoke also caught and fixed tmux's octal escaping
of non-printing format separators; that behavior now has a regression test.

### SPIKE-002: Codex app-server thread identity

Status: Passed
Decision: Supports ADR-004 and ADR-006

Tested against Codex CLI 0.147.0 in a fresh temporary `CODEX_HOME`:

- initialized an app-server stdio JSON-RPC connection;
- confirmed the server reports its active Codex home;
- listed threads;
- started a thread without sending a model turn;
- received an exact native thread ID;
- set the thread's name;
- terminated the isolated process.

Result: Wipsaw can obtain and store supported thread IDs without scraping
rollout files. Account authentication, concurrent real-account homes, event
streaming, and version compatibility remain separate spikes.

Implementation update, 2026-08-08: `wipsaw thread create` now starts and names
a native thread through app-server, records both the native and Wipsaw IDs,
captures the resolved model/provider/reasoning, status, working directory, and
rollout path, and can bind that record to an existing tab in the same SQLite
transaction. A live disposable-home test confirmed the stored native ID is
readable from a new app-server process after creation. Codex `thread/list`
does not include this turnless thread, although `thread/read` by exact ID does;
Wipsaw therefore treats its own registry as the table of contents and never
uses native list visibility as proof that a known thread disappeared.

Launcher update, 2026-08-08: `wipsaw thread resume` now validates the target
tab's account, home, and model-profile binding, then starts `codex resume` by
exact native ID in that tab's tmux pane. It exports only non-secret Wipsaw and
thread identifiers plus `CODEX_HOME`; the credential remains inside that home.
A detached live smoke reached the Codex TUI login screen in a disposable,
unauthenticated home. The launch command restores the configured login shell
when Codex exits.

### SPIKE-003: Wiphand runtime and Docker executor inventory

Status: Passed
Decision: Supports ADR-003, ADR-005, and ADR-007

Confirmed locally:

- Docker 24.0.2 and Compose 2.18.1 are reachable;
- the Wiphand Compose file resolves 9 default services and 11 services with
  the optional agent-memory profile;
- the existing Wiphand API, workers, dispatcher, beat, Redis, Postgres,
  frontend, Mem0, and pgvector containers are running;
- Wiphand stores Docker container ID, image, networks, mounts, exit code,
  environment key names, stdout, and stderr in run metadata/output;
- the focused Docker-executor and API test suites pass;
- Wiphand exposes 43 route declarations and a CRUD helper/skill;
- no Wiphand MCP server implementation was found.

Result: source transplantation is lower risk than a scheduler rewrite. Stable
Wipsaw execution IDs and persisted streaming logs must be added because the
current public handle is still a research-run UUID plus backend metadata.

### SPIKE-004: Codex credential input capabilities

Status: Partially passed
Decision: Supports the direction of ADR-006

Codex CLI 0.147.0 confirms support for ChatGPT/device authentication plus API
key and access-token login through stdin. No secret value was read or changed.

Remaining proof: two authenticated homes must run concurrently without session,
usage, MCP, plugin, skill, or credential crossover.

Implementation update, 2026-08-08: Wipsaw now registers personal and company
account identities, rejects raw credential values in favor of secret
references, canonicalizes Codex-home paths, and prevents one writable home from
being assigned to two accounts. Unit and live CLI tests passed. Authentication
and concurrent app-server proof remain outstanding, so this spike is still only
partially passed.

### SPIKE-012: Navigator and shell-shortcut viability

Status: Passed for the local alpha
Decision: Keep Ratatui as the control surface and tmux as the durable PTY owner

Validated locally with Ratatui 0.30.2, Crossterm 0.29.0, tmux 3.4, Bash, and
the user's Oh My Zsh setup:

- rendered and exited both the wide test backend and a real 80-column
  pseudo-terminal without leaving raw or alternate-screen state behind;
- created a workspace and tab through the interactive prompts and confirmed
  their SQLite and tmux records;
- injected `codex`, `manager`, `lumberg`, and `lumbergh` only in Wipsaw shells;
- detected that a pre-existing `.zshrc` `codex` function outranked a PATH-only
  shim, then proved the managed rc loads the user's configuration first and
  the Wipsaw function last;
- confirmed the private tmux environment and managed `Ctrl-b w`, `Ctrl-b c`,
  `Ctrl-b ,`, and `Ctrl-b m` bindings.

Result: the initial TUI and shortcut architecture is usable without changing
global shell files. Nested SSH, custom shell profiles beyond Bash/Zsh, and the
bundled-tmux distribution matrix remain follow-up work.

Hardening update, 2026-08-08: repeated navigator prefix bindings now use a
session guard, making `Ctrl-b w` a close/toggle while the popup is active
instead of allowing recursively nested popups. Wipsaw also reloads its private
tmux config for live registered workspaces after upgrades.

Dashboard update, 2026-08-08: replaced the equal-weight three-table landing
screen with a manager-first Home view and separate Sessions, Threads, and WIPs
views. Live 144×42 and 80×24 pseudo-terminal checks confirmed that Lumbergh,
the primary action, navigation, selected workspace, and key guidance remain
visible across both layout tiers. The wide layout uses the source W logo's
center notch and saw cutout as Unicode full/half-block art; the compact layout
uses a one-cell badge. Ratatui backend tests cover both sizes and the first-run
Home-to-manager prompt flow.

### SPIKE-013: First-run Codex app-server recovery

Status: Passed for the local alpha

The first real managed workspace exposed an intermittent app-server close
during `initialize`. The same registered Codex home and request subsequently
passed direct initialization, `wipsaw home probe`, and eight concurrent probe
processes, so authentication and the request shape were not the failure.

The adapter now captures bounded app-server stderr, retries transient startup
and pipe closes three times with backoff, and returns an initialization-specific
error with a `wipsaw init` recovery path. Tests cover both a server that closes
twice before succeeding and a persistent failure whose stderr must reach the
operator. `wipsaw init` provides an idempotent first-run verification command,
and new or previously unconfigured tabs inherit the preferred current home.

## Priority spikes

### SPIKE-005: Multi-account and multi-home isolation

Priority: P0
Status: In progress

Questions:

- Can two authenticated Codex homes run app-server concurrently?
- Does every created thread remain discoverable only from its owning home?
- Which files are mutated by login, plugins, MCPs, skills, and usage state?
- Can an API-funded home and ChatGPT-session home coexist safely?
- What account identity and rate-limit metadata can be displayed without
  reading secrets?

Exit criteria:

- automated two-home isolation test;
- no credential value in argv, logs, tmux environment, SQLite, or Postgres;
- documented logout/re-auth/expired-token behavior;
- accepted secret-provider interface.

This spike requires dedicated test credentials or user-controlled interactive
login and must not repurpose existing account state silently.

Implementation update, 2026-08-08: a disposable two-home smoke test confirmed
that separate app-server processes report only the explicitly registered
`CODEX_HOME`, and tab creation rejects an account/home mismatch. Both homes
were deliberately unauthenticated, so this proves process/path isolation only;
credential, usage, plugin, MCP, and skill crossover remain unproven.

### SPIKE-006: Codex execution event and log capture

Priority: P0

Test `thread/start`, `turn/start`, notifications, `thread/read`, interruption,
and persisted history. Determine whether a scheduled Codex job should use
app-server directly or `codex exec --json` while still returning a native
thread ID.

Exit criteria:

- create `run_id` and `execution_id` before dispatch;
- persist an exact native thread ID;
- stream inspectable progress without making the thread interactive;
- recover final state after Wipsaw/runtime restart;
- document when `resumable` is true or false.

### SPIKE-007: Docker execution handle and durable logs

Priority: P0

Add a disposable test WIP that emits interleaved stdout/stderr, survives long
enough for follow mode, exits, and is removed.

Exit criteria:

- `execution_id` works before container creation and after removal;
- ordered/paged/followed log semantics are defined;
- truncation and retention behavior are explicit;
- cancellation, timeout, missing daemon, and daemon restart are covered;
- raw Docker identifiers remain diagnostic details only.

### SPIKE-008: Wiphand-to-Wipsaw compatibility transplant

Priority: P0

Create a temporary import branch or copy after the repository strategy is
approved. Introduce `/api/v1/wips` and Wipsaw schemas while keeping existing
tables and legacy contract tests operational.

Exit criteria:

- create/list/update/preview/trigger a WIP through renamed APIs;
- existing Wiphand tests remain green or have intentional migration notes;
- trigger returns `wip_id`, `run_id`, and `execution_id`;
- skill is renamed and uses Wipsaw CLI/API terminology;
- a source inventory classifies all domain scripts into core, bundled pack, or
  deprecated with no silent deletion.

### SPIKE-009: Model controls end to end

Priority: P0
Status: In progress

Validate global/profile/tab/WIP/run precedence for model, provider, reasoning
effort, search, sandbox, and approval policy across app-server, CLI fallback,
and container execution.

Exit criteria:

- resolved setting includes provenance;
- unsupported model/account combinations fail before dispatch;
- execution audit stores selected account/model but no credential;
- a one-run override does not mutate the WIP or tab default.

Implementation update, 2026-08-08: reusable profiles and tab bindings now
cover model, provider, reasoning effort, web search, sandbox, and approval
policy. Unit tests assert the exact app-server payload, and a live Codex
0.147.0 test returned the requested model, provider, and reasoning effort.
Layer provenance, WIP/run precedence, account/model compatibility, and
container propagation remain open.

### SPIKE-010: Bundled tmux and Linux compatibility

Priority: P1

Evaluate system-tmux preference and a bundled fallback across supported Linux
libc/distribution and terminal combinations. Record tmux and dependency license
notices required for distribution.

Minimum matrix:

- glibc and chosen musl target if feasible;
- bash and zsh;
- common true-color terminal, 256-color terminal, and SSH terminal;
- system tmux absent, compatible, and too old;
- `$TERM`, Unicode width, clipboard, popup, and resize behavior.

Exit criteria: a clean Linux environment with no tmux can launch Wipsaw using
the bundled fallback without changing the user's tmux configuration.

### SPIKE-011: Hardened container authority

Priority: P1 before external release

Compare a narrow executor service, rootless Docker, and rootless Podman for the
inherited job set. Inventory which current Wiphand jobs truly require broad
host access or Docker socket access.

Exit criteria:

- API/frontend never receive the Docker socket;
- job mounts come from approved resource bindings;
- secret delivery is account-scoped and redacted;
- threat model covers a malicious job image and compromised worker;
- compatibility exceptions are explicit and visibly unsafe.

### SPIKE-012: Remote SSH and hidden tmux

Priority: P1

Test local Wipsaw tmux containing an SSH connection to a remote hidden tmux
server with the remote prefix/status disabled and operations proxied locally.

Exit criteria:

- local prefix keys remain reliable;
- remote work survives disconnect/reconnect;
- resize, color, mouse, paste, and shell signals behave correctly;
- local/remote paths and Codex homes cannot be confused;
- host-key verification uses OpenSSH behavior and is never bypassed silently.

### SPIKE-013: Custom look and extension boundary

Priority: P1

Build a throwaway Ratatui navigator popup/drawer plus generated tmux status
theme. Test theme manifests, key maps, and a non-executable WIP template.

Exit criteria:

- Wipsaw looks distinct without patching tmux;
- navigator can toggle and pin without damaging the active application;
- 16/256/true-color degradation is acceptable;
- extension manifests have version, origin, permissions, and compatibility;
- executable hooks remain disabled by default.

### SPIKE-014: Usage and account-assistance surface

Priority: P2

Inventory supported Codex/app-server usage and rate-limit fields by auth kind.
Design a provider-capability model for opening login/account-management flows or
walking a user through their own supported reset workflow.

Exit criteria:

- displayed values have a named source and timestamp;
- unavailable values are shown as unknown, not estimated facts;
- Wipsaw never claims to reset ChatGPT/OpenAI credits;
- no automation violates provider account controls or terms.

## Spike execution order

```text
005 account isolation ─┐
006 Codex events ──────┼──> 009 model controls
007 durable logs ──────┤
008 runtime transplant ┘

010 tmux packaging ───────> first distributable local alpha
011 Docker hardening ─────> external alpha
012 SSH ──────────────────> remote alpha
013 extensions ───────────> customization alpha
014 usage ────────────────> account dashboard
```

## Required spike report format

```text
Spike:
Date/environment:
Question:
Setup:
Observed result:
Artifacts/tests:
Limitations:
Decision changed:
Follow-up:
```
