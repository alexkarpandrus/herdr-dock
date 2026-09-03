use crate::Result;
use crate::git::{check_branch_name, default_base_ref, git_refs, message};
use crate::herdr::{current_herdr_session, herdr, open_workspace};
use crate::model::{
    DockRecord, DockRepository, DockTab, RepositoryPlan, load_state, lock_state, save_state,
};
use crate::prompts::{
    prompt_base_ref, prompt_directory, prompt_name, prompt_repositories, upsert_preset,
};
use crate::repos::{
    expand_home, load_config, load_repositories, merge_recent_repositories, remember_repository,
    repository_key, required_directory, write_search_root,
};
use crate::ui::{BaseRefChoice, Ui, confirm_create, slugify};
use crate::worktrees::WorktreeManager;
use std::{
    error::Error,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

pub(crate) fn create_dock() -> Result<()> {
    let config_dir = required_directory("HERDR_PLUGIN_CONFIG_DIR")?;
    let state_dir = required_directory("HERDR_PLUGIN_STATE_DIR")?;
    let config_path = config_dir.join("config.toml");
    let state_path = state_dir.join("state.json");
    let _state_lock = lock_state(&state_path)?;
    let mut config = load_config(&config_path)?;
    let mut state = load_state(&state_path)?;
    let herdr_session = current_herdr_session()?;
    let mut repositories = load_repositories(&config.repositories)?;
    merge_recent_repositories(&mut repositories, &state.recent_repositories);
    if repositories.is_empty() && config.repository_search_roots.is_empty() {
        let root = {
            let mut ui = Ui::start()?;
            prompt_directory(&mut ui, "Create dock · add a repository search root")?
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
            let chosen = plans
                .iter()
                .map(|plan| (plan.repository.name.clone(), plan.base_ref.clone()))
                .collect::<Vec<_>>();
            let Some(choice) =
                prompt_base_ref(&mut ui, &repository.name, &refs, &initial, &chosen)?
            else {
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
