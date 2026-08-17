//! Port of `.upstream/packages/agent/test/agent-loop.test.ts`.

use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use pi_core::{
    AbortSignal, AssistantContent, Context, Cost, InputContent, Message, StopReason, Usage,
};
use pretty_assertions::assert_eq;
use serde_json::{json, Value};

use pi_agent::testing::{
    assistant_message, empty_schema, mock_model, text_message, tool_call, tool_use_message,
    value_schema, ExecuteFn, FnTool, Gate, ScriptedStream, Turn,
};
use pi_agent::AgentError;
use pi_agent::ToolContext;
use pi_agent::{
    agent_loop, agent_loop_continue, AfterToolCall, AfterToolCallContext, AfterToolCallResult,
    AgentContext, AgentEvent, AgentLoopConfig, AgentLoopTurnUpdate, AgentMessage, AgentToolRef,
    BeforeToolCall, BeforeToolCallContext, BeforeToolCallResult, ContextTransform,
    MessageConverter, MessageSource, PrepareNextTurn, ShouldStopAfterTurn, ToolExecutionMode,
    ToolResult, TurnContext,
};
use pi_tools::ToolError;

// --- helpers ---------------------------------------------------------------

fn user(text: &str) -> AgentMessage {
    AgentMessage::user_text(text)
}

fn base_config() -> AgentLoopConfig {
    AgentLoopConfig::new(mock_model())
}

fn context_with_tools(tools: Vec<AgentToolRef>) -> AgentContext {
    AgentContext {
        tools,
        ..Default::default()
    }
}

fn event_types(events: &[AgentEvent]) -> Vec<&'static str> {
    events.iter().map(|e| e.kind()).collect()
}

fn tool_result_text(result: &ToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|c| c.as_text().map(|t| t.text.clone()))
        .collect::<Vec<_>>()
        .join("")
}

/// Build a tool from an async closure over the validated arguments.
fn fn_tool(
    name: &str,
    parameters: Value,
    execute: impl Fn(Value) -> Result<ToolResult, ToolError> + Send + Sync + 'static,
) -> Arc<FnTool> {
    let execute = Arc::new(execute);
    let boxed: ExecuteFn = Arc::new(move |params, _context, _signal| {
        let execute = execute.clone();
        Box::pin(async move { execute(params) })
    });
    Arc::new(FnTool::new(name, parameters, boxed))
}

fn arg_str(params: &Value, key: &str) -> String {
    params
        .get(key)
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string()
}

/// Records every `value` argument the tool was executed with.
fn echo_tool(recorder: Arc<Mutex<Vec<String>>>) -> Arc<FnTool> {
    fn_tool("echo", value_schema(), move |params| {
        let value = arg_str(&params, "value");
        recorder.lock().push(value.clone());
        Ok(
            ToolResult::text(format!("echoed: {value}"))
                .with_details(Some(json!({"value": value}))),
        )
    })
}

// --- hook adapters ---------------------------------------------------------

struct FnConverter<F>(F);

#[async_trait]
impl<F> MessageConverter for FnConverter<F>
where
    F: Fn(&[AgentMessage]) -> Vec<Message> + Send + Sync + 'static,
{
    async fn convert(&self, messages: &[AgentMessage]) -> Vec<Message> {
        (self.0)(messages)
    }
}

struct FnTransform<F>(F);

#[async_trait]
impl<F> ContextTransform for FnTransform<F>
where
    F: Fn(Vec<AgentMessage>) -> Vec<AgentMessage> + Send + Sync + 'static,
{
    async fn transform(
        &self,
        messages: Vec<AgentMessage>,
        _signal: Option<AbortSignal>,
    ) -> Vec<AgentMessage> {
        (self.0)(messages)
    }
}

struct FnBefore<F>(F);

#[async_trait]
impl<F> BeforeToolCall for FnBefore<F>
where
    F: Fn(BeforeToolCallContext) -> Option<BeforeToolCallResult> + Send + Sync + 'static,
{
    async fn before_tool_call(
        &self,
        context: BeforeToolCallContext,
        _signal: Option<AbortSignal>,
    ) -> Option<BeforeToolCallResult> {
        (self.0)(context)
    }
}

struct FnAfter<F>(F);

#[async_trait]
impl<F> AfterToolCall for FnAfter<F>
where
    F: Fn(AfterToolCallContext) -> Result<Option<AfterToolCallResult>, AgentError>
        + Send
        + Sync
        + 'static,
{
    async fn after_tool_call(
        &self,
        context: AfterToolCallContext,
        _signal: Option<AbortSignal>,
    ) -> Result<Option<AfterToolCallResult>, AgentError> {
        (self.0)(context)
    }
}

struct FnStop<F>(F);

#[async_trait]
impl<F> ShouldStopAfterTurn for FnStop<F>
where
    F: Fn(TurnContext) -> bool + Send + Sync + 'static,
{
    async fn should_stop_after_turn(&self, context: TurnContext) -> bool {
        (self.0)(context)
    }
}

struct FnPrepare<F>(F);

