//! Port of `.upstream/packages/agent/test/harness/nodejs-env.test.ts`.
//!
//! Everything here runs against the real filesystem and the real shell, so every
//! test is rooted in a `tempfile::TempDir` and no command escapes it.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use pi_core::AbortHandle;
use pi_tools::error::{ExecutionErrorCode, FileErrorCode};
use pi_tools::{
    execute_shell_with_capture, ExecutionEnvRef, FileKind, FileSystem, LocalExecutionEnv, Shell,
    ShellCaptureOptions, ShellExecOptions,
};

fn temp_env() -> (tempfile::TempDir, LocalExecutionEnv) {
    let dir = tempfile::tempdir().unwrap();
    // macOS puts temp dirs under a symlinked /var, and the shell reports the
    // resolved path in $PWD; canonicalize up front so comparisons line up.
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let env = LocalExecutionEnv::new(root.to_string_lossy().into_owned());
    (dir, env)
}

fn join(env: &LocalExecutionEnv, rest: &str) -> String {
    format!("{}/{rest}", env.cwd())
}

#[tokio::test]
async fn reads_writes_lists_and_removes_files_and_directories() {
    let (_dir, env) = temp_env();

    assert_eq!(
        env.absolute_path("nested/child", None).await.unwrap(),
        join(&env, "nested/child")
    );
    env.create_dir("nested/child", true, None).await.unwrap();
    env.write_file("nested/child/file.txt", b"hel", None)
        .await
        .unwrap();
    env.append_file("nested/child/file.txt", b"lo", None)
        .await
        .unwrap();
    assert_eq!(
        env.read_text_file("nested/child/file.txt", None)
            .await
            .unwrap(),
        "hello"
    );
    assert_eq!(
        env.read_text_lines("nested/child/file.txt", Some(1), None)
            .await
            .unwrap(),
        vec!["hello".to_string()]
    );
    assert_eq!(
        env.read_binary_file("nested/child/file.txt", None)
            .await
            .unwrap(),
        b"hello".to_vec()
    );

    let entries = env.list_dir("nested/child", None).await.unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "file.txt");
    assert_eq!(entries[0].path, join(&env, "nested/child/file.txt"));
    assert_eq!(entries[0].kind, FileKind::File);
    assert_eq!(entries[0].size, 5);
    assert!(entries[0].mtime_ms > 0);

    assert!(env.exists("nested/child/file.txt", None).await.unwrap());
    env.remove("nested/child/file.txt", false, false, None)
        .await
        .unwrap();
    assert!(!env.exists("nested/child/file.txt", None).await.unwrap());
}

#[tokio::test]
async fn expands_home_relative_paths_and_file_urls() {
    let (_dir, env) = temp_env();
    let home = std::env::var("HOME").unwrap();

    assert_eq!(
        env.absolute_path("~/pi-node-env-test", None).await.unwrap(),
        format!("{home}/pi-node-env-test")
    );
    let file_path = join(&env, "file with spaces.txt");
    assert_eq!(
        env.absolute_path(&format!("file://{}", file_path.replace(' ', "%20")), None)
            .await
            .unwrap(),
        file_path
    );
}

#[tokio::test]
async fn returns_file_info_without_following_symlinks() {
    let (_dir, env) = temp_env();
    env.create_dir("dir", true, None).await.unwrap();
    env.write_file("dir/file.txt", b"hello", None)
        .await
        .unwrap();
    std::os::unix::fs::symlink(join(&env, "dir/file.txt"), join(&env, "file-link")).unwrap();
    std::os::unix::fs::symlink(join(&env, "dir"), join(&env, "dir-link")).unwrap();

    assert_eq!(
        env.file_info("dir", None).await.unwrap().kind,
        FileKind::Directory
    );
    let file = env.file_info("dir/file.txt", None).await.unwrap();
    assert_eq!(file.kind, FileKind::File);
    assert_eq!(file.size, 5);
    assert_eq!(
        env.file_info("file-link", None).await.unwrap().kind,
        FileKind::Symlink
    );
    assert_eq!(
        env.file_info("dir-link", None).await.unwrap().kind,
        FileKind::Symlink
    );
    assert_eq!(
        env.canonical_path("file-link", None).await.unwrap(),
        std::fs::canonicalize(join(&env, "dir/file.txt"))
            .unwrap()
            .to_string_lossy()
    );
}

