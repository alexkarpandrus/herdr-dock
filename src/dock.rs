use crate::Result;
use crate::create::default_dock_tabs;
use crate::git::message;
use crate::herdr::{herdr, live_workspaces, open_workspace, resume_agents};
use crate::model::{DockAgent, DockRecord, DockTab, LiveWorkspace, State, save_state};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
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
