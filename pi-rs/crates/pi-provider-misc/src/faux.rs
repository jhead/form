//! The `faux` test provider. Port of `packages/ai/src/providers/faux.ts`.
//!
//! A fully in-process [`ApiClient`] that replays scripted assistant messages as
//! a real event stream: text/thinking/tool-call deltas with configurable
//! chunking and pacing, injected failures, aborts, simulated prompt caching and
//! usage, and deferred responses. It never touches the network, so it is the
//! default double for the agent loop, session and harness test suites.
//!
//! ```no_run
//! # use pi_provider_misc::faux::{FauxProvider, faux_assistant_message};
//! # async fn demo() {
//! let faux = FauxProvider::new();
//! faux.set_responses(vec![faux_assistant_message("hello").into()]);
//!
//! let model = faux.model();
//! let context = pi_core::Context::new(vec![pi_core::UserMessage::text("hi").into()]);
//! let stream = pi_core::ApiClient::stream_simple(&faux, &model, &context, &Default::default())
//!     .await
//!     .unwrap();
//! let message = stream.into_final_message().await.unwrap();
//! assert_eq!(message.text(), "hello");
//! # }
//! ```
//!
//! Streaming runs on a spawned task, so calls must happen inside a tokio
//! runtime (`#[tokio::test]` is enough).
//!
//! ## Deliberate differences from upstream
//!
//! - Upstream defaults `api` to a random string so each registration is
//!   distinct in its global registry. There is no global registry here, so the
//!   default is the stable `"faux"`; pass [`FauxOptions::api`] when a test needs
//!   two providers side by side.
//! - Event `partial` snapshots are deep clones. Upstream shares one mutable
//!   content array across every emitted snapshot, so its earlier events
//!   retroactively show later text. Here each event carries the state as of the
//!   moment it was emitted, which is what the protocol says it means.
//! - Chunk sizes come from a seedable PRNG ([`FauxOptions::seed`]) instead of
//!   `Math.random`, so an exact event sequence can be asserted without pinning
//!   `token_size` to a single value.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use pi_core::api::{ApiClient, ApiClientRef, StreamFn};
use pi_core::content::{AssistantContent, TextContent, ThinkingContent, ToolCall};
use pi_core::error::AiError;
use pi_core::event::{
    AssistantMessageEvent, AssistantMessageEventSink, AssistantMessageEventStream, DoneReason,
    ErrorReason,
};
use pi_core::message::{now_ms, AssistantMessage, DeferredHandle, Message, StopReason, Usage};
use pi_core::model::{CacheRetention, Modality, Model, ModelCost, ModelCostRates};
use pi_core::options::{
    DeferredFetchOptions, ProviderResponse, RequestOptions, SimpleStreamOptions, StreamOptions,
};
use pi_core::tool::Context;
use pi_core::{Api, InputContent, UserContent};
use serde_json::{Map, Value};

use crate::provider::{ProviderDescriptor, ProviderRegistration};
use pi_http::estimate::estimate_text_tokens;

pub const FAUX_API: &str = "faux";
pub const FAUX_PROVIDER: &str = "faux";
pub const FAUX_MODEL_ID: &str = "faux-1";
pub const FAUX_MODEL_NAME: &str = "Faux Model";
pub const FAUX_BASE_URL: &str = "http://localhost:0";

const DEFAULT_MIN_TOKEN_SIZE: u32 = 3;
const DEFAULT_MAX_TOKEN_SIZE: u32 = 5;
const DEFAULT_CONTEXT_WINDOW: u64 = 128_000;
const DEFAULT_MAX_TOKENS: u64 = 16_384;

// ---------------------------------------------------------------------------
// Message construction helpers
// ---------------------------------------------------------------------------

/// A text block for a scripted response.
pub fn faux_text(text: impl Into<String>) -> AssistantContent {
    AssistantContent::Text(TextContent {
        text: text.into(),
        text_signature: None,
    })
}

/// A thinking block for a scripted response.
pub fn faux_thinking(thinking: impl Into<String>) -> AssistantContent {
    AssistantContent::Thinking(ThinkingContent {
        thinking: thinking.into(),
        ..Default::default()
    })
}

/// A tool call with a generated id.
pub fn faux_tool_call(name: impl Into<String>, arguments: Value) -> AssistantContent {
    faux_tool_call_with_id(random_id("tool"), name, arguments)
}

/// A tool call with a caller-chosen id, so assertions can name it.
pub fn faux_tool_call_with_id(
    id: impl Into<String>,
    name: impl Into<String>,
    arguments: Value,
) -> AssistantContent {
    AssistantContent::ToolCall(ToolCall {
        id: id.into(),
        name: name.into(),
        arguments: match arguments {
            Value::Object(map) => map,
            Value::Null => Map::new(),
            other => {
                let mut map = Map::new();
                map.insert("value".to_string(), other);
                map
            }
        },
        thought_signature: None,
        namespace: None,
    })
}

/// Anything that can stand in for the content of a scripted assistant message.
pub trait IntoFauxContent {
    fn into_faux_content(self) -> Vec<AssistantContent>;
}

impl IntoFauxContent for &str {
    fn into_faux_content(self) -> Vec<AssistantContent> {
        vec![faux_text(self)]
    }
}

