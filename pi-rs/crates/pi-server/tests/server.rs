//! Ports of `.upstream/packages/server/test/{listener,server}.test.ts` plus the
//! server-restart case from `sessions.test.ts`.

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use common::{socket_dir, start_harness_with};
use parking_lot::Mutex;
use pi_protocol::{
    Command, CommandResult, ProtocolErrorCode, ServerMessage, ServerMessageDecoder, SessionCommand,
    ThinkingLevel,
};
use pi_server::connection::{ByteConnection, ByteConnectionAcceptor, CloseFuture, SendFuture};
use pi_server::testing::{
    connect_unix_test_client, create_test_server, TestServerService, TestSessionRuntime,
};
use pi_server::unix::{create_unix_server, UnixServerOptions};
use pi_server::{
    PiServer, PiServerListener, PiServerOptions, SessionRuntime, SessionService, TransportError,
};

// ---------------------------------------------------------------------------
// listener composition
// ---------------------------------------------------------------------------

struct TestListener {
    address: Mutex<Option<String>>,
    accepted: Mutex<Option<Arc<dyn ByteConnectionAcceptor>>>,
    start_count: AtomicUsize,
    close_count: AtomicUsize,
    start_error: Option<TransportError>,
}

impl TestListener {
    fn new(address: &str, start_error: Option<TransportError>) -> Arc<Self> {
        Arc::new(Self {
            address: Mutex::new(Some(address.to_string())),
            accepted: Mutex::new(None),
            start_count: AtomicUsize::new(0),
            close_count: AtomicUsize::new(0),
            start_error,
        })
    }
}

#[async_trait]
impl PiServerListener for TestListener {
    fn address(&self) -> Option<String> {
        self.address.lock().clone()
    }

    async fn start(&self, accept: Arc<dyn ByteConnectionAcceptor>) -> Result<(), TransportError> {
        self.start_count.fetch_add(1, Ordering::SeqCst);
        *self.accepted.lock() = Some(accept);
        match &self.start_error {
            Some(error) => Err(error.clone()),
            None => Ok(()),
        }
    }

    async fn close(&self) -> Result<(), TransportError> {
        self.close_count.fetch_add(1, Ordering::SeqCst);
        *self.address.lock() = None;
        Ok(())
    }
}

#[tokio::test]
async fn starts_and_closes_every_configured_listener() {
    let first = TestListener::new("first", None);
    let second = TestListener::new("second", None);
    let harness = create_test_server(
        None,
        PiServerOptions::new(vec![
            Arc::clone(&first) as Arc<dyn PiServerListener>,
            Arc::clone(&second) as Arc<dyn PiServerListener>,
        ]),
    )
    .expect("server");

    harness.server.start().await.expect("start");
    assert_eq!(harness.server.addresses(), vec!["first", "second"]);
    assert!(first.accepted.lock().is_some());
    assert!(second.accepted.lock().is_some());

    harness.server.close().await;
    assert_eq!(first.close_count.load(Ordering::SeqCst), 1);
    assert_eq!(second.close_count.load(Ordering::SeqCst), 1);
    assert!(harness.server.addresses().is_empty());
}

#[tokio::test]
async fn closes_previously_started_listeners_when_startup_fails() {
    let first = TestListener::new("first", None);
    let failure = TransportError::new("listener failed");
    let second = TestListener::new("second", Some(failure.clone()));
    let harness = create_test_server(
        None,
        PiServerOptions::new(vec![
            Arc::clone(&first) as Arc<dyn PiServerListener>,
            Arc::clone(&second) as Arc<dyn PiServerListener>,
        ]),
    )
    .expect("server");

    assert_eq!(harness.server.start().await.expect_err("fails"), failure);
    assert_eq!(first.close_count.load(Ordering::SeqCst), 1);
    assert_eq!(second.close_count.load(Ordering::SeqCst), 0);
}

// ---------------------------------------------------------------------------
// option validation
// ---------------------------------------------------------------------------

