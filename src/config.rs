use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct AppConfig {
    #[serde(default = "default_theme")]
    pub theme: String,
    #[serde(default)]
    pub keybinds: HashMap<String, String>,
    #[serde(default)]
    pub custom_commands: Vec<CustomCommand>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CustomCommand {
    pub id: String,
    pub title: String,
    pub context: CommandContext,
    pub command: String,
    #[serde(default)]
    pub needs_confirmation: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CommandContext {
    Repo,
    File,
    Revision,
}

fn default_theme() -> String {
    "auto".to_string()
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            theme: default_theme(),
            keybinds: HashMap::new(),
            custom_commands: Vec::new(),
        }
    }
}

pub fn load_config() -> AppConfig {
    match config_path() {
        Some(path) => read_config(path).unwrap_or_default(),
        None => AppConfig::default(),
    }
}

fn config_path() -> Option<PathBuf> {
    let mut base = dirs::config_dir()?;
    base.push("easyhg");
    base.push("config.toml");
    Some(base)
}

fn read_config(path: PathBuf) -> Option<AppConfig> {
    if !path.exists() {
        return None;
    }
    let raw = fs::read_to_string(path).ok()?;
    toml::from_str::<AppConfig>(&raw).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_config() {
        let raw = r#"
theme = "dark"

[keybinds]
commit = "C"

[[custom_commands]]
id = "lint"
title = "Run Lint"
context = "repo"
command = "cargo clippy"
needs_confirmation = true
"#;
        let config = toml::from_str::<AppConfig>(raw).expect("config parses");
        assert_eq!(config.theme, "dark");
        assert_eq!(config.keybinds.get("commit"), Some(&"C".to_string()));
        assert_eq!(config.custom_commands.len(), 1);
        assert!(config.custom_commands[0].needs_confirmation);
    }
}
