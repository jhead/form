//! Port of `.upstream/packages/server/src/transports/unix/listener.ts`.
//!
//! The filesystem dance is the interesting part and is preserved exactly: bind
//! to a private derived path, hard-link it to the advertised path, and on
//! shutdown only unlink an inode this listener still owns. That is what stops a
//! restart from deleting a *different* server's socket, and it is covered by
//! upstream's own tests.

use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::future::Shared;
use futures::FutureExt;
use parking_lot::Mutex;
use pi_protocol::DEFAULT_MAX_FRAME_LENGTH;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener as TokioUnixListener, UnixStream};
use tokio::sync::mpsc;

use crate::connection::{
    ByteConnection, ByteConnectionAcceptor, ByteConnectionHandler, CloseFuture, SendFuture,
};
use crate::errors::TransportError;
use crate::listener::PiServerListener;
use crate::types::{ServerErrorHandler, ServerErrorReport};
use crate::unix::types::UnixListenerOptions;

const DEFAULT_SOCKET_MODE: u32 = 0o600;
const DEFAULT_GRACEFUL_CLOSE_TIMEOUT_MS: u64 = 5_000;
const SOCKET_PROBE_TIMEOUT: Duration = Duration::from_millis(1_000);
const READ_CHUNK: usize = 64 * 1024;

/// `sizeof(sockaddr_un.sun_path) - 1`.
pub const MAX_UNIX_SOCKET_PATH_BYTES: usize = if cfg!(target_os = "linux") { 107 } else { 103 };

pub fn validate_unix_socket_path(path: &Path, description: &str) -> Result<(), TransportError> {
    let bytes = path.as_os_str().as_encoded_bytes();
    if bytes.is_empty() {
        return Err(TransportError::new(format!(
            "{description} must not be empty"
        )));
    }
    if bytes.len() > MAX_UNIX_SOCKET_PATH_BYTES {
        return Err(TransportError::new(format!(
            "{description} is too long; maximum is {MAX_UNIX_SOCKET_PATH_BYTES} UTF-8 bytes"
        )));
    }
    Ok(())
}

struct ResolvedOptions {
    path: PathBuf,
    mode: u32,
    graceful_close_timeout: Duration,
    max_pending_bytes: u64,
    on_error: Option<ServerErrorHandler>,
}

