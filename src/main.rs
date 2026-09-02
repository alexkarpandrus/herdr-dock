mod worktrees;

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
use worktrees::WorktreeManager;

type Result<T> = std::result::Result<T, Box<dyn Error>>;

#[derive(Debug, Deserialize)]
#[serde(default)]
struct Config {
    branch_prefix: String,
    worktree_root: Option<PathBuf>,
    worktree_manager: WorktreeManager,
    repository_search_roots: Vec<PathBuf>,
    repositories: Vec<RepositoryConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            branch_prefix: "agent".into(),
            worktree_root: None,
            worktree_manager: WorktreeManager::Git,
            repository_search_roots: Vec::new(),
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
    #[serde(default)]
    recent_repositories: Vec<PathBuf>,
}

#[derive(Clone, Deserialize, Serialize)]
struct Preset {
    name: String,
    repositories: Vec<String>,
}

type RepositorySelection = (Vec<Repository>, Option<Preset>, Vec<PathBuf>);

#[derive(Deserialize, Serialize)]
struct DockRecord {
    name: String,
    slug: String,
    branch: String,
    root: PathBuf,
    workspace_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    herdr_session: Option<String>,
    created_at_unix: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    completed_at_unix: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    archived_at_unix: Option<u64>,
    #[serde(default)]
    worktree_manager: WorktreeManager,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    agents: Vec<DockAgent>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    tabs: Vec<DockTab>,
    repositories: Vec<DockRepository>,
}

#[derive(Deserialize, Serialize)]
struct DockRepository {
    name: String,
    source: PathBuf,
    worktree: PathBuf,
    base_ref: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct DockTab {
    label: String,
    cwd: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct DockAgent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    kind: String,
    cwd: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tab: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    session: Option<AgentSession>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct AgentSession {
    source: String,
    agent: String,
    kind: String,
    value: String,
}

struct OpenedWorkspace {
    id: String,
    tabs: Vec<OpenedTab>,
}

struct OpenedTab {
    cwd: PathBuf,
    root_pane_id: String,
}

#[derive(Clone)]
struct LiveWorkspace {
    status: String,
    agents: Vec<AgentOverview>,
    tabs: Vec<LiveTab>,
}

#[derive(Clone)]
struct LiveTab {
    id: String,
    label: String,
    cwd: String,
    number: u64,
}

#[derive(Clone)]
struct AgentOverview {
    name: String,
    kind: String,
    status: String,
    cwd: String,
    tab_id: Option<String>,
    launch_name: Option<String>,
    session: Option<AgentSession>,
}

struct DockOverview {
    name: String,
    branch: String,
    root: PathBuf,
    workspace_id: String,
    herdr_session: Option<String>,
    status: String,
    open: bool,
    done: bool,
    archived: bool,
    tab_count: usize,
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
    let state_path = state_dir.join("state.json");
    let mut state = load_state(&state_path)?;
    if state.docks.is_empty() {
        return Err(message("no docks have been created yet"));
    }

    let current_session = current_herdr_session()?;
    let mut docks = collect_overview(&mut state, &state_path)?;
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
                    "{} {} [{}] · {} tabs · {} repos · {} dirty · {} agents",
                    if start + offset == cursor { ">" } else { " " },
                    dock.name,
                    dock.status,
                    dock.tab_count,
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
            format!(
                "Herdr session: {}",
                dock.herdr_session.as_deref().unwrap_or("current/legacy")
            ),
            String::new(),
            "Agents".into(),
        ]);
        if dock.agents.is_empty() {
            lines.push("  none".into());
        } else {
            lines.extend(dock.agents.iter().map(|agent| {
                format!(
                    "  {} ({}) [{}] · {} · session {}",
                    agent.name,
                    agent.kind,
                    agent.status,
                    agent.cwd,
                    agent
                        .session
                        .as_ref()
                        .map(|session| session.value.as_str())
                        .unwrap_or("unavailable")
                )
            }));
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
            if dock.archived {
                "↑/↓ select · R refresh · Esc close".into()
            } else if dock.done {
                "↑/↓ select · Enter reopen · A archive/remove · R refresh · Esc close".into()
            } else {
                "↑/↓ select · Enter focus/reopen · D close/done · A archive/remove · R refresh · Esc close"
                    .into()
            },
        ]);
        ui.frame("Dock overview", &lines)?;

        match read_key()?.code {
            KeyCode::Esc => return Ok(()),
            KeyCode::Up => cursor = cursor.saturating_sub(1),
            KeyCode::Down => cursor = (cursor + 1).min(docks.len() - 1),
            KeyCode::Char('r') | KeyCode::Char('R') => {
                docks = collect_overview(&mut state, &state_path)?;
            }
            KeyCode::Char('d') | KeyCode::Char('D') if !dock.archived && !dock.done => {
                let root = dock.root.clone();
                let open = dock.open;
                if !confirm_complete(&mut ui, dock)? {
                    continue;
                }
                let index = state
                    .docks
                    .iter()
                    .position(|record| record.root == root)
                    .ok_or_else(|| message("dock history record is missing"))?;
                let completed_at = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
                drop(ui);
                let complete_result = (|| -> Result<()> {
                    check_dock_session(&state.docks[index], current_session.as_deref())?;
                    if open {
                        herdr(&["workspace", "close", &state.docks[index].workspace_id])?;
                    }
                    state.docks[index].completed_at_unix = Some(completed_at);
                    if state.docks[index].herdr_session.is_none() {
                        state.docks[index].herdr_session = current_session.clone();
                    }
                    save_state(&state_path, &state)
                })();
                ui = Ui::start()?;
                match complete_result {
                    Ok(()) => docks = collect_overview(&mut state, &state_path)?,
                    Err(error) => show_notice(&mut ui, "Dock not closed", &error.to_string())?,
                }
            }
            KeyCode::Char('a') | KeyCode::Char('A') if !dock.archived => {
                let root = dock.root.clone();
                let open = dock.open;
                if !confirm_archive(&mut ui, dock)? {
                    continue;
                }
                let index = state
                    .docks
                    .iter()
                    .position(|record| record.root == root)
                    .ok_or_else(|| message("dock history record is missing"))?;
                let archived_at = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
                drop(ui);
                let mut archive_result = if open {
                    check_dock_session(&state.docks[index], current_session.as_deref())
                        .and_then(|()| archive_dock(&state.docks[index], true))
                } else {
                    archive_dock(&state.docks[index], false)
                };
                if archive_result.is_ok() {
                    state.docks[index].archived_at_unix = Some(archived_at);
                    if let Err(error) = save_state(&state_path, &state) {
                        state.docks[index].archived_at_unix = None;
                        archive_result = Err(error);
                    }
                }
                ui = Ui::start()?;
                match archive_result {
                    Ok(()) => docks = collect_overview(&mut state, &state_path)?,
                    Err(error) => {
                        show_notice(&mut ui, "Dock not archived", &error.to_string())?;
                    }
                }
            }
            KeyCode::Enter if !dock.archived => {
                let root = dock.root.clone();
                let open = dock.open;
                let workspace_id = dock.workspace_id.clone();
                let index = state
                    .docks
                    .iter()
                    .position(|record| record.root == root)
                    .ok_or_else(|| message("dock history record is missing"))?;
                drop(ui);
                let result = if open {
                    check_dock_session(&state.docks[index], current_session.as_deref())
                        .and_then(|()| herdr(&["workspace", "focus", &workspace_id]).map(drop))
                        .map(|()| Vec::new())
                } else {
                    reopen_dock(&mut state, index, &state_path, current_session.as_deref())
                };
                match result {
                    Ok(errors) if errors.is_empty() => return Ok(()),
                    Ok(errors) => {
                        ui = Ui::start()?;
                        show_notice(&mut ui, "Workspace reopened", &errors.join("\n"))?;
                        return Ok(());
                    }
                    Err(error) => {
                        ui = Ui::start()?;
                        show_notice(&mut ui, "Workspace not opened", &error.to_string())?;
                        docks = collect_overview(&mut state, &state_path)?;
                    }
                }
            }
            _ => {}
        }
    }
}

