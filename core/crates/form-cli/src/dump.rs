//! `protocol-dump` — one JSON fixture per protocol variant.
//!
//! These files are the tripwire for Swift/Rust drift (spec 00 §8): the Swift test target
//! decodes every one into its own type, re-encodes, and diffs. That only works if the
//! values are *representative* — an all-zeros struct serializes the same whether or not a
//! field name is misspelled on one side, so every field here carries a distinctive value.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{json, Map, Value};

use form_core::protocol::*;
use form_core::settings::Settings;

// Fixed, obviously-fake identifiers: a diff between two dumps should show a protocol change,
// never a fresh uuid.
const SESSION: &str = "ses_9f2c14aa7b0e4d1e";
const OTHER_SESSION: &str = "ses_1b77d0e5a4c34f92";
const RUN: &str = "run_5c8e21d4b9f04a37";
const COMMAND: &str = "cmd_7be31a90c5d24e88";
const ENTRY: &str = "ent_3d5f8821aa094c60";
const PARENT_ENTRY: &str = "ent_0c19b4e77f2d4a15";
const GROUP: &str = "grp_41ab77c2e8ff4b03";
const TOOL_CALL: &str = "toolu_0af31d6c";
const ATTACHMENT: &str = "att_88c1e0f39d7b4a25";
const TIMESTAMP: TimestampMs = 1_755_112_233_444;

// ---------------------------------------------------------------- building blocks

fn model_ref() -> ModelRef {
    ModelRef {
        provider_id: "anthropic".to_string(),
        model_id: "claude-sonnet-4-5".to_string(),
        thinking_level: ThinkingLevel::High,
    }
}

fn cost() -> Cost {
    Cost {
        input: 0.037_44,
        output: 0.034_65,
        cache_read: 0.002_46,
        cache_write: 0.003_84,
        total: 0.078_39,
    }
}

fn usage() -> Usage {
    Usage {
        input: 12_480,
        output: 2_310,
        cache_read: 8_192,
        cache_write: 1_024,
        cache_write_1h: Some(512),
        reasoning: Some(880),
        total_tokens: 24_506,
        cost: cost(),
    }
}

fn tool_call() -> ToolCall {
    let mut arguments = Map::new();
    arguments.insert("path".to_string(), json!("src/server/health.rs"));
    arguments.insert("startLine".to_string(), json!(42));
    ToolCall {
        id: TOOL_CALL.to_string(),
        name: "read".to_string(),
        arguments,
        thought_signature: Some("sig_9d31c0".to_string()),
        namespace: Some("fs".to_string()),
    }
}

/// Every content block kind in one message, so the untagged/tagged mix is exercised.
fn assistant_message() -> AssistantMessage {
    AssistantMessage {
        content: vec![
            AssistantContent::Text(TextContent {
                text: "I'll add the health check endpoint.".to_string(),
                text_signature: Some("sig_text_44a1".to_string()),
            }),
            AssistantContent::Thinking(ThinkingContent {
                thinking: "The router is registered in main.rs.".to_string(),
                thinking_signature: Some("sig_think_71b9".to_string()),
                redacted: true,
            }),
            AssistantContent::ToolCall(tool_call()),
        ],
        api: "anthropic-messages".to_string(),
        provider: "anthropic".to_string(),
        model: "claude-sonnet-4-5".to_string(),
        response_id: Some("msg_01Hx9QpR2tVw".to_string()),
        diagnostics: Some(vec![AssistantMessageDiagnostic {
            code: "cache_miss".to_string(),
            message: "prompt cache was cold for this turn".to_string(),
            detail: Some(json!({ "prefixTokens": 8192 })),
            timestamp: Some(TIMESTAMP - 1_200),
        }]),
        usage: usage(),
        stop_reason: StopReason::ToolUse,
        error_message: None,
        timestamp: TIMESTAMP,
    }
}

