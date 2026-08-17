//! Port of `.upstream/packages/agent/test/agent.test.ts`.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use parking_lot::Mutex;
use pi_core::{
    AbortSignal, AssistantMessageEvent, AssistantMessageEventStream, DoneReason, ErrorReason,
    ModelThinkingLevel, StopReason, StreamFn,
};
use pretty_assertions::assert_eq;
use serde_json::json;

use pi_tools::ToolUpdateCallback;

use pi_agent::testing::{
    assistant_message, empty_schema, mock_model, text_message, tool_call, tool_use_message,
    value_schema, ExecuteFn, FnTool, Gate, ScriptedStream, Turn,
};
use pi_agent::{
    Agent, AgentEvent, AgentEventListener, AgentMessage, AgentOptions, AgentToolRef,
    InitialAgentState, QueueMode, ToolContext, ToolResult,
};

// --- helpers ---------------------------------------------------------------

type EventCallback = Arc<dyn Fn(&AgentEvent) + Send + Sync>;

/// Records every event, optionally awaiting a barrier for one event kind.
struct Recorder {
    events: Arc<Mutex<Vec<AgentEvent>>>,
    signals: Arc<Mutex<Vec<AbortSignal>>>,
    barrier: Option<(&'static str, Gate)>,
    barrier_done: Arc<Mutex<bool>>,
    on_event: Option<EventCallback>,
}

impl Recorder {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            events: Arc::new(Mutex::new(Vec::new())),
            signals: Arc::new(Mutex::new(Vec::new())),
            barrier: None,
            barrier_done: Arc::new(Mutex::new(false)),
            on_event: None,
        })
    }

    fn with_barrier(kind: &'static str, gate: Gate) -> Arc<Self> {
        Arc::new(Self {
            events: Arc::new(Mutex::new(Vec::new())),
            signals: Arc::new(Mutex::new(Vec::new())),
            barrier: Some((kind, gate)),
            barrier_done: Arc::new(Mutex::new(false)),
            on_event: None,
        })
    }

    fn with_callback(callback: EventCallback) -> Arc<Self> {
        Arc::new(Self {
            events: Arc::new(Mutex::new(Vec::new())),
            signals: Arc::new(Mutex::new(Vec::new())),
            barrier: None,
            barrier_done: Arc::new(Mutex::new(false)),
            on_event: Some(callback),
        })
    }

    fn kinds(&self) -> Vec<&'static str> {
        self.events.lock().iter().map(|e| e.kind()).collect()
    }

    fn count(&self) -> usize {
        self.events.lock().len()
    }

    fn count_of(&self, kind: &str) -> usize {
        self.events
            .lock()
            .iter()
            .filter(|e| e.kind() == kind)
            .count()
    }
}

#[async_trait]
impl AgentEventListener for Recorder {
    async fn on_event(&self, event: AgentEvent, signal: AbortSignal) {
        self.events.lock().push(event.clone());
        self.signals.lock().push(signal);
        if let Some(callback) = &self.on_event {
            callback(&event);
        }
        if let Some((kind, gate)) = &self.barrier {
            if event.kind() == *kind {
                gate.wait().await;
                *self.barrier_done.lock() = true;
            }
        }
    }
}

fn simple_agent(turns: Vec<Turn>) -> (Agent, ScriptedStream) {
    let script = ScriptedStream::new(turns).with_fallback(Turn::Done(text_message("ok")));
    let agent = Agent::new(AgentOptions {
        stream_fn: Some(script.clone().into_stream_fn()),
        ..Default::default()
    });
    (agent, script)
}

