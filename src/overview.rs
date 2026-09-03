use crate::Result;
use crate::archive::archive_dock;
use crate::dock::{
    check_dock_session, ensure_no_live_legacy_workspace, legacy_workspace_is_live, reopen_dock,
    sync_dock_agents,
};
use crate::git::{message, optional_git};
use crate::herdr::{current_herdr_session, herdr, live_workspaces};
use crate::model::{
    AgentOverview, DockOverview, DockRecord, LiveWorkspace, RepositoryOverview, State, load_state,
    lock_state, save_state,
};
use crate::repos::required_directory;
use crate::ui::{
    Segment, Ui, confirm_archive, confirm_complete, plain, read_key, show_notice, styled,
};
use crossterm::event::KeyCode;
use crossterm::style::Color;
use crossterm::terminal;
use std::{
    collections::BTreeMap,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

fn status_color(status: &str) -> Option<Color> {
    match status {
        "working" | "running" | "clean" => Some(Color::Green),
        "dirty" | "idle" | "session unknown" | "unavailable" => Some(Color::Yellow),
        "missing" => Some(Color::Red),
        "archived" | "done" | "closed" | "saved" => Some(Color::DarkGrey),
        _ => None,
    }
}

fn status_segment(status: &str) -> Segment {
    match status_color(status) {
        Some(color) => styled(color, status),
        None => plain(status),
    }
}

fn overview_hint(dock: &DockOverview) -> String {
    if dock.archived {
        "↑/↓ select · / filter · R refresh · Esc close".into()
    } else if dock.done {
        "↑/↓ select · Enter reopen · A archive · / filter · R refresh · Esc close".into()
    } else {
        "↑/↓ select · Enter focus/reopen · D close · A archive · / filter · R refresh · Esc close"
            .into()
    }
}
pub(crate) fn show_overview() -> Result<()> {
    let state_dir = required_directory("HERDR_PLUGIN_STATE_DIR")?;
    let state_path = state_dir.join("state.json");
    let _state_lock = lock_state(&state_path)?;
    let mut state = load_state(&state_path)?;
    if state.docks.is_empty() {
        return Err(message("no docks have been created yet"));
    }

    let current_session = current_herdr_session()?;
    let mut docks = collect_overview(&mut state, &state_path, current_session.as_deref())?;
    let mut cursor: usize = 0;
    let mut filter = String::new();
    let mut filtering = false;
    let mut ui = Ui::start()?;
    loop {
        let matches = docks
            .iter()
            .enumerate()
            .filter(|(_, dock)| {
                filter.is_empty() || dock.name.to_lowercase().contains(&filter.to_lowercase())
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        cursor = cursor.min(matches.len().saturating_sub(1));
        let height = terminal::size()?.1 as usize;
        let visible = (height / 3).max(1).min(matches.len().max(1));
        let start = cursor.saturating_sub(visible.saturating_sub(1));
        let mut lines: Vec<Vec<Segment>> = Vec::new();

        if matches.is_empty() {
            lines.push(vec![plain(format!("No docks match \"{filter}\"."))]);
        } else {
            lines.extend(matches[start..start + visible].iter().enumerate().map(
                |(offset, index)| {
                    let dock = &docks[*index];
                    let dirty = dock
                        .repositories
                        .iter()
                        .filter(|repository| repository.status == "dirty")
                        .count();
                    vec![
                        plain(if start + offset == cursor { ">" } else { " " }),
                        plain(format!(" {}", dock.name)),
                        plain(" ["),
                        status_segment(&dock.status),
                        plain(format!(
                            "] · {} tabs · {} repos · {} dirty · {} agents",
                            dock.tab_count,
                            dock.repositories.len(),
                            dirty,
                            dock.agents.len()
                        )),
                    ]
                },
            ));

            let dock = &docks[matches[cursor]];
            lines.push(vec![plain(String::new())]);
            lines.push(vec![plain(format!("Branch: {}", dock.branch))]);
            lines.push(vec![plain(format!("Root: {}", dock.root.display()))]);
            lines.push(vec![plain(format!(
                "Herdr session: {}",
                dock.herdr_session.as_deref().unwrap_or("current/legacy")
            ))]);
            lines.push(vec![plain(String::new())]);
            lines.push(vec![plain("Agents")]);
            if dock.agents.is_empty() {
                lines.push(vec![plain("  none")]);
            } else {
                for agent in &dock.agents {
                    let session = agent
                        .session
                        .as_ref()
                        .map(|session| session.value.as_str())
                        .unwrap_or("unavailable");
                    lines.push(vec![
                        plain(format!("  {}", agent.name)),
                        plain(format!(" ({}) [", agent.kind)),
                        status_segment(&agent.status),
                        plain(format!("] · {} · session {session}", agent.cwd)),
                    ]);
                }
            }
            lines.push(vec![plain(String::new())]);
            lines.push(vec![plain("Repositories")]);
            for repository in &dock.repositories {
                lines.push(vec![
                    plain(format!("  {}", repository.name)),
                    plain(" ["),
                    status_segment(&repository.status),
                    plain(format!("] · {}", repository.commit)),
                ]);
            }
            lines.truncate(height.saturating_sub(4));
        }

        lines.push(vec![plain(String::new())]);
        if filtering {
            lines.push(vec![plain(format!("Filter: {filter}_"))]);
        } else if !filter.is_empty() {
            lines.push(vec![styled(Color::Cyan, format!("Filter: {filter}"))]);
        }
        lines.push(vec![plain(if matches.is_empty() {
            "Esc back · type to filter".into()
        } else if filtering {
            "Type to filter · Enter done · Esc clear".into()
        } else {
            overview_hint(&docks[matches[cursor]])
        })]);

        ui.frame_styled("Dock overview", &lines)?;

        match read_key()?.code {
            KeyCode::Esc if filtering => {
                filtering = false;
                filter.clear();
                cursor = 0;
            }
            KeyCode::Esc if !filter.is_empty() => {
                filter.clear();
                cursor = 0;
            }
            KeyCode::Esc => return Ok(()),
            KeyCode::Char('/') if !filtering => filtering = true,
            KeyCode::Enter if filtering => filtering = false,
            KeyCode::Backspace if filtering => {
                filter.pop();
                cursor = 0;
            }
            KeyCode::Char(character) if filtering => {
                filter.push(character);
                cursor = 0;
            }
            KeyCode::Up => cursor = cursor.saturating_sub(1),
            KeyCode::Down if !matches.is_empty() => cursor = (cursor + 1).min(matches.len() - 1),
            KeyCode::Char('r') | KeyCode::Char('R') if !filtering => {
                docks = collect_overview(&mut state, &state_path, current_session.as_deref())?;
            }
            KeyCode::Char('d') | KeyCode::Char('D') if !filtering && !matches.is_empty() => {
                let dock = &docks[matches[cursor]];
                if dock.archived || dock.done {
                    continue;
                }
                let index = dock.record_index;
                let open = dock.open;
                if !confirm_complete(&mut ui, dock)? {
                    continue;
                }
                let completed_at = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
                drop(ui);
                let complete_result = (|| -> Result<()> {
                    check_dock_session(&state.docks[index], current_session.as_deref())?;
                    if open {
                        let live = live_workspaces()?;
                        if sync_dock_agents(&mut state.docks, &live, current_session.as_deref()) {
                            save_state(&state_path, &state)?;
                        }
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
                    Ok(()) => {
                        docks =
                            collect_overview(&mut state, &state_path, current_session.as_deref())?
                    }
                    Err(error) => show_notice(&mut ui, "Dock not closed", &error.to_string())?,
                }
            }
            KeyCode::Char('a') | KeyCode::Char('A') if !filtering && !matches.is_empty() => {
                let dock = &docks[matches[cursor]];
                if dock.archived {
                    continue;
                }
                let index = dock.record_index;
                let open = dock.open;
                if !confirm_archive(&mut ui, dock)? {
                    continue;
                }
                let archived_at = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
                drop(ui);
                let mut archive_result = (|| -> Result<()> {
                    ensure_no_live_legacy_workspace(&state.docks[index])?;
                    if open {
                        check_dock_session(&state.docks[index], current_session.as_deref())?;
                        archive_dock(&state.docks[index], true)
                    } else {
                        archive_dock(&state.docks[index], false)
                    }
                })();
                if archive_result.is_ok() {
                    state.docks[index].archived_at_unix = Some(archived_at);
                    if let Err(error) = save_state(&state_path, &state) {
                        state.docks[index].archived_at_unix = None;
                        archive_result = Err(error);
                    }
                }
                ui = Ui::start()?;
                match archive_result {
                    Ok(()) => {
                        docks =
                            collect_overview(&mut state, &state_path, current_session.as_deref())?
                    }
                    Err(error) => {
                        show_notice(&mut ui, "Dock not archived", &error.to_string())?;
                    }
                }
            }
            KeyCode::Enter if !filtering && !matches.is_empty() => {
                let dock = &docks[matches[cursor]];
                if dock.archived {
                    continue;
                }
                let index = dock.record_index;
                let open = dock.open;
                let workspace_id = dock.workspace_id.clone();
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
                        docks =
                            collect_overview(&mut state, &state_path, current_session.as_deref())?;
                    }
                }
            }
            _ => {}
        }
    }
}
pub(crate) fn collect_overview(
    state: &mut State,
    state_path: &Path,
    current_session: Option<&str>,
) -> Result<Vec<DockOverview>> {
    let live = live_workspaces()?;
    if sync_dock_agents(&mut state.docks, &live, current_session) {
        save_state(state_path, state)?;
    }
    Ok(build_overview(&state.docks, &live, current_session))
}
pub(crate) fn build_overview(
    records: &[DockRecord],
    live: &BTreeMap<String, LiveWorkspace>,
    current_session: Option<&str>,
) -> Vec<DockOverview> {
    records
        .iter()
        .enumerate()
        .rev()
        .map(|(record_index, record)| {
            let archived = record.archived_at_unix.is_some();
            let done = record.completed_at_unix.is_some();
            let workspace = (!archived
                && current_session.is_some()
                && record.herdr_session.as_deref() == current_session)
                .then(|| live.get(&record.workspace_id))
                .flatten();
            let legacy_workspace = !archived && legacy_workspace_is_live(record, live);
            let status = if archived {
                "archived".into()
            } else if legacy_workspace {
                "session unknown".into()
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
                record_index,
                name: record.name.clone(),
                branch: record.branch.clone(),
                root: record.root.clone(),
                workspace_id: record.workspace_id.clone(),
                herdr_session: record.herdr_session.clone(),
                status,
                open: workspace.is_some() || legacy_workspace,
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
