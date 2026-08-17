//! Port of `.upstream/packages/server/src/testing/`.
//!
//! Shipped (not `#[cfg(test)]`) because upstream exports it: the same harness
//! backs this crate's tests and any downstream conformance suite.

pub mod client;
pub mod service;

pub use client::{connect_unix_test_client, ProtocolTestClient};
pub use service::{
    test_model, Deferred, ListModelsHook, ListSessionsHook, PromptOutcome, TestServerService,
    TestSessionRuntime,
};

use std::sync::Arc;

use crate::errors::TransportError;
use crate::server::PiServer;
use crate::types::{PiServerOptions, SessionService};

/// Port of `testing/server.ts`: an unstarted `PiServer` with a default service.
pub struct TestServer {
    pub server: PiServer,
    pub service: Arc<dyn SessionService>,
}

pub fn create_test_server(
    service: Option<Arc<dyn SessionService>>,
    options: PiServerOptions,
) -> Result<TestServer, TransportError> {
    let service = service.unwrap_or_else(|| TestServerService::new() as Arc<dyn SessionService>);
    Ok(TestServer {
        server: PiServer::new(Arc::clone(&service), options)?,
        service,
    })
}
