//! In-memory [`ExecutionEnv`](crate::types::ExecutionEnv) for tests.
//!
//! Upstream's tool tests subclass `NodeExecutionEnv` to slow down or block
//! individual operations. Rust has no inheritance, so the same seams are hooks
//! on this environment: [`MemoryExecutionEnv::set_write_hook`] and
//! [`MemoryExecutionEnv::set_shell_handler`].
//!
//! Nothing here touches the real filesystem, which is also why the tool tests
//! that must not run a real shell use it.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use futures::future::BoxFuture;
use parking_lot::Mutex;
use pi_core::AbortSignal;

use crate::error::{
    ExecResult, ExecutionError, ExecutionErrorCode, FileError, FileErrorCode, FileResult,
};
use crate::types::{
    check_abort, FileInfo, FileKind, FileSystem, Shell, ShellExecOptions, ShellOutput,
};

/// Scripted `exec` implementation. Receives the command and the options so it
/// can drive `on_stdout` / `on_stderr` just like a real shell.
pub type ShellHandler = Arc<
    dyn Fn(String, ShellExecOptions) -> BoxFuture<'static, ExecResult<ShellOutput>> + Send + Sync,
>;

/// Called before a write lands. Used to block or delay a write in tests.
pub type WriteHook = Arc<dyn Fn(String, Vec<u8>) -> BoxFuture<'static, ()> + Send + Sync>;

#[derive(Debug, Clone)]
enum Node {
    File { content: Vec<u8>, mtime_ms: i64 },
    Dir,
    Symlink { target: String },
}

pub struct MemoryExecutionEnv {
    cwd: String,
    nodes: Mutex<BTreeMap<String, Node>>,
    shell_handler: Mutex<Option<ShellHandler>>,
    write_hook: Mutex<Option<WriteHook>>,
    counter: AtomicUsize,
}

impl std::fmt::Debug for MemoryExecutionEnv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryExecutionEnv")
            .field("cwd", &self.cwd)
            .field("entries", &self.nodes.lock().len())
            .finish()
    }
}

impl MemoryExecutionEnv {
    pub fn new(cwd: impl Into<String>) -> Self {
        let cwd = normalize_absolute(&cwd.into());
        let mut nodes = BTreeMap::new();
        nodes.insert("/".to_string(), Node::Dir);
        let env = Self {
            cwd,
            nodes: Mutex::new(nodes),
            shell_handler: Mutex::new(None),
            write_hook: Mutex::new(None),
            counter: AtomicUsize::new(0),
        };
        let cwd = env.cwd.clone();
        env.ensure_dirs(&cwd);
        env
    }

    pub fn set_shell_handler(&self, handler: ShellHandler) {
        *self.shell_handler.lock() = Some(handler);
    }

    pub fn set_write_hook(&self, hook: WriteHook) {
        *self.write_hook.lock() = Some(hook);
    }

    /// Seed a text file, creating parent directories.
    pub async fn write_text(&self, path: &str, content: &str) {
        self.write_file(path, content.as_bytes(), None)
            .await
            .expect("seed write");
    }

    /// Read a text file, panicking if it is missing. Test convenience.
    pub async fn read_text(&self, path: &str) -> String {
        self.read_text_file(path, None).await.expect("read")
    }

    /// Create a symlink. Only the final component is resolved by
    /// [`FileSystem::canonical_path`], which is all the tools need.
    pub fn symlink(&self, target: &str, link_path: &str) {
        let link = self.resolve(link_path);
        self.ensure_dirs(parent_of(&link));
        self.nodes.lock().insert(
            link,
            Node::Symlink {
                target: target.to_string(),
            },
        );
    }

    fn resolve(&self, path: &str) -> String {
        if path.starts_with('/') {
            normalize_absolute(path)
        } else {
            normalize_absolute(&format!("{}/{}", self.cwd, path))
        }
    }

    fn ensure_dirs(&self, path: &str) {
        let mut nodes = self.nodes.lock();
        let mut current = String::from("/");
        nodes.entry(current.clone()).or_insert(Node::Dir);
        for segment in path.split('/').filter(|s| !s.is_empty()) {
            if current == "/" {
                current = format!("/{segment}");
            } else {
                current = format!("{current}/{segment}");
            }
            nodes.entry(current.clone()).or_insert(Node::Dir);
        }
    }

