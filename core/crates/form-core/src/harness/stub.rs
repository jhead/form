//! The generator itself.
//!
//! Reading order: [`Runner::run`] is the agent loop, [`Runner::turn`] is one assistant
//! response plus its tool calls. Everything else is cadence, accounting, or the abort path.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Map, Value};

use crate::app::{ToolInvocationRecord, TurnRecord};
use crate::catalog::{self, Model};
use crate::context::{self, estimate_tokens};
use crate::protocol::{
    now_ms, AssistantContent, AssistantMessage, AssistantMessageEvent, Cost, DoneReason, Entry,
    EntryKind, ErrorReason, EventKind, InputContent, Message, RunOutcome, Session, SessionStatus,
    SessionSummary, StopReason, ThinkingLevel, ToolCall, ToolResultMessage, Usage,
};

use super::plan::{self, TurnPlan, MAX_TURNS};
use super::tools::PlannedTool;
use super::{AbortSignal, Harness, RunContext, RunRequest};

/// Longest the run can go without noticing an abort. Spec 02 §3 budgets 100 ms; sleeping in
/// slices this size is what makes `abortRun` land between events rather than between turns.
const ABORT_POLL_MS: u64 = 20;

/// Deterministic mock agent. Emits the protocol `pi-agent` emits, with seeded content.
pub struct StubHarness;

#[async_trait::async_trait]
impl Harness for StubHarness {
    async fn run(&self, req: RunRequest, ctx: Arc<dyn RunContext>, abort: AbortSignal) {
        Runner::new(req, ctx, abort).run().await;
    }
}

/// Outcome of one turn, from the loop's point of view.
enum Step {
    Continue,
    Failed,
    /// The assistant stream itself ended `error { reason: aborted }`.
    Aborted,
    /// The abort arrived while tools were executing, so the *next* turn is the one that
    /// carries `error { reason: aborted }` — which is what `pi` does, since the assistant
    /// message had already terminated.
    AbortedInTools,
}

struct TurnMeta {
    started_at: i64,
    ttft_ms: Option<i64>,
}

struct Runner {
    req: RunRequest,
    ctx: Arc<dyn RunContext>,
    abort: AbortSignal,
    api: &'static str,
    /// `None` for a model the catalog does not know; cost then reads as zero rather than
    /// as an invented number.
    model: Option<Model>,
    started: i64,
    /// Accumulated across every turn; this is what `run_end` reports.
    total: Usage,
    /// The system prompt and tool schemas, in tokens — the fixed head of every request and
    /// the part the prompt cache is actually about (F11.10).
    prompt_overhead: u64,
    /// Everything the provider would see on the next request, in tokens.
    context_tokens: u64,
    /// The part of that already covered by the prompt cache.
    cached_prefix: u64,
    /// The newest message, which is never cached yet — billed as uncached input.
    fresh_tokens: u64,
}

impl Runner {
    fn new(req: RunRequest, ctx: Arc<dyn RunContext>, abort: AbortSignal) -> Self {
        Self {
            api: api_for(&req.model.provider_id),
            model: catalog::resolve(&req.model),
            started: now_ms(),
            total: Usage::default(),
            prompt_overhead: ctx
                .prompt_overhead_tokens()
                .unwrap_or_else(|| default_prompt_overhead(req.workspace_root.as_deref())),
            context_tokens: 0,
            cached_prefix: 0,
            fresh_tokens: 0,
            req,
            ctx,
            abort,
        }
    }

    // ------------------------------------------------------------ the loop

