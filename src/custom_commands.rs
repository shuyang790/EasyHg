use std::collections::{HashMap, HashSet};
use std::hash::Hash;

pub const SUPPORTED_TEMPLATE_VARS: &[&str] = &["repo_root", "branch", "file", "rev", "node"];

pub fn parse_command_parts(raw: &str) -> Result<(String, Vec<String>), String> {
    #[derive(Clone, Copy)]
    enum QuoteMode {
        Single,
        Double,
    }

    let mut parts = Vec::new();
    let mut current = String::new();
    let mut quote_mode = None;
    let mut chars = raw.chars().peekable();

    while let Some(ch) = chars.next() {
        match quote_mode {
            Some(QuoteMode::Single) => {
                if ch == '\'' {
                    quote_mode = None;
                } else {
                    current.push(ch);
                }
            }
            Some(QuoteMode::Double) => match ch {
                '"' => quote_mode = None,
                '\\' => {
                    let escaped = chars
                        .next()
                        .ok_or_else(|| "custom command has trailing escape".to_string())?;
                    current.push(escaped);
                }
                _ => current.push(ch),
            },
            None => match ch {
                '\'' => quote_mode = Some(QuoteMode::Single),
                '"' => quote_mode = Some(QuoteMode::Double),
                '\\' => {
                    let escaped = chars
                        .next()
                        .ok_or_else(|| "custom command has trailing escape".to_string())?;
                    current.push(escaped);
                }
                c if c.is_whitespace() => {
                    if !current.is_empty() {
                        parts.push(std::mem::take(&mut current));
                    }
                    while chars.peek().is_some_and(|peek| peek.is_whitespace()) {
                        chars.next();
                    }
                }
                _ => current.push(ch),
            },
        }
    }

    if quote_mode.is_some() {
        return Err("custom command has unmatched quote".to_string());
    }
    if !current.is_empty() {
        parts.push(current);
    }
    if parts.is_empty() {
        return Err("custom command has empty executable".to_string());
    }
    Ok((parts[0].clone(), parts[1..].to_vec()))
}

pub fn render_template(raw: &str, vars: &HashMap<&str, String>) -> String {
    let mut rendered = raw.to_string();
    for (name, value) in vars {
        rendered = rendered.replace(&format!("{{{name}}}"), value);
    }
    rendered
}

pub fn template_vars(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut idx = 0usize;
    while idx < raw.len() {
        let remainder = &raw[idx..];
        let Some(start_off) = remainder.find('{') else {
            break;
        };
        let start = idx + start_off;
        let after_start = start + 1;
        if after_start >= raw.len() {
            break;
        }
        let Some(end_off) = raw[after_start..].find('}') else {
            break;
        };
        let end = after_start + end_off;
        let candidate = &raw[after_start..end];
        if is_template_var_name(candidate) && seen.insert(candidate) {
            out.push(candidate.to_string());
        }
        idx = end + 1;
    }
    out
}

pub fn unknown_template_vars(raw: &str) -> Vec<String> {
    let supported = SUPPORTED_TEMPLATE_VARS
        .iter()
        .copied()
        .collect::<HashSet<_>>();
    template_vars(raw)
        .into_iter()
        .filter(|name| !supported.contains(name.as_str()))
        .collect()
}

pub fn unresolved_template_vars<K>(raw: &str, vars: &HashMap<K, String>) -> Vec<String>
where
    K: Eq + Hash + AsRef<str>,
{
    let available = vars.keys().map(AsRef::as_ref).collect::<HashSet<_>>();
    template_vars(raw)
        .into_iter()
        .filter(|name| !available.contains(name.as_str()))
        .collect()
}

fn is_template_var_name(raw: &str) -> bool {
    let mut chars = raw.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_template_vars() {
        let names = template_vars("echo {repo_root} {branch} {repo_root} {rev}");
        assert_eq!(names, vec!["repo_root", "branch", "rev"]);
    }

    #[test]
    fn unknown_template_vars_reports_unsupported_names() {
        let names = unknown_template_vars("echo {repo_root} {bogus} {also_bad}");
        assert_eq!(names, vec!["bogus", "also_bad"]);
    }
}
