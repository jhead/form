//! Port of `.upstream/packages/server/test/sessions.test.ts`.

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use common::{start_harness, start_harness_with, Harness};
use pi_protocol::{
    Command, CommandResult, CreateCommand, ListCommand, ModelRef, PromptCommand, ProtocolErrorCode,
    ServerEvent, ServerMessage, SessionCommand, SessionMetadata, SessionPhase, SessionSnapshot,
    SetModelCommand, SetThinkingCommand, ThinkingLevel, TranscriptProgress,
};
use pi_server::testing::{
    connect_unix_test_client, Deferred, ProtocolTestClient, TestServerService,
};
use pi_server::SessionRuntime;

async fn connect(harness: &Harness) -> Arc<ProtocolTestClient> {
    let client = connect_unix_test_client(harness.socket_path())
        .await
        .expect("wire client connects");
    client.hello().await;
    client
}

async fn attach(client: &ProtocolTestClient, session_id: &str) -> SessionSnapshot {
    let response = client
        .request(Command::Attach(SessionCommand::new(session_id)))
        .await;
    match response.result {
        Some(CommandResult::Attach(result)) => result.session,
        other => panic!("attach failed: {other:?} / {:?}", response.error),
    }
}

fn session_result(result: Option<CommandResult>) -> SessionSnapshot {
    match result {
        Some(
            CommandResult::Create(result)
            | CommandResult::Attach(result)
            | CommandResult::Prompt(result)
            | CommandResult::Steer(result)
            | CommandResult::Abort(result)
            | CommandResult::SetModel(result)
            | CommandResult::SetThinking(result),
        ) => result.session,
        other => panic!("expected a session result, got {other:?}"),
    }
}

fn list_result(result: Option<CommandResult>) -> Vec<SessionMetadata> {
    match result {
        Some(CommandResult::List(result)) => result.sessions,
        other => panic!("expected a list result, got {other:?}"),
    }
}