fn resolve(options: UnixListenerOptions) -> Result<ResolvedOptions, TransportError> {
    validate_unix_socket_path(&options.path, "PiServer Unix socket path")?;
    let mode = options.mode.unwrap_or(DEFAULT_SOCKET_MODE);
    if mode > 0o777 {
        return Err(TransportError::new(
            "PiServer Unix socket mode must be an integer between 0 and 0o777",
        ));
    }
    let max_frame_length = options.max_frame_length.unwrap_or(DEFAULT_MAX_FRAME_LENGTH);
    if max_frame_length == 0 {
        return Err(TransportError::new(format!(
            "PiServer maxFrameLength must be an integer between 1 and {}",
            u32::MAX
        )));
    }
    let max_frame_length = u64::from(max_frame_length);
    let max_pending_bytes = options.max_pending_bytes.unwrap_or(max_frame_length * 4);
    if max_pending_bytes < max_frame_length + 4 {
        return Err(TransportError::new(
            "PiServer maxPendingBytes must be a safe integer at least maxFrameLength + 4",
        ));
    }
    let graceful_close_timeout_ms = options
        .graceful_close_timeout_ms
        .unwrap_or(DEFAULT_GRACEFUL_CLOSE_TIMEOUT_MS);
    if graceful_close_timeout_ms == 0 || graceful_close_timeout_ms > i32::MAX as u64 {
        return Err(TransportError::new(format!(
            "PiServer gracefulCloseTimeoutMs must be an integer between 1 and {}",
            i32::MAX
        )));
    }
    Ok(ResolvedOptions {
        path: options.path,
        mode,
        graceful_close_timeout: Duration::from_millis(graceful_close_timeout_ms),
        max_pending_bytes,
        on_error: options.on_error,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileIdentity {
    dev: u64,
    ino: u64,
}

#[derive(Default)]
struct ListenerState {
    accept_task: Option<tokio::task::JoinHandle<()>>,
    socket_identity: Option<FileIdentity>,
    owned_bind_path: Option<PathBuf>,
    bound_path: Option<PathBuf>,
    connections: Vec<(u64, Arc<UnixByteConnection>)>,
    next_connection_id: u64,
    started: bool,
}

pub struct UnixListener {
    self_weak: std::sync::Weak<UnixListener>,
    options: ResolvedOptions,
    state: Mutex<ListenerState>,
    closing: AtomicBool,
    close_guard: tokio::sync::Mutex<bool>,
}

pub fn create_unix_listener(
    options: UnixListenerOptions,
) -> Result<Arc<dyn PiServerListener>, TransportError> {
    let options = resolve(options)?;
    let listener = Arc::new_cyclic(|weak: &std::sync::Weak<UnixListener>| UnixListener {
        self_weak: weak.clone(),
        options,
        state: Mutex::new(ListenerState::default()),
        closing: AtomicBool::new(false),
        close_guard: tokio::sync::Mutex::new(false),
    });
    Ok(listener)
}

impl UnixListener {
    fn report(&self, error: TransportError) {
        let Some(handler) = &self.options.on_error else {
            return;
        };
        // Error observers cannot affect listener state.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            handler(ServerErrorReport::Transport(error))
        }));
    }

    fn register(self: &Arc<Self>, socket: UnixStream, accept: &Arc<dyn ByteConnectionAcceptor>) {
        if self.closing.load(Ordering::SeqCst) {
            drop(socket);
            return;
        }
        let connection = UnixByteConnection::new(
            socket,
            self.options.graceful_close_timeout,
            self.options.max_pending_bytes,
        );
        let id = {
            let mut state = self.state.lock();
            state.next_connection_id += 1;
            let id = state.next_connection_id;
            state.connections.push((id, Arc::clone(&connection)));
            id
        };
        let handler = accept.accept(Arc::clone(&connection) as Arc<dyn ByteConnection>);
        let listener = self.self_weak.clone();
        connection.start_reader(handler, move || {
            if let Some(listener) = listener.upgrade() {
                listener
                    .state
                    .lock()
                    .connections
                    .retain(|(candidate, _)| *candidate != id);
            }
        });
    }

    async fn cleanup_owned_socket(&self) -> Result<(), TransportError> {
        let (identity, path) = {
            let mut state = self.state.lock();
            (state.socket_identity.take(), self.options.path.clone())
        };
        let Some(identity) = identity else {
            return Ok(());
        };
        let current = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        if !current.file_type().is_socket()
            || current.dev() != identity.dev
            || current.ino() != identity.ino
        {
            return Ok(());
        }

        // Rename-then-verify: only remove the inode this listener created, even
        // if something replaced the path between the stat and the unlink.
        let preserved = sibling(&path, ".c-", &random_suffix());
        match std::fs::rename(&path, &preserved) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        }
        let moved = std::fs::symlink_metadata(&preserved)?;
        if moved.file_type().is_socket()
            && moved.dev() == identity.dev
            && moved.ino() == identity.ino
        {
            remove_path(&preserved)?;
            return Ok(());
        }
        match std::fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                std::fs::rename(&preserved, &path)?;
            }
            Err(error) => return Err(error.into()),
            Ok(_) => {}
        }
        Err(TransportError::new(format!(
            "Unix listener path changed during cleanup; preserved replacement at {}",
            preserved.display()
        )))
    }

    async fn close_internal(&self) {
        let (accept_task, connections, owned) = {
            let mut state = self.state.lock();
            state.bound_path = None;
            (
                state.accept_task.take(),
                std::mem::take(&mut state.connections),
                state.owned_bind_path.take(),
            )
        };
        if let Some(accept_task) = accept_task {
            accept_task.abort();
        }
        for (_, connection) in connections {
            let _ = connection.close(None).await;
        }
        if let Err(error) = self.cleanup_owned_socket().await {
            self.report(error);
        }
        if let Some(owned) = owned {
            if let Err(error) = remove_path(&owned) {
                self.report(error);
            }
        }
        self.state.lock().started = false;
    }
}

