//! Provider-scoped proxy resolution.
//! Port of `packages/ai/src/utils/node-http-proxy.ts`.
//!
//! Reads `HTTPS_PROXY` / `HTTP_PROXY` / `ALL_PROXY` / `NO_PROXY` (either case)
//! from [`RequestOptions::env`](pi_core::options::RequestOptions) first and the
//! process environment second, so a caller can point one provider at a proxy
//! without touching the whole process.
//!
//! Only `http:` and `https:` proxies are supported; SOCKS and PAC URLs are
//! rejected loudly rather than silently ignored, because silently bypassing a
//! configured proxy is an egress-policy failure.

use pi_core::options::ProviderEnv;
use url::Url;

use crate::provider_env::get_provider_env_value;

pub const UNSUPPORTED_PROXY_PROTOCOL_MESSAGE: &str =
    "Unsupported proxy protocol. SOCKS and PAC proxy URLs are not supported; use an HTTP or HTTPS proxy URL.";

/// Proxy configuration that cannot be used as given.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProxyError {
    #[error("Invalid proxy URL {proxy:?}: {message}")]
    InvalidProxyUrl { proxy: String, message: String },
    #[error("{UNSUPPORTED_PROXY_PROTOCOL_MESSAGE} Got {protocol}:")]
    UnsupportedProtocol { protocol: String },
}

impl ProxyError {
    pub fn code(&self) -> &'static str {
        match self {
            ProxyError::InvalidProxyUrl { .. } => "invalid_proxy_url",
            ProxyError::UnsupportedProtocol { .. } => "unsupported_proxy_protocol",
        }
    }
}

fn default_proxy_port(scheme: &str) -> u16 {
    match scheme {
        "ftp" => 21,
        "gopher" => 70,
        "http" | "ws" => 80,
        "https" | "wss" => 443,
        _ => 0,
    }
}

/// `key` looked up lower-case then upper-case, scoped env before process env.
fn get_proxy_env(key: &str, env: &ProviderEnv) -> String {
    let lower = key.to_lowercase();
    let upper = key.to_uppercase();
    env.get(&lower)
        .filter(|v| !v.is_empty())
        .cloned()
        .or_else(|| env.get(&upper).filter(|v| !v.is_empty()).cloned())
        .or_else(|| get_provider_env_value(&lower, &ProviderEnv::new()))
        .or_else(|| get_provider_env_value(&upper, &ProviderEnv::new()))
        .unwrap_or_default()
}

/// Whether `hostname:port` should go through a proxy given `NO_PROXY`.
///
/// `NO_PROXY` is a comma/whitespace separated list. `*` disables proxying
/// entirely; an entry may carry a port (`host:8080`) and may be a suffix match
/// when it starts with `.` or `*`.
pub fn should_proxy_hostname(hostname: &str, port: u16, env: &ProviderEnv) -> bool {
    let no_proxy = get_proxy_env("no_proxy", env).to_lowercase();
    if no_proxy.is_empty() {
        return true;
    }
    if no_proxy == "*" {
        return false;
    }

    no_proxy.split([',', ' ', '\t', '\n', '\r']).all(|entry| {
        if entry.is_empty() {
            return true;
        }
        let (mut entry_host, entry_port) = match entry.rsplit_once(':') {
            Some((host, port_text))
                if !port_text.is_empty() && port_text.bytes().all(|b| b.is_ascii_digit()) =>
            {
                (host, port_text.parse::<u16>().unwrap_or(0))
            }
            _ => (entry, 0),
        };
        // A port-qualified entry only excludes that port.
        if entry_port != 0 && entry_port != port {
            return true;
        }
        if !entry_host.starts_with('.') && !entry_host.starts_with('*') {
            return hostname != entry_host;
        }
        if let Some(stripped) = entry_host.strip_prefix('*') {
            entry_host = stripped;
        }
        !hostname.ends_with(entry_host)
    })
}