    async fn run(mut self) {
        self.emit(EventKind::RunStart {
            session_id: self.req.session_id.clone(),
            run_id: self.req.run_id.clone(),
        });

        // The user's message is already in the transcript — `Core` appends it before the run
        // starts so the UI can render it without waiting — but it still costs input tokens.
        let prompt = self.req.prompt.clone();
        self.note_message(&prompt);

        let mut turn_index = self.req.turn_index;
        let mut planned: VecDeque<TurnPlan> = self.plan(turn_index).into();
        let mut pending: Option<String> = None;
        let mut emitted = 0u32;
        let mut outcome = RunOutcome::Completed;

        loop {
            // Checked every turn boundary, which is where a message queued mid-run joins the
            // transcript (F1.7).
            if pending.is_none() {
                pending = self.ctx.take_queued_prompt();
            }
            if planned.is_empty() {
                if pending.is_none() || emitted >= MAX_TURNS {
                    break;
                }
                planned.extend(self.plan(turn_index));
            }
            if emitted >= MAX_TURNS {
                break;
            }
            let Some(turn) = planned.pop_front() else {
                break;
            };

            match self.turn(turn, pending.take()).await {
                Step::Continue => {}
                Step::Failed => {
                    outcome = RunOutcome::Failed;
                    break;
                }
                Step::Aborted => {
                    outcome = RunOutcome::Aborted;
                    break;
                }
                Step::AbortedInTools => {
                    self.aborted_turn();
                    outcome = RunOutcome::Aborted;
                    break;
                }
            }
            turn_index += 1;
            emitted += 1;
        }

        self.emit(EventKind::RunEnd {
            session_id: self.req.session_id.clone(),
            run_id: self.req.run_id.clone(),
            outcome,
            usage: self.total.clone(),
            duration_ms: (now_ms() - self.started).max(0) as u64,
        });
    }

    fn plan(&self, turn_index: u32) -> Vec<TurnPlan> {
        plan::plan_run(
            &self.req.session_id,
            turn_index,
            &self.req.model.model_id,
            self.req.workspace_root.as_deref(),
            &self.req.prompt,
            self.req.model.thinking_level != ThinkingLevel::Off,
        )
    }

    // ------------------------------------------------------------ one turn

