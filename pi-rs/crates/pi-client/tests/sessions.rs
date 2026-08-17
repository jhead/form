//! Port of `.upstream/packages/client/test/sessions.test.ts`.
//!
//! The lease rules are the client's most intricate behaviour: shared leases are
//! reference counted, an exclusive lease excludes everything, a failed detach
//! restores the lease, and a reacquisition queues behind the detach in flight.

mod support;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use pi_client::{AcquireSessionOptions, PiClientError};
use pi_protocol::{
    ClientMessage, CommandResult, DetachResult, ProtocolError, ProtocolErrorCode, ResponseEnvelope,
    ServerMessage, SessionResult,
};
use support::{collect_requests, connect_client, make_server, session_snapshot, MemoryByteServer};

/// Answers `attach` and `detach` immediately, like most upstream cases.
fn auto_attach_detach(server: &Arc<MemoryByteServer>) {
    server.on_message(|server, message| {
        let ClientMessage::Request(envelope) = message else {
            return;
        };
        match &envelope.request {
            pi_protocol::Command::Attach(attach) => {
                server.send(&ServerMessage::Response(ResponseEnvelope::ok(
                    envelope.id.clone(),
                    CommandResult::Attach(SessionResult {
                        session: session_snapshot(&attach.session_id),
                    }),
                )));
            }
            pi_protocol::Command::Detach(detach) => {
                server.send(&ServerMessage::Response(ResponseEnvelope::ok(
                    envelope.id.clone(),
                    CommandResult::Detach(DetachResult {
                        session_id: detach.session_id.clone(),
                    }),
                )));
            }
            _ => {}
        }
    });
}

