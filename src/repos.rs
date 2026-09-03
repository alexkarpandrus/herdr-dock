use crate::Result;
use crate::git::{git, message};
use crate::model::{Config, Repository, RepositoryConfig};
use std::{
    collections::BTreeSet,
    env, fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
};

pub(crate) fn required_directory(variable: &str) -> Result<PathBuf> {
    let path = env::var_os(variable).ok_or_else(|| message(format!("{variable} is not set")))?;
    let path = PathBuf::from(path);
    fs::create_dir_all(&path)?;
    Ok(path)
}
pub(crate) fn load_config(path: &Path) -> Result<Config> {
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
pub(crate) fn load_repositories(configured: &[RepositoryConfig]) -> Result<Vec<Repository>> {
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
pub(crate) fn load_repository(path: &Path, configured_name: Option<&str>) -> Result<Repository> {
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
pub(crate) fn merge_recent_repositories(repositories: &mut Vec<Repository>, recent: &[PathBuf]) {
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
pub(crate) fn remember_repository(recent: &mut Vec<PathBuf>, repository: PathBuf) {
    recent.retain(|existing| existing != &repository);
    recent.insert(0, repository);
}
pub(crate) fn discover_repositories_with_progress(
    search_roots: &[PathBuf],
    progress: Option<&AtomicUsize>,
) -> Result<Vec<Repository>> {
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
                if let Some(counter) = progress {
                    counter.fetch_add(1, Ordering::Relaxed);
                }
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
pub(crate) fn expand_home(path: &Path) -> Result<PathBuf> {
    let value = path.to_string_lossy();
    if value == "~" || value.starts_with("~/") {
        let home = env::var_os("HOME").ok_or_else(|| message("HOME is not set"))?;
        Ok(PathBuf::from(home).join(value.trim_start_matches("~/")))
    } else {
        Ok(path.to_owned())
    }
}
pub(crate) fn repository_key(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}
