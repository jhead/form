//! The deterministic demo corpus (spec 01 §6).
//!
//! This is a product surface: the Home dashboard (F11) renders it on first launch, so the
//! shape of the data has to be believable, not merely non-empty. That means a diurnal and
//! weekly rhythm rather than uniform noise, sessions that are mostly short with a long tail,
//! several models, runs that aborted or failed, tool invocations per turn, and a few real
//! image attachments.
//!
//! Given a fixed seed and a fixed anchor timestamp the output is byte-identical, so
//! screenshots and tests are stable.

pub(in crate::app) mod corpus;
pub(in crate::app) mod png;

use chrono::{DateTime, Datelike, Duration, Local, NaiveDate, TimeZone};
use rand::distributions::WeightedIndex;
use rand::prelude::*;
use rand_chacha::ChaCha8Rng;
use rusqlite::{params, Connection};

use crate::catalog;
use crate::error::Result;
use crate::protocol::{
    now_ms, AssistantContent, AssistantMessage, Attachment, Cost, Entry, EntryKind, ImageContent,
    InputContent, Message, ModelRef, RunOutcome, SessionStatus, SessionSummary, StopReason,
    ThinkingLevel, ToolCall, ToolResultMessage, Usage, UserContent, UserMessage,
};

use super::store::{
    image_dimensions, index_title, insert_attachment, insert_session, insert_turn, new_id,
    sha256_hex, touch_root, write_entry, Store, ToolInvocationRecord, TurnRecord,
};

pub const DEFAULT_SEED: u64 = 0xf0_1d_5e_ed;

/// How far back the corpus reaches. Long enough that `All` differs from `30d`, short enough
/// that the activity heatmap is dense rather than a scattering of dots.
const SPAN_DAYS: i64 = 120;

/// Relative likelihood a session starts on a given weekday, Monday first. Weekends are quiet
/// but not empty — this is somebody who codes for a living and tinkers on Sunday evening.
const WEEKDAY_WEIGHT: [f64; 7] = [1.0, 1.05, 1.0, 0.95, 0.8, 0.32, 0.4];

/// Relative likelihood a session starts in a given local hour. Morning ramp, lunch dip,
/// afternoon peak, an evening tail that thins out after midnight.
const HOUR_WEIGHT: [f64; 24] = [
    0.04, 0.02, 0.01, 0.01, 0.01, 0.02, 0.06, 0.18, 0.55, 0.95, 1.00, 0.92, 0.55, 0.70, 0.98, 1.00,
    0.90, 0.72, 0.48, 0.42, 0.50, 0.46, 0.30, 0.12,
];

const MODELS: [(&str, &str, ThinkingLevel, f64); 4] = [
    ("anthropic", "claude-opus-5", ThinkingLevel::High, 0.42),
    ("anthropic", "claude-sonnet-5", ThinkingLevel::Medium, 0.28),
    ("anthropic", "claude-opus-5", ThinkingLevel::Max, 0.12),
    ("openai", "gpt-5", ThinkingLevel::High, 0.18),
];

const FOLLOW_UPS: &[&str] = &[
    "Looks good. Can you add a test for that?",
    "What happens if it's called concurrently?",
    "Can you explain why you chose that over the alternative?",
    "Run the suite and show me the output.",
    "Tidy that up — the naming is inconsistent with the rest of the module.",
    "One more edge case: what if the input is empty?",
    "Ship it. Anything else you'd flag for review?",
    "Roll that back, I preferred the first version.",
    "Can you make it work without the extra allocation?",
    "Document the invariant so the next person doesn't undo it.",
];

const TOOL_RESULTS: &[&str] = &[
    "read 268 lines",
    "3 files changed, 41 insertions(+), 12 deletions(-)",
    "12 matches across 4 files",
    "test result: ok. 34 passed; 0 failed; 0 ignored",
    "wrote 92 lines",
    "no matches",
    "Finished `dev` profile [unoptimized + debuginfo] in 4.21s",
];

