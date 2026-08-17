//! Port of `packages/ai/src/auth/oauth/load.ts`.
//!
//! Upstream's loaders exist to keep Node-only flow code out of browser bundles
//! via dynamic imports. Rust has no such problem, so this is just the registry:
//! a name-keyed lookup so a host (or the Swift bridge) can ask for a flow by
//! provider id without linking each one by hand, plus the registration hook
//! upstream needs for standalone binaries.

use std::collections::BTreeMap;
use std::sync::Arc;

use parking_lot::RwLock;

use crate::http::OAuthHttp;
use crate::oauth::{
    AnthropicOAuth, GitHubCopilotOAuth, KimiCodingOAuth, OpenAICodexOAuth, OpenRouterOAuth,
    RadiusOAuth, XaiOAuth,
};
use crate::provider_auth::OAuthFlow;

/// Provider ids with a built-in OAuth flow, in the order upstream lists them.
pub const BUILT_IN_OAUTH_PROVIDERS: &[&str] = &[
    "anthropic",
    "openai-codex",
    "github-copilot",
    "openrouter",
    "kimi-coding",
    "xai",
];

/// Build a built-in flow by provider id.
///
/// `radius` is absent: it is parameterised by gateway URL, so construct it with
/// [`RadiusOAuth::new`] (or [`OAuthFlows::radius`]).
pub fn load_oauth_flow(provider_id: &str) -> Option<Arc<dyn OAuthFlow>> {
    OAuthFlows::default().load(provider_id)
}

/// Flow factory bound to one HTTP client.
#[derive(Clone, Default)]
pub struct OAuthFlows {
    http: OAuthHttp,
}

impl OAuthFlows {
    pub fn new(http: OAuthHttp) -> Self {
        Self { http }
    }

    pub fn anthropic(&self) -> Arc<dyn OAuthFlow> {
        Arc::new(AnthropicOAuth::new(self.http.clone()))
    }

    pub fn openai_codex(&self) -> Arc<dyn OAuthFlow> {
        Arc::new(OpenAICodexOAuth::new(self.http.clone()))
    }

    pub fn github_copilot(&self) -> Arc<dyn OAuthFlow> {
        Arc::new(GitHubCopilotOAuth::new(self.http.clone()))
    }

    pub fn openrouter(&self) -> Arc<dyn OAuthFlow> {
        Arc::new(OpenRouterOAuth::new(self.http.clone()))
    }

    pub fn kimi_coding(&self) -> Arc<dyn OAuthFlow> {
        Arc::new(KimiCodingOAuth::new(self.http.clone()))
    }

    pub fn xai(&self) -> Arc<dyn OAuthFlow> {
        Arc::new(XaiOAuth::new(self.http.clone()))
    }

    pub fn radius(&self, name: impl Into<String>, gateway: impl AsRef<str>) -> Arc<dyn OAuthFlow> {
        Arc::new(RadiusOAuth::new(self.http.clone(), name, gateway))
    }

    /// Registered override first, then the built-ins.
    pub fn load(&self, provider_id: &str) -> Option<Arc<dyn OAuthFlow>> {
        if let Some(flow) = registry().read().get(provider_id) {
            return Some(flow.clone());
        }
        Some(match provider_id {
            "anthropic" => self.anthropic(),
            "openai-codex" => self.openai_codex(),
            "github-copilot" => self.github_copilot(),
            "openrouter" => self.openrouter(),
            "kimi-coding" => self.kimi_coding(),
            "xai" => self.xai(),
            _ => return None,
        })
    }
}

impl std::fmt::Debug for OAuthFlows {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuthFlows").finish_non_exhaustive()
    }
}

type Registry = RwLock<BTreeMap<String, Arc<dyn OAuthFlow>>>;

fn registry() -> &'static Registry {
    static REGISTRY: std::sync::OnceLock<Registry> = std::sync::OnceLock::new();
    REGISTRY.get_or_init(|| RwLock::new(BTreeMap::new()))
}

/// Register a flow for a provider id, replacing any built-in of the same name.
/// The port of `registerBundledOAuthFlowLoaders`, generalised so a host can
/// also add flows this crate does not ship.
pub fn register_oauth_flow(provider_id: impl Into<String>, flow: Arc<dyn OAuthFlow>) {
    registry().write().insert(provider_id.into(), flow);
}

/// Drop a registered override.
pub fn unregister_oauth_flow(provider_id: &str) {
    registry().write().remove(provider_id);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_built_in_provider_id_loads() {
        for provider_id in BUILT_IN_OAUTH_PROVIDERS {
            assert!(
                load_oauth_flow(provider_id).is_some(),
                "{provider_id} should load"
            );
        }
        assert!(load_oauth_flow("not-a-provider").is_none());
        // Radius needs a gateway, so it is deliberately not in the by-id table.
        assert!(load_oauth_flow("radius").is_none());
    }

    #[test]
    fn only_subscription_backed_flows_report_as_subscriptions() {
        let flows = OAuthFlows::default();
        for flow in [
            flows.anthropic(),
            flows.openai_codex(),
            flows.github_copilot(),
            flows.kimi_coding(),
            flows.xai(),
        ] {
            assert!(flow.is_subscription(), "{} is a subscription", flow.name());
        }
        assert!(!flows.openrouter().is_subscription());
        assert!(!flows
            .radius("Radius", "gateway.example.com")
            .is_subscription());
    }
}
