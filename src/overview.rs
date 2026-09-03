use crate::Result;
use crate::archive::archive_dock;
use crate::dock::{
    check_dock_session, ensure_no_live_legacy_workspace, legacy_workspace_is_live, reopen_dock,
    sync_dock_agents,
};
use crate::git::optional_git;
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
        "↑↓ move · ←→ column · / filter · R refresh · ? help · Esc close".into()
    } else if dock.done {
        "↑↓ move · ←→ column · Enter reopen · A archive · / filter · R refresh · ? help · Esc close"
            .into()
    } else {
        "↑↓ move · ←→ column · Enter focus/reopen · D close · A archive · / filter · R refresh · ? help · Esc close"
            .into()
    }
}

fn show_help(ui: &mut Ui) -> Result<()> {
    let lines = vec![
        vec![plain("Keys")],
        vec![plain(
            "  ↑↓ move ←→ column    Enter focus/reopen    D close    A archive",
        )],
        vec![plain(
            "  / filter      R refresh             Esc close  ? help",
        )],
        vec![plain(String::new())],
        vec![plain("Status colors")],
        vec![
            styled(Color::Green, "  working · clean"),
            plain("        "),
            styled(Color::Yellow, "dirty · idle · unavailable"),
        ],
        vec![
            styled(Color::Red, "  missing"),
            plain("          "),
            styled(Color::DarkGrey, "done · archived · closed · saved"),
        ],
        vec![plain(String::new())],
        vec![plain("Press any key to close.")],
    ];
    ui.frame_styled("Dock overview · help", &lines)?;
    read_key()?;
    Ok(())
}
/// The kanban lane a dock belongs to, in on-screen column order.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Lane {
    Working,
    Closed,
    Done,
    Archived,
}

fn lane_for(dock: &DockOverview) -> Lane {
    if dock.archived {
        Lane::Archived
    } else if dock.done {
        Lane::Done
    } else if dock.open {
        Lane::Working
    } else {
        Lane::Closed
    }
}

fn lane_color(lane: Lane) -> Color {
    match lane {
        Lane::Working => Color::Green,
        Lane::Closed => Color::Cyan,
        Lane::Done => Color::DarkGrey,
        Lane::Archived => Color::DarkGrey,
    }
}

fn lane_title(lane: Lane) -> &'static str {
    match lane {
        Lane::Working => "Working",
        Lane::Closed => "Closed",
        Lane::Done => "Done",
        Lane::Archived => "Archived",
    }
}
fn dirty_count(dock: &DockOverview) -> usize {
    dock.repositories
        .iter()
        .filter(|repository| repository.status == "dirty")
        .count()
}

/// Render the matched docks as a kanban board: one column per lane, one card per dock.
/// The lanes (in on-screen order) that actually contain a matched dock.
fn active_lanes(docks: &[DockOverview], matches: &[usize]) -> Vec<Lane> {
    let mut present = [false; 4];
    for index in matches {
        present[match lane_for(&docks[*index]) {
            Lane::Working => 0,
            Lane::Closed => 1,
            Lane::Done => 2,
            Lane::Archived => 3,
        }] = true;
    }
    let mut lanes = Vec::new();
    for (i, is) in present.iter().enumerate() {
        if *is {
            lanes.push(match i {
                0 => Lane::Working,
                1 => Lane::Closed,
                2 => Lane::Done,
                _ => Lane::Archived,
            });
        }
    }
    lanes
}

/// Flat cursor index into `matches` for a dock in `lane` at `rank` (0-based within the lane).
fn index_for_lane_rank(
    docks: &[DockOverview],
    matches: &[usize],
    lane: Lane,
    rank: usize,
) -> Option<usize> {
    let mut seen = 0;
    for (i, index) in matches.iter().enumerate() {
        if lane_for(&docks[*index]) == lane {
            if seen == rank {
                return Some(i);
            }
            seen += 1;
        }
    }
    None
}

fn cursor_rank(docks: &[DockOverview], matches: &[usize], cursor: usize) -> (usize, Lane) {
    let lane = lane_for(&docks[matches[cursor]]);
    let mut rank = 0;
    for (i, index) in matches.iter().enumerate() {
        if i >= cursor {
            break;
        }
        if lane_for(&docks[*index]) == lane {
            rank += 1;
        }
    }
    (rank, lane)
}