#[async_trait]
impl<F> PrepareNextTurn for FnPrepare<F>
where
    F: Fn(TurnContext) -> Option<AgentLoopTurnUpdate> + Send + Sync + 'static,
{
    async fn prepare_next_turn(&self, context: TurnContext) -> Option<AgentLoopTurnUpdate> {
        (self.0)(context)
    }
}

struct FnSource<F>(F);

#[async_trait]
impl<F> MessageSource for FnSource<F>
where
    F: Fn() -> Vec<AgentMessage> + Send + Sync + 'static,
{
    async fn take_messages(&self) -> Vec<AgentMessage> {
        (self.0)()
    }
}

// --- tests -----------------------------------------------------------------

#[tokio::test]
async fn emits_the_full_event_sequence_for_a_simple_turn() {
    let script = ScriptedStream::new(vec![Turn::Done(text_message("Hi there!"))]);
    let context = AgentContext {
        system_prompt: "You are helpful.".into(),
        ..Default::default()
    };

    let mut run = agent_loop(
        vec![user("Hello")],
        context,
        base_config(),
        None,
        script.into_stream_fn(),
    );
    let events = run.collect_events().await;
    let messages = run.result().await.unwrap();

    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role(), "user");
    assert_eq!(messages[1].role(), "assistant");
    assert_eq!(
        event_types(&events),
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
}

#[tokio::test]
async fn convert_to_llm_can_filter_custom_message_types() {
    let converted: Arc<Mutex<Vec<Message>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = converted.clone();

    let mut config = base_config();
    config.convert_to_llm = Arc::new(FnConverter(move |messages: &[AgentMessage]| {
        let out: Vec<Message> = messages
            .iter()
            .filter(|m| m.role() != "custom")
            .filter_map(|m| m.as_llm_message())
            .collect();
        *sink.lock() = out.clone();
        out
    }));

    let notification = AgentMessage::Custom(pi_agent::CustomMessage {
        custom_type: "notification".into(),
        content: pi_core::UserContent::Text("This is a notification".into()),
        display: false,
        details: None,
        timestamp: pi_core::now_ms(),
    });
    let context = AgentContext {
        messages: vec![notification],
        ..Default::default()
    };

    let script = ScriptedStream::new(vec![Turn::Done(text_message("Response"))]);
    let mut run = agent_loop(
        vec![user("Hello")],
        context,
        config,
        None,
        script.into_stream_fn(),
    );
    run.collect_events().await;
    run.result().await.unwrap();

    let converted = converted.lock();
    assert_eq!(converted.len(), 1);
    assert_eq!(converted[0].role(), "user");
}

#[tokio::test]
async fn transform_context_runs_before_convert_to_llm() {
    let transformed_len = Arc::new(Mutex::new(0usize));
    let converted_len = Arc::new(Mutex::new(0usize));

    let mut config = base_config();
    let sink = transformed_len.clone();
    config.transform_context = Some(Arc::new(FnTransform(move |messages: Vec<AgentMessage>| {
        // Keep only the last two, the way upstream's pruning example does.
        let pruned = messages[messages.len().saturating_sub(2)..].to_vec();
        *sink.lock() = pruned.len();
        pruned
    })));
    let sink = converted_len.clone();
    config.convert_to_llm = Arc::new(FnConverter(move |messages: &[AgentMessage]| {
        let out: Vec<Message> = messages.iter().filter_map(|m| m.as_llm_message()).collect();
        *sink.lock() = out.len();
        out
    }));

    let context = AgentContext {
        messages: vec![
            user("old message 1"),
            AgentMessage::Assistant(text_message("old response 1")),
            user("old message 2"),
            AgentMessage::Assistant(text_message("old response 2")),
        ],
        ..Default::default()
    };

    let script = ScriptedStream::new(vec![Turn::Done(text_message("Response"))]);
    let mut run = agent_loop(
        vec![user("new message")],
        context,
        config,
        None,
        script.into_stream_fn(),
    );
    run.collect_events().await;
    run.result().await.unwrap();

    assert_eq!(*transformed_len.lock(), 2);
    assert_eq!(*converted_len.lock(), 2);
}