#[tokio::test]
async fn serializes_server_snapshot_revisions() {
    let service = TestServerService::new();
    let first_started = Arc::new(Deferred::<()>::new());
    let second_started = Arc::new(Deferred::<()>::new());
    let first_release = Arc::new(Deferred::<()>::new());
    let second_release = Arc::new(Deferred::<()>::new());
    let controlled = Arc::new(parking_lot::Mutex::new(false));
    let started_count = Arc::new(AtomicUsize::new(0));
    {
        let (fs, ss) = (Arc::clone(&first_started), Arc::clone(&second_started));
        let (fr, sr) = (Arc::clone(&first_release), Arc::clone(&second_release));
        let controlled = Arc::clone(&controlled);
        let started_count = Arc::clone(&started_count);
        service.set_list_models_hook(Arc::new(move || {
            if !*controlled.lock() {
                return Box::pin(async { Ok(()) });
            }
            let index = started_count.fetch_add(1, Ordering::SeqCst) + 1;
            let (started, release) = match index {
                1 => (Some(Arc::clone(&fs)), Some(Arc::clone(&fr))),
                2 => (Some(Arc::clone(&ss)), Some(Arc::clone(&sr))),
                _ => (None, None),
            };
            Box::pin(async move {
                if let (Some(started), Some(release)) = (started, release) {
                    started.resolve(());
                    release.wait().await;
                }
                Ok(())
            })
        }));
    }
    let harness = start_harness_with(Arc::clone(&service), |options| options).await;
    let client = connect(&harness).await;
    *controlled.lock() = true;
    let message_index = client.message_count();

    let first_create = {
        let client = Arc::clone(&client);
        tokio::spawn(async move {
            client
                .request(Command::Create(CreateCommand {
                    name: Some("first".to_string()),
                    ..Default::default()
                }))
                .await
        })
    };
    first_started.wait().await;
    let second_create = {
        let client = Arc::clone(&client);
        tokio::spawn(async move {
            client
                .request(Command::Create(CreateCommand {
                    name: Some("second".to_string()),
                    ..Default::default()
                }))
                .await
        })
    };
    tokio::task::yield_now().await;

    first_release.resolve(());
    second_started.wait().await;
    second_release.resolve(());
    first_create.await.expect("join");
    second_create.await.expect("join");

    client
        .next_from(message_index, |message| match message {
            ServerMessage::Event(envelope) => match &envelope.event {
                ServerEvent::ServerSnapshot(event) => event.snapshot.revision == 2,
                _ => false,
            },
            _ => false,
        })
        .await;

    let revisions: Vec<u64> = client
        .messages()
        .into_iter()
        .skip(message_index)
        .filter_map(|message| match message {
            ServerMessage::Event(envelope) => match envelope.event {
                ServerEvent::ServerSnapshot(event) => Some(event.snapshot.revision),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(revisions, vec![1, 2]);

    harness.server.close().await;
}

#[tokio::test]
async fn creates_server_assigned_durable_ids_and_supports_list_attach_and_detach() {
    let harness = start_harness().await;
    let client = connect(&harness).await;

    let created = client
        .request(Command::Create(CreateCommand {
            cwd: Some("/work".to_string()),
            name: Some("Created".to_string()),
            ..Default::default()
        }))
        .await;
    assert!(created.ok);
    let session = session_result(created.result);
    let created_id = session.id.clone();
    assert_eq!(Some(created_id.clone()), harness.service.last_created_id());
    assert_eq!(session.cwd, "/work");
    assert_eq!(session.name.as_deref(), Some("Created"));
    assert!(session.attached);
    assert!(session.locked);

    let listed = list_result(client.request(Command::List(ListCommand {})).await.result);
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, created_id);
    assert_eq!(listed[0].session_name.as_deref(), Some("Created"));
    assert_eq!(listed[0].cwd.as_deref(), Some("/work"));

    let detached = client
        .request(Command::Detach(SessionCommand::new(&created_id)))
        .await;
    assert!(detached.ok);
    assert_eq!(
        harness.service.latest_runtime(&created_id).dispose_count(),
        1
    );
    // Detaching again is a no-op that still succeeds.
    assert!(
        client
            .request(Command::Detach(SessionCommand::new(&created_id)))
            .await
            .ok
    );

    let reattached = attach(&client, &created_id).await;
    assert_eq!(reattached.id, created_id);
    assert_eq!(harness.service.runtime_count(&created_id), 2);

    harness.server.close().await;
}

#[tokio::test]
async fn preserves_backend_metadata_while_refreshing_live_session_metadata() {
    let service = TestServerService::new();
    service.seed_with(
        "session-1",
        Some("Live name".to_string()),
        "/tmp/pi-server-conformance",
        ModelRef {
            provider: "test".to_string(),
            id: "small".to_string(),
        },
        ThinkingLevel::Off,
    );
    service.set_list_sessions_hook(Arc::new(|metadata| {
        Box::pin(async move {
            Ok(metadata
                .into_iter()
                .map(|item| SessionMetadata {
                    parent_session_id: Some("parent-1".to_string()),
                    session_name: Some("stale name".to_string()),
                    ..item
                })
                .collect())
        })
    }));
    let harness = start_harness_with(service, |options| options).await;
    let client = connect(&harness).await;
    attach(&client, "session-1").await;

    let listed = list_result(client.request(Command::List(ListCommand {})).await.result);
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, "session-1");
    assert_eq!(listed[0].parent_session_id.as_deref(), Some("parent-1"));
    assert_eq!(
        listed[0].session_name.as_deref(),
        Some("Live name"),
        "the live snapshot wins over the stored metadata"
    );
    assert_eq!(listed[0].cwd.as_deref(), Some("/tmp/pi-server-conformance"));

    harness.server.close().await;
}

#[tokio::test]
async fn keeps_multiple_attachments_on_one_connection_independent() {
    let service = TestServerService::new();
    service.seed("first");
    service.seed("second");
    let harness = start_harness_with(Arc::clone(&service), |options| options).await;
    let client = connect(&harness).await;
    attach(&client, "first").await;
    attach(&client, "second").await;

    client
        .request(Command::Detach(SessionCommand::new("first")))
        .await;
    assert_eq!(service.latest_runtime("first").dispose_count(), 1);
    assert_eq!(service.latest_runtime("second").dispose_count(), 0);

    let response = client
        .request(Command::SetThinking(SetThinkingCommand {
            session_id: "second".to_string(),
            thinking_level: ThinkingLevel::Medium,
        }))
        .await;
    let session = session_result(response.result);
    assert_eq!(session.id, "second");
    assert_eq!(session.thinking_level, ThinkingLevel::Medium);

    harness.server.close().await;
}

#[tokio::test]
async fn broadcasts_snapshots_and_progress_only_to_attached_clients() {
    let service = TestServerService::new();
    service.seed("session-1");
    let harness = start_harness_with(Arc::clone(&service), |options| options).await;
    let attached_client = connect(&harness).await;
    let unattached_client = connect(&harness).await;
    attach(&attached_client, "session-1").await;
    let runtime = service.latest_runtime("session-1");

    let progress = TranscriptProgress::AssistantDelta(pi_protocol::AssistantDelta {
        message_id: "assistant-1".to_string(),
        content_index: 0,
        kind: pi_protocol::AssistantDeltaKind::Text,
        delta: "hello".to_string(),
    });
    runtime.emit_progress(progress.clone());
    attached_client
        .next(|message| {
            matches!(message, ServerMessage::Event(envelope)
                if matches!(envelope.event, ServerEvent::SessionProgress(_)))
        })
        .await;
    assert!(!unattached_client.messages().iter().any(|message| matches!(
        message,
        ServerMessage::Event(envelope)
            if matches!(envelope.event, ServerEvent::SessionProgress(_))
    )));

    let index = attached_client.message_count();
    runtime.emit_snapshot();
    let expected = runtime.snapshot().await.expect("snapshot").revision;
    let message = attached_client
        .next_from(index, |message| match message {
            ServerMessage::Event(envelope) => match &envelope.event {
                ServerEvent::SessionSnapshot(event) => event.snapshot.revision == expected,
                _ => false,
            },
            _ => false,
        })
        .await;
    let ServerMessage::Event(envelope) = message else {
        panic!("expected an event")
    };
    let ServerEvent::SessionSnapshot(event) = envelope.event else {
        panic!("expected a session snapshot")
    };
    assert_eq!(event.snapshot.id, "session-1");
    assert!(event.snapshot.attached);
    assert!(event.snapshot.locked);
    assert!(!unattached_client.messages().iter().any(|message| matches!(
        message,
        ServerMessage::Event(envelope)
            if matches!(envelope.event, ServerEvent::SessionSnapshot(_))
    )));

    harness.server.close().await;
}

#[tokio::test]
async fn allows_every_attached_client_to_control_a_singleton_live_runtime() {
    let service = TestServerService::new();
    service.seed("session-1");
    let harness = start_harness_with(Arc::clone(&service), |options| options).await;
    let first = connect(&harness).await;
    let second = connect(&harness).await;
    attach(&first, "session-1").await;

    let listed = list_result(second.request(Command::List(ListCommand {})).await.result);
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].session_name.as_deref(), Some("Session session-1"));

    attach(&second, "session-1").await;
    assert_eq!(service.runtime_count("session-1"), 1);

    let response = second
        .request(Command::SetModel(SetModelCommand {
            session_id: "session-1".to_string(),
            model: ModelRef {
                provider: "test".to_string(),
                id: "large".to_string(),
            },
        }))
        .await;
    assert_eq!(session_result(response.result).model.id, "large");
    first
        .next(|message| match message {
            ServerMessage::Event(envelope) => match &envelope.event {
                ServerEvent::SessionSnapshot(event) => event.snapshot.model.id == "large",
                _ => false,
            },
            _ => false,
        })
        .await;

    let response = first
        .request(Command::SetThinking(SetThinkingCommand {
            session_id: "session-1".to_string(),
            thinking_level: ThinkingLevel::High,
        }))
        .await;
    assert_eq!(
        session_result(response.result).thinking_level,
        ThinkingLevel::High
    );

    harness.server.close().await;
}