/// Move the flat cursor in a kanban-aware way: up/down within a lane, left/right across lanes.
fn move_cursor(docks: &[DockOverview], matches: &[usize], cursor: usize, dir: &str) -> usize {
    if matches.is_empty() {
        return 0;
    }
    let (rank, lane) = cursor_rank(docks, matches, cursor);
    match dir {
        "down" => {
            let lane_count = matches
                .iter()
                .filter(|index| lane_for(&docks[**index]) == lane)
                .count();
            if rank + 1 < lane_count {
                index_for_lane_rank(docks, matches, lane, rank + 1).unwrap_or(cursor)
            } else {
                cursor
            }
        }
        "up" => {
            if rank > 0 {
                index_for_lane_rank(docks, matches, lane, rank - 1).unwrap_or(cursor)
            } else {
                cursor
            }
        }
        "left" | "right" => {
            let lanes = active_lanes(docks, matches);
            let cur = lanes.iter().position(|l| *l == lane).unwrap_or(0);
            let last = lanes.len().saturating_sub(1);
            let target = if dir == "left" {
                cur.saturating_sub(1)
            } else {
                (cur + 1).min(last)
            };
            if target == cur {
                cursor
            } else {
                index_for_lane_rank(docks, matches, lanes[target], rank).unwrap_or(cursor)
            }
        }
        _ => cursor,
    }
}

fn board_lines(
    docks: &[DockOverview],
    matches: &[usize],
    cursor: usize,
    filter: &str,
) -> Vec<Vec<Segment>> {
    if matches.is_empty() {
        let message = if filter.is_empty() {
            "No docks yet.".to_string()
        } else {
            format!("No docks match \"{filter}\".")
        };
        return vec![vec![plain(message)]];
    }

    // Group matched docks by lane, preserving the flat matching order.
    let lanes = [Lane::Working, Lane::Closed, Lane::Done, Lane::Archived];
    let mut groups: [Vec<usize>; 4] = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
    for (i, index) in matches.iter().enumerate() {
        let lane = lane_for(&docks[*index]);
        let col = match lane {
            Lane::Working => 0,
            Lane::Closed => 1,
            Lane::Done => 2,
            Lane::Archived => 3,
        };
        groups[col].push(i);
    }

    let (term_cols, _) = terminal::size().unwrap_or((120, 40));
    let term_cols = term_cols as usize;
    let nlanes = groups.iter().filter(|g| !g.is_empty()).count().max(1);
    let col_width = (term_cols.saturating_sub(2) / nlanes).clamp(18, 34);

    // Build each lane into a block of lines: header + one card per dock.
    let mut lane_blocks: Vec<Vec<Vec<Segment>>> = Vec::new();
    let mut max_rows = 0usize;
    for (col, group) in groups.iter().enumerate() {
        let lane = lanes[col];
        let mut block: Vec<Vec<Segment>> = Vec::new();
        let count = group.len();
        block.push(vec![
            styled(lane_color(lane), format!(" {}", lane_title(lane))),
            plain(format!("  {count}")),
        ]);
        block.push(vec![plain(" ".repeat(col_width))]);
        for (pos, slot) in group.iter().enumerate() {
            let i = *slot;
            let dock = &docks[matches[i]];
            let selected = i == cursor;
            block.extend(card_lines(dock, col_width, selected));
            if pos + 1 < group.len() {
                block.push(vec![plain(" ".repeat(col_width))]);
            }
        }
        if block.len() > max_rows {
            max_rows = block.len();
        }
        lane_blocks.push(block);
    }

    // Lay the lanes side by side, padding shorter columns with blanks.
    let mut lines: Vec<Vec<Segment>> = Vec::new();
    for row in 0..max_rows {
        let mut line: Vec<Segment> = Vec::new();
        for (col, block) in lane_blocks.iter().enumerate() {
            if let Some(segments) = block.get(row) {
                line.extend(segments.iter().cloned());
            } else {
                line.push(plain(" ".repeat(col_width + 2)));
            }
            if col + 1 < lane_blocks.len() {
                line.push(styled(Color::DarkGrey, "│"));
            }
        }
        lines.push(line);
    }
    lines
}

