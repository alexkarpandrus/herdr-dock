use crate::Result;
use crate::create::with_cleanup_errors;
use crate::git::{checked, git, message};
use crate::herdr::herdr;
use crate::model::{DockRecord, DockRepository};
use std::{
    collections::BTreeSet,
    fs, io,
    path::{Path, PathBuf},
    process::Command,
};

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
