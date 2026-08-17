//! Port of `.upstream/packages/server/src/sessions.ts`.
//!
//! One live runtime per session id, shared by every connection attached to it.
//! Acquisition, disposal and the "keep a busy session alive after its last
//! client disconnects" rule are all here.

use std::collections::HashMap;
use std::sync::{Arc, Weak};

use futures::future::Shared;
use futures::FutureExt;
use indexmap::IndexMap;
use parking_lot::Mutex;
use pi_protocol::{
    Command, CommandResult, DetachResult, EventEnvelope, ListResult, ServerEvent, SessionMetadata,
    SessionPhase, SessionProgressEvent, SessionResult, SessionSnapshot, SessionSnapshotEvent,
};

use crate::connection::ConnectionState;
use crate::errors::PiServerError;
use crate::server::ServerInner;
use crate::types::{
    CreateSessionOptions, PromptInput, SessionRuntime, SessionRuntimeEvent, SessionService,
    Unsubscribe,
};

type SharedUnit = Shared<futures::channel::oneshot::Receiver<()>>;
type SharedLive =
    Shared<futures::channel::oneshot::Receiver<Result<Arc<LiveSession>, PiServerError>>>;

pub(crate) struct LiveSession {
    pub(crate) id: String,
    pub(crate) runtime: Arc<dyn SessionRuntime>,
    state: Mutex<LiveSessionState>,
}

#[derive(Default)]
struct LiveSessionState {
    /// Keyed by connection id; the value keeps the connection alive while it is
    /// attached, matching upstream's `Set<ConnectionState>`.
    connections: IndexMap<String, Arc<ConnectionState>>,
    unsubscribe: Option<Unsubscribe>,
    operation_count: u64,
    ready: bool,
    terminal: bool,
    disposing: Option<SharedUnit>,
    dispose_signal: Option<futures::channel::oneshot::Sender<()>>,
}

fn to_metadata(snapshot: &SessionSnapshot) -> SessionMetadata {
    SessionMetadata {
        id: snapshot.id.clone(),
        created_at: snapshot.created_at,
        updated_at: Some(snapshot.updated_at),
        parent_session_id: None,
        session_name: snapshot.name.clone(),
        cwd: Some(snapshot.cwd.clone()),
    }
}

/// Which service call produces the runtime for an acquisition.
enum Acquisition {
    Create(Box<CreateSessionOptions>),
    Open(String),
}

pub(crate) struct LiveSessionManager {
    server: Mutex<Weak<ServerInner>>,
    service: Arc<dyn SessionService>,
    live: Mutex<IndexMap<String, Arc<LiveSession>>>,
    opening: Mutex<HashMap<String, SharedLive>>,
}