/// The failure shape: `errorMessage` populated, which the happy path leaves absent.
fn failed_message() -> AssistantMessage {
    AssistantMessage {
        content: vec![AssistantContent::text("Partial answer before the failure.")],
        stop_reason: StopReason::Error,
        error_message: Some("upstream returned 529 overloaded".to_string()),
        ..assistant_message()
    }
}

fn partial_message() -> AssistantMessage {
    AssistantMessage {
        content: vec![AssistantContent::text("I'll add the heal")],
        stop_reason: StopReason::Pending,
        usage: Usage::default(),
        diagnostics: None,
        ..assistant_message()
    }
}

/// A user message in its block form, so `UserContent`'s untagged image branch is covered.
fn user_message() -> UserMessage {
    UserMessage {
        content: UserContent::Blocks(vec![
            InputContent::Text(TextContent {
                text: "Add a /healthz endpoint. See the screenshot.".to_string(),
                text_signature: None,
            }),
            InputContent::Image(ImageContent {
                data: "iVBORw0KGgoAAAANSUhEUg==".to_string(),
                mime_type: "image/png".to_string(),
            }),
        ]),
        timestamp: TIMESTAMP - 5_000,
    }
}

fn tool_result_message() -> ToolResultMessage {
    ToolResultMessage {
        tool_call_id: TOOL_CALL.to_string(),
        tool_name: "read".to_string(),
        content: vec![InputContent::text(
            "268 lines read from src/server/health.rs",
        )],
        details: Some(json!({ "linesRead": 268, "truncated": false })),
        is_error: false,
        timestamp: TIMESTAMP + 900,
    }
}

fn user_entry() -> Entry {
    Entry {
        id: PARENT_ENTRY.to_string(),
        session_id: SESSION.to_string(),
        seq: 6,
        parent_id: None,
        timestamp: TIMESTAMP - 5_000,
        kind: EntryKind::Message {
            message: Message::User(user_message()),
        },
    }
}

fn assistant_entry() -> Entry {
    Entry {
        id: ENTRY.to_string(),
        session_id: SESSION.to_string(),
        seq: 7,
        parent_id: Some(PARENT_ENTRY.to_string()),
        timestamp: TIMESTAMP,
        kind: EntryKind::Message {
            message: Message::Assistant(assistant_message()),
        },
    }
}

fn session_summary() -> SessionSummary {
    SessionSummary {
        id: SESSION.to_string(),
        title: "Health check endpoint".to_string(),
        title_is_custom: true,
        group_id: Some(GROUP.to_string()),
        index: 3,
        workspace_root: Some("/Users/x/dev/api".to_string()),
        model_ref: model_ref(),
        status: SessionStatus::Streaming,
        message_count: 18,
        total_tokens: 148_320,
        archived: false,
        pinned: true,
        created_at: TIMESTAMP - 86_400_000,
        updated_at: TIMESTAMP,
    }
}

fn session_group() -> SessionGroup {
    SessionGroup {
        id: GROUP.to_string(),
        name: "API work".to_string(),
        index: 1,
        collapsed: true,
    }
}

fn context_usage() -> ContextUsage {
    ContextUsage {
        session_id: SESSION.to_string(),
        used: 61_440,
        total: 200_000,
        segments: vec![
            ContextSegment {
                kind: SegmentKind::System,
                tokens: 1_820,
            },
            ContextSegment {
                kind: SegmentKind::Tools,
                tokens: 4_310,
            },
            ContextSegment {
                kind: SegmentKind::Transcript,
                tokens: 46_990,
            },
            ContextSegment {
                kind: SegmentKind::Attachments,
                tokens: 4_320,
            },
            ContextSegment {
                kind: SegmentKind::OutputReserve,
                tokens: 4_000,
            },
        ],
        cost: cost(),
        message_count: 18,
    }
}

