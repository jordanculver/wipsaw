<p align="center">
  <img src="assets/brand/wipsaw-logo.png" alt="Wipsaw angular W with a saw-tooth cutout" width="420">
</p>

<h1 align="center">Wipsaw</h1>

Wipsaw is a Linux-first, tmux-style workspace for managing Codex terminals,
multiple Codex and ChatGPT accounts, remote hosts, and scheduled WIPs inherited
from Wiphand.

The project is currently in its first usable alpha slice. The Rust CLI and
Ratatui navigator can create and inspect private tmux workspaces/tabs, register
isolated account and Codex-home metadata, and create, inspect, and resume named
native Codex threads with Wipsaw-managed IDs. The WIP runtime transplant,
settings editor, secret-provider integration, and public WIP MCP surface are
not implemented yet. A private, capability-scoped manager MCP server is live.

## Quick start

Wipsaw is for Linux Codex users. The current alpha requires a stable Rust
toolchain, the Codex CLI, and tmux 3.4 or later.

```bash
git clone https://github.com/jordanculver/wipsaw.git
cd wipsaw
cargo install --path .
wipsaw init
wipsaw doctor
wipsaw
```

`wipsaw init` adopts the active `CODEX_HOME` and existing authentication,
verifies the exact Codex binary and app-server handshake, installs the managed
shell shortcuts, and prints the selected account/home before anything is
launched. It is safe to rerun as a setup or repair check.

The navigator opens on a dashboard instead of dropping a new user into several
empty tables. Lumbergh—the one top-level manager—is embedded directly on Home
and is ready as soon as Wipsaw starts. Press `Enter` to focus its composer and
describe what you want to set up, run, or inspect. Press `m` from any view to
return to Lumbergh. Use `c` to create a workspace; every workspace receives a
separate embedded **Middle Manager** that is opened from its manager tab in
Sessions.

Lumbergh and every Middle Manager run as resumable `codex exec --json` threads
with `gpt-5.6-terra` and medium reasoning. Their generated Codex homes link the
selected account's existing `auth.json` without copying it, ignore personal
Codex configuration, and expose only the Wipsaw manager skill plus two private
Wipsaw MCP tools. Personal MCPs, plugins, apps, skills, shell execution, image
tools, and multi-agent tools are not loaded into manager sessions.

Use `1` through `4` for Home, Sessions, Threads, and WIPs; `Tab` cycles between
those views. `c` creates a workspace, `n` creates and starts a named Codex
session, and `t` creates a shell tab. Press `Enter` for the current view's
primary action and `?` for the complete guide. The layout collapses from a
dashboard with navigation and live system status to a compact header at narrow
terminal widths.

Opening any stopped workspace or one of its tabs automatically reconstructs
the private tmux session from Wipsaw's registry, reconciles new tmux window
IDs, and restores its Middle Manager entry point. To repair or warm one without
attaching, run:

```bash
wipsaw workspace start "workspace name"
```

On a new Wipsaw registry, the first command automatically adopts the active
`CODEX_HOME`, or `~/.codex` when that variable is unset, as the `current`
account and home. Wipsaw references that directory in place, so the user's
existing Codex authentication, sessions, settings, skills, and plugins remain
available to ordinary Codex tabs without another login or copied credential;
manager sessions retain the isolated surface described above. Set
`WIPSAW_AUTO_ADOPT_CODEX=0` to disable this behavior.

App-server initialization is retried three times for transient process and
pipe failures. If all attempts fail, Wipsaw includes the captured Codex stderr
and points back to `wipsaw init` instead of returning only a closed-protocol
message.

Power users can then register additional, isolated account identities and
Codex homes:

```bash
wipsaw account add company \
  --auth chatgpt-session \
  --owner company

wipsaw home add company-home \
  --account company \
  --path "$HOME/.local/share/wipsaw/codex-homes/company" \
  --codex-binary "$(command -v codex)" \
  --create

wipsaw profile add careful \
  --model gpt-5.6-sol \
  --reasoning-effort high \
  --search true \
  --sandbox workspace-write \
  --approval-policy on-request

wipsaw tab create development api \
  --account company \
  --home company-home \
  --profile careful

wipsaw thread create "API implementation" \
  --home company-home \
  --profile careful \
  --workspace development \
  --tab api

wipsaw thread list --home company-home
wipsaw thread inspect thread_...
wipsaw thread resume thread_... \
  --workspace development \
  --tab api \
  --attach
```

`thread resume` replaces the target tab's shell process with the Codex TUI,
using the thread's owning Codex home and existing authentication. When Codex
exits, Wipsaw starts the managed shell again in that tab.

Every Wipsaw shell preserves the user's Bash or Zsh configuration, including
Oh My Zsh, and then installs tab-aware commands without changing global shell
files:

- `codex [resume options]` resumes the tab's mapped thread, creating and naming
  it on first use with the tab's home/account/model settings;
