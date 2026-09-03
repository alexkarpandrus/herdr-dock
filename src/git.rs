use crate::Result;
use std::{
    error::Error,
    io,
    path::Path,
    process::{Command, Output, Stdio},
};

pub(crate) fn check_branch_name(branch: &str) -> Result<()> {
    checked(
        Command::new("git")
            .args(["check-ref-format", "--branch", branch])
            .stdout(Stdio::null()),
    )?;
    Ok(())
}
pub(crate) fn git_refs(repository: &Path) -> Result<Vec<String>> {
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
pub(crate) fn default_base_ref(repository: &Path, refs: &[String]) -> Option<String> {
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
pub(crate) fn git<const N: usize>(repository: &Path, arguments: [&str; N]) -> Result<String> {
    checked(
        Command::new("git")
            .arg("-C")
            .arg(repository)
            .args(arguments),
    )
}
pub(crate) fn optional_git<const N: usize>(
    repository: &Path,
    arguments: [&str; N],
) -> Option<String> {
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
pub(crate) fn checked(command: &mut Command) -> Result<String> {
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
pub(crate) fn message(value: impl Into<String>) -> Box<dyn Error> {
    io::Error::other(value.into()).into()
}
