//! Port of `.upstream/packages/client/test/state.test.ts`.

mod support;

use std::sync::{Arc, Mutex};

use pi_protocol::{
    AssistantDelta, AssistantDeltaKind, CommandResult, ResponseEnvelope, ServerEvent,
    ServerMessage, SessionMetadata, SessionPhase, SessionResult, SessionSnapshot,
    SessionSnapshotEvent, ThinkingLevel, TranscriptProgress,
};
use support::{
    attach_session, base_server_snapshot, collect_requests, connect_client, find_request,
    make_server, session_snapshot,
};

fn snapshot_event(snapshot: SessionSnapshot) -> ServerMessage {
    ServerMessage::Event(pi_protocol::EventEnvelope {
        event: ServerEvent::SessionSnapshot(SessionSnapshotEvent { snapshot }),
    })
}

#[tokio::test]
async fn reduces_only_authoritative_snapshots_and_supports_unsubscribe() {
    let server = make_server();
    let client = connect_client(&server).await;
    let requests = collect_requests(&server);
    let mut initial = session_snapshot("session-1");
    initial.revision = 1;
    let handle = attach_session(&client, &server, initial.clone()).await;

    let observed: Arc<Mutex<Vec<u64>>> = Arc::default();
    let progress_types: Arc<Mutex<Vec<String>>> = Arc::default();
    let sink = Arc::clone(&observed);
    let unsubscribe = handle
        .subscribe(Arc::new(move |snapshot: SessionSnapshot| {
            sink.lock().unwrap().push(snapshot.revision)
        }))
        .expect("subscribe");
    let event_sink = Arc::clone(&progress_types);
    let unsubscribe_events = handle
        .on_event(Arc::new(move |event: ServerEvent| {
            event_sink.lock().unwrap().push(
                match event {
                    ServerEvent::ServerSnapshot(_) => "server_snapshot",
                    ServerEvent::SessionSnapshot(_) => "session_snapshot",
                    ServerEvent::SessionProgress(_) => "session_progress",
                    ServerEvent::SessionRemoved(_) => "session_removed",
                }
                .to_string(),
            )
        }))
        .expect("subscribe");

    server.send(&ServerMessage::Event(pi_protocol::EventEnvelope {
        event: ServerEvent::SessionProgress(pi_protocol::SessionProgressEvent {
            session_id: "session-1".to_string(),
            progress: TranscriptProgress::AssistantDelta(AssistantDelta {
                message_id: "assistant-1".to_string(),
                content_index: 0,
                kind: AssistantDeltaKind::Text,
                delta: "hi".to_string(),
            }),
        }),
    }));
    assert_eq!(*progress_types.lock().unwrap(), vec!["session_progress"]);
    assert_eq!(handle.snapshot(), Some(initial.clone()));

    let prompting = {
        let handle = handle.clone();
        tokio::spawn(async move { handle.prompt("hello").await })
    };
    while requests
        .lock()
        .unwrap()
        .iter()
        .all(|request| request.request.name() != "prompt")
    {
        tokio::task::yield_now().await;
    }
    assert_eq!(handle.snapshot(), Some(initial));

    let prompt_request = find_request(&requests, "prompt");
    let mut updated = session_snapshot("session-1");
    updated.revision = 2;
    updated.phase = SessionPhase::Turn;
    server.send(&ServerMessage::Response(ResponseEnvelope::ok(
        prompt_request.id,
        CommandResult::Prompt(SessionResult {
            session: updated.clone(),
        }),
    )));
    assert_eq!(prompting.await.expect("join").expect("prompt"), updated);
    assert_eq!(handle.snapshot(), Some(updated));
    assert_eq!(*observed.lock().unwrap(), vec![2]);

    unsubscribe.unsubscribe();
    unsubscribe_events.unsubscribe();
    let mut later = session_snapshot("session-1");
    later.revision = 3;
    server.send(&snapshot_event(later));
    assert_eq!(*observed.lock().unwrap(), vec![2]);
}

#[tokio::test]
async fn keeps_session_leases_attached_across_server_metadata_snapshots() {
    let server = make_server();
    let client = connect_client(&server).await;
    let handle = attach_session(&client, &server, session_snapshot("session-1")).await;

    let mut snapshot = base_server_snapshot();
    snapshot.revision = 2;
    snapshot.sessions = vec![SessionMetadata {
        id: "session-1".to_string(),
        created_at: 1,
        updated_at: None,
        parent_session_id: None,
        session_name: Some("Named session".to_string()),
        cwd: None,
    }];
    server.send(&ServerMessage::Event(pi_protocol::EventEnvelope {
        event: ServerEvent::ServerSnapshot(pi_protocol::ServerSnapshotEvent { snapshot }),
    }));

    assert!(handle.attached());
}

#[tokio::test]
async fn a_delayed_command_response_cannot_replace_a_newer_event_snapshot() {
    let server = make_server();
    let client = connect_client(&server).await;
    let mut initial = session_snapshot("session-1");
    initial.revision = 1;
    let handle = attach_session(&client, &server, initial).await;
    let requests = collect_requests(&server);

    let changing = {
        let handle = handle.clone();
        tokio::spawn(async move { handle.set_thinking(ThinkingLevel::High).await })
    };
    while requests
        .lock()
        .unwrap()
        .iter()
        .all(|request| request.request.name() != "set_thinking")
    {
        tokio::task::yield_now().await;
    }
    let request = find_request(&requests, "set_thinking");

    let mut newer = session_snapshot("session-1");
    newer.revision = 3;
    newer.thinking_level = ThinkingLevel::High;
    server.send(&snapshot_event(newer));

    let mut stale = session_snapshot("session-1");
    stale.revision = 2;
    stale.thinking_level = ThinkingLevel::Medium;
    server.send(&ServerMessage::Response(ResponseEnvelope::ok(
        request.id,
        CommandResult::SetThinking(SessionResult { session: stale }),
    )));

    changing.await.expect("join").expect("set_thinking");
    let snapshot = handle.snapshot().expect("snapshot");
    assert_eq!(snapshot.revision, 3);
    assert_eq!(snapshot.thinking_level, ThinkingLevel::High);
}

#[tokio::test]
async fn an_attach_response_cannot_replace_a_newer_snapshot_from_the_reacquired_runtime() {
    let server = make_server();
    let client = connect_client(&server).await;

    let mut existing = session_snapshot("session-1");
    existing.revision = 10;
    existing.attached = false;
    server.send(&snapshot_event(existing));

    server.on_message(|server, message| {
        let pi_protocol::ClientMessage::Request(envelope) = message else {
            return;
        };
        if envelope.request.name() != "attach" {
            return;
        }
        let mut newer = session_snapshot("session-1");
        newer.revision = 3;
        newer.thinking_level = ThinkingLevel::High;
        server.send(&snapshot_event(newer));
        let mut stale = session_snapshot("session-1");
        stale.revision = 2;
        stale.thinking_level = ThinkingLevel::Medium;
        server.send(&ServerMessage::Response(ResponseEnvelope::ok(
            envelope.id,
            CommandResult::Attach(SessionResult { session: stale }),
        )));
    });

    let handle = client.attach_session("session-1").await.expect("attach");
    let snapshot = handle.snapshot().expect("snapshot");
    assert_eq!(snapshot.revision, 3);
    assert_eq!(snapshot.thinking_level, ThinkingLevel::High);
}
