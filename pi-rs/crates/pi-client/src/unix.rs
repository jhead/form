//! Port of `.upstream/packages/client/src/unix.ts`.
//!
//! The write path mirrors upstream's `writeTail` + `pendingBytes`: `send`
//! reserves its share of the budget and enqueues synchronously, a single writer
//! task drains the queue in order, and the returned future resolves when that
//! chunk reaches the socket.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use pi_protocol::DEFAULT_MAX_FRAME_LENGTH;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot};

use crate::errors::TransportError;
use crate::transport::{
    ByteTransport, ByteTransportFactory, ByteTransportHandlers, ConnectFuture, SendFuture,
};

/// `sizeof(sockaddr_un.sun_path) - 1`, which differs per platform.
pub const MAX_UNIX_SOCKET_PATH_BYTES: usize = if cfg!(target_os = "linux") { 107 } else { 103 };

const READ_CHUNK: usize = 64 * 1024;

#[derive(Debug, Clone)]
pub struct UnixTransportOptions {
    pub path: PathBuf,
    pub max_pending_bytes: Option<u64>,
}

impl UnixTransportOptions {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            max_pending_bytes: None,
        }
    }
}

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

/// Creates fresh Unix-domain socket transports for `PiClient` attempts.
#[derive(Debug)]
pub struct UnixTransportFactory {
    path: PathBuf,
    max_pending_bytes: u64,
}

impl UnixTransportFactory {
    pub fn new(options: UnixTransportOptions) -> Result<Arc<Self>, TransportError> {
        validate_unix_socket_path(&options.path, "Unix transport path")?;
        let max_pending_bytes = options
            .max_pending_bytes
            .unwrap_or(u64::from(DEFAULT_MAX_FRAME_LENGTH) * 4);
        if max_pending_bytes == 0 {
            return Err(TransportError::new(
                "Unix transport maxPendingBytes must be a positive safe integer",
            ));
        }
        Ok(Arc::new(Self {
            path: options.path,
            max_pending_bytes,
        }))
    }
}

impl ByteTransportFactory for UnixTransportFactory {
    fn connect(&self, handlers: Arc<dyn ByteTransportHandlers>) -> ConnectFuture {
        let path = self.path.clone();
        let max_pending_bytes = self.max_pending_bytes;
        Box::pin(async move {
            let stream = UnixStream::connect(&path)
                .await
                .map_err(TransportError::from)?;
            Ok(spawn_transport(stream, max_pending_bytes, handlers))
        })
    }
}

struct WriteJob {
    bytes: Vec<u8>,
    done: oneshot::Sender<Result<(), TransportError>>,
}

pub(crate) struct UnixByteTransport {
    queue: Mutex<Option<mpsc::UnboundedSender<WriteJob>>>,
    pending_bytes: Arc<AtomicU64>,
    max_pending_bytes: u64,
    closed: AtomicBool,
    terminal: Arc<AtomicBool>,
    reader: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

fn spawn_transport(
    stream: UnixStream,
    max_pending_bytes: u64,
    handlers: Arc<dyn ByteTransportHandlers>,
) -> Arc<dyn ByteTransport> {
    let (mut read_half, mut write_half) = stream.into_split();
    let terminal = Arc::new(AtomicBool::new(false));
    let (sender, mut receiver) = mpsc::unbounded_channel::<WriteJob>();

    let read_terminal = Arc::clone(&terminal);
    let reader = tokio::spawn(async move {
        let mut buffer = vec![0u8; READ_CHUNK];
        loop {
            match read_half.read(&mut buffer).await {
                Ok(0) => {
                    if !read_terminal.swap(true, Ordering::SeqCst) {
                        handlers.on_close();
                    }
                    return;
                }
                Ok(count) => {
                    if read_terminal.load(Ordering::SeqCst) {
                        return;
                    }
                    handlers.on_data(&buffer[..count]);
                }
                Err(error) => {
                    if !read_terminal.swap(true, Ordering::SeqCst) {
                        handlers.on_error(TransportError::from(error));
                    }
                    return;
                }
            }
        }
    });

    tokio::spawn(async move {
        while let Some(job) = receiver.recv().await {
            let result = write_half
                .write_all(&job.bytes)
                .await
                .map_err(TransportError::from);
            let _ = job.done.send(result);
        }
        let _ = write_half.shutdown().await;
    });

    Arc::new(UnixByteTransport {
        queue: Mutex::new(Some(sender)),
        pending_bytes: Arc::new(AtomicU64::new(0)),
        max_pending_bytes,
        closed: AtomicBool::new(false),
        terminal,
        reader: Mutex::new(Some(reader)),
    })
}

impl ByteTransport for UnixByteTransport {
    fn send(&self, chunk: Vec<u8>) -> SendFuture {
        if self.closed.load(Ordering::SeqCst) {
            return Box::pin(async { Err(TransportError::new("Unix transport is closed")) });
        }
        let length = chunk.len() as u64;
        // Reserve eagerly so a caller that queues several writes before
        // awaiting any of them still sees the budget shrink.
        let reserved =
            self.pending_bytes
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |pending| {
                    (pending + length <= self.max_pending_bytes).then_some(pending + length)
                });
        if reserved.is_err() {
            return Box::pin(async {
                Err(TransportError::new(
                    "Unix transport exceeded its pending byte limit",
                ))
            });
        }

        let (done, wait) = oneshot::channel();
        let queued = self
            .queue
            .lock()
            .as_ref()
            .map(|sender| sender.send(WriteJob { bytes: chunk, done }))
            .transpose();
        if queued.is_err() || queued.as_ref().is_ok_and(Option::is_none) {
            self.pending_bytes.fetch_sub(length, Ordering::SeqCst);
            return Box::pin(async { Err(TransportError::new("Unix transport is closed")) });
        }

        let pending = Arc::clone(&self.pending_bytes);
        Box::pin(async move {
            let outcome = wait
                .await
                .unwrap_or_else(|_| Err(TransportError::new("Unix transport closed during write")));
            pending.fetch_sub(length, Ordering::SeqCst);
            outcome
        })
    }

    fn close(&self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        self.terminal.store(true, Ordering::SeqCst);
        // Dropping the queue sender stops the writer task, which shuts the
        // socket down; aborting the reader drops the read half.
        self.queue.lock().take();
        if let Some(reader) = self.reader.lock().take() {
            reader.abort();
        }
    }
}