#[tokio::test]
async fn executes_tool_calls_and_lets_after_tool_call_replace_usage() {
    let executed = Arc::new(Mutex::new(Vec::new()));
    let tool_usage = Usage {
        input: 1,
        output: 2,
        cache_read: 3,
        cache_write: 4,
        total_tokens: 10,
        cost: Cost {
            input: 0.1,
            output: 0.2,
            cache_read: 0.3,
            cache_write: 0.4,
            total: 1.0,
        },
        ..Default::default()
    };
    let patched_usage = Usage {
        input: 5,
        output: 6,
        cache_read: 7,
        cache_write: 8,
        total_tokens: 26,
        ..Default::default()
    };

    let recorder = executed.clone();
    let usage = tool_usage.clone();
    let tool = fn_tool("echo", value_schema(), move |params| {
        let value = arg_str(&params, "value");
        recorder.lock().push(value.clone());
        Ok(ToolResult {
            content: vec![InputContent::text(format!("echoed: {value}"))],
            details: Some(json!({"value": value})),
            usage: Some(usage.clone()),
            ..Default::default()
        })
    });

    let observed: Arc<Mutex<Option<Usage>>> = Arc::new(Mutex::new(None));
    let observed_sink = observed.clone();
    let patched = patched_usage.clone();
    let mut config = base_config();
    config.after_tool_call = Some(Arc::new(FnAfter(move |ctx: AfterToolCallContext| {
        *observed_sink.lock() = ctx.result.usage.clone();
        Ok(Some(AfterToolCallResult {
            usage: Some(patched.clone()),
            ..Default::default()
        }))
    })));

    let script = ScriptedStream::new(vec![
        Turn::Done(tool_use_message(vec![tool_call(
            "tool-1",
            "echo",
            json!({"value": "hello"}),
        )])),
        Turn::Done(text_message("done")),
    ]);

    let mut run = agent_loop(
        vec![user("echo something")],
        context_with_tools(vec![tool]),
        config,
        None,
        script.into_stream_fn(),
    );
    let events = run.collect_events().await;
    let messages = run.result().await.unwrap();

    assert_eq!(&*executed.lock(), &["hello".to_string()]);
    assert!(events
        .iter()
        .any(|e| matches!(e, AgentEvent::ToolExecutionStart { .. })));
    let end = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::ToolExecutionEnd { is_error, .. } => Some(*is_error),
            _ => None,
        })
        .unwrap();
    assert!(!end);
    assert_eq!(observed.lock().clone(), Some(tool_usage));

    let tool_result = messages
        .iter()
        .find_map(|m| m.as_tool_result())
        .expect("tool result message");
    assert_eq!(tool_result.usage.clone(), Some(patched_usage));
}

#[tokio::test]
async fn does_not_execute_tool_calls_from_a_length_truncated_message() {
    let executed = Arc::new(Mutex::new(Vec::new()));
    let tool = echo_tool(executed.clone());

    let truncated = assistant_message(
        vec![tool_call("tool-1", "echo", json!({"value": "hel"}))],
        StopReason::Length,
    );
    let script = ScriptedStream::new(vec![
        Turn::Done(truncated),
        Turn::Done(text_message("done")),
    ]);
    let stream = script.clone();

    let mut run = agent_loop(
        vec![user("echo something")],
        context_with_tools(vec![tool]),
        base_config(),
        None,
        script.into_stream_fn(),
    );
    let events = run.collect_events().await;
    let messages = run.result().await.unwrap();

    assert!(executed.lock().is_empty());
    let (is_error, text) = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::ToolExecutionEnd {
                is_error, result, ..
            } => Some((*is_error, tool_result_text(result))),
            _ => None,
        })
        .unwrap();
    assert!(is_error);
    assert!(text.contains("output token limit"));

    // The loop continues so the model can re-issue the call.
    assert_eq!(stream.call_count(), 2);
    assert_eq!(messages.last().unwrap().role(), "assistant");
}

#[tokio::test]
async fn before_tool_call_can_replace_args_without_revalidation() {
    let executed: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let recorder = executed.clone();
    let tool = fn_tool("echo", value_schema(), move |params| {
        recorder.lock().push(params.get("value").cloned().unwrap());
        Ok(ToolResult::text("ok"))
    });

    let mut config = base_config();
    // The replacement (a number) would fail the schema; upstream mutates the
    // validated args in place and never revalidates, so it must reach execute.
    config.before_tool_call = Some(Arc::new(FnBefore(|_ctx| {
        Some(BeforeToolCallResult {
            args: Some(json!({"value": 123})),
            ..Default::default()
        })
    })));

    let script = ScriptedStream::new(vec![
        Turn::Done(tool_use_message(vec![tool_call(
            "tool-1",
            "echo",
            json!({"value": "hello"}),
        )])),
        Turn::Done(text_message("done")),
    ]);

    let mut run = agent_loop(
        vec![user("echo something")],
        context_with_tools(vec![tool]),
        config,
        None,
        script.into_stream_fn(),
    );
    run.collect_events().await;
    run.result().await.unwrap();

    assert_eq!(&*executed.lock(), &[json!(123)]);
}

#[tokio::test]
async fn prepare_arguments_runs_before_validation() {
    let executed: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let schema = json!({
        "type": "object",
        "properties": {
            "edits": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": { "oldText": {"type": "string"}, "newText": {"type": "string"} },
                    "required": ["oldText", "newText"]
                }
            }
        },
        "required": ["edits"],
        "additionalProperties": false
    });

    let recorder = executed.clone();
    let base = fn_tool("edit", schema, move |params| {
        recorder.lock().push(params.get("edits").cloned().unwrap());
        Ok(ToolResult::text("edited"))
    });
    // Fold a flat {oldText,newText} call into the array shape the schema wants.
    let tool: AgentToolRef = Arc::new(Arc::try_unwrap(base).ok().unwrap().with_prepare_arguments(
        Arc::new(|args: Value| {
            let (Some(old), Some(new)) = (args.get("oldText"), args.get("newText")) else {
                return args;
            };
            let mut edits = args
                .get("edits")
                .and_then(|v| v.as_array().cloned())
                .unwrap_or_default();
            edits.push(json!({"oldText": old, "newText": new}));
            json!({ "edits": edits })
        }),
    ));

    let script = ScriptedStream::new(vec![
        Turn::Done(tool_use_message(vec![tool_call(
            "tool-1",
            "edit",
            json!({"oldText": "before", "newText": "after"}),
        )])),
        Turn::Done(text_message("done")),
    ]);

    let mut run = agent_loop(
        vec![user("edit something")],
        context_with_tools(vec![tool]),
        base_config(),
        None,
        script.into_stream_fn(),
    );
    run.collect_events().await;
    run.result().await.unwrap();

    assert_eq!(
        &*executed.lock(),
        &[json!([{"oldText": "before", "newText": "after"}])]
    );
}