fn collect_overview(state: &mut State, state_path: &Path) -> Result<Vec<DockOverview>> {
    let live = live_workspaces()?;
    if sync_dock_agents(&mut state.docks, &live) {
        save_state(state_path, state)?;
    }
    Ok(build_overview(&state.docks, &live))
}

fn build_overview(
    records: &[DockRecord],
    live: &BTreeMap<String, LiveWorkspace>,
) -> Vec<DockOverview> {
    records
        .iter()
        .rev()
        .map(|record| {
            let archived = record.archived_at_unix.is_some();
            let done = record.completed_at_unix.is_some();
            let workspace = (!archived)
                .then(|| live.get(&record.workspace_id))
                .flatten();
            let status = if archived {
                "archived".into()
            } else if !record.root.exists() {
                "missing".into()
            } else if done && workspace.is_none() {
                "done".into()
            } else {
                workspace
                    .map(|workspace| workspace.status.clone())
                    .unwrap_or_else(|| "closed".into())
            };
            let repositories = record
                .repositories
                .iter()
                .map(|repository| {
                    let status = if archived {
                        "archived".into()
                    } else if !repository.worktree.exists() {
                        "missing".into()
                    } else {
                        match optional_git(&repository.worktree, ["status", "--porcelain"]) {
                            Some(output) if output.is_empty() => "clean".into(),
                            Some(_) => "dirty".into(),
                            None => "unavailable".into(),
                        }
                    };
                    let commit = if archived {
                        optional_git(
                            &repository.source,
                            ["log", "-1", "--pretty=%h %s", record.branch.as_str()],
                        )
                    } else {
                        optional_git(&repository.worktree, ["log", "-1", "--pretty=%h %s"])
                    }
                    .unwrap_or_else(|| "no commit".into());
                    RepositoryOverview {
                        name: repository.name.clone(),
                        status,
                        commit,
                    }
                })
                .collect();
            let agents = workspace.map_or_else(
                || {
                    record
                        .agents
                        .iter()
                        .map(|agent| AgentOverview {
                            name: agent.name.clone().unwrap_or_else(|| agent.kind.clone()),
                            kind: agent.kind.clone(),
                            status: if done { "done" } else { "saved" }.into(),
                            cwd: agent.cwd.to_string_lossy().into(),
                            tab_id: None,
                            launch_name: agent.name.clone(),
                            session: agent.session.clone(),
                        })
                        .collect()
                },
                |workspace| workspace.agents.clone(),
            );
            DockOverview {
                name: record.name.clone(),
                branch: record.branch.clone(),
                root: record.root.clone(),
                workspace_id: record.workspace_id.clone(),
                herdr_session: record.herdr_session.clone(),
                status,
                open: workspace.is_some(),
                done,
                archived,
                tab_count: workspace.map_or_else(
                    || record.tabs.len().max(1),
                    |workspace| workspace.tabs.len().max(1),
                ),
                agents,
                repositories,
            }
        })
        .collect()
}

fn confirm_archive(ui: &mut Ui, dock: &DockOverview) -> Result<bool> {
    ui.frame(
        "Archive/remove dock",
        &[
            format!("Dock: {}", dock.name),
            format!("Worktrees: {}", dock.repositories.len()),
            String::new(),
            "This closes the workspace and removes clean worktrees.".into(),
            "Branches and the archived history record remain.".into(),
            String::new(),
            "Y archive/remove · any other key cancel".into(),
        ],
    )?;
    Ok(matches!(
        read_key()?.code,
        KeyCode::Char('y') | KeyCode::Char('Y')
    ))
}

fn confirm_complete(ui: &mut Ui, dock: &DockOverview) -> Result<bool> {
    ui.frame(
        "Close dock",
        &[
            format!("Dock: {}", dock.name),
            String::new(),
            "This closes its workspace, tabs, and running processes.".into(),
            "Worktrees and resumable agent session IDs remain.".into(),
            String::new(),
            "Y close/done · any other key cancel".into(),
        ],
    )?;
    Ok(matches!(
        read_key()?.code,
        KeyCode::Char('y') | KeyCode::Char('Y')
    ))
}

fn show_notice(ui: &mut Ui, title: &str, text: &str) -> Result<()> {
    ui.frame(title, &[text.into(), String::new(), "Press any key".into()])?;
    read_key()?;
    Ok(())
}

fn archive_dock(record: &DockRecord, close_workspace: bool) -> Result<()> {
    let root_exists = record.root.exists();
    if root_exists && !record.root.is_dir() {
        return Err(message(format!(
            "dock root is not a directory: {}",
            record.root.display()
        )));
    }

    let mut has_worktrees = false;
    let mut allowed =
        BTreeSet::from([record.root.join("AGENTS.md"), record.root.join("CLAUDE.md")]);
    for repository in &record.repositories {
        if repository.worktree.parent() != Some(record.root.as_path()) {
            return Err(message(format!(
                "worktree is outside the dock root: {}",
                repository.worktree.display()
            )));
        }
        allowed.insert(repository.worktree.clone());
        if !repository.worktree.exists() {
            if let Some(registered) =
                registered_worktree(&repository.source, &repository.worktree, &record.branch)?
            {
                if registered == repository.worktree {
                    return Err(message(format!(
                        "worktree is missing but still registered: {}",
                        repository.worktree.display()
                    )));
                }
                return Err(message(format!(
                    "branch {} is checked out at {}",
                    record.branch,
                    registered.display()
                )));
            }
            continue;
        }
        if !repository.worktree.is_dir() {
            return Err(message(format!(
                "worktree is not a directory: {}",
                repository.worktree.display()
            )));
        }
        if !git(&repository.worktree, ["status", "--porcelain"])?.is_empty() {
            return Err(message(format!(
                "{} has uncommitted or untracked changes",
                repository.name
            )));
        }
        has_worktrees = true;
    }
    if root_exists {
        for entry in fs::read_dir(&record.root)? {
            let path = entry?.path();
            if !allowed.contains(&path) {
                return Err(message(format!(
                    "dock root contains an unexpected file: {}",
                    path.display()
                )));
            }
        }
    }
    if has_worktrees {
        record.worktree_manager.ensure_available()?;
    }

    if close_workspace {
        herdr(&["workspace", "close", &record.workspace_id])?;
    }
    for repository in &record.repositories {
        if repository.worktree.is_dir() {
            record
                .worktree_manager
                .remove(&repository.source, &repository.worktree)?;
        }
    }
    if root_exists {
        for guide in ["AGENTS.md", "CLAUDE.md"] {
            let path = record.root.join(guide);
            if path.exists() {
                fs::remove_file(path)?;
            }
        }
        fs::remove_dir(&record.root)?;
    }
    Ok(())
}

