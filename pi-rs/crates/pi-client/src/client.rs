//! Port of `.upstream/packages/client/src/client.ts`.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Weak};

use futures::future::Shared;
use futures::FutureExt;
use parking_lot::Mutex;
use pi_protocol::{
    encode_client_message, Command, CommandResult, CreateCommand, FrameDecoderOptions, ListCommand,
    RequestEnvelope, ServerEvent, ServerMessage, ServerSnapshot, SessionCommand, SessionMetadata,
};
use tokio::sync::oneshot;

use crate::connection::{Connection, ConnectionHost};
use crate::errors::PiClientError;
use crate::session_handle::{AcquireSessionOptions, LeaseToken, PiSessionHandle, SessionLeaseMode};
use crate::state::ClientState;
use crate::transport::ByteTransportFactory;
use crate::types::{
    invoke_listener, ConnectionState, ConnectionStateChange, CreateSessionOptions, Listener,
    ListenerSet, PiClientOptions, Unsubscribe,
};

/// A completion other callers can await. `futures`' oneshot receiver is used
/// because its output is `Clone`, which `Shared` requires.
type SharedCompletion<T> = Shared<futures::channel::oneshot::Receiver<T>>;
type SharedResult = SharedCompletion<Result<(), PiClientError>>;

struct PendingRequest {
    command: Command,
    responder: oneshot::Sender<Result<CommandResult, PiClientError>>,
}

#[derive(Default)]
struct LeaseRegistry {
    counts: HashMap<String, u64>,
    exclusive: HashMap<String, Arc<LeaseToken>>,
    generations: HashMap<String, u64>,
    cleanup_required: HashSet<String>,
}

pub(crate) struct ClientInner {
    self_weak: Weak<ClientInner>,
    options: PiClientOptions,
    connection: Connection,
    state: ClientState,
    pending: Mutex<HashMap<String, PendingRequest>>,
    leases: Mutex<LeaseRegistry>,
    attachments: Mutex<HashMap<String, SharedResult>>,
    detachments: Mutex<HashMap<String, SharedResult>>,
    reconciliations: Mutex<HashMap<String, SharedResult>>,
    connection_state_listeners: Arc<ListenerSet<ConnectionStateChange>>,
    request_sequence: AtomicU64,
    disposed: AtomicBool,
}

impl ConnectionHost for ClientInner {
    fn connection(&self) -> &Connection {
        &self.connection
    }

    fn transport_factory(&self) -> Arc<dyn ByteTransportFactory> {
        Arc::clone(&self.options.transport_factory)
    }

    fn on_handshake(&self, snapshot: ServerSnapshot) {
        self.state.apply_server_snapshot(snapshot);
    }

    fn on_message(&self, message: ServerMessage) {
        self.handle_message(message);
    }

    fn on_state_change(&self, change: ConnectionStateChange) {
        self.handle_connection_state_change(change);
    }
}

impl ClientInner {
    pub(crate) fn state(&self) -> &ClientState {
        &self.state
    }

    pub(crate) fn connected(&self) -> bool {
        self.connection.state() == ConnectionState::Connected
    }

    pub(crate) fn assert_not_disposed(&self) -> Result<(), PiClientError> {
        if self.disposed.load(Ordering::SeqCst) {
            return Err(PiClientError::Disposed);
        }
        Ok(())
    }

    fn frame_options(&self) -> FrameDecoderOptions {
        FrameDecoderOptions::with_max_frame_length(self.connection.max_frame_length())
    }

    // -- requests ----------------------------------------------------------