/// A tool whose "first" call blocks until released, so concurrency is observable.
fn gated_tool(
    name: &str,
    gate: Gate,
    first_resolved: Arc<Mutex<bool>>,
    parallel_observed: Arc<Mutex<bool>>,
) -> Arc<FnTool> {
    let boxed: ExecuteFn = Arc::new(move |params, _context, _signal| {
        let gate = gate.clone();
        let first_resolved = first_resolved.clone();
        let parallel_observed = parallel_observed.clone();
        Box::pin(async move {
            let value = arg_str(&params, "value");
            if value == "first" {
                gate.wait().await;
                *first_resolved.lock() = true;
            }
            if value == "second" && !*first_resolved.lock() {
                *parallel_observed.lock() = true;
            }
            Ok(ToolResult::text(format!("echoed: {value}")))
        })
    });
    Arc::new(FnTool::new(name, value_schema(), boxed))
}

fn two_call_script() -> ScriptedStream {
    ScriptedStream::new(vec![
        Turn::Done(tool_use_message(vec![
            tool_call("tool-1", "echo", json!({"value": "first"})),
            tool_call("tool-2", "echo", json!({"value": "second"})),
        ])),
        Turn::Done(text_message("done")),
    ])
}

#[tokio::test]
async fn parallel_ends_in_completion_order_but_persists_results_in_source_order() {
    let gate = Gate::new();
    let first_resolved = Arc::new(Mutex::new(false));
    let parallel_observed = Arc::new(Mutex::new(false));
    let tool = gated_tool(
        "echo",
        gate.clone(),
        first_resolved,
        parallel_observed.clone(),
    );

    let mut config = base_config();
    config.tool_execution = ToolExecutionMode::Parallel;

    let mut run = agent_loop(
        vec![user("echo both")],
        context_with_tools(vec![tool]),
        config,
        None,
        two_call_script().into_stream_fn(),
    );

    // Release the blocked first tool once the second has had a chance to run.
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        gate.open();
    });

    let events = run.collect_events().await;
    run.result().await.unwrap();

    let end_ids: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolExecutionEnd { tool_call_id, .. } => Some(tool_call_id.clone()),
            _ => None,
        })
        .collect();
    let result_ids: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::MessageEnd { message } => {
                message.as_tool_result().map(|r| r.tool_call_id.clone())
            }
            _ => None,
        })
        .collect();
    let turn_ids: Vec<String> = events
        .iter()
        .flat_map(|e| match e {
            AgentEvent::TurnEnd { tool_results, .. } => tool_results
                .iter()
                .map(|r| r.tool_call_id.clone())
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        })
        .collect();

    assert!(*parallel_observed.lock());
    assert_eq!(end_ids, vec!["tool-2".to_string(), "tool-1".to_string()]);
    assert_eq!(result_ids, vec!["tool-1".to_string(), "tool-2".to_string()]);
    assert_eq!(turn_ids, vec!["tool-1".to_string(), "tool-2".to_string()]);
}

#[tokio::test]
async fn a_sequential_tool_forces_the_whole_batch_sequential() {
    let gate = Gate::new();
    let first_resolved = Arc::new(Mutex::new(false));
    let parallel_observed = Arc::new(Mutex::new(false));
    let tool: AgentToolRef = Arc::new(
        Arc::try_unwrap(gated_tool(
            "echo",
            gate.clone(),
            first_resolved,
            parallel_observed.clone(),
        ))
        .ok()
        .unwrap()
        .with_execution_mode(ToolExecutionMode::Sequential),
    );

    // Config default is parallel; the tool must still force sequential.
    let mut run = agent_loop(
        vec![user("run both")],
        context_with_tools(vec![tool]),
        base_config(),
        None,
        two_call_script().into_stream_fn(),
    );

    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        gate.open();
    });

    let events = run.collect_events().await;
    run.result().await.unwrap();

    assert!(!*parallel_observed.lock());
    let result_ids: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::MessageEnd { message } => {
                message.as_tool_result().map(|r| r.tool_call_id.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(result_ids, vec!["tool-1".to_string(), "tool-2".to_string()]);
}

#[tokio::test]
async fn one_sequential_tool_among_several_forces_sequential() {
    let order: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let gate = Gate::new();

    let recorder = order.clone();
    let slow_gate = gate.clone();
    let slow_body: ExecuteFn = Arc::new(move |params, _context, _signal| {
        let recorder = recorder.clone();
        let gate = slow_gate.clone();
        Box::pin(async move {
            let value = arg_str(&params, "value");
            recorder.lock().push(format!("slow:{value}"));
            if value == "a" {
                gate.wait().await;
            }
            Ok(ToolResult::text("slow"))
        })
    });
    let slow: AgentToolRef = Arc::new(
        FnTool::new("slow", value_schema(), slow_body)
            .with_execution_mode(ToolExecutionMode::Sequential),
    );

    let recorder = order.clone();
    let fast = fn_tool("fast", value_schema(), move |params| {
        recorder
            .lock()
            .push(format!("fast:{}", arg_str(&params, "value")));
        Ok(ToolResult::text("fast"))
    });

    let script = ScriptedStream::new(vec![
        Turn::Done(tool_use_message(vec![
            tool_call("tool-1", "slow", json!({"value": "a"})),
            tool_call("tool-2", "fast", json!({"value": "b"})),
        ])),
        Turn::Done(text_message("done")),
    ]);

    let mut run = agent_loop(
        vec![user("run both")],
        context_with_tools(vec![slow, fast]),
        base_config(),
        None,
        script.into_stream_fn(),
    );

    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        gate.open();
    });

    run.collect_events().await;
    run.result().await.unwrap();

    let order = order.lock();
    assert_eq!(order[0], "slow:a");
    assert!(order.contains(&"fast:b".to_string()));
}

