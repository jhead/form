//! Small helpers shared by the four OpenAI adapters.
//!
//! What is left here is genuinely OpenAI-shaped: [`MappedLevel`] and the
//! provider-error formatting the four adapters' tests assert on. The generic
//! ports that used to live alongside them have moved to the crates that own
//! them — `shortHash` and `getProviderEnvValue` to `pi-http`,
//! `calculateCost` to `pi-provider-common`, `clampThinkingLevel` and
//! `getSupportedThinkingLevels` onto [`pi_core::Model`] itself.
//!
//! Two things stayed on purpose. `pi_http::headers::pi_user_agent` renders
//! `pi (os arch)`, while `utils/pi-user-agent.ts` renders
//! `pi (os release; arch)` — the OpenAI adapters send the upstream spelling
//! because some proxies gate on it, so [`pi_user_agent`] here is not the same
//! string and cannot be swapped. Likewise `pi_http::format_provider_error`
//! takes a `NormalizedProviderError`; these adapters format from an already
//! folded [`AiError`], which is a different input, not a duplicate function.

use pi_core::model::ModelThinkingLevel;
use pi_core::options::{ProviderEnv, ProviderHeaders};
use pi_core::{AiError, Model};

/// Port of `sanitizeSurrogates` — the identity on a Rust `str`. See
/// [`pi_provider_common::sanitize_unicode`] for why it still exists.
pub use pi_provider_common::sanitize_unicode::sanitize_surrogates;

/// Port of `utils/hash.ts#shortHash`. Lives in `pi-http`; re-exported so the
/// call sites here stay diffable against upstream.
pub use pi_http::hash::short_hash;

/// Scoped overrides win over the process environment. Thin alias so the call
/// sites read like upstream; the implementation lives in `pi-http`.
pub fn provider_env_value(name: &str, env: &ProviderEnv) -> Option<String> {
    pi_http::provider_env::get_provider_env_value(name, env)
}

/// Port of `utils/pi-user-agent.ts#getPiUserAgent`.
pub fn pi_user_agent() -> String {
    format!(
        "pi ({} {}; {})",
        std::env::consts::OS,
        os_release(),
        std::env::consts::ARCH
    )
}

fn os_release() -> String {
    // `os.release()` has no portable Rust equivalent; the string is only used as
    // a UA hint, so fall back to the target family when it is unavailable.
    std::env::var("PI_OS_RELEASE").unwrap_or_else(|_| std::env::consts::FAMILY.to_string())
}

/// Port of `forcePiUserAgent`: drop any caller `user-agent` spelling, then set ours.
pub fn force_pi_user_agent(headers: &mut ProviderHeaders) {
    let existing: Vec<String> = headers
        .keys()
        .filter(|k| k.eq_ignore_ascii_case("user-agent"))
        .cloned()
        .collect();
    for key in existing {
        headers.remove(&key);
    }
    headers.insert("User-Agent".to_string(), Some(pi_user_agent()));
}

/// Port of `models.ts#calculateCost`. Mutates `usage.cost` in place.
pub use pi_provider_common::cost::calculate_cost;

/// The provider value for a thinking level, honouring `model.thinkingLevelMap`.
///
/// Returns `Mapped::Missing` when the map has no entry (caller falls back to the
/// pi level name), `Mapped::Null` for an explicit `null` (unsupported), and
/// `Mapped::Value` otherwise. This tri-state matters: upstream distinguishes
/// `undefined` from `null` in almost every thinking-format branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MappedLevel {
    Missing,
    Null,
    Value(String),
}

impl MappedLevel {
    pub fn lookup(model: &Model, level: ModelThinkingLevel) -> Self {
        match model
            .thinking_level_map
            .as_ref()
            .and_then(|m| m.get(&level))
        {
            None => MappedLevel::Missing,
            Some(None) => MappedLevel::Null,
            Some(Some(value)) => MappedLevel::Value(value.clone()),
        }
    }

    /// `model.thinkingLevelMap?.[level] ?? fallback` — `null` also falls back,
    /// matching JavaScript's `??` on an explicit null.
    pub fn or(self, fallback: &str) -> String {
        match self {
            MappedLevel::Value(value) => value,
            _ => fallback.to_string(),
        }
    }

    /// `const x = map[level]; typeof x === "string" ? x : requested` — the shape
    /// used by the baseten/zai branches, where `undefined` falls back but an
    /// explicit `null` suppresses the field.
    pub fn value_or_requested(self, requested: Option<&str>) -> Option<String> {
        match self {
            MappedLevel::Value(value) => Some(value),
            MappedLevel::Missing => requested.map(str::to_string),
            MappedLevel::Null => None,
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, MappedLevel::Null)
    }
}

/// Cap for the error body echoed into `AssistantMessage.errorMessage`.
pub const MAX_PROVIDER_ERROR_BODY_CHARS: usize = 4000;

pub fn truncate_error_text(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let head: String = text.chars().take(max_chars).collect();
    let dropped = text.chars().count() - max_chars;
    format!("{head}... [truncated {dropped} chars]")
}

/// Port of `formatProviderError(normalizeProviderError(error), prefix)`.
///
/// The Rust transport already folds the provider envelope into
/// [`AiError::Provider`], so the "probe the SDK error shape" half of upstream is
/// unnecessary; what remains is the display composition, which tests assert on.
pub fn format_provider_error(error: &AiError, prefix: Option<&str>) -> String {
    match error {
        AiError::Provider {
            status,
            message,
            body,
            ..
        } => {
            let body_text = body
                .as_ref()
                .map(|b| truncate_error_text(b.to_string().trim(), MAX_PROVIDER_ERROR_BODY_CHARS))
                .filter(|b| !b.is_empty() && !message.contains(b.as_str()));
            match (prefix, body_text) {
                (Some(prefix), Some(body)) => format!("{prefix} ({status}): {body}"),
                (Some(prefix), None) => format!("{prefix} ({status}): {message}"),
                (None, Some(body)) => format!("{status}: {body}"),
                (None, None) => message.clone(),
            }
        }
        other => match prefix {
            Some(prefix) => format!("{prefix}: {}", other.message()),
            None => other.message(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `short_hash` itself is `pi-http`'s and tested there; this pins the two
    /// digests that reach the OpenAI wire, because tool-call ids derived from
    /// them are replayed out of stored sessions.
    #[test]
    fn short_hash_matches_upstream_reference_values() {
        // Reference values produced by the upstream TypeScript implementation.
        assert_eq!(short_hash(""), "k4n83c7h0j2b");
        assert_eq!(short_hash("hello"), "1h6qa0qrowduu");
        assert_eq!(short_hash("call_abc123"), "sb0y391xa16ki");
        assert_eq!(short_hash("fc_1234567890"), "705r0c1vomy0g");
    }
}