#[async_trait]
impl PiServerListener for UnixListener {
    fn address(&self) -> Option<String> {
        self.state
            .lock()
            .bound_path
            .as_ref()
            .map(|path| path.display().to_string())
    }

    async fn start(&self, accept: Arc<dyn ByteConnectionAcceptor>) -> Result<(), TransportError> {
        if self.state.lock().started {
            return Err(TransportError::new("Unix listener is already started"));
        }
        if self.closing.load(Ordering::SeqCst) {
            return Err(TransportError::new("Unix listener is closing or closed"));
        }

        let path = self.options.path.clone();
        let owned_bind_path = owned_bind_path(&path);
        validate_unix_socket_path(&owned_bind_path, "PiServer private Unix bind path")?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
            let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
        }
        remove_stale_socket(&path).await?;
        remove_stale_socket(&owned_bind_path).await?;
        self.state.lock().owned_bind_path = Some(owned_bind_path.clone());

        let bound = TokioUnixListener::bind(&owned_bind_path)?;
        let outcome = (|| -> Result<FileIdentity, TransportError> {
            let stats = std::fs::symlink_metadata(&owned_bind_path)?;
            if !stats.file_type().is_socket() {
                return Err(TransportError::new(format!(
                    "Unix listener path is not a socket after binding: {}",
                    owned_bind_path.display()
                )));
            }
            let identity = FileIdentity {
                dev: stats.dev(),
                ino: stats.ino(),
            };
            std::fs::hard_link(&owned_bind_path, &path)?;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(self.options.mode))?;
            Ok(identity)
        })();

        let identity = match outcome {
            Ok(identity) => identity,
            Err(error) => {
                drop(bound);
                if let Err(cleanup) = self.cleanup_owned_socket().await {
                    self.report(cleanup);
                }
                let owned = self.state.lock().owned_bind_path.take();
                if let Some(owned) = owned {
                    let _ = remove_path(&owned);
                }
                return Err(error);
            }
        };

        let accept_task = {
            let listener = self.self_weak.clone();
            tokio::spawn(async move {
                loop {
                    match bound.accept().await {
                        Ok((socket, _)) => match listener.upgrade() {
                            Some(listener) => listener.register(socket, &accept),
                            None => return,
                        },
                        Err(error) => {
                            if let Some(listener) = listener.upgrade() {
                                listener.report(TransportError::from(error));
                            }
                            return;
                        }
                    }
                }
            })
        };

        {
            let mut state = self.state.lock();
            state.socket_identity = Some(identity);
            state.bound_path = Some(path);
            state.accept_task = Some(accept_task);
            state.started = true;
        }
        Ok(())
    }

    async fn close(&self) -> Result<(), TransportError> {
        self.closing.store(true, Ordering::SeqCst);
        let mut guard = self.close_guard.lock().await;
        if *guard {
            return Ok(());
        }
        self.close_internal().await;
        *guard = true;
        Ok(())
    }
}

/// The bind path is derived from the advertised path so two servers configured
/// for the same socket collide on it too.
fn owned_bind_path(path: &Path) -> PathBuf {
    let digest = Sha256::digest(path.as_os_str().as_encoded_bytes());
    let suffix: String = format!("{digest:x}").chars().take(8).collect();
    sibling(path, ".p-", &suffix)
}

fn sibling(path: &Path, prefix: &str, suffix: &str) -> PathBuf {
    let parent = path.parent().unwrap_or(Path::new("."));
    parent.join(format!("{prefix}{suffix}"))
}

fn random_suffix() -> String {
    uuid::Uuid::new_v4().to_string().chars().take(6).collect()
}

fn remove_path(path: &Path) -> Result<(), TransportError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