impl IntoFauxContent for String {
    fn into_faux_content(self) -> Vec<AssistantContent> {
        vec![faux_text(self)]
    }
}

impl IntoFauxContent for AssistantContent {
    fn into_faux_content(self) -> Vec<AssistantContent> {
        vec![self]
    }
}

impl IntoFauxContent for Vec<AssistantContent> {
    fn into_faux_content(self) -> Vec<AssistantContent> {
        self
    }
}

/// A finished assistant message with `stop_reason` `Stop`.
///
/// `api`, `provider` and `model` are rewritten by the provider when the message
/// is streamed, so the placeholders here rarely matter.
pub fn faux_assistant_message(content: impl IntoFauxContent) -> AssistantMessage {
    let mut message = AssistantMessage::pending(FAUX_API, FAUX_PROVIDER, FAUX_MODEL_ID);
    message.content = content.into_faux_content();
    message.stop_reason = StopReason::Stop;
    message
}

/// A scripted message that ends the turn with `stop_reason` `ToolUse`.
pub fn faux_tool_use_message(content: impl IntoFauxContent) -> AssistantMessage {
    let mut message = faux_assistant_message(content);
    message.stop_reason = StopReason::ToolUse;
    message
}

/// A scripted message that terminates the stream with an `Error` event.
pub fn faux_error_message(error_message: impl Into<String>) -> AssistantMessage {
    let mut message = faux_assistant_message(Vec::new());
    message.stop_reason = StopReason::Error;
    message.error_message = Some(error_message.into());
    message
}

/// A scripted message that terminates the stream with an `Aborted` event.
pub fn faux_aborted_message() -> AssistantMessage {
    let mut message = faux_assistant_message(Vec::new());
    message.stop_reason = StopReason::Aborted;
    message.error_message = Some("Request was aborted".to_string());
    message
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// One model the faux provider should expose.
#[derive(Debug, Clone)]
pub struct FauxModelDefinition {
    pub id: String,
    pub name: Option<String>,
    pub reasoning: bool,
    pub input: Option<Vec<Modality>>,
    pub cost: Option<ModelCostRates>,
    pub context_window: Option<u64>,
    pub max_tokens: Option<u64>,
}

impl FauxModelDefinition {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: None,
            reasoning: false,
            input: None,
            cost: None,
            context_window: None,
            max_tokens: None,
        }
    }

    pub fn named(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    pub fn reasoning(mut self, reasoning: bool) -> Self {
        self.reasoning = reasoning;
        self
    }

    pub fn input(mut self, input: Vec<Modality>) -> Self {
        self.input = Some(input);
        self
    }

    pub fn cost(mut self, cost: ModelCostRates) -> Self {
        self.cost = Some(cost);
        self
    }

    pub fn context_window(mut self, tokens: u64) -> Self {
        self.context_window = Some(tokens);
        self
    }

    pub fn max_tokens(mut self, tokens: u64) -> Self {
        self.max_tokens = Some(tokens);
        self
    }
}

/// Deferred-response simulation.
#[derive(Debug, Clone, Default)]
pub struct FauxDeferredOptions {
    /// Fetches that report "still pending" before the scripted response is ready.
    pub pending_fetches: u32,
    pub poll_after_ms: Option<u64>,
}

/// Chunk size range, in estimated tokens (one token is four characters).
#[derive(Debug, Clone, Copy)]
pub struct FauxTokenSize {
    pub min: u32,
    pub max: u32,
}

impl Default for FauxTokenSize {
    fn default() -> Self {
        Self {
            min: DEFAULT_MIN_TOKEN_SIZE,
            max: DEFAULT_MAX_TOKEN_SIZE,
        }
    }
}

impl FauxTokenSize {
    /// Fixed-size chunks, which makes the event sequence fully deterministic.
    pub fn fixed(tokens: u32) -> Self {
        Self {
            min: tokens,
            max: tokens,
        }
    }
}

/// Everything configurable about a [`FauxProvider`].
#[derive(Debug, Clone, Default)]
pub struct FauxOptions {
    /// Defaults to `"faux"`. Must equal `Model::api` for the models it serves.
    pub api: Option<String>,
    /// Defaults to `"faux"`.
    pub provider: Option<String>,
    /// Defaults to a single `faux-1` text+image model.
    pub models: Vec<FauxModelDefinition>,
    pub deferred: Option<FauxDeferredOptions>,
    /// Pace deltas at roughly this many tokens per second. `None` streams as
    /// fast as the consumer accepts, yielding between chunks.
    pub tokens_per_second: Option<f64>,
    pub token_size: Option<FauxTokenSize>,
    /// Seed for chunk sizing. Fixed by default so runs are reproducible.
    pub seed: Option<u64>,
}

impl FauxOptions {
    pub fn api(mut self, api: impl Into<String>) -> Self {
        self.api = Some(api.into());
        self
    }

    pub fn provider(mut self, provider: impl Into<String>) -> Self {
        self.provider = Some(provider.into());
        self
    }

    pub fn models(mut self, models: Vec<FauxModelDefinition>) -> Self {
        self.models = models;
        self
    }

    pub fn tokens_per_second(mut self, tokens_per_second: f64) -> Self {
        self.tokens_per_second = Some(tokens_per_second);
        self
    }