#[tokio::test]
async fn all_parallel_tools_run_concurrently() {
    let gate = Gate::new();
    let parallel_observed = Arc::new(Mutex::new(false));
    let tool: AgentToolRef = Arc::new(
        Arc::try_unwrap(gated_tool(
            "echo",
            gate.clone(),
            Arc::new(Mutex::new(false)),
            parallel_observed.clone(),
        ))
        .ok()
        .unwrap()
        .with_execution_mode(ToolExecutionMode::Parallel),
    );

    let mut run = agent_loop(
        vec![user("echo both")],
        context_with_tools(vec![tool]),
        base_config(),
        None,
        two_call_script().into_stream_fn(),
    );

    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        gate.open();
    });

    run.collect_events().await;
    run.result().await.unwrap();
    assert!(*parallel_observed.lock());
}

#[tokio::test]
async fn prepare_next_turn_snapshot_applies_to_the_next_request() {
    let tool = echo_tool(Arc::new(Mutex::new(Vec::new())));
    let prepared = Arc::new(Mutex::new(false));

    let mut config = base_config();
    let flag = prepared.clone();
    config.prepare_next_turn = Some(Arc::new(FnPrepare(move |ctx: TurnContext| {
        if *flag.lock() {
            return None;
        }
        *flag.lock() = true;
        let mut context = ctx.context.clone();
        context.system_prompt = "second prompt".into();
        Some(AgentLoopTurnUpdate {
            context: Some(context),
            ..Default::default()
        })
    })));

    let script = ScriptedStream::new(vec![
        Turn::Done(tool_use_message(vec![tool_call(
            "tool-1",
            "echo",
            json!({"value": "hello"}),
        )])),
        Turn::Done(text_message("done")),
    ]);
    let stream = script.clone();

    let context = AgentContext {
        system_prompt: "first prompt".into(),
        tools: vec![tool],
        ..Default::default()
    };
    let mut run = agent_loop(
        vec![user("echo something")],
        context,
        config,
        None,
        script.into_stream_fn(),
    );
    run.collect_events().await;
    run.result().await.unwrap();

    let requests = stream.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1].context.system_prompt.as_deref(),
        Some("second prompt")
    );
}

#[tokio::test]
async fn should_stop_after_turn_ends_the_run_before_polling_follow_ups() {
    let executed = Arc::new(Mutex::new(Vec::new()));
    let tool = echo_tool(executed.clone());

    let steering_polls = Arc::new(Mutex::new(0usize));
    let follow_up_polls = Arc::new(Mutex::new(0usize));
    let callback_tool_ids: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let callback_roles: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));

    let mut config = base_config();
    let counter = steering_polls.clone();
    config.get_steering_messages = Some(Arc::new(FnSource(move || {
        *counter.lock() += 1;
        Vec::new()
    })));
    let counter = follow_up_polls.clone();
    config.get_follow_up_messages = Some(Arc::new(FnSource(move || {
        *counter.lock() += 1;
        vec![user("follow up should stay queued")]
    })));
    let ids = callback_tool_ids.clone();
    let roles = callback_roles.clone();
    config.should_stop_after_turn = Some(Arc::new(FnStop(move |ctx: TurnContext| {
        assert_eq!(ctx.message.role(), "assistant");
        *ids.lock() = ctx
            .tool_results
            .iter()
            .map(|r| r.tool_call_id.clone())
            .collect();
        *roles.lock() = ctx.context.messages.iter().map(|m| m.role()).collect();
        true
    })));

    let script = ScriptedStream::new(vec![Turn::Done(tool_use_message(vec![tool_call(
        "tool-1",
        "echo",
        json!({"value": "hello"}),
    )]))])
    .with_fallback(Turn::Done(text_message("should not run")));
    let stream = script.clone();

    let mut run = agent_loop(
        vec![user("echo something")],
        context_with_tools(vec![tool]),
        config,
        None,
        script.into_stream_fn(),
    );
    let events = run.collect_events().await;
    let messages = run.result().await.unwrap();

    assert_eq!(stream.call_count(), 1);
    assert_eq!(&*executed.lock(), &["hello".to_string()]);
    assert_eq!(*steering_polls.lock(), 1);
    assert_eq!(*follow_up_polls.lock(), 0);
    assert_eq!(&*callback_tool_ids.lock(), &["tool-1".to_string()]);
    assert_eq!(
        &*callback_roles.lock(),
        &["user", "assistant", "toolResult"]
    );
    assert_eq!(
        messages.iter().map(|m| m.role()).collect::<Vec<_>>(),
        vec!["user", "assistant", "toolResult"]
    );
    assert_eq!(
        event_types(&events),
        vec![
            "agent_start",
            "turn_start",
            "message_start",
            "message_end",
            "message_start",
            "message_end",
            "tool_execution_start",
            "tool_execution_end",
            "message_start",
            "message_end",
            "turn_end",
            "agent_end",
        ]
    );
}

