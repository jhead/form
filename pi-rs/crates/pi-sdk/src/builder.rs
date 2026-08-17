//! Assembling the SDK: catalog + provider adapters + credentials, then agents.
//!
//! Upstream wires this together in the coding-agent CLI, which is out of scope
//! for the port. The equivalent has to live somewhere, so it lives here — an
//! embedder should not have to know that a `ModelRegistry` needs adapters
//! registered by api id before it can resolve a model.

use std::sync::Arc;

use pi_agent::{Agent, AgentOptions, InitialAgentState};
use pi_auth::{AuthContext, CredentialStore, DefaultAuthContext, InMemoryCredentialStore};
use pi_catalog::{CatalogError, ModelRegistry};
use pi_core::{AiError, Model, ModelThinkingLevel, StreamFn};
use pi_tools::{AgentToolRef, ExecutionEnvRef};

/// Everything that can go wrong assembling or driving the SDK.
///
/// Flat and code-tagged, like [`pi_core::AiError`], because FFI callers match
/// on [`SdkError::code`] and display [`SdkError::message`].
#[derive(Debug, Clone, thiserror::Error)]
pub enum SdkError {
    #[error("catalog error: {0}")]
    Catalog(String),
    #[error("auth error: {0}")]
    Auth(String),
    #[error(transparent)]
    Ai(#[from] AiError),
    #[error("agent error: {0}")]
    Agent(String),
    #[error("configuration error: {0}")]
    Config(String),
}

impl SdkError {
    /// Stable machine-readable code. FFI callers switch on this.
    pub fn code(&self) -> &'static str {
        match self {
            SdkError::Catalog(_) => "catalog",
            SdkError::Auth(_) => "auth",
            SdkError::Ai(err) => err.code(),
            SdkError::Agent(_) => "agent",
            SdkError::Config(_) => "config",
        }
    }

    pub fn message(&self) -> String {
        self.to_string()
    }
}

impl From<CatalogError> for SdkError {
    fn from(err: CatalogError) -> Self {
        SdkError::Catalog(err.to_string())
    }
}

impl From<pi_auth::AuthError> for SdkError {
    fn from(err: pi_auth::AuthError) -> Self {
        SdkError::Auth(err.to_string())
    }
}

impl From<pi_agent::AgentError> for SdkError {
    fn from(err: pi_agent::AgentError) -> Self {
        SdkError::Agent(err.to_string())
    }
}

/// The assembled SDK. Cheap to clone; every field is behind an `Arc`.
#[derive(Clone)]
pub struct Pi {
    registry: Arc<ModelRegistry>,
    credentials: Arc<dyn CredentialStore>,
    auth_context: Arc<dyn AuthContext>,
}

impl Pi {
    pub fn builder() -> PiBuilder {
        PiBuilder::default()
    }

    /// The model catalog and provider registry.
    pub fn registry(&self) -> &Arc<ModelRegistry> {
        &self.registry
    }

    pub fn credentials(&self) -> &Arc<dyn CredentialStore> {
        &self.credentials
    }

    /// Resolve `"provider/model-id"` (or a bare, unambiguous model id) to a
    /// [`Model`], with credentials applied where the provider needs them.
    pub async fn resolve_model(&self, reference: &str) -> Result<Model, SdkError> {
        Ok(self.registry.resolve_model(reference)?)
    }

    /// Every model the catalog knows about.
    pub fn models(&self) -> Vec<Model> {
        self.registry.models()
    }

    /// A [`StreamFn`] bound to `model`'s API adapter.
    pub fn stream_fn_for(&self, model: &Model) -> Result<StreamFn, SdkError> {
        Ok(self.registry.stream_fn_for_model(model)?)
    }

    /// Resolve credentials for a provider: stored credential first, then the
    /// provider's environment variables, refreshing an expiring OAuth token
    /// transparently.
    ///
    /// `Ok(None)` means the provider has no usable credential — distinct from
    /// an error, which means resolution itself failed.
    ///
    /// Note the precedence: a *stored* credential owns the provider, and env is
    /// consulted only when nothing is stored. That is upstream's order, and it
    /// is deliberate — a stale environment variable must not silently override
    /// a credential the user explicitly saved.
    pub async fn resolve_auth(
        &self,
        provider_id: &str,
    ) -> Result<Option<pi_auth::AuthResult>, SdkError> {
        let provider = self.auth_provider(provider_id);
        Ok(pi_auth::resolve_provider_auth(
            &provider,
            self.credentials.clone(),
            self.auth_context.clone(),
            &pi_auth::AuthResolutionOverrides::default(),
        )
        .await?)
    }

    /// The auth description for a provider: its env-var chain plus the built-in
    /// OAuth flow when one is registered for that id.
    pub fn auth_provider(&self, provider_id: &str) -> pi_auth::AuthProvider {
        let env_vars = pi_auth::api_key_env_vars(provider_id).unwrap_or_default();
        let mut auth = pi_auth::ProviderAuth::api_key(Arc::new(pi_auth::EnvApiKeyAuth::new(
            format!("{provider_id} API key"),
            env_vars,
        )));
        if let Some(flow) = pi_auth::load_oauth_flow(provider_id) {
            auth = auth.with_oauth(flow);
        }
        pi_auth::AuthProvider::new(provider_id, auth)
    }

    /// Start building an [`Agent`] against this catalog.
    pub fn agent(&self) -> AgentBuilder {
        AgentBuilder::new(self.clone())
    }
}

