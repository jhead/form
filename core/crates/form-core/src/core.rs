//! The `Core` façade — everything the FFI layer needs, and nothing C-specific.
//!
//! `form-cli` and the Rust tests drive this directly; `form-ffi` is a thin JSON-in/JSON-out
//! wrapper over it. Keeping the C details out of here is what lets a future subprocess
//! transport reuse the same object.
//!
//! This file is the one place where the workstreams meet: it routes the frozen protocol
//! (spec 00) onto the store (W1), the harness (W2), the stats engine (W3), the catalog,
//! settings and context accounting (W4), and the markdown parser (W5).

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use tokio::runtime::Runtime;

use crate::app::TurnRecord;
use crate::app::{self, AddAttachment, AttachmentSource, SearchScope, Store, StoreOptions};
use crate::catalog;
use crate::context;
use crate::error::{CoreError, Result};
use crate::events::{EventBus, Listener};
use crate::harness::pi::PiHarness;
use crate::harness::{AbortSignal, Harness, RunContext, RunRequest};
use crate::markdown;
use crate::protocol::*;
use crate::settings::{Settings, SettingsStore};
use crate::stats;

/// A prompt sent while a run was streaming, held until the next turn boundary (F1.7).
/// Carries its attachments, so queueing does not quietly drop them.
#[derive(Debug, Clone)]
pub struct QueuedPrompt {
    pub text: String,
    pub attachment_ids: Vec<String>,
}

type PromptQueues = Arc<Mutex<HashMap<String, VecDeque<QueuedPrompt>>>>;

pub struct Core {
    config: CoreConfig,
    data_dir: PathBuf,
    store: Arc<Store>,
    settings_store: SettingsStore,
    settings: Mutex<Settings>,
    harness: Arc<dyn Harness>,
    /// Projected from pi's registry once at startup. Replaces form's hand-written table.
    catalog: crate::catalog::Catalog,
    bus: EventBus,
    runtime: Runtime,
    /// Live runs, keyed by session id, so `abortRun` can reach the right one.
    active: Arc<Mutex<HashMap<String, AbortSignal>>>,
    queued: PromptQueues,
    /// So a run finishing on a worker thread can start the next queued run.
    me: std::sync::Weak<Core>,
    /// A corrupt settings file is found during construction, before anyone can subscribe.
    /// Held here and emitted on first subscribe rather than dropped on the floor.
    pending_issue: Mutex<Option<EventKind>>,
}

impl Core {
    pub fn new(config: CoreConfig) -> Result<Arc<Self>> {
        let data_dir = PathBuf::from(&config.data_dir);
        let store = Arc::new(Store::open_with(
            &data_dir,
            StoreOptions {
                seed_mock_data: config.seed_mock_data,
                ..Default::default()
            },
        )?);

        let settings_store = SettingsStore::new(&data_dir);
        let outcome = settings_store.load_reporting();
        let mut harness_error: Option<String> = None;
        let pending_issue = outcome.issue.map(|issue| EventKind::Error {
            code: issue.code,
            message: issue.message,
            detail: issue.detail,
        });

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .thread_name("form-core")
            .build()
            .map_err(|e| CoreError::Internal(format!("tokio runtime: {e}")))?;

        // A GUI app launched from Finder inherits no shell environment, so the key can only
        // come from a file. Do this before the SDK is built; `pi-auth` reads the process env.
        let env_file = crate::env::load(&data_dir).or_else(|| {
            std::env::current_dir()
                .ok()
                .and_then(|cwd| crate::env::load(&cwd))
        });
        if let Some(path) = &env_file {
            tracing::info!(path = %path.display(), "loaded credentials from .env");
        }

        // The real harness needs the network to resolve its catalog, so it is built on the
        // runtime. A failure here is not fatal: the app still opens, and every run reports
        // the reason rather than the window refusing to appear.
        let (harness, catalog): (Arc<dyn Harness>, crate::catalog::Catalog) =
            if config.harness == crate::protocol::HarnessKind::Stub {
                (
                    Arc::new(crate::harness::StubHarness),
                    crate::catalog::builtin(),
                )
            } else {
                match runtime.block_on(PiHarness::new(system_prompt_for(&outcome.settings))) {
                    Ok(harness) => {
                        tracing::info!(models = harness.model_count(), "pi harness ready");
                        let catalog = harness.catalog();
                        (Arc::new(harness), catalog)
                    }
                    Err(reason) => {
                        // The window still opens and says why, rather than refusing to launch.
                        tracing::error!(%reason, "pi harness unavailable");
                        harness_error = Some(reason);
                        (Arc::new(UnavailableHarness), crate::catalog::builtin())
                    }
                }
            };

        // Loading the syntax set costs ~22 ms and is otherwise paid lazily by whichever
        // parse first sees a fenced code block — which, during a live run, is a dropped
        // frame in the middle of a streaming answer. Doing it on a blocking worker keeps it
        // off both the first frame and startup itself.
        runtime.spawn_blocking(markdown::warm);

        Ok(Arc::new_cyclic(|me| Self {
            me: me.clone(),
            config,
            data_dir,
            store,
            settings_store,
            settings: Mutex::new(outcome.settings),
            harness,
            catalog,
            bus: EventBus::new(),
            runtime,
            active: Arc::new(Mutex::new(HashMap::new())),
            queued: Arc::new(Mutex::new(HashMap::new())),
            pending_issue: Mutex::new(pending_issue.or(harness_error.map(|message| {
                EventKind::Error {
                    code: "harness_unavailable".to_string(),
                    message,
                    detail: None,
                }
            }))),
        }))
    }

