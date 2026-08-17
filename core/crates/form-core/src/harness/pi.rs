//! The real harness: `pi-agent` driving a live provider.
//!
//! This replaces [`super::StubHarness`] in the running app. The translation below is short
//! because form's event protocol was derived from `pi`'s in the first place — see
//! `docs/specs/16-pi-integration.md`.
//!
//! Two things are deliberately not `pi`'s job and stay here: mapping form's `ModelRef` onto a
//! `provider/model` reference string, and refreshing OpenRouter's model list at runtime.

use std::sync::Arc;

use pi_agent::types::AgentEvent;
use pi_auth::{ApiKeyCredential, Credential, InMemoryCredentialStore};
use pi_core::{AbortSignal as PiAbortSignal, Api, Model as PiModel, ModelCost, ModelCostRates};
use pi_sdk::Pi;

use crate::harness::{AbortSignal, Harness, RunContext, RunRequest, TurnRecord};
use crate::protocol::{EntryKind, EventKind, Message, ModelRef, RunOutcome, ThinkingLevel};

const OPENROUTER_MODELS_URL: &str = "https://openrouter.ai/api/v1/models";

/// A built SDK plus the catalog refresh, shared across runs.
pub struct PiHarness {
    pi: Arc<Pi>,
    system_prompt: String,
}

impl PiHarness {
    /// Build the SDK and register OpenRouter's live model list.
    ///
    /// The bundled catalog is a point-in-time snapshot and goes stale: `z-ai/glm-5.2:free`
    /// is served by OpenRouter today but is not in it. Refreshing at startup means a model
    /// the user can actually call is a model the app can actually select. A failed refresh
    /// is not fatal — the snapshot still resolves hundreds of models.
    pub async fn new(system_prompt: String) -> Result<Self, String> {
        // The SDK's default store is empty and never consults the environment, so a key in
        // `.env` would resolve to "No API key for provider". Seed the store from the env
        // instead, using pi's own per-provider variable names.
        let pi = Pi::builder()
            .with_builtin_providers()
            .with_credentials(Arc::new(InMemoryCredentialStore::with_credentials(
                credentials_from_env(),
            )))
            .build()
            .map_err(|e| format!("pi sdk: {}", e.message()))?;
        let pi = Arc::new(pi);

        if let Err(reason) = refresh_openrouter_models(&pi).await {
            tracing::warn!(%reason, "openrouter catalog refresh failed; using the bundled snapshot");
        }

        Ok(Self { pi, system_prompt })
    }

    pub fn model_count(&self) -> usize {
        self.pi.models().len()
    }

    /// The catalog as form models it, grouped by provider.
    ///
    /// This replaces form's hand-written table. Everything the picker shows is now a model
    /// the SDK can actually resolve, and the context ring gets a real window instead of zero.
    pub fn catalog(&self) -> crate::catalog::Catalog {
        use crate::catalog::{AuthMethod, Capabilities, Model, Pricing, Provider};
        use std::collections::BTreeMap;

        let mut providers: BTreeMap<String, Provider> = BTreeMap::new();
        for model in self.pi.models() {
            let entry = providers
                .entry(model.provider.clone())
                .or_insert_with(|| Provider {
                    id: model.provider.clone(),
                    name: pretty_provider(&model.provider),
                    base_url: model.base_url.clone(),
                    auth: vec![AuthMethod::ApiKey],
                    env_vars: pi_auth::api_key_env_vars(&model.provider)
                        .unwrap_or_default()
                        .into_iter()
                        .map(str::to_string)
                        .collect(),
                    models: Vec::new(),
                    note: None,
                });

            let thinking_levels = if model.reasoning {
                crate::catalog::ALL_THINKING_LEVELS.to_vec()
            } else {
                vec![ThinkingLevel::Off]
            };

            entry.models.push(Model {
                id: model.id.clone(),
                name: model.name.clone(),
                family: model.provider.clone(),
                context_window: model.context_window,
                max_output: model.max_tokens,
                pricing: Pricing {
                    input: model.cost.rates.input,
                    output: model.cost.rates.output,
                    cache_read: model.cost.rates.cache_read,
                    cache_write: model.cost.rates.cache_write,
                },
                capabilities: Capabilities {
                    vision: model
                        .input
                        .iter()
                        .any(|m| matches!(m, pi_core::Modality::Image)),
                    tools: true,
                    reasoning: model.reasoning,
                    caching: model.cost.rates.cache_read > 0.0,
                    streaming: true,
                },
                thinking_levels,
                released: None,
                deprecated: false,
            });
        }

        for provider in providers.values_mut() {
            provider.models.sort_by(|a, b| a.id.cmp(&b.id));
        }
        crate::catalog::Catalog {
            providers: providers.into_values().collect(),
            generated_at: crate::protocol::now_ms().to_string(),
            note: Some("live from pi-catalog, with OpenRouter refreshed at startup".into()),
        }
    }