#[test]
fn rejects_unix_socket_paths_that_cannot_fit_in_sockaddr_un() {
    let service = TestServerService::new() as Arc<dyn SessionService>;
    let error = create_unix_server(
        service,
        UnixServerOptions::new(format!("/tmp/{}", "x".repeat(512))),
    )
    .expect_err("path is too long");
    assert!(error.to_string().contains("too long"));
}

#[test]
fn rejects_an_empty_server_id() {
    let service = TestServerService::new() as Arc<dyn SessionService>;
    let error = PiServer::new(service, PiServerOptions::new(Vec::new()).with_server_id(""))
        .expect_err("empty id");
    assert!(error.to_string().contains("serverId"));
}

#[test]
fn rejects_pending_byte_limits_smaller_than_one_maximum_frame() {
    let service = TestServerService::new() as Arc<dyn SessionService>;
    let error = create_unix_server(
        service,
        UnixServerOptions::new("/tmp/pi-server-limit-test.sock")
            .with_max_frame_length(128)
            .with_max_pending_bytes(131),
    )
    .expect_err("budget is too small");
    assert!(error.to_string().contains("maxPendingBytes"));
}

#[test]
fn rejects_out_of_range_timeouts() {
    let service = TestServerService::new() as Arc<dyn SessionService>;
    let error = create_unix_server(
        service.clone(),
        UnixServerOptions::new("/tmp/pi-server-timeout-test.sock")
            .with_graceful_close_timeout_ms(2_147_483_648),
    )
    .expect_err("timeout is too large");
    assert!(error.to_string().contains("gracefulCloseTimeoutMs"));

    let error = PiServer::new(
        service,
        PiServerOptions::new(Vec::new()).with_handshake_timeout_ms(0),
    )
    .expect_err("timeout is zero");
    assert!(error.to_string().contains("handshakeTimeoutMs"));
}

#[tokio::test]
async fn rejects_an_overlong_derived_private_unix_bind_path() {
    let max_length = if cfg!(target_os = "linux") { 107 } else { 103 };
    let suffix_length = "/tmp//s".len();
    let path = format!("/tmp/{}/s", "x".repeat(max_length - suffix_length));
    let server = create_unix_server(
        TestServerService::new() as Arc<dyn SessionService>,
        UnixServerOptions::new(path),
    )
    .expect("options are valid; the derived path is not");
    let error = server.start().await.expect_err("derived path is too long");
    assert!(
        error.to_string().contains("private Unix bind path")
            && error.to_string().contains("too long"),
        "unexpected error: {error}"
    );
}

#[tokio::test]
async fn rejects_concurrent_start_calls_without_leaking_the_unix_listener() {
    let directory = socket_dir();
    let path = directory.path().join("s.sock");
    let server = create_unix_server(
        TestServerService::new() as Arc<dyn SessionService>,
        UnixServerOptions::new(&path),
    )
    .expect("server");

    let first = {
        let server = server.clone();
        tokio::spawn(async move { server.start().await })
    };
    // The concurrent call is refused; whichever one loses the race reports it.
    let second = server.start().await;
    let first = first.await.expect("join");
    let refusal = match (&first, &second) {
        (Ok(()), Err(error)) | (Err(error), Ok(())) => error.to_string(),
        other => panic!("expected exactly one refusal, got {other:?}"),
    };
    assert!(
        refusal.contains("starting") || refusal.contains("started"),
        "unexpected refusal: {refusal}"
    );

    server.close().await;
    assert!(server.addresses().is_empty());
    assert!(!path.exists());
}

// ---------------------------------------------------------------------------
// handshake timeout against a blocked output queue
// ---------------------------------------------------------------------------

struct BlockedConnection {
    closed: Arc<Mutex<bool>>,
    final_chunk: Arc<Mutex<Option<Vec<u8>>>>,
}

impl ByteConnection for BlockedConnection {
    fn closed(&self) -> bool {
        *self.closed.lock()
    }

    fn send(&self, _chunk: Vec<u8>) -> SendFuture {
        // Never resolves, like upstream's blocked socket.
        Box::pin(std::future::pending())
    }