/// Emits `start`, then blocks until `gate` fires, then completes.
fn gated_stream_fn(gate: Gate) -> StreamFn {
    Arc::new(move |_model, _context, _options| {
        let gate = gate.clone();
        Box::pin(async move {
            let (sink, stream) = AssistantMessageEventStream::channel(8);
            tokio::spawn(async move {
                sink.send(AssistantMessageEvent::Start {
                    partial: assistant_message(Vec::new(), StopReason::Pending),
                })
                .await;
                gate.wait().await;
                sink.send(AssistantMessageEvent::Done {
                    reason: DoneReason::Stop,
                    message: text_message("Done"),
                })
                .await;
            });
            Ok(stream)
        })
    })
}

/// Emits `start`, then waits for the abort signal and terminates with `aborted`.
fn abort_aware_stream_fn() -> StreamFn {
    Arc::new(move |_model, _context, options| {
        let signal = options.stream.request.signal.clone();
        Box::pin(async move {
            let (sink, stream) = AssistantMessageEventStream::channel(8);
            tokio::spawn(async move {
                sink.send(AssistantMessageEvent::Start {
                    partial: assistant_message(Vec::new(), StopReason::Pending),
                })
                .await;
                if let Some(signal) = signal {
                    signal.aborted().await;
                }
                sink.send(AssistantMessageEvent::Error {
                    reason: ErrorReason::Aborted,
                    error: assistant_message(Vec::new(), StopReason::Aborted),
                })
                .await;
            });
            Ok(stream)
        })
    })
}

// --- tests -----------------------------------------------------------------

#[tokio::test]
async fn creates_an_agent_with_default_state() {
    let agent = Agent::new(AgentOptions::default());
    let state = agent.state();
    assert_eq!(state.system_prompt, "");
    assert_eq!(state.model.id, "unknown");
    assert_eq!(state.thinking_level, ModelThinkingLevel::Off);
    assert!(state.tools.is_empty());
    assert!(state.messages.is_empty());
    assert!(!state.is_streaming);
    assert!(state.streaming_message.is_none());
    assert!(state.pending_tool_calls.is_empty());
    assert!(state.error_message.is_none());
}

#[tokio::test]
async fn creates_an_agent_with_custom_initial_state() {
    let agent = Agent::new(AgentOptions {
        initial_state: Some(InitialAgentState {
            system_prompt: Some("You are a helpful assistant.".into()),
            model: Some(mock_model()),
            thinking_level: Some(ModelThinkingLevel::Low),
            ..Default::default()
        }),
        ..Default::default()
    });
    let state = agent.state();
    assert_eq!(state.system_prompt, "You are a helpful assistant.");
    assert_eq!(state.model.id, "mock");
    assert_eq!(state.thinking_level, ModelThinkingLevel::Low);
}

#[tokio::test]
async fn subscribing_emits_nothing_and_unsubscribing_stops_delivery() {
    let (agent, _) = simple_agent(vec![Turn::Done(text_message("ok"))]);
    let recorder = Recorder::new();
    let subscription = agent.subscribe(recorder.clone());
    assert_eq!(recorder.count(), 0);

    // State mutators never emit.
    agent.set_system_prompt("Test prompt");
    assert_eq!(recorder.count(), 0);
    assert_eq!(agent.state().system_prompt, "Test prompt");

    subscription.unsubscribe();
    agent.prompt_text("hi", Vec::new()).await.unwrap();
    assert_eq!(recorder.count(), 0);
}

#[tokio::test]
async fn emits_the_full_lifecycle_for_a_failed_run() {
    let agent = Agent::new(AgentOptions {
        stream_fn: Some(pi_agent::testing::failing_stream_fn("provider exploded")),
        ..Default::default()
    });
    let recorder = Recorder::new();
    agent.subscribe(recorder.clone());

    agent.prompt_text("hello", Vec::new()).await.unwrap();

    assert_eq!(
        recorder.kinds(),
        vec![
            "agent_start",
            "turn_start",
            "message_start",
            "message_end",
            "message_start",
            "message_end",
            "turn_end",
            "agent_end",
        ]
    );
    let messages = agent.messages();
    let last = messages.last().unwrap().as_assistant().unwrap();
    assert_eq!(last.stop_reason, StopReason::Error);
    assert_eq!(last.error_message.as_deref(), Some("provider exploded"));
    assert_eq!(
        agent.state().error_message.as_deref(),
        Some("provider exploded")
    );
}