#[tokio::test]
async fn stops_when_every_tool_result_sets_terminate() {
    let tool = fn_tool("echo", value_schema(), |params| {
        Ok(ToolResult {
            content: vec![InputContent::text(format!(
                "echoed: {}",
                arg_str(&params, "value")
            ))],
            terminate: Some(true),
            ..Default::default()
        })
    });

    let script = ScriptedStream::new(vec![Turn::Done(tool_use_message(vec![tool_call(
        "tool-1",
        "echo",
        json!({"value": "hello"}),
    )]))])
    .with_fallback(Turn::Done(text_message("should not run")));
    let stream = script.clone();

    let mut run = agent_loop(
        vec![user("echo something")],
        context_with_tools(vec![tool]),
        base_config(),
        None,
        script.into_stream_fn(),
    );
    let events = run.collect_events().await;
    let messages = run.result().await.unwrap();

    assert_eq!(stream.call_count(), 1);
    assert_eq!(
        messages.iter().map(|m| m.role()).collect::<Vec<_>>(),
        vec!["user", "assistant", "toolResult"]
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, AgentEvent::TurnEnd { .. }))
            .count(),
        1
    );
}

#[tokio::test]
async fn before_tool_call_block_with_terminate_stops_the_run() {
    let executed = Arc::new(Mutex::new(false));
    let flag = executed.clone();
    let tool = fn_tool("echo", value_schema(), move |_params| {
        *flag.lock() = true;
        Ok(ToolResult::text("should not execute"))
    });

    let mut config = base_config();
    config.before_tool_call = Some(Arc::new(FnBefore(|_ctx| {
        Some(BeforeToolCallResult {
            block: Some(true),
            reason: Some("Blocked by policy".into()),
            terminate: Some(true),
            args: None,
        })
    })));

    let script = ScriptedStream::new(vec![Turn::Done(tool_use_message(vec![tool_call(
        "tool-1",
        "echo",
        json!({"value": "hello"}),
    )]))])
    .with_fallback(Turn::Done(text_message("should not run")));
    let stream = script.clone();

    let mut run = agent_loop(
        vec![user("echo something")],
        context_with_tools(vec![tool]),
        config,
        None,
        script.into_stream_fn(),
    );
    run.collect_events().await;
    let messages = run.result().await.unwrap();

    assert!(!*executed.lock());
    assert_eq!(stream.call_count(), 1);
    let result = messages.iter().find_map(|m| m.as_tool_result()).unwrap();
    assert!(result.is_error);
    assert!(result
        .content
        .contains(&InputContent::text("Blocked by policy")));
}

#[tokio::test]
async fn a_mixed_batch_with_one_terminating_block_continues() {
    let executed = Arc::new(Mutex::new(Vec::new()));
    let tool = echo_tool(executed.clone());

    let mut config = base_config();
    config.tool_execution = ToolExecutionMode::Parallel;
    config.before_tool_call = Some(Arc::new(FnBefore(|ctx: BeforeToolCallContext| {
        if ctx.args.get("value").and_then(|v| v.as_str()) == Some("first") {
            Some(BeforeToolCallResult {
                block: Some(true),
                reason: Some("Blocked first".into()),
                terminate: Some(true),
                args: None,
            })
        } else {
            None
        }
    })));

    let script = two_call_script();
    let stream = script.clone();
    let mut run = agent_loop(
        vec![user("echo both")],
        context_with_tools(vec![tool]),
        config,
        None,
        script.into_stream_fn(),
    );
    run.collect_events().await;
    run.result().await.unwrap();

    assert_eq!(&*executed.lock(), &["second".to_string()]);
    assert_eq!(stream.call_count(), 2);
}

