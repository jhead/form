use base64::engine::general_purpose::STANDARD;
use base64::Engine;

use super::*;
use crate::catalog;
use crate::protocol::{
    now_ms, AssistantContent, AssistantMessage, Attachment, Entry, ImageContent, StopReason,
    TextContent, ToolCall, ToolResultMessage, Usage, UserMessage,
};

// ---------------------------------------------------------------- fixtures

fn png_bytes(width: u32, height: u32) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"\x89PNG\r\n\x1a\n");
    out.extend_from_slice(&13u32.to_be_bytes());
    out.extend_from_slice(b"IHDR");
    out.extend_from_slice(&width.to_be_bytes());
    out.extend_from_slice(&height.to_be_bytes());
    out.extend_from_slice(&[8, 6, 0, 0, 0]);
    out
}

fn jpeg_bytes(width: u16, height: u16) -> Vec<u8> {
    let mut out = vec![0xff, 0xd8];
    // An APP0 segment first, so the scan has to walk past something.
    out.extend_from_slice(&[0xff, 0xe0, 0x00, 0x10]);
    out.extend_from_slice(b"JFIF\0");
    out.extend_from_slice(&[0; 9]);
    // SOF0: length, precision, height, width, components.
    out.extend_from_slice(&[0xff, 0xc0, 0x00, 0x11, 0x08]);
    out.extend_from_slice(&height.to_be_bytes());
    out.extend_from_slice(&width.to_be_bytes());
    out.push(3);
    out
}

fn session(entries: Vec<EntryKind>) -> Session {
    let now = now_ms();
    let entries = entries
        .into_iter()
        .enumerate()
        .map(|(i, kind)| Entry {
            id: format!("ent_{i}"),
            session_id: "ses_fixture".to_string(),
            seq: i as u64,
            parent_id: (i > 0).then(|| format!("ent_{}", i - 1)),
            timestamp: now + i as i64,
            kind,
        })
        .collect();
    Session {
        summary: crate::protocol::SessionSummary {
            id: "ses_fixture".to_string(),
            title: "Fixture".to_string(),
            title_is_custom: false,
            group_id: None,
            index: 0,
            workspace_root: Some("/Users/x/dev/form".to_string()),
            model_ref: catalog::default_ref(),
            status: crate::protocol::SessionStatus::Idle,
            message_count: 0,
            total_tokens: 0,
            archived: false,
            pinned: false,
            created_at: now,
            updated_at: now,
        },
        entries,
    }
}

fn user(text: &str) -> EntryKind {
    EntryKind::Message {
        message: Message::User(UserMessage::text(text)),
    }
}

fn assistant(text: &str, usage: Usage) -> EntryKind {
    let mut message = AssistantMessage::pending("anthropic-messages", "anthropic", "claude-opus-5");
    message.content.push(AssistantContent::text(text));
    message.content.push(AssistantContent::thinking(
        "The user wants the health endpoint; check the router first.",
    ));
    let mut call = ToolCall::new("toolu_1", "read");
    call.arguments
        .insert("path".to_string(), serde_json::json!("src/main.rs"));
    message.content.push(AssistantContent::ToolCall(call));
    message.usage = usage;
    message.stop_reason = StopReason::ToolUse;
    EntryKind::Message {
        message: Message::Assistant(message),
    }
}

fn tool_result(text: &str) -> EntryKind {
    EntryKind::Message {
        message: Message::ToolResult(ToolResultMessage {
            tool_call_id: "toolu_1".to_string(),
            tool_name: "read".to_string(),
            content: vec![InputContent::Text(TextContent {
                text: text.to_string(),
                text_signature: None,
            })],
            details: None,
            is_error: false,
            timestamp: now_ms(),
        }),
    }
}

fn image_message(width: u32, height: u32) -> EntryKind {
    EntryKind::Message {
        message: Message::User(UserMessage {
            content: UserContent::Blocks(vec![
                InputContent::text("Why does this look wrong?"),
                InputContent::Image(ImageContent {
                    data: STANDARD.encode(png_bytes(width, height)),
                    mime_type: "image/png".to_string(),
                }),
            ]),
            timestamp: now_ms(),
        }),
    }
}