    /// Resolve one of form's `ModelRef`s against the catalog.
    pub async fn resolve(&self, model_ref: &ModelRef) -> Result<PiModel, String> {
        let reference = format!("{}/{}", model_ref.provider_id, model_ref.model_id);
        self.pi
            .resolve_model(&reference)
            .await
            .map_err(|e| format!("{reference}: {}", e.message()))
    }
}

/// `openrouter` reads better as `OpenRouter`. Anything unknown is title-cased on word
/// boundaries rather than left as an id.
fn pretty_provider(id: &str) -> String {
    match id {
        "openrouter" => "OpenRouter".to_string(),
        "openai" => "OpenAI".to_string(),
        "xai" => "xAI".to_string(),
        "deepseek" => "DeepSeek".to_string(),
        "google-vertex" => "Google Vertex".to_string(),
        "github-copilot" => "GitHub Copilot".to_string(),
        other => other
            .split(['-', '_'])
            .map(|word| {
                let mut chars = word.chars();
                match chars.next() {
                    Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                    None => String::new(),
                }
            })
            .collect::<Vec<_>>()
            .join(" "),
    }
}

/// Every provider whose API-key variable is present in the environment.
fn credentials_from_env() -> Vec<(String, Credential)> {
    const PROVIDERS: &[&str] = &[
        "openrouter",
        "anthropic",
        "openai",
        "google",
        "groq",
        "mistral",
        "deepseek",
        "xai",
    ];
    PROVIDERS
        .iter()
        .filter_map(|provider| {
            let key = pi_auth::get_env_api_key(provider, None)?;
            Some((
                provider.to_string(),
                Credential::ApiKey(ApiKeyCredential::new(key)),
            ))
        })
        .collect()
}

/// Fetch the provider's current model list and register it.
async fn refresh_openrouter_models(pi: &Pi) -> Result<usize, String> {
    #[derive(serde::Deserialize)]
    struct Listing {
        data: Vec<Entry>,
    }
    #[derive(serde::Deserialize)]
    struct Entry {
        id: String,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        context_length: Option<u64>,
        #[serde(default)]
        pricing: Option<Pricing>,
        #[serde(default)]
        top_provider: Option<TopProvider>,
    }
    #[derive(serde::Deserialize)]
    struct Pricing {
        #[serde(default)]
        prompt: Option<String>,
        #[serde(default)]
        completion: Option<String>,
    }
    #[derive(serde::Deserialize)]
    struct TopProvider {
        #[serde(default)]
        max_completion_tokens: Option<u64>,
    }

    let response = reqwest::Client::new()
        .get(OPENROUTER_MODELS_URL)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("http {}", response.status()));
    }
    let listing: Listing = response.json().await.map_err(|e| e.to_string())?;

    // OpenRouter quotes prices per token as strings; the catalog carries $/million.
    let per_million = |value: &Option<String>| -> f64 {
        value
            .as_deref()
            .and_then(|v| v.parse::<f64>().ok())
            .map(|v| v * 1_000_000.0)
            .unwrap_or(0.0)
    };

    let models: Vec<PiModel> = listing
        .data
        .into_iter()
        .map(|entry| {
            let mut model = PiModel::new(
                entry.id.clone(),
                Api::OpenAiCompletions,
                "openrouter",
                "https://openrouter.ai/api/v1",
            );
            model.name = entry.name.unwrap_or_else(|| entry.id.clone());
            model.context_window = entry.context_length.unwrap_or(8_192);
            model.max_tokens = entry
                .top_provider
                .and_then(|p| p.max_completion_tokens)
                .unwrap_or_else(|| model.context_window.min(4_096));
            let pricing = entry.pricing.unwrap_or(Pricing {
                prompt: None,
                completion: None,
            });
            model.cost = ModelCost {
                rates: ModelCostRates {
                    input: per_million(&pricing.prompt),
                    output: per_million(&pricing.completion),
                    cache_read: 0.0,
                    cache_write: 0.0,
                },
                tiers: None,
            };
            model
        })
        .collect();

    let count = models.len();
    pi.registry()
        .set_provider_models("openrouter", models)
        .map_err(|e| e.to_string())?;
    Ok(count)
}