    pub(crate) async fn request(&self, command: Command) -> Result<CommandResult, PiClientError> {
        self.assert_not_disposed()?;
        if !self.connected() {
            return Err(PiClientError::disconnected());
        }
        let id = format!(
            "request-{}",
            self.request_sequence.fetch_add(1, Ordering::SeqCst) + 1
        );
        let (responder, receiver) = oneshot::channel();
        self.pending.lock().insert(
            id.clone(),
            PendingRequest {
                command: command.clone(),
                responder,
            },
        );

        let envelope = pi_protocol::ClientMessage::Request(RequestEnvelope {
            id: id.clone(),
            request: command,
        });
        match encode_client_message(&envelope, self.frame_options()) {
            Ok(frame) => {
                if let Err(error) = self.connection.send(frame) {
                    self.reject_pending(&id, error);
                }
            }
            Err(error) => self.reject_pending(&id, error.into()),
        }

        receiver
            .await
            .unwrap_or_else(|_| Err(PiClientError::disconnected()))
    }

    fn reject_pending(&self, id: &str, error: PiClientError) {
        if let Some(pending) = self.take_pending(id) {
            let _ = pending.responder.send(Err(error));
        }
    }

    fn take_pending(&self, id: &str) -> Option<PendingRequest> {
        self.pending.lock().remove(id)
    }

    fn reject_all_pending(&self, error: PiClientError) {
        let requests: Vec<PendingRequest> = self.pending.lock().drain().map(|(_, r)| r).collect();
        for request in requests {
            let _ = request.responder.send(Err(error.clone()));
        }
    }

    // -- inbound -----------------------------------------------------------

    fn handle_message(&self, message: ServerMessage) {
        match message {
            ServerMessage::Event(envelope) => {
                if let ServerEvent::SessionRemoved(event) = &envelope.event {
                    self.invalidate_session_leases(&event.session_id);
                }
                self.state.apply_event(&envelope.event);
            }
            ServerMessage::Response(envelope) => {
                let Some(pending) = self.take_pending(&envelope.id) else {
                    self.connection.fail(PiClientError::ProtocolViolation(
                        "Response has no matching request".to_string(),
                    ));
                    return;
                };
                if !envelope.ok {
                    let error = envelope.error.map(PiClientError::from).unwrap_or_else(|| {
                        PiClientError::ProtocolViolation(
                            "Failed response carries no error".to_string(),
                        )
                    });
                    let _ = pending.responder.send(Err(error));
                    return;
                }
                let Some(result) = envelope.result else {
                    let error = PiClientError::ProtocolViolation(
                        "Successful response carries no result".to_string(),
                    );
                    let _ = pending.responder.send(Err(error.clone()));
                    self.connection.fail(error);
                    return;
                };
                if result.name() != pending.command.name() {
                    let error = PiClientError::ProtocolViolation(format!(
                        "Response command {} does not match {}",
                        result.name(),
                        pending.command.name()
                    ));
                    let _ = pending.responder.send(Err(error.clone()));
                    self.connection.fail(error);
                    return;
                }
                self.state.apply_result(&result);
                let _ = pending.responder.send(Ok(result));
            }
            ServerMessage::Hello(_) | ServerMessage::HelloError(_) => {}
        }
    }

    fn handle_connection_state_change(&self, change: ConnectionStateChange) {
        if change.state == ConnectionState::Disconnected {
            self.state.clear_attachments();
            self.invalidate_all_session_leases();
            self.reject_all_pending(
                change
                    .error
                    .clone()
                    .unwrap_or_else(PiClientError::disconnected),
            );
        }
        for listener in self.connection_state_listeners.snapshot() {
            if let Err(error) = invoke_listener(&listener, change.clone()) {
                self.state.report_listener_error(error);
            }
        }
    }

    // -- leases ------------------------------------------------------------

    pub(crate) fn lease_generation(&self, session_id: &str) -> u64 {
        self.leases
            .lock()
            .generations
            .get(session_id)
            .copied()
            .unwrap_or(0)
    }

    pub(crate) fn lease_count(&self, session_id: &str) -> u64 {
        self.leases
            .lock()
            .counts
            .get(session_id)
            .copied()
            .unwrap_or(0)
    }