    fn info_at(&self, path: &str) -> FileResult<FileInfo> {
        let nodes = self.nodes.lock();
        let node = nodes.get(path).ok_or_else(|| {
            FileError::not_found(format!("ENOENT: {path}"), Some(path.to_string()))
        })?;
        let (kind, size, mtime_ms) = match node {
            Node::File { content, mtime_ms } => (FileKind::File, content.len() as u64, *mtime_ms),
            Node::Dir => (FileKind::Directory, 0, 0),
            Node::Symlink { target } => (FileKind::Symlink, target.len() as u64, 0),
        };
        Ok(FileInfo {
            name: basename(path).to_string(),
            path: path.to_string(),
            kind,
            size,
            mtime_ms,
        })
    }
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn parent_of(path: &str) -> &str {
    match path.rfind('/') {
        Some(0) | None => "/",
        Some(index) => &path[..index],
    }
}

/// Lexical normalization: collapse `//`, resolve `.` and `..`, drop a trailing `/`.
fn normalize_absolute(path: &str) -> String {
    let mut segments: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            other => segments.push(other),
        }
    }
    if segments.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", segments.join("/"))
    }
}

#[async_trait]
impl FileSystem for MemoryExecutionEnv {
    fn cwd(&self) -> &str {
        &self.cwd
    }

    async fn absolute_path(&self, path: &str, abort: Option<AbortSignal>) -> FileResult<String> {
        check_abort(&abort, Some(path))?;
        Ok(self.resolve(path))
    }

    async fn join_path(&self, parts: &[String], abort: Option<AbortSignal>) -> FileResult<String> {
        check_abort(&abort, None)?;
        Ok(normalize_absolute(&parts.join("/")))
    }

    async fn read_text_file(&self, path: &str, abort: Option<AbortSignal>) -> FileResult<String> {
        let bytes = self.read_binary_file(path, abort).await?;
        String::from_utf8(bytes)
            .map_err(|e| FileError::new(FileErrorCode::Invalid, e.to_string(), Some(path.into())))
    }