    pub fn subscribe(&self, listener: Listener) -> i32 {
        let token = self.bus.subscribe(listener);
        // Deliver anything that happened before there was anyone to tell.
        if let Some(kind) = self.pending_issue.lock().unwrap().take() {
            self.bus.emit_kind(kind);
        }
        token
    }

    pub fn unsubscribe(&self, token: i32) {
        self.bus.unsubscribe(token);
    }

    fn settings_snapshot(&self) -> Settings {
        self.settings.lock().unwrap().clone()
    }

    /// Settings as the app should see them, with `hasKey` reflecting what the core can
    /// actually resolve.
    ///
    /// The stored flag only ever recorded what the Preferences pane wrote, so a key supplied
    /// through the environment or a `.env` showed the provider as unconfigured while requests
    /// to it succeeded. Deriving the flag at read time means the pane cannot disagree with
    /// the thing doing the work.
    fn settings_for_app(&self) -> Settings {
        let mut settings = self.settings_snapshot();
        let resolvable = crate::credentials::providers_with_keys();
        for (provider_id, provider) in settings.providers.iter_mut() {
            provider.has_key = resolvable.iter().any(|p| p == provider_id);
        }
        for provider_id in resolvable {
            settings
                .providers
                .entry(provider_id)
                .or_insert_with(|| crate::settings::ProviderSettings {
                    enabled: true,
                    base_url_override: None,
                    has_key: true,
                    ..Default::default()
                })
                .has_key = true;
        }
        settings
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

            // Global search skips archived sessions; a scoped find inside an open one does
            // not — see W1's note in `app::search`.
            Query::SearchSessions { q, limit } => Envelope::ok(self.store.search(
                &q,
                SearchScope::All,
                limit.unwrap_or(30),
            )?),
            Query::SearchInSession { session_id, q } => Envelope::ok(self.store.search(
                &q,
                SearchScope::Session(session_id),
                200,
            )?),

            Query::GetSettings => Envelope::ok(self.settings_for_app()),
            Query::GetCatalog => Envelope::ok(self.catalog.clone()),
            Query::GetStats { range, tz } => {
                Envelope::ok(stats::compute_at(&self.data_dir, range, &tz)?)
            }
            Query::GetContextUsage { session_id } => Envelope::ok(self.context_usage(&session_id)?),
            Query::RenderMarkdown { text, complete } => {
                Envelope::ok(markdown::parse_streaming(&text, complete.unwrap_or(true)))
            }
            Query::ResolvePath { session_id, path } => {
                let session = self.store.get_session(&session_id)?;
                let root = session.summary.workspace_root.as_ref().map(PathBuf::from);
                Envelope::ok(app::resolve_in_workspace(root.as_deref(), &path)?)
            }
            Query::GetAttachment { attachment_id } => {
                Envelope::ok(self.store.get_attachment(&attachment_id)?)
            }
            Query::ListRecentRoots => Envelope::ok(self.store.list_recent_roots()?),
        })
    }

    fn context_usage(&self, session_id: &str) -> Result<ContextUsage> {
        let session = self.store.get_session(session_id)?;
        Ok(context::context_usage(
            &session,
            self.model_of(&session.summary.model_ref),
        ))
    }

    /// Look a session's model up in the live catalog.
    fn model_of(&self, model_ref: &ModelRef) -> Option<&crate::catalog::Model> {
        self.catalog
            .providers
            .iter()
            .find(|p| p.id == model_ref.provider_id)?
            .models
            .iter()
            .find(|m| m.id == model_ref.model_id)
    }

    /// Recompute and publish the ring (F10). Cheap enough to call on every turn boundary,
    /// and doing it here means the view never has to estimate anything itself.
    fn emit_context_usage(&self, session_id: &str) {
        if let Ok(usage) = self.context_usage(session_id) {
            self.bus.emit_kind(EventKind::ContextUsageChanged { usage });
        }
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
        let cmd = Some(command_id);
        match command {
            // --- sessions ---
            Command::CreateSession {
                group_id,
                title,
                workspace_root,
                model_ref,
            } => {
                let model_ref = model_ref
                    .unwrap_or_else(|| self.settings_snapshot().defaults.model_ref.clone());
                let session = self.store.create_session(
                    group_id,
                    title,
                    workspace_root.clone(),
                    Some(model_ref),
                )?;
                if let Some(root) = workspace_root {
                    let _ = self.store.touch_recent_root(&root);
                }
                self.emit(EventKind::SessionCreated { session }, &cmd);
                Ok(())
            }

            Command::SendPrompt {
                session_id,
                text,
                attachment_ids,
            } => self.start_run(session_id, text, attachment_ids, cmd),

            Command::AbortRun { session_id } => {
                if let Some(signal) = self.active.lock().unwrap().get(&session_id) {
                    signal.abort();
                }
                // Anything the user queued behind an aborted run is no longer wanted.
                self.queued.lock().unwrap().remove(&session_id);
                Ok(())
            }

            Command::RenameSession { session_id, title } => {
                let session = self.store.rename_session(&session_id, &title)?;
                self.emit(EventKind::SessionUpdated { session }, &cmd);
                Ok(())
            }
            Command::DeleteSession { session_id } => {
                self.store.delete_session(&session_id)?;
                self.emit(EventKind::SessionDeleted { session_id }, &cmd);
                self.bus.emit_kind(EventKind::StatsInvalidated);
                Ok(())
            }
            Command::ArchiveSession {
                session_id,
                archived,
            } => {
                let session = self.store.set_archived(&session_id, archived)?;
                self.emit(EventKind::SessionUpdated { session }, &cmd);
                Ok(())
            }
            Command::PinSession { session_id, pinned } => {
                let session = self.store.set_pinned(&session_id, pinned)?;
                self.emit(EventKind::SessionUpdated { session }, &cmd);
                Ok(())
            }
            Command::MoveSession {
                session_id,
                group_id,
                index,
            } => {
                self.store
                    .move_session(&session_id, group_id.as_deref(), index)?;
                let session = self.store.get_summary(&session_id)?;
                self.emit(EventKind::SessionUpdated { session }, &cmd);
                self.emit_groups(&cmd);
                Ok(())
            }
            Command::SetSessionModel {
                session_id,
                model_ref,
            } => {
                let session = self.store.set_session_model(&session_id, &model_ref)?;
                self.emit(EventKind::SessionUpdated { session }, &cmd);
                // The window and output reserve changed with the model (F10.1).
                self.emit_context_usage(&session_id);
                Ok(())
            }
            Command::SetWorkspaceRoot { session_id, path } => {
                if let Some(root) = path.as_deref() {
                    let _ = self.store.touch_recent_root(root);
                }
                let session = self.store.set_workspace_root(&session_id, path)?;
                self.emit(EventKind::SessionUpdated { session }, &cmd);
                Ok(())
            }
            Command::BranchFromMessage {
                session_id,
                entry_id,
            } => {
                let session = self.store.branch_from_message(&session_id, &entry_id)?;
                self.emit(EventKind::SessionCreated { session }, &cmd);
                Ok(())
            }
            Command::RetryMessage {
                session_id,
                entry_id,
            } => {
                // Rewind to just before the message, then replay it as a fresh prompt.
                let prompt = self.user_text_of(&session_id, &entry_id)?;
                self.store.truncate_after(&session_id, &entry_id)?;
                self.store.truncate_after(&session_id, &entry_id)?;
                let session = self.store.get_summary(&session_id)?;
                self.emit(EventKind::SessionUpdated { session }, &cmd);
                self.start_run(session_id, prompt, Vec::new(), cmd)
            }

            // --- groups ---
            Command::CreateGroup { name } => {
                self.store.create_group(&name)?;
                self.emit_groups(&cmd);
                Ok(())
            }
            Command::RenameGroup { group_id, name } => {
                let groups = self.store.rename_group(&group_id, &name)?;
                self.emit(EventKind::GroupsChanged { groups }, &cmd);
                Ok(())
            }
            Command::DeleteGroup { group_id } => {
                let groups = self.store.delete_group(&group_id)?;
                // Its sessions were orphaned to Ungrouped rather than deleted, so their group
                // membership changed too. `groups_changed` is the app's cue to re-read the
                // session list; there is deliberately no second event for it.
                self.emit(EventKind::GroupsChanged { groups }, &cmd);
                Ok(())
            }
            Command::ReorderGroup { group_id, index } => {
                let groups = self.store.reorder_group(&group_id, index)?;
                self.emit(EventKind::GroupsChanged { groups }, &cmd);
                Ok(())
            }
            Command::SetGroupCollapsed {
                group_id,
                collapsed,
            } => {
                let groups = self.store.set_group_collapsed(&group_id, collapsed)?;
                self.emit(EventKind::GroupsChanged { groups }, &cmd);
                Ok(())
            }

            // --- attachments ---
            Command::AddAttachment {
                session_id,
                path,
                bytes_base64,
                filename,
                mime,
            } => {
                let source = match (path, bytes_base64) {
                    (Some(path), _) => AttachmentSource::Path(path),
                    (None, Some(b64)) => AttachmentSource::Bytes(
                        base64::engine::general_purpose::STANDARD
                            .decode(b64.as_bytes())
                            .map_err(|e| CoreError::InvalidRequest(format!("bytesBase64: {e}")))?,
                    ),
                    (None, None) => {
                        return Err(CoreError::InvalidRequest(
                            "addAttachment needs either path or bytesBase64".into(),
                        ))
                    }
                };
                let attachment = self.store.add_attachment(AddAttachment {
                    session_id: session_id.clone(),
                    source,
                    filename,
                    mime,
                })?;
                self.emit(EventKind::AttachmentAdded { attachment }, &cmd);
                if let Some(session_id) = session_id {
                    self.emit_context_usage(&session_id);
                }
                Ok(())
            }
            Command::SetAttachmentThumbnail {
                attachment_id,
                path,
            } => {
                self.store.set_thumb_path(&attachment_id, &path)?;
                let attachment = self.store.get_attachment(&attachment_id)?;
                self.emit(EventKind::AttachmentAdded { attachment }, &cmd);
                Ok(())
            }
            Command::RemoveAttachment { attachment_id } => {
                self.store.remove_attachment(&attachment_id)?;
                self.emit(EventKind::AttachmentRemoved { attachment_id }, &cmd);
                Ok(())
            }

            // --- settings ---
            Command::UpdateSettings { settings } => {
                let mut parsed: Settings = serde_json::from_value(settings)?;
                // The store's normalize, not the document's: it also restores fields the app
                // is not allowed to set, like the real data directory.
                self.settings_store.normalize(&mut parsed);
                self.settings_store.save(&parsed)?;
                let value = serde_json::to_value(&parsed)?;
                *self.settings.lock().unwrap() = parsed;
                self.emit(EventKind::SettingsChanged { settings: value }, &cmd);
                Ok(())
            }
        }
    }

    fn emit(&self, kind: EventKind, command_id: &Option<String>) {
        self.bus.emit_for(kind, command_id.clone());
    }

    fn emit_groups(&self, command_id: &Option<String>) {
        if let Ok(groups) = self.store.list_groups() {
            self.emit(EventKind::GroupsChanged { groups }, command_id);
        }
    }

    fn user_text_of(&self, session_id: &str, entry_id: &str) -> Result<String> {
        let session = self.store.get_session(session_id)?;
        session
            .entries
            .iter()
            .find(|e| e.id == entry_id)
            .and_then(|e| match &e.kind {
                EntryKind::Message {
                    message: Message::User(m),
                } => Some(m.content.to_text()),
                _ => None,
            })
            .ok_or_else(|| CoreError::InvalidRequest(format!("{entry_id} is not a user message")))
    }

    /// Build the user message, folding in any attachments so they are part of the transcript
    /// rather than a UI-only decoration (F3.5). Images become `ImageContent` blocks; anything
    /// else is named in text, which is all a model could do with it anyway.
    fn user_message(&self, prompt: &str, attachment_ids: &[String]) -> UserMessage {
        if attachment_ids.is_empty() {
            return UserMessage::text(prompt);
        }

        let mut blocks = Vec::with_capacity(attachment_ids.len() + 1);
        for id in attachment_ids {
            let Ok(attachment) = self.store.get_attachment(id) else {
                continue;
            };
            let is_image = attachment.mime.starts_with("image/");
            let bytes = is_image
                .then(|| std::fs::read(&attachment.path).ok())
                .flatten();

            match bytes {
                Some(bytes) => blocks.push(InputContent::Image(ImageContent {
                    data: base64::engine::general_purpose::STANDARD.encode(bytes),
                    mime_type: attachment.mime.clone(),
                })),
                // A file we cannot inline is still worth naming — silently dropping it would
                // leave the transcript disagreeing with what the user saw themselves send.
                None => blocks.push(InputContent::text(format!(
                    "[attached: {} ({}, {} bytes)]",
                    attachment.filename, attachment.mime, attachment.bytes
                ))),
            }
        }
        blocks.push(InputContent::text(prompt));

        UserMessage {
            content: UserContent::Blocks(blocks),
            timestamp: now_ms(),
        }
    }

    /// Spawn a run on the tokio runtime. Returns immediately; everything else is events.
    fn start_run(
        &self,
        session_id: String,
        prompt: String,
        attachment_ids: Vec<String>,
        command_id: Option<String>,
    ) -> Result<()> {
        // Sending during a run queues rather than failing (F1.7). The harness pulls the
        // prompt at its next turn boundary and appends the user entry there, so the
        // transcript order matches what actually reached the model.
        if self.active.lock().unwrap().contains_key(&session_id) {
            self.queued
                .lock()
                .unwrap()
                .entry(session_id)
                .or_default()
                .push_back(QueuedPrompt {
                    text: prompt,
                    attachment_ids,
                });
            return Ok(());
        }

        let session = self.store.get_session(&session_id)?;

        // The user's message joins the transcript before the run starts, so the UI can render
        // it immediately rather than waiting for the first assistant event.
        let user_entry = self.store.append_entry(
            &session_id,
            EntryKind::Message {
                message: Message::User(self.user_message(&prompt, &attachment_ids)),
            },
        )?;
        self.emit(
            EventKind::MessageStart {
                session_id: session_id.clone(),
                entry: user_entry.clone(),
            },
            &command_id,
        );
        self.emit(
            EventKind::MessageEnd {
                session_id: session_id.clone(),
                entry: user_entry,
            },
            &command_id,
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

        let settings = self.settings_snapshot();
        let ctx: Arc<dyn RunContext> = Arc::new(CoreRunContext {
            session_id: session_id.clone(),
            store: self.store.clone(),
            bus: self.bus.clone(),
            queued: self.queued.clone(),
            command_id: command_id.clone(),
            speed: self.config.harness_speed,
            system_prompt: settings.defaults.system_prompt.clone(),
        });

        let request = RunRequest {
            session_id: session_id.clone(),
            run_id: format!("run_{}", uuid::Uuid::new_v4().simple()),
            command_id: command_id.clone(),
            prompt,
            model: session.summary.model_ref.clone(),
            workspace_root: session.summary.workspace_root.clone(),
            // Seeding content from the session's real turn count is what stops a long
            // session from answering every question the same way.
            turn_index: self.store.count_turns(&session_id).unwrap_or(0) as u32,
        };

        let harness = self.harness.clone();
        let store = self.store.clone();
        let bus = self.bus.clone();
        let active = self.active.clone();
        let queued = self.queued.clone();
        let me = self.me.clone();

        if let Ok(s) = store.set_status(&session_id, SessionStatus::Streaming) {
            bus.emit_kind(EventKind::SessionUpdated { session: s });
        }

        self.runtime.spawn(async move {
            harness.run(request, ctx, signal).await;
            active.lock().unwrap().remove(&session_id);

            if let Ok(s) = store.set_status(&session_id, SessionStatus::Idle) {
                bus.emit_kind(EventKind::SessionUpdated { session: s });
            }
            if let Ok(session) = store.get_session(&session_id) {
                let model = catalog::resolve_ref(&session.summary.model_ref);
                bus.emit_kind(EventKind::ContextUsageChanged {
                    usage: context::context_usage(&session, model),
                });
            }
            bus.emit_kind(EventKind::StatsInvalidated);

            // A prompt queued between the run's last turn boundary and this point would
            // otherwise be stranded, because the harness has already stopped asking for one.
            // Starting a fresh run is the only correct recovery.
            let stranded = queued
                .lock()
                .ok()
                .and_then(|mut q| q.get_mut(&session_id).and_then(|q| q.pop_front()));
            if let (Some(queued), Some(core)) = (stranded, me.upgrade()) {
                let _ = core.start_run(session_id, queued.text, queued.attachment_ids, None);
            }
        });

        Ok(())
    }
}