async fn remove_stale_socket(path: &Path) -> Result<(), TransportError> {
    let original = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if !original.file_type().is_socket() {
        return Err(TransportError::new(format!(
            "Refusing to remove non-socket Unix listener path: {}",
            path.display()
        )));
    }
    if is_socket_live(path).await? {
        return Err(TransportError::new(format!(
            "Unix listener is already running: {}",
            path.display()
        )));
    }

    let preserved = sibling(path, ".s-", &random_suffix());
    match std::fs::rename(path, &preserved) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    }
    let current = std::fs::symlink_metadata(&preserved)?;
    if !current.file_type().is_socket()
        || current.dev() != original.dev()
        || current.ino() != original.ino()
    {
        match std::fs::symlink_metadata(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                std::fs::rename(&preserved, path)?;
            }
            Err(error) => return Err(error.into()),
            Ok(_) => {}
        }
        return Err(TransportError::new(format!(
            "Unix listener path changed while checking for a stale socket: {}",
            path.display()
        )));
    }
    remove_path(&preserved)
}

async fn is_socket_live(path: &Path) -> Result<bool, TransportError> {
    match tokio::time::timeout(SOCKET_PROBE_TIMEOUT, UnixStream::connect(path)).await {
        // A probe that times out is treated as live, like upstream.
        Err(_) => Ok(true),
        Ok(Ok(_)) => Ok(true),
        Ok(Err(error)) => match error.kind() {
            std::io::ErrorKind::ConnectionRefused
            | std::io::ErrorKind::NotFound
            | std::io::ErrorKind::BrokenPipe
            | std::io::ErrorKind::ConnectionReset => Ok(false),
            _ => Ok(false),
        },
    }
}

// ---------------------------------------------------------------------------
// connection
// ---------------------------------------------------------------------------

type SharedUnit = Shared<futures::channel::oneshot::Receiver<()>>;

enum Job {
    Write {
        bytes: Vec<u8>,
        done: tokio::sync::oneshot::Sender<Result<(), TransportError>>,
    },
    Final(Option<Vec<u8>>),
}

/// Exported for transport-level verification, like upstream's `@internal`
/// `UnixByteConnection`.
pub struct UnixByteConnection {
    queue: Mutex<Option<mpsc::UnboundedSender<Job>>>,
    pending_bytes: Arc<AtomicU64>,
    max_pending_bytes: u64,
    closed: Arc<AtomicBool>,
    closing: Arc<AtomicBool>,
    graceful_close_timeout: Duration,
    writer_done: SharedUnit,
    reader: Mutex<Option<tokio::task::JoinHandle<()>>>,
    read_half: Mutex<Option<tokio::net::unix::OwnedReadHalf>>,
}

impl UnixByteConnection {
    pub fn new(
        socket: UnixStream,
        graceful_close_timeout: Duration,
        max_pending_bytes: u64,
    ) -> Arc<Self> {
        let (read_half, mut write_half) = socket.into_split();
        let (queue_tx, mut queue_rx) = mpsc::unbounded_channel::<Job>();
        let (done_tx, done_rx) = futures::channel::oneshot::channel();

        tokio::spawn(async move {
            while let Some(job) = queue_rx.recv().await {
                match job {
                    Job::Write { bytes, done } => {
                        let result = write_half
                            .write_all(&bytes)
                            .await
                            .map_err(TransportError::from);
                        let _ = done.send(result);
                    }
                    Job::Final(bytes) => {
                        if let Some(bytes) = bytes {
                            let _ = write_half.write_all(&bytes).await;
                        }
                        break;
                    }
                }
            }
            let _ = write_half.shutdown().await;
            let _ = done_tx.send(());
        });

        Arc::new(Self {
            queue: Mutex::new(Some(queue_tx)),
            pending_bytes: Arc::new(AtomicU64::new(0)),
            max_pending_bytes,
            closed: Arc::new(AtomicBool::new(false)),
            closing: Arc::new(AtomicBool::new(false)),
            graceful_close_timeout,
            writer_done: done_rx.shared(),
            reader: Mutex::new(None),
            read_half: Mutex::new(Some(read_half)),
        })
    }