#[tokio::test]
async fn awaits_async_subscribers_before_prompt_resolves() {
    let barrier = Gate::new();
    let (agent, _) = simple_agent(vec![Turn::Done(text_message("ok"))]);
    let recorder = Recorder::with_barrier("agent_end", barrier.clone());
    agent.subscribe(recorder.clone());

    let running = agent.clone();
    let handle = tokio::spawn(async move { running.prompt_text("hello", Vec::new()).await });

    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(!handle.is_finished());
    assert!(!*recorder.barrier_done.lock());
    assert!(agent.is_streaming());

    barrier.open();
    handle.await.unwrap().unwrap();

    assert!(*recorder.barrier_done.lock());
    assert!(!agent.is_streaming());
}

#[tokio::test]
async fn wait_for_idle_waits_for_async_subscribers() {
    let barrier = Gate::new();
    let (agent, _) = simple_agent(vec![Turn::Done(text_message("ok"))]);
    agent.subscribe(Recorder::with_barrier("message_end", barrier.clone()));

    let running = agent.clone();
    let prompt = tokio::spawn(async move { running.prompt_text("hello", Vec::new()).await });

    // `wait_for_idle` resolves immediately when no run is active (upstream's
    // `activeRun?.promise ?? Promise.resolve()`), so let the spawned run start.
    while !agent.is_streaming() {
        tokio::task::yield_now().await;
    }
    let waiting = agent.clone();
    let idle = tokio::spawn(async move { waiting.wait_for_idle().await });

    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(!idle.is_finished());
    assert!(agent.is_streaming());

    barrier.open();
    prompt.await.unwrap().unwrap();
    idle.await.unwrap();
    assert!(!agent.is_streaming());
}

#[tokio::test]
async fn passes_the_active_abort_signal_to_subscribers() {
    let agent = Agent::new(AgentOptions {
        stream_fn: Some(abort_aware_stream_fn()),
        ..Default::default()
    });
    let recorder = Recorder::new();
    agent.subscribe(recorder.clone());

    let running = agent.clone();
    let prompt = tokio::spawn(async move { running.prompt_text("hello", Vec::new()).await });

    tokio::time::sleep(Duration::from_millis(20)).await;
    let signal = recorder.signals.lock().first().cloned().unwrap();
    assert!(!signal.is_aborted());

    agent.abort();
    prompt.await.unwrap().unwrap();
    assert!(signal.is_aborted());
}

#[tokio::test]
async fn ignores_tool_updates_after_the_execution_settles() {
    let captured: Arc<Mutex<Option<ToolUpdateCallback>>> = Arc::new(Mutex::new(None));
    let sink = captured.clone();
    let body: ExecuteFn = Arc::new(move |_params, context: ToolContext, _signal| {
        let sink = sink.clone();
        Box::pin(async move {
            *sink.lock() = context.on_update.clone();
            context.emit_update(ToolResult::text("running"));
            Ok(ToolResult {
                content: vec![pi_core::InputContent::text("ok")],
                terminate: Some(true),
                ..Default::default()
            })
        })
    });
    let tool: AgentToolRef = Arc::new(FnTool::new("delayed_tool", empty_schema(), body));

    let script = ScriptedStream::new(vec![Turn::Done(tool_use_message(vec![tool_call(
        "call-1",
        "delayed_tool",
        json!({}),
    )]))]);
    let agent = Agent::new(AgentOptions {
        initial_state: Some(InitialAgentState {
            tools: Some(vec![tool]),
            ..Default::default()
        }),
        stream_fn: Some(script.into_stream_fn()),
        ..Default::default()
    });
    let recorder = Recorder::new();
    agent.subscribe(recorder.clone());

    agent.prompt_text("run tool", Vec::new()).await.unwrap();
    let count_after_prompt = recorder.count();

    // A late call must be dropped, not emitted.
    if let Some(update) = captured.lock().clone() {
        update(ToolResult::text("late"));
    }
    tokio::time::sleep(Duration::from_millis(10)).await;

    assert_eq!(recorder.count_of("tool_execution_update"), 1);
    assert_eq!(recorder.count(), count_after_prompt);
}