fn card_lines(dock: &DockOverview, width: usize, selected: bool) -> Vec<Vec<Segment>> {
    let inner = width.saturating_sub(4);
    let accent = if selected {
        Color::Cyan
    } else {
        Color::DarkGrey
    };

    let name = truncate(&dock.name, inner.saturating_sub(2));
    let name_len = name.chars().count();
    let mut title: Vec<Segment> = vec![plain(if selected { "┌▶ " } else { "┌  " })];
    title.push(styled(accent, name));
    title.push(plain("─".repeat(inner.saturating_sub(name_len + 3))));
    title.push(plain("┐"));

    let status = truncate(&dock.status, inner.saturating_sub(8));
    let status_text = format!("{} · {} tabs", status, dock.tab_count);
    let mut status_row: Vec<Segment> = vec![plain("│ ")];
    status_row.extend([
        status_segment(&status),
        plain(format!(" · {} tabs", dock.tab_count)),
    ]);
    status_row.push(plain(
        " ".repeat(inner.saturating_sub(status_text.chars().count() + 1)),
    ));
    status_row.push(plain("│"));

    let dirty = dirty_count(dock);
    let agents = dock.agents.len();
    let summary = format!(
        "{} repos · {} dirty · {} agent{}",
        dock.repositories.len(),
        dirty,
        agents,
        if agents == 1 { "" } else { "s" }
    );
    let summary = truncate(&summary, inner.saturating_sub(1));
    let mut summary_row: Vec<Segment> = vec![plain("│ ")];
    summary_row.push(if dirty > 0 {
        styled(Color::Yellow, &summary)
    } else {
        plain(&summary)
    });
    summary_row.push(plain(
        " ".repeat(inner.saturating_sub(summary.chars().count() + 1)),
    ));
    summary_row.push(plain("│"));

    let branch = truncate(dock.branch.as_str(), inner.saturating_sub(1));
    let mut branch_row: Vec<Segment> = vec![plain("│ ")];
    branch_row.push(plain(&branch));
    branch_row.push(plain(
        " ".repeat(inner.saturating_sub(branch.chars().count() + 1)),
    ));
    branch_row.push(plain("│"));

    vec![
        title,
        status_row,
        summary_row,
        branch_row,
        vec![plain("└"), plain("─".repeat(inner)), plain("┘")],
    ]
}

/// Detail pane for the selected dock, shown below the kanban board.
fn detail_lines(dock: &DockOverview) -> Vec<Vec<Segment>> {
    let mut lines: Vec<Vec<Segment>> = Vec::new();
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
    lines
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{cut}…")
}

pub(crate) fn show_overview() -> Result<()> {
    let state_dir = required_directory("HERDR_PLUGIN_STATE_DIR")?;
    let state_path = state_dir.join("state.json");
    let _state_lock = lock_state(&state_path)?;
    let mut state = load_state(&state_path)?;
    if state.docks.is_empty() {
        let mut ui = Ui::start()?;
        ui.frame(
            "Dock overview",
            &[
                "No docks yet.".into(),
                String::new(),
                "Create one with prefix+d, or run:".into(),
                "  herdr plugin action invoke create --plugin herdr-dock".into(),
                String::new(),
                "Add the hotkeys with:".into(),
                "  herdr plugin action invoke setup --plugin herdr-dock".into(),
                String::new(),
                "Press any key to close.".into(),
            ],
        )?;
        read_key()?;
        return Ok(());
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
                filter.is_empty()
                    || dock.name.to_lowercase().contains(&filter.to_lowercase())
                    || dock.branch.to_lowercase().contains(&filter.to_lowercase())
                    || dock.status.to_lowercase().contains(&filter.to_lowercase())
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        cursor = cursor.min(matches.len().saturating_sub(1));
        let mut lines = board_lines(&docks, &matches, cursor, &filter);

        // Detail pane for the selected dock (hidden while typing a filter).
        if !matches.is_empty() && !filtering {
            let dock = &docks[matches[cursor]];
            let height = terminal::size()?.1 as usize;
            let mut detail = detail_lines(dock);
            let budget = height.saturating_sub(lines.len() + 5);
            if detail.len() > budget {
                detail.truncate(budget.max(2));
            }
            lines.push(vec![plain(String::new())]);
            lines.extend(detail);
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

            KeyCode::Char('?') if !filtering => show_help(&mut ui)?,
            KeyCode::Enter if filtering => filtering = false,
            KeyCode::Backspace if filtering => {
                filter.pop();
                cursor = 0;
            }
            KeyCode::Char(character) if filtering => {
                filter.push(character);
                cursor = 0;
            }
            KeyCode::Up => cursor = move_cursor(&docks, &matches, cursor, "up"),
            KeyCode::Down if !matches.is_empty() => {
                cursor = move_cursor(&docks, &matches, cursor, "down")
            }
            KeyCode::Left if !filtering => cursor = move_cursor(&docks, &matches, cursor, "left"),
            KeyCode::Right if !filtering => cursor = move_cursor(&docks, &matches, cursor, "right"),
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
