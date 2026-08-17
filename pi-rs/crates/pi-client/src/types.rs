//! Port of `.upstream/packages/client/src/types.ts`, plus the listener
//! plumbing upstream gets for free from JavaScript closures.

use std::collections::HashMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};

use parking_lot::Mutex;
use pi_protocol::{ModelRef, ThinkingLevel};

use crate::transport::ByteTransportFactory;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConnectionState {
    Disconnected,
    Connecting,
    Connected,
}

impl ConnectionState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disconnected => "disconnected",
            Self::Connecting => "connecting",
            Self::Connected => "connected",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConnectionStateChange {
    pub state: ConnectionState,
    pub error: Option<crate::PiClientError>,
}

/// A subscriber failure. Rust callbacks return `()`, so the only way one can
/// fail is by panicking; the payload's message is recovered here so upstream's
/// "report but do not let it corrupt client state" behaviour still holds.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct ListenerError {
    pub message: String,
}

pub type ListenerErrorHandler = Arc<dyn Fn(ListenerError) + Send + Sync>;

/// Listeners take owned values so no lifetime leaks into the public API.
pub type Listener<T> = Arc<dyn Fn(T) + Send + Sync>;

#[derive(Clone)]
pub struct PiClientOptions {
    pub transport_factory: Arc<dyn ByteTransportFactory>,
    pub max_frame_length: Option<u32>,
    /// Reports subscriber failures without allowing them to corrupt client state.
    pub on_listener_error: Option<ListenerErrorHandler>,
}

impl PiClientOptions {
    pub fn new(transport_factory: Arc<dyn ByteTransportFactory>) -> Self {
        Self {
            transport_factory,
            max_frame_length: None,
            on_listener_error: None,
        }
    }

    pub fn with_max_frame_length(mut self, max_frame_length: u32) -> Self {
        self.max_frame_length = Some(max_frame_length);
        self
    }

    pub fn with_listener_error_handler(mut self, handler: ListenerErrorHandler) -> Self {
        self.on_listener_error = Some(handler);
        self
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CreateSessionOptions {
    pub cwd: Option<String>,
    pub name: Option<String>,
    pub model: Option<ModelRef>,
    pub thinking_level: Option<ThinkingLevel>,
}

/// Handle returned by every `subscribe`/`on_event` call. Dropping it does
/// nothing; call [`Unsubscribe::unsubscribe`] (upstream calls the returned
/// function). Repeated calls are harmless.
#[derive(Clone)]
pub struct Unsubscribe(Arc<dyn Fn() + Send + Sync>);

impl Unsubscribe {
    pub(crate) fn new(action: impl Fn() + Send + Sync + 'static) -> Self {
        Self(Arc::new(action))
    }

    pub fn unsubscribe(&self) {
        (self.0)();
    }
}

impl std::fmt::Debug for Unsubscribe {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Unsubscribe")
    }
}

/// Runs one listener, converting a panic into a [`ListenerError`] the caller
/// can report. Upstream wraps every notification in `try`/`catch` for the same
/// reason: a consumer bug must not leave protocol state half-updated.
pub(crate) fn invoke_listener<T>(listener: &Listener<T>, value: T) -> Result<(), ListenerError> {
    let value = AssertUnwindSafe(value);
    catch_unwind(AssertUnwindSafe(move || listener(value.0))).map_err(|payload| ListenerError {
        message: panic_message(payload.as_ref()),
    })
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&'static str>() {
        return (*message).to_string();
    }
    if let Some(message) = payload.downcast_ref::<String>() {
        return message.clone();
    }
    "listener panicked".to_string()
}

/// An insertion-ordered set of listeners with stable removal tokens. Upstream
/// uses a `Set` of closures; Rust closures have no identity, so entries are
/// keyed by a counter and iterated in key order.
pub(crate) struct ListenerSet<T> {
    next_id: AtomicU64,
    listeners: Mutex<HashMap<u64, Listener<T>>>,
}

impl<T> Default for ListenerSet<T> {
    fn default() -> Self {
        Self {
            next_id: AtomicU64::new(0),
            listeners: Mutex::new(HashMap::new()),
        }
    }
}

impl<T: Clone + 'static> ListenerSet<T> {
    pub(crate) fn add(self: &Arc<Self>, listener: Listener<T>) -> Unsubscribe {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.listeners.lock().insert(id, listener);
        let weak: Weak<Self> = Arc::downgrade(self);
        Unsubscribe::new(move || {
            if let Some(set) = weak.upgrade() {
                set.listeners.lock().remove(&id);
            }
        })
    }

    pub(crate) fn clear(&self) {
        self.listeners.lock().clear();
    }

    /// Snapshots the listeners so the lock is never held while user code runs.
    pub(crate) fn snapshot(&self) -> Vec<Listener<T>> {
        let listeners = self.listeners.lock();
        let mut entries: Vec<_> = listeners.iter().map(|(id, l)| (*id, l.clone())).collect();
        entries.sort_by_key(|(id, _)| *id);
        entries.into_iter().map(|(_, l)| l).collect()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.listeners.lock().is_empty()
    }
}
