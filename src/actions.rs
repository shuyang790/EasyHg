use std::collections::{HashMap, HashSet};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionId {
    Quit,
    Help,
    FocusNext,
    FocusPrev,
    MoveDown,
    MoveUp,
    RefreshSnapshot,
    RefreshDetails,
    Commit,
    Bookmark,
    Shelve,
    Push,
    Pull,
    Incoming,
    Outgoing,
    UpdateSelected,
    UnshelveSelected,
    ResolveMark,
    ResolveUnmark,
    RebaseSelected,
    HisteditSelected,
    HardRefresh,
}

impl ActionId {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Quit => "quit",
            Self::Help => "help",
            Self::FocusNext => "focus_next",
            Self::FocusPrev => "focus_prev",
            Self::MoveDown => "move_down",
            Self::MoveUp => "move_up",
            Self::RefreshSnapshot => "refresh_snapshot",
            Self::RefreshDetails => "refresh_details",
            Self::Commit => "commit",
            Self::Bookmark => "bookmark",
            Self::Shelve => "shelve",
            Self::Push => "push",
            Self::Pull => "pull",
            Self::Incoming => "incoming",
            Self::Outgoing => "outgoing",
            Self::UpdateSelected => "update_selected",
            Self::UnshelveSelected => "unshelve_selected",
            Self::ResolveMark => "resolve_mark",
            Self::ResolveUnmark => "resolve_unmark",
            Self::RebaseSelected => "rebase_selected",
            Self::HisteditSelected => "histedit_selected",
            Self::HardRefresh => "hard_refresh",
        }
    }

    pub fn from_str(raw: &str) -> Option<Self> {
        match raw.trim() {
            "quit" => Some(Self::Quit),
            "help" => Some(Self::Help),
            "focus_next" => Some(Self::FocusNext),
            "focus_prev" => Some(Self::FocusPrev),
            "move_down" => Some(Self::MoveDown),
            "move_up" => Some(Self::MoveUp),
            "refresh_snapshot" => Some(Self::RefreshSnapshot),
            "refresh_details" => Some(Self::RefreshDetails),
            "commit" => Some(Self::Commit),
            "bookmark" => Some(Self::Bookmark),
            "shelve" => Some(Self::Shelve),
            "push" => Some(Self::Push),
            "pull" => Some(Self::Pull),
            "incoming" => Some(Self::Incoming),
            "outgoing" => Some(Self::Outgoing),
            "update_selected" => Some(Self::UpdateSelected),
            "unshelve_selected" => Some(Self::UnshelveSelected),
            "resolve_mark" => Some(Self::ResolveMark),
            "resolve_unmark" => Some(Self::ResolveUnmark),
            "rebase_selected" => Some(Self::RebaseSelected),
            "histedit_selected" => Some(Self::HisteditSelected),
            "hard_refresh" => Some(Self::HardRefresh),
            _ => None,
        }
    }

    pub fn all() -> &'static [Self] {
        &[
            Self::Quit,
            Self::Help,
            Self::FocusNext,
            Self::FocusPrev,
            Self::MoveDown,
            Self::MoveUp,
            Self::RefreshSnapshot,
            Self::RefreshDetails,
            Self::Commit,
            Self::Bookmark,
            Self::Shelve,
            Self::Push,
            Self::Pull,
            Self::Incoming,
            Self::Outgoing,
            Self::UpdateSelected,
            Self::UnshelveSelected,
            Self::ResolveMark,
            Self::ResolveUnmark,
            Self::RebaseSelected,
            Self::HisteditSelected,
            Self::HardRefresh,
        ]
    }
}

