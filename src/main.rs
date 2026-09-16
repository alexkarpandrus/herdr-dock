mod archive;
mod create;
mod dock;
mod git;
mod herdr;
mod model;
mod overview;
mod prompts;
mod repos;
mod setup;
mod ui;
mod worktrees;

use std::{
    env,
    error::Error,
    io::{self, IsTerminal},
    process::{Command, Stdio},
};

pub(crate) type Result<T> = std::result::Result<T, Box<dyn Error>>;

fn main() {
    if let Err(error) = run() {
        report_error(error.as_ref());
        std::process::exit(1);
    }
}

fn report_error(error: &dyn Error) {
    if io::stdin().is_terminal()
        && let Ok(mut ui) = crate::ui::Ui::start()
        && crate::ui::show_notice(&mut ui, "herdr-dock", &error.to_string()).is_ok()
    {
        return;
    }
    eprintln!("herdr-dock: {error}");
    if io::stdin().is_terminal() {
        let _ = io::stdin().read_line(&mut String::new());
    }
}

fn run() -> Result<()> {
    match env::args().nth(1).as_deref() {
        Some("open") => open_popup(env::args().nth(2).as_deref().unwrap_or("create")),
        Some("create") => crate::create::create_dock(),
        Some("overview") => crate::overview::show_overview(),
        Some("setup") => crate::setup::setup(),
        _ => Err(crate::git::message(
            "expected `open`, `create`, `overview`, or `setup`",
        )),
    }
}

fn open_popup(entrypoint: &str) -> Result<()> {
    let herdr = env::var_os("HERDR_BIN_PATH").unwrap_or_else(|| "herdr".into());
    let status = Command::new(herdr)
        .args([
            "plugin",
            "pane",
            "open",
            "--plugin",
            "herdr-dock",
            "--entrypoint",
            entrypoint,
        ])
        .stdin(Stdio::null())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(crate::git::message(format!(
            "could not open popup: {status}"
        )))
    }
}

