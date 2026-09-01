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

## Install for development

```sh
cargo build --release
herdr plugin link /path/to/herdr-dock
```

Local links use the existing build. GitHub installs run the manifest's `cargo build --release` command.

## Configure repositories

Invoke the action once to create the plugin's `config.toml`, or find its directory with:

```sh
herdr plugin config-dir herdr-dock
```

```toml
branch_prefix = "agent"
# By default, worktrees live under HERDR_PLUGIN_STATE_DIR/workspaces.
# worktree_root = "~/worktrees"
# Plain Git is the default. Use Worktrunk for lifecycle hooks and setup.
# worktree_manager = "worktrunk"

[[repositories]]
name = "api"
path = "~/src/api"

[[repositories]]
name = "web"
path = "~/src/web"
```

The optional `name` becomes the worktree directory and tab label. Repository names must be unique.

`worktree_manager = "worktrunk"` requires [`wt`](https://worktrunk.dev/) on the plugin's `PATH`. Herdr Dock overrides Worktrunk's path for each command so worktrees stay under the shared dock root. Worktrunk hooks remain enabled and can ask for approval. Each dock saves its manager, so later configuration changes do not change how that dock is archived.

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