    async fn turn(&mut self, plan: TurnPlan, injected: Option<String>) -> Step {
        let session_id = self.req.session_id.clone();
        let meta = TurnMeta {
            started_at: now_ms(),
            ttft_ms: None,
        };
        self.emit(EventKind::TurnStart {
            session_id: session_id.clone(),
            run_id: self.req.run_id.clone(),
        });

        if let Some(text) = injected {
            self.append_user(&text);
        }

        let mut partial = AssistantMessage::pending(
            self.api,
            self.req.model.provider_id.as_str(),
            self.req.model.model_id.as_str(),
        );
        let Some(entry) = self.ctx.append_entry(
            &session_id,
            EntryKind::Message {
                message: Message::Assistant(partial.clone()),
            },
        ) else {
            // The session went away underneath us. Nothing to report it on but the run end.
            return Step::Failed;
        };
        self.emit(EventKind::MessageStart {
            session_id: session_id.clone(),
            entry: entry.clone(),
        });
        self.emit_update(
            &entry.id,
            AssistantMessageEvent::Start {
                partial: partial.clone(),
            },
        );

        if !self.sleep(plan.ttft_ms).await {
            return self.conclude(&meta, entry, partial, ErrorReason::Aborted, ABORTED);
        }
        let meta = TurnMeta {
            ttft_ms: Some((now_ms() - meta.started_at).max(0)),
            ..meta
        };

        // --- thinking (F6.3) ---
        if let Some(thinking) = plan.thinking.clone() {
            partial
                .content
                .push(AssistantContent::thinking(String::new()));
            let index = partial.content.len() - 1;
            self.emit_update(
                &entry.id,
                AssistantMessageEvent::ThinkingStart {
                    content_index: index,
                    partial: partial.clone(),
                },
            );
            for chunk in chunk_words(&thinking, plan.words_per_delta) {
                if let Some(AssistantContent::Thinking(t)) = partial.content.get_mut(index) {
                    t.thinking.push_str(&chunk);
                }
                self.emit_update(
                    &entry.id,
                    AssistantMessageEvent::ThinkingDelta {
                        content_index: index,
                        delta: chunk,
                        partial: partial.clone(),
                    },
                );
                // Reasoning streams roughly twice as fast as prose.
                if !self.sleep(plan.delta_ms / 2).await {
                    return self.conclude(&meta, entry, partial, ErrorReason::Aborted, ABORTED);
                }
            }
            self.emit_update(
                &entry.id,
                AssistantMessageEvent::ThinkingEnd {
                    content_index: index,
                    content: thinking,
                    partial: partial.clone(),
                },
            );
        }

        // --- text ---
        let body = plan.response.text.clone();
        let chunks = chunk_words(&body, plan.words_per_delta);
        // A failing turn dies part-way through the prose, the way an upstream 500 does.
        let cutoff = if plan.failure.is_some() {
            (chunks.len() * 2 / 5).max(1)
        } else {
            chunks.len()
        };
        partial.content.push(AssistantContent::text(String::new()));
        let index = partial.content.len() - 1;
        self.emit_update(
            &entry.id,
            AssistantMessageEvent::TextStart {
                content_index: index,
                partial: partial.clone(),
            },
        );
        for chunk in chunks.into_iter().take(cutoff) {
            if let Some(AssistantContent::Text(t)) = partial.content.get_mut(index) {
                t.text.push_str(&chunk);
            }
            self.emit_update(
                &entry.id,
                AssistantMessageEvent::TextDelta {
                    content_index: index,
                    delta: chunk,
                    partial: partial.clone(),
                },
            );
            if !self.sleep(plan.delta_ms).await {
                return self.conclude(&meta, entry, partial, ErrorReason::Aborted, ABORTED);
            }
        }

        if let Some(reason) = plan.failure {
            // No `text_end`: the content block never closed, which is exactly what the UI
            // has to cope with when a provider drops mid-stream.
            return self.conclude(&meta, entry, partial, ErrorReason::Error, reason);
        }

        let text = match partial.content.get(index) {
            Some(AssistantContent::Text(t)) => t.text.clone(),
            _ => String::new(),
        };
        self.emit_update(
            &entry.id,
            AssistantMessageEvent::TextEnd {
                content_index: index,
                content: text,
                partial: partial.clone(),
            },
        );

        // --- tool calls ---
        for tool in &plan.tools {
            if !self
                .stream_tool_call(&entry.id, &mut partial, tool, plan.delta_ms)
                .await
            {
                return self.conclude(&meta, entry, partial, ErrorReason::Aborted, ABORTED);
            }
        }

        let reason = if plan.response.truncated {
            DoneReason::Length
        } else if plan.tools.is_empty() {
            DoneReason::Stop
        } else {
            DoneReason::ToolUse
        };
        partial.stop_reason = match reason {
            DoneReason::Stop => StopReason::Stop,
            DoneReason::Length => StopReason::Length,
            DoneReason::ToolUse => StopReason::ToolUse,
            DoneReason::Deferred => StopReason::Deferred,
        };
        partial.usage = self.usage_for(&partial);
        let usage = partial.usage.clone();
        self.emit_update(
            &entry.id,
            AssistantMessageEvent::Done {
                reason,
                message: partial.clone(),
            },
        );
        self.finalize(entry, partial);

        // --- tool execution ---
        let mut aborted = false;
        let mut invocations = Vec::with_capacity(plan.tools.len());
        for tool in &plan.tools {
            if !self.execute_tool(tool, &mut invocations).await {
                aborted = true;
                break;
            }
        }

        self.end_turn(
            &meta,
            usage,
            if aborted {
                RunOutcome::Aborted
            } else {
                RunOutcome::Completed
            },
            invocations,
        );

        if aborted {
            Step::AbortedInTools
        } else {
            Step::Continue
        }
    }