async fn wait_for(label: &str, mut predicate: impl FnMut() -> bool) {
    for _ in 0..10_000 {
        if predicate() {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("timed out waiting for {label}");
}

#[tokio::test]
async fn keeps_multiple_session_handles_independent_and_enforces_detach() {
    let server = make_server();
    let client = connect_client(&server).await;
    auto_attach_detach(&server);

    let first = client.attach_session("session-1").await.expect("attach");
    let second = client.attach_session("session-2").await.expect("attach");
    assert!(first.attached());
    assert!(second.attached());

    first.detach().await.expect("detach");
    assert!(!first.attached());
    assert!(second.attached());
    assert_eq!(
        first.abort().await.expect_err("detached"),
        PiClientError::SessionDetached {
            session_id: "session-1".to_string()
        }
    );
}

#[tokio::test]
async fn detaches_a_shared_session_only_after_its_final_lease_is_released() {
    let server = make_server();
    let client = connect_client(&server).await;
    let commands: Arc<Mutex<Vec<String>>> = Arc::default();
    let sink = Arc::clone(&commands);
    server.on_message(move |_, message| {
        if let ClientMessage::Request(envelope) = message {
            sink.lock()
                .unwrap()
                .push(envelope.request.name().to_string());
        }
    });
    auto_attach_detach(&server);

    let first = client.attach_session("session-1").await.expect("attach");
    let second = client.attach_session("session-1").await.expect("attach");
    assert_eq!(*commands.lock().unwrap(), vec!["attach"]);

    first.detach().await.expect("detach");
    assert!(!first.attached());
    assert!(second.attached());
    assert_eq!(*commands.lock().unwrap(), vec!["attach"]);

    second.detach().await.expect("detach");
    assert!(!second.attached());
    assert_eq!(*commands.lock().unwrap(), vec!["attach", "detach"]);
}

#[tokio::test]
async fn enforces_exclusive_and_shared_lease_modes() {
    let server = make_server();
    let client = connect_client(&server).await;
    auto_attach_detach(&server);

    let shared = client
        .acquire_session("session-1", AcquireSessionOptions::shared())
        .await
        .expect("shared");
    let refused = client
        .acquire_session("session-1", AcquireSessionOptions::exclusive())
        .await
        .expect_err("exclusive is refused");
    assert!(matches!(refused, PiClientError::SessionOwnership { .. }));
    shared.dispose().await.expect("dispose");

    let exclusive = client
        .acquire_session("session-1", AcquireSessionOptions::exclusive())
        .await
        .expect("exclusive");
    let refused = client
        .acquire_session("session-1", AcquireSessionOptions::shared())
        .await
        .expect_err("shared is refused");
    assert!(matches!(refused, PiClientError::SessionOwnership { .. }));
    exclusive.dispose().await.expect("dispose");
}

#[tokio::test]
async fn invalidated_leases_dispose_without_protocol_cleanup() {
    let server = make_server();
    let client = connect_client(&server).await;
    let detaches = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&detaches);
    server.on_message(move |server, message| {
        let ClientMessage::Request(envelope) = message else {
            return;
        };
        match &envelope.request {
            pi_protocol::Command::Attach(attach) => {
                server.send(&ServerMessage::Response(ResponseEnvelope::ok(
                    envelope.id.clone(),
                    CommandResult::Attach(SessionResult {
                        session: session_snapshot(&attach.session_id),
                    }),
                )));
            }
            pi_protocol::Command::Detach(_) => {
                counter.fetch_add(1, Ordering::SeqCst);
            }
            _ => {}
        }
    });

    let lease = client
        .acquire_session("session-1", AcquireSessionOptions::exclusive())
        .await
        .expect("exclusive");

    client.disconnect("test");

    lease.dispose().await.expect("dispose resolves");
    assert!(!lease.active());
    assert_eq!(detaches.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn rejects_commands_while_releasing_and_restores_an_explicit_detach_after_failure() {
    let server = make_server();
    let client = connect_client(&server).await;
    let requests = collect_requests(&server);
    server.on_message(|server, message| {
        if let ClientMessage::Request(envelope) = message {
            if let pi_protocol::Command::Attach(attach) = &envelope.request {
                server.send(&ServerMessage::Response(ResponseEnvelope::ok(
                    envelope.id.clone(),
                    CommandResult::Attach(SessionResult {
                        session: session_snapshot(&attach.session_id),
                    }),
                )));
            }
        }
    });

    let lease = client
        .acquire_session("session-1", AcquireSessionOptions::exclusive())
        .await
        .expect("exclusive");

    let first_detach = {
        let lease = lease.clone();
        tokio::spawn(async move { lease.detach().await })
    };
    let requests_for_wait = Arc::clone(&requests);
    wait_for("the first detach request", move || {
        requests_for_wait
            .lock()
            .unwrap()
            .iter()
            .any(|request| request.request.name() == "detach")
    })
    .await;

    // The lease is releasing, so commands are refused.
    assert_eq!(
        lease.abort().await.expect_err("releasing"),
        PiClientError::SessionDetached {
            session_id: "session-1".to_string()
        }
    );

    let failed = support::last_request(&requests);
    server.send(&ServerMessage::Response(ResponseEnvelope::failed(
        failed.id,
        ProtocolError::new(ProtocolErrorCode::InvalidRequest, "retry"),
    )));
    let error = first_detach
        .await
        .expect("join")
        .expect_err("detach failed");
    assert_eq!(error.to_string(), "retry");
    assert!(lease.active(), "an explicit detach restores the lease");

    let second_detach = {
        let lease = lease.clone();
        tokio::spawn(async move { lease.detach().await })
    };
    let requests_for_wait = Arc::clone(&requests);
    wait_for("the retry detach request", move || {
        requests_for_wait
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.request.name() == "detach")
            .count()
            == 2
    })
    .await;
    let retry = support::last_request(&requests);
    server.send(&ServerMessage::Response(ResponseEnvelope::ok(
        retry.id,
        CommandResult::Detach(DetachResult {
            session_id: "session-1".to_string(),
        }),
    )));
    second_detach.await.expect("join").expect("detach");
    assert!(!lease.active());
}

#[tokio::test]
async fn serializes_reacquisition_behind_final_lease_detachment() {
    let server = make_server();
    let client = connect_client(&server).await;
    let requests = collect_requests(&server);
    server.on_message(|server, message| {
        if let ClientMessage::Request(envelope) = message {
            if let pi_protocol::Command::Attach(attach) = &envelope.request {
                server.send(&ServerMessage::Response(ResponseEnvelope::ok(
                    envelope.id.clone(),
                    CommandResult::Attach(SessionResult {
                        session: session_snapshot(&attach.session_id),
                    }),
                )));
            }
        }
    });

    let first = client.attach_session("session-1").await.expect("attach");
    let detaching = {
        let first = first.clone();
        tokio::spawn(async move { first.detach().await })
    };
    let requests_for_wait = Arc::clone(&requests);
    wait_for("the detach request", move || {
        requests_for_wait
            .lock()
            .unwrap()
            .iter()
            .any(|request| request.request.name() == "detach")
    })
    .await;
    let detach_request = support::last_request(&requests);

    let reacquiring = {
        let client = client.clone();
        tokio::spawn(async move { client.attach_session("session-1").await })
    };
    tokio::task::yield_now().await;
    assert_eq!(
        requests
            .lock()
            .unwrap()
            .iter()
            .map(|request| request.request.name())
            .collect::<Vec<_>>(),
        vec!["attach", "detach"],
        "the reacquisition waits for the detach in flight"
    );

    server.send(&ServerMessage::Response(ResponseEnvelope::ok(
        detach_request.id,
        CommandResult::Detach(DetachResult {
            session_id: "session-1".to_string(),
        }),
    )));
    detaching.await.expect("join").expect("detach");

    let handle = reacquiring.await.expect("join").expect("reattach");
    assert!(handle.attached());
    assert_eq!(
        requests
            .lock()
            .unwrap()
            .iter()
            .map(|request| request.request.name())
            .collect::<Vec<_>>(),
        vec!["attach", "detach", "attach"]
    );
}

#[tokio::test]
async fn accepts_a_lower_revision_after_detaching_and_reacquiring() {
    let server = make_server();
    let client = connect_client(&server).await;
    let attach_count = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&attach_count);
    server.on_message(move |server, message| {
        let ClientMessage::Request(envelope) = message else {
            return;
        };
        match &envelope.request {
            pi_protocol::Command::Attach(attach) => {
                let mut snapshot = session_snapshot(&attach.session_id);
                snapshot.revision = if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                    10
                } else {
                    0
                };
                server.send(&ServerMessage::Response(ResponseEnvelope::ok(
                    envelope.id.clone(),
                    CommandResult::Attach(SessionResult { session: snapshot }),
                )));
            }
            pi_protocol::Command::Detach(detach) => {
                server.send(&ServerMessage::Response(ResponseEnvelope::ok(
                    envelope.id.clone(),
                    CommandResult::Detach(DetachResult {
                        session_id: detach.session_id.clone(),
                    }),
                )));
            }
            _ => {}
        }
    });

    let first = client.attach_session("session-1").await.expect("attach");
    assert_eq!(first.snapshot().map(|snapshot| snapshot.revision), Some(10));
    first.detach().await.expect("detach");

    let reopened = client.attach_session("session-1").await.expect("reattach");
    assert_eq!(
        reopened.snapshot().map(|snapshot| snapshot.revision),
        Some(0)
    );
}