    async fn read_text_lines(
        &self,
        path: &str,
        max_lines: Option<usize>,
        abort: Option<AbortSignal>,
    ) -> FileResult<Vec<String>> {
        if max_lines == Some(0) {
            return Ok(Vec::new());
        }
        let text = self.read_text_file(path, abort).await?;
        let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
        if let Some(max) = max_lines {
            lines.truncate(max);
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
        let node = self.nodes.lock().get(&resolved).cloned();
        match node {
            Some(Node::File { content, .. }) => Ok(content),
            Some(Node::Dir) => Err(FileError::new(
                FileErrorCode::IsDirectory,
                format!("EISDIR: {resolved}"),
                Some(resolved),
            )),
            Some(Node::Symlink { target }) => {
                let resolved_target = if target.starts_with('/') {
                    normalize_absolute(&target)
                } else {
                    normalize_absolute(&format!("{}/{}", parent_of(&resolved), target))
                };
                Box::pin(self.read_binary_file(&resolved_target, abort)).await
            }
            None => Err(FileError::not_found(
                format!("ENOENT: {resolved}"),
                Some(resolved),
            )),
        }
    }

    async fn write_file(
        &self,
        path: &str,
        content: &[u8],
        abort: Option<AbortSignal>,
    ) -> FileResult<()> {
        let resolved = self.resolve(path);
        check_abort(&abort, Some(&resolved))?;
        let hook = self.write_hook.lock().clone();
        if let Some(hook) = hook {
            hook(resolved.clone(), content.to_vec()).await;
        }
        check_abort(&abort, Some(&resolved))?;
        self.ensure_dirs(parent_of(&resolved));
        // Writes through a symlink land on its target, like the real filesystem.
        let target = {
            let nodes = self.nodes.lock();
            match nodes.get(&resolved) {
                Some(Node::Symlink { target }) => Some(target.clone()),
                _ => None,
            }
        };
        let destination = match target {
            Some(target) if target.starts_with('/') => normalize_absolute(&target),
            Some(target) => normalize_absolute(&format!("{}/{}", parent_of(&resolved), target)),
            None => resolved,
        };
        self.nodes.lock().insert(
            destination,
            Node::File {
                content: content.to_vec(),
                mtime_ms: pi_core::now_ms(),
            },
        );
        Ok(())
    }

    async fn append_file(
        &self,
        path: &str,
        content: &[u8],
        abort: Option<AbortSignal>,
    ) -> FileResult<()> {
        let existing = match self.read_binary_file(path, abort.clone()).await {
            Ok(bytes) => bytes,
            Err(error) if error.code == FileErrorCode::NotFound => Vec::new(),
            Err(error) => return Err(error),
        };
        let mut merged = existing;
        merged.extend_from_slice(content);
        self.write_file(path, &merged, abort).await
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
        let mut nodes = self.nodes.lock();
        let node = nodes.remove(&source).ok_or_else(|| {
            FileError::not_found(format!("ENOENT: {source}"), Some(source.clone()))
        })?;
        nodes.insert(destination, node);
        Ok(())
    }

    async fn file_info(&self, path: &str, abort: Option<AbortSignal>) -> FileResult<FileInfo> {
        let resolved = self.resolve(path);
        check_abort(&abort, Some(&resolved))?;
        self.info_at(&resolved)
    }

    async fn list_dir(&self, path: &str, abort: Option<AbortSignal>) -> FileResult<Vec<FileInfo>> {
        let resolved = self.resolve(path);
        check_abort(&abort, Some(&resolved))?;
        match self.info_at(&resolved)?.kind {
            FileKind::Directory => {}
            _ => {
                return Err(FileError::new(
                    FileErrorCode::NotDirectory,
                    format!("ENOTDIR: {resolved}"),
                    Some(resolved),
                ))
            }
        }
        let prefix = if resolved == "/" {
            "/".to_string()
        } else {
            format!("{resolved}/")
        };
        let children: Vec<String> = {
            let nodes = self.nodes.lock();
            nodes
                .keys()
                .filter(|key| {
                    key.starts_with(&prefix)
                        && !key[prefix.len()..].contains('/')
                        && *key != &prefix
                })
                .cloned()
                .collect()
        };
        children.iter().map(|child| self.info_at(child)).collect()
    }

    async fn canonical_path(&self, path: &str, abort: Option<AbortSignal>) -> FileResult<String> {
        let mut resolved = self.resolve(path);
        check_abort(&abort, Some(&resolved))?;
        for _ in 0..10 {
            let node = {
                let nodes = self.nodes.lock();
                nodes.get(&resolved).cloned()
            };
            match node {
                Some(Node::Symlink { target }) => {
                    resolved = if target.starts_with('/') {
                        normalize_absolute(&target)
                    } else {
                        normalize_absolute(&format!("{}/{}", parent_of(&resolved), target))
                    };
                }
                Some(_) => return Ok(resolved),
                None => {
                    return Err(FileError::not_found(
                        format!("ENOENT: {resolved}"),
                        Some(resolved),
                    ))
                }
            }
        }
        Err(FileError::new(
            FileErrorCode::Invalid,
            "ELOOP: too many symlink levels",
            Some(resolved),
        ))
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
        if !recursive && !self.nodes.lock().contains_key(parent_of(&resolved)) {
            return Err(FileError::not_found(
                format!("ENOENT: {}", parent_of(&resolved)),
                Some(resolved),
            ));
        }
        self.ensure_dirs(&resolved);
        Ok(())
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
        let mut nodes = self.nodes.lock();
        if !nodes.contains_key(&resolved) {
            return if force {
                Ok(())
            } else {
                Err(FileError::not_found(
                    format!("ENOENT: {resolved}"),
                    Some(resolved),
                ))
            };
        }
        let prefix = format!("{resolved}/");
        let children: Vec<String> = nodes
            .keys()
            .filter(|key| key.starts_with(&prefix))
            .cloned()
            .collect();
        if !children.is_empty() && !recursive {
            return Err(FileError::new(
                FileErrorCode::Invalid,
                format!("ENOTEMPTY: {resolved}"),
                Some(resolved),
            ));
        }
        for child in children {
            nodes.remove(&child);
        }
        nodes.remove(&resolved);
        Ok(())
    }

    async fn create_temp_dir(
        &self,
        prefix: Option<&str>,
        abort: Option<AbortSignal>,
    ) -> FileResult<String> {
        check_abort(&abort, None)?;
        let id = self.counter.fetch_add(1, Ordering::SeqCst);
        let path = format!("/tmp/{}{id}", prefix.unwrap_or("tmp-"));
        self.ensure_dirs(&path);
        Ok(path)
    }

    async fn create_temp_file(
        &self,
        prefix: Option<&str>,
        suffix: Option<&str>,
        abort: Option<AbortSignal>,
    ) -> FileResult<String> {
        let dir = self.create_temp_dir(Some("tmp-"), abort.clone()).await?;
        let id = self.counter.fetch_add(1, Ordering::SeqCst);
        let path = format!("{dir}/{}{id}{}", prefix.unwrap_or(""), suffix.unwrap_or(""));
        self.write_file(&path, b"", abort).await?;
        Ok(path)
    }
}

#[async_trait]
impl Shell for MemoryExecutionEnv {
    async fn exec(&self, command: &str, options: ShellExecOptions) -> ExecResult<ShellOutput> {
        let handler = self.shell_handler.lock().clone();
        match handler {
            Some(handler) => handler(command.to_string(), options).await,
            None => Err(ExecutionError::new(
                ExecutionErrorCode::ShellUnavailable,
                "MemoryExecutionEnv has no shell handler configured",
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reads_writes_lists_and_removes() {
        let env = MemoryExecutionEnv::new("/work");
        env.write_text("nested/child/file.txt", "hel").await;
        env.append_file("nested/child/file.txt", b"lo", None)
            .await
            .unwrap();
        assert_eq!(env.read_text("nested/child/file.txt").await, "hello");

        let entries = env.list_dir("nested/child", None).await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "file.txt");
        assert_eq!(entries[0].kind, FileKind::File);
        assert_eq!(entries[0].size, 5);

        assert!(env.exists("nested/child/file.txt", None).await.unwrap());
        env.remove("nested/child/file.txt", false, false, None)
            .await
            .unwrap();
        assert!(!env.exists("nested/child/file.txt", None).await.unwrap());
    }

    #[tokio::test]
    async fn resolves_symlinks_only_through_canonical_path() {
        let env = MemoryExecutionEnv::new("/work");
        env.write_text("target.txt", "hello").await;
        env.symlink("target.txt", "link.txt");

        assert_eq!(
            env.file_info("link.txt", None).await.unwrap().kind,
            FileKind::Symlink
        );
        assert_eq!(
            env.canonical_path("link.txt", None).await.unwrap(),
            "/work/target.txt"
        );
        assert_eq!(env.read_text("link.txt").await, "hello");
    }

    #[tokio::test]
    async fn reports_missing_paths_as_not_found() {
        let env = MemoryExecutionEnv::new("/work");
        let error = env.file_info("missing.txt", None).await.unwrap_err();
        assert_eq!(error.code, FileErrorCode::NotFound);
        assert_eq!(error.path.as_deref(), Some("/work/missing.txt"));
        assert!(!env.exists("missing.txt", None).await.unwrap());
    }

    #[tokio::test]
    async fn rejects_pre_aborted_operations() {
        let env = MemoryExecutionEnv::new("/work");
        env.write_text("file.txt", "hello").await;
        let (handle, signal) = pi_core::AbortHandle::new();
        handle.abort();

        let error = env
            .read_text_file("file.txt", Some(signal.clone()))
            .await
            .unwrap_err();
        assert_eq!(error.code, FileErrorCode::Aborted);
        let error = env
            .write_file("other.txt", b"x", Some(signal))
            .await
            .unwrap_err();
        assert_eq!(error.code, FileErrorCode::Aborted);
    }

    #[tokio::test]
    async fn shell_is_unavailable_until_scripted() {
        let env = MemoryExecutionEnv::new("/work");
        let error = env
            .exec("echo hi", ShellExecOptions::new())
            .await
            .unwrap_err();
        assert_eq!(error.code, ExecutionErrorCode::ShellUnavailable);

        env.set_shell_handler(Arc::new(|command, options| {
            Box::pin(async move {
                if let Some(cb) = &options.on_stdout {
                    cb(&format!("ran {command}")).ok();
                }
                Ok(ShellOutput {
                    stdout: format!("ran {command}"),
                    stderr: String::new(),
                    exit_code: 0,
                })
            })
        }));
        let output = env.exec("echo hi", ShellExecOptions::new()).await.unwrap();
        assert_eq!(output.stdout, "ran echo hi");
    }
}
