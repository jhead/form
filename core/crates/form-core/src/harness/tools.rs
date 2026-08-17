//! The tool set (spec 02 §5).
//!
//! Nothing here executes anything. The point is arguments that look like they came from a
//! model that had read the workspace, and results carrying the fields the transcript renders:
//! diff counts for mutating tools (F1.3), progress for the determinate bar (F6.2).

use rand::Rng;
use rand_chacha::ChaCha8Rng;
use serde_json::{json, Map, Value};

use super::rng::pick;

pub(super) struct PlannedTool {
    pub id: String,
    pub name: &'static str,
    pub args: Map<String, Value>,
    /// How long `tool_execution_start` → `tool_execution_end` takes, before the multiplier.
    pub exec_ms: u64,
    /// One `tool_execution_update` per entry.
    pub progress: Vec<Value>,
    pub result: Value,
    /// What goes in the tool-result message the model sees.
    pub text: String,
    pub is_error: bool,
}

/// Weighted so `read`/`bash`/`grep` dominate, the way a real session looks.
const TOOL_WEIGHTS: &[(&str, u32)] = &[
    ("read", 30),
    ("bash", 18),
    ("grep", 14),
    ("edit", 14),
    ("glob", 10),
    ("write", 8),
    ("web_fetch", 6),
];

const FILES: &[&str] = &[
    "src/main.rs",
    "src/server.rs",
    "src/routes/health.rs",
    "src/middleware/auth.rs",
    "core/crates/form-core/src/core.rs",
    "core/crates/form-core/src/harness/mod.rs",
    "app/Sources/FormUI/Chat/TranscriptView.swift",
    "app/Sources/FormCore/Transport.swift",
    "web/src/components/Composer.tsx",
    "scripts/deploy.py",
    "tests/health.rs",
    "Makefile",
    "package.json",
    "README.md",
];

const COMMANDS: &[&str] = &[
    "cargo test -p form-core harness:: -- --nocapture",
    "cargo clippy --all-targets -- -D warnings",
    "swift build -c debug",
    "npm run lint -- --max-warnings 0",
    "git status --short",
    "make lint",
    "pytest -q tests/",
    "rg -n \"health_check\" src",
];

const PATTERNS: &[&str] = &[
    "fn health_check",
    "TODO\\(W\\d\\)",
    "AbortSignal",
    "record_turn",
    "impl Harness",
    "message_update",
];

const GLOBS: &[&str] = &[
    "**/*.rs",
    "src/**/*.swift",
    "**/Cargo.toml",
    "docs/specs/*.md",
    "**/*.test.ts",
];

const URLS: &[&str] = &[
    "https://docs.rs/tokio/latest/tokio/time/fn.sleep.html",
    "https://example.com/runbook/health-checks",
    "https://developer.apple.com/documentation/swiftui/observable",
    "https://example.com/api/changelog",
];

const NEEDLE: &[&str] = &[
    "the auth layer wraps the health route",
    "`speed` divides every sleep",
    "abort is polled between events",
    "the partial is accumulated, not stubbed",
];