fn fixture_transcript() -> Session {
    session(vec![
        user("Add a health check endpoint and wire it into the router."),
        assistant(
            "I'll look at the router first, then add the handler.",
            Usage {
                input: 1_200,
                output: 340,
                cache_read: 3_400,
                cache_write: 800,
                total_tokens: 5_740,
                ..Default::default()
            },
        ),
        tool_result("fn main() { /* 268 lines */ }"),
        user("Looks good — run the tests."),
        assistant(
            "Tests pass: 41 passed, 0 failed.",
            Usage {
                input: 900,
                output: 120,
                total_tokens: 1_020,
                ..Default::default()
            },
        ),
    ])
}

fn opus() -> Model {
    catalog::resolve(&catalog::default_ref()).unwrap()
}

// ---------------------------------------------------------------- estimator

#[test]
fn estimate_tokens_is_the_shared_estimator() {
    assert_eq!(estimate_tokens(""), 0);
    assert_eq!(estimate_tokens("abcd"), 1);
    assert_eq!(estimate_tokens("abcde"), 2);
    // CJK counts double before the divide, so it lands near one token per character.
    assert!(estimate_tokens("日本語のテキスト") > estimate_tokens("abcdefgh"));
    assert!(estimate_tokens(&"x".repeat(4_000)) == 1_000);
}

// ---------------------------------------------------------------- usage

#[test]
fn segments_sum_to_used_over_a_fixture_transcript() {
    let session = fixture_transcript();
    let model = opus();
    let usage = context_usage(&session, Some(&model));

    let sum: u64 = usage.segments.iter().map(|s| s.tokens).sum();
    assert_eq!(usage.used, sum);
    assert_eq!(usage.total, model.context_window);
    assert_eq!(usage.message_count, 4, "two user, two assistant");

    let by_kind = |kind: SegmentKind| {
        usage
            .segments
            .iter()
            .find(|s| s.kind == kind)
            .map(|s| s.tokens)
            .unwrap()
    };
    assert!(by_kind(SegmentKind::System) > 0, "system prompt is counted");
    assert!(by_kind(SegmentKind::Tools) > 0, "tool schemas are counted");
    assert!(by_kind(SegmentKind::Transcript) > 0);
    assert_eq!(by_kind(SegmentKind::Attachments), 0, "no images here");
    assert_eq!(by_kind(SegmentKind::OutputReserve), model.max_output);

    // Every segment kind appears exactly once, in the order the popover renders them.
    let kinds: Vec<SegmentKind> = usage.segments.iter().map(|s| s.kind).collect();
    assert_eq!(
        kinds,
        vec![
            SegmentKind::System,
            SegmentKind::Tools,
            SegmentKind::Transcript,
            SegmentKind::Attachments,
            SegmentKind::OutputReserve,
        ]
    );
}

#[test]
fn used_saturates_at_the_window_instead_of_overflowing() {
    let mut session = fixture_transcript();
    session.entries.push(Entry {
        id: "ent_big".to_string(),
        session_id: "ses_fixture".to_string(),
        seq: 99,
        parent_id: None,
        timestamp: now_ms(),
        kind: user(&"lorem ipsum dolor sit amet ".repeat(20_000)),
    });

    let small = catalog::resolve(&crate::protocol::ModelRef {
        provider_id: "ollama".into(),
        model_id: "qwen3:32b".into(),
        thinking_level: crate::protocol::ThinkingLevel::Off,
    })
    .unwrap();

    let usage = context_usage(&session, Some(&small));
    let sum: u64 = usage.segments.iter().map(|s| s.tokens).sum();
    assert!(sum > usage.total, "the fixture must overflow this window");
    assert_eq!(usage.used, usage.total);
    assert_eq!(usage.fraction(), 1.0);
}

#[test]
fn an_unresolved_model_reports_the_raw_sum() {
    let usage = context_usage(&fixture_transcript(), None);
    let sum: u64 = usage.segments.iter().map(|s| s.tokens).sum();
    assert_eq!(usage.total, 0);
    assert_eq!(usage.used, sum);
    assert_eq!(usage.fraction(), 0.0, "no window means no fraction to draw");
}

#[test]
fn an_empty_session_still_pays_for_prompt_and_tools() {
    let model = opus();
    let usage = context_usage(&session(vec![]), Some(&model));
    assert_eq!(usage.message_count, 0);
    assert_eq!(
        usage.used,
        system_prompt_tokens(&session(vec![]), "") + tool_schema_tokens() + model.max_output
    );
    assert!(usage.fraction() > 0.0 && usage.fraction() < 1.0);
}

