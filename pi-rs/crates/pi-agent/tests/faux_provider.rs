//! The agent loop driven by W8's faux provider.
//!
//! `ScriptedStream` (in `pi_agent::testing`) hands the loop one terminal event
//! per turn, which is what the ported upstream loop tests want. This file
//! exercises the other half: a *real* `ApiClient` that streams `start` /
//! `text_delta` / `toolcall_*` / `done` the way a provider does, so the
//! partial-message tracking and `message_update` plumbing are covered against
//! something other than a hand-rolled double.

use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use pi_core::{AssistantMessageEvent, StopReason};
use pretty_assertions::assert_eq;
use serde_json::{json, Value};

use pi_agent::testing::{empty_schema, ExecuteFn, FnTool};
use pi_agent::{
    agent_loop, Agent, AgentContext, AgentEvent, AgentEventListener, AgentLoopConfig, AgentMessage,
    AgentOptions, AgentToolRef, InitialAgentState, ToolResult,
};
use pi_provider_misc::{
    faux_assistant_message, faux_tool_call_with_id, FauxProvider, FauxResponse,
};

fn user(text: &str) -> AgentMessage {
    AgentMessage::user_text(text)
}

fn echo_tool(recorder: Arc<Mutex<Vec<Value>>>) -> AgentToolRef {
    let body: ExecuteFn = Arc::new(move |args, _context, _signal| {
        let recorder = recorder.clone();
        Box::pin(async move {
            recorder.lock().push(args);
            Ok(ToolResult::text("echoed"))
        })
    });
    Arc::new(FnTool::new("echo", empty_schema(), body))
}

#[tokio::test]
async fn streams_a_real_provider_turn_with_deltas() {
    let faux = FauxProvider::new();
    faux.set_texts(&["Hello there"]);

    let config = AgentLoopConfig::new(faux.model());
    let mut run = agent_loop(
        vec![user("hi")],
        AgentContext::default(),
        config,
        None,
        faux.stream_fn(),
    );
    let events = run.collect_events().await;
    let messages = run.result().await.unwrap();

    // The faux provider streams, so the loop must emit message_update events
    // between message_start and message_end for the assistant turn.
    let updates: Vec<&AgentEvent> = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::MessageUpdate { .. }))
        .collect();
    assert!(
        !updates.is_empty(),
        "expected streamed message_update events"
    );
    assert!(updates.iter().any(|e| matches!(
        e,
        AgentEvent::MessageUpdate {
            assistant_message_event: AssistantMessageEvent::TextDelta { .. },
            ..
        }
    )));

    // Every update carries the accumulated partial, and the last one matches
    // the final message text.
    let assistant = messages.last().unwrap().as_assistant().unwrap();
    assert_eq!(assistant.text(), "Hello there");
    assert_eq!(assistant.stop_reason, StopReason::Stop);
    assert_eq!(faux.call_count(), 1);
}

#[tokio::test]
async fn runs_a_tool_turn_end_to_end_through_the_provider() {
    let faux = FauxProvider::new();
    faux.set_responses(vec![
        FauxResponse::from(faux_assistant_message(faux_tool_call_with_id(
            "call-1",
            "echo",
            json!({}),
        ))),
        FauxResponse::text("all done"),
    ]);

    let executed = Arc::new(Mutex::new(Vec::new()));
    let context = AgentContext {
        tools: vec![echo_tool(executed.clone())],
        ..Default::default()
    };

    let mut run = agent_loop(
        vec![user("use the tool")],
        context,
        AgentLoopConfig::new(faux.model()),
        None,
        faux.stream_fn(),
    );
    let events = run.collect_events().await;
    let messages = run.result().await.unwrap();

    assert_eq!(executed.lock().len(), 1);
    assert_eq!(faux.call_count(), 2);
    assert_eq!(
        messages.iter().map(|m| m.role()).collect::<Vec<_>>(),
        vec!["user", "assistant", "toolResult", "assistant"]
    );
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::ToolExecutionEnd {
            is_error: false,
            ..
        }
    )));

    // The second request must carry the first turn's assistant message and the
    // tool result, in order.
    let second = &faux.requests()[1];
    assert_eq!(
        second
            .context
            .messages
            .iter()
            .map(|m| m.role())
            .collect::<Vec<_>>(),
        vec!["user", "assistant", "toolResult"]
    );
}

#[tokio::test]
async fn a_provider_failure_is_encoded_in_the_stream_not_thrown() {
    let faux = FauxProvider::new();
    faux.set_responses(vec![FauxResponse::failure("upstream is down")]);

    let mut run = agent_loop(
        vec![user("hi")],
        AgentContext::default(),
        AgentLoopConfig::new(faux.model()),
        None,
        faux.stream_fn(),
    );
    let events = run.collect_events().await;
    let messages = run.result().await.unwrap();

    // The loop treats it as a normal terminal turn: no Err, and the run ends
    // right after turn_end rather than starting another request.
    let assistant = messages.last().unwrap().as_assistant().unwrap();
    assert_eq!(assistant.stop_reason, StopReason::Error);
    assert_eq!(assistant.error_message.as_deref(), Some("upstream is down"));
    assert_eq!(events.last().unwrap().kind(), "agent_end");
    assert_eq!(faux.call_count(), 1);
}

struct Recorder(Arc<Mutex<Vec<AgentEvent>>>);

#[async_trait]
impl AgentEventListener for Recorder {
    async fn on_event(&self, event: AgentEvent, _signal: pi_core::AbortSignal) {
        self.0.lock().push(event);
    }
}

#[tokio::test]
async fn agent_drives_the_faux_provider_and_tracks_the_streaming_message() {
    let faux = FauxProvider::new();
    faux.set_texts(&["first answer", "second answer"]);

    let agent = Agent::new(AgentOptions {
        initial_state: Some(InitialAgentState {
            model: Some(faux.model()),
            ..Default::default()
        }),
        stream_fn: Some(faux.stream_fn()),
        ..Default::default()
    });
    let events = Arc::new(Mutex::new(Vec::new()));
    agent.subscribe(Arc::new(Recorder(events.clone())));

    agent.prompt_text("one", Vec::new()).await.unwrap();
    agent.prompt_text("two", Vec::new()).await.unwrap();

    assert_eq!(faux.call_count(), 2);
    assert_eq!(
        agent
            .messages()
            .iter()
            .map(|m| m.role())
            .collect::<Vec<_>>(),
        vec!["user", "assistant", "user", "assistant"]
    );
    // Streaming state is cleared once each run settles.
    assert!(agent.state().streaming_message.is_none());
    assert!(!agent.is_streaming());
    assert!(events
        .lock()
        .iter()
        .any(|e| matches!(e, AgentEvent::MessageUpdate { .. })));
}