fn attachment() -> Attachment {
    Attachment {
        id: ATTACHMENT.to_string(),
        session_id: Some(SESSION.to_string()),
        sha256: "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08".to_string(),
        filename: "dashboard.png".to_string(),
        mime: "image/png".to_string(),
        bytes: 284_913,
        width: Some(1_512),
        height: Some(982),
        path: "/Users/x/Library/Application Support/form/attachments/9f86d0.png".to_string(),
        thumb_path: Some(
            "/Users/x/Library/Application Support/form/attachments/9f86d0.thumb.png".to_string(),
        ),
        created_at: TIMESTAMP - 3_600_000,
    }
}

/// The real default document — so the fixture tracks whatever shape W4's `Settings` has —
/// with the fields that default to `""`, `0`, `false` or an omitted `Option` overwritten.
/// Those are exactly the fields a Swift decoder can get wrong without anyone noticing.
fn settings_document() -> Value {
    let mut doc = serde_json::to_value(Settings::default()).unwrap_or_else(|_| json!({}));

    let set = |doc: &mut Value, path: &[&str], value: Value| {
        let mut cursor = doc;
        let Some((last, parents)) = path.split_last() else {
            return;
        };
        for key in parents {
            match cursor.get_mut(*key) {
                Some(next) => cursor = next,
                None => return,
            }
        }
        if let Some(object) = cursor.as_object_mut() {
            object.insert((*last).to_string(), value);
        }
    };

    set(&mut doc, &["general", "telemetry"], json!(true));
    set(&mut doc, &["appearance", "textSizeMultiplier"], json!(1.15));
    set(&mut doc, &["appearance", "sidebarCollapsed"], json!(true));
    set(
        &mut doc,
        &["defaults", "systemPrompt"],
        json!("Prefer small diffs and cite file paths."),
    );
    set(&mut doc, &["editor", "wrapCode"], json!(true));
    set(&mut doc, &["editor", "font"], json!("Berkeley Mono"));
    set(
        &mut doc,
        &["advanced", "dataDir"],
        json!("/Users/x/Library/Application Support/form"),
    );
    set(&mut doc, &["advanced", "harnessSpeed"], json!(2.5));
    // `baseUrlOverride` is skipped when absent, so without this the field never appears in a
    // fixture at all and Swift's decoding of it is never exercised.
    set(
        &mut doc,
        &["providers", "ollama", "baseUrlOverride"],
        json!("http://127.0.0.1:11434"),
    );
    set(&mut doc, &["providers", "anthropic", "hasKey"], json!(true));
    set(
        &mut doc,
        &["shortcuts"],
        json!({ "session.new": "cmd+n", "chat.send": "cmd+return" }),
    );
    doc
}

// ---------------------------------------------------------------- samples

pub fn commands() -> Vec<Command> {
    vec![
        Command::CreateSession {
            group_id: Some(GROUP.to_string()),
            title: Some("Health check endpoint".to_string()),
            workspace_root: Some("/Users/x/dev/api".to_string()),
            model_ref: Some(model_ref()),
        },
        Command::SendPrompt {
            session_id: SESSION.to_string(),
            text: "Add a /healthz endpoint and a test for it.".to_string(),
            attachment_ids: vec![ATTACHMENT.to_string()],
        },
        Command::AbortRun {
            session_id: SESSION.to_string(),
        },
        Command::RenameSession {
            session_id: SESSION.to_string(),
            title: "Health check endpoint".to_string(),
        },
        Command::DeleteSession {
            session_id: SESSION.to_string(),
        },
        Command::ArchiveSession {
            session_id: SESSION.to_string(),
            archived: true,
        },
        Command::PinSession {
            session_id: SESSION.to_string(),
            pinned: true,
        },
        Command::MoveSession {
            session_id: SESSION.to_string(),
            group_id: Some(GROUP.to_string()),
            index: 2,
        },
        Command::CreateGroup {
            name: "API work".to_string(),
        },
        Command::RenameGroup {
            group_id: GROUP.to_string(),
            name: "API work".to_string(),
        },
        Command::DeleteGroup {
            group_id: GROUP.to_string(),
        },
        Command::ReorderGroup {
            group_id: GROUP.to_string(),
            index: 1,
        },
        Command::SetGroupCollapsed {
            group_id: GROUP.to_string(),
            collapsed: true,
        },
        Command::SetSessionModel {
            session_id: SESSION.to_string(),
            model_ref: model_ref(),
        },
        Command::SetWorkspaceRoot {
            session_id: SESSION.to_string(),
            path: Some("/Users/x/dev/api".to_string()),
        },
        Command::UpdateSettings {
            settings: settings_document(),
        },
        Command::AddAttachment {
            session_id: Some(SESSION.to_string()),
            path: Some("/Users/x/Desktop/dashboard.png".to_string()),
            bytes_base64: Some("iVBORw0KGgoAAAANSUhEUg==".to_string()),
            filename: "dashboard.png".to_string(),
            mime: "image/png".to_string(),
        },
        Command::RemoveAttachment {
            attachment_id: ATTACHMENT.to_string(),
        },
        Command::BranchFromMessage {
            session_id: SESSION.to_string(),
            entry_id: ENTRY.to_string(),
        },
        Command::RetryMessage {
            session_id: SESSION.to_string(),
            entry_id: ENTRY.to_string(),
        },
    ]
}