    fn reserve_session_lease(
        &self,
        session_id: &str,
        mode: SessionLeaseMode,
    ) -> Result<Arc<LeaseToken>, PiClientError> {
        let mut leases = self.leases.lock();
        let count = leases.counts.get(session_id).copied().unwrap_or(0);
        if mode == SessionLeaseMode::Exclusive && count > 0 {
            return Err(PiClientError::SessionOwnership {
                session_id: session_id.to_string(),
                message: format!("Session {session_id} already has an active lease"),
            });
        }
        if mode == SessionLeaseMode::Shared && leases.exclusive.contains_key(session_id) {
            return Err(PiClientError::SessionOwnership {
                session_id: session_id.to_string(),
                message: format!("Session {session_id} has an exclusive lease"),
            });
        }
        let token = Arc::new(LeaseToken { mode });
        leases.counts.insert(session_id.to_string(), count + 1);
        if mode == SessionLeaseMode::Exclusive {
            leases
                .exclusive
                .insert(session_id.to_string(), Arc::clone(&token));
        }
        Ok(token)
    }

    pub(crate) fn release_session_lease(&self, session_id: &str, token: &Arc<LeaseToken>) {
        let mut leases = self.leases.lock();
        let count = leases.counts.get(session_id).copied().unwrap_or(0);
        if count <= 1 {
            leases.counts.remove(session_id);
        } else {
            leases.counts.insert(session_id.to_string(), count - 1);
        }
        if leases
            .exclusive
            .get(session_id)
            .is_some_and(|current| Arc::ptr_eq(current, token))
        {
            leases.exclusive.remove(session_id);
        }
    }

    pub(crate) fn mark_cleanup_required(&self, session_id: &str) {
        self.leases
            .lock()
            .cleanup_required
            .insert(session_id.to_string());
    }

    fn invalidate_session_leases(&self, session_id: &str) {
        let mut leases = self.leases.lock();
        leases.counts.remove(session_id);
        leases.exclusive.remove(session_id);
        leases.cleanup_required.remove(session_id);
        let generation = leases.generations.get(session_id).copied().unwrap_or(0);
        leases
            .generations
            .insert(session_id.to_string(), generation + 1);
    }

    fn invalidate_all_session_leases(&self) {
        let ids: Vec<String> = self.leases.lock().counts.keys().cloned().collect();
        for id in ids {
            self.invalidate_session_leases(&id);
        }
        self.leases.lock().cleanup_required.clear();
    }

    fn create_session_lease(&self, session_id: &str, token: Arc<LeaseToken>) -> PiSessionHandle {
        let generation = {
            let mut leases = self.leases.lock();
            *leases
                .generations
                .entry(session_id.to_string())
                .or_insert(0)
        };
        PiSessionHandle::new(
            session_id.to_string(),
            self.self_weak.clone(),
            token,
            generation,
        )
    }

    // -- attach / detach ---------------------------------------------------

    /// Sends `detach` under the shared-detachment registry so a concurrent
    /// `acquire_session` waits for it (upstream's `#sessionDetachments`).
    pub(crate) async fn detach_session(&self, session_id: &str) -> Result<(), PiClientError> {
        let (sender, shared) = shared_slot(&self.detachments, session_id);
        if let Some(sender) = sender {
            let result = self
                .request(Command::Detach(SessionCommand::new(session_id)))
                .await
                .map(|_| ());
            self.detachments.lock().remove(session_id);
            let _ = sender.send(result);
        }
        shared
            .await
            .unwrap_or_else(|_| Err(PiClientError::disconnected()))
    }

    async fn run_attachment(&self, session_id: &str) -> Result<(), PiClientError> {
        let (sender, shared) = shared_slot(&self.attachments, session_id);
        if let Some(sender) = sender {
            let result = self.attach_session_request(session_id).await;
            self.attachments.lock().remove(session_id);
            let _ = sender.send(result);
        }
        shared
            .await
            .unwrap_or_else(|_| Err(PiClientError::disconnected()))
    }

