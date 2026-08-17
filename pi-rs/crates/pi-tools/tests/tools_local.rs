//! End-to-end tool tests against the real filesystem and shell.
//!
//! The upstream counterparts live in
//! `.upstream/packages/agent/test/harness/tools.test.ts`, which runs the tools
//! against a `NodeExecutionEnv` rooted in a temp dir. The cases that only need a
//! fake environment are unit tests next to each tool; these are the ones that
//! must exercise the real thing.

use std::sync::Arc;

use pi_tools::tools::edit::EditToolDetails;
use pi_tools::{
    default_tools, AgentTool, BashTool, EditTool, ExecutionEnvRef, LocalExecutionEnv, ReadTool,
    ToolContext, WriteTool,
};
use serde_json::json;

fn temp_context() -> (tempfile::TempDir, ExecutionEnvRef, ToolContext) {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let env: ExecutionEnvRef =
        Arc::new(LocalExecutionEnv::new(root.to_string_lossy().into_owned()));
    let context = ToolContext::new(env.clone()).with_tool_call_id("call-1");
    (dir, env, context)
}

#[tokio::test]
async fn bash_executes_commands_and_combines_stdout_and_stderr() {
    let (_dir, _env, context) = temp_context();

    let result = BashTool::new()
        .execute(
            json!({ "command": "printf out; printf err >&2" }),
            &context,
            None,
        )
        .await
        .unwrap();

    let output = result.text_output();
    assert!(output.contains("out"), "{output}");
    assert!(output.contains("err"), "{output}");
}

#[tokio::test]
async fn bash_reports_nonzero_exits() {
    let (_dir, _env, context) = temp_context();

    let error = BashTool::new()
        .execute(
            json!({ "command": "printf failed; exit 7" }),
            &context,
            None,
        )
        .await
        .unwrap_err();

    let message = error.message();
    assert!(message.contains("failed"), "{message}");
    assert!(message.contains("Command exited with code 7"), "{message}");
}

#[tokio::test]
async fn bash_reports_timeouts() {
    let (_dir, _env, context) = temp_context();

    let error = BashTool::new()
        .execute(
            json!({ "command": "sleep 5", "timeout": 0.05 }),
            &context,
            None,
        )
        .await
        .unwrap_err();

    assert!(
        error
            .message()
            .contains("Command timed out after 0.05 seconds"),
        "{}",
        error.message()
    );
}

#[tokio::test]
async fn bash_runs_in_the_environment_cwd() {
    let (_dir, env, context) = temp_context();

    let result = BashTool::new()
        .execute(json!({ "command": "pwd" }), &context, None)
        .await
        .unwrap();

    assert_eq!(result.text_output().trim(), env.cwd());
}

#[tokio::test]
async fn write_read_and_edit_round_trip_on_disk() {
    let (_dir, env, context) = temp_context();

    WriteTool::new()
        .execute(
            json!({ "path": "nested/dir/file.txt", "content": "alpha\nbeta\n" }),
            &context,
            None,
        )
        .await
        .unwrap();

    let read = ReadTool::new()
        .execute(json!({ "path": "nested/dir/file.txt" }), &context, None)
        .await
        .unwrap();
    assert_eq!(read.text_output(), "alpha\nbeta\n");

    let edited = EditTool::new()
        .execute(
            json!({
                "path": "nested/dir/file.txt",
                "edits": [{ "oldText": "alpha", "newText": "ALPHA" }]
            }),
            &context,
            None,
        )
        .await
        .unwrap();
    let details: EditToolDetails = serde_json::from_value(edited.details.clone().unwrap()).unwrap();
    assert_eq!(details.first_changed_line, Some(1));
    assert!(details.patch.contains("+ALPHA"));

    assert_eq!(
        env.read_text_file("nested/dir/file.txt", None)
            .await
            .unwrap(),
        "ALPHA\nbeta\n"
    );
}

#[tokio::test]
async fn edit_follows_symlinks_and_preserves_bom_and_crlf() {
    let (_dir, env, context) = temp_context();
    env.write_file("target.txt", "\u{FEFF}one\r\ntwo\r\n".as_bytes(), None)
        .await
        .unwrap();
    std::os::unix::fs::symlink(
        format!("{}/target.txt", env.cwd()),
        format!("{}/link.txt", env.cwd()),
    )
    .unwrap();

    EditTool::new()
        .execute(
            json!({ "path": "link.txt", "edits": [{ "oldText": "two", "newText": "TWO" }] }),
            &context,
            None,
        )
        .await
        .unwrap();

    assert_eq!(
        env.read_text_file("target.txt", None).await.unwrap(),
        "\u{FEFF}one\r\nTWO\r\n"
    );
}

#[tokio::test]
async fn read_reports_images_from_disk() {
    let (_dir, env, context) = temp_context();
    // 1x1 PNG.
    let png: Vec<u8> = {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD
            .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGNgYGD4DwABBAEAX+XDSwAAAABJRU5ErkJggg==")
            .unwrap()
    };
    env.write_file("shot.png", &png, None).await.unwrap();

    let result = ReadTool::new()
        .execute(json!({ "path": "shot.png" }), &context, None)
        .await
        .unwrap();

    assert!(result.text_output().contains("Read image file [image/png]"));
    assert_eq!(result.content.len(), 2);
}

#[tokio::test]
async fn find_and_grep_work_against_a_real_tree() {
    let (_dir, env, context) = temp_context();
    env.create_dir("src", true, None).await.unwrap();
    env.write_file("src/lib.rs", b"pub fn needle() {}\n", None)
        .await
        .unwrap();
    env.write_file("README.md", b"docs\n", None).await.unwrap();

    let tools = default_tools();
    let find = tools.iter().find(|t| t.name() == "find").unwrap();
    let grep = tools.iter().find(|t| t.name() == "grep").unwrap();

    let found = find
        .execute(json!({ "pattern": "*.rs" }), &context, None)
        .await
        .unwrap();
    assert_eq!(found.text_output(), "src/lib.rs");

    let matched = grep
        .execute(json!({ "pattern": "needle" }), &context, None)
        .await
        .unwrap();
    assert_eq!(matched.text_output(), "src/lib.rs:1: pub fn needle() {}");
}

#[test]
fn default_tools_declare_object_schemas() {
    let tools = default_tools();
    let names: Vec<&str> = tools.iter().map(|tool| tool.name()).collect();
    assert_eq!(names, vec!["bash", "read", "write", "edit", "find", "grep"]);

    for tool in &tools {
        let declaration = tool.declaration();
        assert_eq!(declaration.name, tool.name());
        assert!(!declaration.description.is_empty());
        assert_eq!(declaration.parameters["type"], "object");
        assert!(declaration.parameters["properties"].is_object());
        // The declaration must round-trip as JSON for the provider wire format.
        serde_json::to_string(&declaration).unwrap();
    }
}