/// Supplies the API key per request.
///
/// `AgentBuilder` wires the stream function but leaves this hook unset, so without it every
/// request fails with "No API key for provider" even when the key is right there in the
/// environment. `Pi::resolve_auth` is the resolver that knows the precedence: a stored
/// credential owns the provider, and env is consulted only when nothing is stored.
struct AuthBridge {
    pi: Arc<Pi>,
}

#[async_trait::async_trait]
impl pi_agent::types::ApiKeyProvider for AuthBridge {
    async fn api_key(&self, provider: &str) -> Option<String> {
        match self.pi.resolve_auth(provider).await {
            Ok(Some(result)) => result.auth.api_key.clone(),
            Ok(None) => {
                tracing::warn!(provider, "no credential for provider");
                None
            }
            Err(e) => {
                tracing::warn!(provider, error = %e.message(), "credential resolution failed");
                None
            }
        }
    }
}

/// Bridges `pi`'s listener callback onto form's [`RunContext`].
struct Bridge {
    ctx: Arc<dyn RunContext>,
    session_id: String,
    run_id: String,
    entry: parking_lot_lite::Slot,
}

/// A tiny mutex slot; the assistant entry does not exist until `message_start` arrives.
mod parking_lot_lite {
    use std::sync::Mutex;

    use crate::protocol::Entry;

    #[derive(Default)]
    pub struct Slot(Mutex<Option<Entry>>);

    impl Slot {
        pub fn set(&self, value: Entry) {
            *self.0.lock().expect("slot poisoned") = Some(value);
        }
        pub fn get(&self) -> Option<Entry> {
            self.0.lock().expect("slot poisoned").clone()
        }
    }
}

#[async_trait::async_trait]
impl pi_agent::AgentEventListener for Bridge {
    async fn on_event(&self, event: AgentEvent, _signal: PiAbortSignal) {
        match event {
            AgentEvent::AgentStart => {}
            AgentEvent::TurnStart => self.ctx.emit(EventKind::TurnStart {
                session_id: self.session_id.clone(),
                run_id: self.run_id.clone(),
            }),
            AgentEvent::MessageStart { message } => {
                // Persist through the store so the transcript is durable, then report the
                // entry it was given — the app keys its rendering on that id.
                if let Some(message) = convert_message(&message) {
                    if let Some(entry) = self
                        .ctx
                        .append_entry(&self.session_id, EntryKind::Message { message })
                    {
                        self.entry.set(entry.clone());
                        self.ctx.emit(EventKind::MessageStart {
                            session_id: self.session_id.clone(),
                            entry,
                        });
                    }
                }
            }
            AgentEvent::MessageUpdate {
                assistant_message_event,
                ..
            } => {
                let Some(entry) = self.entry.get() else {
                    return;
                };
                let entry_id = entry.id;
                // The streaming event crosses unchanged: form's `AssistantMessageEvent` is
                // pi's, field for field, which `tests/pi_compat.rs` enforces.
                if let Ok(event) =
                    serde_json::to_value(&assistant_message_event).and_then(serde_json::from_value)
                {
                    self.ctx.emit(EventKind::MessageUpdate {
                        session_id: self.session_id.clone(),
                        entry_id,
                        event,
                    });
                }
            }
            AgentEvent::MessageEnd { message } => {
                let Some(mut entry) = self.entry.get() else {
                    return;
                };
                if let Some(converted) = convert_message(&message) {
                    entry.kind = EntryKind::Message { message: converted };
                    self.ctx.replace_entry(&entry);
                    self.ctx.emit(EventKind::MessageEnd {
                        session_id: self.session_id.clone(),
                        entry,
                    });
                }
            }
            AgentEvent::ToolExecutionStart {
                tool_call_id,
                tool_name,
                args,
            } => self.ctx.emit(EventKind::ToolExecutionStart {
                session_id: self.session_id.clone(),
                tool_call_id,
                tool_name,
                args,
            }),
            AgentEvent::ToolExecutionUpdate {
                tool_call_id,
                partial_result,
                ..
            } => self.ctx.emit(EventKind::ToolExecutionUpdate {
                session_id: self.session_id.clone(),
                tool_call_id,
                partial_result: serde_json::to_value(&partial_result).unwrap_or_default(),
            }),
            AgentEvent::ToolExecutionEnd {
                tool_call_id,
                result,
                is_error,
                ..
            } => self.ctx.emit(EventKind::ToolExecutionEnd {
                session_id: self.session_id.clone(),
                tool_call_id,
                result: serde_json::to_value(&result).unwrap_or_default(),
                is_error,
            }),
            AgentEvent::TurnEnd { .. } => {}
            AgentEvent::AgentEnd { .. } => {}
        }
    }
}