fn registered_worktree(source: &Path, worktree: &Path, branch: &str) -> Result<Option<PathBuf>> {
    let list = checked(Command::new("git").arg("-C").arg(source).args([
        "worktree",
        "list",
        "--porcelain",
        "-z",
    ]))?;
    let branch_ref = format!("branch refs/heads/{branch}");
    let mut current = None;
    for field in list.split('\0') {
        if let Some(path) = field.strip_prefix("worktree ") {
            let path = PathBuf::from(path);
            if path == worktree {
                return Ok(Some(path));
            }
            current = Some(path);
        } else if field == branch_ref {
            return Ok(current);
        } else if field.is_empty() {
            current = None;
        }
    }
    Ok(None)
}

fn live_workspaces() -> Result<BTreeMap<String, LiveWorkspace>> {
    let workspace_response = herdr_json(&["workspace", "list"])?;
    let tab_response = herdr_json(&["tab", "list"])?;
    let pane_response = herdr_json(&["pane", "list"])?;
    let agent_response = herdr_json(&["agent", "list"])?;
    Ok(parse_live_workspaces(
        &workspace_response,
        &tab_response,
        &pane_response,
        &agent_response,
    ))
}

fn parse_live_workspaces(
    workspace_response: &Value,
    tab_response: &Value,
    pane_response: &Value,
    agent_response: &Value,
) -> BTreeMap<String, LiveWorkspace> {
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
                        tabs: Vec::new(),
                    },
                );
            }
        }
    }

    let mut tab_cwds = BTreeMap::new();
    if let Some(items) = pane_response
        .pointer("/result/panes")
        .and_then(Value::as_array)
    {
        for pane in items {
            if let (Some(tab_id), Some(cwd)) = (
                pane.get("tab_id").and_then(Value::as_str),
                pane.get("cwd")
                    .and_then(Value::as_str)
                    .or_else(|| pane.get("foreground_cwd").and_then(Value::as_str)),
            ) {
                tab_cwds.entry(tab_id.to_owned()).or_insert(cwd.to_owned());
            }
        }
    }
    if let Some(items) = tab_response
        .pointer("/result/tabs")
        .and_then(Value::as_array)
    {
        for tab in items {
            let Some(workspace_id) = tab.get("workspace_id").and_then(Value::as_str) else {
                continue;
            };
            let Some(id) = tab.get("tab_id").and_then(Value::as_str) else {
                continue;
            };
            let Some(workspace) = workspaces.get_mut(workspace_id) else {
                continue;
            };
            workspace.tabs.push(LiveTab {
                id: id.into(),
                label: tab
                    .get("label")
                    .and_then(Value::as_str)
                    .unwrap_or("tab")
                    .into(),
                cwd: tab_cwds.get(id).cloned().unwrap_or_default(),
                number: tab
                    .get("number")
                    .and_then(Value::as_u64)
                    .unwrap_or(u64::MAX),
            });
        }
    }
    for workspace in workspaces.values_mut() {
        workspace.tabs.sort_by_key(|tab| tab.number);
    }

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
            let kind = agent
                .get("agent")
                .and_then(Value::as_str)
                .unwrap_or("agent")
                .to_owned();
            let launch_name = agent.get("name").and_then(Value::as_str).map(str::to_owned);
            let session = agent.get("agent_session").and_then(|session| {
                Some(AgentSession {
                    source: session.get("source")?.as_str()?.into(),
                    agent: session.get("agent")?.as_str()?.into(),
                    kind: session.get("kind")?.as_str()?.into(),
                    value: session.get("value")?.as_str()?.into(),
                })
            });
            workspace.agents.push(AgentOverview {
                name: launch_name.clone().unwrap_or_else(|| kind.clone()),
                kind,
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
                tab_id: agent
                    .get("tab_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                launch_name,
                session,
            });
        }
    }
    workspaces
}

fn sync_dock_agents(records: &mut [DockRecord], live: &BTreeMap<String, LiveWorkspace>) -> bool {
    let mut changed = false;
    for record in records
        .iter_mut()
        .filter(|record| record.archived_at_unix.is_none())
    {
        let Some(workspace) = live.get(&record.workspace_id) else {
            continue;
        };
        if !workspace.tabs.is_empty() && workspace.tabs.iter().all(|tab| !tab.cwd.is_empty()) {
            let tabs = workspace
                .tabs
                .iter()
                .map(|tab| DockTab {
                    label: tab.label.clone(),
                    cwd: PathBuf::from(&tab.cwd),
                })
                .collect::<Vec<_>>();
            if record.tabs != tabs {
                record.tabs = tabs;
                changed = true;
            }
        }
        if workspace.agents.is_empty() {
            continue;
        }
        let mut agents = workspace
            .agents
            .iter()
            .filter(|agent| agent.kind != "agent" && !agent.cwd.is_empty())
            .map(|agent| {
                let cwd = PathBuf::from(&agent.cwd);
                let session = agent.session.clone().or_else(|| {
                    record
                        .agents
                        .iter()
                        .find(|saved| {
                            saved.kind == agent.kind
                                && saved.name == agent.launch_name
                                && saved.cwd == cwd
                        })
                        .and_then(|saved| saved.session.clone())
                });
                DockAgent {
                    name: agent.launch_name.clone(),
                    kind: agent.kind.clone(),
                    cwd,
                    tab: agent
                        .tab_id
                        .as_ref()
                        .and_then(|tab_id| workspace.tabs.iter().position(|tab| &tab.id == tab_id)),
                    session,
                }
            })
            .collect::<Vec<_>>();
        for saved in &record.agents {
            if saved.session.is_some()
                && !agents.iter().any(|agent| {
                    agent.name == saved.name
                        && agent.kind == saved.kind
                        && agent.cwd == saved.cwd
                        && agent.tab == saved.tab
                })
            {
                agents.push(saved.clone());
            }
        }
        agents.sort_by(|left, right| {
            left.tab
                .cmp(&right.tab)
                .then_with(|| left.cwd.cmp(&right.cwd))
                .then_with(|| left.kind.cmp(&right.kind))
                .then_with(|| left.name.cmp(&right.name))
        });
        if record.agents != agents {
            record.agents = agents;
            changed = true;
        }
    }
    changed
}