/// Sessions that get an image attachment, by index into [`corpus::TOPICS`].
const SESSIONS_WITH_IMAGES: [usize; 4] = [4, 5, 6, 18];
/// The one session left in an `error` state, so the sidebar's error dot (F2.4) is real.
const SESSION_WITH_ERROR: usize = 24;
const PINNED: [usize; 2] = [2, 13];
const ARCHIVED: [usize; 2] = [14, 22];

// ------------------------------------------------------------ planning

struct PlannedTool {
    name: String,
    started_at: i64,
    duration_ms: i64,
    is_error: bool,
    result: String,
}

struct PlannedTurn {
    started_at: i64,
    ended_at: i64,
    ttft_ms: Option<i64>,
    prompt: String,
    thinking: Option<String>,
    reply: String,
    usage: Usage,
    outcome: RunOutcome,
    tools: Vec<PlannedTool>,
    image: Option<usize>,
}

struct PlannedSession {
    topic: usize,
    id: String,
    model: ModelRef,
    created_at: i64,
    updated_at: i64,
    turns: Vec<PlannedTurn>,
}

pub fn seed(store: &Store, seed: u64, anchor_ms: i64) -> Result<()> {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let anchor = Local
        .timestamp_millis_opt(anchor_ms)
        .single()
        .unwrap_or_else(Local::now);

    let mut sessions: Vec<PlannedSession> = corpus::TOPICS
        .iter()
        .enumerate()
        .map(|(i, _)| plan_session(&mut rng, &anchor, anchor_ms, i))
        .collect();
    // Newest first, which is also the sidebar's default order (F2.1).
    sessions.sort_by_key(|s| std::cmp::Reverse(s.updated_at));

    // Image bytes are generated outside the transaction so the blob files exist before any
    // row points at them.
    let images = write_images(store, &sessions)?;

    store.with_tx(|tx| {
        let groups = write_groups(tx, anchor_ms)?;
        let mut next_idx = [0i64; corpus::GROUPS.len() + 1];
        for planned in &sessions {
            let topic = &corpus::TOPICS[planned.topic];
            let slot = topic.group.map(|g| g + 1).unwrap_or(0);
            let summary = SessionSummary {
                id: planned.id.clone(),
                title: topic.title.to_string(),
                title_is_custom: false,
                group_id: topic.group.map(|g| groups[g].clone()),
                index: next_idx[slot] as u32,
                workspace_root: topic.root.map(expand_home),
                model_ref: planned.model.clone(),
                status: if planned.topic == SESSION_WITH_ERROR {
                    SessionStatus::Error
                } else {
                    SessionStatus::Idle
                },
                message_count: 0,
                total_tokens: 0,
                archived: ARCHIVED.contains(&planned.topic),
                pinned: PINNED.contains(&planned.topic),
                created_at: planned.created_at,
                updated_at: planned.updated_at,
            };
            next_idx[slot] += 1;

            insert_session(tx, &summary)?;
            index_title(tx, &summary.id, &summary.title)?;
            if let Some(root) = &summary.workspace_root {
                touch_root(tx, root, planned.updated_at)?;
            }
            write_transcript(tx, planned, &images)?;
        }
        Ok(())
    })
}

