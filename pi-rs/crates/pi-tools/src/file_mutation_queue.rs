//! Serialization of concurrent file mutations.
//!
//! Port of `.upstream/packages/agent/src/harness/tools/file-mutation-queue.ts`.
//!
//! Two tool calls that target the same file through different spellings (a
//! relative path and a symlink to it, say) must not interleave their
//! read-modify-write. Upstream keys a promise chain by
//! `(WeakMap<ExecutionEnv>, canonicalPath)`. Rust has no weak map keyed by a
//! trait object, so the registry is keyed by the environment's `Arc` address
//! plus the canonical path, and entries are dropped once no call holds them —
//! which also means an address can only be reused after every queue for the old
//! environment is gone.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Weak};

use once_cell::sync::Lazy;
use parking_lot::Mutex;

use crate::error::{FileErrorCode, ToolError, ToolResultOf};
use crate::types::ExecutionEnvRef;

type QueueKey = (usize, String);
type Queue = Arc<tokio::sync::Mutex<()>>;

static QUEUES: Lazy<Mutex<HashMap<QueueKey, Weak<tokio::sync::Mutex<()>>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// Canonical path when the file exists, absolute path when it does not yet.
async fn mutation_queue_key(env: &ExecutionEnvRef, path: &str) -> ToolResultOf<String> {
    let absolute = env.absolute_path(path, None).await?;
    match env.canonical_path(&absolute, None).await {
        Ok(canonical) => Ok(canonical),
        Err(error)
            if error.code == FileErrorCode::NotFound
                || error.code == FileErrorCode::NotSupported =>
        {
            Ok(absolute)
        }
        Err(error) => Err(ToolError::File(error)),
    }
}

fn acquire_queue(key: QueueKey) -> Queue {
    let mut queues = QUEUES.lock();
    if let Some(existing) = queues.get(&key).and_then(Weak::upgrade) {
        return existing;
    }
    let queue: Queue = Arc::new(tokio::sync::Mutex::new(()));
    queues.insert(key, Arc::downgrade(&queue));
    queue
}

fn release_queue(key: &QueueKey) {
    let mut queues = QUEUES.lock();
    if queues.get(key).is_some_and(|weak| weak.strong_count() == 0) {
        queues.remove(key);
    }
}

/// Serialize file mutations targeting the same environment and canonical path.
///
/// The lock is held until `f` settles, including when it fails or is aborted, so
/// a cancelled write cannot let the next one observe a half-written file.
pub async fn with_file_mutation_queue<F, Fut, T>(
    env: &ExecutionEnvRef,
    path: &str,
    f: F,
) -> ToolResultOf<T>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = ToolResultOf<T>>,
{
    let key: QueueKey = (Arc::as_ptr(env) as *const () as usize, {
        mutation_queue_key(env, path).await?
    });
    let queue = acquire_queue(key.clone());
    let result = {
        let _guard = queue.lock().await;
        f().await
    };
    drop(queue);
    release_queue(&key);
    result
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::memory::MemoryExecutionEnv;

    #[tokio::test]
    async fn serializes_mutations_on_the_same_canonical_path() {
        let env: ExecutionEnvRef = Arc::new(MemoryExecutionEnv::new("/work"));
        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));

        let mut handles = Vec::new();
        for _ in 0..8 {
            let env = env.clone();
            let active = active.clone();
            let max_active = max_active.clone();
            handles.push(tokio::spawn(async move {
                with_file_mutation_queue(&env, "file.txt", || async {
                    let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                    max_active.fetch_max(now, Ordering::SeqCst);
                    tokio::task::yield_now().await;
                    active.fetch_sub(1, Ordering::SeqCst);
                    Ok::<_, ToolError>(())
                })
                .await
                .unwrap();
            }));
        }
        for handle in handles {
            handle.await.unwrap();
        }

        assert_eq!(max_active.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn different_paths_do_not_block_each_other() {
        let env: ExecutionEnvRef = Arc::new(MemoryExecutionEnv::new("/work"));
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();

        let blocker = {
            let env = env.clone();
            tokio::spawn(async move {
                with_file_mutation_queue(&env, "a.txt", || async {
                    rx.await.ok();
                    Ok::<_, ToolError>(())
                })
                .await
                .unwrap();
            })
        };
        // Give the blocker time to take its lock.
        tokio::task::yield_now().await;

        with_file_mutation_queue(&env, "b.txt", || async { Ok::<_, ToolError>(()) })
            .await
            .unwrap();
        tx.send(()).ok();
        blocker.await.unwrap();
    }

    #[tokio::test]
    async fn releases_the_queue_entry_when_no_call_holds_it() {
        let env: ExecutionEnvRef = Arc::new(MemoryExecutionEnv::new("/work"));
        with_file_mutation_queue(&env, "gone.txt", || async { Ok::<_, ToolError>(()) })
            .await
            .unwrap();
        let key = (
            Arc::as_ptr(&env) as *const () as usize,
            "/work/gone.txt".to_string(),
        );
        assert!(!QUEUES.lock().contains_key(&key));
    }
}