    pub fn token_size(mut self, token_size: FauxTokenSize) -> Self {
        self.token_size = Some(token_size);
        self
    }

    pub fn deferred(mut self, deferred: FauxDeferredOptions) -> Self {
        self.deferred = Some(deferred);
        self
    }

    pub fn seed(mut self, seed: u64) -> Self {
        self.seed = Some(seed);
        self
    }
}

// ---------------------------------------------------------------------------
// Scripted responses
// ---------------------------------------------------------------------------

/// What a response factory is handed when it runs.
#[derive(Clone, Debug)]
pub struct FauxRequest {
    pub model: Model,
    pub context: Context,
    pub options: SimpleStreamOptions,
    /// Provider counters as of this call, `call_count` already incremented.
    pub state: FauxProviderState,
}

pub type FauxResponseFuture =
    Pin<Box<dyn Future<Output = Result<AssistantMessage, String>> + Send>>;
pub type FauxResponseFn = Arc<dyn Fn(FauxRequest) -> FauxResponseFuture + Send + Sync>;

/// One queued response: a fixed message, or a factory that computes one.
#[derive(Clone)]
pub enum FauxResponse {
    Message(Box<AssistantMessage>),
    Factory(FauxResponseFn),
}

impl std::fmt::Debug for FauxResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FauxResponse::Message(message) => f.debug_tuple("Message").field(message).finish(),
            FauxResponse::Factory(_) => f.write_str("Factory(..)"),
        }
    }
}

impl From<AssistantMessage> for FauxResponse {
    fn from(message: AssistantMessage) -> Self {
        FauxResponse::Message(Box::new(message))
    }
}

impl FauxResponse {
    /// A plain text answer with `stop_reason` `Stop`.
    pub fn text(text: impl Into<String>) -> Self {
        faux_assistant_message(text.into()).into()
    }

    /// A response whose factory fails, producing a terminal `Error` event whose
    /// `error_message` is `message`.
    pub fn failure(message: impl Into<String>) -> Self {
        let message = message.into();
        Self::from_fn(move |_| Err(message.clone()))
    }

    /// Compute the response synchronously from the request.
    pub fn from_fn<F>(factory: F) -> Self
    where
        F: Fn(FauxRequest) -> Result<AssistantMessage, String> + Send + Sync + 'static,
    {
        FauxResponse::Factory(Arc::new(move |request| {
            let result = factory(request);
            Box::pin(std::future::ready(result))
        }))
    }

    /// Compute the response asynchronously from the request.
    pub fn from_async_fn<F, Fut>(factory: F) -> Self
    where
        F: Fn(FauxRequest) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<AssistantMessage, String>> + Send + 'static,
    {
        FauxResponse::Factory(Arc::new(move |request| Box::pin(factory(request))))
    }

    async fn resolve(&self, request: FauxRequest) -> Result<AssistantMessage, String> {
        match self {
            FauxResponse::Message(message) => Ok((**message).clone()),
            FauxResponse::Factory(factory) => factory(request).await,
        }
    }
}

/// Observable counters, snapshotted by [`FauxProvider::state`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FauxProviderState {
    pub call_count: usize,
    pub deferred_fetch_count: usize,
    pub cancelled_deferred: Vec<DeferredHandle>,
}

// ---------------------------------------------------------------------------
// The provider
// ---------------------------------------------------------------------------

/// Scriptable in-process [`ApiClient`]. Cheap to clone; clones share state.
#[derive(Clone)]
pub struct FauxProvider {
    core: Arc<FauxCore>,
}

impl std::fmt::Debug for FauxProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FauxProvider")
            .field("api", &self.core.api)
            .field("provider", &self.core.provider)
            .field(
                "models",
                &self.core.models.iter().map(|m| &m.id).collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}

impl Default for FauxProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl FauxProvider {
    pub fn new() -> Self {
        Self::with_options(FauxOptions::default())
    }

    pub fn with_options(options: FauxOptions) -> Self {
        Self {
            core: Arc::new(FauxCore::new(options)),
        }
    }

    /// API id this provider answers to; equals every served model's `api`.
    pub fn api_id(&self) -> &str {
        &self.core.api
    }

    /// Provider id; equals every served model's `provider`.
    pub fn provider_id(&self) -> &str {
        &self.core.provider
    }

    pub fn models(&self) -> Vec<Model> {
        self.core.models.clone()
    }

    /// The first configured model.
    pub fn model(&self) -> Model {
        self.core.models[0].clone()
    }

    pub fn model_by_id(&self, model_id: &str) -> Option<Model> {
        self.core.models.iter().find(|m| m.id == model_id).cloned()
    }

    /// Replace the response queue.
    pub fn set_responses(&self, responses: Vec<FauxResponse>) {
        *self.core.responses.lock().expect("faux responses") = responses.into();
    }

    /// Append to the response queue.
    pub fn append_responses(&self, responses: Vec<FauxResponse>) {
        self.core
            .responses
            .lock()
            .expect("faux responses")
            .extend(responses);
    }

    /// Append a single response.
    pub fn push_response(&self, response: impl Into<FauxResponse>) {
        self.core
            .responses
            .lock()
            .expect("faux responses")
            .push_back(response.into());
    }