pub const DEFAULT_BINDINGS: &[(ActionId, &str)] = &[
    (ActionId::Quit, "q"),
    (ActionId::Help, "?"),
    (ActionId::FocusNext, "tab"),
    (ActionId::FocusPrev, "shift+tab"),
    (ActionId::MoveDown, "down"),
    (ActionId::MoveDown, "j"),
    (ActionId::MoveUp, "up"),
    (ActionId::MoveUp, "k"),
    (ActionId::RefreshSnapshot, "r"),
    (ActionId::RefreshDetails, "d"),
    (ActionId::Commit, "c"),
    (ActionId::Bookmark, "b"),
    (ActionId::Shelve, "s"),
    (ActionId::Push, "p"),
    (ActionId::Pull, "P"),
    (ActionId::Incoming, "i"),
    (ActionId::Outgoing, "o"),
    (ActionId::UpdateSelected, "u"),
    (ActionId::UnshelveSelected, "U"),
    (ActionId::ResolveMark, "m"),
    (ActionId::ResolveUnmark, "M"),
    (ActionId::RebaseSelected, "R"),
    (ActionId::HisteditSelected, "H"),
    (ActionId::HardRefresh, "ctrl+l"),
];

#[derive(Debug, Clone)]
pub struct ActionKeyMap {
    event_to_action: HashMap<String, ActionId>,
    primary_for_action: HashMap<ActionId, String>,
}

