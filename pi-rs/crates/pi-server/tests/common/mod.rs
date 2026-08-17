//! Shared fixtures for the `pi-server` test suite.

#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::Arc;

use pi_server::testing::TestServerService;
use pi_server::unix::{create_unix_server, UnixServerOptions};
use pi_server::{PiServer, ServerErrorReport, SessionService};
use tempfile::TempDir;

/// A started Unix server plus the temp directory that owns its socket.
pub struct Harness {
    pub server: PiServer,
    pub service: Arc<TestServerService>,
    pub errors: Arc<parking_lot::Mutex<Vec<ServerErrorReport>>>,
    _directory: TempDir,
}

impl Harness {
    pub fn socket_path(&self) -> String {
        self.server
            .addresses()
            .first()
            .cloned()
            .expect("the server is bound")
    }
}

pub fn socket_dir() -> TempDir {
    // Sockets are bound in the temp dir; macOS' default TMPDIR is long enough
    // that the 103-byte sockaddr_un budget matters, so keep names short.
    tempfile::Builder::new()
        .prefix("pis")
        .tempdir_in("/tmp")
        .expect("temp dir")
}

pub async fn start_harness() -> Harness {
    start_harness_with(TestServerService::new(), |options| options).await
}

pub async fn start_harness_with(
    service: Arc<TestServerService>,
    configure: impl FnOnce(UnixServerOptions) -> UnixServerOptions,
) -> Harness {
    let directory = socket_dir();
    let path: PathBuf = directory.path().join("s.sock");
    let errors: Arc<parking_lot::Mutex<Vec<ServerErrorReport>>> = Arc::default();
    let sink = Arc::clone(&errors);
    let options = configure(
        UnixServerOptions::new(path)
            .with_error_handler(Arc::new(move |report| sink.lock().push(report))),
    );
    let server = create_unix_server(Arc::clone(&service) as Arc<dyn SessionService>, options)
        .expect("server options");
    server.start().await.expect("server starts");
    Harness {
        server,
        service,
        errors,
        _directory: directory,
    }
}
