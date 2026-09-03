use crate::Result;
use crate::git::message;
use crate::worktrees::WorktreeManager;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Deserialize)]
#[serde(default)]
pub(crate) struct Config {
    pub(crate) branch_prefix: String,
    pub(crate) worktree_root: Option<PathBuf>,
    pub(crate) worktree_manager: WorktreeManager,
    pub(crate) repository_search_roots: Vec<PathBuf>,
    pub(crate) repositories: Vec<RepositoryConfig>,
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
pub(crate) enum RepositoryConfig {
    Path(PathBuf),
    Named { name: Option<String>, path: PathBuf },
}

#[derive(Clone, Debug)]
pub(crate) struct Repository {
    pub(crate) name: String,
    pub(crate) path: PathBuf,
}

#[derive(Clone, Debug)]
pub(crate) struct RepositoryPlan {
    pub(crate) repository: Repository,
    pub(crate) base_ref: String,
}

#[derive(Default, Deserialize, Serialize)]
pub(crate) struct State {
    #[serde(default)]
    pub(crate) base_refs: BTreeMap<String, String>,
    #[serde(default)]
    pub(crate) docks: Vec<DockRecord>,
    #[serde(default)]
    pub(crate) presets: Vec<Preset>,
    #[serde(default)]
    pub(crate) recent_repositories: Vec<PathBuf>,
}

#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct Preset {
    pub(crate) name: String,
    pub(crate) repositories: Vec<String>,
}

pub(crate) type RepositorySelection = (Vec<Repository>, Option<Preset>, Vec<PathBuf>);

#[derive(Deserialize, Serialize)]
pub(crate) struct DockRecord {
    pub(crate) name: String,
    pub(crate) slug: String,
    pub(crate) branch: String,
    pub(crate) root: PathBuf,
    pub(crate) workspace_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) herdr_session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) completed_at_unix: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) archived_at_unix: Option<u64>,
    #[serde(default)]
    pub(crate) worktree_manager: WorktreeManager,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) agents: Vec<DockAgent>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) tabs: Vec<DockTab>,
    pub(crate) repositories: Vec<DockRepository>,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct DockRepository {
    pub(crate) name: String,
    pub(crate) source: PathBuf,
    pub(crate) worktree: PathBuf,
    pub(crate) base_ref: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct DockTab {
    pub(crate) label: String,
    pub(crate) cwd: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct DockAgent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) name: Option<String>,
    pub(crate) kind: String,
    pub(crate) cwd: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) tab: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) session: Option<AgentSession>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct AgentSession {
    pub(crate) source: String,
    pub(crate) agent: String,
    pub(crate) kind: String,
    pub(crate) value: String,
}

pub(crate) struct OpenedWorkspace {
    pub(crate) id: String,
    pub(crate) tabs: Vec<OpenedTab>,
}

pub(crate) struct OpenedTab {
    pub(crate) cwd: PathBuf,
    pub(crate) root_pane_id: String,
}

#[derive(Clone)]
pub(crate) struct LiveWorkspace {
    pub(crate) status: String,
    pub(crate) agents: Vec<AgentOverview>,
    pub(crate) tabs: Vec<LiveTab>,
}

#[derive(Clone)]
pub(crate) struct LiveTab {
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) cwd: String,
    pub(crate) number: u64,
}

#[derive(Clone)]
pub(crate) struct AgentOverview {
    pub(crate) name: String,
    pub(crate) kind: String,
    pub(crate) status: String,
    pub(crate) cwd: String,
    pub(crate) tab_id: Option<String>,
    pub(crate) launch_name: Option<String>,
    pub(crate) session: Option<AgentSession>,
}

pub(crate) struct DockOverview {
    pub(crate) record_index: usize,
    pub(crate) name: String,
    pub(crate) branch: String,
    pub(crate) root: PathBuf,
    pub(crate) workspace_id: String,
    pub(crate) herdr_session: Option<String>,
    pub(crate) status: String,
    pub(crate) open: bool,
    pub(crate) done: bool,
    pub(crate) archived: bool,
    pub(crate) tab_count: usize,
    pub(crate) agents: Vec<AgentOverview>,
    pub(crate) repositories: Vec<RepositoryOverview>,
}

pub(crate) struct RepositoryOverview {
    pub(crate) name: String,
    pub(crate) status: String,
    pub(crate) commit: String,
}

pub(crate) fn load_state(path: &Path) -> Result<State> {
    if path.exists() {
        Ok(serde_json::from_slice(&fs::read(path)?)?)
    } else {
        Ok(State::default())
    }
}

pub(crate) fn lock_state(path: &Path) -> Result<fs::File> {
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path.with_extension("json.lock"))?;
    match lock.try_lock() {
        Ok(()) => Ok(lock),
        Err(fs::TryLockError::WouldBlock) => Err(message(
            "another Herdr Dock action is open; close it and try again",
        )),
        Err(fs::TryLockError::Error(error)) => Err(error.into()),
    }
}

pub(crate) fn save_state(path: &Path, state: &State) -> Result<()> {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("state.json");
    let temporary = path.with_file_name(format!(".{file_name}.{}.tmp", std::process::id()));
    let result = (|| -> Result<()> {
        fs::write(&temporary, serde_json::to_vec_pretty(state)?)?;
        fs::rename(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}
