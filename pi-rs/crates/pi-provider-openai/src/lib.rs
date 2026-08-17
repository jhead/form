//! `openai-completions`, `openai-responses`, `azure-openai-responses` and
//! `openai-codex-responses` API adapters.
//!
//! Port of `packages/ai/src/api/{openai-completions,openai-responses,
//! openai-responses-shared,azure-openai-responses,openai-codex-responses,
//! openai-prompt-cache,github-copilot-headers}.ts`. The shared
//! `{transform-messages,simple-options,constrained-sampling}.ts` and
//! `models.ts#calculateCost` live in [`pi_provider_common`], which every
//! adapter crate uses.
//!
//! ## Shape
//!
//! Each wire format is a unit-ish struct implementing [`pi_core::ApiClient`]:
//!
//! | type | `api()` |
//! |---|---|
//! | [`OpenAiCompletionsClient`] | `openai-completions` |
//! | [`OpenAiResponsesClient`] | `openai-responses` |
//! | [`AzureOpenAiResponsesClient`] | `azure-openai-responses` |
//! | [`OpenAiCodexResponsesClient`] | `openai-codex-responses` |
//!
//! The three Responses dialects share message conversion, tool conversion and
//! the SSE→event mapping through [`openai_responses_shared`], mirroring
//! upstream's `openai-responses-shared.ts`.
//!
//! ## Adapter-specific options
//!
//! `ApiClient::stream` takes the shared [`pi_core::options::StreamOptions`].
//! Upstream's per-adapter option interfaces (`toolChoice`, `reasoningEffort`,
//! `serviceTier`, the Azure endpoint overrides, …) travel in
//! `StreamOptions::provider_options` under the TypeScript field names; see
//! [`options::ProviderOptionKey`].
//!
//! ## Error contract
//!
//! Request, model and runtime failures are encoded **in the stream** as an
//! `Error` event whose `AssistantMessage` carries `stop_reason` `Error` or
//! `Aborted`. `Err` is reserved for a programmer error detected before any
//! request work starts.
//!
//! That includes a missing credential, from `stream_simple` as well as from
//! `stream`. Upstream's `streamSimple` throws synchronously in that one spot
//! and this port used to mirror it; it no longer does, because the `ApiClient`
//! contract in `pi-core` reserves `Err` for programmer errors and an FFI caller
//! should not have to handle one condition on two paths. All four adapters in
//! this crate — and the Anthropic, Google and Mistral adapters — behave the
//! same way.

pub mod compat;
pub mod github_copilot_headers;
pub mod openai_prompt_cache;
pub mod options;
pub mod providers;
pub mod transport;
pub mod util;

pub mod azure_openai_responses;
pub mod openai_codex_responses;
pub mod openai_completions;
pub mod openai_responses;
pub mod openai_responses_shared;

pub use azure_openai_responses::AzureOpenAiResponsesClient;
pub use compat::{
    completions_compat, detect_completions_compat, responses_compat, CompletionsCompat,
    DeferredToolsMode, ResponsesCompat,
};
pub use openai_codex_responses::OpenAiCodexResponsesClient;
pub use openai_completions::OpenAiCompletionsClient;
pub use openai_responses::OpenAiResponsesClient;
pub use options::{with_provider_option, ProviderOptionKey};
pub use providers::{openai_provider_descriptors, ProviderAuthKind, ProviderDescriptor};

use std::sync::Arc;

use pi_core::ApiClientRef;

/// Every adapter in this crate, ready to register with a provider registry.
///
/// Returned as plain `Arc<dyn ApiClient>` so `pi-catalog` can register them
/// without this crate depending on the registry.
pub fn all_api_clients() -> Vec<ApiClientRef> {
    vec![
        Arc::new(OpenAiCompletionsClient::new()),
        Arc::new(OpenAiResponsesClient::new()),
        Arc::new(AzureOpenAiResponsesClient::new()),
        Arc::new(OpenAiCodexResponsesClient::new()),
    ]
}
