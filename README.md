<div align="center">

<img src="docs/logo.svg" alt="herdr-dock logo" width="140" />

# herdr-dock

**One persistent root session for work across repositories, with child sessions whenever needed.**

`herdr-dock` creates sibling Git worktrees on one shared branch, gives the root session the project goal and repository map, prepares one tab per repository, and remembers the complete workspace for later.

</div>

---

<p align="center">
  <a href="https://github.com/alexkarpandrus/herdr-dock/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/alexkarpandrus/herdr-dock/ci.yml?branch=main&label=CI&style=flat-square" alt="CI status" /></a>
  <a href="https://crates.io/"><img src="https://img.shields.io/badge/rust-1.89%2B-orange?style=flat-square&logo=rust" alt="Rust 1.89+" /></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue?style=flat-square" alt="License MIT" /></a>
  <img src="https://img.shields.io/badge/platform-macOS%20%7C%20Linux-1f2937?style=flat-square" alt="macOS & Linux" />
  <img src="https://img.shields.io/badge/Herdr-%E2%89%A50.8.2-7c3aed?style=flat-square" alt="Herdr ≥0.8.2" />
  <img src="https://img.shields.io/badge/plugin-action-create%20%7C%20overview%20%7C%20setup-0ea5e9?style=flat-square" alt="Plugin actions" />
</p>

---

## Demo

Name the project, state the goal, choose or reuse a branch, pick repositories, review, and create. The overview shows live status, dirty repositories, and commit subjects; it can also add repositories as the scope changes.

<p align="center">
  <img src="docs/herdr-dock-demo.gif" alt="herdr-dock create flow, workspace, and overview demo" width="900" />
</p>

1. **Name the project and goal** — the goal is written into the shared agent guides.
2. **Choose the branch** — keep the generated name or enter an existing local branch to resume work.
3. **Pick repositories** — a saved quick list stays on top; type to search configured roots.
4. **Choose base refs** — only repositories without the branch need a base; `Tab` applies one ref to every compatible repository.
5. **Review and create** — existing branches are marked as reused before any worktree is created.
6. **Workspace and overview** — work from the root session, start child sessions when useful, add repositories with `E`, and resume the dock later.

---

## What it does

The `herdr-dock.create` action opens a terminal popup that:

1. accepts a project name and an optional goal;
2. accepts the generated branch name or any existing local branch;
3. selects one or more repositories and loads or saves presets with `P` and `S`;
4. selects and remembers base refs only where the target branch does not exist;
5. reviews which repositories will create or reuse the branch;
6. writes the goal and repository map to shared `AGENTS.md` and `CLAUDE.md` files; and
7. opens a `root` tab plus one tab per repository; the root can start and communicate with child agents in any repository.

The `herdr-dock.overview` action opens a kanban board with Working, Closed, Done, and optional Archived columns. `N` creates a dock and `G` edits the selected dock goal in its saved state and shared agent guides. Cards show root-session status, child-session counts, and repository health. Repository details report missing worktrees and whether each dock branch has no unique commits, is ahead, is behind, or has diverged from its saved base ref. `S` starts a same-kind child in a chosen tab, `F` focuses a child, and `X` stops a child split without closing its tab. Manual Herdr spawning remains supported. Press Enter to focus or reopen a dock, `E` to add repositories, `C` to park its workspace, `D` to toggle done and active status, `A` to archive and remove clean worktrees, or `H` to show and hide archived docks. Parking preserves lifecycle status, worktrees, and resumable agent sessions. Reopening a done dock keeps it done until you press `D`. The detail pane expands the root and child session tree plus repository status.

The `herdr-dock.setup` action writes the recommended keybindings into your Herdr configuration.

---

## Why herd them into a dock?

Running one agent per service gets messy fast: each repo solves its half of a feature, the branches drift, and nobody has the full picture. A dock rounds the whole change up into one pen:

- **One shared branch** across every repository, so the work stays in lockstep.
- **Sibling worktrees** — lambs on their own lead, so your main checkouts stay clean and yours to use.
- **Shared context** — `AGENTS.md` and `CLAUDE.md` state the project goal and repository map for every agent.
- **Root and child sessions** — the persistent root can work directly or use Herdr to start and communicate with child agents in any repository, without a required delegation structure.
- **Resumable sessions** — close the dock and Herdr keeps the session IDs, so reopening gets back to work, not to square one.

---

## Persistence and lifecycle

Herdr Dock uses one JSON file at `$HERDR_PLUGIN_STATE_DIR/state.json`. It does not use a database. It writes through a temporary file and renames it atomically. It writes only when dock lifecycle data or resumable agent metadata changes.

A per-state-file lock permits only one Herdr Dock management action at a time. If another create or overview action is already open for the same state directory, the second action exits with a retry message. Running docks and agent sessions are not locked. Temporary state files include the writer process ID, so concurrent or interrupted writers do not share a temporary path.

Each dock record stores its goal, Herdr session, workspace ID, tabs, repositories, lifecycle timestamps, and the last observed agents. Each agent record stores its tab, name, kind, working directory, and resumable session ID or path:

