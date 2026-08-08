<p align="center">
  <img src="assets/brand/wipsaw-icon.png" alt="Wipsaw W and saw-tooth logo" width="220">
</p>

<h1 align="center">Wipsaw</h1>

Wipsaw is a Linux-first, tmux-style workspace for managing Codex terminals,
multiple Codex and ChatGPT accounts, remote hosts, and scheduled WIPs inherited
from Wiphand.

The project is currently in its first usable alpha slice. The Rust CLI and
Ratatui navigator can create and inspect private tmux workspaces/tabs, register
isolated account and Codex-home metadata, and create, inspect, and resume named
native Codex threads with Wipsaw-managed IDs. The WIP runtime transplant,
settings editor, secret-provider integration, and MCP server are not
implemented yet.

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

In the navigator, press `c` to create a workspace in the current directory,
`l` to move to its tabs, and `c` again to create a shell tab. Move to Codex
threads with `3`; `c` there creates a tab and starts its named Codex thread.
Press `Enter` to attach or open the selected item and `?` for the complete key
map. The narrow layout shows one table at a time; terminals 100 columns or
wider show workspaces, tabs, and Codex threads together.

On a new Wipsaw registry, the first command automatically adopts the active
`CODEX_HOME`, or `~/.codex` when that variable is unset, as the `current`
account and home. Wipsaw references that directory in place, so the user's
existing Codex authentication, sessions, settings, skills, and plugins remain
available without another login or copied credential. Set
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
- `manager`, `lumberg`, and `lumbergh` select the workspace's persistent
  Lumbergh thread, creating it on first use;
- `Ctrl-b w` opens the navigator as a tmux popup, `Ctrl-b c` opens its managed
  tab-creation prompt, `Ctrl-b ,` renames the current managed tab, and
  `Ctrl-b m` selects Lumbergh.

Common navigator controls are `h/j/k/l`, arrow keys, `1`/`2`/`3`, `g`/`G`,
`Enter`, `c` or `n` to create, `,` to rename a tab, `r` to refresh, and `q` to
close. Its prefix mode accepts `Ctrl-b` followed by `w`, `t`, `s`, `n`, `p`,
`c`, `?`, or `q`.

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
- Workspace create/list/attach and tab create/list/rename.
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
- A responsive keyboard-first navigator for workspaces, tabs, and managed
  Codex threads, including create and rename prompts.
- A Wipsaw logo asset for GitHub/Linux packaging and a compact saw-tooth mark
  in the portable terminal header.
- Wipsaw-only `codex`, `manager`, `lumberg`, and `lumbergh` overrides that are
  loaded after the user's Bash/Zsh/Oh My Zsh configuration without editing it.
- `Ctrl-b w` navigator popup plus managed `Ctrl-b c`, `Ctrl-b ,`, and
  `Ctrl-b m` bindings in the private tmux server. Navigator popups are guarded
  per session, so repeating a popup binding toggles/closes instead of nesting.
- Human-readable and JSON output suitable for the future MCP/manager layer.

## License

Wipsaw is released under the [MIT License](LICENSE).