pub fn queries() -> Vec<Query> {
    vec![
        Query::ListSessions {
            include_archived: true,
        },
        Query::GetSession {
            session_id: SESSION.to_string(),
        },
        Query::SearchSessions {
            q: "health check".to_string(),
            limit: Some(25),
        },
        Query::SearchInSession {
            session_id: SESSION.to_string(),
            q: "healthz".to_string(),
        },
        Query::GetSettings,
        Query::GetCatalog,
        Query::GetStats {
            range: StatsRange::D30,
            tz: "Europe/London".to_string(),
        },
        Query::GetContextUsage {
            session_id: SESSION.to_string(),
        },
        Query::RenderMarkdown {
            text: "# Plan\n\n1. Add the route\n2. Test it\n".to_string(),
            complete: Some(false),
        },
        Query::ResolvePath {
            session_id: SESSION.to_string(),
            path: "src/server/health.rs".to_string(),
        },
        Query::GetAttachment {
            attachment_id: ATTACHMENT.to_string(),
        },
        Query::ListRecentRoots,
    ]
}

/// Full [`Event`] envelopes, not bare kinds — that is what crosses the boundary. A few carry
/// no `commandId` so the omitted-optional shape is covered too (spec 00 §1.5).
pub fn events() -> Vec<Event> {
    let with = |kind| Event {
        timestamp: TIMESTAMP,
        command_id: Some(COMMAND.to_string()),
        kind,
    };
    let without = |kind| Event {
        timestamp: TIMESTAMP,
        command_id: None,
        kind,
    };

    vec![
        with(EventKind::RunStart {
            session_id: SESSION.to_string(),
            run_id: RUN.to_string(),
        }),
        with(EventKind::TurnStart {
            session_id: SESSION.to_string(),
            run_id: RUN.to_string(),
        }),
        with(EventKind::MessageStart {
            session_id: SESSION.to_string(),
            entry: user_entry(),
        }),
        with(EventKind::MessageUpdate {
            session_id: SESSION.to_string(),
            entry_id: ENTRY.to_string(),
            event: AssistantMessageEvent::TextDelta {
                content_index: 0,
                delta: "th endpoint".to_string(),
                partial: partial_message(),
            },
        }),
        with(EventKind::MessageEnd {
            session_id: SESSION.to_string(),
            entry: assistant_entry(),
        }),
        with(EventKind::ToolExecutionStart {
            session_id: SESSION.to_string(),
            tool_call_id: TOOL_CALL.to_string(),
            tool_name: "read".to_string(),
            args: json!({ "path": "src/server/health.rs", "startLine": 42 }),
        }),
        with(EventKind::ToolExecutionUpdate {
            session_id: SESSION.to_string(),
            tool_call_id: TOOL_CALL.to_string(),
            partial_result: json!({ "progress": 0.66, "bytesRead": 18_204 }),
        }),
        with(EventKind::ToolExecutionEnd {
            session_id: SESSION.to_string(),
            tool_call_id: TOOL_CALL.to_string(),
            result: json!({ "text": "read 268 lines", "linesAdded": 268, "linesRemoved": 0 }),
            is_error: false,
        }),
        with(EventKind::TurnEnd {
            session_id: SESSION.to_string(),
            run_id: RUN.to_string(),
            usage: usage(),
        }),
        with(EventKind::RunEnd {
            session_id: SESSION.to_string(),
            run_id: RUN.to_string(),
            outcome: RunOutcome::Completed,
            usage: usage(),
            duration_ms: 8_412,
        }),
        with(EventKind::SessionCreated {
            session: session_summary(),
        }),
        without(EventKind::SessionUpdated {
            session: SessionSummary {
                id: OTHER_SESSION.to_string(),
                title: "Untitled".to_string(),
                title_is_custom: false,
                group_id: None,
                index: 0,
                workspace_root: None,
                status: SessionStatus::Idle,
                pinned: false,
                archived: true,
                ..session_summary()
            },
        }),
        with(EventKind::SessionDeleted {
            session_id: SESSION.to_string(),
        }),
        without(EventKind::GroupsChanged {
            groups: vec![
                session_group(),
                SessionGroup {
                    id: "grp_5e0a91bb7c114d38".to_string(),
                    name: "Scratch".to_string(),
                    index: 2,
                    collapsed: false,
                },
            ],
        }),
        with(EventKind::SettingsChanged {
            settings: settings_document(),
        }),
        without(EventKind::ContextUsageChanged {
            usage: context_usage(),
        }),
        without(EventKind::StatsInvalidated),
        with(EventKind::AttachmentAdded {
            attachment: attachment(),
        }),
        with(EventKind::AttachmentRemoved {
            attachment_id: ATTACHMENT.to_string(),
        }),
        without(EventKind::Error {
            code: "attachment_rejected".to_string(),
            message: "dashboard.png is 284913 bytes, over the 262144 byte limit".to_string(),
            detail: Some(json!({ "attachmentId": ATTACHMENT, "limitBytes": 262_144 })),
        }),
    ]
}