```json
{
  "herdr_session": "default",
  "workspace_id": "w1",
  "goal": "Ship OAuth login across API and web",
  "completed_at_unix": 1740000000,
  "tabs": [{"label": "root", "cwd": "/work/dock"}, {"label": "api", "cwd": "/work/dock/api"}],
  "agents": [{
    "name": "root",
    "kind": "codex",
    "cwd": "/work/dock",
    "tab": 0,
    "session": {
      "source": "herdr:codex",
      "agent": "codex",
      "kind": "id",
      "value": "session-id"
    }
  }]
}
```

The overview refreshes this metadata from Herdr and displays the root session above its children. Enter focuses a live workspace or recreates a closed workspace, resumes the root first, and then resumes supported child sessions. Missing or unsupported sessions are reported without blocking the remaining sessions. `E` adds repositories on the dock branch and adds tabs to a live workspace.

`C` parks the workspace, tabs, and processes while preserving worktrees, lifecycle status, and resumable agent sessions. `D` toggles done and active status without changing the workspace. Reopening preserves that status.

`A` is the destructive archive action. It refuses dirty worktrees and reports the path plus an inspection command. It removes verified worktrees but keeps Git branches and the archived history record. Archived docks are hidden until you press `H`.

---

## Install

### Quick start

```sh
# 1. Install
herdr plugin install alexkarpandrus/herdr-dock

# 2. Create the first dock
herdr plugin action invoke create --plugin herdr-dock
```

The first create flow can install the recommended hotkeys, asks for a repository search root when needed, then creates the dock. You can still edit `config.toml` to add explicit `[[repositories]]` blocks or more search roots.

Requirements:

- macOS or Linux;
- [Herdr](https://herdr.dev) 0.8.2 or newer;
- Git; and
- Rust 1.89 and Cargo only to build from source; otherwise a prebuilt binary is fetched from GitHub Releases (macOS arm64/x86_64, Linux x86_64).

### Link a local checkout

Use this option when developing the plugin:

```sh
git clone https://github.com/alexkarpandrus/herdr-dock.git
cd herdr-dock
cargo build --release
herdr plugin link "$PWD" --enabled
```

The local link uses the existing binary. Run `cargo build --release` again after source changes.

---

## Configure repositories

Open the plugin configuration file:

```sh
${EDITOR:-vi} "$(herdr plugin config-dir herdr-dock)/config.toml"
```

```toml
branch_prefix = "agent"
# By default, worktrees live under HERDR_PLUGIN_STATE_DIR/workspaces.
# worktree_root = "~/worktrees"
# Plain Git is the default. Use Worktrunk for lifecycle hooks and setup.
# worktree_manager = "worktrunk"
# Type in the repository picker to search Git repositories under these roots.
repository_search_roots = ["~/Src"]

[[repositories]]
name = "api"
path = "~/src/api"

[[repositories]]
name = "web"
path = "~/src/web"
```

The optional `name` becomes the worktree directory and tab label. Repository names must be unique.

`repository_search_roots` enables fuzzy repository search directly in the repository picker. The saved quick list stays at the top. Start typing to filter it and show matching Git repositories from the configured roots below. Press Space to select a search result, then Enter to continue. Herdr Dock saves it in the quick list for next time.

The base-ref picker also filters branches and remote refs as you type.

`worktree_manager = "worktrunk"` requires [`wt`](https://worktrunk.dev/) on the plugin's `PATH`. Herdr Dock overrides Worktrunk's path for each command so worktrees stay under the shared dock root. Worktrunk hooks remain enabled and can ask for approval. Each dock saves its manager, so later configuration changes do not change how that dock is archived.

Run the create action after saving the configuration:

```sh
herdr plugin action invoke create --plugin herdr-dock
```

---

## Bind a hotkey

Add a plugin action binding to your Herdr user configuration:

```toml
[[keys.command]]
key = "prefix+d"
type = "plugin_action"
command = "herdr-dock.create"
description = "create dock"

[[keys.command]]
key = "prefix+o"
type = "plugin_action"
command = "herdr-dock.overview"
description = "show dock overview"
```

With Herdr's default `ctrl+b` prefix, press `ctrl+b`, then `d` to create a dock or `ctrl+b`, then `o` to open the overview. Herdr plugin v1 cannot register keybindings from a plugin manifest, so users must add these bindings to their Herdr configuration.

You can also add them automatically with:

```sh
herdr plugin action invoke setup --plugin herdr-dock
```

---

## Development

```sh
cargo test
HERDR_DOCK_TEST_WORKTRUNK=1 cargo test real_worktrunk_lifecycle_when_enabled -- --nocapture
cargo clippy --all-targets -- -D warnings
```

### Releases

Use scoped commit messages such as `dock: add root coordinator tabs`. Add exactly one `release:patch`, `release:minor`, or `release:major` label to a pull request that should publish a version. When the pull request merges, the version workflow updates `Cargo.toml`, `Cargo.lock`, and `herdr-plugin.toml` in a `release: vX.Y.Z` commit, creates the tag and GitHub Release with generated notes, and uploads the release binaries. Pull requests without a release label do not publish.

---

## License

MIT