#[test]
fn the_system_segment_follows_the_resolved_prompt() {
    let session = fixture_transcript();
    let base = context_usage(&session, Some(&opus()));
    let with_custom = context_usage_with(
        &session,
        Some(&opus()),
        &ContextOptions {
            system_prompt: "Always answer in French. ".repeat(20),
            include_tools: true,
            ..ContextOptions::default()
        },
    );
    let system_of = |u: &ContextUsage| {
        u.segments
            .iter()
            .find(|s| s.kind == SegmentKind::System)
            .unwrap()
            .tokens
    };
    assert!(system_of(&with_custom) > system_of(&base));

    // The workspace root is part of the resolved prompt, so an unconfined session differs.
    let mut unconfined = session.clone();
    unconfined.summary.workspace_root = None;
    assert_ne!(
        system_prompt_tokens(&unconfined, ""),
        system_prompt_tokens(&session, "")
    );
    assert!(resolve_system_prompt(&session, "").contains("/Users/x/dev/form"));
}

#[test]
fn tools_can_be_switched_off() {
    let session = fixture_transcript();
    let usage = context_usage_with(&session, Some(&opus()), &ContextOptions::default());
    let tools = usage
        .segments
        .iter()
        .find(|s| s.kind == SegmentKind::Tools)
        .unwrap()
        .tokens;
    assert_eq!(tools, 0);
    assert!(tool_schema_tokens() > 500, "the real schemas are not free");
    assert_eq!(tools::count(), tools::names().len());
    assert!(tools::names().contains(&"read".to_string()));
}

// ---------------------------------------------------------------- attachments

#[test]
fn transcript_images_are_charged_by_dimension() {
    let small = context_usage(&session(vec![image_message(200, 200)]), Some(&opus()));
    let large = context_usage(&session(vec![image_message(2400, 1800)]), Some(&opus()));
    let attachments_of = |u: &ContextUsage| {
        u.segments
            .iter()
            .find(|s| s.kind == SegmentKind::Attachments)
            .unwrap()
            .tokens
    };
    assert_eq!(attachments_of(&small), image_tokens(200, 200));
    assert!(attachments_of(&large) > attachments_of(&small));
    assert!(attachments_of(&small) > 0);
}

#[test]
fn an_undecodable_image_costs_the_fixed_fallback() {
    let entry = EntryKind::Message {
        message: Message::User(UserMessage {
            content: UserContent::Blocks(vec![InputContent::Image(ImageContent {
                data: "not base64 at all!!".to_string(),
                mime_type: "image/png".to_string(),
            })]),
            timestamp: now_ms(),
        }),
    };
    let usage = context_usage(&session(vec![entry]), Some(&opus()));
    let attachments = usage
        .segments
        .iter()
        .find(|s| s.kind == SegmentKind::Attachments)
        .unwrap()
        .tokens;
    assert_eq!(attachments, UNKNOWN_IMAGE_TOKENS);
}

#[test]
fn pending_attachments_move_the_ring_before_sending() {
    let session = fixture_transcript();
    let staged = Attachment {
        id: "att_1".to_string(),
        session_id: Some("ses_fixture".to_string()),
        sha256: "deadbeef".to_string(),
        filename: "screenshot.png".to_string(),
        mime: "image/png".to_string(),
        bytes: 240_000,
        width: Some(1600),
        height: Some(1000),
        path: "/tmp/screenshot.png".to_string(),
        thumb_path: None,
        created_at: now_ms(),
    };
    let before = context_usage(&session, Some(&opus()));
    let after = context_usage_with(
        &session,
        Some(&opus()),
        &ContextOptions {
            include_tools: true,
            pending_attachments: vec![staged.clone()],
            ..ContextOptions::default()
        },
    );
    assert_eq!(after.used - before.used, image_tokens(1600, 1000));
    assert_eq!(attachment_tokens(&staged), image_tokens(1600, 1000));

    // A non-image attachment falls back to its byte length.
    let text_file = Attachment {
        mime: "text/plain".to_string(),
        width: None,
        height: None,
        bytes: 8_000,
        ..staged
    };
    assert_eq!(attachment_tokens(&text_file), 2_000);
}

#[test]
fn image_tokens_scales_and_saturates() {
    assert_eq!(image_tokens(0, 0), UNKNOWN_IMAGE_TOKENS);
    assert_eq!(image_tokens(8, 8), image::MIN_IMAGE_TOKENS);
    assert_eq!(image_tokens(1000, 1000), 1_000_000u64.div_ceil(750));
    // Beyond the edge cap, cost stops growing with the source size.
    assert_eq!(image_tokens(8000, 8000), image_tokens(16_000, 16_000));
    assert!(image_tokens(1600, 1000) > image_tokens(800, 500));
}