/// The system prompt the agent runs with. The user's own text replaces the default rather
/// than appending to it, matching what the preferences pane says it does.
fn system_prompt_for(settings: &Settings) -> String {
    let configured = settings.defaults.system_prompt.trim();
    if configured.is_empty() {
        crate::context::BASE_SYSTEM_PROMPT.to_string()
    } else {
        configured.to_string()
    }
}

/// Stands in when the SDK could not start. It answers every run with the reason instead of
/// pretending to be an agent — the app must never show invented output.
struct UnavailableHarness;

#[async_trait::async_trait]
impl Harness for UnavailableHarness {
    async fn run(&self, req: RunRequest, ctx: Arc<dyn RunContext>, _abort: AbortSignal) {
        ctx.emit(EventKind::RunStart {
            session_id: req.session_id.clone(),
            run_id: req.run_id.clone(),
        });
        ctx.emit(EventKind::Error {
            code: "harness_unavailable".to_string(),
            message: "The agent backend did not start. Check the log and your API key.".to_string(),
            detail: None,
        });
        ctx.emit(EventKind::RunEnd {
            session_id: req.session_id,
            run_id: req.run_id,
            outcome: crate::protocol::RunOutcome::Failed,
            usage: Default::default(),
            duration_ms: 0,
        });
    }
}

