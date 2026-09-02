use crate::{checked, git, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs, io,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum WorktreeManager {
    #[default]
    Git,
    Worktrunk,
}

impl WorktreeManager {
    pub(crate) fn ensure_available(self) -> Result<()> {
        if self == Self::Worktrunk {
            let output = Command::new("wt")
                .arg("--version")
                .output()
                .map_err(|error| {
                    io::Error::new(
                        error.kind(),
                        format!(
                        "could not run Worktrunk; install `wt` from https://worktrunk.dev/: {error}"
                    ),
                    )
                })?;
            if !output.status.success() {
                return Err(io::Error::other(format!(
                    "Worktrunk is unavailable: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                ))
                .into());
            }
        }
        Ok(())
    }
    pub(crate) fn create(
        self,
        source: &Path,
        destination: &Path,
        branch: &str,
        base_ref: &str,
        branch_exists: bool,
    ) -> Result<()> {
        match self {
            Self::Git => {
                checked(&mut git_create_command(
                    source,
                    destination,
                    branch,
                    base_ref,
                    branch_exists,
                ))?;
            }
            Self::Worktrunk => run_worktrunk(&mut worktrunk_create_command(
                source,
                destination,
                branch,
                base_ref,
                branch_exists,
            ))?,
        }
        verify_worktree(destination, branch)
    }

    pub(crate) fn remove(self, source: &Path, worktree: &Path) -> Result<()> {
        match self {
            Self::Git => {
                checked(
                    Command::new("git")
                        .arg("-C")
                        .arg(source)
                        .args(["worktree", "remove", "--"])
                        .arg(worktree),
                )?;
            }
            Self::Worktrunk => run_worktrunk(&mut worktrunk_remove_command(source, worktree))?,
        }
        Ok(())
    }
}

fn git_create_command(
    source: &Path,
    destination: &Path,
    branch: &str,
    base_ref: &str,
    branch_exists: bool,
) -> Command {
    let mut command = Command::new("git");
    command.arg("-C").arg(source).args(["worktree", "add"]);
    if branch_exists {
        command.arg(destination).arg(branch);
    } else {
        command.args(["-b", branch]).arg(destination).arg(base_ref);
    }
    command
}

fn worktrunk_create_command(
    source: &Path,
    destination: &Path,
    branch: &str,
    base_ref: &str,
    branch_exists: bool,
) -> Command {
    let mut command = Command::new("wt");
    command
        .arg("-C")
        .arg(source)
        .env("WORKTRUNK_WORKTREE_PATH", destination)
        .arg("switch");
    if branch_exists {
        command.arg(branch);
    } else {
        command.args(["--create", branch, "--base", base_ref]);
    }
    command.arg("--no-cd");
    command
}

fn worktrunk_remove_command(source: &Path, worktree: &Path) -> Command {
    let mut command = Command::new("wt");
    command
        .arg("-C")
        .arg(source)
        .args(["remove", "--no-delete-branch", "--foreground"])
        .arg(worktree);
    command
}

fn run_worktrunk(command: &mut Command) -> Result<()> {
    let description = format!("{command:?}");
    let status = command.status().map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("could not run Worktrunk; install `wt` from https://worktrunk.dev/: {error}"),
        )
    })?;
    if !status.success() {
        return Err(io::Error::other(format!(
            "Worktrunk command failed ({status}): {description}"
        ))
        .into());
    }
    Ok(())
}

fn verify_worktree(destination: &Path, branch: &str) -> Result<()> {
    let expected = fs::canonicalize(destination).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!(
                "worktree manager did not create {}: {error}",
                destination.display()
            ),
        )
    })?;
    let actual = PathBuf::from(git(destination, ["rev-parse", "--show-toplevel"])?);
    if fs::canonicalize(&actual)? != expected {
        return Err(io::Error::other(format!(
            "worktree manager created {}, expected {}",
            actual.display(),
            destination.display()
        ))
        .into());
    }
    let actual_branch = git(destination, ["branch", "--show-current"])?;
    if actual_branch != branch {
        return Err(io::Error::other(format!(
            "worktree at {} uses branch {actual_branch}, expected {branch}",
            destination.display()
        ))
        .into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn worktrunk_commands_keep_paths_and_branches_explicit() {
        let create = worktrunk_create_command(
            Path::new("/repos/api"),
            Path::new("/docks/oauth/api"),
            "agent/oauth",
            "main",
            false,
        );
        assert_eq!(create.get_program(), OsStr::new("wt"));
        assert_eq!(
            create.get_args().collect::<Vec<_>>(),
            [
                "-C",
                "/repos/api",
                "switch",
                "--create",
                "agent/oauth",
                "--base",
                "main",
                "--no-cd",
            ]
            .map(OsStr::new)
        );
        assert!(create.get_envs().any(|(key, value)| {
            key == OsStr::new("WORKTRUNK_WORKTREE_PATH")
                && value == Some(OsStr::new("/docks/oauth/api"))
        }));

        let remove =
            worktrunk_remove_command(Path::new("/repos/api"), Path::new("/docks/oauth/api"));
        assert_eq!(
            remove.get_args().collect::<Vec<_>>(),
            [
                "-C",
                "/repos/api",
                "remove",
                "--no-delete-branch",
                "--foreground",
                "/docks/oauth/api",
            ]
            .map(OsStr::new)
        );
    }

    #[test]
    fn real_worktrunk_lifecycle_when_enabled() -> Result<()> {
        if std::env::var_os("HERDR_DOCK_TEST_WORKTRUNK").is_none() {
            return Ok(());
        }

        let temporary = std::env::temp_dir().join(format!(
            "herdr-dock-worktrunk-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        let source = temporary.join("source");
        let dock_root = temporary.join("dock");
        let destination = dock_root.join("source");
        fs::create_dir_all(&source)?;
        checked(Command::new("git").arg("init").arg("--quiet").arg(&source))?;
        fs::write(source.join("README.md"), "test\n")?;
        checked(
            Command::new("git")
                .arg("-C")
                .arg(&source)
                .args(["add", "."]),
        )?;
        checked(Command::new("git").arg("-C").arg(&source).args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "--quiet",
            "-m",
            "initial",
        ]))?;
        fs::create_dir_all(&dock_root)?;

        WorktreeManager::Worktrunk.create(
            &source,
            &destination,
            "agent/worktrunk",
            "HEAD",
            false,
        )?;
        WorktreeManager::Worktrunk.remove(&source, &destination)?;
        assert!(!destination.exists());

        WorktreeManager::Worktrunk.create(
            &source,
            &destination,
            "agent/worktrunk",
            "HEAD",
            true,
        )?;
        WorktreeManager::Worktrunk.remove(&source, &destination)?;
        assert!(!destination.exists());
        checked(Command::new("git").arg("-C").arg(&source).args([
            "rev-parse",
            "--verify",
            "refs/heads/agent/worktrunk",
        ]))?;
        fs::remove_dir_all(temporary)?;
        Ok(())
    }
}
