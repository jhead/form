//! Port of `packages/agent/src/harness/events.ts`.
//!
//! Upstream's bus is synchronous and deliberately re-entrant: `watch()` invokes
//! the snapshot callback while already subscribed, so an `emit()` from inside
//! that callback must be buffered rather than lost. The port keeps that
//! guarantee by never holding a lock while a listener runs.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunStartEvent {
    pub lane: String,
    pub run_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RunOutcome {
    Completed,
    Aborted,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunEndEvent {
    pub lane: String,
    pub run_id: String,
    pub outcome: RunOutcome,
    pub leaf_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HarnessEvent {
    RunStart(RunStartEvent),
    RunEnd(RunEndEvent),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum HarnessEventType {
    RunStart,
    RunEnd,
}

impl HarnessEvent {
    pub fn event_type(&self) -> HarnessEventType {
        match self {
            HarnessEvent::RunStart(_) => HarnessEventType::RunStart,
            HarnessEvent::RunEnd(_) => HarnessEventType::RunEnd,
        }
    }
}

/// Passive listener. Async listeners upstream are fire-and-forget (`void
/// listener(event)`), so the port takes a plain `Fn`.
pub type HarnessEventListener = Arc<dyn Fn(&HarnessEvent) + Send + Sync>;

/// Unsubscribe token. Dropping it does nothing; call [`Subscription::unsubscribe`].
pub struct Subscription {
    bus: Arc<BusInner>,
    id: u64,
}

impl Subscription {
    pub fn unsubscribe(self) {
        self.bus.listeners.lock().remove(&self.id);
    }
}

enum WatchState {
    Buffering(Vec<HarnessEvent>),
    Live(HarnessEventListener),
}

struct WatchSlot {
    state: Mutex<WatchState>,
}

#[derive(Default)]
struct BusInner {
    listeners: Mutex<BTreeMap<u64, (HarnessEventType, HarnessEventListener)>>,
    watchers: Mutex<BTreeMap<u64, Arc<WatchSlot>>>,
    next_id: AtomicU64,
}

/// Snapshot-plus-stream handle returned by [`HarnessEventBus::watch`].
pub struct WatchHandle<TSnapshot> {
    pub snapshot: TSnapshot,
    bus: Arc<BusInner>,
    id: u64,
    slot: Arc<WatchSlot>,
}

impl<TSnapshot> WatchHandle<TSnapshot> {
    /// Flush everything buffered since the snapshot, then deliver live events.
    pub fn start(&self, listener: HarnessEventListener) {
        // Stay in buffering mode while flushing so re-entrant emissions keep order.
        loop {
            let pending = {
                let mut state = self.slot.state.lock();
                match &mut *state {
                    WatchState::Buffering(buffer) if !buffer.is_empty() => std::mem::take(buffer),
                    WatchState::Buffering(_) => {
                        *state = WatchState::Live(listener.clone());
                        return;
                    }
                    WatchState::Live(_) => return,
                }
            };
            for event in pending {
                listener(&event);
            }
        }
    }

    pub fn unsubscribe(&self) {
        self.bus.watchers.lock().remove(&self.id);
        *self.slot.state.lock() = WatchState::Buffering(Vec::new());
    }
}

/// Synchronous fan-out bus for harness lifecycle events.
#[derive(Clone, Default)]
pub struct HarnessEventBus {
    inner: Arc<BusInner>,
}

impl HarnessEventBus {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a listener for future events of one type. Earlier events are not
    /// replayed and no snapshot is provided; use [`Self::watch`] for both.
    pub fn on(&self, event_type: HarnessEventType, listener: HarnessEventListener) -> Subscription {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        self.inner
            .listeners
            .lock()
            .insert(id, (event_type, listener));
        Subscription {
            bus: self.inner.clone(),
            id,
        }
    }

    /// Publish to type listeners first, then to every watcher.
    pub fn emit(&self, event: HarnessEvent) {
        let matching: Vec<HarnessEventListener> = self
            .inner
            .listeners
            .lock()
            .values()
            .filter(|(t, _)| *t == event.event_type())
            .map(|(_, l)| l.clone())
            .collect();
        for listener in matching {
            listener(&event);
        }

        let slots: Vec<Arc<WatchSlot>> = self.inner.watchers.lock().values().cloned().collect();
        for slot in slots {
            // Resolve the live listener under the lock, but call it outside so a
            // re-entrant emit() from the listener cannot deadlock.
            let live = {
                let mut state = slot.state.lock();
                match &mut *state {
                    WatchState::Buffering(buffer) => {
                        buffer.push(event.clone());
                        None
                    }
                    WatchState::Live(listener) => Some(listener.clone()),
                }
            };
            if let Some(listener) = live {
                listener(&event);
            }
        }
    }

    /// Subscribe, then capture a snapshot. Events emitted during the capture are
    /// buffered and delivered by [`WatchHandle::start`], so there is no gap.
    pub fn watch<TSnapshot>(
        &self,
        capture_snapshot: impl FnOnce() -> TSnapshot,
    ) -> WatchHandle<TSnapshot> {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let slot = Arc::new(WatchSlot {
            state: Mutex::new(WatchState::Buffering(Vec::new())),
        });
        self.inner.watchers.lock().insert(id, slot.clone());
        let snapshot = capture_snapshot();
        WatchHandle {
            snapshot,
            bus: self.inner.clone(),
            id,
            slot,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_start() -> HarnessEvent {
        HarnessEvent::RunStart(RunStartEvent {
            lane: "main".into(),
            run_id: "run-1".into(),
        })
    }

    fn run_end() -> HarnessEvent {
        HarnessEvent::RunEnd(RunEndEvent {
            lane: "main".into(),
            run_id: "run-1".into(),
            outcome: RunOutcome::Completed,
            leaf_id: "entry-1".into(),
        })
    }

    #[test]
    fn delivers_matching_events_to_direct_listeners_and_watchers() {
        let bus = HarnessEventBus::new();
        let direct = Arc::new(Mutex::new(Vec::new()));
        let watched = Arc::new(Mutex::new(Vec::new()));

        let sink = direct.clone();
        let off = bus.on(
            HarnessEventType::RunStart,
            Arc::new(move |e: &HarnessEvent| sink.lock().push(e.clone())),
        );
        let watch = bus.watch(|| ());
        let sink = watched.clone();
        watch.start(Arc::new(move |e: &HarnessEvent| {
            sink.lock().push(e.clone())
        }));

        bus.emit(run_start());
        bus.emit(run_end());
        off.unsubscribe();
        bus.emit(run_start());

        assert_eq!(&*direct.lock(), &[run_start()]);
        assert_eq!(&*watched.lock(), &[run_start(), run_end(), run_start()]);
    }

    #[test]
    fn captures_snapshot_without_gap_then_flushes() {
        let bus = HarnessEventBus::new();
        let received = Arc::new(Mutex::new(Vec::new()));

        let emitting_bus = bus.clone();
        let watch = bus.watch(|| {
            emitting_bus.emit(run_start());
            "snapshot"
        });
        assert_eq!(watch.snapshot, "snapshot");
        assert!(received.lock().is_empty());

        let sink = received.clone();
        watch.start(Arc::new(move |e: &HarnessEvent| {
            sink.lock().push(e.clone())
        }));
        assert_eq!(&*received.lock(), &[run_start()]);

        bus.emit(run_end());
        assert_eq!(&*received.lock(), &[run_start(), run_end()]);

        watch.unsubscribe();
        bus.emit(run_start());
        assert_eq!(&*received.lock(), &[run_start(), run_end()]);
    }
}