impl ActionKeyMap {
    pub fn from_overrides(overrides: &HashMap<String, String>) -> Result<Self, Vec<String>> {
        let mut issues = Vec::new();

        let mut action_to_keys = HashMap::<ActionId, Vec<String>>::new();
        let mut event_to_action = HashMap::<String, ActionId>::new();
        for (action, key) in DEFAULT_BINDINGS {
            let canonical = canonicalize_key_binding(key).expect("default key is valid");
            event_to_action.insert(canonical.clone(), *action);
            action_to_keys.entry(*action).or_default().push(canonical);
        }

        let mut parsed_overrides = Vec::<(ActionId, String)>::new();
        for (action_name, key_raw) in overrides {
            let Some(action) = ActionId::from_str(action_name) else {
                issues.push(format!(
                    "unknown keybinding action '{action_name}' (expected one of: {})",
                    ActionId::all()
                        .iter()
                        .map(|id| id.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
                continue;
            };
            match canonicalize_key_binding(key_raw) {
                Ok(canonical) => {
                    parsed_overrides.push((action, canonical));
                }
                Err(err) => {
                    issues.push(format!("invalid keybinding for '{action_name}': {err}"));
                }
            }
        }

        for (action, key) in parsed_overrides {
            action_to_keys.insert(action, vec![key]);
        }

        event_to_action.clear();
        let mut primary_for_action = HashMap::<ActionId, String>::new();
        let mut seen = HashSet::<String>::new();
        for (action, keys) in action_to_keys {
            if keys.is_empty() {
                issues.push(format!("no keybinding for action '{}'", action.as_str()));
                continue;
            }
            primary_for_action.insert(action, keys[0].clone());
            for key in keys {
                if !seen.insert(key.clone()) {
                    issues.push(format!("duplicate keybinding '{key}'"));
                    continue;
                }
                event_to_action.insert(key, action);
            }
        }

        if !issues.is_empty() {
            return Err(issues);
        }

        Ok(Self {
            event_to_action,
            primary_for_action,
        })
    }

    pub fn action_for_event(&self, key: KeyEvent) -> Option<ActionId> {
        let canonical = canonicalize_key_event(key)?;
        self.event_to_action.get(&canonical).copied()
    }

    pub fn key_for_action(&self, action: ActionId) -> Option<&str> {
        self.primary_for_action.get(&action).map(String::as_str)
    }
}

pub fn validate_key_overrides(overrides: &HashMap<String, String>) -> Vec<String> {
    ActionKeyMap::from_overrides(overrides)
        .err()
        .unwrap_or_default()
}

pub fn canonicalize_key_binding(raw: &str) -> Result<String, String> {
    let text = raw.trim();
    if text.is_empty() {
        return Err("empty keybinding".to_string());
    }
    let mut tokens = text.split('+').map(str::trim).collect::<Vec<_>>();
    if tokens.iter().any(|t| t.is_empty()) {
        return Err(format!("invalid keybinding '{text}'"));
    }

    let key_token = tokens.pop().expect("non-empty after trim");
    let mut ctrl = false;
    let mut alt = false;
    let mut shift = false;

    for modifier in tokens {
        match modifier.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => ctrl = true,
            "alt" => alt = true,
            "shift" => shift = true,
            other => return Err(format!("unknown modifier '{other}'")),
        }
    }

    let key = normalize_key_token(key_token, shift)?;
    Ok(canonical_key_string(key, ctrl, alt, shift))
}

fn normalize_key_token(token: &str, shift: bool) -> Result<String, String> {
    let key = token.trim();
    if key.chars().count() == 1 {
        return Ok(key.to_string());
    }
    match key.to_ascii_lowercase().as_str() {
        "tab" => Ok("tab".to_string()),
        "backtab" => {
            if !shift {
                return Ok("tab".to_string());
            }
            Ok("tab".to_string())
        }
        "up" => Ok("up".to_string()),
        "down" => Ok("down".to_string()),
        "enter" => Ok("enter".to_string()),
        "esc" | "escape" => Ok("esc".to_string()),
        "backspace" => Ok("backspace".to_string()),
        _ => Err(format!("unknown key '{key}'")),
    }
}

fn canonicalize_key_event(event: KeyEvent) -> Option<String> {
    let ctrl = event.modifiers.contains(KeyModifiers::CONTROL);
    let alt = event.modifiers.contains(KeyModifiers::ALT);
    let mut shift = event.modifiers.contains(KeyModifiers::SHIFT);

    let key = match event.code {
        KeyCode::Char(c) => {
            // Char event already captures case; shift modifier does not need to be part of identity.
            shift = false;
            c.to_string()
        }
        KeyCode::Tab => "tab".to_string(),
        KeyCode::BackTab => {
            shift = true;
            "tab".to_string()
        }
        KeyCode::Up => "up".to_string(),
        KeyCode::Down => "down".to_string(),
        KeyCode::Enter => "enter".to_string(),
        KeyCode::Esc => "esc".to_string(),
        KeyCode::Backspace => "backspace".to_string(),
        _ => return None,
    };

    Some(canonical_key_string(key, ctrl, alt, shift))
}

fn canonical_key_string(key: String, ctrl: bool, alt: bool, shift: bool) -> String {
    let mut parts = Vec::new();
    if ctrl {
        parts.push("ctrl".to_string());
    }
    if alt {
        parts.push("alt".to_string());
    }
    if shift {
        parts.push("shift".to_string());
    }
    parts.push(key);
    parts.join("+")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_named_and_char_keys() {
        assert_eq!(canonicalize_key_binding("tab").expect("tab"), "tab");
        assert_eq!(
            canonicalize_key_binding("shift+tab").expect("shift tab"),
            "shift+tab"
        );
        assert_eq!(canonicalize_key_binding("P").expect("char"), "P");
        assert_eq!(
            canonicalize_key_binding("ctrl+l").expect("ctrl+l"),
            "ctrl+l"
        );
    }

    #[test]
    fn rejects_invalid_keybinding_tokens() {
        let err = canonicalize_key_binding("meta+x").expect_err("invalid modifier");
        assert!(err.contains("unknown modifier"));
        let err = canonicalize_key_binding("ctrl+space").expect_err("invalid key");
        assert!(err.contains("unknown key"));
    }

    #[test]
    fn override_validation_catches_unknown_action_and_duplicates() {
        let mut overrides = HashMap::new();
        overrides.insert("bogus".to_string(), "x".to_string());
        overrides.insert("quit".to_string(), "x".to_string());
        overrides.insert("help".to_string(), "x".to_string());
        let issues = validate_key_overrides(&overrides);
        assert!(
            issues
                .iter()
                .any(|line| line.contains("unknown keybinding action"))
        );
        assert!(
            issues
                .iter()
                .any(|line| line.contains("duplicate keybinding"))
        );
    }
}
