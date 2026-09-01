use crossterm::{
    cursor::{Hide, MoveTo, Show},
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute, queue,
    style::{Attribute, Print, SetAttribute},
    terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    error::Error,
    fs,
    io::{self, IsTerminal, Stdout, Write},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

#[derive(Debug, Deserialize)]
#[serde(default)]
struct Config {
    branch_prefix: String,
    worktree_root: Option<PathBuf>,
    repositories: Vec<RepositoryConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            branch_prefix: "agent".into(),
            worktree_root: None,
            repositories: Vec::new(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RepositoryConfig {
    Path(PathBuf),
    Named { name: Option<String>, path: PathBuf },
}

#[derive(Clone, Debug)]
struct Repository {
    name: String,
    path: PathBuf,
}

#[derive(Clone, Debug)]
struct RepositoryPlan {
    repository: Repository,
    base_ref: String,
}

#[derive(Default, Deserialize, Serialize)]
struct State {
    #[serde(default)]
    base_refs: BTreeMap<String, String>,
    #[serde(default)]
    docks: Vec<DockRecord>,
    #[serde(default)]
    presets: Vec<Preset>,
}

#[derive(Clone, Deserialize, Serialize)]
struct Preset {
    name: String,
    repositories: Vec<String>,
}

#[derive(Deserialize, Serialize)]
struct DockRecord {
    name: String,
    slug: String,
    branch: String,
    root: PathBuf,
    workspace_id: String,
    created_at_unix: u64,
    repositories: Vec<DockRepository>,
}

#[derive(Deserialize, Serialize)]
struct DockRepository {
    name: String,
    source: PathBuf,
    worktree: PathBuf,
    base_ref: String,
}

#[derive(Clone)]
struct LiveWorkspace {
    status: String,
    agents: Vec<AgentOverview>,
}

#[derive(Clone)]
struct AgentOverview {
    name: String,
    status: String,
    cwd: String,
}

struct DockOverview {
    name: String,
    branch: String,
    root: PathBuf,
    workspace_id: String,
    status: String,
    open: bool,
    agents: Vec<AgentOverview>,
    repositories: Vec<RepositoryOverview>,
}

struct RepositoryOverview {
    name: String,
    status: String,
    commit: String,
}

struct Ui {
    stdout: Stdout,
}

impl Ui {
    fn start() -> Result<Self> {
        terminal::enable_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(error) = execute!(stdout, EnterAlternateScreen, Hide) {
            let _ = terminal::disable_raw_mode();
            return Err(error.into());
        }
        Ok(Self { stdout })
    }

    fn frame(&mut self, title: &str, lines: &[String]) -> Result<()> {
        queue!(
            self.stdout,
            MoveTo(0, 0),
            Clear(ClearType::All),
            SetAttribute(Attribute::Bold),
            Print(title),
            SetAttribute(Attribute::Reset),
            Print("\r\n\r\n")
        )?;
        for line in lines {
            queue!(self.stdout, Print(line), Print("\r\n"))?;
        }
        self.stdout.flush()?;
        Ok(())
    }
}

impl Drop for Ui {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
        let _ = execute!(self.stdout, Show, LeaveAlternateScreen);
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("herdr-dock: {error}");
        if io::stdin().is_terminal() {
            eprintln!("\nPress Enter to close.");
            let _ = io::stdin().read_line(&mut String::new());
        }
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    match env::args().nth(1).as_deref() {
        Some("open") => open_popup(env::args().nth(2).as_deref().unwrap_or("create")),
        Some("create") => create_dock(),
        Some("overview") => show_overview(),
        _ => Err(message("expected `open`, `create`, or `overview`")),
    }
}

fn open_popup(entrypoint: &str) -> Result<()> {
    let herdr = env::var_os("HERDR_BIN_PATH").unwrap_or_else(|| "herdr".into());
    let status = Command::new(herdr)
        .args([
            "plugin",
            "pane",
            "open",
            "--plugin",
            "herdr-dock",
            "--entrypoint",
            entrypoint,
        ])
        .stdin(Stdio::null())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(message(format!("could not open popup: {status}")))
    }
}

fn show_overview() -> Result<()> {
    let state_dir = required_directory("HERDR_PLUGIN_STATE_DIR")?;
    let state = load_state(&state_dir.join("state.json"))?;
    if state.docks.is_empty() {
        return Err(message("no docks have been created yet"));
    }

    let mut docks = collect_overview(&state.docks)?;
    let mut cursor: usize = 0;
    let mut ui = Ui::start()?;
    loop {
        let height = terminal::size()?.1 as usize;
        let visible = (height / 3).max(1).min(docks.len());
        let start = cursor.saturating_sub(visible.saturating_sub(1));
        let mut lines = docks[start..start + visible]
            .iter()
            .enumerate()
            .map(|(offset, dock)| {
                let dirty = dock
                    .repositories
                    .iter()
                    .filter(|repository| repository.status == "dirty")
                    .count();
                format!(
                    "{} {} [{}] · {} repos · {} dirty · {} agents",
                    if start + offset == cursor { ">" } else { " " },
                    dock.name,
                    dock.status,
                    dock.repositories.len(),
                    dirty,
                    dock.agents.len()
                )
            })
            .collect::<Vec<_>>();
        let dock = &docks[cursor];
        lines.extend([
            String::new(),
            format!("Branch: {}", dock.branch),
            format!("Root: {}", dock.root.display()),
            String::new(),
            "Agents".into(),
        ]);
        if dock.agents.is_empty() {
            lines.push("  none".into());
        } else {
            lines.extend(
                dock.agents
                    .iter()
                    .map(|agent| format!("  {} [{}] · {}", agent.name, agent.status, agent.cwd)),
            );
        }
        lines.push(String::new());
        lines.push("Repositories".into());
        lines.extend(dock.repositories.iter().map(|repository| {
            format!(
                "  {} [{}] · {}",
                repository.name, repository.status, repository.commit
            )
        }));
        lines.truncate(height.saturating_sub(4));
        lines.extend([
            String::new(),
            "↑/↓ select · Enter focus open workspace · R refresh · Esc close".into(),
        ]);
        ui.frame("Dock overview", &lines)?;

        match read_key()?.code {
            KeyCode::Esc => return Ok(()),
            KeyCode::Up => cursor = cursor.saturating_sub(1),
            KeyCode::Down => cursor = (cursor + 1).min(docks.len() - 1),
            KeyCode::Char('r') | KeyCode::Char('R') => {
                docks = collect_overview(&state.docks)?;
            }
            KeyCode::Enter if dock.open => {
                herdr(&["workspace", "focus", &dock.workspace_id])?;
                return Ok(());
            }
            _ => {}
        }
    }
}

fn collect_overview(records: &[DockRecord]) -> Result<Vec<DockOverview>> {
    Ok(build_overview(records, &live_workspaces()?))
}

fn build_overview(
    records: &[DockRecord],
    live: &BTreeMap<String, LiveWorkspace>,
) -> Vec<DockOverview> {
    records
        .iter()
        .rev()
        .map(|record| {
            let workspace = live.get(&record.workspace_id);
            let status = if !record.root.exists() {
                "missing".into()
            } else {
                workspace
                    .map(|workspace| workspace.status.clone())
                    .unwrap_or_else(|| "closed".into())
            };
            let repositories = record
                .repositories
                .iter()
                .map(|repository| {
                    let status = if !repository.worktree.exists() {
                        "missing".into()
                    } else {
                        match optional_git(&repository.worktree, ["status", "--porcelain"]) {
                            Some(output) if output.is_empty() => "clean".into(),
                            Some(_) => "dirty".into(),
                            None => "unavailable".into(),
                        }
                    };
                    let commit =
                        optional_git(&repository.worktree, ["log", "-1", "--pretty=%h %s"])
                            .unwrap_or_else(|| "no commit".into());
                    RepositoryOverview {
                        name: repository.name.clone(),
                        status,
                        commit,
                    }
                })
                .collect();
            DockOverview {
                name: record.name.clone(),
                branch: record.branch.clone(),
                root: record.root.clone(),
                workspace_id: record.workspace_id.clone(),
                status,
                open: workspace.is_some(),
                agents: workspace
                    .map(|workspace| workspace.agents.clone())
                    .unwrap_or_default(),
                repositories,
            }
        })
        .collect()
}

fn live_workspaces() -> Result<BTreeMap<String, LiveWorkspace>> {
    let workspace_response = herdr_json(&["workspace", "list"])?;
    let mut workspaces = BTreeMap::new();
    if let Some(items) = workspace_response
        .pointer("/result/workspaces")
        .and_then(Value::as_array)
    {
        for workspace in items {
            if let Some(workspace_id) = workspace.get("workspace_id").and_then(Value::as_str) {
                workspaces.insert(
                    workspace_id.into(),
                    LiveWorkspace {
                        status: workspace
                            .get("agent_status")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown")
                            .into(),
                        agents: Vec::new(),
                    },
                );
            }
        }
    }

    let agent_response = herdr_json(&["agent", "list"])?;
    if let Some(items) = agent_response
        .pointer("/result/agents")
        .and_then(Value::as_array)
    {
        for agent in items {
            let Some(workspace_id) = agent.get("workspace_id").and_then(Value::as_str) else {
                continue;
            };
            let Some(workspace) = workspaces.get_mut(workspace_id) else {
                continue;
            };
            workspace.agents.push(AgentOverview {
                name: agent
                    .get("agent")
                    .and_then(Value::as_str)
                    .or_else(|| agent.get("pane_id").and_then(Value::as_str))
                    .unwrap_or("agent")
                    .into(),
                status: agent
                    .get("agent_status")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .into(),
                cwd: agent
                    .get("foreground_cwd")
                    .and_then(Value::as_str)
                    .or_else(|| agent.get("cwd").and_then(Value::as_str))
                    .unwrap_or("")
                    .into(),
            });
        }
    }
    Ok(workspaces)
}

fn create_dock() -> Result<()> {
    let config_dir = required_directory("HERDR_PLUGIN_CONFIG_DIR")?;
    let state_dir = required_directory("HERDR_PLUGIN_STATE_DIR")?;
    let config_path = config_dir.join("config.toml");
    let state_path = state_dir.join("state.json");
    let config = load_config(&config_path)?;
    let repositories = load_repositories(&config.repositories)?;
    if repositories.is_empty() {
        return Err(message(format!(
            "no repositories configured; add [[repositories]] entries to {}",
            config_path.display()
        )));
    }
    let mut state = load_state(&state_path)?;
    let worktree_root = config
        .worktree_root
        .as_deref()
        .map(expand_home)
        .transpose()?
        .unwrap_or_else(|| state_dir.join("workspaces"));
    if !worktree_root.is_absolute() {
        return Err(message("worktree_root must be absolute or start with `~/`"));
    }

    let selection = {
        let mut ui = Ui::start()?;
        let Some(name) = prompt_name(&mut ui, &config.branch_prefix)? else {
            return Ok(());
        };
        let Some((selected, preset)) = prompt_repositories(&mut ui, &repositories, &state.presets)?
        else {
            return Ok(());
        };
        let mut plans = Vec::with_capacity(selected.len());
        for repository in selected {
            let refs = git_refs(&repository.path)?;
            let remembered = state.base_refs.get(&repository_key(&repository.path));
            let initial = remembered
                .filter(|value| refs.contains(value))
                .cloned()
                .or_else(|| default_base_ref(&repository.path, &refs))
                .unwrap_or_else(|| "HEAD".into());
            let Some(base_ref) = prompt_base_ref(&mut ui, &repository.name, &refs, &initial)?
            else {
                return Ok(());
            };
            plans.push(RepositoryPlan {
                repository,
                base_ref,
            });
        }
        (name, plans, preset)
    };

    let (name, plans, preset) = selection;
    let slug = slugify(&name);
    let branch = format!("{}/{}", config.branch_prefix, slug);
    check_branch_name(&branch)?;
    let root = worktree_root.join(&slug);

    println!("Creating {branch} in {}...", root.display());
    let worktrees = materialize_worktrees(&root, &name, &branch, &plans)?;
    let workspace_id = open_workspace(&name, &root, &plans, &worktrees)?;

    for plan in &plans {
        state
            .base_refs
            .insert(repository_key(&plan.repository.path), plan.base_ref.clone());
    }
    if let Some(preset) = preset {
        upsert_preset(&mut state.presets, preset);
    }
    state.docks.push(DockRecord {
        name: name.clone(),
        slug,
        branch,
        root: root.clone(),
        workspace_id,
        created_at_unix: SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
        repositories: plans
            .iter()
            .zip(&worktrees)
            .map(|(plan, worktree)| DockRepository {
                name: plan.repository.name.clone(),
                source: plan.repository.path.clone(),
                worktree: worktree.clone(),
                base_ref: plan.base_ref.clone(),
            })
            .collect(),
    });
    save_state(&state_path, &state)?;
    println!("Created dock {name}.");
    Ok(())
}

fn prompt_name(ui: &mut Ui, prefix: &str) -> Result<Option<String>> {
    let mut name = String::new();
    loop {
        let slug = slugify(&name);
        ui.frame(
            "Create dock · project name",
            &[
                format!("> {name}"),
                String::new(),
                format!(
                    "Branch: {prefix}/{}",
                    if slug.is_empty() { "…" } else { &slug }
                ),
                String::new(),
                "Enter continue · Esc cancel · Backspace delete".into(),
            ],
        )?;
        match read_key()?.code {
            KeyCode::Esc => return Ok(None),
            KeyCode::Enter if !slug.is_empty() => return Ok(Some(name.trim().into())),
            KeyCode::Backspace => {
                name.pop();
            }
            KeyCode::Char(character) => name.push(character),
            _ => {}
        }
    }
}

fn prompt_repositories(
    ui: &mut Ui,
    repositories: &[Repository],
    presets: &[Preset],
) -> Result<Option<(Vec<Repository>, Option<Preset>)>> {
    let mut cursor: usize = 0;
    let mut selected = vec![false; repositories.len()];
    let mut preset_name = None;
    loop {
        let height = terminal::size()?.1.saturating_sub(8) as usize;
        let visible = height.max(1).min(repositories.len());
        let start = cursor.saturating_sub(visible.saturating_sub(1));
        let mut lines = repositories[start..start + visible]
            .iter()
            .enumerate()
            .map(|(offset, repository)| {
                let index = start + offset;
                format!(
                    "{} [{}] {}  {}",
                    if index == cursor { ">" } else { " " },
                    if selected[index] { "x" } else { " " },
                    repository.name,
                    repository.path.display()
                )
            })
            .collect::<Vec<_>>();
        if let Some(name) = &preset_name {
            lines.push(format!("Save as preset: {name}"));
        }
        lines.push(String::new());
        lines.push(if presets.is_empty() {
            "↑/↓ move · Space select · S save preset · Enter continue · Esc cancel".into()
        } else {
            "↑/↓ move · Space select · P load preset · S save preset · Enter continue · Esc cancel"
                .into()
        });
        ui.frame("Create dock · repositories", &lines)?;
        match read_key()?.code {
            KeyCode::Esc => return Ok(None),
            KeyCode::Up => cursor = cursor.saturating_sub(1),
            KeyCode::Down => cursor = (cursor + 1).min(repositories.len() - 1),
            KeyCode::Char(' ') => selected[cursor] = !selected[cursor],
            KeyCode::Char('p') | KeyCode::Char('P') if !presets.is_empty() => {
                if let Some(preset) = prompt_preset(ui, presets)? {
                    selected = selection_for_preset(repositories, &preset);
                    preset_name = None;
                }
            }
            KeyCode::Char('s') | KeyCode::Char('S') if selected.iter().any(|value| *value) => {
                preset_name = prompt_text(ui, "Create dock · preset name")?;
            }
            KeyCode::Enter if selected.iter().any(|value| *value) => {
                let chosen = repositories
                    .iter()
                    .zip(&selected)
                    .filter(|(_, selected)| **selected)
                    .map(|(repository, _)| repository.clone())
                    .collect::<Vec<_>>();
                let preset = preset_name.map(|name| Preset {
                    name,
                    repositories: chosen
                        .iter()
                        .map(|repository| repository_key(&repository.path))
                        .collect(),
                });
                return Ok(Some((chosen, preset)));
            }
            _ => {}
        }
    }
}

fn prompt_preset(ui: &mut Ui, presets: &[Preset]) -> Result<Option<Preset>> {
    let mut cursor: usize = 0;
    loop {
        let height = terminal::size()?.1.saturating_sub(7) as usize;
        let visible = height.max(1).min(presets.len());
        let start = cursor.saturating_sub(visible.saturating_sub(1));
        let mut lines = presets[start..start + visible]
            .iter()
            .enumerate()
            .map(|(offset, preset)| {
                format!(
                    "{} {} ({} repositories)",
                    if start + offset == cursor { ">" } else { " " },
                    preset.name,
                    preset.repositories.len()
                )
            })
            .collect::<Vec<_>>();
        lines.extend([String::new(), "↑/↓ move · Enter load · Esc back".into()]);
        ui.frame("Create dock · load preset", &lines)?;
        match read_key()?.code {
            KeyCode::Esc => return Ok(None),
            KeyCode::Up => cursor = cursor.saturating_sub(1),
            KeyCode::Down => cursor = (cursor + 1).min(presets.len() - 1),
            KeyCode::Enter => return Ok(Some(presets[cursor].clone())),
            _ => {}
        }
    }
}

fn prompt_text(ui: &mut Ui, title: &str) -> Result<Option<String>> {
    let mut value = String::new();
    loop {
        ui.frame(
            title,
            &[
                format!("> {value}"),
                String::new(),
                "Enter save · Esc back · Backspace delete".into(),
            ],
        )?;
        match read_key()?.code {
            KeyCode::Esc => return Ok(None),
            KeyCode::Enter if !value.trim().is_empty() => {
                return Ok(Some(value.trim().into()));
            }
            KeyCode::Backspace => {
                value.pop();
            }
            KeyCode::Char(character) => value.push(character),
            _ => {}
        }
    }
}

fn selection_for_preset(repositories: &[Repository], preset: &Preset) -> Vec<bool> {
    let paths = preset.repositories.iter().cloned().collect::<BTreeSet<_>>();
    repositories
        .iter()
        .map(|repository| paths.contains(&repository_key(&repository.path)))
        .collect()
}

fn upsert_preset(presets: &mut Vec<Preset>, preset: Preset) {
    if let Some(existing) = presets
        .iter_mut()
        .find(|existing| existing.name.eq_ignore_ascii_case(&preset.name))
    {
        *existing = preset;
    } else {
        presets.push(preset);
    }
}

fn prompt_base_ref(
    ui: &mut Ui,
    repository: &str,
    refs: &[String],
    initial: &str,
) -> Result<Option<String>> {
    let mut cursor = refs.iter().position(|value| value == initial).unwrap_or(0);
    loop {
        let height = terminal::size()?.1.saturating_sub(7) as usize;
        let visible = height.max(1).min(refs.len());
        let start = cursor.saturating_sub(visible.saturating_sub(1));
        let mut lines = refs[start..start + visible]
            .iter()
            .enumerate()
            .map(|(offset, reference)| {
                format!(
                    "{} {}",
                    if start + offset == cursor { ">" } else { " " },
                    reference
                )
            })
            .collect::<Vec<_>>();
        lines.extend([String::new(), "↑/↓ move · Enter select · Esc cancel".into()]);
        ui.frame(&format!("Create dock · base ref for {repository}"), &lines)?;
        match read_key()?.code {
            KeyCode::Esc => return Ok(None),
            KeyCode::Up => cursor = cursor.saturating_sub(1),
            KeyCode::Down => cursor = (cursor + 1).min(refs.len() - 1),
            KeyCode::Enter => return Ok(Some(refs[cursor].clone())),
            _ => {}
        }
    }
}

fn read_key() -> Result<KeyEvent> {
    loop {
        if let Event::Key(key) = event::read()?
            && matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
        {
            if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                return Ok(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
            }
            return Ok(key);
        }
    }
}

fn required_directory(variable: &str) -> Result<PathBuf> {
    let path = env::var_os(variable).ok_or_else(|| message(format!("{variable} is not set")))?;
    let path = PathBuf::from(path);
    fs::create_dir_all(&path)?;
    Ok(path)
}

fn load_config(path: &Path) -> Result<Config> {
    if !path.exists() {
        fs::write(
            path,
            r#"branch_prefix = "agent"
# worktree_root = "~/worktrees"

# Add one block per repository:
# [[repositories]]
# name = "api"
# path = "~/src/api"
"#,
        )?;
        return Err(message(format!(
            "created {}; add repositories and run the action again",
            path.display()
        )));
    }
    let config: Config = toml::from_str(&fs::read_to_string(path)?)?;
    let prefix = config.branch_prefix.trim_matches('/');
    if prefix.is_empty() || prefix != config.branch_prefix {
        return Err(message("branch_prefix must be a non-empty Git ref segment"));
    }
    Ok(config)
}

fn load_repositories(configured: &[RepositoryConfig]) -> Result<Vec<Repository>> {
    let mut repositories = Vec::new();
    let mut paths = BTreeSet::new();
    let mut names = BTreeSet::new();
    for configured in configured {
        let (configured_name, configured_path) = match configured {
            RepositoryConfig::Path(path) => (None, path),
            RepositoryConfig::Named { name, path } => (name.as_deref(), path),
        };
        let path = expand_home(configured_path)?.canonicalize()?;
        let root = git(&path, ["rev-parse", "--show-toplevel"])?;
        let root = PathBuf::from(root.trim()).canonicalize()?;
        if !paths.insert(root.clone()) {
            continue;
        }
        let name = configured_name
            .map(str::to_owned)
            .or_else(|| {
                root.file_name()
                    .map(|value| value.to_string_lossy().into_owned())
            })
            .ok_or_else(|| message(format!("cannot name repository {}", root.display())))?;
        if Path::new(&name)
            .file_name()
            .and_then(|value| value.to_str())
            != Some(&name)
            || matches!(name.as_str(), "." | "..")
        {
            return Err(message(format!("invalid repository name `{name}`")));
        }
        if !names.insert(name.clone()) {
            return Err(message(format!("repository name `{name}` is not unique")));
        }
        repositories.push(Repository { name, path: root });
    }
    Ok(repositories)
}

fn load_state(path: &Path) -> Result<State> {
    if path.exists() {
        Ok(serde_json::from_slice(&fs::read(path)?)?)
    } else {
        Ok(State::default())
    }
}

fn save_state(path: &Path, state: &State) -> Result<()> {
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(state)?)?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn expand_home(path: &Path) -> Result<PathBuf> {
    let value = path.to_string_lossy();
    if value == "~" || value.starts_with("~/") {
        let home = env::var_os("HOME").ok_or_else(|| message("HOME is not set"))?;
        Ok(PathBuf::from(home).join(value.trim_start_matches("~/")))
    } else {
        Ok(path.to_owned())
    }
}

fn slugify(value: &str) -> String {
    let mut slug = String::new();
    for character in value.trim().chars().flat_map(char::to_lowercase) {
        if character.is_alphanumeric() {
            slug.push(character);
        } else if !slug.is_empty() && !slug.ends_with('_') {
            slug.push('_');
        }
    }
    slug.trim_end_matches('_').into()
}

fn check_branch_name(branch: &str) -> Result<()> {
    checked(
        Command::new("git")
            .args(["check-ref-format", "--branch", branch])
            .stdout(Stdio::null()),
    )?;
    Ok(())
}

fn git_refs(repository: &Path) -> Result<Vec<String>> {
    let output = git(
        repository,
        [
            "for-each-ref",
            "--format=%(refname:short)",
            "refs/heads",
            "refs/remotes",
        ],
    )?;
    let mut refs = output
        .lines()
        .filter(|reference| !reference.ends_with("/HEAD"))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    refs.push("HEAD".into());
    refs.sort();
    refs.dedup();
    Ok(refs)
}

fn default_base_ref(repository: &Path, refs: &[String]) -> Option<String> {
    let origin_head = optional_git(
        repository,
        [
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ],
    );
    origin_head
        .filter(|value| refs.contains(value))
        .or_else(|| {
            optional_git(repository, ["branch", "--show-current"])
                .filter(|value| !value.is_empty() && refs.contains(value))
        })
        .or_else(|| Some("HEAD".into()))
}

fn materialize_worktrees(
    root: &Path,
    name: &str,
    branch: &str,
    plans: &[RepositoryPlan],
) -> Result<Vec<PathBuf>> {
    if root.exists() {
        return Err(message(format!(
            "dock directory already exists: {}",
            root.display()
        )));
    }
    if let Some(parent) = root.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::create_dir(root)?;
    let mut created: Vec<(PathBuf, PathBuf)> = Vec::new();
    let result = (|| {
        for plan in plans {
            let destination = root.join(&plan.repository.name);
            let exists = Command::new("git")
                .arg("-C")
                .arg(&plan.repository.path)
                .args(["show-ref", "--verify", "--quiet"])
                .arg(format!("refs/heads/{branch}"))
                .status()?
                .success();
            let mut command = Command::new("git");
            command
                .arg("-C")
                .arg(&plan.repository.path)
                .args(["worktree", "add"]);
            if exists {
                command.arg(&destination).arg(branch);
            } else {
                command
                    .args(["-b", branch])
                    .arg(&destination)
                    .arg(&plan.base_ref);
            }
            checked(&mut command)?;
            created.push((plan.repository.path.clone(), destination));
        }
        write_agent_guides(root, name, branch, plans)?;
        Ok(())
    })();
    if let Err(error) = result {
        for (repository, worktree) in created.iter().rev() {
            let _ = Command::new("git")
                .arg("-C")
                .arg(repository)
                .args(["worktree", "remove", "--force"])
                .arg(worktree)
                .status();
        }
        let _ = fs::remove_dir_all(root);
        return Err(error);
    }
    Ok(created.into_iter().map(|(_, path)| path).collect())
}

fn write_agent_guides(
    root: &Path,
    name: &str,
    branch: &str,
    plans: &[RepositoryPlan],
) -> Result<()> {
    let repositories = plans
        .iter()
        .map(|plan| {
            format!(
                "- `{}`: `{}` (created from `{}`)",
                plan.repository.name,
                root.join(&plan.repository.name).display(),
                plan.base_ref
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let guide = format!(
        "# {name}\n\nThis directory is a Herdr dock that groups related Git worktrees.\nAll repositories use branch `{branch}`. Work inside the repository directories, not this root.\n\n## Repositories\n\n{repositories}\n"
    );
    fs::write(root.join("AGENTS.md"), &guide)?;
    fs::write(root.join("CLAUDE.md"), guide)?;
    Ok(())
}

fn open_workspace(
    name: &str,
    root: &Path,
    plans: &[RepositoryPlan],
    worktrees: &[PathBuf],
) -> Result<String> {
    let first_cwd = if plans.len() == 1 {
        &worktrees[0]
    } else {
        root
    };
    let response = herdr_json(&[
        "workspace",
        "create",
        "--cwd",
        &first_cwd.to_string_lossy(),
        "--label",
        name,
        "--no-focus",
    ])?;
    let workspace_id = json_string(&response, "/result/workspace/workspace_id")?;
    let first_tab_id = json_string(&response, "/result/tab/tab_id")?;
    let first_label = if plans.len() == 1 {
        plans[0].repository.name.as_str()
    } else {
        "shared"
    };
    herdr(&["tab", "rename", &first_tab_id, first_label])?;
    if plans.len() > 1 {
        for (plan, worktree) in plans.iter().zip(worktrees) {
            herdr(&[
                "tab",
                "create",
                "--workspace",
                &workspace_id,
                "--cwd",
                &worktree.to_string_lossy(),
                "--label",
                &plan.repository.name,
                "--no-focus",
            ])?;
        }
    }
    herdr(&["workspace", "focus", &workspace_id])?;
    Ok(workspace_id)
}

fn repository_key(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn json_string(value: &Value, pointer: &str) -> Result<String> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| message(format!("Herdr response omitted {pointer}")))
}

fn herdr_json(arguments: &[&str]) -> Result<Value> {
    Ok(serde_json::from_str(&herdr(arguments)?)?)
}

fn herdr(arguments: &[&str]) -> Result<String> {
    let program = env::var_os("HERDR_BIN_PATH").unwrap_or_else(|| "herdr".into());
    checked(Command::new(program).args(arguments))
}

fn git<const N: usize>(repository: &Path, arguments: [&str; N]) -> Result<String> {
    checked(
        Command::new("git")
            .arg("-C")
            .arg(repository)
            .args(arguments),
    )
}

fn optional_git<const N: usize>(repository: &Path, arguments: [&str; N]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(arguments)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().into())
}

fn checked(command: &mut Command) -> Result<String> {
    let description = format!("{command:?}");
    let Output {
        status,
        stdout,
        stderr,
    } = command.output()?;
    if !status.success() {
        let detail = String::from_utf8_lossy(&stderr).trim().to_owned();
        return Err(message(if detail.is_empty() {
            format!("command failed ({status}): {description}")
        } else {
            format!("command failed ({status}): {description}\n{detail}")
        }));
    }
    Ok(String::from_utf8(stdout)?.trim().into())
}

fn message(value: impl Into<String>) -> Box<dyn Error> {
    io::Error::other(value.into()).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_is_lower_snake_case() {
        assert_eq!(slugify("  Add OAuth 2.0 / Login  "), "add_oauth_2_0_login");
        assert_eq!(slugify("Café launch"), "café_launch");
    }

    #[test]
    fn loads_and_replaces_repository_presets() -> Result<()> {
        let repositories = ["api", "web"].map(|name| Repository {
            name: name.into(),
            path: PathBuf::from(format!("/repos/{name}")),
        });
        let preset = Preset {
            name: "Frontend".into(),
            repositories: vec!["/repos/web".into()],
        };
        assert_eq!(selection_for_preset(&repositories, &preset), [false, true]);

        let mut presets = vec![preset];
        upsert_preset(
            &mut presets,
            Preset {
                name: "frontend".into(),
                repositories: vec!["/repos/api".into(), "/repos/web".into()],
            },
        );
        assert_eq!(presets.len(), 1);
        assert_eq!(presets[0].repositories.len(), 2);

        let old_state: State = serde_json::from_str(r#"{"base_refs":{},"docks":[]}"#)?;
        assert!(old_state.presets.is_empty());
        Ok(())
    }

    #[test]
    fn creates_matching_worktrees_and_agent_guides() -> Result<()> {
        let temporary = env::temp_dir().join(format!(
            "herdr-dock-test-{}-{}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
        ));
        fs::create_dir_all(&temporary)?;
        let repositories = ["api", "web"]
            .map(|name| -> Result<Repository> {
                let path = temporary.join(name);
                fs::create_dir(&path)?;
                checked(Command::new("git").arg("init").arg("--quiet").arg(&path))?;
                fs::write(path.join("README.md"), name)?;
                checked(Command::new("git").arg("-C").arg(&path).args(["add", "."]))?;
                checked(Command::new("git").arg("-C").arg(&path).args([
                    "-c",
                    "user.name=Test",
                    "-c",
                    "user.email=test@example.com",
                    "commit",
                    "--quiet",
                    "-m",
                    "initial",
                ]))?;
                Ok(Repository {
                    name: name.into(),
                    path,
                })
            })
            .into_iter()
            .collect::<Result<Vec<_>>>()?;
        let plans = repositories
            .into_iter()
            .map(|repository| RepositoryPlan {
                repository,
                base_ref: "HEAD".into(),
            })
            .collect::<Vec<_>>();
        let root = temporary.join("workspaces").join("oauth_login");
        let worktrees = materialize_worktrees(&root, "OAuth login", "agent/oauth_login", &plans)?;

        assert_eq!(worktrees.len(), 2);
        for worktree in &worktrees {
            assert_eq!(
                git(worktree, ["branch", "--show-current"])?,
                "agent/oauth_login"
            );
        }
        assert!(fs::read_to_string(root.join("AGENTS.md"))?.contains("api"));
        assert_eq!(
            fs::read(root.join("AGENTS.md"))?,
            fs::read(root.join("CLAUDE.md"))?
        );

        fs::write(worktrees[0].join("dirty.txt"), "changed")?;
        let record = DockRecord {
            name: "OAuth login".into(),
            slug: "oauth_login".into(),
            branch: "agent/oauth_login".into(),
            root: root.clone(),
            workspace_id: "workspace-1".into(),
            created_at_unix: 1,
            repositories: plans
                .iter()
                .zip(&worktrees)
                .map(|(plan, worktree)| DockRepository {
                    name: plan.repository.name.clone(),
                    source: plan.repository.path.clone(),
                    worktree: worktree.clone(),
                    base_ref: plan.base_ref.clone(),
                })
                .collect(),
        };
        let live = BTreeMap::from([(
            "workspace-1".into(),
            LiveWorkspace {
                status: "working".into(),
                agents: vec![AgentOverview {
                    name: "shared".into(),
                    status: "working".into(),
                    cwd: root.to_string_lossy().into(),
                }],
            },
        )]);
        let overview = build_overview(&[record], &live);
        assert!(overview[0].open);
        assert_eq!(overview[0].status, "working");
        assert_eq!(overview[0].agents.len(), 1);
        assert_eq!(
            overview[0]
                .repositories
                .iter()
                .filter(|repository| repository.status == "dirty")
                .count(),
            1
        );

        fs::remove_dir_all(temporary)?;
        Ok(())
    }
}
