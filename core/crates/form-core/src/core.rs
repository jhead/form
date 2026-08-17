//! The `Core` façade — everything the FFI layer needs, and nothing C-specific.
//!
//! `form-cli` and the Rust tests drive this directly; `form-ffi` is a thin JSON-in/JSON-out
//! wrapper over it. Keeping the C details out of here is what lets a future subprocess
//! transport reuse the same object.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tokio::runtime::Runtime;

use crate::app::{self, Store};
use crate::catalog;
use crate::context;
use crate::error::{CoreError, Result};
use crate::events::{EventBus, Listener};
use crate::harness::{AbortSignal, Harness, RunContext, RunRequest, StubHarness};
use crate::markdown;
use crate::protocol::*;
use crate::settings::{Settings, SettingsStore};
use crate::stats::UsageStats;

pub struct Core {
    config: CoreConfig,
    store: Arc<Store>,
    settings_store: SettingsStore,
    settings: Mutex<Settings>,
    harness: Arc<dyn Harness>,
    bus: EventBus,
    runtime: Runtime,
    /// Live runs, keyed by session id, so `abortRun` can reach the right one.
    active: Arc<Mutex<HashMap<String, AbortSignal>>>,
}

impl Core {
    pub fn new(config: CoreConfig) -> Result<Arc<Self>> {
        let data_dir = PathBuf::from(&config.data_dir);
        let store = Arc::new(Store::open(&data_dir)?);
        let settings_store = SettingsStore::new(&data_dir);
        let settings = settings_store.load();

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .thread_name("form-core")
            .build()
            .map_err(|e| CoreError::Internal(format!("tokio runtime: {e}")))?;

        let core = Arc::new(Self {
            config,
            store,
            settings_store,
            settings: Mutex::new(settings),
            harness: Arc::new(StubHarness),
            bus: EventBus::new(),
            runtime,
            active: Arc::new(Mutex::new(HashMap::new())),
        });

        // TODO(W1): seed the mock corpus here when `seed_mock_data` is set and the store is
        // empty — spec 01 §6. The dashboard's first-launch content depends on it.
        Ok(core)
    }

    pub fn subscribe(&self, listener: Listener) -> i32 {
        self.bus.subscribe(listener)
    }

    pub fn unsubscribe(&self, token: i32) {
        self.bus.unsubscribe(token);
    }

    // ------------------------------------------------------------ queries

    pub fn query_json(&self, json: &str) -> String {
        let query: Query = match serde_json::from_str(json) {
            Ok(q) => q,
            Err(e) => {
                return Envelope::from_error(&CoreError::InvalidRequest(e.to_string())).to_json()
            }
        };
        match self.query(query) {
            Ok(envelope) => envelope.to_json(),
            Err(e) => Envelope::from_error(&e).to_json(),
        }
    }

    fn query(&self, query: Query) -> Result<Envelope> {
        Ok(match query {
            Query::ListSessions { include_archived } => {
                Envelope::ok(self.store.list_sessions(include_archived)?)
            }
            Query::GetSession { session_id } => Envelope::ok(self.store.get_session(&session_id)?),
            Query::GetSettings => Envelope::ok(self.settings.lock().unwrap().clone()),
            Query::GetCatalog => Envelope::ok(catalog::builtin()),
            Query::GetStats { range, .. } => Envelope::ok(UsageStats::empty(range)),
            Query::GetContextUsage { session_id } => {
                let session = self.store.get_session(&session_id)?;
                let model = catalog::resolve(&session.summary.model_ref);
                Envelope::ok(context::context_usage(&session, model.as_ref()))
            }
            Query::RenderMarkdown { text, complete } => {
                Envelope::ok(markdown::parse_streaming(&text, complete.unwrap_or(true)))
            }
            Query::ResolvePath { session_id, path } => {
                let session = self.store.get_session(&session_id)?;
                let root = session.summary.workspace_root.as_ref().map(PathBuf::from);
                Envelope::ok(app::resolve_in_workspace(root.as_deref(), &path)?)
            }
            // TODO: SearchSessions/SearchInSession → W1; GetAttachment/ListRecentRoots → W1.
            other => {
                return Err(CoreError::NotImplemented(query_name(&other)));
            }
        })
    }

    // ------------------------------------------------------------ commands

    pub fn dispatch_json(&self, json: &str) -> String {
        let command: Command = match serde_json::from_str(json) {
            Ok(c) => c,
            Err(e) => {
                return Envelope::from_error(&CoreError::InvalidRequest(e.to_string())).to_json()
            }
        };
        let command_id = format!("cmd_{}", uuid::Uuid::new_v4().simple());
        match self.dispatch(command, command_id.clone()) {
            Ok(()) => Envelope::ok(CommandAck { command_id }).to_json(),
            Err(e) => Envelope::from_error(&e).to_json(),
        }
    }