/// The raw proxy string configured for `target`, or empty when none applies.
fn get_proxy_for_url(target: &Url, env: &ProviderEnv) -> String {
    let scheme = target.scheme();
    let Some(hostname) = target.host_str() else {
        return String::new();
    };
    let port = target.port().unwrap_or_else(|| default_proxy_port(scheme));
    if !should_proxy_hostname(hostname, port, env) {
        return String::new();
    }

    let mut proxy = get_proxy_env(&format!("{scheme}_proxy"), env);
    if proxy.is_empty() {
        proxy = get_proxy_env("all_proxy", env);
    }
    if !proxy.is_empty() && !proxy.contains("://") {
        proxy = format!("{scheme}://{proxy}");
    }
    proxy
}

/// Resolve the HTTP(S) proxy to use for `target_url`.
///
/// `Ok(None)` means "connect directly" — either nothing is configured or
/// `NO_PROXY` excludes the target. An unparseable target is also `Ok(None)`,
/// matching upstream, because the request itself will fail with a better error.
pub fn resolve_http_proxy_url_for_target(
    target_url: &str,
    env: &ProviderEnv,
) -> Result<Option<Url>, ProxyError> {
    let Ok(target) = Url::parse(target_url) else {
        return Ok(None);
    };
    let proxy = get_proxy_for_url(&target, env);
    if proxy.is_empty() {
        return Ok(None);
    }

    let proxy_url = Url::parse(&proxy).map_err(|e| ProxyError::InvalidProxyUrl {
        proxy: proxy.clone(),
        message: e.to_string(),
    })?;
    if proxy_url.scheme() != "http" && proxy_url.scheme() != "https" {
        return Err(ProxyError::UnsupportedProtocol {
            protocol: proxy_url.scheme().to_string(),
        });
    }
    Ok(Some(proxy_url))
}

/// Environment variable names that affect proxy resolution, in both cases.
/// Exposed so callers can key a client cache on just the relevant entries.
pub const PROXY_ENV_KEYS: [&str; 8] = [
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "no_proxy",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
];

