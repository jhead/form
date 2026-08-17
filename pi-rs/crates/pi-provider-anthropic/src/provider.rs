//! Provider descriptor for the catalog.
//!
//! Port of the static half of `packages/ai/src/providers/anthropic.ts`. It is a
//! plain data struct on purpose: `pi-catalog` registers adapters at runtime and
//! must not depend on this crate, so it can copy these fields without linking
//! the adapter in.

/// Static description of a provider that speaks one API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderDescriptor {
    pub id: &'static str,
    pub name: &'static str,
    pub base_url: &'static str,
    /// The API id its models use; matches `ApiClient::api`.
    pub api: &'static str,
    /// Env vars carrying a bearer token, in resolution order. These are sent as
    /// `Authorization: Bearer <value>` rather than `x-api-key`.
    pub auth_token_env: &'static [&'static str],
    /// Env vars carrying an API key, in resolution order.
    pub api_key_env: &'static [&'static str],
    /// Whether the provider offers an OAuth (subscription) login.
    pub supports_oauth: bool,
    /// Headers every request to this provider carries.
    pub headers: &'static [(&'static str, &'static str)],
}

/// Header sent on every Anthropic Messages request.
pub const ANTHROPIC_VERSION_HEADER: (&str, &str) = ("anthropic-version", "2023-06-01");

/// The `anthropic` provider.
pub const ANTHROPIC_PROVIDER: ProviderDescriptor = ProviderDescriptor {
    id: "anthropic",
    name: "Anthropic",
    base_url: "https://api.anthropic.com",
    api: crate::ANTHROPIC_MESSAGES_API,
    auth_token_env: &["ANTHROPIC_AUTH_TOKEN"],
    api_key_env: &["ANTHROPIC_OAUTH_TOKEN", "ANTHROPIC_API_KEY"],
    supports_oauth: true,
    headers: &[
        ("anthropic-version", "2023-06-01"),
        ("anthropic-dangerous-direct-browser-access", "true"),
    ],
};
