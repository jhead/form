//! Port of `packages/coding-agent/src/core/resolve-config-value.ts`.
//!
//! `auth.json` may store an indirection instead of a literal key —
//! `"$ANTHROPIC_API_KEY"`, `"${VAR}-suffix"`, or `"!security find-generic-password …"`.
//! The TypeScript implementation resolves these on read, so a shared machine's
//! `auth.json` only works here if we resolve them identically.

use std::collections::HashMap;
use std::sync::OnceLock;

use parking_lot::Mutex;
use pi_core::options::ProviderEnv;

#[derive(Debug, PartialEq, Eq)]
enum TemplatePart {
    Literal(String),
    Env(String),
}

#[derive(Debug, PartialEq, Eq)]
enum ConfigValue {
    /// `!cmd` — run through the shell, use trimmed stdout.
    Command(String),
    Template(Vec<TemplatePart>),
}

fn is_env_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_env_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn valid_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if is_env_start(c)) && chars.all(is_env_char)
}

fn append_literal(parts: &mut Vec<TemplatePart>, value: &str) {
    if value.is_empty() {
        return;
    }
    if let Some(TemplatePart::Literal(last)) = parts.last_mut() {
        last.push_str(value);
        return;
    }
    parts.push(TemplatePart::Literal(value.to_string()));
}

fn parse_template(config: &str) -> Vec<TemplatePart> {
    let mut parts = Vec::new();
    let bytes = config.as_bytes();
    let mut index = 0usize;

    while index < bytes.len() {
        let Some(offset) = config[index..].find('$') else {
            append_literal(&mut parts, &config[index..]);
            break;
        };
        let dollar = index + offset;
        append_literal(&mut parts, &config[index..dollar]);

        let next = config[dollar + 1..].chars().next();
        match next {
            // `$$` and `$!` escape a literal `$` / `!`.
            Some(c @ ('$' | '!')) => {
                append_literal(&mut parts, &c.to_string());
                index = dollar + 2;
            }
            Some('{') => {
                let start = dollar + 2;
                match config[start..].find('}') {
                    Some(rel_end) => {
                        let end = start + rel_end;
                        let name = &config[start..end];
                        if valid_env_name(name) {
                            parts.push(TemplatePart::Env(name.to_string()));
                        } else {
                            append_literal(&mut parts, &config[dollar..=end]);
                        }
                        index = end + 1;
                    }
                    None => {
                        append_literal(&mut parts, "$");
                        index = dollar + 1;
                    }
                }
            }
            Some(c) if is_env_start(c) => {
                let rest = &config[dollar + 1..];
                let len = rest
                    .char_indices()
                    .take_while(|(_, c)| is_env_char(*c))
                    .map(|(i, c)| i + c.len_utf8())
                    .last()
                    .unwrap_or(0);
                parts.push(TemplatePart::Env(rest[..len].to_string()));
                index = dollar + 1 + len;
            }
            _ => {
                append_literal(&mut parts, "$");
                index = dollar + 1;
            }
        }
    }

    parts
}

fn parse(config: &str) -> ConfigValue {
    if let Some(command) = config.strip_prefix('!') {
        return ConfigValue::Command(command.to_string());
    }
    ConfigValue::Template(parse_template(config))
}

fn resolve_env(name: &str, env: Option<&ProviderEnv>) -> Option<String> {
    env.and_then(|e| e.get(name))
        .filter(|v| !v.is_empty())
        .cloned()
        .or_else(|| std::env::var(name).ok().filter(|v| !v.is_empty()))
}

fn resolve_template(parts: &[TemplatePart], env: Option<&ProviderEnv>) -> Option<String> {
    let mut resolved = String::new();
    for part in parts {
        match part {
            TemplatePart::Literal(value) => resolved.push_str(value),
            TemplatePart::Env(name) => resolved.push_str(&resolve_env(name, env)?),
        }
    }
    Some(resolved)
}

/// Whether the stored value runs a command. Callers that must stay
/// side-effect-free (credential *listing*) check this first.
pub fn is_command_config_value(config: &str) -> bool {
    matches!(parse(config), ConfigValue::Command(_))
}

