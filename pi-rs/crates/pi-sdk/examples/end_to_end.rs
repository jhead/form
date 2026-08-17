//! End-to-end: a scripted provider driving the agent loop, with a real tool
//! call, and the turn persisted to a SQLite session that is then reloaded.
//!
//! Everything here runs offline — the faux provider is a real `ApiClient`, so
//! this exercises the same code path a live provider takes.
//!
//! ```sh
//! cargo run -p pi-sdk --example end_to_end
//! ```

use std::sync::Arc;

use pi_provider_misc::faux::{faux_assistant_message, faux_tool_call, FauxProvider, FauxResponse};
use pi_sdk::agent::{AgentEvent, AgentEventListener};
use pi_sdk::core::AbortSignal;
use pi_sdk::session::repo::SessionRepo;
use pi_sdk::sqlite::SqliteSessionRepo;
use pi_sdk::Pi;

fn describe(payload: &pi_sdk::session::EntryPayload) -> &'static str {
    match payload {
        pi_sdk::session::EntryPayload::Message(_) => "message",
        pi_sdk::session::EntryPayload::Compaction(_) => "compaction",
        pi_sdk::session::EntryPayload::BranchSummary(_) => "branchSummary",
        _ => "other",
    }
}

fn assistant_text(message: &pi_sdk::agent::AgentMessage) -> Option<String> {
    match message {
        pi_sdk::agent::AgentMessage::Assistant(m) => {
            let text = m.text();
            (!text.is_empty()).then_some(text)
        }
        _ => None,
    }
}

/// Prints the event stream as it arrives — the same callback shape an FFI host
/// would use to forward events to its own queue.
struct Printer;

#[async_trait::async_trait]
impl AgentEventListener for Printer {
    async fn on_event(&self, event: AgentEvent, _signal: AbortSignal) {
        match &event {
            AgentEvent::TurnStart => println!("  → turn start"),
            AgentEvent::ToolExecutionStart { tool_name, .. } => {
                println!("  → tool start: {tool_name}")
            }
            AgentEvent::ToolExecutionEnd { tool_name, .. } => {
                println!("  → tool end:   {tool_name}")
            }
            AgentEvent::TurnEnd { .. } => println!("  → turn end"),
            _ => {}
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. A scripted provider: one tool call, then a final answer.
    let faux = FauxProvider::new();
    faux.set_responses(vec![
        FauxResponse::from(faux_assistant_message(faux_tool_call(
            "read",
            serde_json::json!({ "path": "notes.md" }),
        ))),
        FauxResponse::text("The note says: ship it."),
    ]);

    // 2. Assemble the SDK. The faux adapter registers like any other provider,
    //    which is the point — nothing downstream knows it is a test double.
    let model = faux.model();
    let mut faux_provider = pi_sdk::catalog::Provider::new("faux", model.api.clone());
    faux_provider.models = faux.models();
    let pi = Pi::builder()
        .with_provider(faux_provider, faux.client())
        .build()?;

    // 3. A temp working tree for the tools to read.
    let workdir = tempfile::tempdir()?;
    std::fs::write(workdir.path().join("notes.md"), "ship it\n")?;
    let env = Arc::new(pi_sdk::tools::LocalExecutionEnv::new(
        workdir.path().to_string_lossy().to_string(),
    ));

    // 4. A SQLite-backed session.
    let db_path = workdir.path().join("sessions.db");
    let repo = SqliteSessionRepo::new(&db_path);
    let session = repo
        .create(&pi_sdk::session::SessionCreateOptions {
            cwd: Some(workdir.path().to_string_lossy().to_string()),
            ..Default::default()
        })
        .await?;
    let session_id = session.get_metadata().await?.id.clone();
    println!("session {session_id}");

    // 5. Build the agent with the real built-in tools.
    let agent = pi
        .agent()
        .model(model)
        .system_prompt("You read files and answer briefly.")
        .tools(pi_sdk::tools::default_tools())
        .env(env)
        .session_id(session_id.clone())
        .build()?;

    agent.subscribe(Arc::new(Printer));

    println!("prompting…");
    agent.prompt_text("What does notes.md say?", vec![]).await?;

    // 6. Persist the resulting conversation.
    for message in agent.messages() {
        session.append_message(message).await?;
    }

    // 7. Reload from disk and confirm it round-trips.
    let metadata = session.get_metadata().await?;
    drop(session);
    let reopened = repo.open(&metadata).await?;
    let entries = reopened
        .find_entries(&pi_sdk::session::EntryQuery::default())
        .await?;

    println!(
        "\npersisted {} entries; reloaded {}",
        entries.len(),
        entries.len()
    );
    for entry in &entries {
        println!("  seq {} {}", entry.seq, describe(&entry.payload));
    }

    if let Some(text) = agent.messages().iter().rev().find_map(assistant_text) {
        println!("\nfinal answer: {text}");
    }
    Ok(())
}