fn plan_session(
    rng: &mut ChaCha8Rng,
    anchor: &DateTime<Local>,
    anchor_ms: i64,
    topic_idx: usize,
) -> PlannedSession {
    let topic = &corpus::TOPICS[topic_idx];
    let model = pick_model(rng);
    let caching = model.provider_id == "anthropic";

    let start = pick_start(rng, anchor);
    // Length tracks the topic: a shell one-liner is not a twenty-turn session. The scripted
    // prompt count sets the scale, and squaring a uniform draw gives the long tail — most
    // sessions are short, a few are marathons.
    let roll: f64 = rng.gen();
    let turn_count =
        ((topic.prompts.len() as f64 * (0.7 + roll * roll * 4.0)).round() as usize).clamp(2, 30);

    let mut turns = Vec::with_capacity(turn_count);
    let mut cursor = start;
    let mut context = rng.gen_range(1_400..3_200u64);
    let image_slots = if SESSIONS_WITH_IMAGES.contains(&topic_idx) {
        rng.gen_range(1..=2usize)
    } else {
        0
    };

    for t in 0..turn_count {
        // Reply, then read, then reply again: gaps are short inside a working block and
        // occasionally long when the session picks back up later.
        if t > 0 {
            let gap = if rng.gen_bool(0.12) {
                rng.gen_range(45..600) * 60_000i64
            } else {
                rng.gen_range(25..420) * 1_000i64
            };
            cursor += gap;
        }

        let outcome = match rng.gen::<f64>() {
            r if r < 0.045 => RunOutcome::Aborted,
            r if r < 0.075 => RunOutcome::Failed,
            _ => RunOutcome::Completed,
        };

        context += rng.gen_range(700..2_900);
        let usage = plan_usage(rng, &model, context, caching, outcome, t);
        let ttft_ms = match outcome {
            RunOutcome::Failed if rng.gen_bool(0.5) => None,
            _ => Some(rng.gen_range(260..2_400)),
        };
        // Output rate, tokens per second, varies by model and load.
        let rate = rng.gen_range(22.0..68.0f64);
        let duration_ms = ttft_ms.unwrap_or(400) + (usage.output as f64 / rate * 1000.0) as i64;

        let prompt = if t < topic.prompts.len() {
            topic.prompts[t].to_string()
        } else {
            FOLLOW_UPS[(topic_idx * 7 + t * 3) % FOLLOW_UPS.len()].to_string()
        };
        let thinking = (matches!(
            model.thinking_level,
            ThinkingLevel::Medium | ThinkingLevel::High | ThinkingLevel::Max
        ) && rng.gen_bool(0.55))
        .then(|| {
            corpus::fill(
                corpus::THINKING[rng.gen_range(0..corpus::THINKING.len())],
                topic.subject,
                topic.file,
            )
        });

        let tools = plan_tools(rng, cursor, duration_ms, outcome);
        let reply = plan_reply(rng, topic, outcome, !tools.is_empty());

        turns.push(PlannedTurn {
            started_at: cursor,
            ended_at: cursor + duration_ms,
            ttft_ms,
            prompt,
            thinking,
            reply,
            usage,
            outcome,
            tools,
            image: (t < image_slots).then_some(topic_idx + t),
        });
        cursor += duration_ms;
    }

    // A long session that started today can run past the anchor once the between-turn gaps
    // are added up. Sliding the whole session back keeps its internal rhythm and guarantees
    // nothing in the corpus is dated in the future.
    let overflow = turns.last().map(|t| t.ended_at).unwrap_or(start) - anchor_ms;
    if overflow > 0 {
        for turn in &mut turns {
            turn.started_at -= overflow;
            turn.ended_at -= overflow;
            for tool in &mut turn.tools {
                tool.started_at -= overflow;
            }
        }
    }

    let created_at = turns.first().map(|t| t.started_at).unwrap_or(start);
    let updated_at = turns.last().map(|t| t.ended_at).unwrap_or(start);
    PlannedSession {
        topic: topic_idx,
        id: new_id("ses"),
        model,
        created_at,
        updated_at,
        turns,
    }
}

/// A start instant with the weekly and daily rhythm baked in, plus a mild recency bias so
/// the 7-day window is never empty on a fresh install.
fn pick_start(rng: &mut ChaCha8Rng, anchor: &DateTime<Local>) -> i64 {
    let today = anchor.date_naive();
    let day_weights: Vec<f64> = (0..SPAN_DAYS)
        .map(|d| {
            let date = today - Duration::days(d);
            let weekday = date.weekday().num_days_from_monday() as usize;
            let recency = 1.0 + 0.9 * (1.0 - d as f64 / SPAN_DAYS as f64);
            WEEKDAY_WEIGHT[weekday] * recency
        })
        .collect();
    let days_ago = WeightedIndex::new(&day_weights)
        .map(|d| d.sample(rng) as i64)
        .unwrap_or(0);
    let hour = WeightedIndex::new(HOUR_WEIGHT)
        .map(|d| d.sample(rng) as u32)
        .unwrap_or(10);

    let date = today - Duration::days(days_ago);
    local_ms(date, hour, rng.gen_range(0..60), rng.gen_range(0..60))
}

