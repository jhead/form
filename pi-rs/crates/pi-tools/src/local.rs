//! Local (real) filesystem and shell.
//!
//! Port of `.upstream/packages/agent/src/harness/env/nodejs.ts`.
//!
//! Differences from upstream, all deliberate:
//! - Windows shell discovery (Git Bash, WSL `bash.exe` stdin transport) is not
//!   ported; on Windows an explicit `shell_path` is required.
//! - `which bash` is replaced by a `$PATH` scan, so resolving the shell never
//!   spawns a process.
//! - Node decodes pipe bytes with a stateful UTF-8 decoder; `Utf8Decoder` does
//!   the same so a multi-byte character split across two reads is not mangled.

use std::collections::{BTreeMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use parking_lot::Mutex;
use pi_core::AbortSignal;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};

use crate::error::{
    ExecResult, ExecutionError, ExecutionErrorCode, FileError, FileErrorCode, FileResult,
};
use crate::types::{
    check_abort, resolve_timeout_ms, FileInfo, FileKind, FileSystem, OutputCallback, Shell,
    ShellExecOptions, ShellOutput,
};

/// Grace period for stdio to drain after the child exits, matching upstream's
/// `EXIT_STDIO_GRACE_MS`. A detached grandchild can hold the pipe open forever.
const EXIT_STDIO_GRACE_MS: u64 = 100;

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// Lexical resolution, like Node's `path.resolve`: expand `~`, accept `file://`
/// URLs, make relative paths absolute against `cwd`, collapse `.` and `..`.
/// Never touches the filesystem and never resolves symlinks.
pub fn resolve_path(cwd: &str, path: &str) -> String {
    let mut normalized = path.to_string();
    if normalized == "~" {
        if let Some(home) = home_dir() {
            normalized = home.to_string_lossy().into_owned();
        }
    } else if let Some(rest) = normalized.strip_prefix("~/") {
        if let Some(home) = home_dir() {
            normalized = home.join(rest).to_string_lossy().into_owned();
        }
    } else if let Some(rest) = normalized.strip_prefix("file://") {
        // Keep malformed URLs as ordinary paths so the non-throwing contract holds.
        let decoded = percent_decode(rest.strip_prefix("localhost").unwrap_or(rest));
        if decoded.starts_with('/') {
            normalized = decoded;
        }
    }

    let candidate = PathBuf::from(&normalized);
    let joined = if candidate.is_absolute() {
        candidate
    } else {
        Path::new(cwd).join(candidate)
    };
    lexically_normalize(&joined)
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(byte) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn lexically_normalize(path: &Path) -> String {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    let rendered = out.to_string_lossy().into_owned();
    if rendered.is_empty() {
        "/".to_string()
    } else {
        rendered
    }
}

fn file_kind(metadata: &std::fs::Metadata) -> Option<FileKind> {
    if metadata.is_symlink() {
        Some(FileKind::Symlink)
    } else if metadata.is_file() {
        Some(FileKind::File)
    } else if metadata.is_dir() {
        Some(FileKind::Directory)
    } else {
        None
    }
}

fn file_info_from(path: &str, metadata: &std::fs::Metadata) -> FileResult<FileInfo> {
    let Some(kind) = file_kind(metadata) else {
        return Err(FileError::new(
            FileErrorCode::Invalid,
            "Unsupported file type",
            Some(path.to_string()),
        ));
    };
    let mtime_ms = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    Ok(FileInfo {
        name: Path::new(path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string()),
        path: path.to_string(),
        kind,
        size: metadata.len(),
        mtime_ms,
    })
}

/// The real filesystem, rooted at a working directory.
#[derive(Debug, Clone)]
pub struct LocalFileSystem {
    cwd: String,
}

impl LocalFileSystem {
    pub fn new(cwd: impl Into<String>) -> Self {
        Self { cwd: cwd.into() }
    }

    fn resolve(&self, path: &str) -> String {
        resolve_path(&self.cwd, path)
    }
}

#[async_trait]
impl FileSystem for LocalFileSystem {
    fn cwd(&self) -> &str {
        &self.cwd
    }

    async fn absolute_path(&self, path: &str, abort: Option<AbortSignal>) -> FileResult<String> {
        check_abort(&abort, Some(path))?;
        Ok(self.resolve(path))
    }

    async fn join_path(&self, parts: &[String], abort: Option<AbortSignal>) -> FileResult<String> {
        check_abort(&abort, None)?;
        let mut joined = PathBuf::new();
        for part in parts {
            joined.push(part);
        }
        Ok(joined.to_string_lossy().into_owned())
    }

    async fn read_text_file(&self, path: &str, abort: Option<AbortSignal>) -> FileResult<String> {
        let resolved = self.resolve(path);
        check_abort(&abort, Some(&resolved))?;
        tokio::fs::read_to_string(&resolved)
            .await
            .map_err(|e| FileError::from_io(&e, Some(resolved)))
    }

    async fn read_text_lines(
        &self,
        path: &str,
        max_lines: Option<usize>,
        abort: Option<AbortSignal>,
    ) -> FileResult<Vec<String>> {
        let resolved = self.resolve(path);
        check_abort(&abort, Some(&resolved))?;
        if max_lines == Some(0) {
            return Ok(Vec::new());
        }
        let file = tokio::fs::File::open(&resolved)
            .await
            .map_err(|e| FileError::from_io(&e, Some(resolved.clone())))?;
        let mut reader = BufReader::new(file).lines();
        let mut lines = Vec::new();
        loop {
            check_abort(&abort, Some(&resolved))?;
            let next = reader
                .next_line()
                .await
                .map_err(|e| FileError::from_io(&e, Some(resolved.clone())))?;
            match next {
                Some(line) => lines.push(line),
                None => break,
            }
            if max_lines.is_some_and(|max| lines.len() >= max) {
                break;
            }
        }
        Ok(lines)
    }

    async fn read_binary_file(
        &self,
        path: &str,
        abort: Option<AbortSignal>,
    ) -> FileResult<Vec<u8>> {
        let resolved = self.resolve(path);
        check_abort(&abort, Some(&resolved))?;
        tokio::fs::read(&resolved)
            .await
            .map_err(|e| FileError::from_io(&e, Some(resolved)))
    }

    async fn write_file(
        &self,
        path: &str,
        content: &[u8],
        abort: Option<AbortSignal>,
    ) -> FileResult<()> {
        let resolved = self.resolve(path);
        check_abort(&abort, Some(&resolved))?;
        if let Some(parent) = Path::new(&resolved).parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| FileError::from_io(&e, Some(resolved.clone())))?;
        }
        check_abort(&abort, Some(&resolved))?;
        tokio::fs::write(&resolved, content)
            .await
            .map_err(|e| FileError::from_io(&e, Some(resolved)))
    }

    async fn append_file(
        &self,
        path: &str,
        content: &[u8],
        abort: Option<AbortSignal>,
    ) -> FileResult<()> {
        use tokio::io::AsyncWriteExt;
        let resolved = self.resolve(path);
        check_abort(&abort, Some(&resolved))?;
        if let Some(parent) = Path::new(&resolved).parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| FileError::from_io(&e, Some(resolved.clone())))?;
        }
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&resolved)
            .await
            .map_err(|e| FileError::from_io(&e, Some(resolved.clone())))?;
        file.write_all(content)
            .await
            .map_err(|e| FileError::from_io(&e, Some(resolved)))
    }

    async fn rename_file(
        &self,
        source_path: &str,
        destination_path: &str,
        abort: Option<AbortSignal>,
    ) -> FileResult<()> {
        let source = self.resolve(source_path);
        let destination = self.resolve(destination_path);
        check_abort(&abort, Some(&destination))?;
        // Upstream reports the failure against the source path.
        tokio::fs::rename(&source, &destination)
            .await
            .map_err(|e| FileError::from_io(&e, Some(source)))
    }

    async fn file_info(&self, path: &str, abort: Option<AbortSignal>) -> FileResult<FileInfo> {
        let resolved = self.resolve(path);
        check_abort(&abort, Some(&resolved))?;
        let metadata = tokio::fs::symlink_metadata(&resolved)
            .await
            .map_err(|e| FileError::from_io(&e, Some(resolved.clone())))?;
        file_info_from(&resolved, &metadata)
    }

    async fn list_dir(&self, path: &str, abort: Option<AbortSignal>) -> FileResult<Vec<FileInfo>> {
        let resolved = self.resolve(path);
        check_abort(&abort, Some(&resolved))?;
        let mut entries = tokio::fs::read_dir(&resolved)
            .await
            .map_err(|e| FileError::from_io(&e, Some(resolved.clone())))?;
        let mut infos = Vec::new();
        loop {
            check_abort(&abort, Some(&resolved))?;
            let entry = entries
                .next_entry()
                .await
                .map_err(|e| FileError::from_io(&e, Some(resolved.clone())))?;
            let Some(entry) = entry else { break };
            let entry_path = entry.path().to_string_lossy().into_owned();
            let metadata = tokio::fs::symlink_metadata(&entry_path)
                .await
                .map_err(|e| FileError::from_io(&e, Some(entry_path.clone())))?;
            if let Ok(info) = file_info_from(&entry_path, &metadata) {
                infos.push(info);
            }
        }
        Ok(infos)
    }

    async fn canonical_path(&self, path: &str, abort: Option<AbortSignal>) -> FileResult<String> {
        let resolved = self.resolve(path);
        check_abort(&abort, Some(&resolved))?;
        tokio::fs::canonicalize(&resolved)
            .await
            .map(|p| p.to_string_lossy().into_owned())
            .map_err(|e| FileError::from_io(&e, Some(resolved)))
    }

    async fn exists(&self, path: &str, abort: Option<AbortSignal>) -> FileResult<bool> {
        match self.file_info(path, abort).await {
            Ok(_) => Ok(true),
            Err(error) if error.code == FileErrorCode::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }

    async fn create_dir(
        &self,
        path: &str,
        recursive: bool,
        abort: Option<AbortSignal>,
    ) -> FileResult<()> {
        let resolved = self.resolve(path);
        check_abort(&abort, Some(&resolved))?;
        let result = if recursive {
            tokio::fs::create_dir_all(&resolved).await
        } else {
            tokio::fs::create_dir(&resolved).await
        };
        result.map_err(|e| FileError::from_io(&e, Some(resolved)))
    }

    async fn remove(
        &self,
        path: &str,
        recursive: bool,
        force: bool,
        abort: Option<AbortSignal>,
    ) -> FileResult<()> {
        let resolved = self.resolve(path);
        check_abort(&abort, Some(&resolved))?;
        let metadata = match tokio::fs::symlink_metadata(&resolved).await {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && force => return Ok(()),
            Err(error) => return Err(FileError::from_io(&error, Some(resolved))),
        };
        let result = if metadata.is_dir() {
            if recursive {
                tokio::fs::remove_dir_all(&resolved).await
            } else {
                tokio::fs::remove_dir(&resolved).await
            }
        } else {
            tokio::fs::remove_file(&resolved).await
        };
        match result {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && force => Ok(()),
            Err(error) => Err(FileError::from_io(&error, Some(resolved))),
        }
    }

    async fn create_temp_dir(
        &self,
        prefix: Option<&str>,
        abort: Option<AbortSignal>,
    ) -> FileResult<String> {
        check_abort(&abort, None)?;
        let base = std::env::temp_dir();
        for _ in 0..8 {
            let candidate = base.join(format!(
                "{}{}",
                prefix.unwrap_or("tmp-"),
                uuid::Uuid::new_v4().simple()
            ));
            match tokio::fs::create_dir(&candidate).await {
                Ok(()) => return Ok(candidate.to_string_lossy().into_owned()),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(FileError::from_io(
                        &error,
                        Some(candidate.to_string_lossy().into_owned()),
                    ))
                }
            }
        }
        Err(FileError::new(
            FileErrorCode::Unknown,
            "Could not create a unique temporary directory",
            None,
        ))
    }

    async fn create_temp_file(
        &self,
        prefix: Option<&str>,
        suffix: Option<&str>,
        abort: Option<AbortSignal>,
    ) -> FileResult<String> {
        let dir = self.create_temp_dir(Some("tmp-"), abort.clone()).await?;
        let path = Path::new(&dir)
            .join(format!(
                "{}{}{}",
                prefix.unwrap_or(""),
                uuid::Uuid::new_v4(),
                suffix.unwrap_or("")
            ))
            .to_string_lossy()
            .into_owned();
        tokio::fs::write(&path, b"")
            .await
            .map_err(|e| FileError::from_io(&e, Some(path.clone())))?;
        Ok(path)
    }
}