    /// Arguments arrive as fragments — the only thing that exercises partial-JSON rendering
    /// in the argument summary row. Returns `false` if the run was aborted.
    async fn stream_tool_call(
        &self,
        entry_id: &str,
        partial: &mut AssistantMessage,
        tool: &PlannedTool,
        delta_ms: u64,
    ) -> bool {
        let mut call = ToolCall::new(tool.id.clone(), tool.name);
        partial
            .content
            .push(AssistantContent::ToolCall(call.clone()));
        let index = partial.content.len() - 1;
        self.emit_update(
            entry_id,
            AssistantMessageEvent::ToolCallStart {
                content_index: index,
                partial: partial.clone(),
            },
        );

        let encoded = serde_json::to_string(&Value::Object(tool.args.clone())).unwrap_or_default();
        let mut seen = String::new();
        for fragment in fragments(&encoded, 4 + (tool.args.len() % 7)) {
            seen.push_str(&fragment);
            // Best-effort salvage, like a real client's incremental parser: the arguments
            // only materialise once the accumulated fragment happens to be valid JSON.
            if let Ok(args) = serde_json::from_str::<Map<String, Value>>(&seen) {
                call.arguments = args;
                if let Some(AssistantContent::ToolCall(c)) = partial.content.get_mut(index) {
                    c.arguments = call.arguments.clone();
                }
            }
            self.emit_update(
                entry_id,
                AssistantMessageEvent::ToolCallDelta {
                    content_index: index,
                    delta: fragment,
                    partial: partial.clone(),
                },
            );
            if !self.sleep(delta_ms.max(30)).await {
                return false;
            }
        }

        call.arguments = tool.args.clone();
        if let Some(AssistantContent::ToolCall(c)) = partial.content.get_mut(index) {
            c.arguments = call.arguments.clone();
        }
        self.emit_update(
            entry_id,
            AssistantMessageEvent::ToolCallEnd {
                content_index: index,
                tool_call: call,
                partial: partial.clone(),
            },
        );
        true
    }

    /// Returns `false` if the run was aborted part-way through.
    async fn execute_tool(
        &mut self,
        tool: &PlannedTool,
        invocations: &mut Vec<ToolInvocationRecord>,
    ) -> bool {
        let session_id = self.req.session_id.clone();
        let started_at = now_ms();
        self.emit(EventKind::ToolExecutionStart {
            session_id: session_id.clone(),
            tool_call_id: tool.id.clone(),
            tool_name: tool.name.to_string(),
            args: Value::Object(tool.args.clone()),
        });

        let ticks = tool.progress.len().max(1) as u64;
        let mut aborted = false;
        for update in &tool.progress {
            if !self.sleep(tool.exec_ms / ticks).await {
                aborted = true;
                break;
            }
            self.emit(EventKind::ToolExecutionUpdate {
                session_id: session_id.clone(),
                tool_call_id: tool.id.clone(),
                partial_result: update.clone(),
            });
        }

        let (result, text, is_error) = if aborted {
            (
                serde_json::json!({ "aborted": true }),
                ABORTED.to_string(),
                true,
            )
        } else {
            (tool.result.clone(), tool.text.clone(), tool.is_error)
        };

        self.emit(EventKind::ToolExecutionEnd {
            session_id: session_id.clone(),
            tool_call_id: tool.id.clone(),
            result: result.clone(),
            is_error,
        });

        let message = Message::ToolResult(ToolResultMessage {
            tool_call_id: tool.id.clone(),
            tool_name: tool.name.to_string(),
            content: vec![InputContent::text(text.clone())],
            details: Some(result),
            is_error,
            timestamp: now_ms(),
        });
        if let Some(entry) = self
            .ctx
            .append_entry(&session_id, EntryKind::Message { message })
        {
            self.emit(EventKind::MessageStart {
                session_id: session_id.clone(),
                entry: entry.clone(),
            });
            self.emit(EventKind::MessageEnd {
                session_id: session_id.clone(),
                entry,
            });
        }
        self.note_message(&text);

        invocations.push(ToolInvocationRecord {
            tool_name: tool.name.to_string(),
            started_at,
            duration_ms: (now_ms() - started_at).max(0),
            is_error,
        });

        !aborted
    }

    /// A turn that exists only to carry `error { reason: aborted }`, emitted when the abort
    /// arrived after the assistant message had already terminated.
    fn aborted_turn(&mut self) {
        let session_id = self.req.session_id.clone();
        let meta = TurnMeta {
            started_at: now_ms(),
            ttft_ms: None,
        };
        self.emit(EventKind::TurnStart {
            session_id: session_id.clone(),
            run_id: self.req.run_id.clone(),
        });

        let partial = AssistantMessage::pending(
            self.api,
            self.req.model.provider_id.as_str(),
            self.req.model.model_id.as_str(),
        );
        let Some(entry) = self.ctx.append_entry(
            &session_id,
            EntryKind::Message {
                message: Message::Assistant(partial.clone()),
            },
        ) else {
            return;
        };
        self.emit(EventKind::MessageStart {
            session_id,
            entry: entry.clone(),
        });
        self.emit_update(
            &entry.id,
            AssistantMessageEvent::Start {
                partial: partial.clone(),
            },
        );
        self.conclude(&meta, entry, partial, ErrorReason::Aborted, ABORTED);
    }