fn local_ms(date: NaiveDate, hour: u32, minute: u32, second: u32) -> i64 {
    let naive = date
        .and_hms_opt(hour, minute, second)
        .or_else(|| date.and_hms_opt(12, 0, 0))
        .expect("noon always exists");
    // A DST spring-forward gap has no local instant; the next valid one is close enough.
    Local
        .from_local_datetime(&naive)
        .earliest()
        .or_else(|| Local.from_local_datetime(&naive).latest())
        .map(|dt| dt.timestamp_millis())
        .unwrap_or_else(|| naive.and_utc().timestamp_millis())
}

fn pick_model(rng: &mut ChaCha8Rng) -> ModelRef {
    let weights: Vec<f64> = MODELS.iter().map(|m| m.3).collect();
    let i = WeightedIndex::new(&weights)
        .map(|d| d.sample(rng))
        .unwrap_or(0);
    let (provider_id, model_id, thinking_level, _) = MODELS[i];
    ModelRef {
        provider_id: provider_id.to_string(),
        model_id: model_id.to_string(),
        thinking_level,
    }
}

fn plan_usage(
    rng: &mut ChaCha8Rng,
    model: &ModelRef,
    context: u64,
    caching: bool,
    outcome: RunOutcome,
    turn: usize,
) -> Usage {
    let mut input = context;
    let mut cache_read = 0;
    let mut cache_write = 0;
    if caching && turn > 0 {
        // After the first turn most of the prompt is a cache hit; the tail is new.
        cache_read = (input as f64 * rng.gen_range(0.68..0.9)) as u64;
        input -= cache_read;
        if turn % rng.gen_range(4..8) == 0 {
            cache_write = rng.gen_range(400..2_600);
        }
    } else if caching {
        cache_write = input;
        input /= 8;
    }

    let base_output: u64 = rng.gen_range(110..2_600);
    let output = match outcome {
        RunOutcome::Aborted => (base_output / rng.gen_range(2..6)).max(20),
        RunOutcome::Failed => rng.gen_range(0..90),
        RunOutcome::Completed => base_output,
    };
    let reasoning = matches!(
        model.thinking_level,
        ThinkingLevel::Medium | ThinkingLevel::High | ThinkingLevel::Max
    )
    .then(|| (output as f64 * rng.gen_range(0.15..0.55)) as u64);

    let total_tokens = input + output + cache_read + cache_write;
    Usage {
        input,
        output,
        cache_read,
        cache_write,
        cache_write_1h: None,
        reasoning,
        total_tokens,
        cost: price(model, input, output, cache_read, cache_write),
    }
}

fn price(model: &ModelRef, input: u64, output: u64, cache_read: u64, cache_write: u64) -> Cost {
    let pricing = catalog::resolve(model)
        .map(|m| m.pricing)
        .unwrap_or_default();
    let per_million = |tokens: u64, rate: f64| tokens as f64 / 1_000_000.0 * rate;
    let cost = Cost {
        input: per_million(input, pricing.input),
        output: per_million(output, pricing.output),
        cache_read: per_million(cache_read, pricing.cache_read),
        cache_write: per_million(cache_write, pricing.cache_write),
        total: 0.0,
    };
    Cost {
        total: cost.input + cost.output + cost.cache_read + cost.cache_write,
        ..cost
    }
}

