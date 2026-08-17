//! `mistral-conversations`, `pi-messages` and the `faux` test provider.
//!
//! Port of `packages/ai/src/api/{mistral-conversations,pi-messages}.ts`,
//! `packages/ai/src/providers/{mistral,faux}.ts` and their `.lazy.ts` wrappers
//! (which have no Rust equivalent: there is no module loading to defer, so the
//! adapters are constructed directly).
//!
//! Every adapter is a type implementing [`pi_core::ApiClient`] whose
//! [`api()`](pi_core::ApiClient::api) matches the `api` field of the models it
//! serves. None of them return `Err` for a request failure: errors and aborts
//! arrive in the stream as an `Error` event carrying an
//! [`AssistantMessage`](pi_core::AssistantMessage) with `stop_reason`
//! `Error`/`Aborted`.
//!
//! ## Which one to use
//!
//! - [`faux::FauxProvider`] — the scriptable in-process test double. No
//!   network, no fixtures; other crates depend on this crate purely for it, and
//!   it lives in the normal (non-`cfg(test)`) API surface for that reason.
//! - [`mistral_conversations::MistralConversationsApi`] — Mistral's native chat
//!   completions endpoint.
//! - [`pi_messages::PiMessagesApi`] — pi's own message protocol, spoken by the
//!   Radius gateway and by custom providers declaring `"api": "pi-messages"`.
//!
//! ## Registration
//!
//! Adapters do not know about `pi-catalog`. Each exposes a plain
//! [`ProviderDescriptor`] plus its client as a [`ProviderRegistration`], which
//! a catalog registers at runtime.

pub mod faux;
pub mod mistral_conversations;
pub mod pi_messages;
pub mod provider;
pub mod support;

pub use faux::{
    faux_aborted_message, faux_assistant_message, faux_error_message, faux_text, faux_thinking,
    faux_tool_call, faux_tool_call_with_id, faux_tool_use_message, FauxDeferredOptions,
    FauxModelDefinition, FauxOptions, FauxProvider, FauxProviderState, FauxRequest, FauxResponse,
    FauxTokenSize, IntoFauxContent,
};
pub use mistral_conversations::{
    mistral_provider, MistralConversationsApi, MISTRAL_CONVERSATIONS_API,
};
pub use pi_messages::{
    pi_messages_provider, PiMessagesApi, PiMessagesEvent, PiMessagesRewriteImpact, PI_MESSAGES_API,
};
pub use provider::{ProviderDescriptor, ProviderRegistration};

/// Every provider this crate can register: Mistral plus a Radius-style
/// `pi-messages` gateway. The faux provider is intentionally not included —
/// tests construct it explicitly.
pub fn providers() -> Vec<ProviderRegistration> {
    vec![
        mistral_provider(),
        pi_messages_provider("radius", "Radius", "https://radius.earendil.works/v1"),
    ]
}
