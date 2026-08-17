//! The tool schemas that ride along in every request.
//!
//! These mirror the tool names the harness actually calls (`harness::tools`), because the
//! Tools segment of the ring is meant to be the real cost of advertising them — not a
//! constant someone guessed. Serialization happens once behind a `OnceLock`.

use std::sync::OnceLock;

use serde_json::{json, Value};

/// Name, one-line description, and JSON Schema for the arguments — the three things every
/// provider bills you for on every request.
fn schemas() -> Vec<Value> {
    vec![
        json!({
            "name": "read",
            "description": "Read a file from the workspace. Returns the contents with line numbers. Prefer this over shelling out to cat.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Path to the file, absolute or relative to the workspace root." },
                    "offset": { "type": "integer", "description": "First line to read, 1-based." },
                    "limit": { "type": "integer", "description": "Maximum number of lines to read." }
                },
                "required": ["path"]
            }
        }),
        json!({
            "name": "write",
            "description": "Write a file, creating it or replacing its contents entirely. Read the file first unless you are creating it.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Path to the file to write." },
                    "content": { "type": "string", "description": "The complete new contents of the file." }
                },
                "required": ["path", "content"]
            }
        }),
        json!({
            "name": "edit",
            "description": "Replace an exact string in a file. The old string must appear exactly once unless replaceAll is set.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Path to the file to edit." },
                    "oldString": { "type": "string", "description": "Text to replace, including surrounding context to make it unique." },
                    "newString": { "type": "string", "description": "Replacement text." },
                    "replaceAll": { "type": "boolean", "description": "Replace every occurrence instead of requiring exactly one." }
                },
                "required": ["path", "oldString", "newString"]
            }
        }),
        json!({
            "name": "bash",
            "description": "Run a shell command in the workspace root and return its combined output. Commands are not interactive and time out.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "The command line to run." },
                    "timeoutMs": { "type": "integer", "description": "Timeout in milliseconds, default 120000, maximum 600000." },
                    "description": { "type": "string", "description": "A short description of what the command does, shown to the user." }
                },
                "required": ["command"]
            }
        }),
        json!({
            "name": "glob",
            "description": "Find files by glob pattern, sorted by modification time. Use this instead of shelling out to find.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Glob pattern, for example src/**/*.rs." },
                    "path": { "type": "string", "description": "Directory to search in, defaulting to the workspace root." }
                },
                "required": ["pattern"]
            }
        }),
        json!({
            "name": "grep",
            "description": "Search file contents with a regular expression. Returns matching lines with their file and line number.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Regular expression to search for." },
                    "path": { "type": "string", "description": "File or directory to search, defaulting to the workspace root." },
                    "glob": { "type": "string", "description": "Restrict the search to files matching this glob." },
                    "caseInsensitive": { "type": "boolean", "description": "Ignore case while matching." },
                    "contextLines": { "type": "integer", "description": "Lines of context to include around each match." }
                },
                "required": ["pattern"]
            }
        }),
        json!({
            "name": "web_fetch",
            "description": "Fetch a URL and return its content converted to markdown. Use for documentation and API references.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "url": { "type": "string", "description": "Absolute URL to fetch." },
                    "prompt": { "type": "string", "description": "What to extract from the page." }
                },
                "required": ["url"]
            }
        }),
    ]
}

static SERIALIZED: OnceLock<String> = OnceLock::new();

/// Every schema, serialized the way it goes over the wire. One string, because that is how
/// the provider counts it.
pub fn serialized() -> &'static str {
    SERIALIZED.get_or_init(|| serde_json::to_string(&schemas()).unwrap_or_default())
}

pub fn count() -> usize {
    schemas().len()
}

pub fn names() -> Vec<String> {
    schemas()
        .iter()
        .filter_map(|s| s.get("name").and_then(Value::as_str).map(str::to_string))
        .collect()
}
