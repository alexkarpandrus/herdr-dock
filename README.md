# herdr-dock

A [Herdr](https://herdr.dev) plugin for creating one coordinated workspace across multiple Git repositories.

## What it does

The `herdr-dock.create` action opens a terminal popup that:

1. accepts a project name and previews its `<prefix>/<snake_case_slug>` branch;
2. selects one or more configured repositories;
3. loads or saves named repository presets with `P` and `S`;
4. selects and remembers a base ref for each repository;
5. creates the repositories as sibling Git worktrees;
6. writes `AGENTS.md` and `CLAUDE.md` in their shared root; and
7. opens one Herdr tab per repository, plus a `shared` tab when multiple repositories are selected.

The `herdr-dock.overview` action shows saved dock history with live Herdr workspace and agent states, dirty repository counts, and latest commit subjects. Press Enter to focus an open dock. Press `A` to archive one: Herdr asks for confirmation, refuses dirty worktrees, closes the workspace, removes the worktrees, and retains branches plus the archived history record.

## Install

Requirements:

- macOS or Linux;
- [Herdr](https://herdr.dev) 0.7.0 or newer;
- Git; and
- Rust and Cargo to build the plugin.

### Install from GitHub

```sh
herdr plugin install alexkarpandrus/herdr-dock
herdr plugin list
```

Herdr downloads the repository, runs `cargo build --release`, and enables the plugin.

### Link a local checkout

Use this option when developing the plugin:

```sh
git clone https://github.com/alexkarpandrus/herdr-dock.git
cd herdr-dock
cargo build --release
herdr plugin link --enabled "$PWD"
```

The local link uses the existing binary. Run `cargo build --release` again after source changes.

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
# Press A in the repository picker to search Git repositories under these roots.
repository_search_roots = ["~/Src"]

[[repositories]]
name = "api"
path = "~/src/api"

[[repositories]]
name = "web"
path = "~/src/web"
```

The optional `name` becomes the worktree directory and tab label. Repository names must be unique.

`repository_search_roots` enables fuzzy repository search. Press `A` in the repository picker, type part of a repository name or path, and press Enter to add it. Herdr Dock saves selected search results and includes them in the repository quick list next time.

`worktree_manager = "worktrunk"` requires [`wt`](https://worktrunk.dev/) on the plugin's `PATH`. Herdr Dock overrides Worktrunk's path for each command so worktrees stay under the shared dock root. Worktrunk hooks remain enabled and can ask for approval. Each dock saves its manager, so later configuration changes do not change how that dock is archived.

Run the create action after saving the configuration:

```sh
herdr plugin action invoke create --plugin herdr-dock
```

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

Herdr plugin v1 requires users to declare plugin hotkeys in their Herdr configuration.

## Development

```sh
cargo test
HERDR_DOCK_TEST_WORKTRUNK=1 cargo test real_worktrunk_lifecycle_when_enabled -- --nocapture
cargo clippy --all-targets -- -D warnings
```

## License

MIT