#[tokio::test]
async fn lists_symlinks_as_symlinks() {
    let (_dir, env) = temp_env();
    env.write_file("target.txt", b"hello", None).await.unwrap();
    std::os::unix::fs::symlink(join(&env, "target.txt"), join(&env, "link.txt")).unwrap();

    let mut entries: Vec<(String, FileKind)> = env
        .list_dir(".", None)
        .await
        .unwrap()
        .into_iter()
        .map(|entry| (entry.name, entry.kind))
        .collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        entries,
        vec![
            ("link.txt".to_string(), FileKind::Symlink),
            ("target.txt".to_string(), FileKind::File),
        ]
    );
}

#[tokio::test]
async fn stops_reading_text_lines_at_the_requested_limit() {
    let (_dir, env) = temp_env();
    env.write_file("file.txt", b"one\ntwo\nthree", None)
        .await
        .unwrap();
    assert_eq!(
        env.read_text_lines("file.txt", Some(1), None)
            .await
            .unwrap(),
        vec!["one".to_string()]
    );
    assert_eq!(
        env.read_text_lines("file.txt", Some(0), None)
            .await
            .unwrap(),
        Vec::<String>::new()
    );
}

#[tokio::test]
async fn returns_not_found_for_missing_paths() {
    let (_dir, env) = temp_env();
    let error = env.file_info("missing.txt", None).await.unwrap_err();
    assert_eq!(error.code, FileErrorCode::NotFound);
    assert_eq!(error.path, Some(join(&env, "missing.txt")));
    assert!(!env.exists("missing.txt", None).await.unwrap());
}

#[tokio::test]
async fn returns_not_directory_for_listing_a_file() {
    let (_dir, env) = temp_env();
    env.write_file("file.txt", b"hello", None).await.unwrap();
    let error = env.list_dir("file.txt", None).await.unwrap_err();
    assert_eq!(error.code, FileErrorCode::NotDirectory);
}

#[tokio::test]
async fn appends_to_new_files_and_creates_parent_directories() {
    let (_dir, env) = temp_env();
    env.append_file("new/nested/file.txt", b"a", None)
        .await
        .unwrap();
    env.append_file("new/nested/file.txt", b"b", None)
        .await
        .unwrap();
    assert_eq!(
        env.read_text_file("new/nested/file.txt", None)
            .await
            .unwrap(),
        "ab"
    );
}

#[tokio::test]
async fn renames_a_file_and_replaces_the_destination() {
    let (_dir, env) = temp_env();
    env.write_file("source.txt", b"new", None).await.unwrap();
    env.write_file("destination.txt", b"old", None)
        .await
        .unwrap();

    env.rename_file("source.txt", "destination.txt", None)
        .await
        .unwrap();

    assert!(!env.exists("source.txt", None).await.unwrap());
    assert_eq!(
        env.read_text_file("destination.txt", None).await.unwrap(),
        "new"
    );
}

#[tokio::test]
async fn reports_the_source_path_when_a_rename_fails() {
    let (_dir, env) = temp_env();
    env.write_file("destination.txt", b"unchanged", None)
        .await
        .unwrap();

    let error = env
        .rename_file("missing-source.txt", "destination.txt", None)
        .await
        .unwrap_err();

    assert_eq!(error.code, FileErrorCode::NotFound);
    assert_eq!(error.path, Some(join(&env, "missing-source.txt")));
    assert_eq!(
        env.read_text_file("destination.txt", None).await.unwrap(),
        "unchanged"
    );
}