/// `AssistantMessageEvent` is nested inside `message_update`, so a fixture per outer event
/// would only ever cover one of its twelve variants. Swift renders the transcript from these
/// deltas, which makes them the highest-traffic types on the boundary.
pub fn message_events() -> Vec<AssistantMessageEvent> {
    vec![
        AssistantMessageEvent::Start {
            // Not `AssistantMessage::pending`: that stamps `now_ms()`, and a fixture that
            // changes on every dump is a diff nobody can read.
            partial: AssistantMessage {
                content: Vec::new(),
                ..partial_message()
            },
        },
        AssistantMessageEvent::TextStart {
            content_index: 0,
            partial: partial_message(),
        },
        AssistantMessageEvent::TextDelta {
            content_index: 0,
            delta: "th endpoint".to_string(),
            partial: partial_message(),
        },
        AssistantMessageEvent::TextEnd {
            content_index: 0,
            content: "I'll add the health check endpoint.".to_string(),
            partial: partial_message(),
        },
        AssistantMessageEvent::ThinkingStart {
            content_index: 1,
            partial: partial_message(),
        },
        AssistantMessageEvent::ThinkingDelta {
            content_index: 1,
            delta: " is registered in main.rs.".to_string(),
            partial: partial_message(),
        },
        AssistantMessageEvent::ThinkingEnd {
            content_index: 1,
            content: "The router is registered in main.rs.".to_string(),
            partial: partial_message(),
        },
        AssistantMessageEvent::ToolCallStart {
            content_index: 2,
            partial: partial_message(),
        },
        AssistantMessageEvent::ToolCallDelta {
            content_index: 2,
            delta: "{\"path\":\"src/server/".to_string(),
            partial: partial_message(),
        },
        AssistantMessageEvent::ToolCallEnd {
            content_index: 2,
            tool_call: tool_call(),
            partial: partial_message(),
        },
        AssistantMessageEvent::Done {
            reason: DoneReason::ToolUse,
            message: assistant_message(),
        },
        AssistantMessageEvent::Error {
            reason: ErrorReason::Error,
            error: failed_message(),
        },
    ]
}