    async fn attach_session_request(&self, session_id: &str) -> Result<(), PiClientError> {
        // Dropping the cached snapshot means a stale, higher revision cannot
        // suppress the snapshot the reacquired runtime sends back.
        let previous = self.state.forget_session_snapshot(session_id);
        match self
            .request(Command::Attach(SessionCommand::new(session_id)))
            .await
        {
            Ok(_) => Ok(()),
            Err(error) => {
                if let Some(previous) = previous {
                    self.state.restore_session_snapshot(previous);
                }
                Err(error)
            }
        }
    }

    /// Returns `true` when a deferred detach had to be replayed first.
    async fn reconcile_session_cleanup(&self, session_id: &str) -> Result<bool, PiClientError> {
        if !self.leases.lock().cleanup_required.contains(session_id) {
            return Ok(false);
        }
        let (sender, shared) = shared_slot(&self.reconciliations, session_id);
        if let Some(sender) = sender {
            let result = self
                .request(Command::Detach(SessionCommand::new(session_id)))
                .await
                .map(|_| ());
            if result.is_ok() {
                self.leases.lock().cleanup_required.remove(session_id);
            }
            self.reconciliations.lock().remove(session_id);
            let _ = sender.send(result);
        }
        shared
            .await
            .unwrap_or_else(|_| Err(PiClientError::disconnected()))?;
        Ok(true)
    }
}

/// Gets or creates the shared completion for `key`. The sender is returned only
/// to the caller that created the slot; everyone else just awaits.
fn shared_slot(
    registry: &Mutex<HashMap<String, SharedResult>>,
    key: &str,
) -> (
    Option<futures::channel::oneshot::Sender<Result<(), PiClientError>>>,
    SharedResult,
) {
    let mut map = registry.lock();
    if let Some(existing) = map.get(key) {
        return (None, existing.clone());
    }
    let (sender, receiver) = futures::channel::oneshot::channel();
    let shared = receiver.shared();
    map.insert(key.to_string(), shared.clone());
    (Some(sender), shared)
}

/// The session protocol client.
#[derive(Clone)]
pub struct PiClient {
    inner: Arc<ClientInner>,
}

impl PiClient {
    pub fn new(options: PiClientOptions) -> Result<Self, PiClientError> {
        let connection = Connection::new(options.max_frame_length)?;
        let state = ClientState::new(options.on_listener_error.clone());
        let inner = Arc::new_cyclic(|weak: &Weak<ClientInner>| ClientInner {
            self_weak: weak.clone(),
            options,
            connection,
            state,
            pending: Mutex::new(HashMap::new()),
            leases: Mutex::new(LeaseRegistry::default()),
            attachments: Mutex::new(HashMap::new()),
            detachments: Mutex::new(HashMap::new()),
            reconciliations: Mutex::new(HashMap::new()),
            connection_state_listeners: Arc::new(ListenerSet::default()),
            request_sequence: AtomicU64::new(0),
            disposed: AtomicBool::new(false),
        });
        let host: Weak<dyn ConnectionHost> = Arc::downgrade(&inner) as Weak<dyn ConnectionHost>;
        inner.connection.attach_host(host);
        Ok(Self { inner })
    }

    /// Upstream's static `PiClient.connect`: disposes the client if the first
    /// handshake fails so a caller cannot be handed a dead instance.
    pub async fn connect_new(options: PiClientOptions) -> Result<Self, PiClientError> {
        let client = Self::new(options)?;
        match client.connect().await {
            Ok(_) => Ok(client),
            Err(error) => {
                client.dispose().await;
                Err(error)
            }
        }
    }

    pub fn disposed(&self) -> bool {
        self.inner.disposed.load(Ordering::SeqCst)
    }

    pub fn connection_state(&self) -> ConnectionState {
        self.inner.connection.state()
    }

    pub fn connected(&self) -> bool {
        self.inner.connected()
    }

    pub fn snapshot(&self) -> Option<ServerSnapshot> {
        self.inner.state.snapshot()
    }

    pub async fn connect(&self) -> Result<ServerSnapshot, PiClientError> {
        self.inner.assert_not_disposed()?;
        if self.inner.connection.state() == ConnectionState::Disconnected {
            self.inner.state.reset();
        }
        self.inner.connection.connect().await
    }