#[tokio::test]
async fn creates_temporary_directories_and_files() {
    let (_dir, env) = temp_env();
    let temp_dir = env
        .create_temp_dir(Some("node-env-test-"), None)
        .await
        .unwrap();
    assert!(std::path::Path::new(&temp_dir).is_dir());

    let temp_file = env
        .create_temp_file(Some("prefix-"), Some(".txt"), None)
        .await
        .unwrap();
    assert!(std::path::Path::new(&temp_file).is_file());
    assert!(temp_file.ends_with(".txt"));

    std::fs::remove_dir_all(&temp_dir).ok();
    if let Some(parent) = std::path::Path::new(&temp_file).parent() {
        std::fs::remove_dir_all(parent).ok();
    }
}

#[tokio::test]
async fn honours_create_dir_and_remove_options() {
    let (_dir, env) = temp_env();

    let error = env
        .create_dir("missing/child", false, None)
        .await
        .unwrap_err();
    assert_eq!(error.code, FileErrorCode::NotFound);

    env.write_file("dir/child/file.txt", b"hello", None)
        .await
        .unwrap();
    assert!(env.remove("dir", false, false, None).await.is_err());
    env.remove("dir", true, false, None).await.unwrap();
    assert!(!env.exists("dir", None).await.unwrap());

    assert!(env.remove("missing", false, false, None).await.is_err());
    env.remove("missing", false, true, None).await.unwrap();
}

#[tokio::test]
async fn returns_aborted_results_for_pre_aborted_operations() {
    let (_dir, env) = temp_env();
    env.write_file("file.txt", b"hello", None).await.unwrap();
    let (handle, signal) = AbortHandle::new();
    handle.abort();
    let signal = Some(signal);

    for error in [
        env.read_text_file("file.txt", signal.clone())
            .await
            .unwrap_err(),
        env.read_text_lines("file.txt", None, signal.clone())
            .await
            .unwrap_err(),
        env.read_binary_file("file.txt", signal.clone())
            .await
            .unwrap_err(),
        env.write_file("other.txt", b"hello", signal.clone())
            .await
            .unwrap_err(),
        env.rename_file("file.txt", "renamed.txt", signal.clone())
            .await
            .unwrap_err(),
        env.list_dir(".", signal.clone()).await.unwrap_err(),
    ] {
        assert_eq!(error.code, FileErrorCode::Aborted);
    }
}

#[tokio::test]
async fn executes_commands_in_cwd_with_env_overrides() {
    let (_dir, env) = temp_env();
    let mut options = ShellExecOptions::new();
    options
        .env
        .insert("NODE_ENV_TEST".to_string(), "ok".to_string());

    let result = env
        .exec("printf '%s:%s' \"$PWD\" \"$NODE_ENV_TEST\"", options)
        .await
        .unwrap();

    assert_eq!(result.stdout, format!("{}:ok", env.cwd()));
    assert_eq!(result.stderr, "");
    assert_eq!(result.exit_code, 0);
}

#[tokio::test]
async fn applies_shell_env_overrides() {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let mut shell_env = BTreeMap::new();
    shell_env.insert(
        "PI_SESSION_FILE".to_string(),
        "/stale/parent.jsonl".to_string(),
    );
    shell_env.insert("PI_CODING_AGENT".to_string(), "true".to_string());
    let env = LocalExecutionEnv::new(root.to_string_lossy().into_owned()).with_shell_env(shell_env);

    let base = env
        .exec(
            "printf '%s|%s' \"${PI_SESSION_FILE-}\" \"$PI_CODING_AGENT\"",
            ShellExecOptions::new(),
        )
        .await
        .unwrap();
    assert_eq!(base.stdout, "/stale/parent.jsonl|true");

    let mut options = ShellExecOptions::new();
    options.env.insert(
        "PI_SESSION_FILE".to_string(),
        "/sessions/current.jsonl".to_string(),
    );
    let overridden = env
        .exec(
            "printf '%s|%s' \"${PI_SESSION_FILE-}\" \"$PI_CODING_AGENT\"",
            options,
        )
        .await
        .unwrap();
    assert_eq!(overridden.stdout, "/sessions/current.jsonl|true");
}