/// Incremental UTF-8 decoder for pipe reads.
#[derive(Default)]
struct Utf8Decoder {
    pending: Vec<u8>,
}

impl Utf8Decoder {
    fn push(&mut self, bytes: &[u8]) -> String {
        self.pending.extend_from_slice(bytes);
        let mut out = String::new();
        loop {
            match std::str::from_utf8(&self.pending) {
                Ok(text) => {
                    out.push_str(text);
                    self.pending.clear();
                    return out;
                }
                Err(error) => {
                    let valid_up_to = error.valid_up_to();
                    out.push_str(unsafe {
                        // Safe: `valid_up_to` bounds a verified UTF-8 prefix.
                        std::str::from_utf8_unchecked(&self.pending[..valid_up_to])
                    });
                    match error.error_len() {
                        // Truncated sequence: keep the tail for the next read.
                        None => {
                            self.pending.drain(..valid_up_to);
                            return out;
                        }
                        Some(len) => {
                            out.push(char::REPLACEMENT_CHARACTER);
                            self.pending.drain(..valid_up_to + len);
                        }
                    }
                }
            }
        }
    }

    /// Flush any trailing incomplete sequence as replacement characters.
    fn finish(&mut self) -> String {
        if self.pending.is_empty() {
            return String::new();
        }
        self.pending.clear();
        char::REPLACEMENT_CHARACTER.to_string()
    }
}