/// Entries that never appear in a live stub run but do appear in a stored transcript, so
/// `getSession` can hand Swift any of them.
pub fn entries() -> Vec<Entry> {
    let base = assistant_entry();
    vec![
        user_entry(),
        base.clone(),
        Entry {
            id: "ent_7c02af9915d44b71".to_string(),
            seq: 8,
            kind: EntryKind::Message {
                message: Message::ToolResult(tool_result_message()),
            },
            ..base.clone()
        },
        Entry {
            id: "ent_a41d5e8827bc4903".to_string(),
            seq: 9,
            kind: EntryKind::ModelChange {
                provider: "openai".to_string(),
                model_id: "gpt-5".to_string(),
            },
            ..base.clone()
        },
        Entry {
            id: "ent_b90cf1a233ee4d17".to_string(),
            seq: 10,
            kind: EntryKind::ThinkingLevelChange {
                thinking_level: "xhigh".to_string(),
            },
            ..base.clone()
        },
        Entry {
            id: "ent_c15e7d4488aa4f26".to_string(),
            seq: 11,
            kind: EntryKind::Compaction {
                summary: "Earlier turns condensed: routing, tests, and the deploy script."
                    .to_string(),
                tokens_before: 142_880,
            },
            ..base.clone()
        },
        Entry {
            id: "ent_d27b0a6699cc4e35".to_string(),
            seq: 12,
            kind: EntryKind::BranchSummary {
                from_id: PARENT_ENTRY.to_string(),
                summary: "Branched to try the middleware approach instead.".to_string(),
            },
            ..base.clone()
        },
        Entry {
            id: "ent_e38c1b7700dd4a44".to_string(),
            seq: 13,
            kind: EntryKind::Custom {
                custom_type: "workspaceRootChanged".to_string(),
                data: Some(json!({ "from": "/Users/x/dev/api", "to": "/Users/x/dev/api/server" })),
            },
            ..base
        },
    ]
}

// ---------------------------------------------------------------- writing

/// Serializes each sample and pairs it with the filename it goes under.
///
/// A macro rather than a generic function because `form-cli` does not depend on `serde`
/// directly and so cannot name the `Serialize` bound; at a call site it is inferred.
macro_rules! group {
    ($root:expr, $dir:expr, $items:expr, $name:expr) => {
        write_group(
            $root,
            $dir,
            $items
                .iter()
                .filter_map(|item| serde_json::to_value(item).ok())
                .map(|value| ($name(&value), value))
                .collect(),
        )
    };
}

/// Each fixture is named for its wire tag, so a missing variant is obvious from `ls`.
pub fn write_all(root: &Path) -> std::io::Result<usize> {
    let mut written = 0;
    written += group!(root, "commands", commands(), tag_of)?;
    written += group!(root, "queries", queries(), tag_of)?;
    written += group!(root, "events", events(), tag_of)?;
    written += group!(root, "messageEvents", message_events(), tag_of)?;
    written += group!(root, "entries", entries(), entry_file_name)?;
    Ok(written)
}

fn write_group(root: &Path, dir: &str, items: Vec<(String, Value)>) -> std::io::Result<usize> {
    let dir = root.join(dir);
    std::fs::create_dir_all(&dir)?;
    for (name, value) in &items {
        let mut json = serde_json::to_string_pretty(value).unwrap_or_default();
        json.push('\n');
        std::fs::write(dir.join(format!("{name}.json")), json)?;
    }
    Ok(items.len())
}