    pub async fn reconnect(&self) -> Result<ServerSnapshot, PiClientError> {
        self.connect().await
    }

    pub fn disconnect(&self, reason: impl Into<String>) {
        self.inner
            .connection
            .disconnect(PiClientError::Disconnected(reason.into()));
    }

    pub fn subscribe(
        &self,
        listener: Listener<ServerSnapshot>,
    ) -> Result<Unsubscribe, PiClientError> {
        self.inner.assert_not_disposed()?;
        Ok(self.inner.state.subscribe(listener))
    }

    pub fn on_event(&self, listener: Listener<ServerEvent>) -> Result<Unsubscribe, PiClientError> {
        self.inner.assert_not_disposed()?;
        Ok(self.inner.state.on_event(listener))
    }

    pub fn on_connection_state_change(
        &self,
        listener: Listener<ConnectionStateChange>,
    ) -> Result<Unsubscribe, PiClientError> {
        self.inner.assert_not_disposed()?;
        Ok(self.inner.connection_state_listeners.add(listener))
    }

    pub async fn list_sessions(&self) -> Result<Vec<SessionMetadata>, PiClientError> {
        match self.inner.request(Command::List(ListCommand {})).await? {
            CommandResult::List(result) => Ok(result.sessions),
            other => Err(unexpected_result(other)),
        }
    }

    pub async fn create_session(
        &self,
        options: CreateSessionOptions,
    ) -> Result<PiSessionHandle, PiClientError> {
        let result = self
            .inner
            .request(Command::Create(CreateCommand {
                cwd: options.cwd,
                name: options.name,
                model: options.model,
                thinking_level: options.thinking_level,
            }))
            .await?;
        let session = match result {
            CommandResult::Create(result) => result.session,
            other => return Err(unexpected_result(other)),
        };
        let token = self
            .inner
            .reserve_session_lease(&session.id, SessionLeaseMode::Exclusive)?;
        Ok(self.inner.create_session_lease(&session.id, token))
    }

    pub async fn attach_session(&self, session_id: &str) -> Result<PiSessionHandle, PiClientError> {
        self.acquire_session(session_id, AcquireSessionOptions::shared())
            .await
    }

    pub async fn acquire_session(
        &self,
        session_id: &str,
        options: AcquireSessionOptions,
    ) -> Result<PiSessionHandle, PiClientError> {
        self.inner.assert_not_disposed()?;
        let token = self.inner.reserve_session_lease(session_id, options.mode)?;
        match self.acquire_inner(session_id).await {
            Ok(()) => Ok(self.inner.create_session_lease(session_id, token)),
            Err(error) => {
                self.inner.release_session_lease(session_id, &token);
                Err(error)
            }
        }
    }

    async fn acquire_inner(&self, session_id: &str) -> Result<(), PiClientError> {
        let detachment = self.inner.detachments.lock().get(session_id).cloned();
        if let Some(detachment) = detachment {
            // Upstream swallows the detach failure here; the reconciliation
            // path below is what repairs a session left attached.
            let _ = detachment.await;
        }
        let reconciled = self.inner.reconcile_session_cleanup(session_id).await?;
        if reconciled || !self.inner.state.is_session_attached(session_id) {
            self.inner.run_attachment(session_id).await?;
        }
        Ok(())
    }

    pub async fn dispose(&self) {
        if self.inner.disposed.swap(true, Ordering::SeqCst) {
            return;
        }
        self.inner.reject_all_pending(PiClientError::Disposed);
        self.inner.connection.disconnect(PiClientError::Disposed);
        self.inner.state.dispose();
        self.inner.invalidate_all_session_leases();
        self.inner.connection_state_listeners.clear();
    }
}

fn unexpected_result(result: CommandResult) -> PiClientError {
    PiClientError::ProtocolViolation(format!("Unexpected {} result", result.name()))
}

impl std::fmt::Debug for PiClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PiClient")
            .field("state", &self.connection_state())
            .field("disposed", &self.disposed())
            .finish()
    }
}