#[cfg(unix)]
fn kill_process_tree(pid: u32) {
    // The child is its own process-group leader (see `process_group(0)`), so the
    // negated pid reaches every descendant that has not left the group.
    unsafe {
        if libc::killpg(pid as i32, libc::SIGKILL) != 0 {
            libc::kill(pid as i32, libc::SIGKILL);
        }
    }
}

#[cfg(not(unix))]
fn kill_process_tree(_pid: u32) {}

#[derive(Debug, Clone)]
struct ShellConfig {
    shell: String,
    args: Vec<String>,
}

async fn path_exists(path: &str) -> bool {
    tokio::fs::symlink_metadata(path).await.is_ok()
}

async fn find_bash_on_path() -> Option<String> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join("bash");
        if path_exists(&candidate.to_string_lossy()).await {
            return Some(candidate.to_string_lossy().into_owned());
        }
    }
    None
}

async fn get_shell_config(custom_shell_path: Option<&str>) -> ExecResult<ShellConfig> {
    if let Some(custom) = custom_shell_path {
        return if path_exists(custom).await {
            Ok(ShellConfig {
                shell: custom.to_string(),
                args: vec!["-c".to_string()],
            })
        } else {
            Err(ExecutionError::new(
                ExecutionErrorCode::ShellUnavailable,
                format!("Custom shell path not found: {custom}"),
            ))
        };
    }

    if cfg!(windows) {
        return Err(ExecutionError::new(
            ExecutionErrorCode::ShellUnavailable,
            "No bash shell found. Configure an explicit shell path.",
        ));
    }

    if path_exists("/bin/bash").await {
        return Ok(ShellConfig {
            shell: "/bin/bash".to_string(),
            args: vec!["-c".to_string()],
        });
    }
    if let Some(bash) = find_bash_on_path().await {
        return Ok(ShellConfig {
            shell: bash,
            args: vec!["-c".to_string()],
        });
    }
    Ok(ShellConfig {
        shell: "sh".to_string(),
        args: vec!["-c".to_string()],
    })
}