impl LiveSessionManager {
    pub(crate) fn new(service: Arc<dyn SessionService>) -> Self {
        Self {
            server: Mutex::new(Weak::new()),
            service,
            live: Mutex::new(IndexMap::new()),
            opening: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) fn attach_server(&self, server: Weak<ServerInner>) {
        *self.server.lock() = server;
    }

    fn server(&self) -> Option<Arc<ServerInner>> {
        self.server.lock().upgrade()
    }

    fn is_closing(&self) -> bool {
        self.server().is_some_and(|server| server.is_closing())
    }

    fn report_error(&self, error: PiServerError) {
        if let Some(server) = self.server() {
            server.report_service_error(error);
        }
    }

    // -- commands ----------------------------------------------------------

    pub(crate) async fn execute_command(
        &self,
        connection: &Arc<ConnectionState>,
        command: Command,
    ) -> Result<CommandResult, PiServerError> {
        match command {
            Command::List(_) => Ok(CommandResult::List(ListResult {
                sessions: self.list_metadata().await?,
            })),
            Command::Create(create) => {
                let id = uuid::Uuid::new_v4().to_string();
                let options = CreateSessionOptions {
                    id: id.clone(),
                    cwd: create.cwd,
                    name: create.name,
                    model: create.model,
                    thinking_level: create.thinking_level,
                };
                let live = self
                    .acquire(&id, Acquisition::Create(Box::new(options)))
                    .await?;
                self.attach(connection, &live).await?;
                let session = for_connection(self.broadcast_snapshot(&live).await?, connection);
                self.broadcast_server_snapshot();
                Ok(CommandResult::Create(SessionResult { session }))
            }
            Command::Attach(attach) => {
                let live = self
                    .acquire(
                        &attach.session_id,
                        Acquisition::Open(attach.session_id.clone()),
                    )
                    .await?;
                self.attach(connection, &live).await?;
                let session = for_connection(self.broadcast_snapshot(&live).await?, connection);
                self.broadcast_server_snapshot();
                Ok(CommandResult::Attach(SessionResult { session }))
            }
            Command::Detach(detach) => {
                let session_id = detach.session_id;
                let live = self.live.lock().get(&session_id).cloned();
                let was_attached = {
                    let mut inner = connection.inner.lock();
                    inner.session_ids.remove(&session_id)
                };
                if was_attached {
                    if let Some(live) = live {
                        let (remaining, quiescent) = {
                            let mut state = live.state.lock();
                            state.connections.shift_remove(&connection.id);
                            (
                                state.connections.len(),
                                !state.terminal && state.disposing.is_none(),
                            )
                        };
                        if remaining > 0 && quiescent {
                            self.broadcast_snapshot(&live).await?;
                        }
                        self.maybe_dispose(&live).await?;
                    }
                    self.broadcast_server_snapshot();
                }
                Ok(CommandResult::Detach(DetachResult { session_id }))
            }
            Command::Prompt(prompt) => {
                let live = self.require_attached(connection, &prompt.session_id)?;
                let text = prompt.text;
                let session = self
                    .run_operation(connection, &live, |runtime| {
                        Box::pin(async move { runtime.prompt(PromptInput { text }).await })
                    })
                    .await?;
                Ok(CommandResult::Prompt(SessionResult { session }))
            }
            Command::Steer(steer) => {
                let live = self.require_attached(connection, &steer.session_id)?;
                let text = steer.text;
                let session = self
                    .run_operation(connection, &live, |runtime| {
                        Box::pin(async move { runtime.steer(PromptInput { text }).await })
                    })
                    .await?;
                Ok(CommandResult::Steer(SessionResult { session }))
            }
            Command::Abort(abort) => {
                let live = self.require_attached(connection, &abort.session_id)?;
                let session = self
                    .run_operation(connection, &live, |runtime| {
                        Box::pin(async move { runtime.abort().await })
                    })
                    .await?;
                Ok(CommandResult::Abort(SessionResult { session }))
            }
            Command::SetModel(set_model) => {
                let live = self.require_attached(connection, &set_model.session_id)?;
                let model = set_model.model;
                let session = self
                    .run_operation(connection, &live, |runtime| {
                        Box::pin(async move { runtime.set_model(model).await })
                    })
                    .await?;
                Ok(CommandResult::SetModel(SessionResult { session }))
            }
            Command::SetThinking(set_thinking) => {
                let live = self.require_attached(connection, &set_thinking.session_id)?;
                let level = set_thinking.thinking_level;
                let session = self
                    .run_operation(connection, &live, |runtime| {
                        Box::pin(async move { runtime.set_thinking(level).await })
                    })
                    .await?;
                Ok(CommandResult::SetThinking(SessionResult { session }))
            }
        }
    }

    pub(crate) async fn disconnect(&self, connection: &Arc<ConnectionState>) {
        let session_ids: Vec<String> = {
            let mut inner = connection.inner.lock();
            inner.session_ids.drain().collect()
        };
        let sessions: Vec<Arc<LiveSession>> = {
            let live = self.live.lock();
            session_ids
                .iter()
                .filter_map(|id| live.get(id).cloned())
                .collect()
        };
        for live in &sessions {
            live.state.lock().connections.shift_remove(&connection.id);
        }
        for live in &sessions {
            if let Err(error) = self.maybe_dispose(live).await {
                self.report_error(error);
            }
        }
    }

    pub(crate) async fn list_metadata(&self) -> Result<Vec<SessionMetadata>, PiServerError> {
        let stored = self.service.list_sessions().await?;
        let lives: Vec<Arc<LiveSession>> = self
            .live
            .lock()
            .values()
            .filter(|live| live.state.lock().disposing.is_none())
            .cloned()
            .collect();
        let mut live_by_id: IndexMap<String, SessionSnapshot> = IndexMap::new();
        for live in lives {
            let snapshot = self.normalized_snapshot(&live).await?;
            live_by_id.insert(live.id.clone(), snapshot);
        }
        let mut metadata = Vec::with_capacity(stored.len());
        for item in stored {
            match live_by_id.shift_remove(&item.id) {
                // `{ ...item, ...toMetadata(snapshot) }`: the live snapshot wins
                // on every field it carries, including when it carries `None`.
                Some(snapshot) => metadata.push(SessionMetadata {
                    parent_session_id: item.parent_session_id,
                    ..to_metadata(&snapshot)
                }),
                None => metadata.push(item),
            }
        }
        for snapshot in live_by_id.values() {
            metadata.push(to_metadata(snapshot));
        }
        Ok(metadata)
    }

    pub(crate) async fn close(&self) {
        let openings: Vec<SharedLive> = self.opening.lock().values().cloned().collect();
        for opening in openings {
            if let Ok(Err(error)) = opening.await {
                self.report_error(error);
            }
        }
        let sessions: Vec<Arc<LiveSession>> = {
            let mut live = self.live.lock();
            live.drain(..).map(|(_, value)| value).collect()
        };
        for live in sessions {
            let disposing = live.state.lock().disposing.clone();
            if let Some(disposing) = disposing {
                let _ = disposing.await;
                continue;
            }
            let unsubscribe = live.state.lock().unsubscribe.take();
            if let Some(unsubscribe) = unsubscribe {
                unsubscribe.unsubscribe();
            }
            if let Err(error) = live.runtime.dispose().await {
                self.report_error(error);
            }
        }
    }

    // -- internals ---------------------------------------------------------

    async fn run_operation<F>(
        &self,
        connection: &Arc<ConnectionState>,
        live: &Arc<LiveSession>,
        operation: F,
    ) -> Result<SessionSnapshot, PiServerError>
    where
        F: FnOnce(
            Arc<dyn SessionRuntime>,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<(), PiServerError>> + Send>,
        >,
    {
        live.state.lock().operation_count += 1;
        let outcome = async {
            operation(Arc::clone(&live.runtime)).await?;
            Ok(for_connection(
                self.broadcast_snapshot(live).await?,
                connection,
            ))
        }
        .await;
        live.state.lock().operation_count -= 1;
        self.schedule_maybe_dispose(live);
        outcome
    }

    async fn acquire(
        &self,
        id: &str,
        acquisition: Acquisition,
    ) -> Result<Arc<LiveSession>, PiServerError> {
        let mut acquisition = Some(acquisition);
        loop {
            let existing = self.live.lock().get(id).cloned();
            if let Some(existing) = existing {
                let (terminal, disposing) = {
                    let state = existing.state.lock();
                    (state.terminal, state.disposing.clone())
                };
                if terminal {
                    return Err(PiServerError::session_locked(format!(
                        "Session runtime is terminating: {id}"
                    )));
                }
                if let Some(disposing) = disposing {
                    let _ = disposing.await;
                    continue;
                }
                return Ok(existing);
            }

            let (sender, shared) = {
                let mut opening = self.opening.lock();
                match opening.get(id) {
                    Some(existing) => (None, existing.clone()),
                    None => {
                        let (sender, receiver) = futures::channel::oneshot::channel();
                        let shared = receiver.shared();
                        opening.insert(id.to_string(), shared.clone());
                        (Some(sender), shared)
                    }
                }
            };
            if let Some(sender) = sender {
                let acquisition = acquisition
                    .take()
                    .expect("the slot owner runs the acquisition exactly once");
                let result = self.create(id, acquisition).await;
                self.opening.lock().remove(id);
                let _ = sender.send(result);
            }
            return shared
                .await
                .unwrap_or_else(|_| Err(PiServerError::internal("session acquisition cancelled")));
        }
    }

    async fn create(
        &self,
        id: &str,
        acquisition: Acquisition,
    ) -> Result<Arc<LiveSession>, PiServerError> {
        let runtime = match acquisition {
            Acquisition::Create(options) => self.service.create_session(*options).await?,
            Acquisition::Open(session_id) => self.service.open_session(session_id).await?,
        };
        if self.is_closing() {
            let _ = runtime.dispose().await;
            return Err(PiServerError::internal(
                "PiServer closed while acquiring a session runtime",
            ));
        }

        let failure = match runtime.snapshot().await {
            Err(error) => Some(error),
            Ok(snapshot) if snapshot.id != id => Some(PiServerError::invalid_request(format!(
                "Service returned session {} for server-assigned session {id}",
                snapshot.id
            ))),
            Ok(_) => None,
        };
        if let Some(error) = failure {
            if let Err(dispose_error) = runtime.dispose().await {
                self.report_error(dispose_error);
            }
            return Err(error);
        }

        let live = Arc::new(LiveSession {
            id: id.to_string(),
            runtime: Arc::clone(&runtime),
            state: Mutex::new(LiveSessionState::default()),
        });
        let weak_live = Arc::downgrade(&live);
        let server = self.server.lock().clone();
        let unsubscribe = runtime.subscribe(Arc::new(move |event| {
            let (Some(live), Some(server)) = (weak_live.upgrade(), server.upgrade()) else {
                return;
            };
            server.spawn_runtime_event(live, event);
        }));
        live.state.lock().unsubscribe = Some(unsubscribe);
        self.live.lock().insert(id.to_string(), Arc::clone(&live));
        live.state.lock().ready = true;
        Ok(live)
    }

    pub(crate) async fn handle_runtime_event(
        &self,
        live: Arc<LiveSession>,
        event: SessionRuntimeEvent,
    ) {
        match event {
            SessionRuntimeEvent::Error(error) => {
                if let Err(failure) = self.terminate(&live, error).await {
                    self.report_error(failure);
                }
                return;
            }
            SessionRuntimeEvent::Progress(progress) => {
                let envelope = EventEnvelope {
                    event: ServerEvent::SessionProgress(SessionProgressEvent {
                        session_id: live.id.clone(),
                        progress,
                    }),
                };
                let connections: Vec<Arc<ConnectionState>> =
                    live.state.lock().connections.values().cloned().collect();
                if let Some(server) = self.server() {
                    for connection in connections {
                        server.fire_event(&connection, envelope.clone());
                    }
                }
            }
            SessionRuntimeEvent::Snapshot => {
                if let Err(error) = self.broadcast_snapshot(&live).await {
                    self.report_error(error);
                }
            }
        }
        self.schedule_maybe_dispose(&live);
    }

    async fn terminate(
        &self,
        live: &Arc<LiveSession>,
        error: PiServerError,
    ) -> Result<(), PiServerError> {
        {
            let mut state = live.state.lock();
            if state.terminal {
                return Ok(());
            }
            state.terminal = true;
        }
        self.report_error(error);
        let unsubscribe = live.state.lock().unsubscribe.take();
        if let Some(unsubscribe) = unsubscribe {
            unsubscribe.unsubscribe();
        }
        let connections: Vec<Arc<ConnectionState>> =
            live.state.lock().connections.values().cloned().collect();
        if let Some(server) = self.server() {
            for connection in &connections {
                server.close_connection(&connection.connection, None).await;
            }
            for connection in &connections {
                server.disconnect(connection).await;
            }
        }
        self.maybe_dispose(live).await
    }

    async fn normalized_snapshot(
        &self,
        live: &Arc<LiveSession>,
    ) -> Result<SessionSnapshot, PiServerError> {
        let snapshot = live.runtime.snapshot().await?;
        if snapshot.id != live.id {
            return Err(PiServerError::invalid_request(format!(
                "Runtime session ID changed from {} to {}",
                live.id, snapshot.id
            )));
        }
        let attached = !live.state.lock().connections.is_empty();
        Ok(SessionSnapshot {
            phase: live.runtime.phase(),
            attached,
            locked: true,
            ..snapshot
        })
    }

    async fn broadcast_snapshot(
        &self,
        live: &Arc<LiveSession>,
    ) -> Result<SessionSnapshot, PiServerError> {
        let snapshot = self.normalized_snapshot(live).await?;
        let envelope = EventEnvelope {
            event: ServerEvent::SessionSnapshot(SessionSnapshotEvent {
                snapshot: snapshot.clone(),
            }),
        };
        let connections: Vec<Arc<ConnectionState>> =
            live.state.lock().connections.values().cloned().collect();
        if let Some(server) = self.server() {
            for connection in connections {
                server.fire_event(&connection, envelope.clone());
            }
        }
        Ok(snapshot)
    }

    async fn attach(
        &self,
        connection: &Arc<ConnectionState>,
        live: &Arc<LiveSession>,
    ) -> Result<(), PiServerError> {
        let usable = {
            let inner = connection.inner.lock();
            !inner.disconnected
                && inner.stage == crate::connection::ConnectionStage::Ready
                && !connection.connection.closed()
        };
        if !usable {
            self.maybe_dispose(live).await?;
            return Err(PiServerError::invalid_request(
                "Connection closed while attaching to a session",
            ));
        }
        connection.inner.lock().session_ids.insert(live.id.clone());
        live.state
            .lock()
            .connections
            .insert(connection.id.clone(), Arc::clone(connection));
        Ok(())
    }

    fn require_attached(
        &self,
        connection: &Arc<ConnectionState>,
        session_id: &str,
    ) -> Result<Arc<LiveSession>, PiServerError> {
        if !connection.is_attached(session_id) {
            return Err(PiServerError::invalid_request(format!(
                "Connection is not attached to session {session_id}"
            )));
        }
        let live = self.live.lock().get(session_id).cloned();
        match live {
            Some(live) => {
                let state = live.state.lock();
                if state.terminal || state.disposing.is_some() {
                    Err(PiServerError::not_found(format!(
                        "Session is not live: {session_id}"
                    )))
                } else {
                    drop(state);
                    Ok(live)
                }
            }
            None => Err(PiServerError::not_found(format!(
                "Session is not live: {session_id}"
            ))),
        }
    }

    fn schedule_maybe_dispose(&self, live: &Arc<LiveSession>) {
        if let Some(server) = self.server() {
            server.spawn_maybe_dispose(Arc::clone(live));
        }
    }

    pub(crate) async fn maybe_dispose(&self, live: &Arc<LiveSession>) -> Result<(), PiServerError> {
        let (start, waiting) = {
            let mut state = live.state.lock();
            let blocked = self.is_closing()
                || !state.ready
                || state.disposing.is_some()
                || !state.connections.is_empty()
                || state.operation_count > 0
                || (!state.terminal && live.runtime.phase() != SessionPhase::Idle);
            if blocked {
                (false, state.disposing.clone())
            } else {
                let (sender, receiver) = futures::channel::oneshot::channel();
                let shared = receiver.shared();
                state.disposing = Some(shared.clone());
                state.dispose_signal = Some(sender);
                (true, Some(shared))
            }
        };
        if !start {
            if let Some(waiting) = waiting {
                let _ = waiting.await;
            }
            return Ok(());
        }

        let unsubscribe = live.state.lock().unsubscribe.take();
        if let Some(unsubscribe) = unsubscribe {
            unsubscribe.unsubscribe();
        }
        let result = live.runtime.dispose().await;
        {
            let mut map = self.live.lock();
            if map
                .get(&live.id)
                .is_some_and(|current| Arc::ptr_eq(current, live))
            {
                map.shift_remove(&live.id);
            }
        }
        if let Some(signal) = live.state.lock().dispose_signal.take() {
            let _ = signal.send(());
        }
        if !self.is_closing() {
            self.broadcast_server_snapshot();
        }
        result
    }

    fn broadcast_server_snapshot(&self) {
        if let Some(server) = self.server() {
            server.broadcast_server_snapshot();
        }
    }
}

fn for_connection(
    mut snapshot: SessionSnapshot,
    connection: &Arc<ConnectionState>,
) -> SessionSnapshot {
    snapshot.attached = connection.is_attached(&snapshot.id);
    snapshot
}
