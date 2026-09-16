use crate::Result;
use crate::git::{check_branch_name, default_base_ref, git_refs, local_branch_exists, message};
use crate::herdr::{add_workspace_tab, current_herdr_session, herdr, open_workspace};
use crate::model::{
    DockRecord, DockRepository, DockTab, Repository, RepositoryPlan, State, load_state, lock_state,
    save_state,
};
use crate::prompts::{
    prompt_base_ref, prompt_branch, prompt_directory, prompt_goal, prompt_name,
    prompt_repositories, upsert_preset,
};
use crate::repos::{
    expand_home, load_config, load_repositories, merge_recent_repositories, remember_repository,
    repository_key, required_directory, write_search_root,
};
use crate::ui::{
    BaseRefChoice, Ui, confirm_add_repositories, confirm_create, confirm_hotkeys, show_notice,
    slugify,
};
use crate::worktrees::WorktreeManager;
use std::{
    error::Error,
    fs,
    path::{Path, PathBuf},
};

pub(crate) fn create_dock() -> Result<()> {
    let config_dir = required_directory("HERDR_PLUGIN_CONFIG_DIR")?;
    let state_dir = required_directory("HERDR_PLUGIN_STATE_DIR")?;
    let config_path = config_dir.join("config.toml");
    let state_path = state_dir.join("state.json");
    let _state_lock = lock_state(&state_path)?;
    let mut config = load_config(&config_path)?;
    let mut state = load_state(&state_path)?;
    if state.docks.is_empty() && !crate::setup::hotkeys_configured()? {
        let install = {
            let mut ui = Ui::start()?;
            confirm_hotkeys(&mut ui)?
        };
        if install {
            crate::setup::setup()?;
        }
    }
    let herdr_session = current_herdr_session()?;
    let mut repositories = load_repositories(&config.repositories)?;
    merge_recent_repositories(&mut repositories, &state.recent_repositories);
    if repositories.is_empty() && config.repository_search_roots.is_empty() {
        let root = {
            let mut ui = Ui::start()?;
            prompt_directory(&mut ui, "Create dock · setup · repository search root")?
        };
        let Some(root) = root else {
            return Ok(());
        };
        write_search_root(&config_path, &root)?;
        config = load_config(&config_path)?;
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
        let Some(goal) = prompt_goal(&mut ui, "Create dock · 2/6 goal", "")? else {
            return Ok(());
        };
        let default_branch = format!("{}/{}", config.branch_prefix, slugify(&name));
        let Some(branch) = prompt_branch(&mut ui, &default_branch)? else {
            return Ok(());
        };
        check_branch_name(&branch)?;
        let Some((selected, preset, recent)) = prompt_repositories(
            &mut ui,
            "Create dock · 4/6 repositories",
            &repositories,
            &state.presets,
            &config.repository_search_roots,
            &[],
        )?
        else {
            return Ok(());
        };
        let Some(plans) = prompt_repository_plans(
            &mut ui,
            "Create dock · 5/6",
            &branch,
            selected,
            &state.base_refs,
        )?
        else {
            return Ok(());
        };
        (
            name,
            (!goal.is_empty()).then_some(goal),
            branch,
            plans,
            preset,
            recent,
        )
    };

    let (name, goal, branch, plans, preset, recent) = selection;
    let slug = slugify(&name);
    let root = worktree_root.join(&slug);

    {
        let mut ui = Ui::start()?;
        if !confirm_create(&mut ui, &name, goal.as_deref(), &branch, &root, &plans)? {
            return Ok(());
        }
    }

    println!("Creating {branch} in {}...", root.display());
    let worktrees = materialize_worktrees(
        config.worktree_manager,
        &root,
        &name,
        goal.as_deref(),
        &branch,
        &plans,
    )?;
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
        if plan.base_ref != branch {
            state
                .base_refs
                .insert(repository_key(&plan.repository.path), plan.base_ref.clone());
        }
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
        goal,
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
fn prompt_repository_plans(
    ui: &mut Ui,
    title: &str,
    branch: &str,
    selected: Vec<crate::model::Repository>,
    base_refs: &std::collections::BTreeMap<String, String>,
) -> Result<Option<Vec<RepositoryPlan>>> {
    let mut plans = Vec::with_capacity(selected.len());
    let mut base_for_all: Option<String> = None;
    for repository in selected {
        if local_branch_exists(&repository.path, branch)? {
            plans.push(RepositoryPlan {
                repository,
                base_ref: branch.into(),
            });
            continue;
        }
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
        let initial = base_refs
            .get(&repository_key(&repository.path))
            .filter(|value| refs.contains(value))
            .cloned()
            .or_else(|| default_base_ref(&repository.path, &refs))
            .unwrap_or_else(|| "HEAD".into());
        let chosen = plans
            .iter()
            .filter(|plan| plan.base_ref != branch)
            .map(|plan| (plan.repository.name.clone(), plan.base_ref.clone()))
            .collect::<Vec<_>>();
        let Some(choice) = prompt_base_ref(ui, title, &repository.name, &refs, &initial, &chosen)?
        else {
            return Ok(None);
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
    Ok(Some(plans))
}
pub(crate) fn add_repositories_to_dock(
    state: &mut crate::model::State,
    index: usize,
    state_path: &Path,
    open: bool,
) -> Result<Option<usize>> {
    let config_dir = required_directory("HERDR_PLUGIN_CONFIG_DIR")?;
    let config = load_config(&config_dir.join("config.toml"))?;
    let mut repositories = load_repositories(&config.repositories)?;
    merge_recent_repositories(&mut repositories, &state.recent_repositories);
    let excluded = state.docks[index]
        .repositories
        .iter()
        .map(|repository| repository.source.clone())
        .collect::<Vec<_>>();

    let (plans, preset, recent) = {
        let mut ui = Ui::start()?;
        let Some((selected, preset, recent)) = prompt_repositories(
            &mut ui,
            "Add repositories · 1/3 select",
            &repositories,
            &state.presets,
            &config.repository_search_roots,
            &excluded,
        )?
        else {
            return Ok(None);
        };
        if let Some(repository) = selected.iter().find(|repository| {
            state.docks[index]
                .repositories
                .iter()
                .any(|existing| existing.name == repository.name)
        }) {
            show_notice(
                &mut ui,
                "Repository name conflict",
                &format!(
                    "{} is already used in this dock; configure an explicit name",
                    repository.name
                ),
            )?;
            return Ok(None);
        }
        let Some(plans) = prompt_repository_plans(
            &mut ui,
            "Add repositories · 2/3",
            &state.docks[index].branch,
            selected,
            &state.base_refs,
        )?
        else {
            return Ok(None);
        };
        if !confirm_add_repositories(
            &mut ui,
            &state.docks[index].name,
            &state.docks[index].branch,
            &plans,
        )? {
            return Ok(None);
        }
        (plans, preset, recent)
    };

    let manager = state.docks[index].worktree_manager;
    let branch = state.docks[index].branch.clone();
    let root = state.docks[index].root.clone();
    let created = materialize_added_worktrees(manager, &root, &branch, &plans)?;

    let tabs = plans
        .iter()
        .map(|plan| DockTab {
            label: plan.repository.name.clone(),
            cwd: root.join(&plan.repository.name),
        })
        .collect::<Vec<_>>();
    let mut tab_ids = Vec::new();
    if open {
        for tab in &tabs {
            match add_workspace_tab(&state.docks[index].workspace_id, tab) {
                Ok(tab_id) => tab_ids.push(tab_id),
                Err(error) => {
                    return Err(with_cleanup_errors(
                        error,
                        cleanup_added_resources(manager, &created, &tab_ids),
                    ));
                }
            }
        }
    }

    let mut all_plans = state.docks[index]
        .repositories
        .iter()
        .map(|repository| RepositoryPlan {
            repository: crate::model::Repository {
                name: repository.name.clone(),
                path: repository.source.clone(),
            },
            base_ref: repository.base_ref.clone(),
        })
        .collect::<Vec<_>>();
    all_plans.extend(plans.iter().cloned());
    let mut guides = Vec::new();
    for name in ["AGENTS.md", "CLAUDE.md"] {
        let path = root.join(name);
        let contents = match fs::read(&path) {
            Ok(contents) => Some(contents),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(with_cleanup_errors(
                    error.into(),
                    cleanup_added_resources(manager, &created, &tab_ids),
                ));
            }
        };
        guides.push((path, contents));
    }
    if let Err(error) = write_agent_guides(
        &root,
        &state.docks[index].name,
        state.docks[index].goal.as_deref(),
        &branch,
        &all_plans,
    ) {
        let mut cleanup = restore_guides(&guides);
        cleanup.extend(cleanup_added_resources(manager, &created, &tab_ids));
        return Err(with_cleanup_errors(error, cleanup));
    }

    let previous_base_refs = state.base_refs.clone();
    let previous_recent = state.recent_repositories.clone();
    let previous_presets = state.presets.clone();
    let repository_count = state.docks[index].repositories.len();
    let previous_tabs = state.docks[index].tabs.clone();
    if state.docks[index].tabs.is_empty() {
        state.docks[index].tabs = default_dock_tabs(&root, &state.docks[index].repositories);
    }
    for (plan, tab) in plans.iter().zip(&tabs) {
        state.docks[index].repositories.push(DockRepository {
            name: plan.repository.name.clone(),
            source: plan.repository.path.clone(),
            worktree: tab.cwd.clone(),
            base_ref: plan.base_ref.clone(),
        });
        state.docks[index].tabs.push(tab.clone());
        if plan.base_ref != branch {
            state
                .base_refs
                .insert(repository_key(&plan.repository.path), plan.base_ref.clone());
        }
    }
    for repository in recent {
        remember_repository(&mut state.recent_repositories, repository);
    }
    if let Some(preset) = preset {
        upsert_preset(&mut state.presets, preset);
    }
    if let Err(error) = save_state(state_path, state) {
        state.docks[index].repositories.truncate(repository_count);
        state.docks[index].tabs = previous_tabs;
        state.base_refs = previous_base_refs;
        state.recent_repositories = previous_recent;
        state.presets = previous_presets;
        let mut cleanup = restore_guides(&guides);
        cleanup.extend(cleanup_added_resources(manager, &created, &tab_ids));
        return Err(with_cleanup_errors(error, cleanup));
    }
    Ok(Some(plans.len()))
}
pub(crate) fn materialize_added_worktrees(
    manager: WorktreeManager,
    root: &Path,
    branch: &str,
    plans: &[RepositoryPlan],
) -> Result<Vec<(PathBuf, PathBuf)>> {
    manager.ensure_available()?;
    let mut created = Vec::new();
    for plan in plans {
        let destination = root.join(&plan.repository.name);
        if destination.exists() {
            return Err(with_cleanup_errors(
                message(format!(
                    "worktree already exists: {}",
                    destination.display()
                )),
                cleanup_added_resources(manager, &created, &[]),
            ));
        }
        let branch_exists = match local_branch_exists(&plan.repository.path, branch) {
            Ok(exists) => exists,
            Err(error) => {
                return Err(with_cleanup_errors(
                    error,
                    cleanup_added_resources(manager, &created, &[]),
                ));
            }
        };
        println!(
            "  {} {}...",
            if branch_exists { "Reusing" } else { "Creating" },
            plan.repository.name
        );
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
            return Err(with_cleanup_errors(
                error,
                cleanup_added_resources(manager, &created, &[]),
            ));
        }
        created.push((plan.repository.path.clone(), destination));
    }
    Ok(created)
}

pub(crate) fn cleanup_added_resources(
    manager: WorktreeManager,
    created: &[(PathBuf, PathBuf)],
    tab_ids: &[String],
) -> Vec<String> {
    let mut errors = Vec::new();
    for tab_id in tab_ids.iter().rev() {
        if let Err(error) = herdr(&["tab", "close", tab_id]) {
            errors.push(format!("could not close tab {tab_id}: {error}"));
        }
    }
    for (repository, worktree) in created.iter().rev() {
        if worktree.exists()
            && let Err(error) = manager.remove(repository, worktree)
        {
            errors.push(error.to_string());
        }
    }
    errors
}

fn restore_guides(guides: &[(PathBuf, Option<Vec<u8>>)]) -> Vec<String> {
    guides
        .iter()
        .filter_map(|(path, contents)| {
            let result = match contents {
                Some(contents) => fs::write(path, contents),
                None => fs::remove_file(path),
            };
            result
                .err()
                .filter(|error| contents.is_some() || error.kind() != std::io::ErrorKind::NotFound)
                .map(|error| format!("could not restore {}: {error}", path.display()))
        })
        .collect()
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
    goal: Option<&str>,
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
            let branch_exists = local_branch_exists(&plan.repository.path, branch)?;
            println!(
                "  {} {}...",
                if branch_exists { "Reusing" } else { "Creating" },
                plan.repository.name
            );
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
        write_agent_guides(root, name, goal, branch, plans)?;
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

fn write_dock_agent_guides(record: &DockRecord) -> Result<()> {
    let plans = record
        .repositories
        .iter()
        .map(|repository| RepositoryPlan {
            repository: Repository {
                name: repository.name.clone(),
                path: repository.source.clone(),
            },
            base_ref: repository.base_ref.clone(),
        })
        .collect::<Vec<_>>();
    write_agent_guides(
        &record.root,
        &record.name,
        record.goal.as_deref(),
        &record.branch,
        &plans,
    )
}

pub(crate) fn update_dock_goal(
    state: &mut State,
    index: usize,
    state_path: &Path,
    goal: Option<String>,
) -> Result<()> {
    if state.docks[index].goal == goal {
        return Ok(());
    }
    let previous = std::mem::replace(&mut state.docks[index].goal, goal);
    if let Err(error) = write_dock_agent_guides(&state.docks[index]) {
        state.docks[index].goal = previous;
        let cleanup_errors = write_dock_agent_guides(&state.docks[index])
            .err()
            .map(|error| format!("could not restore agent guides: {error}"))
            .into_iter()
            .collect();
        return Err(with_cleanup_errors(error, cleanup_errors));
    }
    if let Err(error) = save_state(state_path, state) {
        state.docks[index].goal = previous;
        let cleanup_errors = write_dock_agent_guides(&state.docks[index])
            .err()
            .map(|error| format!("could not restore agent guides: {error}"))
            .into_iter()
            .collect();
        return Err(with_cleanup_errors(error, cleanup_errors));
    }
    Ok(())
}
pub(crate) fn write_agent_guides(
    root: &Path,
    name: &str,
    goal: Option<&str>,
    branch: &str,
    plans: &[RepositoryPlan],
) -> Result<()> {
    let repositories = plans
        .iter()
        .map(|plan| {
            if plan.base_ref == branch {
                format!(
                    "- `{}`: `{}` (reused existing branch)",
                    plan.repository.name,
                    root.join(&plan.repository.name).display()
                )
            } else {
                format!(
                    "- `{}`: `{}` (created from `{}`)",
                    plan.repository.name,
                    root.join(&plan.repository.name).display(),
                    plan.base_ref
                )
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let goal = goal
        .map(|goal| format!("## Goal\n\n{goal}\n\n"))
        .unwrap_or_default();
    let guide = format!(
        "# {name}\n\n\
This directory is a Herdr dock that groups related Git worktrees.\n\
All repositories use branch `{branch}`. Work inside the repository directories, not this root.\n\n\
{goal}\
## Repositories\n\n\
{repositories}\n\n\
## Sessions\n\n\
The agent in `root` is the root session. It can use Herdr to create or focus tabs and panes, start child agents in any repository, and list, prompt, read, wait for, or focus those agents.\n\
Repository tabs provide ready working directories; they do not assign roles or prescribe a delegation structure. Use child sessions only when useful.\n"
    );
    fs::write(root.join("AGENTS.md"), &guide)?;
    fs::write(root.join("CLAUDE.md"), guide)?;
    Ok(())
}
pub(crate) fn default_dock_tabs(root: &Path, repositories: &[DockRepository]) -> Vec<DockTab> {
    let mut tabs = vec![DockTab {
        label: "root".into(),
        cwd: root.to_path_buf(),
    }];
    tabs.extend(repositories.iter().map(|repository| DockTab {
        label: repository.name.clone(),
        cwd: repository.worktree.clone(),
    }));
    tabs
}
