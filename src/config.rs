use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use serde::Deserialize;

use crate::actions;

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
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default = "default_show_output")]
    pub show_output: bool,
    #[serde(default)]
    pub needs_confirmation: bool,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CommandContext {
    Repo,
    File,
    Revision,
}

fn default_theme() -> String {
    "auto".to_string()
}

fn default_show_output() -> bool {
    true
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

#[derive(Debug, Clone)]
pub struct ConfigLoadReport {
    pub config: AppConfig,
    pub path: Option<PathBuf>,
    pub issues: Vec<String>,
}

#[allow(dead_code)]
pub fn load_config() -> AppConfig {
    load_config_with_report().config
}

pub fn load_config_with_report() -> ConfigLoadReport {
    let path = default_config_path();
    let mut issues = Vec::new();
    let config = match path.clone() {
        Some(path) => match read_config(&path) {
            Ok(Some(config)) => config,
            Ok(None) => AppConfig::default(),
            Err(err) => {
                issues.push(err);
                AppConfig::default()
            }
        },
        None => {
            issues.push("failed to locate user config directory".to_string());
            AppConfig::default()
        }
    };

    issues.extend(validate_config(&config));

    ConfigLoadReport {
        config,
        path,
        issues,
    }
}

pub fn default_config_path() -> Option<PathBuf> {
    let mut base = dirs::config_dir()?;
    base.push("easyhg");
    base.push("config.toml");
    Some(base)
}

fn read_config(path: &PathBuf) -> Result<Option<AppConfig>, String> {
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(path).map_err(|err| format!("failed reading {path:?}: {err}"))?;
    let config = toml::from_str::<AppConfig>(&raw)
        .map_err(|err| format!("failed parsing {path:?} as TOML: {err}"))?;
    Ok(Some(config))
}

pub fn validate_config(config: &AppConfig) -> Vec<String> {
    let mut issues = Vec::new();
    match config.theme.trim() {
        "auto" | "light" | "dark" => {}
        other => issues.push(format!(
            "invalid theme '{other}' (expected: auto, light, dark)"
        )),
    }

    issues.extend(actions::validate_key_overrides(&config.keybinds));

    let mut ids = std::collections::HashSet::new();
    for command in &config.custom_commands {
        if command.id.trim().is_empty() {
            issues.push("custom command has empty id".to_string());
        }
        if command.title.trim().is_empty() {
            issues.push(format!("custom command '{}' has empty title", command.id));
        }
        if command.command.trim().is_empty() {
            issues.push(format!("custom command '{}' has empty command", command.id));
        }
        for arg in &command.args {
            if arg.trim().is_empty() {
                issues.push(format!(
                    "custom command '{}' has an empty arg entry",
                    command.id
                ));
                break;
            }
        }
        for key in command.env.keys() {
            if key.trim().is_empty() {
                issues.push(format!("custom command '{}' has empty env key", command.id));
                break;
            }
        }
        if !command.id.trim().is_empty() && !ids.insert(command.id.clone()) {
            issues.push(format!("duplicate custom command id '{}'", command.id));
        }
    }
    issues
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
args = ["--all-targets"]
show_output = true
needs_confirmation = true
"#;
        let config = toml::from_str::<AppConfig>(raw).expect("config parses");
        assert_eq!(config.theme, "dark");
        assert_eq!(config.keybinds.get("commit"), Some(&"C".to_string()));
        assert_eq!(config.custom_commands.len(), 1);
        assert!(config.custom_commands[0].needs_confirmation);
        assert_eq!(config.custom_commands[0].args, vec!["--all-targets"]);
        assert!(config.custom_commands[0].show_output);
    }

    #[test]
    fn validate_config_reports_errors() {
        let mut config = AppConfig::default();
        config.theme = "neon".to_string();
        config
            .keybinds
            .insert("unknown_action".to_string(), "x".to_string());
        config.custom_commands = vec![
            CustomCommand {
                id: "dup".to_string(),
                title: "".to_string(),
                context: CommandContext::Repo,
                command: "".to_string(),
                args: vec!["".to_string()],
                env: HashMap::new(),
                show_output: true,
                needs_confirmation: false,
            },
            CustomCommand {
                id: "dup".to_string(),
                title: "ok".to_string(),
                context: CommandContext::Repo,
                command: "echo hi".to_string(),
                args: Vec::new(),
                env: HashMap::new(),
                show_output: true,
                needs_confirmation: false,
            },
        ];

        let issues = validate_config(&config);
        assert!(issues.iter().any(|line| line.contains("invalid theme")));
        assert!(
            issues
                .iter()
                .any(|line| line.contains("unknown keybinding action"))
        );
        assert!(issues.iter().any(|line| line.contains("empty title")));
        assert!(issues.iter().any(|line| line.contains("empty command")));
        assert!(issues.iter().any(|line| line.contains("empty arg entry")));
        assert!(
            issues
                .iter()
                .any(|line| line.contains("duplicate custom command id"))
        );
    }
}
