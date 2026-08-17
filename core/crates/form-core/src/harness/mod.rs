//! The stub harness — a deterministic event source shaped exactly like the real one.
//!
//! **Owner: W2** (`docs/specs/02-stub-harness.md`).
//!
//! What is here now is the minimum that proves the boundary: one turn, thinking → text →
//! tool call → tool execution, with real cadence and real `partial` accumulation. W2 grows
//! it into the full generator (multi-turn, rich markdown, many tools, failures, queueing)
//! without changing this trait.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde_json::json;

use crate::protocol::{
    now_ms, AssistantContent, AssistantMessage, AssistantMessageEvent, DoneReason, Entry,
    EntryKind, ErrorReason, EventKind, Message, ModelRef, RunOutcome, StopReason, ToolCall, Usage,
};

/// Cooperative cancellation. A Swift caller cannot drop a Rust future, so aborting is an
/// explicit signal the run polls between events — the same convention as `pi-core`.
#[derive(Clone, Default)]
pub struct AbortSignal(Arc<AtomicBool>);

impl AbortSignal {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn abort(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_aborted(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

pub struct RunRequest {
    pub session_id: String,
    pub run_id: String,
    pub command_id: Option<String>,
    pub prompt: String,
    pub model: ModelRef,
    pub workspace_root: Option<String>,
    pub turn_index: u32,
}

/// Emitting side of the run. The concrete implementation writes to the store and the
/// event bus; the harness only decides *what* happens and *when*.
pub trait RunContext: Send + Sync {
    fn emit(&self, kind: EventKind);
    fn append_entry(&self, session_id: &str, kind: EntryKind) -> Option<Entry>;
    fn replace_entry(&self, entry: &Entry);
    /// Multiplier on all sleeps. 1.0 is human-realistic; tests use 100.0.
    fn speed(&self) -> f64;
}

#[async_trait::async_trait]
pub trait Harness: Send + Sync {
    async fn run(&self, req: RunRequest, ctx: Arc<dyn RunContext>, abort: AbortSignal);
}

/// Minimal deterministic stub. TODO(W2): replace the fixed script with the generator from
/// spec 02 §5 — rich markdown, several tools, multi-turn, failure and abort variety.
pub struct StubHarness;

impl StubHarness {
    async fn sleep(ctx: &Arc<dyn RunContext>, ms: u64) {
        let scaled = (ms as f64 / ctx.speed().max(0.01)).round() as u64;
        if scaled > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(scaled)).await;
        }
    }
}

#[async_trait::async_trait]
impl Harness for StubHarness {
    async fn run(&self, req: RunRequest, ctx: Arc<dyn RunContext>, abort: AbortSignal) {
        let started = now_ms();
        let api = "anthropic-messages";
        let provider = req.model.provider_id.clone();
        let model = req.model.model_id.clone();

        ctx.emit(EventKind::RunStart {
            session_id: req.session_id.clone(),
            run_id: req.run_id.clone(),
        });
        ctx.emit(EventKind::TurnStart {
            session_id: req.session_id.clone(),
            run_id: req.run_id.clone(),
        });

        let mut partial = AssistantMessage::pending(api, &provider, &model);
        let Some(entry) = ctx.append_entry(
            &req.session_id,
            EntryKind::Message {
                message: Message::Assistant(partial.clone()),
            },
        ) else {
            return;
        };
        ctx.emit(EventKind::MessageStart {
            session_id: req.session_id.clone(),
            entry: entry.clone(),
        });

        let emit_update = |event: AssistantMessageEvent| EventKind::MessageUpdate {
            session_id: req.session_id.clone(),
            entry_id: entry.id.clone(),
            event,
        };

        ctx.emit(emit_update(AssistantMessageEvent::Start {
            partial: partial.clone(),
        }));

        // Time to first token.
        Self::sleep(&ctx, 420).await;

        macro_rules! bail_if_aborted {
            () => {
                if abort.is_aborted() {
                    partial.stop_reason = StopReason::Aborted;
                    partial.error_message = Some("aborted by user".to_string());
                    ctx.emit(emit_update(AssistantMessageEvent::Error {
                        reason: ErrorReason::Aborted,
                        error: partial.clone(),
                    }));
                    finish(&ctx, &req, entry, partial, RunOutcome::Aborted, started);
                    return;
                }
            };
        }

        // --- thinking ---
        bail_if_aborted!();
        partial
            .content
            .push(AssistantContent::thinking(String::new()));
        let thinking_idx = partial.content.len() - 1;
        ctx.emit(emit_update(AssistantMessageEvent::ThinkingStart {
            content_index: thinking_idx,
            partial: partial.clone(),
        }));
        for chunk in [
            "Looking at the request",
            " and the workspace",
            " to plan an approach.",
        ] {
            bail_if_aborted!();
            if let Some(AssistantContent::Thinking(t)) = partial.content.get_mut(thinking_idx) {
                t.thinking.push_str(chunk);
            }
            ctx.emit(emit_update(AssistantMessageEvent::ThinkingDelta {
                content_index: thinking_idx,
                delta: chunk.to_string(),
                partial: partial.clone(),
            }));
            Self::sleep(&ctx, 90).await;
        }
        let thinking_text = match partial.content.get(thinking_idx) {
            Some(AssistantContent::Thinking(t)) => t.thinking.clone(),
            _ => String::new(),
        };
        ctx.emit(emit_update(AssistantMessageEvent::ThinkingEnd {
            content_index: thinking_idx,
            content: thinking_text,
            partial: partial.clone(),
        }));

        // --- text ---
        partial.content.push(AssistantContent::text(String::new()));
        let text_idx = partial.content.len() - 1;
        ctx.emit(emit_update(AssistantMessageEvent::TextStart {
            content_index: text_idx,
            partial: partial.clone(),
        }));
        let body = stub_reply(&req.prompt);
        for chunk in chunk_text(&body, 6) {
            bail_if_aborted!();
            if let Some(AssistantContent::Text(t)) = partial.content.get_mut(text_idx) {
                t.text.push_str(&chunk);
            }
            ctx.emit(emit_update(AssistantMessageEvent::TextDelta {
                content_index: text_idx,
                delta: chunk,
                partial: partial.clone(),
            }));
            Self::sleep(&ctx, 28).await;
        }
        ctx.emit(emit_update(AssistantMessageEvent::TextEnd {
            content_index: text_idx,
            content: body.clone(),
            partial: partial.clone(),
        }));

        // --- one tool call, so the collapsed tool group renders (F1.3) ---
        bail_if_aborted!();
        let mut tool_call =
            ToolCall::new(format!("toolu_{}", uuid::Uuid::new_v4().simple()), "read");
        tool_call.arguments.insert(
            "path".to_string(),
            json!(req
                .workspace_root
                .clone()
                .unwrap_or_else(|| ".".to_string())),
        );
        partial
            .content
            .push(AssistantContent::ToolCall(tool_call.clone()));
        let tool_idx = partial.content.len() - 1;
        ctx.emit(emit_update(AssistantMessageEvent::ToolCallStart {
            content_index: tool_idx,
            partial: partial.clone(),
        }));
        // Arguments arrive as fragments, which is what exercises partial-JSON rendering.
        for frag in ["{\"path\":", "\"src/", "main.rs\"}"] {
            ctx.emit(emit_update(AssistantMessageEvent::ToolCallDelta {
                content_index: tool_idx,
                delta: frag.to_string(),
                partial: partial.clone(),
            }));
            Self::sleep(&ctx, 30).await;
        }
        ctx.emit(emit_update(AssistantMessageEvent::ToolCallEnd {
            content_index: tool_idx,
            tool_call: tool_call.clone(),
            partial: partial.clone(),
        }));

        let usage = estimate_usage(&partial);
        partial.usage = usage.clone();
        partial.stop_reason = StopReason::ToolUse;
        ctx.emit(emit_update(AssistantMessageEvent::Done {
            reason: DoneReason::ToolUse,
            message: partial.clone(),
        }));

        // --- tool execution ---
        ctx.emit(EventKind::ToolExecutionStart {
            session_id: req.session_id.clone(),
            tool_call_id: tool_call.id.clone(),
            tool_name: tool_call.name.clone(),
            args: serde_json::Value::Object(tool_call.arguments.clone()),
        });
        for progress in [0.33_f64, 0.66, 1.0] {
            bail_if_aborted!();
            ctx.emit(EventKind::ToolExecutionUpdate {
                session_id: req.session_id.clone(),
                tool_call_id: tool_call.id.clone(),
                partial_result: json!({ "progress": progress }),
            });
            Self::sleep(&ctx, 160).await;
        }
        ctx.emit(EventKind::ToolExecutionEnd {
            session_id: req.session_id.clone(),
            tool_call_id: tool_call.id.clone(),
            result: json!({ "linesAdded": 268, "linesRemoved": 0, "text": "read 268 lines" }),
            is_error: false,
        });

        finish(&ctx, &req, entry, partial, RunOutcome::Completed, started);
    }
}

fn finish(
    ctx: &Arc<dyn RunContext>,
    req: &RunRequest,
    entry: Entry,
    message: AssistantMessage,
    outcome: RunOutcome,
    started: i64,
) {
    let usage = message.usage.clone();
    let final_entry = Entry {
        kind: EntryKind::Message {
            message: Message::Assistant(message),
        },
        ..entry
    };
    ctx.replace_entry(&final_entry);
    ctx.emit(EventKind::MessageEnd {
        session_id: req.session_id.clone(),
        entry: final_entry,
    });
    ctx.emit(EventKind::TurnEnd {
        session_id: req.session_id.clone(),
        run_id: req.run_id.clone(),
        usage: usage.clone(),
    });
    ctx.emit(EventKind::RunEnd {
        session_id: req.session_id.clone(),
        run_id: req.run_id.clone(),
        outcome,
        usage,
        duration_ms: (now_ms() - started).max(0) as u64,
    });
}

/// Split on whitespace boundaries so deltas land like real token chunks.
fn chunk_text(text: &str, words_per_chunk: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut count = 0;
    for word in text.split_inclusive(char::is_whitespace) {
        current.push_str(word);
        count += 1;
        if count >= words_per_chunk {
            out.push(std::mem::take(&mut current));
            count = 0;
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// TODO(W2): the real generator produces varied markdown per spec 02 §5.
fn stub_reply(prompt: &str) -> String {
    format!(
        "I'll take a look at that.\n\n\
         You asked: **{}**\n\n\
         Here's the plan:\n\n\
         1. Read the relevant source\n\
         2. Make the change\n\
         3. Run the tests\n\n\
         ```rust\nfn main() {{\n    println!(\"hello from the stub harness\");\n}}\n```\n\n\
         Let me start by reading the file.",
        prompt.trim()
    )
}

fn estimate_usage(message: &AssistantMessage) -> Usage {
    let output = (message.text().len() as u64 / 4).max(1);
    let input = 1_200;
    Usage {
        input,
        output,
        total_tokens: input + output,
        ..Default::default()
    }
}
