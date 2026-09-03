use crate::Result;
use crate::git::message;
use std::{
    env, fs,
    io::{self, IsTerminal},
    path::PathBuf,
};

const CREATE_BINDING: &str = r#"

[[keys.command]]
key = "prefix+d"
type = "plugin_action"
command = "herdr-dock.create"
description = "create dock"
"#;

const OVERVIEW_BINDING: &str = r#"

[[keys.command]]
key = "prefix+o"
type = "plugin_action"
command = "herdr-dock.overview"
description = "show dock overview"
"#;

fn config_path() -> Result<PathBuf> {
    if let Some(dir) = env::var_os("HERDR_CONFIG_DIR") {
        return Ok(PathBuf::from(dir).join("config.toml"));
    }
    let base = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .ok_or_else(|| message("HOME is not set; cannot locate the Herdr config"))?;
    Ok(base.join("herdr").join("config.toml"))
}

pub(crate) fn setup() -> Result<()> {
    let path = config_path()?;
    let original = if path.exists() {
        fs::read_to_string(&path)?
    } else {
        String::new()
    };

    let mut config = original.clone();
    let mut lines = Vec::new();
    if config.contains("command = \"herdr-dock.create\"") {
        lines.push("prefix+d already bound".to_string());
    } else {
        config.push_str(CREATE_BINDING);
        lines.push("added prefix+d (create dock)".to_string());
    }
    if config.contains("command = \"herdr-dock.overview\"") {
        lines.push("prefix+o already bound".to_string());
    } else {
        config.push_str(OVERVIEW_BINDING);
        lines.push("added prefix+o (dock overview)".to_string());
    }

    if config == original {
        lines.push(format!("keybindings already present in {}", path.display()));
    } else {
        if path.exists() {
            let backup = path.with_extension("toml.bak");
            fs::write(&backup, &original)?;
            lines.push(format!("backed up config to {}", backup.display()));
        }
        fs::write(&path, config)?;
        lines.push(format!("wrote keybindings to {}", path.display()));
    }
    lines.push("Reload with: herdr server reload-config".to_string());
    lines.push("Then press prefix+d to create a dock and prefix+o for the overview.".to_string());

    if io::stdin().is_terminal() {
        let mut ui = crate::ui::Ui::start()?;
        crate::ui::show_notice(&mut ui, "herdr-dock · hotkeys", &lines.join("\n"))?;
    } else {
        for line in &lines {
            println!("herdr-dock: {line}");
        }
    }
    Ok(())
}
