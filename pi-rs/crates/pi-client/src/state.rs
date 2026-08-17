//! Port of `.upstream/packages/client/src/state.ts`.
//!
//! One behavioural note: upstream notifies listeners inline because JavaScript
//! is single-threaded and re-entrant calls are safe. Here every notification
//! happens *after* the state lock is released, so a listener that calls back
//! into the client (`disconnect()` from a snapshot listener, which upstream's
//! tests exercise) cannot deadlock.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use parking_lot::Mutex;
use pi_protocol::{CommandResult, ServerEvent, ServerSnapshot, SessionSnapshot};

use crate::types::{invoke_listener, Listener, ListenerErrorHandler, ListenerSet, Unsubscribe};

#[derive(Default)]
struct StateData {
    snapshot: Option<ServerSnapshot>,
    session_snapshots: HashMap<String, SessionSnapshot>,
    attached_session_ids: HashSet<String>,
}

/// Keyed listener registries. Upstream drops the entry when its set empties so
/// a long-lived client does not accumulate one map entry per session ever seen;
/// the registry is an `Arc` so an unsubscribe closure can hold a `Weak` to it
/// and prune without keeping the client alive.
type KeyedListeners<T> = Arc<Mutex<HashMap<String, Arc<ListenerSet<T>>>>>;

pub(crate) struct ClientState {
    data: Mutex<StateData>,
    snapshot_listeners: Arc<ListenerSet<ServerSnapshot>>,
    event_listeners: Arc<ListenerSet<ServerEvent>>,
    session_snapshot_listeners: KeyedListeners<SessionSnapshot>,
    session_event_listeners: KeyedListeners<ServerEvent>,
    on_listener_error: Option<ListenerErrorHandler>,
}

impl ClientState {
    pub(crate) fn new(on_listener_error: Option<ListenerErrorHandler>) -> Self {
        Self {
            data: Mutex::new(StateData::default()),
            snapshot_listeners: Arc::new(ListenerSet::default()),
            event_listeners: Arc::new(ListenerSet::default()),
            session_snapshot_listeners: Arc::new(Mutex::new(HashMap::new())),
            session_event_listeners: Arc::new(Mutex::new(HashMap::new())),
            on_listener_error,
        }
    }

    pub(crate) fn snapshot(&self) -> Option<ServerSnapshot> {
        self.data.lock().snapshot.clone()
    }

    pub(crate) fn reset(&self) {
        let mut data = self.data.lock();
        data.snapshot = None;
        data.session_snapshots.clear();
        data.attached_session_ids.clear();
    }

    pub(crate) fn clear_attachments(&self) {
        self.data.lock().attached_session_ids.clear();
    }

    pub(crate) fn dispose(&self) {
        self.reset();
        self.snapshot_listeners.clear();
        self.event_listeners.clear();
        self.session_snapshot_listeners.lock().clear();
        self.session_event_listeners.lock().clear();
    }

    pub(crate) fn get_session_snapshot(&self, session_id: &str) -> Option<SessionSnapshot> {
        self.data.lock().session_snapshots.get(session_id).cloned()
    }

    pub(crate) fn is_session_attached(&self, session_id: &str) -> bool {
        self.data.lock().attached_session_ids.contains(session_id)
    }

    pub(crate) fn forget_session_snapshot(&self, session_id: &str) -> Option<SessionSnapshot> {
        self.data.lock().session_snapshots.remove(session_id)
    }

    pub(crate) fn restore_session_snapshot(&self, snapshot: SessionSnapshot) {
        let mut data = self.data.lock();
        data.session_snapshots
            .entry(snapshot.id.clone())
            .or_insert(snapshot);
    }

    pub(crate) fn subscribe(&self, listener: Listener<ServerSnapshot>) -> Unsubscribe {
        self.snapshot_listeners.add(listener)
    }

    pub(crate) fn on_event(&self, listener: Listener<ServerEvent>) -> Unsubscribe {
        self.event_listeners.add(listener)
    }

    pub(crate) fn subscribe_session(
        &self,
        session_id: &str,
        listener: Listener<SessionSnapshot>,
    ) -> Unsubscribe {
        add_keyed(&self.session_snapshot_listeners, session_id, listener)
    }

    pub(crate) fn on_session_event(
        &self,
        session_id: &str,
        listener: Listener<ServerEvent>,
    ) -> Unsubscribe {
        add_keyed(&self.session_event_listeners, session_id, listener)
    }