/// A tool call plus everything its execution will report. Planned up front so the whole run
/// is a pure function of the seed.
pub(super) fn plan_tool(
    rng: &mut ChaCha8Rng,
    workspace_root: Option<&str>,
    index: usize,
) -> PlannedTool {
    let name = weighted_name(rng);
    let id = format!("toolu_{:016x}{:04x}", rng.gen::<u64>(), index as u16);
    // A failing call every so often, so the error affordance in the tool row is reachable.
    let is_error = rng.gen_ratio(1, 9);

    let mut args = Map::new();
    let (result, text, exec_ms) = match name {
        "read" => {
            let path = file_path(rng, workspace_root);
            args.insert("path".into(), json!(path.clone()));
            if rng.gen_ratio(1, 3) {
                args.insert("offset".into(), json!(rng.gen_range(1..400)));
                args.insert("limit".into(), json!(pick(rng, &[80, 120, 200, 400])));
            }
            if is_error {
                (
                    json!({ "path": path, "code": "enoent" }),
                    format!("no such file: {path}"),
                    rng.gen_range(90..260),
                )
            } else {
                let lines = rng.gen_range(24..640);
                (
                    json!({ "path": path, "lines": lines, "bytes": lines * 38 }),
                    format!("read {lines} lines from {path}"),
                    rng.gen_range(180..900),
                )
            }
        }
        "write" => {
            let path = file_path(rng, workspace_root);
            let added = rng.gen_range(12..320);
            args.insert("path".into(), json!(path.clone()));
            args.insert(
                "content".into(),
                json!(format!("// {} lines written by the agent\n", added)),
            );
            (
                json!({ "path": path, "linesAdded": added, "linesRemoved": 0 }),
                format!("created {path} (+{added} -0)"),
                rng.gen_range(200..700),
            )
        }
        "edit" => {
            let path = file_path(rng, workspace_root);
            let added = rng.gen_range(1..80);
            let removed = rng.gen_range(0..40);
            args.insert("path".into(), json!(path.clone()));
            args.insert(
                "oldString".into(),
                json!("return Err(CoreError::RunAlreadyActive(session_id));"),
            );
            args.insert(
                "newString".into(),
                json!("self.queue(session_id, text);\n        return Ok(());"),
            );
            if is_error {
                (
                    json!({ "path": path, "code": "no_match" }),
                    format!("oldString did not match anything in {path}"),
                    rng.gen_range(80..200),
                )
            } else {
                (
                    json!({ "path": path, "linesAdded": added, "linesRemoved": removed }),
                    format!("edited {path} (+{added} -{removed})"),
                    rng.gen_range(240..1_100),
                )
            }
        }
        "bash" => {
            let command = *pick(rng, COMMANDS);
            args.insert("command".into(), json!(command));
            if let Some(root) = workspace_root {
                args.insert("cwd".into(), json!(root));
            }
            if is_error {
                (
                    json!({ "exitCode": 101, "stdout": "", "stderr": "error: test failed, to rerun pass `-p form-core`" }),
                    format!("`{command}` exited 101"),
                    rng.gen_range(900..4_000),
                )
            } else {
                let out = format!(
                    "test result: ok. {} passed; 0 failed",
                    rng.gen_range(3..180)
                );
                (
                    json!({ "exitCode": 0, "stdout": out, "stderr": "" }),
                    out,
                    rng.gen_range(600..4_000),
                )
            }
        }
        "grep" => {
            let pattern = *pick(rng, PATTERNS);
            args.insert("pattern".into(), json!(pattern));
            args.insert(
                "path".into(),
                json!(workspace_root.unwrap_or(".").to_string()),
            );
            if rng.gen_ratio(1, 2) {
                args.insert("glob".into(), json!(*pick(rng, GLOBS)));
            }
            let matches = rng.gen_range(0..48);
            let files = (matches / 3).max(if matches == 0 { 0 } else { 1 });
            (
                json!({ "matches": matches, "files": files, "pattern": pattern }),
                format!("{matches} matches in {files} files"),
                rng.gen_range(200..1_400),
            )
        }
        "glob" => {
            let pattern = *pick(rng, GLOBS);
            args.insert("pattern".into(), json!(pattern));
            args.insert(
                "path".into(),
                json!(workspace_root.unwrap_or(".").to_string()),
            );
            let files = rng.gen_range(1..90);
            (
                json!({ "files": files, "pattern": pattern }),
                format!("{files} files match {pattern}"),
                rng.gen_range(150..600),
            )
        }
        _ => {
            let url = *pick(rng, URLS);
            args.insert("url".into(), json!(url));
            args.insert("prompt".into(), json!("summarise the cancellation section"));
            if is_error {
                (
                    json!({ "url": url, "status": 503 }),
                    format!("{url} returned 503"),
                    rng.gen_range(500..2_400),
                )
            } else {
                let bytes = rng.gen_range(4_000..180_000);
                (
                    json!({ "url": url, "status": 200, "bytes": bytes }),
                    format!("fetched {bytes} bytes — {}", pick(rng, NEEDLE)),
                    rng.gen_range(400..3_200),
                )
            }
        }
    };

    let ticks = rng.gen_range(3..=8);
    let progress = progress_ticks(name, ticks, &result);

    PlannedTool {
        id,
        name,
        args,
        exec_ms,
        progress,
        result,
        text,
        is_error,
    }
}

fn weighted_name(rng: &mut ChaCha8Rng) -> &'static str {
    let total: u32 = TOOL_WEIGHTS.iter().map(|(_, w)| w).sum();
    let mut roll = rng.gen_range(0..total);
    for (name, weight) in TOOL_WEIGHTS {
        if roll < *weight {
            return name;
        }
        roll -= weight;
    }
    TOOL_WEIGHTS[0].0
}

fn file_path(rng: &mut ChaCha8Rng, workspace_root: Option<&str>) -> String {
    let file = *pick(rng, FILES);
    match workspace_root {
        Some(root) => format!("{}/{}", root.trim_end_matches('/'), file),
        None => file.to_string(),
    }
}

/// Progress payloads carry `progress` in 0…1 so the row can go determinate (F6.2), plus a
/// tool-shaped detail so the expanded view has something to show.
fn progress_ticks(name: &str, ticks: usize, result: &Value) -> Vec<Value> {
    (1..=ticks)
        .map(|i| {
            let progress = i as f64 / ticks as f64;
            match name {
                "bash" => json!({
                    "progress": progress,
                    "stdout": format!("running… {}/{}", i, ticks),
                }),
                "read" | "write" | "edit" => json!({
                    "progress": progress,
                    "path": result.get("path").cloned().unwrap_or(Value::Null),
                }),
                "grep" | "glob" => json!({
                    "progress": progress,
                    "scanned": i * 128,
                }),
                _ => json!({ "progress": progress }),
            }
        })
        .collect()
}