#[tokio::test]
async fn can_replace_rather_than_inherit_the_shell_environment() {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let mut shell_env = BTreeMap::new();
    shell_env.insert("PI_CONFIGURED".to_string(), "configured".to_string());
    let env = LocalExecutionEnv::new(root.to_string_lossy().into_owned()).with_shell_env(shell_env);

    let mut options = ShellExecOptions::new();
    options.inherit_env = false;
    options
        .env
        .insert("PI_EXPLICIT".to_string(), "explicit".to_string());
    let result = env
        .exec(
            "printf '%s:%s:%s' \"${HOME-}\" \"${PI_CONFIGURED-}\" \"${PI_EXPLICIT-}\"",
            options,
        )
        .await
        .unwrap();

    assert_eq!(result.stdout, "::explicit");
}

#[tokio::test]
async fn returns_non_zero_exit_codes_as_success() {
    let (_dir, env) = temp_env();
    let result = env.exec("exit 7", ShellExecOptions::new()).await.unwrap();
    assert_eq!(result.exit_code, 7);
    assert_eq!(result.stdout, "");
}

#[tokio::test]
async fn streams_stdout_and_stderr_chunks() {
    let (_dir, env) = temp_env();
    let stdout = Arc::new(parking_lot::Mutex::new(String::new()));
    let stderr = Arc::new(parking_lot::Mutex::new(String::new()));

    let mut options = ShellExecOptions::new();
    {
        let sink = stdout.clone();
        options.on_stdout = Some(Arc::new(move |chunk| {
            sink.lock().push_str(chunk);
            Ok(())
        }));
        let sink = stderr.clone();
        options.on_stderr = Some(Arc::new(move |chunk| {
            sink.lock().push_str(chunk);
            Ok(())
        }));
    }

    let result = env
        .exec("printf out; printf err >&2", options)
        .await
        .unwrap();

    assert_eq!(result.stdout, "out");
    assert_eq!(result.stderr, "err");
    assert_eq!(result.exit_code, 0);
    assert_eq!(*stdout.lock(), "out");
    assert_eq!(*stderr.lock(), "err");
}

#[tokio::test]
async fn returns_timeout_errors() {
    let (_dir, env) = temp_env();
    let error = env
        .exec(
            "sleep 5",
            ShellExecOptions::new().with_timeout_secs(Some(0.05)),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ExecutionErrorCode::Timeout);
}

#[tokio::test]
async fn rejects_invalid_timeouts() {
    let (_dir, env) = temp_env();
    let error = env
        .exec("true", ShellExecOptions::new().with_timeout_secs(Some(0.0)))
        .await
        .unwrap_err();
    assert_eq!(error.code, ExecutionErrorCode::Timeout);
    assert!(error.message.contains("Invalid timeout"));
}

#[tokio::test]
async fn returns_callback_errors_from_stream_handlers() {
    let (_dir, env) = temp_env();
    let mut options = ShellExecOptions::new();
    options.on_stdout = Some(Arc::new(|_chunk| Err("callback failed".to_string())));

    let error = env.exec("printf out", options).await.unwrap_err();
    assert_eq!(error.code, ExecutionErrorCode::CallbackError);
    assert_eq!(error.message, "callback failed");
}