fn plan_tools(
    rng: &mut ChaCha8Rng,
    started_at: i64,
    duration_ms: i64,
    outcome: RunOutcome,
) -> Vec<PlannedTool> {
    if matches!(outcome, RunOutcome::Failed) && rng.gen_bool(0.6) {
        return Vec::new();
    }
    let count = match rng.gen::<f64>() {
        r if r < 0.18 => 0,
        r if r < 0.52 => 1,
        r if r < 0.78 => 2,
        r if r < 0.93 => 3,
        _ => rng.gen_range(4..7),
    };
    (0..count)
        .map(|i| {
            let offset = duration_ms.max(1) * (i as i64 + 1) / (count as i64 + 1);
            PlannedTool {
                name: corpus::TOOLS[rng.gen_range(0..corpus::TOOLS.len())].to_string(),
                started_at: started_at + offset,
                duration_ms: rng.gen_range(18..4_200),
                is_error: rng.gen_bool(0.07),
                result: TOOL_RESULTS[rng.gen_range(0..TOOL_RESULTS.len())].to_string(),
            }
        })
        .collect()
}

fn plan_reply(
    rng: &mut ChaCha8Rng,
    topic: &corpus::Topic,
    outcome: RunOutcome,
    has_tools: bool,
) -> String {
    match outcome {
        RunOutcome::Failed => String::new(),
        RunOutcome::Aborted => corpus::fill(
            corpus::PREAMBLES[rng.gen_range(0..corpus::PREAMBLES.len())],
            topic.subject,
            topic.file,
        ),
        RunOutcome::Completed => {
            let body = corpus::fill(
                corpus::REPLIES[rng.gen_range(0..corpus::REPLIES.len())],
                topic.subject,
                topic.file,
            );
            if has_tools && rng.gen_bool(0.5) {
                let preamble = corpus::fill(
                    corpus::PREAMBLES[rng.gen_range(0..corpus::PREAMBLES.len())],
                    topic.subject,
                    topic.file,
                );
                format!("{preamble}\n\n{body}")
            } else {
                body
            }
        }
    }
}

// ------------------------------------------------------------ writing

fn write_groups(conn: &Connection, now: i64) -> Result<Vec<String>> {
    let mut ids = Vec::with_capacity(corpus::GROUPS.len());
    for (i, name) in corpus::GROUPS.iter().enumerate() {
        let id = new_id("grp");
        conn.execute(
            "INSERT INTO groups (id, name, idx, collapsed, created_at) VALUES (?1, ?2, ?3, 0, ?4)",
            params![id, name, i as i64, now],
        )?;
        ids.push(id);
    }
    Ok(ids)
}

/// Generate the demo images, write the blobs, and return the attachment records keyed by the
/// slot the transcript refers to.
fn write_images(
    store: &Store,
    sessions: &[PlannedSession],
) -> Result<Vec<(usize, Attachment, String)>> {
    use base64::Engine;
    let dir = store.data_dir.join("attachments");
    std::fs::create_dir_all(&dir)?;

    let mut out = Vec::new();
    for planned in sessions {
        for turn in &planned.turns {
            let Some(slot) = turn.image else { continue };
            let bytes = png::encode_gradient(240, 160, (slot % 4) as u8);
            let sha256 = sha256_hex(&bytes);
            let path = dir.join(&sha256);
            if !path.exists() {
                std::fs::write(&path, &bytes)?;
            }
            let (width, height) = image_dimensions(&bytes).unzip();
            let attachment = Attachment {
                id: new_id("att"),
                session_id: Some(planned.id.clone()),
                sha256,
                filename: format!("screenshot-{slot}.png"),
                mime: "image/png".to_string(),
                bytes: bytes.len() as u64,
                width,
                height,
                path: path.to_string_lossy().into_owned(),
                thumb_path: None,
                created_at: turn.started_at,
            };
            let data = base64::engine::general_purpose::STANDARD.encode(&bytes);
            out.push((slot, attachment, data));
        }
    }
    Ok(out)
}