/// Builder for [`Pi`].
#[derive(Default)]
pub struct PiBuilder {
    registry: Option<Arc<ModelRegistry>>,
    credentials: Option<Arc<dyn CredentialStore>>,
    auth_context: Option<Arc<dyn AuthContext>>,
    register_builtins: bool,
    extra_clients: Vec<pi_core::ApiClientRef>,
    extra_providers: Vec<pi_catalog::Provider>,
}

impl PiBuilder {
    /// Register every built-in provider adapter (Anthropic, the OpenAI family,
    /// Google, Mistral, pi-messages). Without this the catalog knows about
    /// models but has no adapter able to run them.
    pub fn with_builtin_providers(mut self) -> Self {
        self.register_builtins = true;
        self
    }

    /// Register an additional or replacement adapter, keyed by its `api()`.
    ///
    /// This registers the *adapter* only. If the provider is not already in the
    /// catalog, register the provider too — see [`PiBuilder::with_provider`].
    pub fn with_api_client(mut self, client: pi_core::ApiClientRef) -> Self {
        self.extra_clients.push(client);
        self
    }

    /// Register a custom provider and its models alongside an adapter.
    ///
    /// Adapters are keyed by api id and providers by provider id; registering
    /// only the adapter leaves model resolution failing with
    /// `Unknown provider`, which is the easy mistake here.
    pub fn with_provider(
        mut self,
        provider: pi_catalog::Provider,
        client: pi_core::ApiClientRef,
    ) -> Self {
        self.extra_providers.push(provider);
        self.extra_clients.push(client);
        self
    }

    /// Supply a pre-built registry. Defaults to the full generated catalog.
    pub fn with_registry(mut self, registry: Arc<ModelRegistry>) -> Self {
        self.registry = Some(registry);
        self
    }

    /// Where credentials are read from and written to. Defaults to an
    /// in-memory store, so nothing touches disk unless asked; pass
    /// `pi_auth::FileCredentialStore` for upstream's `~/.pi/agent/auth.json`.
    pub fn with_credentials(mut self, store: Arc<dyn CredentialStore>) -> Self {
        self.credentials = Some(store);
        self
    }

    pub fn with_auth_context(mut self, context: Arc<dyn AuthContext>) -> Self {
        self.auth_context = Some(context);
        self
    }

    pub fn build(self) -> Result<Pi, SdkError> {
        let registry = self
            .registry
            .unwrap_or_else(|| Arc::new(ModelRegistry::with_builtins()));

        if self.register_builtins {
            for client in crate::providers::builtin_api_clients() {
                registry.register_api(client);
            }
        }
        for client in self.extra_clients {
            registry.register_api(client);
        }
        for provider in self.extra_providers {
            registry.set_provider(provider);
        }

        Ok(Pi {
            registry,
            credentials: self
                .credentials
                .unwrap_or_else(|| Arc::new(InMemoryCredentialStore::new())),
            auth_context: self.auth_context.unwrap_or_else(DefaultAuthContext::shared),
        })
    }
}

/// Builder for a configured [`Agent`].
///
/// The one piece of assembly worth knowing about: an `Agent` needs a
/// [`StreamFn`], and this resolves it from the model's API adapter so callers
/// never wire a provider by hand.
pub struct AgentBuilder {
    pi: Pi,
    model: Option<Model>,
    system_prompt: Option<String>,
    tools: Vec<AgentToolRef>,
    env: Option<ExecutionEnvRef>,
    thinking_level: Option<ModelThinkingLevel>,
    session_id: Option<String>,
    stream_fn: Option<StreamFn>,
    options: AgentOptions,
}

impl AgentBuilder {
    fn new(pi: Pi) -> Self {
        Self {
            pi,
            model: None,
            system_prompt: None,
            tools: Vec::new(),
            env: None,
            thinking_level: None,
            session_id: None,
            stream_fn: None,
            options: AgentOptions::default(),
        }
    }

    pub fn model(mut self, model: Model) -> Self {
        self.model = Some(model);
        self
    }

    pub fn system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(prompt.into());
        self
    }

    pub fn tools(mut self, tools: Vec<AgentToolRef>) -> Self {
        self.tools = tools;
        self
    }

    /// Give the built-in tools a real filesystem and shell. Without this the
    /// agent runs against an empty in-memory environment.
    pub fn env(mut self, env: ExecutionEnvRef) -> Self {
        self.env = Some(env);
        self
    }

    pub fn thinking_level(mut self, level: ModelThinkingLevel) -> Self {
        self.thinking_level = Some(level);
        self
    }

    /// Forwarded to providers for cache-aware routing.
    pub fn session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    /// Override the resolved stream function — mainly for tests, where
    /// `pi_provider_misc::faux::FauxProvider::stream_fn()` is the usual choice.
    pub fn stream_fn(mut self, stream_fn: StreamFn) -> Self {
        self.stream_fn = Some(stream_fn);
        self
    }

    /// Escape hatch for the hooks the builder does not surface individually.
    pub fn options(mut self, options: AgentOptions) -> Self {
        self.options = options;
        self
    }

    pub fn build(mut self) -> Result<Agent, SdkError> {
        let model = self
            .model
            .clone()
            .ok_or_else(|| SdkError::Config("agent requires a model".into()))?;

        let stream_fn = match self.stream_fn.take() {
            Some(f) => f,
            None => self.pi.stream_fn_for(&model)?,
        };

        self.options.stream_fn = Some(stream_fn);
        self.options.session_id = self.session_id;
        self.options.initial_state = Some(InitialAgentState {
            system_prompt: self.system_prompt,
            model: Some(model),
            thinking_level: self.thinking_level,
            tools: if self.tools.is_empty() {
                None
            } else {
                Some(self.tools)
            },
            env: self.env,
            messages: None,
        });

        Ok(Agent::new(self.options))
    }
}
