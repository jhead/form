//! Header merging. Port of `packages/ai/src/utils/headers.ts`.

use std::collections::BTreeMap;

use pi_core::options::ProviderHeaders;

pub type HeaderMap = BTreeMap<String, String>;

/// Merge caller overrides onto provider defaults.
///
/// Caller values win. A `None` override removes the default header entirely,
/// matching upstream's `null` semantics. Matching is case-insensitive; the
/// caller's spelling is preserved.
pub fn merge_headers(defaults: HeaderMap, overrides: &ProviderHeaders) -> HeaderMap {
    let mut out: HeaderMap = defaults;
    for (name, value) in overrides {
        let lower = name.to_lowercase();
        let existing: Vec<String> = out
            .keys()
            .filter(|k| k.to_lowercase() == lower)
            .cloned()
            .collect();
        for key in existing {
            out.remove(&key);
        }
        if let Some(value) = value {
            out.insert(name.clone(), value.clone());
        }
    }
    out
}

/// The Pi user-agent string. Port of `packages/ai/src/utils/pi-user-agent.ts`.
///
/// Upstream reports `pi (<platform> <release>; <arch>)`. The OS release needs a
/// `uname` binding that would be the crate's only libc dependency, so it is
/// omitted here rather than faked; the platform and architecture still let a
/// provider distinguish clients.
pub fn pi_user_agent() -> String {
    format!("pi ({} {})", std::env::consts::OS, std::env::consts::ARCH)
}

/// Overwrite any caller-supplied `User-Agent` with [`pi_user_agent`].
///
/// Some providers gate features or quotas on the user agent, so adapters that
/// must be identifiable call this after merging caller overrides.
pub fn force_pi_user_agent(headers: &mut HeaderMap) {
    headers.retain(|name, _| !name.eq_ignore_ascii_case("user-agent"));
    headers.insert("User-Agent".to_string(), pi_user_agent());
}

/// Redact sensitive header values for logs and telemetry.
pub fn redact_headers(headers: &HeaderMap) -> HeaderMap {
    const SENSITIVE: [&str; 6] = [
        "authorization",
        "x-api-key",
        "api-key",
        "cookie",
        "proxy-authorization",
        "x-goog-api-key",
    ];
    headers
        .iter()
        .map(|(k, v)| {
            if SENSITIVE.contains(&k.to_lowercase().as_str()) {
                (k.clone(), "<redacted>".to_string())
            } else {
                (k.clone(), v.clone())
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn override_replaces_case_insensitively() {
        let defaults: HeaderMap = [("Content-Type".to_string(), "application/json".to_string())]
            .into_iter()
            .collect();
        let overrides: ProviderHeaders =
            [("content-type".to_string(), Some("text/plain".to_string()))]
                .into_iter()
                .collect();
        let merged = merge_headers(defaults, &overrides);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged["content-type"], "text/plain");
    }

    #[test]
    fn forcing_the_user_agent_replaces_any_spelling() {
        let mut headers: HeaderMap = [
            ("user-agent".to_string(), "curl/8".to_string()),
            ("USER-AGENT".to_string(), "other".to_string()),
            ("Accept".to_string(), "application/json".to_string()),
        ]
        .into_iter()
        .collect();
        force_pi_user_agent(&mut headers);
        assert_eq!(headers["User-Agent"], pi_user_agent());
        assert_eq!(headers["Accept"], "application/json");
        assert_eq!(
            headers
                .keys()
                .filter(|k| k.eq_ignore_ascii_case("user-agent"))
                .count(),
            1
        );
    }

    #[test]
    fn the_user_agent_identifies_the_platform() {
        let agent = pi_user_agent();
        assert!(agent.starts_with("pi ("), "{agent}");
        assert!(agent.contains(std::env::consts::ARCH), "{agent}");
    }

    #[test]
    fn redaction_hides_credentials_but_keeps_other_headers() {
        let headers: HeaderMap = [
            ("Authorization".to_string(), "Bearer secret".to_string()),
            ("X-Api-Key".to_string(), "sk-123".to_string()),
            ("Content-Type".to_string(), "application/json".to_string()),
        ]
        .into_iter()
        .collect();
        let redacted = redact_headers(&headers);
        assert_eq!(redacted["Authorization"], "<redacted>");
        assert_eq!(redacted["X-Api-Key"], "<redacted>");
        assert_eq!(redacted["Content-Type"], "application/json");
    }

    #[test]
    fn null_override_removes_default() {
        let defaults: HeaderMap = [("X-Api-Key".to_string(), "secret".to_string())]
            .into_iter()
            .collect();
        let overrides: ProviderHeaders = [("x-api-key".to_string(), None)].into_iter().collect();
        assert!(merge_headers(defaults, &overrides).is_empty());
    }
}