#[tokio::test]
async fn parallel_batch_continues_when_not_every_result_terminates() {
    let tool = fn_tool("echo", value_schema(), |params| {
        let value = arg_str(&params, "value");
        Ok(ToolResult {
            content: vec![InputContent::text(format!("echoed: {value}"))],
            terminate: Some(value == "first"),
            ..Default::default()
        })
    });

    let mut config = base_config();
    config.tool_execution = ToolExecutionMode::Parallel;

    let script = two_call_script();
    let stream = script.clone();
    let mut run = agent_loop(
        vec![user("echo both")],
        context_with_tools(vec![tool]),
        config,
        None,
        script.into_stream_fn(),
    );
    run.collect_events().await;
    let messages = run.result().await.unwrap();

    assert_eq!(stream.call_count(), 2);
    assert_eq!(
        messages.iter().map(|m| m.role()).collect::<Vec<_>>(),
        vec!["user", "assistant", "toolResult", "toolResult", "assistant"]
    );
}

#[tokio::test]
async fn after_tool_call_can_mark_a_batch_terminating() {
    let tool = echo_tool(Arc::new(Mutex::new(Vec::new())));

    let mut config = base_config();
    config.after_tool_call = Some(Arc::new(FnAfter(|_ctx| {
        Ok(Some(AfterToolCallResult {
            terminate: Some(true),
            ..Default::default()
        }))
    })));

    let script = ScriptedStream::new(vec![Turn::Done(tool_use_message(vec![tool_call(
        "tool-1",
        "echo",
        json!({"value": "hello"}),
    )]))])
    .with_fallback(Turn::Done(text_message("should not run")));
    let stream = script.clone();

    let mut run = agent_loop(
        vec![user("echo something")],
        context_with_tools(vec![tool]),
        config,
        None,
        script.into_stream_fn(),
    );
    run.collect_events().await;
    run.result().await.unwrap();
    assert_eq!(stream.call_count(), 1);
}

#[tokio::test]
async fn queued_steering_messages_are_injected_after_all_tool_calls_complete() {
    let executed = Arc::new(Mutex::new(Vec::new()));
    let tool = echo_tool(executed.clone());

    let delivered = Arc::new(Mutex::new(false));
    let mut config = base_config();
    config.tool_execution = ToolExecutionMode::Sequential;
    let seen = executed.clone();
    let flag = delivered.clone();
    config.get_steering_messages = Some(Arc::new(FnSource(move || {
        if !seen.lock().is_empty() && !*flag.lock() {
            *flag.lock() = true;
            vec![user("interrupt")]
        } else {
            Vec::new()
        }
    })));

    let script = two_call_script();
    let stream = script.clone();
    let mut run = agent_loop(
        vec![user("start")],
        context_with_tools(vec![tool]),
        config,
        None,
        script.into_stream_fn(),
    );
    let events = run.collect_events().await;
    run.result().await.unwrap();

    assert_eq!(
        &*executed.lock(),
        &["first".to_string(), "second".to_string()]
    );

    let tool_ends: Vec<bool> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolExecutionEnd { is_error, .. } => Some(*is_error),
            _ => None,
        })
        .collect();
    assert_eq!(tool_ends, vec![false, false]);

    // The steering message lands after both tool result messages.
    let sequence: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::MessageStart { message } => match message {
                AgentMessage::ToolResult(r) => Some(format!("tool:{}", r.tool_call_id)),
                AgentMessage::User(u) => Some(u.content.to_text()),
                _ => None,
            },
            _ => None,
        })
        .collect();
    let interrupt = sequence.iter().position(|s| s == "interrupt").unwrap();
    assert!(sequence.iter().position(|s| s == "tool:tool-1").unwrap() < interrupt);
    assert!(sequence.iter().position(|s| s == "tool:tool-2").unwrap() < interrupt);

    // ...and is visible to the provider on the second request.
    let requests = stream.requests();
    assert!(saw_user_text(&requests[1].context, "interrupt"));
}

fn saw_user_text(context: &Context, needle: &str) -> bool {
    context.messages.iter().any(|m| {
        m.as_user()
            .map(|u| u.content.to_text() == needle)
            .unwrap_or(false)
    })
}

#[tokio::test]
async fn unknown_tools_produce_an_error_tool_result() {
    let script = ScriptedStream::new(vec![
        Turn::Done(tool_use_message(vec![tool_call(
            "tool-1",
            "missing",
            json!({}),
        )])),
        Turn::Done(text_message("done")),
    ]);

    let mut run = agent_loop(
        vec![user("go")],
        context_with_tools(vec![]),
        base_config(),
        None,
        script.into_stream_fn(),
    );
    let events = run.collect_events().await;
    run.result().await.unwrap();

    let (is_error, text) = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::ToolExecutionEnd {
                is_error, result, ..
            } => Some((*is_error, tool_result_text(result))),
            _ => None,
        })
        .unwrap();
    assert!(is_error);
    assert_eq!(text, "Tool missing not found");
}