fn write_transcript(
    conn: &Connection,
    planned: &PlannedSession,
    images: &[(usize, Attachment, String)],
) -> Result<()> {
    let topic = &corpus::TOPICS[planned.topic];
    let model = &planned.model;
    let mut seq = 0u64;
    let mut parent: Option<String> = None;
    let mut messages = 0i64;
    let mut total_tokens = 0u64;

    let push = |conn: &Connection,
                kind: EntryKind,
                timestamp: i64,
                parent: &mut Option<String>,
                seq: &mut u64|
     -> Result<String> {
        let entry = Entry {
            id: new_id("ent"),
            session_id: planned.id.clone(),
            seq: *seq,
            parent_id: parent.clone(),
            timestamp,
            kind,
        };
        write_entry(conn, &entry)?;
        *seq += 1;
        *parent = Some(entry.id.clone());
        Ok(entry.id)
    };

    for turn in &planned.turns {
        // --- the user's message, with an image inline when this turn carries one ---
        let attachment = turn.image.and_then(|slot| {
            images.iter().find(|(s, a, _)| {
                *s == slot && a.session_id.as_deref() == Some(planned.id.as_str())
            })
        });
        let content = match attachment {
            Some((_, _, data)) => UserContent::Blocks(vec![
                InputContent::text(turn.prompt.clone()),
                InputContent::Image(ImageContent {
                    data: data.clone(),
                    mime_type: "image/png".to_string(),
                }),
            ]),
            None => UserContent::Text(turn.prompt.clone()),
        };
        if let Some((_, a, _)) = attachment {
            insert_attachment(conn, a)?;
        }
        push(
            conn,
            EntryKind::Message {
                message: Message::User(UserMessage {
                    content,
                    timestamp: turn.started_at,
                }),
            },
            turn.started_at,
            &mut parent,
            &mut seq,
        )?;
        messages += 1;

        // --- the assistant's reply ---
        let mut blocks = Vec::new();
        if let Some(thinking) = &turn.thinking {
            blocks.push(AssistantContent::thinking(thinking.clone()));
        }
        if !turn.reply.is_empty() {
            blocks.push(AssistantContent::text(turn.reply.clone()));
        }
        let tool_calls: Vec<ToolCall> = turn
            .tools
            .iter()
            .map(|tool| {
                let mut call = ToolCall::new(new_id("toolu"), tool.name.clone());
                call.arguments
                    .insert("path".to_string(), serde_json::json!(topic.file));
                call
            })
            .collect();
        blocks.extend(tool_calls.iter().cloned().map(AssistantContent::ToolCall));

        let assistant = AssistantMessage {
            content: blocks,
            api: api_for(&model.provider_id).to_string(),
            provider: model.provider_id.clone(),
            model: model.model_id.clone(),
            response_id: Some(new_id("msg")),
            diagnostics: None,
            usage: turn.usage.clone(),
            stop_reason: match turn.outcome {
                RunOutcome::Completed if !tool_calls.is_empty() => StopReason::ToolUse,
                RunOutcome::Completed => StopReason::Stop,
                RunOutcome::Aborted => StopReason::Aborted,
                RunOutcome::Failed => StopReason::Error,
            },
            error_message: matches!(turn.outcome, RunOutcome::Failed)
                .then(|| "upstream returned 529 overloaded_error".to_string()),
            timestamp: turn.ended_at,
        };
        push(
            conn,
            EntryKind::Message {
                message: Message::Assistant(assistant),
            },
            turn.ended_at,
            &mut parent,
            &mut seq,
        )?;
        messages += 1;
        total_tokens += turn.usage.total_tokens;

        // --- tool results, so the collapsed tool group (F1.3) has detail to expand ---
        for (call, tool) in tool_calls.iter().zip(&turn.tools) {
            push(
                conn,
                EntryKind::Message {
                    message: Message::ToolResult(ToolResultMessage {
                        tool_call_id: call.id.clone(),
                        tool_name: tool.name.clone(),
                        content: vec![InputContent::text(tool.result.clone())],
                        details: None,
                        is_error: tool.is_error,
                        timestamp: tool.started_at + tool.duration_ms,
                    }),
                },
                tool.started_at + tool.duration_ms,
                &mut parent,
                &mut seq,
            )?;
        }

        // --- the metrics row the stats engine reads ---
        let mut record = TurnRecord::new(planned.id.clone(), new_id("run"), model.clone());
        record.started_at = turn.started_at;
        record.ended_at = turn.ended_at;
        record.ttft_ms = turn.ttft_ms;
        record.duration_ms = turn.ended_at - turn.started_at;
        record.usage = turn.usage.clone();
        record.outcome = turn.outcome;
        record.tools = turn
            .tools
            .iter()
            .map(|tool| ToolInvocationRecord {
                tool_name: tool.name.clone(),
                started_at: tool.started_at,
                duration_ms: tool.duration_ms,
                is_error: tool.is_error,
            })
            .collect();
        insert_turn(conn, &record)?;
    }

    conn.execute(
        "UPDATE sessions SET message_count = ?2, total_tokens = ?3 WHERE id = ?1",
        params![planned.id, messages, total_tokens as i64],
    )?;
    Ok(())
}