fn current_herdr_session() -> Result<Option<String>> {
    let Some(socket) = env::var_os("HERDR_SOCKET_PATH") else {
        return Ok(None);
    };
    let response = herdr_json(&["session", "list", "--json"])?;
    Ok(response
        .get("sessions")
        .and_then(Value::as_array)
        .and_then(|sessions| {
            sessions.iter().find_map(|session| {
                (session.get("socket_path").and_then(Value::as_str) == socket.to_str())
                    .then(|| {
                        session
                            .get("name")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                    })
                    .flatten()
            })
        }))
}

fn check_dock_session(record: &DockRecord, current: Option<&str>) -> Result<()> {
    if let Some(expected) = record.herdr_session.as_deref()
        && current != Some(expected)
    {
        return Err(message(format!(
            "dock belongs to Herdr session {expected}; reopen it from that session"
        )));
    }
    Ok(())
}

fn reopen_dock(
    state: &mut State,
    index: usize,
    state_path: &Path,
    current_session: Option<&str>,
) -> Result<Vec<String>> {
    check_dock_session(&state.docks[index], current_session)?;
    if !state.docks[index].root.is_dir() {
        return Err(message(format!(
            "dock root is missing: {}",
            state.docks[index].root.display()
        )));
    }
    for repository in &state.docks[index].repositories {
        if !repository.worktree.is_dir() {
            return Err(message(format!(
                "worktree is missing: {}",
                repository.worktree.display()
            )));
        }
    }

    let dock_tabs = if state.docks[index].tabs.is_empty() {
        default_dock_tabs(&state.docks[index].root, &state.docks[index].repositories)
    } else {
        state.docks[index].tabs.clone()
    };
    for tab in &dock_tabs {
        if !tab.cwd.is_dir() {
            return Err(message(format!(
                "tab directory is missing: {}",
                tab.cwd.display()
            )));
        }
    }
    let opened = open_workspace(&state.docks[index].name, &dock_tabs)?;
    let previous_workspace_id =
        std::mem::replace(&mut state.docks[index].workspace_id, opened.id.clone());
    let previous_completed = state.docks[index].completed_at_unix.take();
    let previous_session = state.docks[index].herdr_session.clone();
    let previous_tabs = std::mem::replace(&mut state.docks[index].tabs, dock_tabs);
    if let Some(session) = current_session {
        state.docks[index].herdr_session = Some(session.into());
    }
    if let Err(error) = save_state(state_path, state) {
        state.docks[index].workspace_id = previous_workspace_id;
        state.docks[index].completed_at_unix = previous_completed;
        state.docks[index].herdr_session = previous_session;
        state.docks[index].tabs = previous_tabs;
        let _ = herdr(&["workspace", "close", &opened.id]);
        return Err(error);
    }

    let errors = resume_agents(&state.docks[index], &opened);
    herdr(&["workspace", "focus", &opened.id])?;
    Ok(errors)
}

fn create_dock() -> Result<()> {
    let config_dir = required_directory("HERDR_PLUGIN_CONFIG_DIR")?;
    let state_dir = required_directory("HERDR_PLUGIN_STATE_DIR")?;
    let config_path = config_dir.join("config.toml");
    let state_path = state_dir.join("state.json");
    let config = load_config(&config_path)?;
    let mut state = load_state(&state_path)?;
    let herdr_session = current_herdr_session()?;
    let mut repositories = load_repositories(&config.repositories)?;
    merge_recent_repositories(&mut repositories, &state.recent_repositories);
    if repositories.is_empty() && config.repository_search_roots.is_empty() {
        return Err(message(format!(
            "no repositories configured; add [[repositories]] or repository_search_roots to {}",
            config_path.display()
        )));
    }
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
        let Some((selected, preset, recent)) = prompt_repositories(
            &mut ui,
            &repositories,
            &state.presets,
            &config.repository_search_roots,
        )?
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
        (name, plans, preset, recent)
    };

    let (name, plans, preset, recent) = selection;
    let slug = slugify(&name);
    let branch = format!("{}/{}", config.branch_prefix, slug);
    check_branch_name(&branch)?;
    let root = worktree_root.join(&slug);

    println!("Creating {branch} in {}...", root.display());
    let worktrees = materialize_worktrees(config.worktree_manager, &root, &name, &branch, &plans)?;
    let dock_repositories = plans
        .iter()
        .zip(&worktrees)
        .map(|(plan, worktree)| DockRepository {
            name: plan.repository.name.clone(),
            source: plan.repository.path.clone(),
            worktree: worktree.clone(),
            base_ref: plan.base_ref.clone(),
        })
        .collect::<Vec<_>>();
    let dock_tabs = default_dock_tabs(&root, &dock_repositories);
    let workspace = open_workspace(&name, &dock_tabs)?;

    for plan in &plans {
        state
            .base_refs
            .insert(repository_key(&plan.repository.path), plan.base_ref.clone());
    }
    for repository in recent {
        remember_repository(&mut state.recent_repositories, repository);
    }
    if let Some(preset) = preset {
        upsert_preset(&mut state.presets, preset);
    }
    state.docks.push(DockRecord {
        name: name.clone(),
        slug,
        branch,
        root: root.clone(),
        workspace_id: workspace.id.clone(),
        herdr_session,
        created_at_unix: SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
        completed_at_unix: None,
        archived_at_unix: None,
        worktree_manager: config.worktree_manager,
        agents: Vec::new(),
        tabs: dock_tabs,
        repositories: dock_repositories,
    });
    save_state(&state_path, &state)?;
    herdr(&["workspace", "focus", &workspace.id])?;
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
    search_roots: &[PathBuf],
) -> Result<Option<RepositorySelection>> {
    let mut repositories = repositories.to_vec();
    let mut selected = vec![false; repositories.len()];
    let mut query = String::new();
    let mut cursor = 0;
    let mut preset_name = None;
    let mut recent = Vec::new();
    let mut discovered = None;
    loop {
        let quick_matches = if query.is_empty() {
            repositories.iter().collect::<Vec<_>>()
        } else {
            ranked_repositories(&query, &repositories)
        };
        let found_matches = if query.is_empty() {
            Vec::new()
        } else {
            ranked_repositories(&query, discovered.as_deref().unwrap_or_default())
                .into_iter()
                .filter(|repository| {
                    !repositories
                        .iter()
                        .any(|existing| existing.path == repository.path)
                })
                .collect::<Vec<_>>()
        };
        let choices = quick_matches
            .into_iter()
            .map(|repository| (true, repository))
            .chain(
                found_matches
                    .into_iter()
                    .map(|repository| (false, repository)),
            )
            .collect::<Vec<_>>();
        cursor = cursor.min(choices.len().saturating_sub(1));
        let height = terminal::size()?.1.saturating_sub(9) as usize;
        let visible = height.max(1).min(choices.len().max(1));
        let start = cursor.saturating_sub(visible.saturating_sub(1));
        let mut lines = vec![format!("Search: > {query}"), String::new()];
        if choices.is_empty() {
            lines.push(if query.is_empty() {
                "Type to search configured roots.".into()
            } else {
                "No matches.".into()
            });
        } else {
            lines.extend(choices[start..start + visible].iter().enumerate().map(
                |(offset, (quick, repository))| {
                    let is_selected = *quick
                        && repositories
                            .iter()
                            .position(|existing| existing.path == repository.path)
                            .is_some_and(|index| selected[index]);
                    format!(
                        "{} [{}] {}  {}  ({})",
                        if start + offset == cursor { ">" } else { " " },
                        if is_selected { "x" } else { " " },
                        repository.name,
                        repository.path.display(),
                        if *quick { "quick" } else { "found" }
                    )
                },
            ));
        }
        if let Some(name) = &preset_name {
            lines.push(format!("Save as preset: {name}"));
        }
        lines.push(String::new());
        lines.push(
            "Type to search · ↑/↓ move · Enter add/continue · Space select · Esc clear/cancel"
                .into(),
        );
        if !presets.is_empty() {
            lines.push("Shift+P load preset · Shift+S save preset".into());
        }
        ui.frame("Create dock · repositories", &lines)?;
        match read_key()?.code {
            KeyCode::Esc if !query.is_empty() => {
                query.clear();
                cursor = 0;
            }
            KeyCode::Esc => return Ok(None),
            KeyCode::Up => cursor = cursor.saturating_sub(1),
            KeyCode::Down if !choices.is_empty() => cursor = (cursor + 1).min(choices.len() - 1),
            KeyCode::Char(' ') if query.is_empty() && !choices.is_empty() => {
                selected[cursor] = !selected[cursor]
            }
            KeyCode::Char('P') if query.is_empty() && !presets.is_empty() => {
                if let Some(preset) = prompt_preset(ui, presets)? {
                    selected = selection_for_preset(&repositories, &preset);
                    preset_name = None;
                    cursor = 0;
                }
            }
            KeyCode::Char('S') if query.is_empty() && selected.iter().any(|value| *value) => {
                preset_name = prompt_text(ui, "Create dock · preset name")?;
            }
            KeyCode::Enter if !query.is_empty() && !choices.is_empty() => {
                let (quick, repository) = (choices[cursor].0, choices[cursor].1.clone());
                if quick {
                    if let Some(index) = repositories
                        .iter()
                        .position(|existing| existing.path == repository.path)
                    {
                        selected[index] = true;
                        query.clear();
                        cursor = index;
                    }
                } else if repositories
                    .iter()
                    .any(|existing| existing.name == repository.name)
                {
                    show_notice(
                        ui,
                        "Repository name conflict",
                        &format!(
                            "{} is already used; configure an explicit name",
                            repository.name
                        ),
                    )?;
                } else {
                    recent.push(repository.path.clone());
                    repositories.push(repository);
                    selected.push(true);
                    query.clear();
                    cursor = repositories.len() - 1;
                }
            }
            KeyCode::Enter if query.is_empty() && selected.iter().any(|value| *value) => {
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
                return Ok(Some((chosen, preset, recent)));
            }
            KeyCode::Backspace if !query.is_empty() => {
                query.pop();
                cursor = 0;
            }
            KeyCode::Char(character) => {
                query.push(character);
                cursor = 0;
                if discovered.is_none() && !search_roots.is_empty() {
                    ui.frame(
                        "Create dock · repositories",
                        &[
                            format!("Search: > {query}"),
                            "Scanning configured roots…".into(),
                        ],
                    )?;
                    discovered = Some(discover_repositories(search_roots)?);
                }
            }
            _ => {}
        }
    }
}