    pub(crate) fn start_reader(
        self: &Arc<Self>,
        handler: Arc<dyn ByteConnectionHandler>,
        on_socket_closed: impl Fn() + Send + Sync + 'static,
    ) {
        let Some(mut read_half) = self.read_half.lock().take() else {
            return;
        };
        let connection = Arc::downgrade(self);
        let task = tokio::spawn(async move {
            let mut buffer = vec![0u8; READ_CHUNK];
            loop {
                match read_half.read(&mut buffer).await {
                    Ok(0) => break,
                    Ok(count) => handler.on_data(&buffer[..count]),
                    Err(error) => {
                        handler.on_error(TransportError::from(error));
                        break;
                    }
                }
            }
            if let Some(connection) = connection.upgrade() {
                connection.mark_closed();
            }
            on_socket_closed();
            handler.on_close();
        });
        *self.reader.lock() = Some(task);
    }

    pub fn mark_closed(&self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        self.closing.store(true, Ordering::SeqCst);
        self.queue.lock().take();
    }

    fn destroy(&self) {
        self.mark_closed();
        if let Some(reader) = self.reader.lock().take() {
            reader.abort();
        }
        self.read_half.lock().take();
    }
}

impl ByteConnection for UnixByteConnection {
    fn closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    fn send(&self, chunk: Vec<u8>) -> SendFuture {
        if self.closed.load(Ordering::SeqCst) || self.closing.load(Ordering::SeqCst) {
            return Box::pin(async { Err(TransportError::new("Unix connection is closed")) });
        }
        let length = chunk.len() as u64;
        let reserved =
            self.pending_bytes
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |pending| {
                    (pending + length <= self.max_pending_bytes).then_some(pending + length)
                });
        if reserved.is_err() {
            return Box::pin(async {
                Err(TransportError::new(
                    "Unix connection exceeded its pending byte limit",
                ))
            });
        }
        let (done, wait) = tokio::sync::oneshot::channel();
        let queued = self
            .queue
            .lock()
            .as_ref()
            .map(|sender| sender.send(Job::Write { bytes: chunk, done }).is_ok())
            .unwrap_or(false);
        if !queued {
            self.pending_bytes.fetch_sub(length, Ordering::SeqCst);
            return Box::pin(async { Err(TransportError::new("Unix connection is closed")) });
        }
        let pending = Arc::clone(&self.pending_bytes);
        Box::pin(async move {
            let outcome = wait.await.unwrap_or_else(|_| {
                Err(TransportError::new("Unix connection closed during write"))
            });
            pending.fetch_sub(length, Ordering::SeqCst);
            outcome
        })
    }

    fn close(&self, final_chunk: Option<Vec<u8>>) -> CloseFuture {
        if self.closed.load(Ordering::SeqCst) {
            return Box::pin(async { Ok(()) });
        }
        if !self.closing.swap(true, Ordering::SeqCst) {
            // Queue the final frame behind everything already pending, then
            // drop the sender so the writer task finishes and shuts down.
            let sender = self.queue.lock().take();
            if let Some(sender) = sender {
                let _ = sender.send(Job::Final(final_chunk));
            }
        }
        let writer_done = self.writer_done.clone();
        let timeout = self.graceful_close_timeout;
        let reader = self.reader.lock().take();
        let closed = Arc::clone(&self.closed);
        Box::pin(async move {
            // Graceful: let the queued output drain, then force the socket down.
            let _ = tokio::time::timeout(timeout, writer_done).await;
            if let Some(reader) = reader {
                reader.abort();
            }
            closed.store(true, Ordering::SeqCst);
            Ok(())
        })
    }
}

impl Drop for UnixByteConnection {
    fn drop(&mut self) {
        if let Some(reader) = self.reader.lock().take() {
            reader.abort();
        }
    }
}

impl UnixByteConnection {
    /// Upstream keeps `destroy` implicit in `socket.destroy()`; exposed here so
    /// tests can force a hard close.
    pub fn force_close(&self) {
        self.destroy();
    }
}