    // ------------------------------------------------------------ endings

    /// Terminate the assistant stream with an error, close the message, and end the turn.
    /// Failures are values on the stream, never an error out of `dispatch` (spec 02 §3).
    fn conclude(
        &mut self,
        meta: &TurnMeta,
        entry: Entry,
        mut partial: AssistantMessage,
        reason: ErrorReason,
        detail: &str,
    ) -> Step {
        partial.stop_reason = match reason {
            ErrorReason::Aborted => StopReason::Aborted,
            ErrorReason::Error => StopReason::Error,
        };
        partial.error_message = Some(detail.to_string());
        partial.usage = self.usage_for(&partial);
        let usage = partial.usage.clone();
        self.emit_update(
            &entry.id,
            AssistantMessageEvent::Error {
                reason,
                error: partial.clone(),
            },
        );
        self.finalize(entry, partial);
        match reason {
            ErrorReason::Aborted => {
                self.end_turn(meta, usage, RunOutcome::Aborted, Vec::new());
                Step::Aborted
            }
            ErrorReason::Error => {
                self.end_turn(meta, usage, RunOutcome::Failed, Vec::new());
                Step::Failed
            }
        }
    }

    fn finalize(&self, entry: Entry, message: AssistantMessage) {
        let final_entry = Entry {
            kind: EntryKind::Message {
                message: Message::Assistant(message),
            },
            ..entry
        };
        self.ctx.replace_entry(&final_entry);
        self.emit(EventKind::MessageEnd {
            session_id: self.req.session_id.clone(),
            entry: final_entry,
        });
    }

    fn end_turn(
        &mut self,
        meta: &TurnMeta,
        usage: Usage,
        outcome: RunOutcome,
        tools: Vec<ToolInvocationRecord>,
    ) {
        self.total = self.total.add(&usage);
        self.emit(EventKind::TurnEnd {
            session_id: self.req.session_id.clone(),
            run_id: self.req.run_id.clone(),
            usage: usage.clone(),
        });
        let ended_at = now_ms();
        let mut record = TurnRecord::new(
            self.req.session_id.clone(),
            self.req.run_id.clone(),
            self.req.model.clone(),
        );
        record.started_at = meta.started_at;
        record.ended_at = ended_at;
        record.ttft_ms = meta.ttft_ms;
        record.duration_ms = (ended_at - meta.started_at).max(0);
        record.usage = usage;
        record.outcome = outcome;
        record.tools = tools;
        self.ctx.record_turn(record);
    }

    // ------------------------------------------------------------ plumbing

    fn emit(&self, kind: EventKind) {
        self.ctx.emit(kind);
    }

    fn emit_update(&self, entry_id: &str, event: AssistantMessageEvent) {
        self.ctx.emit(EventKind::MessageUpdate {
            session_id: self.req.session_id.clone(),
            entry_id: entry_id.to_string(),
            event,
        });
    }

    fn append_user(&mut self, text: &str) {
        let session_id = self.req.session_id.clone();
        let message = Message::User(crate::protocol::UserMessage::text(text));
        if let Some(entry) = self
            .ctx
            .append_entry(&session_id, EntryKind::Message { message })
        {
            self.emit(EventKind::MessageStart {
                session_id: session_id.clone(),
                entry: entry.clone(),
            });
            self.emit(EventKind::MessageEnd { session_id, entry });
        }
        self.note_message(text);
    }

    fn note_message(&mut self, text: &str) {
        let tokens = estimate_tokens(text);
        self.context_tokens += tokens;
        self.fresh_tokens += tokens;
    }