#[cfg(test)]
mod tests {
    use crate::Result;
    use crate::archive::*;
    use crate::create::*;
    use crate::dock::*;
    use crate::git::*;
    use crate::herdr::*;
    use crate::model::*;
    use crate::overview::*;
    use crate::prompts::*;
    use crate::repos::*;
    use crate::ui::*;
    use crate::worktrees::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use std::{
        collections::BTreeMap,
        env, fs,
        path::{Path, PathBuf},
        process::Command,
        time::{SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn slug_is_lower_snake_case() {
        assert_eq!(slugify("  Add OAuth 2.0 / Login  "), "add_oauth_2_0_login");
        assert_eq!(slugify("Café launch"), "café_launch");
    }

    #[test]
    fn escapes_terminal_control_characters() {
        assert_eq!(
            terminal_text("safe\x1b]52;clipboard\x07\n"),
            "safe\\u{1b}]52;clipboard\\u{7}\\n"
        );
    }

    #[test]
    fn line_editor_edits_with_cursor() {
        let mut line = Line::new();
        for character in "abc".chars() {
            line.handle(&KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
        }
        assert_eq!(line.text, "abc");
        assert_eq!(line.cursor, 3);

        line.handle(&KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        line.handle(&KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        line.handle(&KeyEvent::new(KeyCode::Char('X'), KeyModifiers::NONE));
        assert_eq!(line.text, "aXbc");
        assert_eq!(line.cursor, 2);

        line.handle(&KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(line.text, "abc");
        assert_eq!(line.cursor, 1);

        line.handle(&KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        line.handle(&KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(line.text, "");
        assert_eq!(line.cursor, 0);

        for character in "foo bar".chars() {
            line.handle(&KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
        }
        line.handle(&KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(line.text, "foo");
        assert_eq!(line.cursor, 3);
    }

    #[test]
    fn dock_tabs_start_with_root_and_include_every_repository() {
        let root = PathBuf::from("/dock");
        let repositories = ["api", "web"].map(|name| DockRepository {
            name: name.into(),
            source: PathBuf::from(format!("/repos/{name}")),
            worktree: root.join(name),
            base_ref: "HEAD".into(),
        });

        let labels = |tabs: Vec<DockTab>| tabs.into_iter().map(|tab| tab.label).collect::<Vec<_>>();
        assert_eq!(
            labels(default_dock_tabs(&root, &repositories)),
            ["root", "api", "web"]
        );
        assert_eq!(
            labels(default_dock_tabs(&root, &repositories[..1])),
            ["root", "api"]
        );
    }

    #[test]
    fn overview_keeps_the_state_record_index() -> Result<()> {
        let records: Vec<DockRecord> = serde_json::from_str(
            r#"[
                {"name":"old","slug":"same","branch":"agent/same","root":"/tmp/same","workspace_id":"old","created_at_unix":1,"archived_at_unix":2,"repositories":[]},
                {"name":"new","slug":"same","branch":"agent/same","root":"/tmp/same","workspace_id":"new","created_at_unix":3,"repositories":[]}
            ]"#,
        )?;

        let live = BTreeMap::from([(
            "new".into(),
            LiveWorkspace {
                status: "working".into(),
                agents: Vec::new(),
                tabs: Vec::new(),
            },
        )]);
        let overview = build_overview(&records, &live, Some("default"));
        assert!(
            overview[0].open,
            "a live legacy workspace must block actions"
        );
        assert_eq!(overview[0].status, "session unknown");
        assert!(check_dock_session(&records[1], Some("default")).is_err());
        assert_eq!(
            overview
                .iter()
                .map(|dock| dock.record_index)
                .collect::<Vec<_>>(),
            [1, 0]
        );
        Ok(())
    }

    #[test]
    fn loads_and_replaces_repository_presets() -> Result<()> {
        let repositories = ["api", "web"].map(|name| Repository {
            name: name.into(),
            path: PathBuf::from(format!("/repos/{name}")),
        });
        let preset = Preset {
            name: "Frontend".into(),
            repositories: vec!["/repos/web".into()],
        };
        assert_eq!(selection_for_preset(&repositories, &preset), [false, true]);

        let mut presets = vec![preset];
        upsert_preset(
            &mut presets,
            Preset {
                name: "frontend".into(),
                repositories: vec!["/repos/api".into(), "/repos/web".into()],
            },
        );
        assert_eq!(presets.len(), 1);
        assert_eq!(presets[0].repositories.len(), 2);

        let old_state: State = serde_json::from_str(r#"{"base_refs":{},"docks":[]}"#)?;
        assert!(old_state.presets.is_empty());
        assert!(old_state.recent_repositories.is_empty());

        let default_config: Config = toml::from_str("branch_prefix = 'agent'")?;
        assert_eq!(default_config.worktree_manager, WorktreeManager::Git);
        assert!(default_config.repository_search_roots.is_empty());
        let search_config: Config =
            toml::from_str("repository_search_roots = ['~/Src', '~/Work']")?;
        assert_eq!(search_config.repository_search_roots.len(), 2);
        let worktrunk_config: Config =
            toml::from_str("branch_prefix = 'agent'\nworktree_manager = 'worktrunk'")?;
        assert_eq!(
            worktrunk_config.worktree_manager,
            WorktreeManager::Worktrunk
        );

        let old_record: DockRecord = serde_json::from_str(
            r#"{"name":"x","slug":"x","branch":"agent/x","root":"/tmp/x","workspace_id":"x","created_at_unix":1,"repositories":[]}"#,
        )?;
        assert_eq!(old_record.worktree_manager, WorktreeManager::Git);
        assert!(old_record.herdr_session.is_none());
        assert!(old_record.goal.is_none());
        assert!(old_record.completed_at_unix.is_none());
        assert!(old_record.agents.is_empty());
        assert!(old_record.tabs.is_empty());
        Ok(())
    }

    #[test]
    fn persists_resumable_agent_sessions() -> Result<()> {
        let temporary = env::temp_dir().join(format!(
            "herdr-dock-agent-state-test-{}-{}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
        ));
        fs::create_dir_all(&temporary)?;
        let workspace_response = serde_json::json!({
            "result": {"workspaces": [{"workspace_id": "w1", "agent_status": "idle"}]}
        });
        let agent_response = serde_json::json!({
            "result": {"agents": [{
                "workspace_id": "w1",
                "name": "reviewer",
                "agent": "codex",
                "agent_status": "idle",
                "tab_id": "t1",
                "pane_id": "p1",
                "foreground_cwd": temporary,
                "agent_session": {
                    "source": "herdr:codex",
                    "agent": "codex",
                    "kind": "id",
                    "value": "session-123"
                }
            }]}
        });
        let tab_response = serde_json::json!({
            "result": {"tabs": [{"workspace_id": "w1", "tab_id": "t1", "label": "work", "number": 1, "pane_count": 1}]}
        });
        let pane_response = serde_json::json!({
            "result": {"panes": [{"tab_id": "t1", "pane_id": "p1", "cwd": temporary}]}
        });
        let live = parse_live_workspaces(
            &workspace_response,
            &tab_response,
            &pane_response,
            &agent_response,
        );
        let mut state = State::default();
        state.docks.push(DockRecord {
            name: "Test dock".into(),
            slug: "test_dock".into(),
            branch: "agent/test_dock".into(),
            goal: None,
            root: temporary.clone(),
            workspace_id: "w1".into(),
            herdr_session: Some("default".into()),
            completed_at_unix: None,
            archived_at_unix: None,
            worktree_manager: WorktreeManager::Git,
            agents: Vec::new(),
            tabs: Vec::new(),
            repositories: Vec::new(),
        });

        check_dock_session(&state.docks[0], Some("default"))?;
        assert!(check_dock_session(&state.docks[0], Some("other")).is_err());
        assert!(check_dock_session(&state.docks[0], None).is_err());

        assert!(!sync_dock_agents(&mut state.docks, &live, Some("other")));
        assert!(state.docks[0].agents.is_empty());

        assert!(sync_dock_agents(&mut state.docks, &live, Some("default")));
        assert!(!sync_dock_agents(&mut state.docks, &live, Some("default")));
        assert_eq!(state.docks[0].tabs[0].label, "work");
        assert_eq!(state.docks[0].agents[0].tab, Some(0));
        assert_eq!(live["w1"].tabs[0].pane_id, "p1");
        assert_eq!(live["w1"].tabs[0].pane_count, 1);
        assert_eq!(live["w1"].agents[0].pane_id.as_deref(), Some("p1"));
        assert_eq!(
            agent_resume_arguments(&state.docks[0].agents[0]),
            Some(vec!["resume".into(), "session-123".into()])
        );
        state.docks[0].tabs = vec![
            DockTab {
                label: "root".into(),
                cwd: temporary.clone(),
            },
            DockTab {
                label: "api".into(),
                cwd: temporary.join("api"),
            },
        ];
        state.docks[0].agents.insert(
            0,
            DockAgent {
                name: Some("api-child".into()),
                kind: "pi".into(),
                cwd: temporary.join("api"),
                tab: Some(1),
                session: None,
            },
        );
        assert_eq!(agent_resume_order(&state.docks[0]), [1, 0]);
        assert!(pane_shell_ready(&serde_json::json!({
            "result": {"process_info": {"foreground_process_group_id": 7, "shell_pid": 7}}
        })));
        assert!(!pane_shell_ready(&serde_json::json!({
            "result": {"process_info": {"foreground_process_group_id": 8, "shell_pid": 7}}
        })));
        assert!(!pane_shell_ready(&serde_json::json!({})));
        state.docks[0].completed_at_unix = Some(2);
        let open_done = build_overview(&state.docks, &live, Some("default"));
        assert!(open_done[0].open);
        let overview = build_overview(&state.docks, &BTreeMap::new(), Some("default"));
        assert_eq!(overview[0].status, "done");
        assert_eq!(overview[0].agents[0].status, "done");
        assert_eq!(
            overview[0]
                .agents
                .iter()
                .find(|agent| agent.is_root)
                .map(|agent| agent.name.as_str()),
            Some("reviewer")
        );
        assert_eq!(
            overview[0]
                .agents
                .iter()
                .filter(|agent| agent.is_root)
                .count(),
            1
        );
        let display_text = |lines: Vec<Vec<Segment>>| {
            lines
                .into_iter()
                .map(|line| {
                    line.into_iter()
                        .map(|segment| match segment {
                            Segment::Plain(text) | Segment::Styled(_, text) => text,
                        })
                        .collect::<Vec<_>>()
                        .join("")
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        let card = display_text(card_lines(&overview[0], 34, false));
        assert!(card.contains("root · codex · done"));
        assert!(card.contains("1 child"));
        let detail = display_text(detail_lines(&overview[0]));
        assert!(detail.contains("root · reviewer"));
        assert!(detail.contains("└─ api-child"));

        let state_path = temporary.join("state.json");
        let first_lock = lock_state(&state_path)?;
        let child = Command::new(env::current_exe()?)
            .args(["--exact", "tests::state_lock_child", "--nocapture"])
            .env("HERDR_DOCK_LOCK_TEST_PATH", &state_path)
            .status()?;
        assert!(child.success());
        drop(first_lock);
        save_state(&state_path, &state)?;
        let loaded = load_state(&state_path)?;
        assert_eq!(loaded.docks[0].agents, state.docks[0].agents);
        assert!(
            !temporary
                .join(format!(".state.json.{}.tmp", std::process::id()))
                .exists()
        );
        fs::remove_dir_all(temporary)?;
        Ok(())
    }

    #[test]
    fn writes_search_root_into_config() -> Result<()> {
        let temporary = env::temp_dir().join(format!(
            "herdr-dock-search-root-test-{}-{}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
        ));
        fs::create_dir_all(&temporary)?;
        let config_path = temporary.join("config.toml");
        write_search_root(&config_path, Path::new("/home/user/Src"))?;
        let config: Config = toml::from_str(&fs::read_to_string(&config_path)?)?;
        assert_eq!(
            config.repository_search_roots,
            vec![PathBuf::from("/home/user/Src")]
        );
        fs::remove_dir_all(temporary)?;
        Ok(())
    }

    #[test]
    fn state_lock_child() -> Result<()> {
        let Some(path) = env::var_os("HERDR_DOCK_LOCK_TEST_PATH") else {
            return Ok(());
        };
        let error = lock_state(Path::new(&path)).expect_err("second process must be refused");
        assert!(error.to_string().contains("another Herdr Dock action"));
        Ok(())
    }

    #[test]
    fn discovers_and_ranks_repositories() -> Result<()> {
        let temporary = env::temp_dir().join(format!(
            "herdr-dock-search-test-{}-{}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
        ));
        let api = temporary.join("services").join("application-api");
        let web = temporary.join("clients").join("web-client");
        fs::create_dir_all(&api)?;
        fs::create_dir_all(&web)?;
        checked(Command::new("git").arg("init").arg("--quiet").arg(&api))?;
        checked(Command::new("git").arg("init").arg("--quiet").arg(&web))?;

        let repositories =
            discover_repositories_with_progress(std::slice::from_ref(&temporary), None)?;
        assert_eq!(repositories.len(), 2);
        assert_eq!(
            ranked_repositories("wc", &repositories)[0].name,
            "web-client"
        );
        assert!(ranked_repositories("missing", &repositories).is_empty());

        let refs = ["HEAD", "main", "origin/main", "feature/search"].map(str::to_owned);
        assert_eq!(ranked_refs("fs", &refs)[0], "feature/search");
        assert!(ranked_refs("missing", &refs).is_empty());

        let mut recent = vec![api.canonicalize()?];
        remember_repository(&mut recent, web.canonicalize()?);
        remember_repository(&mut recent, api.canonicalize()?);
        assert_eq!(recent[0], api.canonicalize()?);
        assert_eq!(recent.len(), 2);

        let mut quick_list = Vec::new();
        merge_recent_repositories(&mut quick_list, &recent);
        assert_eq!(quick_list.len(), 2);
        fs::remove_dir_all(temporary)?;
        Ok(())
    }

    #[test]
    fn creates_matching_worktrees_and_agent_guides() -> Result<()> {
        let temporary = env::temp_dir().join(format!(
            "herdr-dock-test-{}-{}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
        ));
        fs::create_dir_all(&temporary)?;
        let repositories = ["api", "web"]
            .map(|name| -> Result<Repository> {
                let path = temporary.join(name);
                fs::create_dir(&path)?;
                checked(Command::new("git").arg("init").arg("--quiet").arg(&path))?;
                fs::write(path.join("README.md"), name)?;
                checked(Command::new("git").arg("-C").arg(&path).args(["add", "."]))?;
                checked(Command::new("git").arg("-C").arg(&path).args([
                    "-c",
                    "user.name=Test",
                    "-c",
                    "user.email=test@example.com",
                    "commit",
                    "--quiet",
                    "-m",
                    "initial",
                ]))?;
                Ok(Repository {
                    name: name.into(),
                    path,
                })
            })
            .into_iter()
            .collect::<Result<Vec<_>>>()?;
        let plans = repositories
            .into_iter()
            .map(|repository| RepositoryPlan {
                repository,
                base_ref: "HEAD".into(),
            })
            .collect::<Vec<_>>();
        let root = temporary.join("workspaces").join("oauth_login");
        let worktrees = materialize_worktrees(
            WorktreeManager::Git,
            &root,
            "OAuth login",
            Some("Ship OAuth login across API and web"),
            "agent/oauth_login",
            &plans,
        )?;

        assert_eq!(worktrees.len(), 2);
        for worktree in &worktrees {
            assert_eq!(
                git(worktree, ["branch", "--show-current"])?,
                "agent/oauth_login"
            );
        }
        let guide = fs::read_to_string(root.join("AGENTS.md"))?;
        assert!(guide.contains("## Goal\n\nShip OAuth login across API and web"));
        assert!(guide.contains("api"));
        assert!(guide.contains("The agent in `root` is the root session"));
        assert!(guide.contains("start child agents in any repository"));
        assert!(!guide.contains("task and expected report"));
        assert!(!guide.contains("Do not have workers"));
        assert_eq!(
            fs::read(root.join("AGENTS.md"))?,
            fs::read(root.join("CLAUDE.md"))?
        );

        fs::write(worktrees[0].join("dirty.txt"), "changed")?;
        let mut record = DockRecord {
            name: "OAuth login".into(),
            slug: "oauth_login".into(),
            branch: "agent/oauth_login".into(),
            goal: Some("Ship OAuth login across API and web".into()),
            root: root.clone(),
            workspace_id: "workspace-1".into(),
            herdr_session: Some("default".into()),
            completed_at_unix: None,
            archived_at_unix: None,
            worktree_manager: WorktreeManager::Git,
            agents: Vec::new(),
            tabs: Vec::new(),
            repositories: plans
                .iter()
                .zip(&worktrees)
                .map(|(plan, worktree)| DockRepository {
                    name: plan.repository.name.clone(),
                    source: plan.repository.path.clone(),
                    worktree: worktree.clone(),
                    base_ref: plan.base_ref.clone(),
                })
                .collect(),
        };
        let state_path = temporary.join("state.json");
        let mut goal_state = State {
            docks: vec![record],
            ..State::default()
        };
        update_dock_goal(
            &mut goal_state,
            0,
            &state_path,
            Some("Ship the revised OAuth flow".into()),
        )?;
        assert_eq!(
            load_state(&state_path)?.docks[0].goal.as_deref(),
            Some("Ship the revised OAuth flow")
        );
        assert!(
            fs::read_to_string(root.join("AGENTS.md"))?
                .contains("## Goal\n\nShip the revised OAuth flow")
        );
        assert_eq!(
            fs::read(root.join("AGENTS.md"))?,
            fs::read(root.join("CLAUDE.md"))?
        );
        record = goal_state.docks.remove(0);
        let live = BTreeMap::from([(
            "workspace-1".into(),
            LiveWorkspace {
                status: "working".into(),
                agents: vec![AgentOverview {
                    name: "shared".into(),
                    kind: "pi".into(),
                    status: "working".into(),
                    cwd: root.to_string_lossy().into(),
                    tab_id: None,
                    pane_id: None,
                    is_root: false,
                    launch_name: None,
                    session: None,
                }],
                tabs: Vec::new(),
            },
        )]);
        let overview = build_overview(std::slice::from_ref(&record), &live, Some("default"));
        assert!(overview[0].open);
        assert_eq!(overview[0].status, "working");
        assert_eq!(overview[0].agents.len(), 1);
        assert!(overview[0].agents[0].is_root);
        assert_eq!(
            overview[0]
                .repositories
                .iter()
                .filter(|repository| repository.status == "dirty")
                .count(),
            1
        );

        let error =
            archive_dock(&record, false, || Ok(())).expect_err("dirty worktree must be refused");
        let error = error.to_string();
        assert!(error.contains("uncommitted or untracked changes"));
        assert!(error.contains(&worktrees[0].display().to_string()));
        assert!(error.contains("status --short --untracked-files=all"));
        assert!(root.exists());

        fs::remove_file(worktrees[0].join("dirty.txt"))?;
        let agents_guide = root.join("AGENTS.md");
        let guide = fs::read(&agents_guide)?;
        fs::remove_file(&agents_guide)?;
        fs::create_dir(&agents_guide)?;
        archive_dock(&record, false, || Ok(())).expect_err("root cleanup failure must be reported");
        assert!(worktrees.iter().all(|worktree| worktree.is_dir()));
        fs::remove_dir(&agents_guide)?;
        fs::write(&agents_guide, guide)?;
        fs::write(worktrees[0].join(".gitignore"), ".ignored\n")?;
        checked(
            Command::new("git")
                .arg("-C")
                .arg(&worktrees[0])
                .args(["add", ".gitignore"]),
        )?;
        checked(Command::new("git").arg("-C").arg(&worktrees[0]).args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "--quiet",
            "-m",
            "ignore local files",
        ]))?;
        fs::write(worktrees[0].join(".ignored"), "must survive")?;
        let error =
            archive_dock(&record, false, || Ok(())).expect_err("ignored files must be refused");
        assert!(error.to_string().contains("ignored files"));
        fs::remove_file(worktrees[0].join(".ignored"))?;

        fs::write(worktrees[0].join("hidden.txt"), "must survive")?;
        checked(Command::new("git").arg("-C").arg(&worktrees[0]).args([
            "config",
            "status.showUntrackedFiles",
            "no",
        ]))?;
        let error = archive_dock(&record, false, || Ok(()))
            .expect_err("hidden untracked files must be refused");
        assert!(
            error
                .to_string()
                .contains("uncommitted or untracked changes")
        );
        fs::remove_file(worktrees[0].join("hidden.txt"))?;
        let relocated = temporary.join("relocated");
        checked(
            Command::new("git")
                .arg("-C")
                .arg(&record.repositories[1].source)
                .args(["worktree", "move"])
                .arg(&record.repositories[1].worktree)
                .arg(&relocated),
        )?;
        std::os::unix::fs::symlink(&relocated, &record.repositories[1].worktree)?;
        let error = archive_dock(&record, false, || Ok(()))
            .expect_err("symlinked worktree must be refused");
        assert!(error.to_string().contains("symbolic link"));
        assert!(relocated.is_dir());
        fs::remove_file(&record.repositories[1].worktree)?;
        let error = archive_dock(&record, false, || Ok(()))
            .expect_err("relocated worktree must be refused");
        assert!(error.to_string().contains("is checked out at"));
        checked(
            Command::new("git")
                .arg("-C")
                .arg(&record.repositories[1].source)
                .args(["worktree", "move"])
                .arg(&relocated)
                .arg(&record.repositories[1].worktree),
        )?;

        let error = archive_dock(&record, false, || {
            assert!(!root.exists());
            Err(message("state save failed"))
        })
        .expect_err("failed state save must restore archived files");
        assert!(error.to_string().contains("state save failed"));
        assert!(worktrees.iter().all(|worktree| worktree.is_dir()));
        assert!(root.join("AGENTS.md").is_file());
        assert!(root.join("CLAUDE.md").is_file());
        record.worktree_manager.remove(
            &record.repositories[0].source,
            &record.repositories[0].worktree,
        )?;
        assert!(!worktrees[0].exists());
        archive_dock(&record, false, || Ok(()))?;
        assert!(!root.exists());
        archive_dock(&record, false, || Ok(()))?;
        for plan in &plans {
            git(
                &plan.repository.path,
                ["rev-parse", "--verify", "refs/heads/agent/oauth_login"],
            )?;
        }
        let add_root = temporary.join("workspaces").join("add-repository");
        fs::create_dir(&add_root)?;
        let existing_plan = RepositoryPlan {
            repository: plans[0].repository.clone(),
            base_ref: "agent/oauth_login".into(),
        };
        let added = materialize_added_worktrees(
            WorktreeManager::Git,
            &add_root,
            "agent/oauth_login",
            std::slice::from_ref(&existing_plan),
        )?;
        assert_eq!(
            git(&added[0].1, ["branch", "--show-current"])?,
            "agent/oauth_login"
        );
        write_agent_guides(
            &add_root,
            "OAuth login",
            Some("Ship OAuth login"),
            "agent/oauth_login",
            &[existing_plan],
        )?;
        assert!(fs::read_to_string(add_root.join("AGENTS.md"))?.contains("reused existing branch"));
        assert!(cleanup_added_resources(WorktreeManager::Git, &added, &[]).is_empty());
        fs::remove_file(add_root.join("AGENTS.md"))?;
        fs::remove_file(add_root.join("CLAUDE.md"))?;
        fs::remove_dir(&add_root)?;

        let rollback_root = temporary.join("workspaces").join("rollback");
        let rollback_worktrees = materialize_worktrees(
            WorktreeManager::Git,
            &rollback_root,
            "Rollback",
            None,
            "agent/oauth_login",
            &plans,
        )?;
        let rollback_entries = plans
            .iter()
            .zip(&rollback_worktrees)
            .map(|(plan, worktree)| (plan.repository.path.clone(), worktree.clone()))
            .collect::<Vec<_>>();
        assert!(
            cleanup_materialized_worktrees(WorktreeManager::Git, &rollback_root, &rollback_entries)
                .is_empty()
        );
        assert!(!rollback_root.exists());

        record.archived_at_unix = Some(2);
        let overview = build_overview(&[record], &BTreeMap::new(), Some("default"));
        assert_eq!(overview[0].status, "archived");
        assert!(overview[0].archived);
        assert!(
            overview[0]
                .repositories
                .iter()
                .all(|repository| repository.status == "archived")
        );

        fs::remove_dir_all(temporary)?;
        Ok(())
    }
}