/// Environment variables a template value depends on, in first-seen order.
pub fn config_value_env_var_names(config: &str) -> Vec<String> {
    match parse(config) {
        ConfigValue::Command(_) => Vec::new(),
        ConfigValue::Template(parts) => {
            let mut names: Vec<String> = Vec::new();
            for part in parts {
                if let TemplatePart::Env(name) = part {
                    if !names.contains(&name) {
                        names.push(name);
                    }
                }
            }
            names
        }
    }
}

/// Resolve a stored config value. Command results are cached for the life of
/// the process, exactly as upstream does — a keychain prompt must not fire on
/// every credential read.
pub fn resolve_config_value(config: &str, env: Option<&ProviderEnv>) -> Option<String> {
    match parse(config) {
        ConfigValue::Command(command) => execute_command_cached(&command),
        ConfigValue::Template(parts) => resolve_template(&parts, env),
    }
}

/// Same, without consulting or filling the command cache.
pub fn resolve_config_value_uncached(config: &str, env: Option<&ProviderEnv>) -> Option<String> {
    match parse(config) {
        ConfigValue::Command(command) => execute_command(&command),
        ConfigValue::Template(parts) => resolve_template(&parts, env),
    }
}

fn command_cache() -> &'static Mutex<HashMap<String, Option<String>>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<String>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn execute_command_cached(command: &str) -> Option<String> {
    if let Some(cached) = command_cache().lock().get(command) {
        return cached.clone();
    }
    let result = execute_command(command);
    command_cache()
        .lock()
        .insert(command.to_string(), result.clone());
    result
}

/// Runs the command through the platform shell with stdin and stderr closed, so
/// nothing this crate spawns can grab the host's terminal.
fn execute_command(command: &str) -> Option<String> {
    use std::process::{Command, Stdio};

    #[cfg(windows)]
    let mut cmd = {
        let mut cmd = Command::new("cmd");
        cmd.arg("/C").arg(command);
        cmd
    };
    #[cfg(not(windows))]
    let mut cmd = {
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg(command);
        cmd
    };

    let output = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

/// Clear the command cache. Exported for tests, as upstream does.
pub fn clear_config_value_cache() {
    command_cache().lock().clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> ProviderEnv {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn literal_values_pass_through() {
        assert_eq!(
            resolve_config_value("sk-literal", None).as_deref(),
            Some("sk-literal")
        );
    }

    #[test]
    fn credential_scoped_env_takes_precedence() {
        let scoped = env(&[("SCOPED_KEY", "scoped-value")]);
        assert_eq!(
            resolve_config_value("$SCOPED_KEY", Some(&scoped)).as_deref(),
            Some("scoped-value")
        );
        assert_eq!(
            resolve_config_value("${SCOPED_KEY}-suffix", Some(&scoped)).as_deref(),
            Some("scoped-value-suffix")
        );
    }

    #[test]
    fn a_missing_variable_makes_the_whole_template_unresolved() {
        assert_eq!(
            resolve_config_value("$PI_AUTH_DEFINITELY_UNSET", None),
            None
        );
    }

    #[test]
    fn dollar_and_bang_escapes_are_literals() {
        assert_eq!(
            resolve_config_value("$$literal", None).as_deref(),
            Some("$literal")
        );
        assert_eq!(
            resolve_config_value("$!literal", None).as_deref(),
            Some("!literal")
        );
        assert!(!is_command_config_value("$!literal"));
    }

    #[test]
    fn unterminated_brace_stays_literal() {
        assert_eq!(
            resolve_config_value("${OPEN", None).as_deref(),
            Some("${OPEN")
        );
    }

    #[test]
    fn env_var_names_are_reported_in_first_seen_order() {
        assert_eq!(
            config_value_env_var_names("$A-${B}-$A"),
            vec!["A".to_string(), "B".to_string()]
        );
        assert!(config_value_env_var_names("!printf hi").is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn command_values_resolve_through_the_shell() {
        assert!(is_command_config_value("!printf 'command-key'"));
        assert_eq!(
            resolve_config_value_uncached("!printf 'command-key'", None).as_deref(),
            Some("command-key")
        );
    }

    #[cfg(unix)]
    #[test]
    fn failing_commands_resolve_to_nothing() {
        assert_eq!(resolve_config_value_uncached("!exit 1", None), None);
    }
}