/// The `type` tag serde wrote, read back off the serialized value — no second hand-kept list
/// of names to fall out of step with the enum.
pub fn tag_of(value: &Value) -> String {
    value["type"].as_str().unwrap_or("unknown").to_string()
}

/// Entries all carry the `message` tag for three different message roles, so name those by
/// role; the remaining entry kinds are already distinct.
fn entry_file_name(value: &Value) -> String {
    match tag_of(value).as_str() {
        "message" => format!(
            "message.{}",
            value["message"]["role"].as_str().unwrap_or("unknown")
        ),
        other => other.to_string(),
    }
}

/// A document whose `type` no variant will ever match, used to make serde list them.
pub const UNKNOWN_TAG_PROBE: &str = r#"{"type":"__enumerate__"}"#;

/// Every `type` tag serde will accept, recovered from the error it raises for an unknown
/// one. Beats a hand-maintained list: a variant added to `protocol.rs` shows up here without
/// anyone remembering to update anything, which is the whole point of a completeness test.
pub fn wire_tags(unknown_variant_error: &serde_json::Error) -> Vec<String> {
    let message = unknown_variant_error.to_string();
    let Some(list) = message.split("expected one of ").nth(1) else {
        panic!("serde did not enumerate the variants: {message}");
    };
    list.split('`')
        .skip(1)
        .step_by(2)
        .map(str::to_string)
        .collect()
}

/// The tags serde accepts for a protocol enum.
macro_rules! tags {
    ($ty:ty) => {
        wire_tags(
            &serde_json::from_str::<$ty>(UNKNOWN_TAG_PROBE)
                .expect_err("the probe tag must not match a real variant"),
        )
    };
}

/// The filenames the dump will write for a set of samples.
macro_rules! file_names {
    ($items:expr, $name:expr) => {
        $items
            .iter()
            .filter_map(|item| serde_json::to_value(item).ok())
            .map(|value| $name(&value))
            .collect::<Vec<String>>()
    };
}

/// `<dir>/<tag>` for every protocol variant no sample covers. `protocol-dump` refuses to
/// write an incomplete set, so the tool cannot quietly produce a fixture directory that the
/// Swift side would then treat as the whole protocol.
pub fn missing_variants() -> Vec<String> {
    let groups: Vec<(&str, Vec<String>, Vec<String>)> = vec![
        ("commands", tags!(Command), file_names!(commands(), tag_of)),
        ("queries", tags!(Query), file_names!(queries(), tag_of)),
        ("events", tags!(EventKind), file_names!(events(), tag_of)),
        (
            "messageEvents",
            tags!(AssistantMessageEvent),
            file_names!(message_events(), tag_of),
        ),
        (
            "entries",
            tags!(EntryKind),
            file_names!(entries(), entry_file_name),
        ),
    ];

    groups
        .into_iter()
        .flat_map(|(dir, tags, names)| {
            tags.into_iter()
                .filter(move |tag| {
                    // `EntryKind::Message` fans out into one file per message role.
                    !names
                        .iter()
                        .any(|n| n == tag || n.starts_with(&format!("{tag}.")))
                })
                .map(move |tag| format!("{dir}/{tag}"))
        })
        .collect()
}

