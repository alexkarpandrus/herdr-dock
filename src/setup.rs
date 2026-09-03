use crate::Result;
use crate::git::message;
use std::{env, fs, path::PathBuf};

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
    let mut added = false;
    if config.contains("command = \"herdr-dock.create\"") {
        println!("herdr-dock: prefix+d already bound");
    } else {
        config.push_str(CREATE_BINDING);
        added = true;
    }
    if config.contains("command = \"herdr-dock.overview\"") {
        println!("herdr-dock: prefix+o already bound");
    } else {
        config.push_str(OVERVIEW_BINDING);
        added = true;
    }

    if !added {
        println!(
            "herdr-dock: keybindings already present in {}",
            path.display()
        );
        println!("Reload with: herdr server reload-config");
        return Ok(());
    }

    if path.exists() {
        let backup = path.with_extension("toml.bak");
        fs::write(&backup, &original)?;
        println!("herdr-dock: backed up config to {}", backup.display());
    }
    fs::write(&path, config)?;
    println!("herdr-dock: added keybindings to {}", path.display());
    println!("Reload with: herdr server reload-config");
    println!("Then press prefix+d to create a dock and prefix+o for the overview.");
    Ok(())
}
