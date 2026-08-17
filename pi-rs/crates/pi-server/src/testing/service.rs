//! Port of `.upstream/packages/server/src/testing/service.ts`.
//!
//! Upstream's tests subclass `TestServerService` to inject failures and delays.
//! Rust has no inheritance, so the same variation points are injectable hooks.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use futures::future::Shared;
use futures::FutureExt;
use indexmap::IndexMap;
use parking_lot::Mutex;
use pi_protocol::{
    AssistantContent, AssistantRole, AssistantStatus, AssistantStopReason, AssistantTranscriptItem,
    Modality, ModelCost, ModelMetadata, ModelRef, SessionMetadata, SessionPhase, SessionSnapshot,
    TextContent, ThinkingLevel, TranscriptItem, TranscriptProgress, UserContent, UserRole,
    UserTranscriptItem,
};

use crate::errors::PiServerError;
use crate::types::{
    CreateSessionOptions, PromptInput, RuntimeEventListener, SessionRuntime, SessionRuntimeEvent,
    SessionService, Unsubscribe,
};

pub fn test_model() -> ModelMetadata {
    ModelMetadata {
        provider: "test".to_string(),
        id: "small".to_string(),
        name: "Test Small".to_string(),
        api: "test-api".to_string(),
        reasoning: true,
        input: vec![Modality::Text, Modality::Image],
        context_window: 16_000,
        max_tokens: 2_000,
        cost: ModelCost {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
        },
        supported_thinking_levels: vec![
            ThinkingLevel::Off,
            ThinkingLevel::Medium,
            ThinkingLevel::High,
        ],
        authenticated: true,
    }
}

type SharedResult<T> = Shared<futures::channel::oneshot::Receiver<T>>;

/// A promise that can be resolved once and awaited many times.
pub struct Deferred<T: Clone + Send + Sync + 'static> {
    sender: Mutex<Option<futures::channel::oneshot::Sender<T>>>,
    receiver: SharedResult<T>,
}

impl<T: Clone + Send + Sync + 'static> Default for Deferred<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Clone + Send + Sync + 'static> Deferred<T> {
    pub fn new() -> Self {
        let (sender, receiver) = futures::channel::oneshot::channel();
        Self {
            sender: Mutex::new(Some(sender)),
            receiver: receiver.shared(),
        }
    }

    pub fn resolve(&self, value: T) {
        if let Some(sender) = self.sender.lock().take() {
            let _ = sender.send(value);
        }
    }