#[tokio::test]
async fn returns_shell_unavailable_and_spawn_errors() {
    let (_dir, env) = temp_env();

    let missing_shell = LocalExecutionEnv::new(env.cwd().to_string())
        .with_shell_path(Some(join(&env, "missing-shell")));
    let error = missing_shell
        .exec("printf ok", ShellExecOptions::new())
        .await
        .unwrap_err();
    assert_eq!(error.code, ExecutionErrorCode::ShellUnavailable);

    env.write_file("not-executable-shell", b"not executable", None)
        .await
        .unwrap();
    let bad_shell = LocalExecutionEnv::new(env.cwd().to_string())
        .with_shell_path(Some(join(&env, "not-executable-shell")));
    let error = bad_shell
        .exec("printf ok", ShellExecOptions::new())
        .await
        .unwrap_err();
    assert_eq!(error.code, ExecutionErrorCode::SpawnError);
}

#[tokio::test]
async fn reports_a_missing_working_directory_before_spawning() {
    let (_dir, env) = temp_env();
    let missing = LocalExecutionEnv::new(join(&env, "missing"));
    let error = missing
        .exec("printf ok", ShellExecOptions::new())
        .await
        .unwrap_err();

    assert_eq!(error.code, ExecutionErrorCode::SpawnError);
    assert!(error.message.contains("Working directory does not exist"));
}

#[tokio::test(flavor = "multi_thread")]
async fn returns_an_aborted_result_for_aborted_commands() {
    let (_dir, env) = temp_env();
    let (handle, signal) = AbortHandle::new();

    let task = tokio::spawn(async move {
        let env = env;
        env.exec(
            "sleep 5",
            ShellExecOptions::new().with_abort(Some(signal.clone())),
        )
        .await
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    handle.abort();

    let error = task.await.unwrap().unwrap_err();
    assert_eq!(error.code, ExecutionErrorCode::Aborted);
}

#[tokio::test(flavor = "multi_thread")]
async fn cleanup_terminates_active_shell_processes() {
    let (_dir, env) = temp_env();
    let env: ExecutionEnvRef = Arc::new(env);

    let execution = {
        let env = env.clone();
        tokio::spawn(async move {
            env.exec("touch started; sleep 60", ShellExecOptions::new())
                .await
        })
    };
    for _ in 0..200 {
        if env.exists("started", None).await.unwrap() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(env.exists("started", None).await.unwrap());

    env.cleanup_shell().await;
    let result = tokio::time::timeout(Duration::from_secs(5), execution)
        .await
        .expect("exec settled after cleanup")
        .unwrap();
    assert!(result.is_ok());
}

#[tokio::test]
async fn captures_large_shell_output_to_a_full_output_file() {
    let (_dir, env) = temp_env();
    let env: ExecutionEnvRef = Arc::new(env);

    let result = execute_shell_with_capture(
        &env,
        "i=1; while [ $i -le 15000 ]; do echo line; i=$((i + 1)); done",
        ShellCaptureOptions::new(),
    )
    .await
    .unwrap();

    assert!(result.truncated);
    let path = result.full_output_path.clone().expect("full output path");
    let full_output = env.read_text_file(&path, None).await.unwrap();
    assert!(full_output.lines().count() > 10000);
    assert!(result.output.len() < full_output.len());

    if let Some(parent) = std::path::Path::new(&path).parent() {
        std::fs::remove_dir_all(parent).ok();
    }
}

#[tokio::test]
async fn decodes_multibyte_output_split_across_reads() {
    let (_dir, env) = temp_env();
    // 40k of a 3-byte character guarantees the pipe splits a character across
    // two reads, which is what the incremental decoder exists for.
    let content = "\u{4e2d}".repeat(40000);
    env.write_file("wide.txt", content.as_bytes(), None)
        .await
        .unwrap();
    let result = env
        .exec("cat wide.txt", ShellExecOptions::new())
        .await
        .unwrap();

    assert_eq!(result.stdout.chars().filter(|c| *c == '中').count(), 40000);
    assert!(!result.stdout.contains(char::REPLACEMENT_CHARACTER));
}
