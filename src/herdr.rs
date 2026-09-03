use crate::Result;
use crate::git::{checked, message};
use crate::model::{
    AgentOverview, AgentSession, DockAgent, DockRecord, DockTab, LiveTab, LiveWorkspace, OpenedTab,
    OpenedWorkspace,
};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    path::Path,
    process::Command,
};

pub(crate) fn live_workspaces() -> Result<BTreeMap<String, LiveWorkspace>> {
    let workspace_response = herdr_json(&["workspace", "list"])?;
    let tab_response = herdr_json(&["tab", "list"])?;
    let pane_response = herdr_json(&["pane", "list"])?;
    let agent_response = herdr_json(&["agent", "list"])?;
    Ok(parse_live_workspaces(
        &workspace_response,
        &tab_response,
        &pane_response,
        &agent_response,
    ))
}
pub(crate) fn parse_live_workspaces(
    workspace_response: &Value,
    tab_response: &Value,
    pane_response: &Value,
    agent_response: &Value,
) -> BTreeMap<String, LiveWorkspace> {
    let mut workspaces = BTreeMap::new();
    if let Some(items) = workspace_response
        .pointer("/result/workspaces")
        .and_then(Value::as_array)
    {
        for workspace in items {
            if let Some(workspace_id) = workspace.get("workspace_id").and_then(Value::as_str) {
                workspaces.insert(
                    workspace_id.into(),
                    LiveWorkspace {
                        status: workspace
                            .get("agent_status")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown")
                            .into(),
                        agents: Vec::new(),
                        tabs: Vec::new(),
                    },
                );
            }
        }
    }

    let mut tab_cwds = BTreeMap::new();
    if let Some(items) = pane_response
        .pointer("/result/panes")
        .and_then(Value::as_array)
    {
        for pane in items {
            if let (Some(tab_id), Some(cwd)) = (
                pane.get("tab_id").and_then(Value::as_str),
                pane.get("cwd")
                    .and_then(Value::as_str)
                    .or_else(|| pane.get("foreground_cwd").and_then(Value::as_str)),
            ) {
                tab_cwds.entry(tab_id.to_owned()).or_insert(cwd.to_owned());
            }
        }
    }
    if let Some(items) = tab_response
        .pointer("/result/tabs")
        .and_then(Value::as_array)
    {
        for tab in items {
            let Some(workspace_id) = tab.get("workspace_id").and_then(Value::as_str) else {
                continue;
            };
            let Some(id) = tab.get("tab_id").and_then(Value::as_str) else {
                continue;
            };
            let Some(workspace) = workspaces.get_mut(workspace_id) else {
                continue;
            };
            workspace.tabs.push(LiveTab {
                id: id.into(),
                label: tab
                    .get("label")
                    .and_then(Value::as_str)
                    .unwrap_or("tab")
                    .into(),
                cwd: tab_cwds.get(id).cloned().unwrap_or_default(),
                number: tab
                    .get("number")
                    .and_then(Value::as_u64)
                    .unwrap_or(u64::MAX),
            });
        }
    }
    for workspace in workspaces.values_mut() {
        workspace.tabs.sort_by_key(|tab| tab.number);
    }

    if let Some(items) = agent_response
        .pointer("/result/agents")
        .and_then(Value::as_array)
    {
        for agent in items {
            let Some(workspace_id) = agent.get("workspace_id").and_then(Value::as_str) else {
                continue;
            };
            let Some(workspace) = workspaces.get_mut(workspace_id) else {
                continue;
            };
            let kind = agent
                .get("agent")
                .and_then(Value::as_str)
                .unwrap_or("agent")
                .to_owned();
            let launch_name = agent.get("name").and_then(Value::as_str).map(str::to_owned);
            let session = agent.get("agent_session").and_then(|session| {
                Some(AgentSession {
                    source: session.get("source")?.as_str()?.into(),
                    agent: session.get("agent")?.as_str()?.into(),
                    kind: session.get("kind")?.as_str()?.into(),
                    value: session.get("value")?.as_str()?.into(),
                })
            });
            workspace.agents.push(AgentOverview {
                name: launch_name.clone().unwrap_or_else(|| kind.clone()),
                kind,
                status: agent
                    .get("agent_status")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .into(),
                cwd: agent
                    .get("foreground_cwd")
                    .and_then(Value::as_str)
                    .or_else(|| agent.get("cwd").and_then(Value::as_str))
                    .unwrap_or("")
                    .into(),
                tab_id: agent
                    .get("tab_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                launch_name,
                session,
            });
        }
    }
    workspaces
}
pub(crate) fn current_herdr_session() -> Result<Option<String>> {
    let Some(socket) = env::var_os("HERDR_SOCKET_PATH") else {
        return Ok(None);
    };
    let response = herdr_json(&["session", "list", "--json"])?;
    Ok(response
        .get("sessions")
        .and_then(Value::as_array)
        .and_then(|sessions| {
            sessions.iter().find_map(|session| {
                (session.get("socket_path").and_then(Value::as_str) == socket.to_str())
                    .then(|| {
                        session
                            .get("name")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                    })
                    .flatten()
            })
        }))
}
pub(crate) fn open_workspace(name: &str, dock_tabs: &[DockTab]) -> Result<OpenedWorkspace> {
    let first = dock_tabs
        .first()
        .ok_or_else(|| message("dock must have at least one tab"))?;
    let response = herdr_json(&[
        "workspace",
        "create",
        "--cwd",
        &first.cwd.to_string_lossy(),
        "--label",
        name,
        "--no-focus",
    ])?;
    let workspace_id = json_string(&response, "/result/workspace/workspace_id")?;
    let setup = (|| -> Result<Vec<OpenedTab>> {
        let first_tab_id = json_string(&response, "/result/tab/tab_id")?;
        let first_pane_id = json_string(&response, "/result/root_pane/pane_id")?;
        herdr(&["tab", "rename", &first_tab_id, &first.label])?;
        let mut tabs = vec![OpenedTab {
            cwd: first.cwd.clone(),
            root_pane_id: first_pane_id,
        }];
        for tab in &dock_tabs[1..] {
            let response = herdr_json(&[
                "tab",
                "create",
                "--workspace",
                &workspace_id,
                "--cwd",
                &tab.cwd.to_string_lossy(),
                "--label",
                &tab.label,
                "--no-focus",
            ])?;
            tabs.push(OpenedTab {
                cwd: tab.cwd.clone(),
                root_pane_id: json_string(&response, "/result/root_pane/pane_id")?,
            });
        }
        Ok(tabs)
    })();
    match setup {
        Ok(tabs) => Ok(OpenedWorkspace {
            id: workspace_id,
            tabs,
        }),
        Err(error) => {
            if let Err(close_error) = herdr(&["workspace", "close", &workspace_id]) {
                return Err(message(format!(
                    "{error}; also failed to close partial workspace: {close_error}"
                )));
            }
            Err(error)
        }
    }
}
pub(crate) fn resume_agents(record: &DockRecord, workspace: &OpenedWorkspace) -> Vec<String> {
    let mut errors = Vec::new();
    let mut occupied_tabs = BTreeSet::new();
    for (index, agent) in record.agents.iter().enumerate() {
        let Some(resume_arguments) = agent_resume_arguments(agent) else {
            errors.push(format!(
                "{} has no supported resumable session",
                agent.name.as_deref().unwrap_or(&agent.kind)
            ));
            continue;
        };
        let tab_index = agent
            .tab
            .filter(|index| *index < workspace.tabs.len())
            .or_else(|| {
                workspace
                    .tabs
                    .iter()
                    .enumerate()
                    .filter(|(_, tab)| agent.cwd.starts_with(&tab.cwd))
                    .max_by_key(|(_, tab)| tab.cwd.components().count())
                    .map(|(index, _)| index)
            });
        let Some(tab_index) = tab_index else {
            errors.push(format!("{} has no matching dock tab", agent.cwd.display()));
            continue;
        };
        let tab = &workspace.tabs[tab_index];
        let pane_id = if !occupied_tabs.contains(&tab_index) && agent.cwd == tab.cwd {
            occupied_tabs.insert(tab_index);
            Ok(tab.root_pane_id.clone())
        } else {
            let response = herdr_json(&[
                "pane",
                "split",
                &tab.root_pane_id,
                "--direction",
                "right",
                "--cwd",
                &agent.cwd.to_string_lossy(),
                "--no-focus",
            ]);
            response.and_then(|response| json_string(&response, "/result/pane/pane_id"))
        };
        let pane_id = match pane_id {
            Ok(pane_id) => pane_id,
            Err(error) => {
                errors.push(format!("{}: {error}", agent.cwd.display()));
                continue;
            }
        };
        let name = agent_launch_name(record, agent, index);
        let mut arguments = vec![
            "agent".into(),
            "start".into(),
            name.clone(),
            "--kind".into(),
            agent.kind.clone(),
            "--pane".into(),
            pane_id,
            "--".into(),
        ];
        arguments.extend(resume_arguments);
        let references = arguments.iter().map(String::as_str).collect::<Vec<_>>();
        if let Err(error) = herdr(&references) {
            errors.push(format!("{name}: {error}"));
        }
    }
    errors
}
pub(crate) fn agent_launch_name(record: &DockRecord, agent: &DockAgent, index: usize) -> String {
    if let Some(name) = agent.name.as_ref().filter(|name| valid_agent_name(name)) {
        return name.clone();
    }
    let mut base = format!(
        "dock-{}-{}",
        record
            .slug
            .chars()
            .filter(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
            .collect::<String>(),
        agent
            .kind
            .chars()
            .filter(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
            .collect::<String>()
    );
    let suffix = format!("-{}", index + 1);
    base.truncate(32 - suffix.len());
    base.push_str(&suffix);
    base
}
pub(crate) fn valid_agent_name(name: &str) -> bool {
    name.len() <= 32
        && name.starts_with(|character: char| character.is_ascii_lowercase())
        && name.chars().all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || matches!(character, '_' | '-')
        })
}
pub(crate) fn agent_resume_arguments(agent: &DockAgent) -> Option<Vec<String>> {
    let session = agent.session.as_ref()?;
    if session.agent != agent.kind
        || session.value.is_empty()
        || session.value.chars().any(char::is_control)
    {
        return None;
    }
    match session.kind.as_str() {
        "id" if session.value.len() > 512 => return None,
        "path" if session.value.len() > 4096 || !Path::new(&session.value).is_absolute() => {
            return None;
        }
        "id" | "path" => {}
        _ => return None,
    }

    let value = session.value.clone();
    match (
        session.source.as_str(),
        agent.kind.as_str(),
        session.kind.as_str(),
    ) {
        ("herdr:claude", "claude", "id") => Some(vec!["--resume".into(), value]),
        ("herdr:codex", "codex", "id") => Some(vec!["resume".into(), value]),
        ("herdr:copilot", "copilot", "id") => Some(vec![format!("--resume={value}")]),
        ("herdr:devin", "devin", "id")
        | ("herdr:droid", "droid", "id")
        | ("herdr:hermes", "hermes", "id")
        | ("herdr:qodercli", "qodercli", "id")
        | ("herdr:qwen", "qwen", "id")
        | ("herdr:grok", "grok", "id") => Some(vec!["--resume".into(), value]),
        ("herdr:kimi", "kimi", "id")
        | ("herdr:opencode", "opencode", "id")
        | ("herdr:kilo", "kilo", "id") => Some(vec!["--session".into(), value]),
        ("herdr:mastracode", "mastracode", "id") => Some(vec!["--thread".into(), value]),
        ("herdr:pi", "pi", "id" | "path") => Some(vec!["--session".into(), value]),
        ("herdr:omp", "omp", "id" | "path") => Some(vec![format!("--resume={value}")]),
        ("herdr:cursor", "cursor", "id") => Some(vec!["--resume".into(), value]),
        ("herdr:antigravity_cli", "agy", "id") => Some(vec!["--conversation".into(), value]),
        _ => None,
    }
}
pub(crate) fn json_string(value: &Value, pointer: &str) -> Result<String> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| message(format!("Herdr response omitted {pointer}")))
}
pub(crate) fn herdr_json(arguments: &[&str]) -> Result<Value> {
    Ok(serde_json::from_str(&herdr(arguments)?)?)
}
pub(crate) fn herdr(arguments: &[&str]) -> Result<String> {
    let program = env::var_os("HERDR_BIN_PATH").unwrap_or_else(|| "herdr".into());
    checked(Command::new(program).args(arguments))
}