    pub async fn wait(&self) -> Option<T> {
        self.receiver.clone().await.ok()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptOutcome {
    Complete,
    Aborted,
}

#[derive(Clone)]
struct StoredSession {
    snapshot: SessionSnapshot,
}

type Listeners = Arc<Mutex<Vec<(u64, RuntimeEventListener)>>>;

pub struct TestSessionRuntime {
    id: String,
    stored: Arc<Mutex<HashMap<String, StoredSession>>>,
    on_dispose: Box<dyn Fn() + Send + Sync>,
    listeners: Listeners,
    next_listener_id: Mutex<u64>,
    pending_prompt: Mutex<Option<Arc<Deferred<PromptOutcome>>>>,
    pub disposed: Arc<Deferred<()>>,
    dispose_count: Mutex<u64>,
    steers: Mutex<Vec<PromptInput>>,
}

impl TestSessionRuntime {
    fn snapshot_now(&self) -> SessionSnapshot {
        self.stored
            .lock()
            .get(&self.id)
            .expect("stored session")
            .snapshot
            .clone()
    }

    pub fn dispose_count(&self) -> u64 {
        *self.dispose_count.lock()
    }

    pub fn steers(&self) -> Vec<PromptInput> {
        self.steers.lock().clone()
    }

    pub fn set_phase(&self, phase: SessionPhase) {
        let mut stored = self.stored.lock();
        if let Some(session) = stored.get_mut(&self.id) {
            session.snapshot.phase = phase;
        }
    }

    pub fn finish_prompt(&self) {
        let pending = self.pending_prompt.lock().clone();
        pending
            .expect("No prompt is pending")
            .resolve(PromptOutcome::Complete);
    }

    pub fn emit_progress(&self, progress: TranscriptProgress) {
        self.emit(SessionRuntimeEvent::Progress(progress));
    }

    pub fn emit_error(&self, error: PiServerError) {
        self.emit(SessionRuntimeEvent::Error(error));
    }

    pub fn emit_snapshot(&self) {
        self.emit(SessionRuntimeEvent::Snapshot);
    }

    fn emit(&self, event: SessionRuntimeEvent) {
        let listeners: Vec<RuntimeEventListener> = self
            .listeners
            .lock()
            .iter()
            .map(|(_, listener)| Arc::clone(listener))
            .collect();
        for listener in listeners {
            listener(event.clone());
        }
    }

    fn update(&self, mutate: impl FnOnce(&mut SessionSnapshot)) {
        {
            let mut stored = self.stored.lock();
            let session = stored.get_mut(&self.id).expect("stored session");
            mutate(&mut session.snapshot);
            session.snapshot.revision += 1;
            session.snapshot.updated_at += 1;
        }
        self.emit_snapshot();
    }
}

#[async_trait]
impl SessionRuntime for TestSessionRuntime {
    async fn snapshot(&self) -> Result<SessionSnapshot, PiServerError> {
        Ok(self.snapshot_now())
    }

    fn phase(&self) -> SessionPhase {
        self.snapshot_now().phase
    }

    async fn prompt(&self, input: PromptInput) -> Result<(), PiServerError> {
        if self.phase() != SessionPhase::Idle {
            return Err(PiServerError::busy("A prompt is already running"));
        }
        let done = Arc::new(Deferred::new());
        *self.pending_prompt.lock() = Some(Arc::clone(&done));
        let text = input.text.clone();
        self.update(|snapshot| {
            let revision = snapshot.revision + 1;
            snapshot.phase = SessionPhase::Turn;
            snapshot
                .transcript
                .push(TranscriptItem::User(UserTranscriptItem {
                    id: format!("user-{revision}"),
                    role: UserRole::User,
                    content: vec![UserContent::Text(TextContent { text: text.clone() })],
                    timestamp: revision,
                }));
        });
        let outcome = done.wait().await.unwrap_or(PromptOutcome::Aborted);
        let text = input.text;
        self.update(|snapshot| {
            let revision = snapshot.revision + 1;
            let model = snapshot.model.clone();
            let (content, status, stop_reason) = match outcome {
                PromptOutcome::Complete => (
                    vec![AssistantContent::Text(TextContent {
                        text: format!("reply:{text}"),
                    })],
                    AssistantStatus::Complete,
                    AssistantStopReason::Stop,
                ),
                PromptOutcome::Aborted => (
                    vec![AssistantContent::Text(TextContent {
                        text: String::new(),
                    })],
                    AssistantStatus::Aborted,
                    AssistantStopReason::Aborted,
                ),
            };
            snapshot.phase = SessionPhase::Idle;
            snapshot
                .transcript
                .push(TranscriptItem::Assistant(AssistantTranscriptItem {
                    id: format!("assistant-{revision}"),
                    role: AssistantRole::Assistant,
                    content,
                    model,
                    response_model: None,
                    usage: None,
                    timestamp: revision,
                    status,
                    stop_reason: Some(stop_reason),
                    error_message: None,
                }));
        });
        *self.pending_prompt.lock() = None;
        Ok(())
    }

    async fn steer(&self, input: PromptInput) -> Result<(), PiServerError> {
        if self.phase() == SessionPhase::Idle {
            return Err(PiServerError::busy("There is no active prompt to steer"));
        }
        self.steers.lock().push(input.clone());
        self.update(|snapshot| {
            let revision = snapshot.revision + 1;
            snapshot.queued_steer_count += 1;
            snapshot.queued_steer.push(UserTranscriptItem {
                id: format!("steer-{revision}"),
                role: UserRole::User,
                content: vec![UserContent::Text(TextContent { text: input.text })],
                timestamp: revision,
            });
        });
        Ok(())
    }

    async fn abort(&self) -> Result<(), PiServerError> {
        let pending = self.pending_prompt.lock().clone();
        match pending {
            Some(pending) => {
                pending.resolve(PromptOutcome::Aborted);
                Ok(())
            }
            None => Err(PiServerError::busy("There is no active prompt to abort")),
        }
    }

    async fn set_model(&self, model: ModelRef) -> Result<(), PiServerError> {
        if self.phase() != SessionPhase::Idle {
            return Err(PiServerError::busy("Session is busy"));
        }
        self.update(|snapshot| snapshot.model = model);
        Ok(())
    }

    async fn set_thinking(&self, thinking_level: ThinkingLevel) -> Result<(), PiServerError> {
        if self.phase() != SessionPhase::Idle {
            return Err(PiServerError::busy("Session is busy"));
        }
        self.update(|snapshot| snapshot.thinking_level = thinking_level);
        Ok(())
    }

    fn subscribe(&self, listener: RuntimeEventListener) -> Unsubscribe {
        let id = {
            let mut next = self.next_listener_id.lock();
            *next += 1;
            *next
        };
        self.listeners.lock().push((id, listener));
        let listeners = Arc::downgrade(&self.listeners);
        Unsubscribe::new(move || {
            if let Some(listeners) = listeners.upgrade() {
                listeners.lock().retain(|(candidate, _)| *candidate != id);
            }
        })
    }

    async fn dispose(&self) -> Result<(), PiServerError> {
        *self.dispose_count.lock() += 1;
        (self.on_dispose)();
        self.disposed.resolve(());
        Ok(())
    }
}

struct ListDelay {
    entered: Arc<Deferred<()>>,
    release: Arc<Deferred<()>>,
}

/// Post-processes (or replaces) the metadata `list_sessions` would return.
pub type ListSessionsHook = Arc<
    dyn Fn(
            Vec<SessionMetadata>,
        )
            -> Pin<Box<dyn Future<Output = Result<Vec<SessionMetadata>, PiServerError>> + Send>>
        + Send
        + Sync,
>;

pub type ListModelsHook =
    Arc<dyn Fn() -> Pin<Box<dyn Future<Output = Result<(), PiServerError>> + Send>> + Send + Sync>;

#[derive(Default)]
pub struct TestServerService {
    self_weak: Mutex<std::sync::Weak<TestServerService>>,
    sessions: Arc<Mutex<HashMap<String, StoredSession>>>,
    order: Mutex<Vec<String>>,
    runtimes: Mutex<IndexMap<String, Vec<Arc<TestSessionRuntime>>>>,
    locked: Mutex<Vec<String>>,
    last_created_id: Mutex<Option<String>>,
    next_list_delay: Mutex<Option<ListDelay>>,
    list_sessions_hook: Mutex<Option<ListSessionsHook>>,
    list_models_hook: Mutex<Option<ListModelsHook>>,
    create_id_override: Mutex<Option<String>>,
}

impl TestServerService {
    pub fn new() -> Arc<Self> {
        let service = Arc::new(Self::default());
        *service.self_weak.lock() = Arc::downgrade(&service);
        service
    }

    fn arc(&self) -> Arc<Self> {
        self.self_weak
            .lock()
            .upgrade()
            .expect("TestServerService must be constructed with TestServerService::new")
    }

    pub fn last_created_id(&self) -> Option<String> {
        self.last_created_id.lock().clone()
    }

    pub fn is_locked(&self, id: &str) -> bool {
        self.locked.lock().iter().any(|entry| entry == id)
    }

    pub fn lock_session(&self, id: &str) {
        self.locked.lock().push(id.to_string());
    }

    pub fn runtime_count(&self, id: &str) -> usize {
        self.runtimes
            .lock()
            .get(id)
            .map(|runtimes| runtimes.len())
            .unwrap_or(0)
    }

    pub fn latest_runtime(&self, id: &str) -> Arc<TestSessionRuntime> {
        self.runtimes
            .lock()
            .get(id)
            .and_then(|runtimes| runtimes.last().cloned())
            .unwrap_or_else(|| panic!("No runtime for {id}"))
    }

    pub fn set_list_sessions_hook(&self, hook: ListSessionsHook) {
        *self.list_sessions_hook.lock() = Some(hook);
    }

    pub fn set_list_models_hook(&self, hook: ListModelsHook) {
        *self.list_models_hook.lock() = Some(hook);
    }

    pub fn set_create_id_override(&self, id: impl Into<String>) {
        *self.create_id_override.lock() = Some(id.into());
    }

    pub fn seed(&self, id: &str) {
        self.seed_with(
            id,
            Some(format!("Session {id}")),
            "/tmp/pi-server-conformance",
            ModelRef {
                provider: "test".to_string(),
                id: "small".to_string(),
            },
            ThinkingLevel::Off,
        );
    }

    pub fn seed_with(
        &self,
        id: &str,
        name: Option<String>,
        cwd: &str,
        model: ModelRef,
        thinking_level: ThinkingLevel,
    ) {
        let snapshot = SessionSnapshot {
            id: id.to_string(),
            name,
            cwd: cwd.to_string(),
            created_at: 1,
            updated_at: 1,
            phase: SessionPhase::Idle,
            model,
            thinking_level,
            attached: false,
            locked: false,
            revision: 0,
            transcript: Vec::new(),
            queued_steer: Vec::new(),
            queued_steer_count: 0,
        };
        let mut sessions = self.sessions.lock();
        if sessions
            .insert(id.to_string(), StoredSession { snapshot })
            .is_none()
        {
            self.order.lock().push(id.to_string());
        }
    }

    /// Blocks the next `list_sessions` until the returned release is resolved.
    pub fn delay_next_list(&self) -> (Arc<Deferred<()>>, Arc<Deferred<()>>) {
        let entered = Arc::new(Deferred::new());
        let release = Arc::new(Deferred::new());
        *self.next_list_delay.lock() = Some(ListDelay {
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
        });
        (entered, release)
    }

    fn acquire(self: &Arc<Self>, id: &str) -> Arc<TestSessionRuntime> {
        assert!(
            self.sessions.lock().contains_key(id),
            "Unknown session: {id}"
        );
        self.locked.lock().push(id.to_string());
        let service = Arc::clone(self);
        let session_id = id.to_string();
        let runtime = Arc::new(TestSessionRuntime {
            id: id.to_string(),
            stored: Arc::clone(&self.sessions),
            on_dispose: Box::new(move || {
                service.locked.lock().retain(|entry| entry != &session_id);
            }),
            listeners: Arc::new(Mutex::new(Vec::new())),
            next_listener_id: Mutex::new(0),
            pending_prompt: Mutex::new(None),
            disposed: Arc::new(Deferred::new()),
            dispose_count: Mutex::new(0),
            steers: Mutex::new(Vec::new()),
        });
        self.runtimes
            .lock()
            .entry(id.to_string())
            .or_default()
            .push(Arc::clone(&runtime));
        runtime
    }
}

#[async_trait]
impl SessionService for TestServerService {
    async fn list_sessions(&self) -> Result<Vec<SessionMetadata>, PiServerError> {
        let delay = self.next_list_delay.lock().take();
        if let Some(delay) = delay {
            delay.entered.resolve(());
            delay.release.wait().await;
        }
        let metadata: Vec<SessionMetadata> = {
            let sessions = self.sessions.lock();
            self.order
                .lock()
                .iter()
                .filter_map(|id| sessions.get(id))
                .map(|stored| SessionMetadata {
                    id: stored.snapshot.id.clone(),
                    created_at: stored.snapshot.created_at,
                    updated_at: Some(stored.snapshot.updated_at),
                    parent_session_id: None,
                    session_name: stored.snapshot.name.clone(),
                    cwd: Some(stored.snapshot.cwd.clone()),
                })
                .collect()
        };
        let hook = self.list_sessions_hook.lock().clone();
        match hook {
            Some(hook) => hook(metadata).await,
            None => Ok(metadata),
        }
    }

    async fn list_models(&self) -> Result<Vec<ModelMetadata>, PiServerError> {
        let hook = self.list_models_hook.lock().clone();
        if let Some(hook) = hook {
            hook().await?;
        }
        Ok(vec![test_model()])
    }

    async fn create_session(
        &self,
        options: CreateSessionOptions,
    ) -> Result<Arc<dyn SessionRuntime>, PiServerError> {
        let id = self
            .create_id_override
            .lock()
            .clone()
            .unwrap_or_else(|| options.id.clone());
        *self.last_created_id.lock() = Some(id.clone());
        if self.sessions.lock().contains_key(&id) {
            return Err(PiServerError::session_locked("Session already exists"));
        }
        self.seed_with(
            &id,
            options.name.or_else(|| Some(format!("Session {id}"))),
            options
                .cwd
                .as_deref()
                .unwrap_or("/tmp/pi-server-conformance"),
            options.model.unwrap_or(ModelRef {
                provider: "test".to_string(),
                id: "small".to_string(),
            }),
            options.thinking_level.unwrap_or(ThinkingLevel::Off),
        );
        Ok(self.arc().acquire(&id) as Arc<dyn SessionRuntime>)
    }

    async fn open_session(
        &self,
        session_id: String,
    ) -> Result<Arc<dyn SessionRuntime>, PiServerError> {
        if !self.sessions.lock().contains_key(&session_id) {
            return Err(PiServerError::not_found(format!(
                "Unknown session: {session_id}"
            )));
        }
        if self.is_locked(&session_id) {
            return Err(PiServerError::session_locked(format!(
                "Session is locked: {session_id}"
            )));
        }
        Ok(self.arc().acquire(&session_id) as Arc<dyn SessionRuntime>)
    }
}