/// Whether `env` carries any proxy-relevant override at all.
pub fn has_proxy_overrides(env: &ProviderEnv) -> bool {
    PROXY_ENV_KEYS
        .iter()
        .any(|key| env.get(*key).is_some_and(|v| !v.is_empty()))
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

    const TARGET: &str = "https://bedrock-runtime.us-east-1.amazonaws.com";

    #[test]
    fn resolves_http_and_https_proxy_urls() {
        let e = env(&[("HTTPS_PROXY", "http://proxy.example:8080")]);
        assert_eq!(
            resolve_http_proxy_url_for_target(TARGET, &e)
                .unwrap()
                .map(|u| u.to_string()),
            Some("http://proxy.example:8080/".to_string())
        );
    }

    #[test]
    fn respects_no_proxy_exclusions() {
        let e = env(&[
            ("HTTPS_PROXY", "http://proxy.example:8080"),
            ("NO_PROXY", "bedrock-runtime.us-east-1.amazonaws.com"),
        ]);
        assert_eq!(resolve_http_proxy_url_for_target(TARGET, &e).unwrap(), None);
    }

    #[test]
    fn a_star_no_proxy_disables_proxying_entirely() {
        let e = env(&[
            ("HTTPS_PROXY", "http://proxy.example:8080"),
            ("NO_PROXY", "*"),
        ]);
        assert_eq!(resolve_http_proxy_url_for_target(TARGET, &e).unwrap(), None);
    }

    #[test]
    fn no_proxy_suffix_and_port_entries() {
        let base = "http://proxy.example:8080";
        // Suffix match via leading dot.
        let e = env(&[("HTTPS_PROXY", base), ("NO_PROXY", ".amazonaws.com")]);
        assert_eq!(resolve_http_proxy_url_for_target(TARGET, &e).unwrap(), None);
        // Suffix match via leading star.
        let e = env(&[("HTTPS_PROXY", base), ("NO_PROXY", "*.amazonaws.com")]);
        assert_eq!(resolve_http_proxy_url_for_target(TARGET, &e).unwrap(), None);
        // Non-matching entry leaves proxying on.
        let e = env(&[
            ("HTTPS_PROXY", base),
            ("NO_PROXY", "other.example,localhost"),
        ]);
        assert!(resolve_http_proxy_url_for_target(TARGET, &e)
            .unwrap()
            .is_some());
        // A port-qualified entry only applies to that port; 443 is implied here.
        let e = env(&[
            ("HTTPS_PROXY", base),
            ("NO_PROXY", "bedrock-runtime.us-east-1.amazonaws.com:8443"),
        ]);
        assert!(resolve_http_proxy_url_for_target(TARGET, &e)
            .unwrap()
            .is_some());
        let e = env(&[
            ("HTTPS_PROXY", base),
            ("NO_PROXY", "bedrock-runtime.us-east-1.amazonaws.com:443"),
        ]);
        assert_eq!(resolve_http_proxy_url_for_target(TARGET, &e).unwrap(), None);
    }

    #[test]
    fn scoped_env_aliases_beat_process_env_aliases() {
        std::env::set_var("https_proxy", "http://process-proxy.example:8080");
        let e = env(&[("HTTPS_PROXY", "http://scoped-proxy.example:8080")]);
        let resolved = resolve_http_proxy_url_for_target(TARGET, &e).unwrap();
        std::env::remove_var("https_proxy");
        assert_eq!(
            resolved.map(|u| u.to_string()),
            Some("http://scoped-proxy.example:8080/".to_string())
        );
    }

    #[test]
    fn rejects_socks_and_pac_proxy_urls_explicitly() {
        for proxy in [
            "socks5://proxy.example:1080",
            "pac+https://proxy.example/pac",
        ] {
            let e = env(&[("HTTPS_PROXY", proxy)]);
            let err = resolve_http_proxy_url_for_target(TARGET, &e).unwrap_err();
            assert!(
                err.to_string().contains(UNSUPPORTED_PROXY_PROTOCOL_MESSAGE),
                "{proxy}: {err}"
            );
            assert_eq!(err.code(), "unsupported_proxy_protocol");
        }
    }

    #[test]
    fn a_bare_host_port_proxy_gains_the_target_scheme() {
        let e = env(&[("HTTPS_PROXY", "proxy.example:8080")]);
        assert_eq!(
            resolve_http_proxy_url_for_target(TARGET, &e)
                .unwrap()
                .map(|u| u.to_string()),
            Some("https://proxy.example:8080/".to_string())
        );
    }

    #[test]
    fn all_proxy_is_the_fallback_for_any_scheme() {
        let e = env(&[("ALL_PROXY", "http://proxy.example:3128")]);
        assert!(resolve_http_proxy_url_for_target(TARGET, &e)
            .unwrap()
            .is_some());
        assert!(
            resolve_http_proxy_url_for_target("http://api.example.com", &e)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn scheme_specific_proxy_beats_all_proxy() {
        let e = env(&[
            ("ALL_PROXY", "http://all.example:3128"),
            ("HTTPS_PROXY", "http://https.example:8080"),
        ]);
        assert_eq!(
            resolve_http_proxy_url_for_target(TARGET, &e)
                .unwrap()
                .map(|u| u.to_string()),
            Some("http://https.example:8080/".to_string())
        );
    }

    #[test]
    fn an_unparseable_target_resolves_to_no_proxy() {
        let e = env(&[("HTTPS_PROXY", "http://proxy.example:8080")]);
        assert_eq!(
            resolve_http_proxy_url_for_target("not a url", &e).unwrap(),
            None
        );
    }

    #[test]
    fn detects_scoped_proxy_overrides() {
        assert!(!has_proxy_overrides(&ProviderEnv::new()));
        assert!(!has_proxy_overrides(&env(&[("HTTPS_PROXY", "")])));
        assert!(has_proxy_overrides(&env(&[("https_proxy", "http://p:1")])));
    }
}