/// The real shell, spawning `bash -c` (or `sh -c`) per command.
#[derive(Debug)]
pub struct LocalShell {
    cwd: String,
    shell_path: Option<String>,
    shell_env: BTreeMap<String, String>,
    active_pids: Mutex<HashSet<u32>>,
}

impl LocalShell {
    pub fn new(cwd: impl Into<String>) -> Self {
        Self {
            cwd: cwd.into(),
            shell_path: None,
            shell_env: BTreeMap::new(),
            active_pids: Mutex::new(HashSet::new()),
        }
    }

    /// Explicit shell binary. Must exist, or `exec` fails with `shell_unavailable`.
    pub fn with_shell_path(mut self, shell_path: Option<String>) -> Self {
        self.shell_path = shell_path;
        self
    }

    /// Base environment layered under the per-call `env` overrides.
    pub fn with_shell_env(mut self, shell_env: BTreeMap<String, String>) -> Self {
        self.shell_env = shell_env;
        self
    }
}

async fn pump<R>(
    mut reader: R,
    buffer: Arc<Mutex<String>>,
    callback: Option<OutputCallback>,
    callback_error: Arc<Mutex<Option<ExecutionError>>>,
    kill: tokio::sync::watch::Sender<bool>,
) where
    R: tokio::io::AsyncRead + Unpin + Send,
{
    let mut decoder = Utf8Decoder::default();
    let mut raw = [0u8; 8192];
    loop {
        let read = match reader.read(&mut raw).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        let mut text = decoder.push(&raw[..read]);
        if text.is_empty() {
            continue;
        }
        buffer.lock().push_str(&text);
        if let Some(callback) = &callback {
            if let Err(message) = callback(&text) {
                *callback_error.lock() = Some(ExecutionError::new(
                    ExecutionErrorCode::CallbackError,
                    message,
                ));
                let _ = kill.send(true);
                return;
            }
        }
        text.clear();
    }
    let trailing = decoder.finish();
    if !trailing.is_empty() {
        buffer.lock().push_str(&trailing);
        if let Some(callback) = &callback {
            if let Err(message) = callback(&trailing) {
                *callback_error.lock() = Some(ExecutionError::new(
                    ExecutionErrorCode::CallbackError,
                    message,
                ));
                let _ = kill.send(true);
            }
        }
    }
}

