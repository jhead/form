//! Port of `.upstream/packages/server/src/snapshots.ts`.
//!
//! Upstream's `broadcastQueue` is a promise chain, which gives strict FIFO by
//! call order. Here that is an unbounded channel with a single drain task at a
//! time: `broadcast()` enqueues synchronously, so the revision each broadcast
//! claims follows the order the broadcasts were requested in.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};

use parking_lot::Mutex;
use pi_protocol::{
    EventEnvelope, ModelMetadata, ServerEvent, ServerSnapshot, ServerSnapshotEvent,
    PROTOCOL_VERSION,
};
use tokio::sync::mpsc;

use crate::connection::{ConnectionStage, ConnectionState};
use crate::errors::PiServerError;
use crate::server::ServerInner;
use crate::types::SessionService;

pub(crate) struct ServerSnapshotPublisher {
    server: Mutex<Weak<ServerInner>>,
    server_id: String,
    service: Arc<dyn SessionService>,
    revision: AtomicU64,
    queue_tx: mpsc::UnboundedSender<()>,
    queue_rx: tokio::sync::Mutex<mpsc::UnboundedReceiver<()>>,
}

impl ServerSnapshotPublisher {
    pub(crate) fn new(server_id: String, service: Arc<dyn SessionService>) -> Self {
        let (queue_tx, queue_rx) = mpsc::unbounded_channel();
        Self {
            server: Mutex::new(Weak::new()),
            server_id,
            service,
            revision: AtomicU64::new(0),
            queue_tx,
            queue_rx: tokio::sync::Mutex::new(queue_rx),
        }
    }

    pub(crate) fn attach_server(&self, server: Weak<ServerInner>) {
        *self.server.lock() = server;
    }

    fn server(&self) -> Option<Arc<ServerInner>> {
        self.server.lock().upgrade()
    }

    pub(crate) fn current_revision(&self) -> u64 {
        self.revision.load(Ordering::SeqCst)
    }

    pub(crate) async fn get(
        &self,
        models: Option<Vec<ModelMetadata>>,
    ) -> Result<ServerSnapshot, PiServerError> {
        let Some(server) = self.server() else {
            return Err(PiServerError::internal("PiServer has been dropped"));
        };
        // Read the revision *before* awaiting, as upstream's object literal
        // does: a snapshot taken while another broadcast is in flight must
        // report the revision it was started at, so the handshake can notice
        // the mismatch and send a catch-up event.
        let revision = self.current_revision();
        let sessions = server.sessions().list_metadata().await?;
        let models = match models {
            Some(models) => models,
            None => self.service.list_models().await?,
        };
        Ok(ServerSnapshot {
            server_id: self.server_id.clone(),
            protocol_version: PROTOCOL_VERSION,
            revision,
            sessions,
            models,
        })
    }

    /// Enqueues one broadcast. Fire and forget, like upstream's callers.
    pub(crate) fn broadcast(self: &Arc<Self>) {
        if self.queue_tx.send(()).is_err() {
            return;
        }
        let publisher = Arc::clone(self);
        tokio::spawn(async move {
            let mut receiver = publisher.queue_rx.lock().await;
            while receiver.try_recv().is_ok() {
                if let Err(error) = publisher.perform_broadcast().await {
                    if let Some(server) = publisher.server() {
                        server.report_service_error(error);
                    }
                }
            }
        });
    }

    async fn perform_broadcast(&self) -> Result<(), PiServerError> {
        let Some(server) = self.server() else {
            return Ok(());
        };
        let ready: Vec<Arc<ConnectionState>> = server
            .connections()
            .into_iter()
            .filter(|connection| {
                let inner = connection.inner.lock();
                inner.stage == ConnectionStage::Ready && !inner.disconnected
            })
            .collect();
        if ready.is_empty() || server.is_closing() {
            return Ok(());
        }
        let revision = self.revision.fetch_add(1, Ordering::SeqCst) + 1;
        let models = self.service.list_models().await?;
        let mut snapshot = self.get(Some(models)).await?;
        snapshot.revision = revision;
        let envelope = EventEnvelope {
            event: ServerEvent::ServerSnapshot(ServerSnapshotEvent { snapshot }),
        };
        for connection in ready {
            server.send_event(&connection, envelope.clone()).await;
        }
        Ok(())
    }
}