    fn dispatch(&self, command: Command, command_id: String) -> Result<()> {
        match command {
            Command::CreateSession {
                group_id,
                title,
                workspace_root,
                model_ref,
            } => {
                let session =
                    self.store
                        .create_session(group_id, title, workspace_root, model_ref)?;
                self.bus
                    .emit_for(EventKind::SessionCreated { session }, Some(command_id));
                Ok(())
            }

            Command::SendPrompt {
                session_id, text, ..
            } => self.start_run(session_id, text, command_id),

            Command::AbortRun { session_id } => {
                if let Some(signal) = self.active.lock().unwrap().get(&session_id) {
                    signal.abort();
                }
                Ok(())
            }

            Command::RenameSession { .. }
            | Command::DeleteSession { .. }
            | Command::ArchiveSession { .. }
            | Command::MoveSession { .. } => {
                Err(CoreError::NotImplemented("session mutation (W1)"))
            }

            Command::UpdateSettings { settings } => {
                let mut parsed: Settings = serde_json::from_value(settings)?;
                parsed.normalize();
                self.settings_store.save(&parsed)?;
                let value = serde_json::to_value(&parsed)?;
                *self.settings.lock().unwrap() = parsed;
                self.bus.emit_for(
                    EventKind::SettingsChanged { settings: value },
                    Some(command_id),
                );
                Ok(())
            }

            _ => Err(CoreError::NotImplemented("command")),
        }
    }

    /// Spawn a run on the tokio runtime. Returns immediately; everything else is events.
    fn start_run(&self, session_id: String, prompt: String, command_id: String) -> Result<()> {
        {
            let active = self.active.lock().unwrap();
            if active.contains_key(&session_id) {
                // TODO(W2): queue instead of rejecting, per F1.7.
                return Err(CoreError::RunAlreadyActive(session_id));
            }
        }

        let session = self.store.get_session(&session_id)?;

        // The user's message is part of the transcript before the run starts, so the UI can
        // render it immediately rather than waiting for the first assistant event.
        let user_entry = self.store.append_entry(
            &session_id,
            EntryKind::Message {
                message: Message::User(UserMessage::text(prompt.clone())),
            },
        )?;
        self.bus.emit_for(
            EventKind::MessageStart {
                session_id: session_id.clone(),
                entry: user_entry.clone(),
            },
            Some(command_id.clone()),
        );
        self.bus.emit_for(
            EventKind::MessageEnd {
                session_id: session_id.clone(),
                entry: user_entry,
            },
            Some(command_id.clone()),
        );

        if let Some(updated) = self.store.maybe_derive_title(&session_id, &prompt)? {
            self.bus
                .emit_kind(EventKind::SessionUpdated { session: updated });
        }

        let signal = AbortSignal::new();
        self.active
            .lock()
            .unwrap()
            .insert(session_id.clone(), signal.clone());

        let ctx: Arc<dyn RunContext> = Arc::new(CoreRunContext {
            store: self.store.clone(),
            bus: self.bus.clone(),
            command_id: Some(command_id),
            speed: self.config.harness_speed,
        });

        let request = RunRequest {
            session_id: session_id.clone(),
            run_id: format!("run_{}", uuid::Uuid::new_v4().simple()),
            command_id: None,
            prompt,
            model: session.summary.model_ref.clone(),
            workspace_root: session.summary.workspace_root.clone(),
            turn_index: 0,
        };

        let harness = self.harness.clone();
        let store = self.store.clone();
        let bus = self.bus.clone();
        let active = self.active.clone();

        if let Ok(s) = store.set_status(&session_id, SessionStatus::Streaming) {
            bus.emit_kind(EventKind::SessionUpdated { session: s });
        }

        self.runtime.spawn(async move {
            harness.run(request, ctx, signal).await;
            active.lock().unwrap().remove(&session_id);
            if let Ok(s) = store.set_status(&session_id, SessionStatus::Idle) {
                bus.emit_kind(EventKind::SessionUpdated { session: s });
            }
            bus.emit_kind(EventKind::StatsInvalidated);
        });

        Ok(())
    }
}

struct CoreRunContext {
    store: Arc<Store>,
    bus: EventBus,
    command_id: Option<String>,
    speed: f64,
}

impl RunContext for CoreRunContext {
    fn emit(&self, kind: EventKind) {
        self.bus.emit_for(kind, self.command_id.clone());
    }

    fn append_entry(&self, session_id: &str, kind: EntryKind) -> Option<Entry> {
        self.store.append_entry(session_id, kind).ok()
    }

    fn replace_entry(&self, entry: &Entry) {
        let _ = self.store.replace_entry(entry);
    }

    fn speed(&self) -> f64 {
        self.speed
    }
}

fn query_name(q: &Query) -> &'static str {
    match q {
        Query::ListSessions { .. } => "listSessions",
        Query::GetSession { .. } => "getSession",
        Query::SearchSessions { .. } => "searchSessions",
        Query::SearchInSession { .. } => "searchInSession",
        Query::GetSettings => "getSettings",
        Query::GetCatalog => "getCatalog",
        Query::GetStats { .. } => "getStats",
        Query::GetContextUsage { .. } => "getContextUsage",
        Query::RenderMarkdown { .. } => "renderMarkdown",
        Query::ResolvePath { .. } => "resolvePath",
        Query::GetAttachment { .. } => "getAttachment",
        Query::ListRecentRoots => "listRecentRoots",
    }
}
