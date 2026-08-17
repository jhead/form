//! The outbound event bus.
//!
//! Delivery contract (spec 00 §7): events reach the subscriber **in order and never
//! concurrently**. The FFI layer owns the single dispatcher thread; this bus only fans a
//! bounded queue out to the registered listeners.

use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Mutex};

use crate::protocol::{Event, EventKind};

pub type Listener = Arc<dyn Fn(&Event) + Send + Sync>;

#[derive(Clone, Default)]
pub struct EventBus {
    inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
    listeners: Mutex<Vec<(i32, Listener)>>,
    next_token: AtomicI32,
}

impl EventBus {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn subscribe(&self, listener: Listener) -> i32 {
        let token = self.inner.next_token.fetch_add(1, Ordering::Relaxed) + 1;
        self.inner
            .listeners
            .lock()
            .expect("event bus poisoned")
            .push((token, listener));
        token
    }

    /// After this returns, the listener is guaranteed not to be invoked again — the lock is
    /// the same one `emit` holds, so an in-flight delivery has finished.
    pub fn unsubscribe(&self, token: i32) {
        self.inner
            .listeners
            .lock()
            .expect("event bus poisoned")
            .retain(|(t, _)| *t != token);
    }

    pub fn emit(&self, event: Event) {
        let listeners = self.inner.listeners.lock().expect("event bus poisoned");
        for (_, listener) in listeners.iter() {
            listener(&event);
        }
    }

    pub fn emit_kind(&self, kind: EventKind) {
        self.emit(Event::new(kind));
    }

    pub fn emit_for(&self, kind: EventKind, command_id: Option<String>) {
        self.emit(Event::with_command(kind, command_id));
    }
}