    /// Replace the queue with scripted messages. Shorthand for the common case.
    pub fn set_messages(&self, messages: Vec<AssistantMessage>) {
        self.set_responses(messages.into_iter().map(FauxResponse::from).collect());
    }

    /// Replace the queue with plain text answers, each ending the turn.
    pub fn set_texts(&self, texts: &[&str]) {
        self.set_responses(texts.iter().map(|text| FauxResponse::text(*text)).collect());
    }

    pub fn pending_response_count(&self) -> usize {
        self.core.responses.lock().expect("faux responses").len()
    }

    pub fn state(&self) -> FauxProviderState {
        self.core.state.lock().expect("faux state").clone()
    }

    pub fn call_count(&self) -> usize {
        self.state().call_count
    }

    /// Every `stream`/`stream_simple` call recorded in order, so tests can
    /// assert on what the agent loop actually sent.
    pub fn requests(&self) -> Vec<FauxRequest> {
        self.core.requests.lock().expect("faux requests").clone()
    }

    /// The most recent recorded request.
    pub fn last_request(&self) -> Option<FauxRequest> {
        self.core
            .requests
            .lock()
            .expect("faux requests")
            .last()
            .cloned()
    }

    /// Clear counters, recorded requests, simulated prompt cache and deferred state.
    pub fn reset(&self) {
        *self.core.state.lock().expect("faux state") = FauxProviderState::default();
        self.core.requests.lock().expect("faux requests").clear();
        self.core.prompt_cache.lock().expect("faux cache").clear();
        self.core.deferred.lock().expect("faux deferred").clear();
    }

    /// This provider as a shared [`ApiClient`], for registries.
    pub fn client(&self) -> ApiClientRef {
        Arc::new(self.clone())
    }

    /// This provider as a [`StreamFn`], the shape the agent loop takes.
    pub fn stream_fn(&self) -> StreamFn {
        pi_core::api::stream_fn_from_client(self.client())
    }

    /// Plain descriptor a catalog can register.
    pub fn descriptor(&self) -> ProviderDescriptor {
        ProviderDescriptor {
            id: self.core.provider.clone(),
            name: "Faux".to_string(),
            base_url: Some(FAUX_BASE_URL.to_string()),
            api: self.core.api.clone(),
            api_key_env: Vec::new(),
            api_key_label: Some("Faux".to_string()),
            models: self.core.models.clone(),
        }
    }

    /// Descriptor plus client, ready to hand to a registry.
    pub fn registration(&self) -> ProviderRegistration {
        ProviderRegistration {
            descriptor: self.descriptor(),
            client: self.client(),
        }
    }

    fn start(
        &self,
        model: &Model,
        context: &Context,
        options: SimpleStreamOptions,
    ) -> AssistantMessageEventStream {
        let (sink, stream) = AssistantMessageEventStream::channel(1);

        // Queue pop and counter bump happen synchronously, exactly like
        // upstream, so `call_count` is accurate the instant `stream` returns.
        let step = self
            .core
            .responses
            .lock()
            .expect("faux responses")
            .pop_front();
        let state = {
            let mut state = self.core.state.lock().expect("faux state");
            state.call_count += 1;
            state.clone()
        };
        let request = FauxRequest {
            model: model.clone(),
            context: context.clone(),
            options: options.clone(),
            state,
        };
        self.core
            .requests
            .lock()
            .expect("faux requests")
            .push(request.clone());

        let core = self.core.clone();
        tokio::spawn(async move { core.run(sink, request, step).await });
        stream
    }
}

#[async_trait]
impl ApiClient for FauxProvider {
    fn api(&self) -> &str {
        &self.core.api
    }

    async fn stream(
        &self,
        model: &Model,
        context: &Context,
        options: &StreamOptions,
    ) -> Result<AssistantMessageEventStream, AiError> {
        Ok(self.start(
            model,
            context,
            SimpleStreamOptions {
                stream: options.clone(),
                ..Default::default()
            },
        ))
    }

    async fn stream_simple(
        &self,
        model: &Model,
        context: &Context,
        options: &SimpleStreamOptions,
    ) -> Result<AssistantMessageEventStream, AiError> {
        Ok(self.start(model, context, options.clone()))
    }

    fn supports_deferred(&self) -> bool {
        true
    }