    pub(crate) fn apply_result(&self, result: &CommandResult) {
        match result {
            CommandResult::List(_) => {}
            CommandResult::Detach(detach) => {
                let previous = {
                    let mut data = self.data.lock();
                    data.attached_session_ids.remove(&detach.session_id);
                    data.session_snapshots.get(&detach.session_id).cloned()
                };
                if let Some(mut snapshot) = previous {
                    snapshot.attached = false;
                    // `force`: a detach must win even though the revision did
                    // not advance.
                    self.apply_session_snapshot(snapshot, true);
                }
            }
            CommandResult::Create(result)
            | CommandResult::Attach(result)
            | CommandResult::Prompt(result)
            | CommandResult::Steer(result)
            | CommandResult::Abort(result)
            | CommandResult::SetModel(result)
            | CommandResult::SetThinking(result) => {
                self.apply_session_snapshot(result.session.clone(), false);
            }
        }
    }

    pub(crate) fn apply_event(&self, event: &ServerEvent) {
        match event {
            ServerEvent::ServerSnapshot(payload) => {
                self.apply_server_snapshot(payload.snapshot.clone());
            }
            ServerEvent::SessionSnapshot(payload) => {
                self.apply_session_snapshot(payload.snapshot.clone(), false);
            }
            ServerEvent::SessionRemoved(payload) => {
                let mut data = self.data.lock();
                data.session_snapshots.remove(&payload.session_id);
                data.attached_session_ids.remove(&payload.session_id);
            }
            ServerEvent::SessionProgress(_) => {}
        }
        self.notify(&self.event_listeners.snapshot(), event.clone());
        if let Some(session_id) = event_session_id(event) {
            let listeners = self
                .session_event_listeners
                .lock()
                .get(session_id)
                .map(|set| set.snapshot())
                .unwrap_or_default();
            self.notify(&listeners, event.clone());
        }
    }

    pub(crate) fn apply_server_snapshot(&self, snapshot: ServerSnapshot) {
        {
            let mut data = self.data.lock();
            if data
                .snapshot
                .as_ref()
                .is_some_and(|current| snapshot.revision < current.revision)
            {
                return;
            }
            data.snapshot = Some(snapshot.clone());
        }
        self.notify(&self.snapshot_listeners.snapshot(), snapshot);
    }

    fn apply_session_snapshot(&self, snapshot: SessionSnapshot, force: bool) {
        {
            let mut data = self.data.lock();
            if !force {
                if let Some(current) = data.session_snapshots.get(&snapshot.id) {
                    if snapshot.revision < current.revision {
                        return;
                    }
                }
            }
            if snapshot.attached {
                data.attached_session_ids.insert(snapshot.id.clone());
            } else {
                data.attached_session_ids.remove(&snapshot.id);
            }
            data.session_snapshots
                .insert(snapshot.id.clone(), snapshot.clone());
        }
        let listeners = self
            .session_snapshot_listeners
            .lock()
            .get(&snapshot.id)
            .map(|set| set.snapshot())
            .unwrap_or_default();
        self.notify(&listeners, snapshot);
    }

    fn notify<T: Clone>(&self, listeners: &[Listener<T>], value: T) {
        for listener in listeners {
            if let Err(error) = invoke_listener(listener, value.clone()) {
                self.report_listener_error(error);
            }
        }
    }

    pub(crate) fn report_listener_error(&self, error: crate::types::ListenerError) {
        let Some(handler) = &self.on_listener_error else {
            return;
        };
        // Diagnostics cannot affect client state.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handler(error)));
    }
}

fn add_keyed<T: Clone + 'static>(
    registry: &KeyedListeners<T>,
    key: &str,
    listener: Listener<T>,
) -> Unsubscribe {
    let set = {
        let mut map = registry.lock();
        map.entry(key.to_string())
            .or_insert_with(|| Arc::new(ListenerSet::default()))
            .clone()
    };
    let inner = set.add(listener);
    // Mirrors upstream's `if (listeners.size === 0) listenersById.delete(id)`.
    let weak = Arc::downgrade(registry);
    let key = key.to_string();
    Unsubscribe::new(move || {
        inner.unsubscribe();
        let Some(registry) = weak.upgrade() else {
            return;
        };
        let mut map = registry.lock();
        if map.get(&key).is_some_and(|set| set.is_empty()) {
            map.remove(&key);
        }
    })
}

fn event_session_id(event: &ServerEvent) -> Option<&str> {
    match event {
        ServerEvent::SessionSnapshot(payload) => Some(&payload.snapshot.id),
        ServerEvent::SessionProgress(payload) => Some(&payload.session_id),
        ServerEvent::SessionRemoved(payload) => Some(&payload.session_id),
        ServerEvent::ServerSnapshot(_) => None,
    }
}
