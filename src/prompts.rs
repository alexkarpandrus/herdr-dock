use crate::Result;
use crate::git::message;
use crate::model::{Preset, Repository, RepositorySelection};
use crate::repos::{discover_repositories_with_progress, repository_key};
use crate::ui::{BaseRefChoice, Line, LineAction, Ui, read_key, show_notice, slugify};
use crossterm::event::{self, KeyCode};
use crossterm::terminal;
use std::{
    collections::BTreeSet,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

pub(crate) fn prompt_name(ui: &mut Ui, prefix: &str) -> Result<Option<String>> {
    let mut line = Line::new();
    loop {
        let slug = slugify(&line.text);
        ui.frame(
            "Create dock · project name",
            &[
                format!("> {}", line.display()),
                String::new(),
                format!(
                    "Branch: {prefix}/{}",
                    if slug.is_empty() { "…" } else { &slug }
                ),
                String::new(),
                "Enter continue · Esc cancel · arrows move · Ctrl+U clear".into(),
            ],
        )?;
        match line.handle(&read_key()?) {
            LineAction::Submit if !slug.is_empty() => return Ok(Some(line.text.trim().into())),
            LineAction::Cancel => return Ok(None),
            _ => {}
        }
    }
}
pub(crate) fn prompt_repositories(
    ui: &mut Ui,
    repositories: &[Repository],
    presets: &[Preset],
    search_roots: &[PathBuf],
) -> Result<Option<RepositorySelection>> {
    let mut repositories = repositories.to_vec();
    let mut selected = vec![false; repositories.len()];
    let mut query = Line::new();
    let mut cursor = 0;
    let mut preset_name = None;
    let mut recent = Vec::new();
    let mut discovered: Option<Vec<Repository>> = None;
    let (scan_tx, scan_rx) = mpsc::channel::<std::result::Result<Vec<Repository>, String>>();
    let mut scan_started = false;
    let found = Arc::new(AtomicUsize::new(0));
    loop {
        if !scan_started
            && discovered.is_none()
            && !search_roots.is_empty()
            && !query.text.is_empty()
        {
            scan_started = true;
            let roots = search_roots.to_vec();
            let tx = scan_tx.clone();
            let counter = Arc::clone(&found);
            thread::spawn(move || {
                let result = discover_repositories_with_progress(&roots, Some(counter.as_ref()))
                    .map_err(|error| error.to_string());
                let _ = tx.send(result);
            });
        }
        if discovered.is_none() {
            match scan_rx.try_recv() {
                Ok(Ok(found_repositories)) => discovered = Some(found_repositories),
                Ok(Err(error)) => return Err(message(error)),
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => discovered = Some(Vec::new()),
            }
        }
        let scanning = discovered.is_none() && scan_started;

        let quick_matches = if query.text.is_empty() {
            repositories.iter().collect::<Vec<_>>()
        } else {
            ranked_repositories(&query.text, &repositories)
        };
        let found_matches = if query.text.is_empty() {
            Vec::new()
        } else {
            ranked_repositories(&query.text, discovered.as_deref().unwrap_or_default())
                .into_iter()
                .filter(|repository| {
                    !repositories
                        .iter()
                        .any(|existing| existing.path == repository.path)
                })
                .collect::<Vec<_>>()
        };
        let choices = quick_matches
            .into_iter()
            .map(|repository| (true, repository))
            .chain(
                found_matches
                    .into_iter()
                    .map(|repository| (false, repository)),
            )
            .collect::<Vec<_>>();
        cursor = cursor.min(choices.len().saturating_sub(1));
        let height = terminal::size()?.1.saturating_sub(9) as usize;
        let visible = height.max(1).min(choices.len().max(1));
        let start = cursor.saturating_sub(visible.saturating_sub(1));
        let mut lines = vec![format!("Search: > {}", query.display())];
        if scanning {
            lines.push(format!(
                "Scanning configured roots… {} found",
                found.load(Ordering::Relaxed)
            ));
        }
        lines.push(String::new());
        if choices.is_empty() {
            lines.push(if query.text.is_empty() {
                "Type to search configured roots.".into()
            } else if scanning {
                "Searching…".into()
            } else {
                "No matches.".into()
            });
        } else {
            lines.extend(choices[start..start + visible].iter().enumerate().map(
                |(offset, (quick, repository))| {
                    let is_selected = *quick
                        && repositories
                            .iter()
                            .position(|existing| existing.path == repository.path)
                            .is_some_and(|index| selected[index]);
                    format!(
                        "{} [{}] {}  {}  ({})",
                        if start + offset == cursor { ">" } else { " " },
                        if is_selected { "x" } else { " " },
                        repository.name,
                        repository.path.display(),
                        if *quick { "quick" } else { "found" }
                    )
                },
            ));
        }
        if let Some(name) = &preset_name {
            lines.push(format!("Save as preset: {name}"));
        }
        lines.push(String::new());
        lines.push(
            "Type to search · ↑/↓ move · Enter add/continue · Space select · Esc clear/cancel"
                .into(),
        );
        if !presets.is_empty() {
            lines.push("Shift+P load preset · Shift+S save preset".into());
        }
        ui.frame("Create dock · repositories", &lines)?;

        let key = if scanning {
            if event::poll(Duration::from_millis(50))? {
                Some(read_key()?)
            } else {
                None
            }
        } else {
            Some(read_key()?)
        };
        let Some(key) = key else {
            continue;
        };
        match key.code {
            KeyCode::Esc if !query.text.is_empty() => {
                query.clear();
                cursor = 0;
            }
            KeyCode::Esc => return Ok(None),
            KeyCode::Up => cursor = cursor.saturating_sub(1),
            KeyCode::Down if !choices.is_empty() => cursor = (cursor + 1).min(choices.len() - 1),
            KeyCode::Char(' ') if query.text.is_empty() && !choices.is_empty() => {
                selected[cursor] = !selected[cursor]
            }
            KeyCode::Char('P') if query.text.is_empty() && !presets.is_empty() => {
                if let Some(preset) = prompt_preset(ui, presets)? {
                    selected = selection_for_preset(&repositories, &preset);
                    preset_name = None;
                    cursor = 0;
                }
            }
            KeyCode::Char('S') if query.text.is_empty() && selected.iter().any(|value| *value) => {
                preset_name = prompt_text(ui, "Create dock · preset name")?;
            }
            KeyCode::Enter if !query.text.is_empty() && !choices.is_empty() => {
                let (quick, repository) = (choices[cursor].0, choices[cursor].1.clone());
                if quick {
                    if let Some(index) = repositories
                        .iter()
                        .position(|existing| existing.path == repository.path)
                    {
                        selected[index] = true;
                        query.clear();
                        cursor = index;
                    }
                } else if repositories
                    .iter()
                    .any(|existing| existing.name == repository.name)
                {
                    show_notice(
                        ui,
                        "Repository name conflict",
                        &format!(
                            "{} is already used; configure an explicit name",
                            repository.name
                        ),
                    )?;
                } else {
                    recent.push(repository.path.clone());
                    repositories.push(repository);
                    selected.push(true);
                    query.clear();
                    cursor = repositories.len() - 1;
                }
            }
            KeyCode::Enter if query.text.is_empty() && selected.iter().any(|value| *value) => {
                let chosen = repositories
                    .iter()
                    .zip(&selected)
                    .filter(|(_, selected)| **selected)
                    .map(|(repository, _)| repository.clone())
                    .collect::<Vec<_>>();
                let preset = preset_name.map(|name| Preset {
                    name,
                    repositories: chosen
                        .iter()
                        .map(|repository| repository_key(&repository.path))
                        .collect(),
                });
                return Ok(Some((chosen, preset, recent)));
            }
            _ => {
                if let LineAction::Edited = query.handle(&key) {
                    cursor = 0;
                }
            }
        }
    }
}
pub(crate) fn ranked_repositories<'a>(
    query: &str,
    repositories: &'a [Repository],
) -> Vec<&'a Repository> {
    let mut matches = repositories
        .iter()
        .filter_map(|repository| {
            let score = fuzzy_score(query, &repository.name).or_else(|| {
                fuzzy_score(query, &repository.path.to_string_lossy()).map(|score| score + 1_000)
            })?;
            Some((score, repository))
        })
        .collect::<Vec<_>>();
    matches.sort_by(|(left_score, left), (right_score, right)| {
        left_score
            .cmp(right_score)
            .then_with(|| left.path.cmp(&right.path))
    });
    matches
        .into_iter()
        .map(|(_, repository)| repository)
        .collect()
}
pub(crate) fn ranked_refs<'a>(query: &str, refs: &'a [String]) -> Vec<&'a String> {
    if query.is_empty() {
        return refs.iter().collect();
    }
    let mut matches = refs
        .iter()
        .filter_map(|reference| Some((fuzzy_score(query, reference)?, reference)))
        .collect::<Vec<_>>();
    matches.sort_by(|(left_score, left), (right_score, right)| {
        left_score.cmp(right_score).then_with(|| left.cmp(right))
    });
    matches
        .into_iter()
        .map(|(_, reference)| reference)
        .collect()
}
pub(crate) fn fuzzy_score(query: &str, candidate: &str) -> Option<usize> {
    let needle = query
        .chars()
        .flat_map(char::to_lowercase)
        .collect::<Vec<_>>();
    if needle.is_empty() {
        return Some(0);
    }
    let haystack = candidate
        .chars()
        .flat_map(char::to_lowercase)
        .collect::<Vec<_>>();
    let mut next = 0;
    let mut previous = None;
    let mut score = 0;
    for character in needle {
        let index = haystack[next..]
            .iter()
            .position(|candidate| *candidate == character)?
            + next;
        score += previous.map_or(index, |previous| index - previous - 1);
        previous = Some(index);
        next = index + 1;
    }
    Some(score)
}
pub(crate) fn prompt_preset(ui: &mut Ui, presets: &[Preset]) -> Result<Option<Preset>> {
    let mut cursor: usize = 0;
    loop {
        let height = terminal::size()?.1.saturating_sub(7) as usize;
        let visible = height.max(1).min(presets.len());
        let start = cursor.saturating_sub(visible.saturating_sub(1));
        let mut lines = presets[start..start + visible]
            .iter()
            .enumerate()
            .map(|(offset, preset)| {
                format!(
                    "{} {} ({} repositories)",
                    if start + offset == cursor { ">" } else { " " },
                    preset.name,
                    preset.repositories.len()
                )
            })
            .collect::<Vec<_>>();
        lines.extend([String::new(), "↑/↓ move · Enter load · Esc back".into()]);
        ui.frame("Create dock · load preset", &lines)?;
        match read_key()?.code {
            KeyCode::Esc => return Ok(None),
            KeyCode::Up => cursor = cursor.saturating_sub(1),
            KeyCode::Down => cursor = (cursor + 1).min(presets.len() - 1),
            KeyCode::Enter => return Ok(Some(presets[cursor].clone())),
            _ => {}
        }
    }
}
pub(crate) fn prompt_text(ui: &mut Ui, title: &str) -> Result<Option<String>> {
    let mut line = Line::new();
    loop {
        ui.frame(
            title,
            &[
                format!("> {}", line.display()),
                String::new(),
                "Enter save · Esc back · arrows move · Ctrl+U clear".into(),
            ],
        )?;
        match line.handle(&read_key()?) {
            LineAction::Submit if !line.text.trim().is_empty() => {
                return Ok(Some(line.text.trim().into()));
            }
            LineAction::Cancel => return Ok(None),
            _ => {}
        }
    }
}
pub(crate) fn selection_for_preset(repositories: &[Repository], preset: &Preset) -> Vec<bool> {
    let paths = preset.repositories.iter().cloned().collect::<BTreeSet<_>>();
    repositories
        .iter()
        .map(|repository| paths.contains(&repository_key(&repository.path)))
        .collect()
}
pub(crate) fn upsert_preset(presets: &mut Vec<Preset>, preset: Preset) {
    if let Some(existing) = presets
        .iter_mut()
        .find(|existing| existing.name.eq_ignore_ascii_case(&preset.name))
    {
        *existing = preset;
    } else {
        presets.push(preset);
    }
}
pub(crate) fn prompt_base_ref(
    ui: &mut Ui,
    repository: &str,
    refs: &[String],
    initial: &str,
) -> Result<Option<BaseRefChoice>> {
    let initial_cursor = refs.iter().position(|value| value == initial).unwrap_or(0);
    let mut query = Line::new();
    let mut cursor = initial_cursor;
    loop {
        let matches = ranked_refs(&query.text, refs);
        cursor = cursor.min(matches.len().saturating_sub(1));
        let height = terminal::size()?.1.saturating_sub(9) as usize;
        let visible = height.max(1).min(matches.len().max(1));
        let start = cursor.saturating_sub(visible.saturating_sub(1));
        let mut lines = vec![format!("Search: > {}", query.display()), String::new()];
        if matches.is_empty() {
            lines.push("No matches.".into());
        } else {
            lines.extend(matches[start..start + visible].iter().enumerate().map(
                |(offset, reference)| {
                    format!(
                        "{} {}",
                        if start + offset == cursor { ">" } else { " " },
                        reference
                    )
                },
            ));
        }
        lines.extend([
            String::new(),
            "Type to search · ↑/↓ move · Enter select · Tab use for all · Esc clear/cancel".into(),
        ]);
        ui.frame(&format!("Create dock · base ref for {repository}"), &lines)?;
        let key = read_key()?;
        match key.code {
            KeyCode::Esc if !query.text.is_empty() => {
                query.clear();
                cursor = initial_cursor;
            }
            KeyCode::Esc => return Ok(None),
            KeyCode::Up => cursor = cursor.saturating_sub(1),
            KeyCode::Down if !matches.is_empty() => cursor = (cursor + 1).min(matches.len() - 1),
            KeyCode::Enter if !matches.is_empty() => {
                return Ok(Some(BaseRefChoice::One(matches[cursor].clone())));
            }
            KeyCode::Tab if !matches.is_empty() => {
                return Ok(Some(BaseRefChoice::All(matches[cursor].clone())));
            }
            _ => {
                if let LineAction::Edited = query.handle(&key) {
                    cursor = if query.text.is_empty() {
                        initial_cursor
                    } else {
                        0
                    };
                }
            }
        }
    }
}