    fn close(&self, final_chunk: Option<Vec<u8>>) -> CloseFuture {
        *self.final_chunk.lock() = final_chunk;
        *self.closed.lock() = true;
        Box::pin(async { Ok(()) })
    }
}

#[tokio::test]
async fn handshake_timeout_cleanup_does_not_wait_for_a_blocked_output_queue() {
    let harness = create_test_server(
        None,
        PiServerOptions::new(Vec::new())
            .with_max_frame_length(1024)
            .with_handshake_timeout_ms(10),
    )
    .expect("server");
    let closed = Arc::new(Mutex::new(false));
    let final_chunk: Arc<Mutex<Option<Vec<u8>>>> = Arc::default();
    let connection = Arc::new(BlockedConnection {
        closed: Arc::clone(&closed),
        final_chunk: Arc::clone(&final_chunk),
    });
    harness.server.accept(connection as Arc<dyn ByteConnection>);

    for _ in 0..200 {
        if *closed.lock() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert!(*closed.lock(), "the connection was torn down");
    let frame = final_chunk
        .lock()
        .clone()
        .expect("a final frame was queued");
    let messages = ServerMessageDecoder::default()
        .push(&frame)
        .expect("the final frame decodes");
    assert!(matches!(
        messages.as_slice(),
        [ServerMessage::HelloError(error)]
            if error.error.code == ProtocolErrorCode::InvalidRequest
    ));
    harness.server.close().await;
}

// ---------------------------------------------------------------------------
// restart
// ---------------------------------------------------------------------------

#[tokio::test]
async fn restores_persisted_sessions_lazily_after_a_server_restart() {
    let service = TestServerService::new();
    service.seed("session-1");
    let first = start_harness_with(Arc::clone(&service), |options| options).await;
    let first_client = connect_unix_test_client(first.socket_path())
        .await
        .expect("connect");
    first_client.hello().await;
    first_client
        .request(Command::Attach(SessionCommand::new("session-1")))
        .await;
    first_client
        .request(Command::SetThinking(pi_protocol::SetThinkingCommand {
            session_id: "session-1".to_string(),
            thinking_level: ThinkingLevel::High,
        }))
        .await;
    first_client.close().await;
    first.server.close().await;
    assert_eq!(service.runtime_count("session-1"), 1);

    let second = start_harness_with(Arc::clone(&service), |options| options).await;
    let second_client = connect_unix_test_client(second.socket_path())
        .await
        .expect("connect");
    second_client.hello().await;
    let response = second_client
        .request(Command::Attach(SessionCommand::new("session-1")))
        .await;
    let Some(CommandResult::Attach(result)) = response.result else {
        panic!("attach failed: {:?}", response.error);
    };
    assert_eq!(result.session.thinking_level, ThinkingLevel::High);
    assert_eq!(service.runtime_count("session-1"), 2);

    second.server.close().await;
}

/// A runtime that stays busy so the "keep it alive after disconnect" branch of
/// `maybe_dispose` is exercised through the public API.
#[tokio::test]
async fn a_terminating_runtime_refuses_reacquisition_until_it_is_gone() {
    let service = TestServerService::new();
    service.seed("session-1");
    let harness = start_harness_with(Arc::clone(&service), |options| options).await;
    let client = connect_unix_test_client(harness.socket_path())
        .await
        .expect("connect");
    client.hello().await;
    client
        .request(Command::Attach(SessionCommand::new("session-1")))
        .await;
    let runtime: Arc<TestSessionRuntime> = service.latest_runtime("session-1");
    assert_eq!(runtime.phase(), pi_protocol::SessionPhase::Idle);

    client
        .request(Command::Detach(SessionCommand::new("session-1")))
        .await;
    runtime.disposed.wait().await;
    assert_eq!(runtime.dispose_count(), 1);

    // The next attach acquires a fresh runtime rather than the disposed one.
    let response = client
        .request(Command::Attach(SessionCommand::new("session-1")))
        .await;
    assert!(response.ok);
    assert_eq!(service.runtime_count("session-1"), 2);

    harness.server.close().await;
}