#[tokio::test]
async fn invalid_arguments_produce_an_error_tool_result_and_never_execute() {
    let executed = Arc::new(Mutex::new(Vec::new()));
    let tool = echo_tool(executed.clone());

    let script = ScriptedStream::new(vec![
        Turn::Done(tool_use_message(vec![tool_call(
            "tool-1",
            "echo",
            json!({"wrong": true}),
        )])),
        Turn::Done(text_message("done")),
    ]);

    let mut run = agent_loop(
        vec![user("go")],
        context_with_tools(vec![tool]),
        base_config(),
        None,
        script.into_stream_fn(),
    );
    let events = run.collect_events().await;
    run.result().await.unwrap();

    assert!(executed.lock().is_empty());
    let (is_error, text) = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::ToolExecutionEnd {
                is_error, result, ..
            } => Some((*is_error, tool_result_text(result))),
            _ => None,
        })
        .unwrap();
    assert!(is_error);
    assert!(text.starts_with("Validation failed for tool \"echo\":"));
}

#[tokio::test]
async fn an_errored_assistant_turn_ends_the_run_immediately() {
    let failed = assistant_message(vec![AssistantContent::text("")], StopReason::Error);
    let script = ScriptedStream::new(vec![Turn::Failed(failed)]);

    let mut run = agent_loop(
        vec![user("hi")],
        AgentContext::default(),
        base_config(),
        None,
        script.into_stream_fn(),
    );
    let events = run.collect_events().await;
    run.result().await.unwrap();

    assert_eq!(
        event_types(&events),
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
    let turn_end = events
        .iter()
        .find(|e| matches!(e, AgentEvent::TurnEnd { .. }))
        .unwrap();
    if let AgentEvent::TurnEnd { tool_results, .. } = turn_end {
        assert!(tool_results.is_empty());
    }
}

#[tokio::test]
async fn tool_progress_updates_are_emitted_before_tool_execution_end() {
    let body: ExecuteFn = Arc::new(|_params, context: ToolContext, _signal| {
        Box::pin(async move {
            context.emit_update(ToolResult::text("running"));
            Ok(ToolResult::text("ok"))
        })
    });
    let tool: AgentToolRef = Arc::new(FnTool::new("progress", empty_schema(), body));

    let script = ScriptedStream::new(vec![
        Turn::Done(tool_use_message(vec![tool_call(
            "tool-1",
            "progress",
            json!({}),
        )])),
        Turn::Done(text_message("done")),
    ]);

    let mut run = agent_loop(
        vec![user("go")],
        context_with_tools(vec![tool]),
        base_config(),
        None,
        script.into_stream_fn(),
    );
    let events = run.collect_events().await;
    run.result().await.unwrap();

    let kinds = event_types(&events);
    let update = kinds
        .iter()
        .position(|k| *k == "tool_execution_update")
        .unwrap();
    let end = kinds
        .iter()
        .position(|k| *k == "tool_execution_end")
        .unwrap();
    assert!(update < end);
}

// --- agent_loop_continue ---------------------------------------------------

#[tokio::test]
async fn continue_rejects_an_empty_context() {
    let script = ScriptedStream::new(vec![]);
    let error = agent_loop_continue(
        AgentContext::default(),
        base_config(),
        None,
        script.into_stream_fn(),
    )
    .err()
    .unwrap();
    assert_eq!(error.message(), "Cannot continue: no messages in context");
}

#[tokio::test]
async fn continue_rejects_an_assistant_tail() {
    let script = ScriptedStream::new(vec![]);
    let context = AgentContext {
        messages: vec![AgentMessage::Assistant(text_message("hi"))],
        ..Default::default()
    };
    let error = agent_loop_continue(context, base_config(), None, script.into_stream_fn())
        .err()
        .unwrap();
    assert_eq!(
        error.message(),
        "Cannot continue from message role: assistant"
    );
}

#[tokio::test]
async fn continue_does_not_re_emit_the_existing_user_message() {
    let script = ScriptedStream::new(vec![Turn::Done(text_message("Response"))]);
    let context = AgentContext {
        system_prompt: "You are helpful.".into(),
        messages: vec![user("Hello")],
        ..Default::default()
    };

    let mut run =
        agent_loop_continue(context, base_config(), None, script.into_stream_fn()).unwrap();
    let events = run.collect_events().await;
    let messages = run.result().await.unwrap();

    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].role(), "assistant");
    let ends: Vec<&AgentMessage> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::MessageEnd { message } => Some(message),
            _ => None,
        })
        .collect();
    assert_eq!(ends.len(), 1);
    assert_eq!(ends[0].role(), "assistant");
}

#[tokio::test]
async fn continue_accepts_a_custom_last_message() {
    let custom = AgentMessage::Custom(pi_agent::CustomMessage {
        custom_type: "hook".into(),
        content: pi_core::UserContent::Text("Hook content".into()),
        display: false,
        details: None,
        timestamp: pi_core::now_ms(),
    });
    let context = AgentContext {
        system_prompt: "You are helpful.".into(),
        messages: vec![custom],
        ..Default::default()
    };

    let mut config = base_config();
    config.convert_to_llm = Arc::new(pi_agent::HarnessMessageConverter);

    let script = ScriptedStream::new(vec![Turn::Done(text_message("Response to custom message"))]);
    let mut run = agent_loop_continue(context, config, None, script.into_stream_fn()).unwrap();
    run.collect_events().await;
    let messages = run.result().await.unwrap();

    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].role(), "assistant");
}
