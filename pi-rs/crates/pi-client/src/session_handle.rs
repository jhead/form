//! Port of `.upstream/packages/client/src/session-handle.ts` plus the lease
//! state machine upstream keeps in closures inside `PiClient#createSessionLease`.

use std::sync::{Arc, Weak};

use parking_lot::Mutex;
use pi_protocol::{
    Command, CommandResult, ModelRef, PromptCommand, ServerEvent, SessionCommand, SessionSnapshot,
    SetModelCommand, SetThinkingCommand, ThinkingLevel,
};

use crate::client::ClientInner;
use crate::errors::PiClientError;
use crate::types::{Listener, Unsubscribe};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SessionLeaseMode {
    Shared,
    Exclusive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AcquireSessionOptions {
    pub mode: SessionLeaseMode,
}

impl AcquireSessionOptions {
    pub fn shared() -> Self {
        Self {
            mode: SessionLeaseMode::Shared,
        }
    }

    pub fn exclusive() -> Self {
        Self {
            mode: SessionLeaseMode::Exclusive,
        }
    }
}

/// Lease identity. Upstream compares object references; here the `Arc` pointer
/// plays that role.
#[derive(Debug)]
pub(crate) struct LeaseToken {
    pub(crate) mode: SessionLeaseMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LeaseState {
    Active,
    Releasing,
    Released,
    Invalidated,
}

struct LeaseLocal {
    state: LeaseState,
    last_error: Option<PiClientError>,
}

pub(crate) struct LeaseInner {
    id: String,
    client: Weak<ClientInner>,
    token: Arc<LeaseToken>,
    generation: u64,
    local: Mutex<LeaseLocal>,
    /// Serializes concurrent `detach`/`dispose`. Upstream memoizes one promise;
    /// the difference is only visible when a release fails and a second caller
    /// is already waiting, in which case this retries instead of replaying the
    /// first failure.
    release_lock: tokio::sync::Mutex<()>,
}

impl LeaseInner {
    fn client(&self) -> Result<Arc<ClientInner>, PiClientError> {
        self.client.upgrade().ok_or(PiClientError::Disposed)
    }

    fn refresh(&self, local: &mut LeaseLocal) {
        if !matches!(local.state, LeaseState::Active | LeaseState::Releasing) {
            return;
        }
        let current = self
            .client
            .upgrade()
            .map(|client| client.lease_generation(&self.id));
        if current != Some(self.generation) {
            local.state = LeaseState::Invalidated;
        }
    }

    fn is_active(&self) -> bool {
        let mut local = self.local.lock();
        self.refresh(&mut local);
        if local.state != LeaseState::Active {
            return false;
        }
        self.client
            .upgrade()
            .is_some_and(|client| client.state().is_session_attached(&self.id))
    }

    fn assert_active(&self) -> Result<(), PiClientError> {
        let mut local = self.local.lock();
        self.assert_active_locked(&mut local)
    }

    fn assert_active_locked(&self, local: &mut LeaseLocal) -> Result<(), PiClientError> {
        let client = self.client()?;
        client.assert_not_disposed()?;
        if !client.connected() {
            return Err(PiClientError::disconnected());
        }
        self.refresh(local);
        let active =
            local.state == LeaseState::Active && client.state().is_session_attached(&self.id);
        if active {
            Ok(())
        } else {
            Err(PiClientError::SessionDetached {
                session_id: self.id.clone(),
            })
        }
    }

    async fn release(&self, relinquish_on_failure: bool) -> Result<(), PiClientError> {
        enum Plan {
            Done,
            Wait,
            Run,
        }
        let plan = {
            let mut local = self.local.lock();
            self.refresh(&mut local);
            match local.state {
                LeaseState::Released | LeaseState::Invalidated => Plan::Done,
                LeaseState::Releasing => Plan::Wait,
                LeaseState::Active => {
                    self.assert_active_locked(&mut local)?;
                    local.state = LeaseState::Releasing;
                    Plan::Run
                }
            }
        };
        match plan {
            Plan::Done => Ok(()),
            Plan::Wait => {
                let _guard = self.release_lock.lock().await;
                let local = self.local.lock();
                match local.state {
                    LeaseState::Released | LeaseState::Invalidated => Ok(()),
                    _ => Err(local
                        .last_error
                        .clone()
                        .unwrap_or(PiClientError::SessionDetached {
                            session_id: self.id.clone(),
                        })),
                }
            }
            Plan::Run => {
                let _guard = self.release_lock.lock().await;
                match self.perform_release().await {
                    Ok(()) => {
                        let mut local = self.local.lock();
                        local.state = LeaseState::Released;
                        local.last_error = None;
                        Ok(())
                    }
                    Err(error) => {
                        let mut local = self.local.lock();
                        self.refresh(&mut local);
                        // A lease invalidated mid-release (the session went
                        // away) resolves rather than failing, as upstream does.
                        if local.state == LeaseState::Invalidated {
                            return Ok(());
                        }
                        if relinquish_on_failure {
                            if let Ok(client) = self.client() {
                                client.release_session_lease(&self.id, &self.token);
                                client.mark_cleanup_required(&self.id);
                            }
                            local.state = LeaseState::Released;
                        } else {
                            local.state = LeaseState::Active;
                        }
                        local.last_error = Some(error.clone());
                        Err(error)
                    }
                }
            }
        }
    }

    async fn perform_release(&self) -> Result<(), PiClientError> {
        let client = self.client()?;
        if client.lease_count(&self.id) > 1 {
            client.release_session_lease(&self.id, &self.token);
            return Ok(());
        }
        let result = client.detach_session(&self.id).await;
        if result.is_ok() {
            client.release_session_lease(&self.id, &self.token);
        }
        result
    }

    async fn request(&self, command: Command) -> Result<CommandResult, PiClientError> {
        self.assert_active()?;
        let client = self.client()?;
        client.request(command).await
    }

    async fn session_request(&self, command: Command) -> Result<SessionSnapshot, PiClientError> {
        match self.request(command).await? {
            CommandResult::Create(result)
            | CommandResult::Attach(result)
            | CommandResult::Prompt(result)
            | CommandResult::Steer(result)
            | CommandResult::Abort(result)
            | CommandResult::SetModel(result)
            | CommandResult::SetThinking(result) => Ok(result.session),
            other => Err(PiClientError::ProtocolViolation(format!(
                "Response command {} does not carry a session",
                other.name()
            ))),
        }
    }
}

/// One acquired session. Upstream calls this `SessionLease`/`PiSessionHandle`.
#[derive(Clone)]
pub struct PiSessionHandle {
    inner: Arc<LeaseInner>,
}

pub type SessionLease = PiSessionHandle;

impl PiSessionHandle {
    pub(crate) fn new(
        id: String,
        client: Weak<ClientInner>,
        token: Arc<LeaseToken>,
        generation: u64,
    ) -> Self {
        Self {
            inner: Arc::new(LeaseInner {
                id,
                client,
                token,
                generation,
                local: Mutex::new(LeaseLocal {
                    state: LeaseState::Active,
                    last_error: None,
                }),
                release_lock: tokio::sync::Mutex::new(()),
            }),
        }
    }

    pub fn id(&self) -> &str {
        &self.inner.id
    }

    pub fn mode(&self) -> SessionLeaseMode {
        self.inner.token.mode
    }

    pub fn attached(&self) -> bool {
        self.inner.is_active()
    }

    /// Upstream's `active`, an alias of `attached`.
    pub fn active(&self) -> bool {
        self.attached()
    }

    pub fn snapshot(&self) -> Option<SessionSnapshot> {
        if !self.inner.is_active() {
            return None;
        }
        self.inner
            .client
            .upgrade()?
            .state()
            .get_session_snapshot(&self.inner.id)
    }

    pub fn subscribe(
        &self,
        listener: Listener<SessionSnapshot>,
    ) -> Result<Unsubscribe, PiClientError> {
        self.inner.assert_active()?;
        let client = self.inner.client()?;
        let lease = Arc::clone(&self.inner);
        Ok(client.state().subscribe_session(
            &self.inner.id,
            Arc::new(move |snapshot| {
                if lease.is_active() {
                    listener(snapshot);
                }
            }),
        ))
    }

    pub fn on_event(&self, listener: Listener<ServerEvent>) -> Result<Unsubscribe, PiClientError> {
        self.inner.assert_active()?;
        let client = self.inner.client()?;
        let lease = Arc::clone(&self.inner);
        Ok(client.state().on_session_event(
            &self.inner.id,
            Arc::new(move |event| {
                // `session_removed` still reaches the listener so a consumer
                // learns why its lease went away.
                if lease.is_active() || matches!(event, ServerEvent::SessionRemoved(_)) {
                    listener(event);
                }
            }),
        ))
    }

    /// Releases the lease, restoring it if the protocol detach fails.
    pub async fn detach(&self) -> Result<(), PiClientError> {
        self.inner.release(false).await
    }

    /// Releases the lease unconditionally, scheduling a protocol cleanup if the
    /// detach failed. Upstream's `dispose`/`Symbol.asyncDispose`.
    pub async fn dispose(&self) -> Result<(), PiClientError> {
        self.inner.release(true).await
    }

    pub async fn prompt(&self, text: impl Into<String>) -> Result<SessionSnapshot, PiClientError> {
        self.inner
            .session_request(Command::Prompt(PromptCommand {
                session_id: self.inner.id.clone(),
                text: text.into(),
            }))
            .await
    }

    pub async fn steer(&self, text: impl Into<String>) -> Result<SessionSnapshot, PiClientError> {
        self.inner
            .session_request(Command::Steer(PromptCommand {
                session_id: self.inner.id.clone(),
                text: text.into(),
            }))
            .await
    }

    pub async fn abort(&self) -> Result<SessionSnapshot, PiClientError> {
        self.inner
            .session_request(Command::Abort(SessionCommand::new(self.inner.id.clone())))
            .await
    }

    pub async fn set_model(&self, model: ModelRef) -> Result<SessionSnapshot, PiClientError> {
        self.inner
            .session_request(Command::SetModel(SetModelCommand {
                session_id: self.inner.id.clone(),
                model,
            }))
            .await
    }

    pub async fn set_thinking(
        &self,
        thinking_level: ThinkingLevel,
    ) -> Result<SessionSnapshot, PiClientError> {
        self.inner
            .session_request(Command::SetThinking(SetThinkingCommand {
                session_id: self.inner.id.clone(),
                thinking_level,
            }))
            .await
    }
}

impl std::fmt::Debug for PiSessionHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PiSessionHandle")
            .field("id", &self.inner.id)
            .field("attached", &self.attached())
            .finish()
    }
}