#[async_trait]
impl Shell for LocalShell {
    async fn exec(&self, command: &str, options: ShellExecOptions) -> ExecResult<ShellOutput> {
        if options.is_aborted() {
            return Err(ExecutionError::aborted());
        }
        let timeout_ms = resolve_timeout_ms(options.timeout_secs)?;
        let cwd = match &options.cwd {
            Some(cwd) => resolve_path(&self.cwd, cwd),
            None => self.cwd.clone(),
        };
        let shell_config = get_shell_config(self.shell_path.as_deref()).await?;
        if !path_exists(&cwd).await {
            return Err(ExecutionError::new(
                ExecutionErrorCode::SpawnError,
                format!("Working directory does not exist: {cwd}\nCannot execute bash commands."),
            ));
        }

        let mut cmd = tokio::process::Command::new(&shell_config.shell);
        cmd.args(&shell_config.args)
            .arg(command)
            .current_dir(&cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if options.inherit_env {
            for (key, value) in &self.shell_env {
                cmd.env(key, value);
            }
        } else {
            cmd.env_clear();
        }
        for (key, value) in &options.env {
            cmd.env(key, value);
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // Own process group so a timeout or abort can kill the whole tree.
            cmd.as_std_mut().process_group(0);
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| ExecutionError::new(ExecutionErrorCode::SpawnError, e.to_string()))?;
        let pid = child.id().unwrap_or(0);
        if pid != 0 {
            self.active_pids.lock().insert(pid);
        }

        let stdout_buffer = Arc::new(Mutex::new(String::new()));
        let stderr_buffer = Arc::new(Mutex::new(String::new()));
        let callback_error: Arc<Mutex<Option<ExecutionError>>> = Arc::new(Mutex::new(None));
        let (kill_tx, mut kill_rx) = tokio::sync::watch::channel(false);

        let stdout_task = child.stdout.take().map(|stdout| {
            tokio::spawn(pump(
                stdout,
                stdout_buffer.clone(),
                options.on_stdout.clone(),
                callback_error.clone(),
                kill_tx.clone(),
            ))
        });
        let stderr_task = child.stderr.take().map(|stderr| {
            tokio::spawn(pump(
                stderr,
                stderr_buffer.clone(),
                options.on_stderr.clone(),
                callback_error.clone(),
                kill_tx.clone(),
            ))
        });

        let timeout_fut = async {
            match timeout_ms {
                Some(ms) => tokio::time::sleep(Duration::from_millis(ms as u64)).await,
                None => std::future::pending::<()>().await,
            }
        };
        tokio::pin!(timeout_fut);
        let abort_signal = options.abort.clone();
        let abort_fut = async {
            match &abort_signal {
                Some(signal) => signal.aborted().await,
                None => std::future::pending::<()>().await,
            }
        };
        tokio::pin!(abort_fut);

        let mut timed_out = false;
        let mut aborted = false;
        let mut callback_killed = false;
        let status = loop {
            tokio::select! {
                biased;
                status = child.wait() => break status,
                _ = &mut timeout_fut, if !timed_out => {
                    timed_out = true;
                    kill_process_tree(pid);
                }
                _ = &mut abort_fut, if !aborted => {
                    aborted = true;
                    kill_process_tree(pid);
                }
                _ = kill_rx.changed(), if !callback_killed => {
                    callback_killed = true;
                    kill_process_tree(pid);
                }
            }
        };

        // Give the pipes a moment to drain; abandon them if a detached
        // descendant is holding them open.
        let _ = tokio::time::timeout(Duration::from_millis(EXIT_STDIO_GRACE_MS), async {
            if let Some(task) = stdout_task {
                let _ = task.await;
            }
            if let Some(task) = stderr_task {
                let _ = task.await;
            }
        })
        .await;

        if pid != 0 {
            self.active_pids.lock().remove(&pid);
        }

        if let Some(error) = callback_error.lock().take() {
            return Err(error);
        }
        if timed_out {
            let seconds = options.timeout_secs.unwrap_or_default();
            return Err(ExecutionError::new(
                ExecutionErrorCode::Timeout,
                format!("timeout:{seconds}"),
            ));
        }
        if aborted || options.is_aborted() {
            return Err(ExecutionError::aborted());
        }
        let status = status
            .map_err(|e| ExecutionError::new(ExecutionErrorCode::SpawnError, e.to_string()))?;

        let stdout = stdout_buffer.lock().clone();
        let stderr = stderr_buffer.lock().clone();
        Ok(ShellOutput {
            stdout,
            stderr,
            exit_code: status.code().unwrap_or(0),
        })
    }

    async fn cleanup_shell(&self) {
        let pids: Vec<u32> = self.active_pids.lock().drain().collect();
        for pid in pids {
            kill_process_tree(pid);
        }
    }
}

/// The default execution environment: real filesystem, real shell.
#[derive(Debug)]
pub struct LocalExecutionEnv {
    fs: LocalFileSystem,
    shell: LocalShell,
}

impl LocalExecutionEnv {
    pub fn new(cwd: impl Into<String>) -> Self {
        let cwd = cwd.into();
        Self {
            fs: LocalFileSystem::new(cwd.clone()),
            shell: LocalShell::new(cwd),
        }
    }