struct CoreRunContext {
    session_id: String,
    store: Arc<Store>,
    bus: EventBus,
    queued: PromptQueues,
    command_id: Option<String>,
    speed: f64,
    system_prompt: String,
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

    fn take_queued_prompt(&self) -> Option<String> {
        // The harness only injects text; attachments on a queued prompt are carried by the
        // run that the stranded-prompt path starts, not by mid-run injection.
        self.queued
            .lock()
            .ok()?
            .get_mut(&self.session_id)
            .and_then(|q| q.pop_front())
            .map(|p| p.text)
    }

    fn prompt_overhead_tokens(&self) -> Option<u64> {
        let session = self.store.get_session(&self.session_id).ok()?;
        Some(
            context::system_prompt_tokens(&session, &self.system_prompt)
                + context::tool_schema_tokens(),
        )
    }

    fn record_turn(&self, turn: TurnRecord) {
        let tokens = turn.usage.total_tokens;
        let session_id = turn.session_id.clone();
        if self.store.record_turn(turn).is_ok() {
            if let Ok(session) = self.store.add_tokens(&session_id, tokens) {
                self.bus.emit_kind(EventKind::SessionUpdated { session });
            }
            // The ring moves as each turn lands, not only when the whole run ends (F10.4).
            if let Ok(session) = self.store.get_session(&session_id) {
                let model = catalog::resolve_ref(&session.summary.model_ref);
                self.bus.emit_kind(EventKind::ContextUsageChanged {
                    usage: context::context_usage(&session, model),
                });
            }
        }
    }
}