/// pi's `AgentMessage` is an open union; form models the three it renders.
fn convert_message(message: &pi_agent::types::AgentMessage) -> Option<Message> {
    serde_json::to_value(message)
        .ok()
        .and_then(|v| serde_json::from_value(v).ok())
}

fn thinking_level(level: ThinkingLevel) -> pi_core::ModelThinkingLevel {
    use pi_core::ModelThinkingLevel as P;
    match level {
        ThinkingLevel::Off => P::Off,
        ThinkingLevel::Minimal => P::Minimal,
        ThinkingLevel::Low => P::Low,
        ThinkingLevel::Medium => P::Medium,
        ThinkingLevel::High => P::High,
        ThinkingLevel::Xhigh => P::Xhigh,
        ThinkingLevel::Max => P::Max,
    }
}

#[async_trait::async_trait]
impl Harness for PiHarness {
    async fn run(&self, req: RunRequest, ctx: Arc<dyn RunContext>, abort: AbortSignal) {
        let started = crate::protocol::now_ms();

        ctx.emit(EventKind::RunStart {
            session_id: req.session_id.clone(),
            run_id: req.run_id.clone(),
        });

        let model = match self.resolve(&req.model).await {
            Ok(model) => model,
            Err(reason) => return fail(&ctx, &req, started, reason),
        };

        let mut options = pi_agent::AgentOptions::default();
        options.get_api_key = Some(Arc::new(AuthBridge {
            pi: self.pi.clone(),
        }));

        let agent = match self
            .pi
            .agent()
            .options(options)
            .model(model)
            .system_prompt(self.system_prompt.clone())
            .thinking_level(thinking_level(req.model.thinking_level))
            .session_id(req.session_id.clone())
            .build()
        {
            Ok(agent) => agent,
            Err(e) => return fail(&ctx, &req, started, format!("agent: {}", e.message())),
        };

        let bridge = Arc::new(Bridge {
            ctx: ctx.clone(),
            session_id: req.session_id.clone(),
            run_id: req.run_id.clone(),
            entry: Default::default(),
        });
        let _subscription = agent.subscribe(bridge);

        // Cancellation is explicit on both sides; poll form's flag and call pi's abort.
        let watch = {
            let abort = abort.clone();
            let agent = agent.clone();
            tokio::spawn(async move {
                while !abort.is_aborted() {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
                agent.abort();
            })
        };

        let outcome = match agent.prompt_text(req.prompt.clone(), Vec::new()).await {
            Ok(()) if abort.is_aborted() => RunOutcome::Aborted,
            Ok(()) => RunOutcome::Completed,
            Err(e) => {
                tracing::warn!(error = %e, "run failed");
                RunOutcome::Failed
            }
        };
        watch.abort();

        // `AgentState` carries messages, not a running total, so sum what the provider
        // actually reported rather than estimating.
        let usage = total_usage(&agent.state());
        let duration_ms = (crate::protocol::now_ms() - started).max(0) as u64;

        ctx.record_turn(TurnRecord {
            id: format!("trn_{}", uuid::Uuid::new_v4().simple()),
            session_id: req.session_id.clone(),
            run_id: req.run_id.clone(),
            model: req.model.clone(),
            started_at: started,
            ended_at: crate::protocol::now_ms(),
            ttft_ms: None,
            duration_ms: duration_ms as i64,
            usage: usage.clone(),
            outcome,
            tools: Vec::new(),
        });

        ctx.emit(EventKind::TurnEnd {
            session_id: req.session_id.clone(),
            run_id: req.run_id.clone(),
            usage: usage.clone(),
        });
        ctx.emit(EventKind::RunEnd {
            session_id: req.session_id.clone(),
            run_id: req.run_id.clone(),
            outcome,
            usage,
            duration_ms,
        });
    }
}

/// Sum the usage the provider reported across this run's assistant messages.
fn total_usage(state: &pi_agent::types::AgentState) -> crate::protocol::Usage {
    let mut total = crate::protocol::Usage::default();
    for message in &state.messages {
        if let Some(Message::Assistant(assistant)) = convert_message(message) {
            total = total.add(&assistant.usage);
        }
    }
    total
}

fn fail(ctx: &Arc<dyn RunContext>, req: &RunRequest, started: i64, reason: String) {
    tracing::error!(%reason, "run could not start");
    ctx.emit(EventKind::Error {
        code: "provider_error".to_string(),
        message: reason,
        detail: None,
    });
    ctx.emit(EventKind::RunEnd {
        session_id: req.session_id.clone(),
        run_id: req.run_id.clone(),
        outcome: RunOutcome::Failed,
        usage: Default::default(),
        duration_ms: (crate::protocol::now_ms() - started).max(0) as u64,
    });
}