    async fn fetch_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: &DeferredFetchOptions,
    ) -> Result<AssistantMessageEventStream, AiError> {
        let (sink, stream) = AssistantMessageEventStream::channel(1);
        self.core
            .state
            .lock()
            .expect("faux state")
            .deferred_fetch_count += 1;

        let core = self.core.clone();
        let model = model.clone();
        let handle = handle.clone();
        let request = options.request.clone();
        tokio::spawn(async move { core.run_deferred_fetch(sink, model, handle, request).await });
        Ok(stream)
    }

    async fn cancel_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: &RequestOptions,
    ) -> Result<(), AiError> {
        {
            let mut state = self.core.state.lock().expect("faux state");
            state.cancelled_deferred.push(handle.clone());
        }
        if let Some(entry) = self
            .core
            .deferred
            .lock()
            .expect("faux deferred")
            .get_mut(&handle.id)
        {
            entry.cancelled = true;
        }
        notify_response(options, model);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Core
// ---------------------------------------------------------------------------

struct DeferredEntry {
    handle: DeferredHandle,
    step: FauxResponse,
    request: FauxRequest,
    pending_fetches: u32,
    cancelled: bool,
    final_message: Option<AssistantMessage>,
}

struct FauxCore {
    api: String,
    provider: String,
    models: Vec<Model>,
    min_token_size: u32,
    max_token_size: u32,
    tokens_per_second: Option<f64>,
    deferred_options: FauxDeferredOptions,
    responses: Mutex<std::collections::VecDeque<FauxResponse>>,
    state: Mutex<FauxProviderState>,
    requests: Mutex<Vec<FauxRequest>>,
    prompt_cache: Mutex<HashMap<String, String>>,
    deferred: Mutex<HashMap<String, DeferredEntry>>,
    rng: Mutex<u64>,
}

impl FauxCore {
    fn new(options: FauxOptions) -> Self {
        let api = options.api.unwrap_or_else(|| FAUX_API.to_string());
        let provider = options
            .provider
            .unwrap_or_else(|| FAUX_PROVIDER.to_string());
        let token_size = options.token_size.unwrap_or_default();
        let min_token_size = token_size.min.min(token_size.max).max(1);
        let max_token_size = token_size.max.max(min_token_size);

        let definitions = if options.models.is_empty() {
            vec![FauxModelDefinition::new(FAUX_MODEL_ID)
                .named(FAUX_MODEL_NAME)
                .input(vec![Modality::Text, Modality::Image])]
        } else {
            options.models
        };

        let models = definitions
            .into_iter()
            .map(|definition| Model {
                name: definition
                    .name
                    .clone()
                    .unwrap_or_else(|| definition.id.clone()),
                id: definition.id,
                api: Api::from(api.clone()),
                provider: provider.clone(),
                base_url: FAUX_BASE_URL.to_string(),
                reasoning: definition.reasoning,
                thinking_level_map: None,
                input: definition
                    .input
                    .unwrap_or_else(|| vec![Modality::Text, Modality::Image]),
                cost: ModelCost {
                    rates: definition.cost.unwrap_or_default(),
                    tiers: None,
                },
                context_window: definition.context_window.unwrap_or(DEFAULT_CONTEXT_WINDOW),
                max_tokens: definition.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
                sampling_params: None,
                headers: None,
                compat: None,
            })
            .collect();

        Self {
            api,
            provider,
            models,
            min_token_size,
            max_token_size,
            tokens_per_second: options.tokens_per_second,
            deferred_options: options.deferred.unwrap_or_default(),
            responses: Mutex::new(Default::default()),
            state: Mutex::new(FauxProviderState::default()),
            requests: Mutex::new(Vec::new()),
            prompt_cache: Mutex::new(HashMap::new()),
            deferred: Mutex::new(HashMap::new()),
            rng: Mutex::new(options.seed.unwrap_or(0x5EED_5EED_5EED_5EED)),
        }
    }

    async fn run(
        &self,
        sink: AssistantMessageEventSink,
        request: FauxRequest,
        step: Option<FauxResponse>,
    ) {
        notify_response(&request.options.stream.request, &request.model);

        let Some(step) = step else {
            let mut message =
                self.error_message("No more faux responses queued", &request.model.id);
            self.apply_usage_estimate(&mut message, &request.context, &request.options.stream);
            emit_error(&sink, ErrorReason::Error, message).await;
            return;
        };

        if request.options.deferred.is_some() {
            let handle = DeferredHandle {
                provider: request.model.provider.clone(),
                model_id: request.model.id.clone(),
                api: request.model.api.as_str().to_string(),
                id: random_id("deferred"),
                expires_at: None,
                poll_after_ms: self.deferred_options.poll_after_ms,
                data: None,
            };
            self.deferred.lock().expect("faux deferred").insert(
                handle.id.clone(),
                DeferredEntry {
                    handle: handle.clone(),
                    step,
                    request: request.clone(),
                    pending_fetches: self.deferred_options.pending_fetches,
                    cancelled: false,
                    final_message: None,
                },
            );
            let message = self.deferred_message(&request.model, handle);
            self.stream_with_deltas(
                &sink,
                message,
                request.options.stream.request.signal.clone(),
            )
            .await;
            return;
        }

        let signal = request.options.stream.request.signal.clone();
        match self.resolve(&step, request.clone()).await {
            Ok(message) => self.stream_with_deltas(&sink, message, signal).await,
            Err(error) => {
                let message = self.error_message(error, &request.model.id);
                emit_error(&sink, ErrorReason::Error, message).await;
            }
        }
    }

    async fn run_deferred_fetch(
        &self,
        sink: AssistantMessageEventSink,
        model: Model,
        handle: DeferredHandle,
        options: RequestOptions,
    ) {
        notify_response(&options, &model);
        let signal = options.signal.clone();

        enum Next {
            StillPending(DeferredHandle),
            Ready(Box<AssistantMessage>),
            Failed(String),
            Resolve(FauxResponse, Box<FauxRequest>),
        }

        let next = {
            let mut deferred = self.deferred.lock().expect("faux deferred");
            match deferred.get_mut(&handle.id) {
                None => Next::Failed(format!("Unknown faux deferred response: {}", handle.id)),
                Some(entry)
                    if entry.handle.provider != handle.provider
                        || entry.handle.model_id != handle.model_id
                        || entry.handle.api != handle.api =>
                {
                    Next::Failed(format!("Unknown faux deferred response: {}", handle.id))
                }
                Some(entry) if entry.cancelled => Next::Failed(format!(
                    "Faux deferred response was cancelled: {}",
                    handle.id
                )),
                Some(entry) if entry.pending_fetches > 0 => {
                    entry.pending_fetches -= 1;
                    Next::StillPending(entry.handle.clone())
                }
                Some(entry) => match &entry.final_message {
                    Some(message) => Next::Ready(Box::new(message.clone())),
                    None => {
                        // The submission's abort signal, response hook and
                        // deferred flag do not apply to the fetch that
                        // materializes the response.
                        let mut request = entry.request.clone();
                        request.options.deferred = None;
                        request.options.stream.request.signal = None;
                        request.options.stream.request.on_response = None;
                        Next::Resolve(entry.step.clone(), Box::new(request))
                    }
                },
            }
        };

        let message = match next {
            Next::Failed(error) => {
                let message = self.error_message(error, &model.id);
                emit_error(&sink, ErrorReason::Error, message).await;
                return;
            }
            Next::StillPending(handle) => self.deferred_message(&model, handle),
            Next::Ready(message) => *message,
            Next::Resolve(step, request) => {
                let resolved = match self.resolve(&step, *request).await {
                    Ok(message) => message,
                    Err(error) => self.error_message(error, &model.id),
                };
                if let Some(entry) = self
                    .deferred
                    .lock()
                    .expect("faux deferred")
                    .get_mut(&handle.id)
                {
                    entry.final_message = Some(resolved.clone());
                }
                resolved
            }
        };

        self.stream_with_deltas(&sink, message, signal).await;
    }

    async fn resolve(
        &self,
        step: &FauxResponse,
        request: FauxRequest,
    ) -> Result<AssistantMessage, String> {
        let context = request.context.clone();
        let stream_options = request.options.stream.clone();
        let model_id = request.model.id.clone();
        let mut message = step.resolve(request).await?;
        message.api = self.api.clone();
        message.provider = self.provider.clone();
        message.model = model_id;
        if message.timestamp == 0 {
            message.timestamp = now_ms();
        }
        self.apply_usage_estimate(&mut message, &context, &stream_options);
        Ok(message)
    }

    fn error_message(&self, error: impl Into<String>, model_id: &str) -> AssistantMessage {
        let mut message = AssistantMessage::pending(&self.api, &self.provider, model_id);
        message.stop_reason = StopReason::Error;
        message.error_message = Some(error.into());
        message
    }

    fn deferred_message(&self, model: &Model, handle: DeferredHandle) -> AssistantMessage {
        let mut message = AssistantMessage::pending(model.api.as_str(), &model.provider, &model.id);
        message.stop_reason = StopReason::Deferred;
        message.deferred = Some(handle);
        message
    }

    /// Port of `withUsageEstimate`: four characters per token, with prompt
    /// caching simulated per `session_id` by common-prefix length.
    fn apply_usage_estimate(
        &self,
        message: &mut AssistantMessage,
        context: &Context,
        options: &StreamOptions,
    ) {
        let prompt_text = serialize_context(context);
        let prompt_tokens = estimate_text_tokens(&prompt_text);
        let output_tokens = estimate_text_tokens(&assistant_content_to_text(&message.content));

        let mut input = prompt_tokens;
        let mut cache_read = 0;
        let mut cache_write = 0;

        if let Some(session_id) = options.session_id.as_ref() {
            if options.cache_retention != Some(CacheRetention::None) {
                let mut cache = self.prompt_cache.lock().expect("faux cache");
                match cache.get(session_id) {
                    Some(previous) => {
                        let shared = common_prefix_len(previous, &prompt_text);
                        cache_read = estimate_text_tokens(&previous[..shared]);
                        cache_write = estimate_text_tokens(&prompt_text[shared..]);
                        // Upstream is `Math.max(0, promptTokens - cacheRead)`:
                        // an explicit clamp at zero, not signed saturation.
                        input = (prompt_tokens - cache_read).max(0);
                    }
                    None => cache_write = prompt_tokens,
                }
                cache.insert(session_id.clone(), prompt_text);
            }
        }

        message.usage = Usage {
            input,
            output: output_tokens,
            cache_read,
            cache_write,
            cache_write_1h: None,
            reasoning: None,
            total_tokens: input + output_tokens + cache_read + cache_write,
            cost: Default::default(),
        };
    }

    fn split_chunks(&self, text: &str) -> Vec<String> {
        let chars: Vec<char> = text.chars().collect();
        let mut chunks = Vec::new();
        let mut index = 0usize;
        while index < chars.len() {
            let span = self.next_token_size() as usize * 4;
            let span = span.max(1);
            let end = (index + span).min(chars.len());
            chunks.push(chars[index..end].iter().collect());
            index = end;
        }
        if chunks.is_empty() {
            chunks.push(String::new());
        }
        chunks
    }

    fn next_token_size(&self) -> u32 {
        if self.min_token_size == self.max_token_size {
            return self.min_token_size;
        }
        let span = (self.max_token_size - self.min_token_size + 1) as u64;
        let mut state = self.rng.lock().expect("faux rng");
        *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = *state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        self.min_token_size + (z % span) as u32
    }

    async fn pace(&self, chunk: &str) {
        match self.tokens_per_second {
            Some(rate) if rate > 0.0 => {
                let seconds = estimate_text_tokens(chunk) as f64 / rate;
                tokio::time::sleep(Duration::from_secs_f64(seconds)).await;
            }
            _ => tokio::task::yield_now().await,
        }
    }

    /// Port of `streamWithDeltas`. Emits the whole event sequence for one
    /// scripted message, honouring the abort signal between every chunk.
    async fn stream_with_deltas(
        &self,
        sink: &AssistantMessageEventSink,
        message: AssistantMessage,
        signal: Option<pi_core::options::AbortSignal>,
    ) {
        let aborted = || signal.as_ref().is_some_and(|s| s.is_aborted());

        let mut partial = message.clone();
        partial.content = Vec::new();
        partial.stop_reason = StopReason::Pending;

        if aborted() {
            emit_error(sink, ErrorReason::Aborted, aborted_message(&partial)).await;
            return;
        }

        if !sink
            .send(AssistantMessageEvent::Start {
                partial: partial.clone(),
            })
            .await
        {
            return;
        }

        for (index, block) in message.content.iter().enumerate() {
            if aborted() {
                emit_error(sink, ErrorReason::Aborted, aborted_message(&partial)).await;
                return;
            }

            match block {
                AssistantContent::Thinking(thinking) => {
                    partial
                        .content
                        .push(AssistantContent::Thinking(ThinkingContent::default()));
                    if !sink
                        .send(AssistantMessageEvent::ThinkingStart {
                            content_index: index,
                            partial: partial.clone(),
                        })
                        .await
                    {
                        return;
                    }
                    for chunk in self.split_chunks(&thinking.thinking) {
                        self.pace(&chunk).await;
                        if aborted() {
                            emit_error(sink, ErrorReason::Aborted, aborted_message(&partial)).await;
                            return;
                        }
                        if let Some(AssistantContent::Thinking(current)) =
                            partial.content.get_mut(index)
                        {
                            current.thinking.push_str(&chunk);
                        }
                        if !sink
                            .send(AssistantMessageEvent::ThinkingDelta {
                                content_index: index,
                                delta: chunk,
                                partial: partial.clone(),
                            })
                            .await
                        {
                            return;
                        }
                    }
                    if !sink
                        .send(AssistantMessageEvent::ThinkingEnd {
                            content_index: index,
                            content: thinking.thinking.clone(),
                            partial: partial.clone(),
                        })
                        .await
                    {
                        return;
                    }
                }
                AssistantContent::Text(text) => {
                    partial
                        .content
                        .push(AssistantContent::Text(TextContent::default()));
                    if !sink
                        .send(AssistantMessageEvent::TextStart {
                            content_index: index,
                            partial: partial.clone(),
                        })
                        .await
                    {
                        return;
                    }
                    for chunk in self.split_chunks(&text.text) {
                        self.pace(&chunk).await;
                        if aborted() {
                            emit_error(sink, ErrorReason::Aborted, aborted_message(&partial)).await;
                            return;
                        }
                        if let Some(AssistantContent::Text(current)) =
                            partial.content.get_mut(index)
                        {
                            current.text.push_str(&chunk);
                        }
                        if !sink
                            .send(AssistantMessageEvent::TextDelta {
                                content_index: index,
                                delta: chunk,
                                partial: partial.clone(),
                            })
                            .await
                        {
                            return;
                        }
                    }
                    if !sink
                        .send(AssistantMessageEvent::TextEnd {
                            content_index: index,
                            content: text.text.clone(),
                            partial: partial.clone(),
                        })
                        .await
                    {
                        return;
                    }
                }
                AssistantContent::ToolCall(call) => {
                    partial.content.push(AssistantContent::ToolCall(ToolCall {
                        id: call.id.clone(),
                        name: call.name.clone(),
                        arguments: Map::new(),
                        thought_signature: None,
                        namespace: None,
                    }));
                    if !sink
                        .send(AssistantMessageEvent::ToolCallStart {
                            content_index: index,
                            partial: partial.clone(),
                        })
                        .await
                    {
                        return;
                    }
                    let arguments =
                        serde_json::to_string(&call.arguments).unwrap_or_else(|_| "{}".to_string());
                    for chunk in self.split_chunks(&arguments) {
                        self.pace(&chunk).await;
                        if aborted() {
                            emit_error(sink, ErrorReason::Aborted, aborted_message(&partial)).await;
                            return;
                        }
                        // Upstream leaves `arguments` empty until the call ends;
                        // only the delta text carries the fragments.
                        if !sink
                            .send(AssistantMessageEvent::ToolCallDelta {
                                content_index: index,
                                delta: chunk,
                                partial: partial.clone(),
                            })
                            .await
                        {
                            return;
                        }
                    }
                    if let Some(AssistantContent::ToolCall(current)) =
                        partial.content.get_mut(index)
                    {
                        current.arguments = call.arguments.clone();
                    }
                    if !sink
                        .send(AssistantMessageEvent::ToolCallEnd {
                            content_index: index,
                            tool_call: call.clone(),
                            partial: partial.clone(),
                        })
                        .await
                    {
                        return;
                    }
                }
            }
        }

        match message.stop_reason {
            StopReason::Pending => {
                let error =
                    self.error_message("Faux response ended without a stop reason", &message.model);
                emit_error(sink, ErrorReason::Error, error).await;
            }
            StopReason::Error => emit_error(sink, ErrorReason::Error, message).await,
            StopReason::Aborted => emit_error(sink, ErrorReason::Aborted, message).await,
            StopReason::Stop => emit_done(sink, DoneReason::Stop, message).await,
            StopReason::Length => emit_done(sink, DoneReason::Length, message).await,
            StopReason::ToolUse => emit_done(sink, DoneReason::ToolUse, message).await,
            StopReason::Deferred => emit_done(sink, DoneReason::Deferred, message).await,
        }
    }
}

async fn emit_done(
    sink: &AssistantMessageEventSink,
    reason: DoneReason,
    message: AssistantMessage,
) {
    sink.send(AssistantMessageEvent::Done { reason, message })
        .await;
}

async fn emit_error(
    sink: &AssistantMessageEventSink,
    reason: ErrorReason,
    error: AssistantMessage,
) {
    sink.send(AssistantMessageEvent::Error { reason, error })
        .await;
}

fn aborted_message(partial: &AssistantMessage) -> AssistantMessage {
    let mut message = partial.clone();
    message.stop_reason = StopReason::Aborted;
    message.error_message = Some("Request was aborted".to_string());
    message.timestamp = now_ms();
    message
}

fn notify_response(options: &RequestOptions, model: &Model) {
    if let Some(on_response) = &options.on_response {
        on_response(
            &ProviderResponse {
                status: 200,
                headers: Default::default(),
            },
            model,
        );
    }
}

fn common_prefix_len(a: &str, b: &str) -> usize {
    let mut length = 0;
    for (left, right) in a.char_indices().zip(b.char_indices()) {
        if left.1 != right.1 {
            break;
        }
        length = left.0 + left.1.len_utf8();
    }
    length
}

fn random_id(prefix: &str) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}:{}:{n}", now_ms())
}