#[tokio::test]
async fn ignores_a_settled_parallel_tool_update_while_another_tool_runs() {
    let captured: Arc<Mutex<Option<ToolUpdateCallback>>> = Arc::new(Mutex::new(None));
    let settled_ended = Gate::new();
    let slow_started = Gate::new();
    let release_slow = Gate::new();

    let sink = captured.clone();
    let settled_body: ExecuteFn = Arc::new(move |_params, context: ToolContext, _signal| {
        let sink = sink.clone();
        Box::pin(async move {
            *sink.lock() = context.on_update.clone();
            Ok(ToolResult {
                content: vec![pi_core::InputContent::text("done")],
                terminate: Some(true),
                ..Default::default()
            })
        })
    });
    let settled: AgentToolRef = Arc::new(FnTool::new("settled_tool", empty_schema(), settled_body));

    let started = slow_started.clone();
    let release = release_slow.clone();
    let slow_body: ExecuteFn = Arc::new(move |_params, _context, _signal| {
        let started = started.clone();
        let release = release.clone();
        Box::pin(async move {
            started.open();
            release.wait().await;
            Ok(ToolResult {
                content: vec![pi_core::InputContent::text("done")],
                terminate: Some(true),
                ..Default::default()
            })
        })
    });
    let slow: AgentToolRef = Arc::new(FnTool::new("slow_tool", empty_schema(), slow_body));

    let script = ScriptedStream::new(vec![Turn::Done(tool_use_message(vec![
        tool_call("call-1", "settled_tool", json!({})),
        tool_call("call-2", "slow_tool", json!({})),
    ]))]);
    let agent = Agent::new(AgentOptions {
        initial_state: Some(InitialAgentState {
            tools: Some(vec![settled, slow]),
            ..Default::default()
        }),
        stream_fn: Some(script.into_stream_fn()),
        ..Default::default()
    });

    let notifier = settled_ended.clone();
    let recorder = Recorder::with_callback(Arc::new(move |event: &AgentEvent| {
        if let AgentEvent::ToolExecutionEnd { tool_call_id, .. } = event {
            if tool_call_id == "call-1" {
                notifier.open();
            }
        }
    }));
    agent.subscribe(recorder.clone());

    let running = agent.clone();
    let prompt = tokio::spawn(async move { running.prompt_text("run tools", Vec::new()).await });

    tokio::join!(slow_started.wait(), settled_ended.wait());
    let count_before_late_update = recorder.count();

    if let Some(update) = captured.lock().clone() {
        update(ToolResult::text("late"));
    }
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert_eq!(recorder.count(), count_before_late_update);

    release_slow.open();
    prompt.await.unwrap().unwrap();
    assert_eq!(recorder.count_of("tool_execution_update"), 0);
}

#[tokio::test]
async fn state_mutators_replace_transcript_and_tools() {
    let (agent, _) = simple_agent(vec![]);
    agent.set_system_prompt("Custom prompt");
    assert_eq!(agent.state().system_prompt, "Custom prompt");

    agent.set_model(mock_model());
    assert_eq!(agent.state().model.id, "mock");

    agent.set_thinking_level(ModelThinkingLevel::High);
    assert_eq!(agent.state().thinking_level, ModelThinkingLevel::High);

    let tool: AgentToolRef = Arc::new(FnTool::new(
        "test",
        value_schema(),
        Arc::new(|_, _, _| Box::pin(async { Ok(ToolResult::text("x")) })),
    ));
    agent.set_tools(vec![tool]);
    assert_eq!(agent.state().tools.len(), 1);

    agent.set_messages(vec![AgentMessage::user_text("Hello")]);
    assert_eq!(agent.messages().len(), 1);
    agent.push_message(AgentMessage::Assistant(text_message("Hi")));
    assert_eq!(agent.messages().len(), 2);
    agent.set_messages(Vec::new());
    assert!(agent.messages().is_empty());
}