    pub fn with_shell_path(mut self, shell_path: Option<String>) -> Self {
        self.shell = self.shell.with_shell_path(shell_path);
        self
    }

    pub fn with_shell_env(mut self, shell_env: BTreeMap<String, String>) -> Self {
        self.shell = self.shell.with_shell_env(shell_env);
        self
    }

    pub fn file_system(&self) -> &LocalFileSystem {
        &self.fs
    }
}

#[async_trait]
impl FileSystem for LocalExecutionEnv {
    fn cwd(&self) -> &str {
        self.fs.cwd()
    }

    async fn absolute_path(&self, path: &str, abort: Option<AbortSignal>) -> FileResult<String> {
        self.fs.absolute_path(path, abort).await
    }

    async fn join_path(&self, parts: &[String], abort: Option<AbortSignal>) -> FileResult<String> {
        self.fs.join_path(parts, abort).await
    }

    async fn read_text_file(&self, path: &str, abort: Option<AbortSignal>) -> FileResult<String> {
        self.fs.read_text_file(path, abort).await
    }

    async fn read_text_lines(
        &self,
        path: &str,
        max_lines: Option<usize>,
        abort: Option<AbortSignal>,
    ) -> FileResult<Vec<String>> {
        self.fs.read_text_lines(path, max_lines, abort).await
    }

