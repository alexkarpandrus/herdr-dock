use crate::Result;
use crate::git::{check_branch_name, checked, default_base_ref, git, git_refs, message};
use crate::herdr::{current_herdr_session, herdr, live_workspaces, open_workspace, resume_agents};
use crate::model::{
    DockAgent, DockRecord, DockRepository, DockTab, LiveWorkspace, RepositoryPlan, State,
    load_state, lock_state, save_state,
};
use crate::repos::{
    expand_home, load_config, load_repositories, merge_recent_repositories, remember_repository,
    repository_key, required_directory,
};
use crate::ui::{
    BaseRefChoice, Ui, confirm_create, prompt_base_ref, prompt_name, prompt_repositories, slugify,
    upsert_preset,
};
use crate::worktrees::WorktreeManager;
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fs, io,
    path::{Path, PathBuf},
    process::Command,
};

pub(crate) fn legacy_workspace_is_live(
    record: &DockRecord,
    live: &BTreeMap<String, LiveWorkspace>,
) -> bool {
    record.herdr_session.is_none() && live.contains_key(&record.workspace_id)
}
pub(crate) fn ensure_no_live_legacy_workspace(record: &DockRecord) -> Result<()> {
    if record.herdr_session.is_none() && legacy_workspace_is_live(record, &live_workspaces()?) {
        return Err(message(
            "legacy dock may still be open; close its old workspace before continuing",
        ));
    }
    Ok(())
}
pub(crate) fn preflight_archive(record: &DockRecord) -> Result<bool> {
    let root_exists = match fs::symlink_metadata(&record.root) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                return Err(message(format!(
                    "dock root is a symbolic link: {}",
                    record.root.display()
                )));
            }
            if !metadata.is_dir() {
                return Err(message(format!(
                    "dock root is not a directory: {}",
                    record.root.display()
                )));
            }
            true
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => return Err(error.into()),
    };
    let canonical_root = root_exists
        .then(|| record.root.canonicalize())
        .transpose()?;
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
        let metadata = match fs::symlink_metadata(&repository.worktree) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
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
            Err(error) => return Err(error.into()),
        };
        if metadata.file_type().is_symlink() {
            return Err(message(format!(
                "worktree is a symbolic link: {}",
                repository.worktree.display()
            )));
        }
        if !metadata.is_dir() {
            return Err(message(format!(
                "worktree is not a directory: {}",
                repository.worktree.display()
            )));
        }

        let canonical_worktree = repository.worktree.canonicalize()?;
        if canonical_worktree.parent() != canonical_root.as_deref() {
            return Err(message(format!(
                "worktree is outside the dock root: {}",
                repository.worktree.display()
            )));
        }
        let actual_root =
            PathBuf::from(git(&repository.worktree, ["rev-parse", "--show-toplevel"])?);
        if actual_root.canonicalize()? != canonical_worktree {
            return Err(message(format!(
                "worktree path resolves to another repository: {}",
                repository.worktree.display()
            )));
        }
        let actual_branch = git(&repository.worktree, ["branch", "--show-current"])?;
        if actual_branch != record.branch {
            return Err(message(format!(
                "{} uses branch {actual_branch}, expected {}",
                repository.name, record.branch
            )));
        }
        if !git(
            &repository.worktree,
            [
                "status",
                "--porcelain",
                "--untracked-files=all",
                "--ignore-submodules=none",
            ],
        )?
        .is_empty()
        {
            return Err(message(format!(
                "{} has uncommitted or untracked changes",
                repository.name
            )));
        }
        if !git(
            &repository.worktree,
            ["ls-files", "--others", "--ignored", "--exclude-standard"],
        )?
        .is_empty()
        {
            return Err(message(format!("{} has ignored files", repository.name)));
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
    Ok(has_worktrees)
}
pub(crate) fn archive_dock(record: &DockRecord, close_workspace: bool) -> Result<()> {
    if preflight_archive(record)? {
        record.worktree_manager.ensure_available()?;
    }
    if close_workspace {
        herdr(&["workspace", "close", &record.workspace_id])?;
        preflight_archive(record)?;
    }

    let mut guides = Vec::new();
    if record.root.exists() {
        for name in ["AGENTS.md", "CLAUDE.md"] {
            let path = record.root.join(name);
            if path.is_file() {
                guides.push((path.clone(), fs::read(path)?));
            }
        }
    }

    let restore = |removed: &[&DockRepository]| {
        let mut errors = Vec::new();
        for repository in removed.iter().rev() {
            if let Err(error) = record.worktree_manager.create(
                &repository.source,
                &repository.worktree,
                &record.branch,
                &repository.base_ref,
                true,
            ) {
                errors.push(error.to_string());
            }
        }
        errors
    };
    let mut removed: Vec<&DockRepository> = Vec::new();
    for repository in &record.repositories {
        if repository.worktree.is_dir() {
            if let Err(error) = record
                .worktree_manager
                .remove(&repository.source, &repository.worktree)
            {
                return Err(with_cleanup_errors(error, restore(&removed)));
            }
            removed.push(repository);
        }
    }

    let cleanup = (|| -> Result<()> {
        if record.root.exists() {
            for guide in ["AGENTS.md", "CLAUDE.md"] {
                let path = record.root.join(guide);
                if path.exists() {
                    fs::remove_file(path)?;
                }
            }
            fs::remove_dir(&record.root)?;
        }
        Ok(())
    })();
    if let Err(error) = cleanup {
        let mut restore_errors = restore(&removed);
        for (path, contents) in guides {
            if !path.exists()
                && let Err(restore_error) = fs::write(&path, contents)
            {
                restore_errors.push(restore_error.to_string());
            }
        }
        return Err(with_cleanup_errors(error, restore_errors));
    }
    Ok(())
}
pub(crate) fn registered_worktree(
    source: &Path,
    worktree: &Path,
    branch: &str,
) -> Result<Option<PathBuf>> {
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
pub(crate) fn sync_dock_agents(
    records: &mut [DockRecord],
    live: &BTreeMap<String, LiveWorkspace>,
    current_session: Option<&str>,
) -> bool {
    let mut changed = false;
    for record in records.iter_mut().filter(|record| {
        record.archived_at_unix.is_none()
            && current_session.is_some()
            && record.herdr_session.as_deref() == current_session
    }) {
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
pub(crate) fn check_dock_session(record: &DockRecord, current: Option<&str>) -> Result<()> {
    let Some(expected) = record.herdr_session.as_deref() else {
        return Err(message(
            "dock belongs to an unknown legacy Herdr session; close its old workspace before reopening it",
        ));
    };
    if current != Some(expected) {
        return Err(message(format!(
            "dock belongs to Herdr session {expected}; reopen it from that session"
        )));
    }
    Ok(())
}
pub(crate) fn reopen_dock(
    state: &mut State,
    index: usize,
    state_path: &Path,
    current_session: Option<&str>,
) -> Result<Vec<String>> {
    if state.docks[index].herdr_session.is_none() {
        if current_session.is_none() {
            return Err(message(
                "cannot reopen legacy dock without a current Herdr session",
            ));
        }
        ensure_no_live_legacy_workspace(&state.docks[index])?;
    } else {
        check_dock_session(&state.docks[index], current_session)?;
    }
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
pub(crate) fn create_dock() -> Result<()> {
    let config_dir = required_directory("HERDR_PLUGIN_CONFIG_DIR")?;
    let state_dir = required_directory("HERDR_PLUGIN_STATE_DIR")?;
    let config_path = config_dir.join("config.toml");
    let state_path = state_dir.join("state.json");
    let _state_lock = lock_state(&state_path)?;
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
        let mut base_for_all: Option<String> = None;
        for repository in selected {
            let refs = git_refs(&repository.path)?;
            if let Some(base_ref) = &base_for_all
                && refs.contains(base_ref)
            {
                plans.push(RepositoryPlan {
                    repository,
                    base_ref: base_ref.clone(),
                });
                continue;
            }
            base_for_all = None;
            let remembered = state.base_refs.get(&repository_key(&repository.path));
            let initial = remembered
                .filter(|value| refs.contains(value))
                .cloned()
                .or_else(|| default_base_ref(&repository.path, &refs))
                .unwrap_or_else(|| "HEAD".into());
            let Some(choice) = prompt_base_ref(&mut ui, &repository.name, &refs, &initial)? else {
                return Ok(());
            };
            match choice {
                BaseRefChoice::One(base_ref) => plans.push(RepositoryPlan {
                    repository,
                    base_ref,
                }),
                BaseRefChoice::All(base_ref) => {
                    plans.push(RepositoryPlan {
                        repository,
                        base_ref: base_ref.clone(),
                    });
                    base_for_all = Some(base_ref);
                }
            }
        }
        (name, plans, preset, recent)
    };

    let (name, plans, preset, recent) = selection;
    let slug = slugify(&name);
    let branch = format!("{}/{}", config.branch_prefix, slug);
    check_branch_name(&branch)?;
    let root = worktree_root.join(&slug);

    {
        let mut ui = Ui::start()?;
        if !confirm_create(&mut ui, &name, &branch, &root, &plans)? {
            return Ok(());
        }
    }

    println!("Creating {branch} in {}...", root.display());
    let worktrees = materialize_worktrees(config.worktree_manager, &root, &name, &branch, &plans)?;
    let created = plans
        .iter()
        .zip(&worktrees)
        .map(|(plan, worktree)| (plan.repository.path.clone(), worktree.clone()))
        .collect::<Vec<_>>();
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
    let workspace = match open_workspace(&name, &dock_tabs) {
        Ok(workspace) => workspace,
        Err(error) => {
            return Err(with_cleanup_errors(
                error,
                cleanup_materialized_worktrees(config.worktree_manager, &root, &created),
            ));
        }
    };

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
        completed_at_unix: None,
        archived_at_unix: None,
        worktree_manager: config.worktree_manager,
        agents: Vec::new(),
        tabs: dock_tabs,
        repositories: dock_repositories,
    });
    if let Err(error) = save_state(&state_path, &state) {
        state.docks.pop();
        let cleanup_errors = match herdr(&["workspace", "close", &workspace.id]) {
            Ok(_) => cleanup_materialized_worktrees(config.worktree_manager, &root, &created),
            Err(close_error) => vec![format!("could not close workspace: {close_error}")],
        };
        return Err(with_cleanup_errors(error, cleanup_errors));
    }
    herdr(&["workspace", "focus", &workspace.id])?;
    println!("Created dock {name}.");
    Ok(())
}
pub(crate) fn cleanup_materialized_worktrees(
    manager: WorktreeManager,
    root: &Path,
    created: &[(PathBuf, PathBuf)],
) -> Vec<String> {
    let mut errors = Vec::new();
    for (repository, worktree) in created.iter().rev() {
        if worktree.exists()
            && let Err(error) = manager.remove(repository, worktree)
        {
            errors.push(error.to_string());
        }
    }
    if errors.is_empty() {
        for guide in ["AGENTS.md", "CLAUDE.md"] {
            let path = root.join(guide);
            if path.exists()
                && let Err(error) = fs::remove_file(&path)
            {
                errors.push(format!("could not remove {}: {error}", path.display()));
            }
        }
        if errors.is_empty()
            && root.exists()
            && let Err(error) = fs::remove_dir(root)
        {
            errors.push(format!("could not remove {}: {error}", root.display()));
        }
    }
    errors
}
pub(crate) fn with_cleanup_errors(
    error: Box<dyn Error>,
    cleanup_errors: Vec<String>,
) -> Box<dyn Error> {
    if cleanup_errors.is_empty() {
        error
    } else {
        message(format!(
            "{error}; cleanup incomplete: {}",
            cleanup_errors.join("; ")
        ))
    }
}
pub(crate) fn materialize_worktrees(
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
        return Err(with_cleanup_errors(
            error,
            cleanup_materialized_worktrees(manager, root, &created),
        ));
    }
    Ok(created.into_iter().map(|(_, path)| path).collect())
}
pub(crate) fn write_agent_guides(
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
pub(crate) fn default_dock_tabs(root: &Path, repositories: &[DockRepository]) -> Vec<DockTab> {
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