#[tokio::test]
async fn queued_messages_do_not_enter_the_transcript_until_drained() {
    let (agent, _) = simple_agent(vec![]);
    agent.steer(AgentMessage::user_text("Steering message"));
    agent.follow_up(AgentMessage::user_text("Follow-up message"));
    assert!(agent.messages().is_empty());
    assert!(agent.has_queued_messages());
    agent.clear_all_queues();
    assert!(!agent.has_queued_messages());
}

#[tokio::test]
async fn abort_without_an_active_run_is_a_no_op() {
    let (agent, _) = simple_agent(vec![]);
    agent.abort();
}

#[tokio::test]
async fn reset_is_rejected_mid_run_without_corrupting_the_transcript() {
    let gate = Gate::new();
    let agent = Agent::new(AgentOptions {
        stream_fn: Some(gated_stream_fn(gate.clone())),
        ..Default::default()
    });

    let running = agent.clone();
    let prompt = tokio::spawn(async move { running.prompt_text("Hello", Vec::new()).await });
    tokio::time::sleep(Duration::from_millis(20)).await;

    assert!(agent.is_streaming());
    assert_eq!(
        agent
            .messages()
            .iter()
            .map(|m| m.role())
            .collect::<Vec<_>>(),
        vec!["user"]
    );
    let error = agent.reset().unwrap_err();
    assert_eq!(
        error.message(),
        "Agent is already processing. Wait for completion before resetting."
    );
    assert!(agent.is_streaming());
    assert_eq!(
        agent
            .messages()
            .iter()
            .map(|m| m.role())
            .collect::<Vec<_>>(),
        vec!["user"]
    );

    gate.open();
    prompt.await.unwrap().unwrap();

    assert!(!agent.is_streaming());
    assert_eq!(
        agent
            .messages()
            .iter()
            .map(|m| m.role())
            .collect::<Vec<_>>(),
        vec!["user", "assistant"]
    );
}

#[tokio::test]
async fn prompt_is_rejected_while_streaming() {
    let agent = Agent::new(AgentOptions {
        stream_fn: Some(abort_aware_stream_fn()),
        ..Default::default()
    });
    let running = agent.clone();
    let first = tokio::spawn(async move { running.prompt_text("First message", Vec::new()).await });
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(agent.is_streaming());

    let error = agent
        .prompt_text("Second message", Vec::new())
        .await
        .unwrap_err();
    assert_eq!(
        error.message(),
        "Agent is already processing a prompt. Use steer() or followUp() to queue messages, or wait for completion."
    );

    agent.abort();
    let _ = first.await.unwrap();
}

#[tokio::test]
async fn continue_is_rejected_while_streaming() {
    let agent = Agent::new(AgentOptions {
        stream_fn: Some(abort_aware_stream_fn()),
        ..Default::default()
    });
    let running = agent.clone();
    let first = tokio::spawn(async move { running.prompt_text("First message", Vec::new()).await });
    tokio::time::sleep(Duration::from_millis(20)).await;

    let error = agent.continue_run().await.unwrap_err();
    assert_eq!(
        error.message(),
        "Agent is already processing. Wait for completion before continuing."
    );

    agent.abort();
    let _ = first.await.unwrap();
}