fn ranked_repositories<'a>(query: &str, repositories: &'a [Repository]) -> Vec<&'a Repository> {
    let mut matches = repositories
        .iter()
        .filter_map(|repository| {
            let score = fuzzy_score(query, &repository.name).or_else(|| {
                fuzzy_score(query, &repository.path.to_string_lossy()).map(|score| score + 1_000)
            })?;
            Some((score, repository))
        })
        .collect::<Vec<_>>();
    matches.sort_by(|(left_score, left), (right_score, right)| {
        left_score
            .cmp(right_score)
            .then_with(|| left.path.cmp(&right.path))
    });
    matches
        .into_iter()
        .map(|(_, repository)| repository)
        .collect()
}

fn ranked_refs<'a>(query: &str, refs: &'a [String]) -> Vec<&'a String> {
    if query.is_empty() {
        return refs.iter().collect();
    }
    let mut matches = refs
        .iter()
        .filter_map(|reference| Some((fuzzy_score(query, reference)?, reference)))
        .collect::<Vec<_>>();
    matches.sort_by(|(left_score, left), (right_score, right)| {
        left_score.cmp(right_score).then_with(|| left.cmp(right))
    });
    matches
        .into_iter()
        .map(|(_, reference)| reference)
        .collect()
}

fn fuzzy_score(query: &str, candidate: &str) -> Option<usize> {
    let needle = query
        .chars()
        .flat_map(char::to_lowercase)
        .collect::<Vec<_>>();
    if needle.is_empty() {
        return Some(0);
    }
    let haystack = candidate
        .chars()
        .flat_map(char::to_lowercase)
        .collect::<Vec<_>>();
    let mut next = 0;
    let mut previous = None;
    let mut score = 0;
    for character in needle {
        let index = haystack[next..]
            .iter()
            .position(|candidate| *candidate == character)?
            + next;
        score += previous.map_or(index, |previous| index - previous - 1);
        previous = Some(index);
        next = index + 1;
    }
    Some(score)
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
    let initial_cursor = refs.iter().position(|value| value == initial).unwrap_or(0);
    let mut query = String::new();
    let mut cursor = initial_cursor;
    loop {
        let matches = ranked_refs(&query, refs);
        cursor = cursor.min(matches.len().saturating_sub(1));
        let height = terminal::size()?.1.saturating_sub(9) as usize;
        let visible = height.max(1).min(matches.len().max(1));
        let start = cursor.saturating_sub(visible.saturating_sub(1));
        let mut lines = vec![format!("Search: > {query}"), String::new()];
        if matches.is_empty() {
            lines.push("No matches.".into());
        } else {
            lines.extend(matches[start..start + visible].iter().enumerate().map(
                |(offset, reference)| {
                    format!(
                        "{} {}",
                        if start + offset == cursor { ">" } else { " " },
                        reference
                    )
                },
            ));
        }
        lines.extend([
            String::new(),
            "Type to search · ↑/↓ move · Enter select · Esc clear/cancel".into(),
        ]);
        ui.frame(&format!("Create dock · base ref for {repository}"), &lines)?;
        match read_key()?.code {
            KeyCode::Esc if !query.is_empty() => {
                query.clear();
                cursor = initial_cursor;
            }
            KeyCode::Esc => return Ok(None),
            KeyCode::Up => cursor = cursor.saturating_sub(1),
            KeyCode::Down if !matches.is_empty() => cursor = (cursor + 1).min(matches.len() - 1),
            KeyCode::Enter if !matches.is_empty() => return Ok(Some(matches[cursor].clone())),
            KeyCode::Backspace if !query.is_empty() => {
                query.pop();
                cursor = if query.is_empty() { initial_cursor } else { 0 };
            }
            KeyCode::Char(character) => {
                query.push(character);
                cursor = 0;
            }
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
# worktree_manager = "worktrunk"
# repository_search_roots = ["~/Src"]

# Add one block per repository:
# [[repositories]]
# name = "api"
# path = "~/src/api"
"#,
        )?;
        return Err(message(format!(
            "created {}; add repositories or repository_search_roots and run the action again",
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
        let repository = load_repository(configured_path, configured_name)?;
        if !paths.insert(repository.path.clone()) {
            continue;
        }
        if !names.insert(repository.name.clone()) {
            return Err(message(format!(
                "repository name `{}` is not unique",
                repository.name
            )));
        }
        repositories.push(repository);
    }
    Ok(repositories)
}

fn load_repository(path: &Path, configured_name: Option<&str>) -> Result<Repository> {
    let path = expand_home(path)?.canonicalize()?;
    let root = git(&path, ["rev-parse", "--show-toplevel"])?;
    let root = PathBuf::from(root.trim()).canonicalize()?;
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
    Ok(Repository { name, path: root })
}

fn merge_recent_repositories(repositories: &mut Vec<Repository>, recent: &[PathBuf]) {
    let mut paths = repositories
        .iter()
        .map(|repository| repository.path.clone())
        .collect::<BTreeSet<_>>();
    let mut names = repositories
        .iter()
        .map(|repository| repository.name.clone())
        .collect::<BTreeSet<_>>();
    for path in recent {
        if let Ok(repository) = load_repository(path, None)
            && paths.insert(repository.path.clone())
            && names.insert(repository.name.clone())
        {
            repositories.push(repository);
        }
    }
}

fn remember_repository(recent: &mut Vec<PathBuf>, repository: PathBuf) {
    recent.retain(|existing| existing != &repository);
    recent.insert(0, repository);
}

fn discover_repositories(search_roots: &[PathBuf]) -> Result<Vec<Repository>> {
    let mut stack = Vec::new();
    for configured_root in search_roots {
        let root = expand_home(configured_root)?
            .canonicalize()
            .map_err(|error| {
                message(format!(
                    "cannot search {}: {error}",
                    configured_root.display()
                ))
            })?;
        if !root.is_dir() {
            return Err(message(format!(
                "repository search root is not a directory: {}",
                root.display()
            )));
        }
        stack.push(root);
    }

    let mut repositories = Vec::new();
    let mut paths = BTreeSet::new();
    while let Some(directory) = stack.pop() {
        if directory.join(".git").exists() {
            if let Ok(repository) = load_repository(&directory, None)
                && paths.insert(repository.path.clone())
            {
                repositories.push(repository);
            }
            continue;
        }
        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            if entry.file_type().is_ok_and(|file_type| file_type.is_dir()) {
                stack.push(entry.path());
            }
        }
    }
    repositories.sort_by(|left, right| left.path.cmp(&right.path));
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
    manager: WorktreeManager,
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
    manager.ensure_available()?;
    if let Some(parent) = root.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::create_dir(root)?;
    let mut created: Vec<(PathBuf, PathBuf)> = Vec::new();
    let result = (|| {
        for plan in plans {
            let destination = root.join(&plan.repository.name);
            let branch_exists = Command::new("git")
                .arg("-C")
                .arg(&plan.repository.path)
                .args(["show-ref", "--verify", "--quiet"])
                .arg(format!("refs/heads/{branch}"))
                .status()?
                .success();
            if let Err(error) = manager.create(
                &plan.repository.path,
                &destination,
                branch,
                &plan.base_ref,
                branch_exists,
            ) {
                if destination.exists() {
                    created.push((plan.repository.path.clone(), destination));
                }
                return Err(error);
            }
            created.push((plan.repository.path.clone(), destination));
        }
        write_agent_guides(root, name, branch, plans)?;
        Ok(())
    })();
    if let Err(error) = result {
        let mut cleanup_errors = Vec::new();
        for (repository, worktree) in created.iter().rev() {
            if let Err(cleanup_error) = manager.remove(repository, worktree) {
                cleanup_errors.push(cleanup_error.to_string());
            }
        }
        if cleanup_errors.is_empty() {
            for guide in ["AGENTS.md", "CLAUDE.md"] {
                let path = root.join(guide);
                if path.exists() {
                    fs::remove_file(path)?;
                }
            }
            fs::remove_dir(root)?;
            return Err(error);
        }
        return Err(message(format!(
            "{error}; cleanup incomplete: {}",
            cleanup_errors.join("; ")
        )));
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

fn default_dock_tabs(root: &Path, repositories: &[DockRepository]) -> Vec<DockTab> {
    if repositories.len() == 1 {
        return vec![DockTab {
            label: repositories[0].name.clone(),
            cwd: repositories[0].worktree.clone(),
        }];
    }
    let mut tabs = vec![DockTab {
        label: "shared".into(),
        cwd: root.to_path_buf(),
    }];
    tabs.extend(repositories.iter().map(|repository| DockTab {
        label: repository.name.clone(),
        cwd: repository.worktree.clone(),
    }));
    tabs
}

fn open_workspace(name: &str, dock_tabs: &[DockTab]) -> Result<OpenedWorkspace> {
    let first = dock_tabs
        .first()
        .ok_or_else(|| message("dock must have at least one tab"))?;
    let response = herdr_json(&[
        "workspace",
        "create",
        "--cwd",
        &first.cwd.to_string_lossy(),
        "--label",
        name,
        "--no-focus",
    ])?;
    let workspace_id = json_string(&response, "/result/workspace/workspace_id")?;
    let setup = (|| -> Result<Vec<OpenedTab>> {
        let first_tab_id = json_string(&response, "/result/tab/tab_id")?;
        let first_pane_id = json_string(&response, "/result/root_pane/pane_id")?;
        herdr(&["tab", "rename", &first_tab_id, &first.label])?;
        let mut tabs = vec![OpenedTab {
            cwd: first.cwd.clone(),
            root_pane_id: first_pane_id,
        }];
        for tab in &dock_tabs[1..] {
            let response = herdr_json(&[
                "tab",
                "create",
                "--workspace",
                &workspace_id,
                "--cwd",
                &tab.cwd.to_string_lossy(),
                "--label",
                &tab.label,
                "--no-focus",
            ])?;
            tabs.push(OpenedTab {
                cwd: tab.cwd.clone(),
                root_pane_id: json_string(&response, "/result/root_pane/pane_id")?,
            });
        }
        Ok(tabs)
    })();
    match setup {
        Ok(tabs) => Ok(OpenedWorkspace {
            id: workspace_id,
            tabs,
        }),
        Err(error) => {
            if let Err(close_error) = herdr(&["workspace", "close", &workspace_id]) {
                return Err(message(format!(
                    "{error}; also failed to close partial workspace: {close_error}"
                )));
            }
            Err(error)
        }
    }
}

fn resume_agents(record: &DockRecord, workspace: &OpenedWorkspace) -> Vec<String> {
    let mut errors = Vec::new();
    let mut occupied_tabs = BTreeSet::new();
    for (index, agent) in record.agents.iter().enumerate() {
        let Some(resume_arguments) = agent_resume_arguments(agent) else {
            errors.push(format!(
                "{} has no supported resumable session",
                agent.name.as_deref().unwrap_or(&agent.kind)
            ));
            continue;
        };
        let tab_index = agent
            .tab
            .filter(|index| *index < workspace.tabs.len())
            .or_else(|| {
                workspace
                    .tabs
                    .iter()
                    .enumerate()
                    .filter(|(_, tab)| agent.cwd.starts_with(&tab.cwd))
                    .max_by_key(|(_, tab)| tab.cwd.components().count())
                    .map(|(index, _)| index)
            });
        let Some(tab_index) = tab_index else {
            errors.push(format!("{} has no matching dock tab", agent.cwd.display()));
            continue;
        };
        let tab = &workspace.tabs[tab_index];
        let pane_id = if !occupied_tabs.contains(&tab_index) && agent.cwd == tab.cwd {
            occupied_tabs.insert(tab_index);
            Ok(tab.root_pane_id.clone())
        } else {
            let response = herdr_json(&[
                "pane",
                "split",
                &tab.root_pane_id,
                "--direction",
                "right",
                "--cwd",
                &agent.cwd.to_string_lossy(),
                "--no-focus",
            ]);
            response.and_then(|response| json_string(&response, "/result/pane/pane_id"))
        };
        let pane_id = match pane_id {
            Ok(pane_id) => pane_id,
            Err(error) => {
                errors.push(format!("{}: {error}", agent.cwd.display()));
                continue;
            }
        };
        let name = agent_launch_name(record, agent, index);
        let mut arguments = vec![
            "agent".into(),
            "start".into(),
            name.clone(),
            "--kind".into(),
            agent.kind.clone(),
            "--pane".into(),
            pane_id,
            "--".into(),
        ];
        arguments.extend(resume_arguments);
        let references = arguments.iter().map(String::as_str).collect::<Vec<_>>();
        if let Err(error) = herdr(&references) {
            errors.push(format!("{name}: {error}"));
        }
    }
    errors
}

fn agent_launch_name(record: &DockRecord, agent: &DockAgent, index: usize) -> String {
    if let Some(name) = agent.name.as_ref().filter(|name| valid_agent_name(name)) {
        return name.clone();
    }
    let mut base = format!(
        "dock-{}-{}",
        record
            .slug
            .chars()
            .filter(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
            .collect::<String>(),
        agent
            .kind
            .chars()
            .filter(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
            .collect::<String>()
    );
    let suffix = format!("-{}", index + 1);
    base.truncate(32 - suffix.len());
    base.push_str(&suffix);
    base
}

fn valid_agent_name(name: &str) -> bool {
    name.len() <= 32
        && name.starts_with(|character: char| character.is_ascii_lowercase())
        && name.chars().all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || matches!(character, '_' | '-')
        })
}

fn agent_resume_arguments(agent: &DockAgent) -> Option<Vec<String>> {
    let session = agent.session.as_ref()?;
    if session.agent != agent.kind
        || session.value.is_empty()
        || session.value.chars().any(char::is_control)
    {
        return None;
    }
    match session.kind.as_str() {
        "id" if session.value.len() > 512 => return None,
        "path" if session.value.len() > 4096 || !Path::new(&session.value).is_absolute() => {
            return None;
        }
        "id" | "path" => {}
        _ => return None,
    }

    let value = session.value.clone();
    match (
        session.source.as_str(),
        agent.kind.as_str(),
        session.kind.as_str(),
    ) {
        ("herdr:claude", "claude", "id") => Some(vec!["--resume".into(), value]),
        ("herdr:codex", "codex", "id") => Some(vec!["resume".into(), value]),
        ("herdr:copilot", "copilot", "id") => Some(vec![format!("--resume={value}")]),
        ("herdr:devin", "devin", "id")
        | ("herdr:droid", "droid", "id")
        | ("herdr:hermes", "hermes", "id")
        | ("herdr:qodercli", "qodercli", "id")
        | ("herdr:qwen", "qwen", "id")
        | ("herdr:grok", "grok", "id") => Some(vec!["--resume".into(), value]),
        ("herdr:kimi", "kimi", "id")
        | ("herdr:opencode", "opencode", "id")
        | ("herdr:kilo", "kilo", "id") => Some(vec!["--session".into(), value]),
        ("herdr:mastracode", "mastracode", "id") => Some(vec!["--thread".into(), value]),
        ("herdr:pi", "pi", "id" | "path") => Some(vec!["--session".into(), value]),
        ("herdr:omp", "omp", "id" | "path") => Some(vec![format!("--resume={value}")]),
        ("herdr:cursor", "cursor", "id") => Some(vec!["--resume".into(), value]),
        ("herdr:antigravity_cli", "agy", "id") => Some(vec!["--conversation".into(), value]),
        _ => None,
    }
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
        assert!(old_state.recent_repositories.is_empty());

        let default_config: Config = toml::from_str("branch_prefix = 'agent'")?;
        assert_eq!(default_config.worktree_manager, WorktreeManager::Git);
        assert!(default_config.repository_search_roots.is_empty());
        let search_config: Config =
            toml::from_str("repository_search_roots = ['~/Src', '~/Work']")?;
        assert_eq!(search_config.repository_search_roots.len(), 2);
        let worktrunk_config: Config =
            toml::from_str("branch_prefix = 'agent'\nworktree_manager = 'worktrunk'")?;
        assert_eq!(
            worktrunk_config.worktree_manager,
            WorktreeManager::Worktrunk
        );

        let old_record: DockRecord = serde_json::from_str(
            r#"{"name":"x","slug":"x","branch":"agent/x","root":"/tmp/x","workspace_id":"x","created_at_unix":1,"repositories":[]}"#,
        )?;
        assert_eq!(old_record.worktree_manager, WorktreeManager::Git);
        assert!(old_record.herdr_session.is_none());
        assert!(old_record.completed_at_unix.is_none());
        assert!(old_record.agents.is_empty());
        assert!(old_record.tabs.is_empty());
        Ok(())
    }

    #[test]
    fn persists_resumable_agent_sessions() -> Result<()> {
        let temporary = env::temp_dir().join(format!(
            "herdr-dock-agent-state-test-{}-{}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
        ));
        fs::create_dir_all(&temporary)?;
        let workspace_response = serde_json::json!({
            "result": {"workspaces": [{"workspace_id": "w1", "agent_status": "idle"}]}
        });
        let agent_response = serde_json::json!({
            "result": {"agents": [{
                "workspace_id": "w1",
                "name": "reviewer",
                "agent": "codex",
                "agent_status": "idle",
                "tab_id": "t1",
                "foreground_cwd": temporary,
                "agent_session": {
                    "source": "herdr:codex",
                    "agent": "codex",
                    "kind": "id",
                    "value": "session-123"
                }
            }]}
        });
        let tab_response = serde_json::json!({
            "result": {"tabs": [{"workspace_id": "w1", "tab_id": "t1", "label": "work", "number": 1}]}
        });
        let pane_response = serde_json::json!({
            "result": {"panes": [{"tab_id": "t1", "cwd": temporary}]}
        });
        let live = parse_live_workspaces(
            &workspace_response,
            &tab_response,
            &pane_response,
            &agent_response,
        );
        let mut state = State::default();
        state.docks.push(DockRecord {
            name: "Test dock".into(),
            slug: "test_dock".into(),
            branch: "agent/test_dock".into(),
            root: temporary.clone(),
            workspace_id: "w1".into(),
            herdr_session: Some("default".into()),
            created_at_unix: 1,
            completed_at_unix: None,
            archived_at_unix: None,
            worktree_manager: WorktreeManager::Git,
            agents: Vec::new(),
            tabs: Vec::new(),
            repositories: Vec::new(),
        });

        check_dock_session(&state.docks[0], Some("default"))?;
        assert!(check_dock_session(&state.docks[0], Some("other")).is_err());
        assert!(check_dock_session(&state.docks[0], None).is_err());

        assert!(sync_dock_agents(&mut state.docks, &live));
        assert!(!sync_dock_agents(&mut state.docks, &live));
        assert_eq!(state.docks[0].tabs[0].label, "work");
        assert_eq!(state.docks[0].agents[0].tab, Some(0));
        assert_eq!(
            agent_resume_arguments(&state.docks[0].agents[0]),
            Some(vec!["resume".into(), "session-123".into()])
        );
        state.docks[0].completed_at_unix = Some(2);
        let overview = build_overview(&state.docks, &BTreeMap::new());
        assert_eq!(overview[0].status, "done");
        assert_eq!(overview[0].agents[0].status, "done");

        let state_path = temporary.join("state.json");
        save_state(&state_path, &state)?;
        let loaded = load_state(&state_path)?;
        assert_eq!(loaded.docks[0].agents, state.docks[0].agents);
        assert!(!temporary.join("state.json.tmp").exists());
        fs::remove_dir_all(temporary)?;
        Ok(())
    }

    #[test]
    fn discovers_and_ranks_repositories() -> Result<()> {
        let temporary = env::temp_dir().join(format!(
            "herdr-dock-search-test-{}-{}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
        ));
        let api = temporary.join("services").join("application-api");
        let web = temporary.join("clients").join("web-client");
        fs::create_dir_all(&api)?;
        fs::create_dir_all(&web)?;
        checked(Command::new("git").arg("init").arg("--quiet").arg(&api))?;
        checked(Command::new("git").arg("init").arg("--quiet").arg(&web))?;

        let repositories = discover_repositories(std::slice::from_ref(&temporary))?;
        assert_eq!(repositories.len(), 2);
        assert_eq!(
            ranked_repositories("wc", &repositories)[0].name,
            "web-client"
        );
        assert!(ranked_repositories("missing", &repositories).is_empty());

        let refs = ["HEAD", "main", "origin/main", "feature/search"].map(str::to_owned);
        assert_eq!(ranked_refs("fs", &refs)[0], "feature/search");
        assert!(ranked_refs("missing", &refs).is_empty());

        let mut recent = vec![api.canonicalize()?];
        remember_repository(&mut recent, web.canonicalize()?);
        remember_repository(&mut recent, api.canonicalize()?);
        assert_eq!(recent[0], api.canonicalize()?);
        assert_eq!(recent.len(), 2);

        let mut quick_list = Vec::new();
        merge_recent_repositories(&mut quick_list, &recent);
        assert_eq!(quick_list.len(), 2);
        fs::remove_dir_all(temporary)?;
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
        let worktrees = materialize_worktrees(
            WorktreeManager::Git,
            &root,
            "OAuth login",
            "agent/oauth_login",
            &plans,
        )?;

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
        let mut record = DockRecord {
            name: "OAuth login".into(),
            slug: "oauth_login".into(),
            branch: "agent/oauth_login".into(),
            root: root.clone(),
            workspace_id: "workspace-1".into(),
            herdr_session: None,
            created_at_unix: 1,
            completed_at_unix: None,
            archived_at_unix: None,
            worktree_manager: WorktreeManager::Git,
            agents: Vec::new(),
            tabs: Vec::new(),
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
                    kind: "pi".into(),
                    status: "working".into(),
                    cwd: root.to_string_lossy().into(),
                    tab_id: None,
                    launch_name: None,
                    session: None,
                }],
                tabs: Vec::new(),
            },
        )]);
        let overview = build_overview(std::slice::from_ref(&record), &live);
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

        let error = archive_dock(&record, false).expect_err("dirty worktree must be refused");
        assert!(
            error
                .to_string()
                .contains("uncommitted or untracked changes")
        );
        assert!(root.exists());

        fs::remove_file(worktrees[0].join("dirty.txt"))?;
        let relocated = temporary.join("relocated");
        checked(
            Command::new("git")
                .arg("-C")
                .arg(&record.repositories[1].source)
                .args(["worktree", "move"])
                .arg(&record.repositories[1].worktree)
                .arg(&relocated),
        )?;
        let error = archive_dock(&record, false).expect_err("relocated worktree must be refused");
        assert!(error.to_string().contains("is checked out at"));
        checked(
            Command::new("git")
                .arg("-C")
                .arg(&record.repositories[1].source)
                .args(["worktree", "move"])
                .arg(&relocated)
                .arg(&record.repositories[1].worktree),
        )?;
        record.worktree_manager.remove(
            &record.repositories[0].source,
            &record.repositories[0].worktree,
        )?;
        assert!(!worktrees[0].exists());
        archive_dock(&record, false)?;
        assert!(!root.exists());
        archive_dock(&record, false)?;
        for plan in &plans {
            git(
                &plan.repository.path,
                ["rev-parse", "--verify", "refs/heads/agent/oauth_login"],
            )?;
        }

        record.archived_at_unix = Some(2);
        let overview = build_overview(&[record], &BTreeMap::new());
        assert_eq!(overview[0].status, "archived");
        assert!(overview[0].archived);
        assert!(
            overview[0]
                .repositories
                .iter()
                .all(|repository| repository.status == "archived")
        );

        fs::remove_dir_all(temporary)?;
        Ok(())
    }
}