#[tokio::test]
async fn does_not_queue_prompts_and_processes_steer_and_abort_mid_turn() {
    let service = TestServerService::new();
    service.seed("session-1");
    let harness = start_harness_with(Arc::clone(&service), |options| options).await;
    let client = connect(&harness).await;
    attach(&client, "session-1").await;

    let prompt = {
        let client = Arc::clone(&client);
        tokio::spawn(async move {
            client
                .request(Command::Prompt(PromptCommand {
                    session_id: "session-1".to_string(),
                    text: "first".to_string(),
                }))
                .await
        })
    };
    client
        .next(|message| match message {
            ServerMessage::Event(envelope) => match &envelope.event {
                ServerEvent::SessionSnapshot(event) => event.snapshot.phase == SessionPhase::Turn,
                _ => false,
            },
            _ => false,
        })
        .await;

    let busy = client
        .request(Command::Prompt(PromptCommand {
            session_id: "session-1".to_string(),
            text: "second".to_string(),
        }))
        .await;
    assert!(!busy.ok);
    assert_eq!(busy.error.expect("error").code, ProtocolErrorCode::Busy);

    let steer = client
        .request(Command::Steer(PromptCommand {
            session_id: "session-1".to_string(),
            text: "adjust".to_string(),
        }))
        .await;
    assert!(steer.ok);
    assert_eq!(
        service
            .latest_runtime("session-1")
            .steers()
            .into_iter()
            .map(|input| input.text)
            .collect::<Vec<_>>(),
        vec!["adjust".to_string()]
    );

    let abort = client
        .request(Command::Abort(SessionCommand::new("session-1")))
        .await;
    assert!(abort.ok);
    let prompt = prompt.await.expect("join");
    assert!(prompt.ok);
    assert_eq!(session_result(prompt.result).phase, SessionPhase::Idle);

    harness.server.close().await;
}