fn api_for(provider: &str) -> &'static str {
    match provider {
        "openai" => "openai-responses",
        _ => "anthropic-messages",
    }
}

/// Roots in the corpus are written with `~` so they read naturally; the store holds absolute
/// paths because that is what confinement resolves against.
fn expand_home(path: &str) -> String {
    match path.strip_prefix("~/") {
        Some(rest) => std::env::var("HOME")
            .map(|home| format!("{home}/{rest}"))
            .unwrap_or_else(|_| path.to_string()),
        None => path.to_string(),
    }
}

/// Convenience for callers that just want "populate it if it's empty, with the stock seed".
pub fn seed_if_empty(store: &Store) -> Result<bool> {
    if !store.is_empty()? {
        return Ok(false);
    }
    seed(store, DEFAULT_SEED, now_ms())?;
    Ok(true)
}

#[cfg(test)]
mod inspect {
    use super::*;

    /// Not an assertion — a readable dump of the corpus so a human can sanity-check the
    /// shape the dashboard will render. Run with `-- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn dump_corpus_shape() {
        let dir = std::env::temp_dir().join("form-corpus-dump");
        let _ = std::fs::remove_dir_all(&dir);
        let store = Store::open(&dir).unwrap();
        seed(&store, DEFAULT_SEED, now_ms()).unwrap();

        let list = store.list_sessions(true).unwrap();
        println!(
            "{} groups, {} sessions",
            list.groups.len(),
            list.sessions.len()
        );
        for s in &list.sessions {
            println!(
                "  [{}] {:<48} {:>3} msgs {:>8} tok  {}",
                s.index, s.title, s.message_count, s.total_tokens, s.model_ref.model_id
            );
        }
        store
            .with_conn(|conn| {
                let (turns, tools, cost): (i64, i64, f64) = conn.query_row(
                    "SELECT (SELECT COUNT(*) FROM turns), (SELECT COUNT(*) FROM tool_invocations),
                            (SELECT SUM(cost_total) FROM turns)",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )?;
                println!("{turns} turns, {tools} tool calls, ${cost:.2} total");
                let mut stmt = conn.prepare(
                    "SELECT outcome, COUNT(*) FROM turns GROUP BY outcome ORDER BY 2 DESC",
                )?;
                let rows = stmt
                    .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                println!("outcomes: {rows:?}");
                Ok(())
            })
            .unwrap();
        // Sum the whole data dir: the WAL is not checkpointed and the blobs live beside it.
        let bytes: u64 = walk(&dir);
        println!("data dir: {} KB", bytes / 1024);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn walk(dir: &std::path::Path) -> u64 {
        std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| {
                let path = e.path();
                if path.is_dir() {
                    walk(&path)
                } else {
                    std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0)
                }
            })
            .sum()
    }
}
