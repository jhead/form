//! `pi-client` against `pi-server` over a real temp Unix socket.
//!
//! This is the test that matters most for W14: both halves were ported from
//! upstream independently, and the only proof that they agree on the wire is
//! running them against each other end to end.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{start_harness, start_harness_with, Harness};
use parking_lot::Mutex;
use pi_client::{
    AcquireSessionOptions, ConnectionState, CreateSessionOptions, PiClient, PiClientError,
    PiClientOptions, PiSessionHandle, UnixTransportFactory, UnixTransportOptions,
};
use pi_protocol::{
    AssistantDelta, AssistantDeltaKind, ModelRef, ServerEvent, SessionPhase, SessionSnapshot,
    ThinkingLevel, TranscriptProgress, PROTOCOL_VERSION,
};
use pi_server::testing::TestServerService;
use pi_server::SessionRuntime;

async fn connect(harness: &Harness) -> PiClient {
    let factory = UnixTransportFactory::new(UnixTransportOptions::new(harness.socket_path()))
        .expect("transport options");
    PiClient::connect_new(PiClientOptions::new(factory))
        .await
        .expect("client connects")
}

/// Polls until `predicate` holds, so tests never depend on task scheduling.
async fn eventually(label: &str, mut predicate: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !predicate() {
        if tokio::time::Instant::now() > deadline {
            panic!("timed out waiting for {label}");
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[tokio::test]
async fn handshake_exchanges_a_server_snapshot() {
    let harness = start_harness().await;
    harness.service.seed("first");
    harness.service.seed("second");

    let factory = UnixTransportFactory::new(UnixTransportOptions::new(harness.socket_path()))
        .expect("transport options");
    let client = PiClient::new(PiClientOptions::new(factory)).expect("client");
    let snapshot = client.connect().await.expect("handshake");

    assert_eq!(snapshot.protocol_version, PROTOCOL_VERSION);
    assert_eq!(snapshot.server_id, harness.server.id());
    assert_eq!(
        snapshot
            .sessions
            .iter()
            .map(|session| session.id.as_str())
            .collect::<Vec<_>>(),
        vec!["first", "second"]
    );
    assert_eq!(snapshot.models.len(), 1);
    assert_eq!(client.connection_state(), ConnectionState::Connected);
    assert_eq!(client.snapshot().map(|snapshot| snapshot.revision), Some(0));

    client.dispose().await;
    harness.server.close().await;
}

#[tokio::test]
async fn lists_creates_and_drives_a_session_end_to_end() {
    let harness = start_harness().await;
    let client = connect(&harness).await;

    assert!(client.list_sessions().await.expect("list").is_empty());

    let handle = client
        .create_session(CreateSessionOptions {
            cwd: Some("/work".to_string()),
            name: Some("Created".to_string()),
            ..Default::default()
        })
        .await
        .expect("create");
    let created = handle.snapshot().expect("snapshot after create");
    assert_eq!(created.cwd, "/work");
    assert_eq!(created.name.as_deref(), Some("Created"));
    assert!(created.attached);
    assert!(created.locked);
    assert_eq!(
        Some(created.id.clone()),
        harness.service.last_created_id(),
        "the server assigns the durable id"
    );

    let listed = client.list_sessions().await.expect("list");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].session_name.as_deref(), Some("Created"));
    assert_eq!(listed[0].cwd.as_deref(), Some("/work"));

    // A prompt only resolves once the runtime finishes its turn, so drive it
    // from a second task while the first is still in flight.
    let session_id = created.id.clone();
    let runtime = harness.service.latest_runtime(&session_id);
    let prompting = {
        let handle = handle.clone();
        tokio::spawn(async move { handle.prompt("hello").await })
    };
    eventually("the runtime to enter its turn", || {
        runtime.phase() == SessionPhase::Turn
    })
    .await;

    let busy = handle
        .prompt("second")
        .await
        .expect_err("prompts are not queued");
    assert_eq!(
        busy.server_code(),
        Some(pi_protocol::ProtocolErrorCode::Busy)
    );

    handle.steer("adjust").await.expect("steer");
    assert_eq!(runtime.steers().len(), 1);

    handle.abort().await.expect("abort");
    let finished = prompting.await.expect("join").expect("prompt resolves");
    assert_eq!(finished.phase, SessionPhase::Idle);
    assert_eq!(finished.transcript.len(), 2);

    handle.detach().await.expect("detach");
    assert!(!handle.attached());
    assert_eq!(runtime.dispose_count(), 1);

    client.dispose().await;
    harness.server.close().await;
}

#[tokio::test]
async fn delivers_snapshots_and_progress_to_attached_clients_only() {
    let harness = start_harness().await;
    harness.service.seed("shared");
    let attached = connect(&harness).await;
    let unattached = connect(&harness).await;

    let observed_events: Arc<Mutex<Vec<ServerEvent>>> = Arc::default();
    let idle_events: Arc<Mutex<Vec<ServerEvent>>> = Arc::default();
    let sink = Arc::clone(&observed_events);
    let handle = attached.attach_session("shared").await.expect("attach");
    let _subscription = handle
        .on_event(Arc::new(move |event| sink.lock().push(event)))
        .expect("subscribe");
    let idle_sink = Arc::clone(&idle_events);
    let _idle_subscription = unattached
        .on_event(Arc::new(move |event| idle_sink.lock().push(event)))
        .expect("subscribe");

    let runtime = harness.service.latest_runtime("shared");
    let progress = TranscriptProgress::AssistantDelta(AssistantDelta {
        message_id: "assistant-1".to_string(),
        content_index: 0,
        kind: AssistantDeltaKind::Text,
        delta: "hello".to_string(),
    });
    runtime.emit_progress(progress.clone());

    eventually("a session_progress event", || {
        observed_events
            .lock()
            .iter()
            .any(|event| matches!(event, ServerEvent::SessionProgress(_)))
    })
    .await;
    let delivered = observed_events
        .lock()
        .iter()
        .find_map(|event| match event {
            ServerEvent::SessionProgress(event) => Some(event.clone()),
            _ => None,
        })
        .expect("progress event");
    assert_eq!(delivered.session_id, "shared");
    assert_eq!(delivered.progress, progress);

    assert!(
        !idle_events
            .lock()
            .iter()
            .any(|event| matches!(event, ServerEvent::SessionProgress(_))),
        "an unattached client must not observe session progress"
    );

    attached.dispose().await;
    unattached.dispose().await;
    harness.server.close().await;
}

#[tokio::test]
async fn two_clients_share_one_live_runtime() {
    let harness = start_harness().await;
    harness.service.seed("shared");
    let first = connect(&harness).await;
    let second = connect(&harness).await;

    let first_handle = first.attach_session("shared").await.expect("attach");
    let second_handle = second.attach_session("shared").await.expect("attach");
    assert_eq!(harness.service.runtime_count("shared"), 1);

    let observed: Arc<Mutex<Vec<SessionSnapshot>>> = Arc::default();
    let sink = Arc::clone(&observed);
    let _subscription = first_handle
        .subscribe(Arc::new(move |snapshot| sink.lock().push(snapshot)))
        .expect("subscribe");

    let updated = second_handle
        .set_model(ModelRef {
            provider: "test".to_string(),
            id: "large".to_string(),
        })
        .await
        .expect("set_model");
    assert_eq!(updated.model.id, "large");

    eventually("the first client to see the new model", || {
        observed
            .lock()
            .iter()
            .any(|snapshot| snapshot.model.id == "large")
    })
    .await;

    let thinking = first_handle
        .set_thinking(ThinkingLevel::High)
        .await
        .expect("set_thinking");
    assert_eq!(thinking.thinking_level, ThinkingLevel::High);

    first.dispose().await;
    second.dispose().await;
    harness.server.close().await;
}

#[tokio::test]
async fn shared_leases_detach_only_once_the_last_one_is_released() {
    let harness = start_harness().await;
    harness.service.seed("shared");
    let client = connect(&harness).await;

    let first = client.attach_session("shared").await.expect("attach");
    let second = client.attach_session("shared").await.expect("attach");
    let runtime = harness.service.latest_runtime("shared");

    first.detach().await.expect("detach");
    assert!(!first.attached());
    assert!(second.attached());
    assert_eq!(runtime.dispose_count(), 0);

    second.detach().await.expect("detach");
    assert!(!second.attached());
    eventually("the runtime to be disposed", || {
        runtime.dispose_count() == 1
    })
    .await;

    client.dispose().await;
    harness.server.close().await;
}

#[tokio::test]
async fn exclusive_leases_are_enforced_client_side() {
    let harness = start_harness().await;
    harness.service.seed("shared");
    let client = connect(&harness).await;

    let shared: PiSessionHandle = client
        .acquire_session("shared", AcquireSessionOptions::shared())
        .await
        .expect("shared lease");
    let conflict = client
        .acquire_session("shared", AcquireSessionOptions::exclusive())
        .await
        .expect_err("exclusive lease is refused");
    assert_eq!(conflict.code(), "session_ownership");
    shared.dispose().await.expect("dispose");

    let exclusive = client
        .acquire_session("shared", AcquireSessionOptions::exclusive())
        .await
        .expect("exclusive lease");
    let conflict = client
        .acquire_session("shared", AcquireSessionOptions::shared())
        .await
        .expect_err("shared lease is refused");
    assert_eq!(conflict.code(), "session_ownership");
    exclusive.dispose().await.expect("dispose");

    client.dispose().await;
    harness.server.close().await;
}

#[tokio::test]
async fn maps_server_errors_onto_typed_client_errors() {
    let service = TestServerService::new();
    service.seed("locked");
    service.lock_session("locked");
    let harness = start_harness_with(service, |options| options).await;
    let client = connect(&harness).await;

    let locked = client
        .attach_session("locked")
        .await
        .expect_err("the service holds the lock");
    assert_eq!(
        locked.server_code(),
        Some(pi_protocol::ProtocolErrorCode::SessionLocked)
    );

    let missing = client
        .attach_session("missing")
        .await
        .expect_err("unknown session");
    assert_eq!(
        missing.server_code(),
        Some(pi_protocol::ProtocolErrorCode::NotFound)
    );

    client.dispose().await;
    harness.server.close().await;
}

#[tokio::test]
async fn server_shutdown_disconnects_the_client() {
    let harness = start_harness().await;
    harness.service.seed("shared");
    let client = connect(&harness).await;
    let handle = client.attach_session("shared").await.expect("attach");

    let states: Arc<Mutex<Vec<ConnectionState>>> = Arc::default();
    let sink = Arc::clone(&states);
    let _subscription = client
        .on_connection_state_change(Arc::new(move |change| sink.lock().push(change.state)))
        .expect("subscribe");

    harness.server.close().await;

    eventually("the client to notice the shutdown", || {
        client.connection_state() == ConnectionState::Disconnected
    })
    .await;
    assert!(states.lock().contains(&ConnectionState::Disconnected));
    assert!(!handle.attached());

    let refused = client.list_sessions().await.expect_err("disconnected");
    assert!(matches!(refused, PiClientError::Disconnected(_)));
    client.dispose().await;
}

#[tokio::test]
async fn a_reconnecting_client_reattaches_to_a_restored_session() {
    let harness = start_harness().await;
    harness.service.seed("durable");
    let client = connect(&harness).await;

    let handle = client.attach_session("durable").await.expect("attach");
    handle
        .set_thinking(ThinkingLevel::High)
        .await
        .expect("set_thinking");
    handle.detach().await.expect("detach");

    let reattached = client.attach_session("durable").await.expect("reattach");
    let snapshot = reattached.snapshot().expect("snapshot");
    assert_eq!(snapshot.thinking_level, ThinkingLevel::High);
    assert_eq!(harness.service.runtime_count("durable"), 2);

    client.dispose().await;
    harness.server.close().await;
}

#[tokio::test]
async fn a_busy_session_survives_a_client_disconnect_and_disposes_when_idle() {
    let harness = start_harness().await;
    harness.service.seed("busy");
    let client = connect(&harness).await;
    let handle = client.attach_session("busy").await.expect("attach");

    let runtime = harness.service.latest_runtime("busy");
    let prompting = {
        let handle = handle.clone();
        tokio::spawn(async move { handle.prompt("survive").await })
    };
    eventually("the runtime to enter its turn", || {
        runtime.phase() == SessionPhase::Turn
    })
    .await;

    client.disconnect("test");
    assert!(prompting.await.expect("join").is_err());
    assert_eq!(runtime.dispose_count(), 0);

    runtime.finish_prompt();
    eventually("the idle runtime to be disposed", || {
        runtime.dispose_count() == 1
    })
    .await;

    let reconnected = connect(&harness).await;
    let handle = reconnected.attach_session("busy").await.expect("attach");
    let snapshot = handle.snapshot().expect("snapshot");
    assert_eq!(snapshot.transcript.len(), 2);

    client.dispose().await;
    reconnected.dispose().await;
    harness.server.close().await;
}