#[test]
fn image_headers_decode_for_every_accepted_format() {
    assert_eq!(image::dimensions(&png_bytes(1280, 720)), Some((1280, 720)));
    assert_eq!(image::dimensions(&jpeg_bytes(640, 480)), Some((640, 480)));

    let mut gif = b"GIF89a".to_vec();
    gif.extend_from_slice(&300u16.to_le_bytes());
    gif.extend_from_slice(&200u16.to_le_bytes());
    assert_eq!(image::dimensions(&gif), Some((300, 200)));

    let mut bmp = b"BM".to_vec();
    bmp.extend_from_slice(&[0; 16]);
    bmp.extend_from_slice(&64u32.to_le_bytes());
    bmp.extend_from_slice(&(-48i32).to_le_bytes()); // bottom-up bitmaps store height negative
    assert_eq!(image::dimensions(&bmp), Some((64, 48)));

    let mut webp = b"RIFF".to_vec();
    webp.extend_from_slice(&[0; 4]);
    webp.extend_from_slice(b"WEBPVP8X");
    webp.extend_from_slice(&[0; 8]); // chunk size + flags
    webp.extend_from_slice(&[0x7f, 0x03, 0x00]); // (896 - 1) little-endian 24-bit
    webp.extend_from_slice(&[0x3f, 0x02, 0x00]); // (576 - 1)
    assert_eq!(image::dimensions(&webp), Some((896, 576)));

    assert_eq!(image::dimensions(b"not an image"), None);

    // A data: URL and a truncated payload both still yield a header.
    let url = format!(
        "data:image/png;base64,{}",
        STANDARD.encode(png_bytes(500, 400))
    );
    assert_eq!(image::dimensions_from_base64(&url), Some((500, 400)));
}

// ---------------------------------------------------------------- cost

#[test]
fn cost_prefers_what_the_provider_reported() {
    let mut reported = Usage {
        input: 1_000,
        output: 1_000,
        total_tokens: 2_000,
        ..Default::default()
    };
    reported.cost = Cost {
        input: 0.25,
        output: 0.75,
        cache_read: 0.0,
        cache_write: 0.0,
        total: 1.0,
    };
    let usage = context_usage(&session(vec![assistant("done", reported)]), Some(&opus()));
    assert!((usage.cost.total - 1.0).abs() < 1e-9);
}

#[test]
fn cost_is_derived_from_the_catalog_when_the_provider_reported_none() {
    let model = opus();
    let usage = context_usage(
        &session(vec![assistant(
            "done",
            Usage {
                input: 1_000_000,
                output: 1_000_000,
                total_tokens: 2_000_000,
                ..Default::default()
            },
        )]),
        Some(&model),
    );
    // 1M in at $5 plus 1M out at $25.
    assert!((usage.cost.total - 30.0).abs() < 1e-9);
    assert!((usage.cost.input - 5.0).abs() < 1e-9);

    // With no model there is nothing to price against, and the total stays honest at zero.
    let unpriced = context_usage(
        &session(vec![assistant(
            "done",
            Usage {
                input: 1_000_000,
                total_tokens: 1_000_000,
                ..Default::default()
            },
        )]),
        None,
    );
    assert_eq!(unpriced.cost.total, 0.0);
}

#[test]
fn cost_accumulates_across_the_session() {
    let usage = context_usage(&fixture_transcript(), Some(&opus()));
    assert!(usage.cost.total > 0.0);
    assert!(usage.cost.cache_read > 0.0, "cached reads are priced too");
    let parts =
        usage.cost.input + usage.cost.output + usage.cost.cache_read + usage.cost.cache_write;
    assert!((usage.cost.total - parts).abs() < 1e-9);
}

#[test]
fn compaction_leaves_only_its_summary_resident() {
    let long = "a".repeat(40_000);
    let with_messages = context_usage(&session(vec![user(&long)]), Some(&opus()));
    let compacted = context_usage(
        &session(vec![EntryKind::Compaction {
            summary: "The user asked for a health endpoint; it was added and tested.".to_string(),
            tokens_before: 10_000,
        }]),
        Some(&opus()),
    );
    assert!(compacted.used < with_messages.used);
    assert_eq!(compacted.message_count, 0, "a compaction is not a message");
}