    async fn read_binary_file(
        &self,
        path: &str,
        abort: Option<AbortSignal>,
    ) -> FileResult<Vec<u8>> {
        self.fs.read_binary_file(path, abort).await
    }

    async fn write_file(
        &self,
        path: &str,
        content: &[u8],
        abort: Option<AbortSignal>,
    ) -> FileResult<()> {
        self.fs.write_file(path, content, abort).await
    }

    async fn append_file(
        &self,
        path: &str,
        content: &[u8],
        abort: Option<AbortSignal>,
    ) -> FileResult<()> {
        self.fs.append_file(path, content, abort).await
    }

    async fn rename_file(
        &self,
        source_path: &str,
        destination_path: &str,
        abort: Option<AbortSignal>,
    ) -> FileResult<()> {
        self.fs
            .rename_file(source_path, destination_path, abort)
            .await
    }

    async fn file_info(&self, path: &str, abort: Option<AbortSignal>) -> FileResult<FileInfo> {
        self.fs.file_info(path, abort).await
    }

    async fn list_dir(&self, path: &str, abort: Option<AbortSignal>) -> FileResult<Vec<FileInfo>> {
        self.fs.list_dir(path, abort).await
    }

    async fn canonical_path(&self, path: &str, abort: Option<AbortSignal>) -> FileResult<String> {
        self.fs.canonical_path(path, abort).await
    }

    async fn exists(&self, path: &str, abort: Option<AbortSignal>) -> FileResult<bool> {
        self.fs.exists(path, abort).await
    }

    async fn create_dir(
        &self,
        path: &str,
        recursive: bool,
        abort: Option<AbortSignal>,
    ) -> FileResult<()> {
        self.fs.create_dir(path, recursive, abort).await
    }

    async fn remove(
        &self,
        path: &str,
        recursive: bool,
        force: bool,
        abort: Option<AbortSignal>,
    ) -> FileResult<()> {
        self.fs.remove(path, recursive, force, abort).await
    }

    async fn create_temp_dir(
        &self,
        prefix: Option<&str>,
        abort: Option<AbortSignal>,
    ) -> FileResult<String> {
        self.fs.create_temp_dir(prefix, abort).await
    }

    async fn create_temp_file(
        &self,
        prefix: Option<&str>,
        suffix: Option<&str>,
        abort: Option<AbortSignal>,
    ) -> FileResult<String> {
        self.fs.create_temp_file(prefix, suffix, abort).await
    }
}

#[async_trait]
impl Shell for LocalExecutionEnv {
    async fn exec(&self, command: &str, options: ShellExecOptions) -> ExecResult<ShellOutput> {
        self.shell.exec(command, options).await
    }

    async fn cleanup_shell(&self) {
        self.shell.cleanup_shell().await
    }
}