    /// Sleep `ms` scaled by the speed multiplier, polling the abort signal throughout.
    /// Returns `false` once aborted.
    async fn sleep(&self, ms: u64) -> bool {
        if self.abort.is_aborted() {
            return false;
        }
        let mut remaining = (ms as f64 / self.ctx.speed().max(0.01)).round() as u64;
        if remaining == 0 {
            // Still yield: at 100× a whole run would otherwise never hand the runtime back,
            // and the abort would land only when the run finished.
            tokio::task::yield_now().await;
            return !self.abort.is_aborted();
        }
        while remaining > 0 {
            let step = remaining.min(ABORT_POLL_MS);
            tokio::time::sleep(Duration::from_millis(step)).await;
            remaining -= step;
            if self.abort.is_aborted() {
                return false;
            }
        }
        true
    }

    /// Token counts for the turn, priced from the catalog so the turn footer, the context
    /// ring and the Home dashboard all report the same figures (spec 02 §6).
    fn usage_for(&mut self, message: &AssistantMessage) -> Usage {
        let mut output = 0u64;
        let mut reasoning = 0u64;
        for block in &message.content {
            match block {
                AssistantContent::Text(t) => output += estimate_tokens(&t.text),
                AssistantContent::Thinking(t) => {
                    let tokens = estimate_tokens(&t.thinking);
                    output += tokens;
                    reasoning += tokens;
                }
                AssistantContent::ToolCall(c) => {
                    let encoded = serde_json::to_string(&c.arguments).unwrap_or_default();
                    // Plus the call envelope the provider bills for.
                    output += estimate_tokens(&encoded) + 8;
                }
            }
        }

        let full = self.prompt_overhead + self.context_tokens;
        let cache_read = self.cached_prefix.min(full);
        let remainder = full - cache_read;
        let input = self.fresh_tokens.min(remainder);
        let cache_write = remainder - input;

        // Everything except the freshest message is cached for the next turn, so later turns
        // report both a read and a small write (F11.10).
        self.cached_prefix = cache_read + cache_write;
        self.fresh_tokens = 0;
        self.context_tokens += output;

        let mut usage = Usage {
            input,
            output,
            cache_read,
            cache_write,
            cache_write_1h: None,
            reasoning: (reasoning > 0).then_some(reasoning),
            total_tokens: input + output + cache_read + cache_write,
            cost: Cost::default(),
        };
        usage.cost = self
            .model
            .as_ref()
            .map(|m| catalog::price(m, &usage))
            .unwrap_or_default();
        usage
    }
}

const ABORTED: &str = "aborted by user";

/// The fixed head of a request, counted with W4's estimator so the number the harness bills
/// and the number the context ring shows are the same number. `resolve_system_prompt` reads
/// nothing from the session but its workspace root, so a shim carrying that is enough — the
/// user's own additions from settings only arrive if the [`RunContext`] supplies them.
fn default_prompt_overhead(workspace_root: Option<&str>) -> u64 {
    let session = Session {
        summary: SessionSummary {
            id: String::new(),
            title: String::new(),
            title_is_custom: false,
            group_id: None,
            index: 0,
            workspace_root: workspace_root.map(str::to_string),
            model_ref: crate::app::default_model_ref(),
            status: SessionStatus::Idle,
            message_count: 0,
            total_tokens: 0,
            archived: false,
            pinned: false,
            created_at: 0,
            updated_at: 0,
        },
        entries: Vec::new(),
    };
    context::system_prompt_tokens(&session, "") + context::tool_schema_tokens()
}

fn api_for(provider_id: &str) -> &'static str {
    match provider_id {
        "anthropic" => "anthropic-messages",
        "google" => "google-generative-ai",
        "openai" => "openai-responses",
        _ => "openai-completions",
    }
}

/// Split on whitespace boundaries so deltas land like real token chunks. Fences and tables
/// straddle chunks as a result, which is the point — that is what F7.3 has to survive.
fn chunk_words(text: &str, words_per_chunk: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut count = 0;
    for word in text.split_inclusive(char::is_whitespace) {
        current.push_str(word);
        count += 1;
        if count >= words_per_chunk.max(1) {
            out.push(std::mem::take(&mut current));
            count = 0;
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// Split a JSON string into `parts` fragments at char boundaries.
fn fragments(text: &str, parts: usize) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return vec![String::new()];
    }
    let parts = parts.clamp(1, chars.len());
    let size = chars.len().div_ceil(parts);
    chars
        .chunks(size)
        .map(|chunk| chunk.iter().collect())
        .collect()
}