#[tokio::test]
async fn continue_processes_queued_follow_ups_after_an_assistant_turn() {
    let (agent, _) = simple_agent(vec![Turn::Done(text_message("Processed"))]);
    agent.set_messages(vec![
        AgentMessage::user_text("Initial"),
        AgentMessage::Assistant(text_message("Initial response")),
    ]);
    agent.follow_up(AgentMessage::user_text("Queued follow-up"));

    agent.continue_run().await.unwrap();

    let messages = agent.messages();
    assert!(messages.iter().any(|m| match m {
        AgentMessage::User(u) => u.content.to_text() == "Queued follow-up",
        _ => false,
    }));
    assert_eq!(messages.last().unwrap().role(), "assistant");
}

#[tokio::test]
async fn continue_keeps_one_at_a_time_steering_from_an_assistant_tail() {
    let (agent, script) = simple_agent(vec![
        Turn::Done(text_message("Processed 1")),
        Turn::Done(text_message("Processed 2")),
    ]);
    agent.set_messages(vec![
        AgentMessage::user_text("Initial"),
        AgentMessage::Assistant(text_message("Initial response")),
    ]);
    agent.steer(AgentMessage::user_text("Steering 1"));
    agent.steer(AgentMessage::user_text("Steering 2"));

    agent.continue_run().await.unwrap();

    let messages = agent.messages();
    let tail: Vec<&'static str> = messages[messages.len() - 4..]
        .iter()
        .map(|m| m.role())
        .collect();
    assert_eq!(tail, vec!["user", "assistant", "user", "assistant"]);
    assert_eq!(script.call_count(), 2);
}

#[tokio::test]
async fn queue_mode_all_drains_every_queued_message_at_once() {
    let (agent, script) = simple_agent(vec![
        Turn::Done(text_message("Processed 1")),
        Turn::Done(text_message("Processed 2")),
    ]);
    agent.set_steering_mode(QueueMode::All);
    assert_eq!(agent.steering_mode(), QueueMode::All);
    agent.set_messages(vec![
        AgentMessage::user_text("Initial"),
        AgentMessage::Assistant(text_message("Initial response")),
    ]);
    agent.steer(AgentMessage::user_text("Steering 1"));
    agent.steer(AgentMessage::user_text("Steering 2"));

    agent.continue_run().await.unwrap();

    let messages = agent.messages();
    let tail: Vec<&'static str> = messages[messages.len() - 3..]
        .iter()
        .map(|m| m.role())
        .collect();
    assert_eq!(tail, vec!["user", "user", "assistant"]);
    assert_eq!(script.call_count(), 1);
}

#[tokio::test]
async fn forwards_session_id_to_the_stream_function() {
    let script = ScriptedStream::new(vec![]).with_fallback(Turn::Done(text_message("ok")));
    let agent = Agent::new(AgentOptions {
        session_id: Some("session-abc".into()),
        stream_fn: Some(script.clone().into_stream_fn()),
        ..Default::default()
    });

    agent.prompt_text("hello", Vec::new()).await.unwrap();
    assert_eq!(
        script.requests()[0].session_id.as_deref(),
        Some("session-abc")
    );

    agent.set_session_id(Some("session-def".into()));
    assert_eq!(agent.session_id().as_deref(), Some("session-def"));

    agent.prompt_text("hello again", Vec::new()).await.unwrap();
    assert_eq!(
        script.requests()[1].session_id.as_deref(),
        Some("session-def")
    );
}

#[tokio::test]
async fn missing_stream_fn_fails_the_run_rather_than_panicking() {
    let agent = Agent::new(AgentOptions::default());
    let recorder = Recorder::new();
    agent.subscribe(recorder.clone());

    agent.prompt_text("hello", Vec::new()).await.unwrap();

    let last = agent.messages();
    let last = last.last().unwrap().as_assistant().unwrap();
    assert_eq!(last.stop_reason, StopReason::Error);
    assert!(last
        .error_message
        .as_deref()
        .unwrap()
        .contains("No default stream function configured"));
}