- `manager` opens the current workspace's Middle Manager;
- `lumberg` and `lumbergh` return to the single dashboard Lumbergh;
- `Ctrl-b w` opens the navigator as a tmux popup, `Ctrl-b c` opens its managed
  tab-creation prompt, `Ctrl-b ,` renames the current managed tab, and
  `Ctrl-b m` opens the current workspace's Middle Manager.

Common navigator controls are `h/j/k/l`, arrow keys, `1`/`2`/`3`/`4`, `Tab`,
`Enter`, `c`, `n`, `t`, `m`, `,` to rename a focused tab, `r` to refresh, and
`q` to close. Its prefix mode accepts `Ctrl-b` followed by `w`, `s`, `t`, `g`,
`m`, `n`, `p`, `c`, `?`, or `q`.

## Navigator design

<p align="center">
  <img src="docs/images/wipsaw-dashboard-concept.png" alt="Wipsaw manager-first terminal dashboard design reference" width="960">
</p>

The image above is the implementation reference. The live TUI uses real
registry, tmux, Codex, account, model-profile, and dependency state; it does
not invent sample sessions. WIPs are visibly marked as the next runtime slice
until the Wiphand scheduler transplant is connected.

API-key and access-token accounts require a reference such as
`secret://account/company-openai`; the CLI rejects raw credential-looking
values. Secret-provider resolution is part of the next account milestone.

Add `--json` to supported commands for manager-friendly output. Errors also use
the documented `{ "error": { "code", "message" } }` shape in JSON mode.

## Documents

- [Product specification](docs/PRODUCT_SPEC.md)
- [Architecture specification](docs/ARCHITECTURE.md)
- [Spike plan and results](docs/SPIKES.md)

## Current decisions

- Linux only.
- Rust host application with Ratatui and Clap.
- MIT license.
- A private tmux server provides durable terminal windows and panes.
- A tested tmux build will be bundled as a fallback when the host has no
  compatible tmux installation.
- Wiphand's scheduler, workers, executors, APIs, persistence, and container
  stack will be transplanted and repurposed behind Wipsaw terminology.
- Each Codex or ChatGPT account owns a separate Codex home. API credentials and
  account tokens are referenced through a secret store, not saved in Wipsaw's
  ordinary database.
- Tabs and WIPs both support explicit model selection through layered profiles.
- WIP runs return Wipsaw-managed IDs that resolve to persisted logs and, when
  available, the originating Codex thread.

## Implemented now

- XDG config/state/data/runtime paths with private directory/database modes.
- Versioned SQLite registry for local hosts, workspaces, tabs, accounts, Codex
  homes, model profiles, and native Codex thread mappings.
- UUIDv7-backed typed IDs such as `ws_...`, `tab_...`, `acct_...`, and
  `thread_...`.
- Generated Wipsaw-only tmux config and private socket.
- Workspace create/list/start/attach and tab create/list/rename. Opening a
  stopped workspace recreates its windows from durable metadata and updates
  reused tmux targets transactionally.
- Dependency doctor for tmux, Codex, Docker, Compose, and SSH.
- Account/home registration with canonical paths and one-account-per-home
  enforcement.
- Zero-configuration first-run adoption of the current machine's Codex home
  and existing authentication.
- Model profiles and tab bindings for model, provider, reasoning effort,
  search, sandbox, and approval policy.
- Codex app-server home probing plus native thread create/name/inspect with the
  native ID, rollout path, resolved model, and Wipsaw ID persisted atomically
  with an optional tab binding.
- Exact-ID Codex TUI resume inside a mapped tmux tab, with account/home
  compatibility checks and shell restoration after exit.
- One persistent Lumbergh on the dashboard and one separate Middle Manager per
  workspace, all embedded in Wipsaw instead of attaching a raw Codex TUI.
- Resumable manager turns through `codex exec --json`, fixed to Terra/medium,
  with async progress, token usage, persistent transcript history, and exact
  native thread IDs.
- Private manager Codex homes and a two-tool Wipsaw MCP server. Manager turns
  ignore inherited user configuration and disable shell, personal MCPs,
  plugins, apps, unrelated skills, image generation, and multi-agent tools.
- A responsive, manager-first dashboard with separate Home, Sessions, Threads,
  and WIPs views, onboarding guidance, live dependency/account summaries, and
  create/rename prompts.
- The exact Wipsaw source logo for GitHub plus a faithful Unicode block-cell W
  and saw-tooth mark in the wide terminal navigation rail.
- Wipsaw-only `codex`, `manager`, `lumberg`, and `lumbergh` overrides that are
  loaded after the user's Bash/Zsh/Oh My Zsh configuration without editing it.
- `Ctrl-b w` navigator popup plus managed `Ctrl-b c`, `Ctrl-b ,`, and
  `Ctrl-b m` bindings in the private tmux server. Navigator popups are guarded
  per session, so repeating a popup binding toggles/closes instead of nesting.
- Human-readable and JSON output shared by the CLI and validated private
  manager MCP layer.

## License

Wipsaw is released under the [MIT License](LICENSE).