// ---------------------------------------------------------------------------
// Context serialization (usage estimation input)
// ---------------------------------------------------------------------------

fn input_content_to_text(content: &[InputContent]) -> String {
    content
        .iter()
        .map(|block| match block {
            InputContent::Text(text) => text.text.clone(),
            InputContent::Image(image) => {
                format!("[image:{}:{}]", image.mime_type, image.data.len())
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn user_content_to_text(content: &UserContent) -> String {
    match content {
        UserContent::Text(text) => text.clone(),
        UserContent::Blocks(blocks) => input_content_to_text(blocks),
    }
}

fn assistant_content_to_text(content: &[AssistantContent]) -> String {
    content
        .iter()
        .map(|block| match block {
            AssistantContent::Text(text) => text.text.clone(),
            AssistantContent::Thinking(thinking) => thinking.thinking.clone(),
            AssistantContent::ToolCall(call) => format!(
                "{}:{}",
                call.name,
                serde_json::to_string(&call.arguments).unwrap_or_else(|_| "{}".into())
            ),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn message_to_text(message: &Message) -> String {
    match message {
        Message::User(user) => user_content_to_text(&user.content),
        Message::Assistant(assistant) => assistant_content_to_text(&assistant.content),
        Message::ToolResult(result) => {
            let mut parts = vec![result.tool_name.clone()];
            parts.extend(
                result
                    .content
                    .iter()
                    .map(|block| input_content_to_text(std::slice::from_ref(block))),
            );
            parts.join("\n")
        }
    }
}

/// Deterministic textual view of a request, used for token estimation and the
/// simulated prompt cache. Mirrors upstream's `serializeContext`.
pub fn serialize_context(context: &Context) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(system_prompt) = &context.system_prompt {
        parts.push(format!("system:{system_prompt}"));
    }
    for message in &context.messages {
        parts.push(format!("{}:{}", message.role(), message_to_text(message)));
    }
    let tools = context.tools();
    if !tools.is_empty() {
        parts.push(format!(
            "tools:{}",
            serde_json::to_string(tools).unwrap_or_default()
        ));
    }
    parts.join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_prefix_is_char_aligned() {
        assert_eq!(common_prefix_len("abc", "abd"), 2);
        assert_eq!(common_prefix_len("🌍x", "🌍y"), 4);
        assert_eq!(common_prefix_len("", "abc"), 0);
    }

    #[test]
    fn fixed_token_size_chunks_by_four_chars() {
        let core = FauxCore::new(FauxOptions::default().token_size(FauxTokenSize::fixed(1)));
        assert_eq!(core.split_chunks("abcdefgh"), vec!["abcd", "efgh"]);
        assert_eq!(core.split_chunks(""), vec![""]);
    }

    #[test]
    fn random_chunk_sizes_stay_in_range() {
        let core =
            FauxCore::new(FauxOptions::default().token_size(FauxTokenSize { min: 2, max: 4 }));
        for chunk in core.split_chunks(&"x".repeat(200)) {
            let tokens = chunk.chars().count();
            assert!((8..=16).contains(&tokens) || tokens < 8, "chunk {tokens}");
        }
    }
}