#[tokio::test]
async fn returns_operation_attachment_state_relative_to_the_requesting_connection() {
    let service = TestServerService::new();
    service.seed("session-1");
    let harness = start_harness_with(Arc::clone(&service), |options| options).await;
    let first = connect(&harness).await;
    let second = connect(&harness).await;
    attach(&first, "session-1").await;
    attach(&second, "session-1").await;

    let prompt = {
        let first = Arc::clone(&first);
        tokio::spawn(async move {
            first
                .request(Command::Prompt(PromptCommand {
                    session_id: "session-1".to_string(),
                    text: "hello".to_string(),
                }))
                .await
        })
    };
    first
        .next(|message| match message {
            ServerMessage::Event(envelope) => match &envelope.event {
                ServerEvent::SessionSnapshot(event) => event.snapshot.phase == SessionPhase::Turn,
                _ => false,
            },
            _ => false,
        })
        .await;
    first
        .request(Command::Detach(SessionCommand::new("session-1")))
        .await;
    service.latest_runtime("session-1").finish_prompt();

    let response = prompt.await.expect("join");
    assert!(response.ok);
    let session = session_result(response.result);
    assert_eq!(session.id, "session-1");
    assert!(
        !session.attached,
        "the snapshot reflects the requesting connection, which detached"
    );

    harness.server.close().await;
}

#[tokio::test]
async fn rejects_and_disposes_a_service_runtime_with_the_wrong_server_assigned_id() {
    let service = TestServerService::new();
    service.set_create_id_override("wrong-id");
    let harness = start_harness_with(Arc::clone(&service), |options| options).await;
    let client = connect(&harness).await;

    let response = client
        .request(Command::Create(CreateCommand::default()))
        .await;
    assert!(!response.ok);
    assert_eq!(
        response.error.expect("error").code,
        ProtocolErrorCode::InvalidRequest
    );
    assert_eq!(service.latest_runtime("wrong-id").dispose_count(), 1);

    harness.server.close().await;
}

#[tokio::test]
async fn maps_service_lock_errors_and_rejects_control_from_unattached_clients() {
    let service = TestServerService::new();
    service.seed("locked");
    service.lock_session("locked");
    let harness = start_harness_with(service, |options| options).await;
    let client = connect(&harness).await;

    let locked = client
        .request(Command::Attach(SessionCommand::new("locked")))
        .await;
    assert_eq!(
        locked.error.expect("error").code,
        ProtocolErrorCode::SessionLocked
    );

    let unattached = client
        .request(Command::Abort(SessionCommand::new("locked")))
        .await;
    assert_eq!(
        unattached.error.expect("error").code,
        ProtocolErrorCode::InvalidRequest
    );

    harness.server.close().await;
}
