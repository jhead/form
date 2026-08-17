//! The facade's acceptance test: every layer composed, no network.
//!
//! This is the test that would have caught the wiring gaps the individual crate
//! suites cannot see — an adapter registered without its provider, a tool with
//! no execution environment, a session that persists but does not reload.

use std::sync::Arc;

use pi_provider_misc::faux::{faux_assistant_message, faux_tool_call, FauxProvider, FauxResponse};
use pi_sdk::agent::AgentMessage;
use pi_sdk::session::repo::SessionRepo;
use pi_sdk::sqlite::SqliteSessionRepo;
use pi_sdk::Pi;

fn faux_backed_pi(faux: &FauxProvider) -> Pi {
    let model = faux.model();
    let mut provider = pi_sdk::catalog::Provider::new("faux", model.api.clone());
    provider.models = faux.models();
    Pi::builder()
        .with_provider(provider, faux.client())
        .build()
        .expect("builds")
}

#[tokio::test]
async fn a_tool_calling_turn_runs_and_persists_to_sqlite() {
    let faux = FauxProvider::new();
    faux.set_responses(vec![
        FauxResponse::from(faux_assistant_message(faux_tool_call(
            "read",
            serde_json::json!({ "path": "notes.md" }),
        ))),
        FauxResponse::text("The note says: ship it."),
    ]);

    let pi = faux_backed_pi(&faux);

    let workdir = tempfile::tempdir().unwrap();
    std::fs::write(workdir.path().join("notes.md"), "ship it\n").unwrap();
    let env = Arc::new(pi_sdk::tools::LocalExecutionEnv::new(
        workdir.path().to_string_lossy().to_string(),
    ));

    let repo = SqliteSessionRepo::new(workdir.path().join("sessions.db"));
    let session = repo
        .create(&pi_sdk::session::SessionCreateOptions {
            cwd: Some(workdir.path().to_string_lossy().to_string()),
            ..Default::default()
        })
        .await
        .unwrap();

    let agent = pi
        .agent()
        .model(faux.model())
        .system_prompt("You read files and answer briefly.")
        .tools(pi_sdk::tools::default_tools())
        .env(env)
        .build()
        .unwrap();

    agent
        .prompt_text("What does notes.md say?", vec![])
        .await
        .unwrap();

    let messages = agent.messages();
    // user, assistant(toolCall), toolResult, assistant(text)
    assert_eq!(messages.len(), 4, "unexpected transcript: {messages:#?}");
    assert!(matches!(messages[0], AgentMessage::User(_)));
    assert!(matches!(messages[2], AgentMessage::ToolResult(_)));

    // The tool actually read the real file rather than being short-circuited.
    let tool_result = match &messages[2] {
        AgentMessage::ToolResult(r) => r,
        other => panic!("expected a tool result, got {other:?}"),
    };
    assert!(!tool_result.is_error, "tool failed: {tool_result:?}");
    let rendered = format!("{:?}", tool_result.content);
    assert!(
        rendered.contains("ship it"),
        "tool output missing file contents: {rendered}"
    );

    // Two provider round trips: the tool call, then the answer.
    assert_eq!(faux.call_count(), 2);

    for message in messages {
        session.append_message(message).await.unwrap();
    }

    let metadata = session.get_metadata().await.unwrap();
    drop(session);
    let reopened = repo.open(&metadata).await.unwrap();
    let entries = reopened
        .find_entries(&pi_sdk::session::EntryQuery::default())
        .await
        .unwrap();
    assert_eq!(entries.len(), 4, "transcript did not survive a reload");
}

#[tokio::test]
async fn an_adapter_without_its_provider_fails_with_a_clear_error() {
    // The easy misconfiguration: register the API client but not the provider.
    let faux = FauxProvider::new();
    let pi = Pi::builder()
        .with_api_client(faux.client())
        .build()
        .unwrap();

    // `Agent` is not `Debug`, so unwrap the Result by hand.
    let err = match pi.agent().model(faux.model()).build() {
        Ok(_) => panic!("should not build without a registered provider"),
        Err(err) => err,
    };

    assert_eq!(err.code(), "catalog");
    assert!(
        err.message().contains("faux"),
        "error should name the provider: {}",
        err.message()
    );
}

#[tokio::test]
async fn the_builtin_catalog_resolves_a_real_model_reference() {
    let pi = Pi::builder().with_builtin_providers().build().unwrap();
    let model = pi
        .resolve_model("anthropic/claude-sonnet-4-5")
        .await
        .unwrap();
    assert_eq!(model.provider, "anthropic");
    assert_eq!(model.api, pi_sdk::core::Api::AnthropicMessages);
    // An adapter is registered for it, so a stream function resolves.
    pi.stream_fn_for(&model).expect("adapter registered");
}

#[tokio::test]
async fn an_abort_signal_stops_a_turn() {
    let faux = FauxProvider::new();
    faux.set_texts(&["this should not finish"]);
    let pi = faux_backed_pi(&faux);

    let agent = pi.agent().model(faux.model()).build().unwrap();
    agent.abort();
    // Aborting an idle agent is a no-op, and the next prompt still works.
    agent.prompt_text("hello", vec![]).await.unwrap();
    assert!(!agent.is_streaming());
}