/// A short human summary for the terminal.
pub fn summary(root: &Path) -> String {
    let counts: BTreeMap<&str, usize> = [
        ("commands", commands().len()),
        ("queries", queries().len()),
        ("events", events().len()),
        ("messageEvents", message_events().len()),
        ("entries", entries().len()),
    ]
    .into_iter()
    .collect();
    let body = counts
        .iter()
        .map(|(k, v)| format!("{v} {k}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("{body}\n{}", root.display())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixtures() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/protocol")
    }

    /// The completeness half of spec 06 §4: enumerate the protocol's variants and fail if a
    /// fixture is missing, then prove each one survives a decode/re-encode round trip. A
    /// renamed field shows up as a difference, because serde drops what it does not know.
    fn check_group(dir: &str, tags: Vec<String>, decode: fn(&str) -> serde_json::Result<Value>) {
        let root = fixtures().join(dir);
        for tag in tags {
            let path = root.join(format!("{tag}.json"));
            assert!(
                path.exists(),
                "no fixture for `{tag}` — run `cargo run --bin form-cli -- protocol-dump` \
                 and commit {}",
                path.display()
            );
            let raw = std::fs::read_to_string(&path).expect("fixture is readable");
            let as_written: Value = serde_json::from_str(&raw)
                .unwrap_or_else(|e| panic!("{} is not valid JSON: {e}", path.display()));
            let round_tripped =
                decode(&raw).unwrap_or_else(|e| panic!("{} does not decode: {e}", path.display()));
            assert_eq!(
                as_written,
                round_tripped,
                "{} does not survive a round trip — a field name has drifted",
                path.display()
            );
        }
    }

    macro_rules! decoder {
        ($ty:ty) => {
            |raw: &str| serde_json::from_str::<$ty>(raw).and_then(serde_json::to_value)
        };
    }

    #[test]
    fn every_command_has_a_fixture() {
        check_group("commands", tags!(Command), decoder!(Command));
    }

    #[test]
    fn every_query_has_a_fixture() {
        check_group("queries", tags!(Query), decoder!(Query));
    }

    /// Event fixtures are whole [`Event`] envelopes — that is what crosses the boundary —
    /// but the tags come from the flattened [`EventKind`].
    #[test]
    fn every_event_has_a_fixture() {
        check_group("events", tags!(EventKind), decoder!(Event));
    }

    #[test]
    fn every_message_event_has_a_fixture() {
        check_group(
            "messageEvents",
            tags!(AssistantMessageEvent),
            decoder!(AssistantMessageEvent),
        );
    }

    /// `EntryKind::Message` covers three message roles, so the filenames are not one-to-one
    /// with the tags; check the names the dump actually writes.
    #[test]
    fn every_entry_kind_has_a_fixture() {
        check_group(
            "entries",
            file_names!(entries(), entry_file_name),
            decoder!(Entry),
        );
    }

    /// A fixture that changes on every dump is noise in every diff — and `now_ms()` inside a
    /// sample constructor is an easy way to get one.
    #[test]
    fn samples_are_deterministic() {
        let once = serde_json::to_value(message_events()).expect("serializes");
        let twice = serde_json::to_value(message_events()).expect("serializes");
        assert_eq!(once, twice, "a sample carries a wall-clock value");
        assert_eq!(
            serde_json::to_value(events()).ok(),
            serde_json::to_value(events()).ok()
        );
        assert_eq!(
            serde_json::to_value(entries()).ok(),
            serde_json::to_value(entries()).ok()
        );
    }

    /// The completeness check `protocol-dump` runs on itself before writing.
    #[test]
    fn the_samples_cover_every_protocol_variant() {
        let missing = missing_variants();
        assert!(missing.is_empty(), "no sample for: {}", missing.join(", "));
    }

    /// An all-zeros struct hides a misspelled field name, because the decoder's default and
    /// the encoder's value look identical on the wire. Every command field must therefore
    /// carry something distinctive.
    #[test]
    fn samples_use_representative_values() {
        for command in commands() {
            let value = serde_json::to_value(&command).expect("commands serialize");
            let object = value.as_object().expect("commands are objects");
            for (key, field) in object {
                if key == "type" {
                    continue;
                }
                assert!(
                    !is_default_ish(field),
                    "{}.{key} is a default value — fixtures must be distinctive",
                    tag_of(&value)
                );
            }
        }
    }

    fn is_default_ish(value: &Value) -> bool {
        match value {
            Value::Null => true,
            Value::String(s) => s.is_empty(),
            Value::Number(n) => n.as_f64() == Some(0.0),
            Value::Bool(b) => !b,
            Value::Array(a) => a.is_empty(),
            Value::Object(o) => o.is_empty(),
        }
    }
}
