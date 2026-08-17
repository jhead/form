//! The credential store: `packages/ai/src/auth/credential-store.ts` (the
//! in-memory default) plus the on-disk `auth.json` store that the TypeScript
//! CLI actually ships (`packages/coding-agent/src/core/auth-storage.ts`).
//!
//! ## On-disk format
//!
//! `~/.pi/agent/auth.json` (overridable with `PI_CODING_AGENT_DIR`), mode 0600,
//! a JSON object keyed by `Provider.id`, pretty-printed with two-space indent
//! and no trailing newline — byte-identical to `JSON.stringify(data, null, 2)`.
//! A user may share a machine between the TypeScript and Rust implementations,
//! so this is a compatibility contract, not an implementation detail.
//!
//! ## Locking
//!
//! Writes take a `<path>.lock` *directory* lock, the same primitive
//! `proper-lockfile` uses upstream, so the two implementations exclude each
//! other cross-process. Within a process an async mutex serializes mutations,
//! which is what makes token refresh single-flight: the second caller runs its
//! mutation only after the first has committed, sees the rotated credential,
//! and returns without a second network round-trip.
//!
//! Unlike upstream, writes go through a temp file plus `rename`, so a crash or
//! a concurrent reader can never observe a half-written `auth.json`. That also
//! lets reads skip the lock entirely.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use async_trait::async_trait;
use parking_lot::Mutex;
use pi_core::options::AbortSignal;
use serde_json::{Map, Value};

use crate::config_value::resolve_config_value;
use crate::context::{expand_tilde, home_dir};
use crate::error::AuthError;
use crate::types::{AuthType, Credential, CredentialInfo};

/// A read-modify-write step run under the store lock.
#[async_trait]
pub trait CredentialMutation: Send + Sync {
    /// Return `Some(credential)` to persist it, `None` to leave the entry as is.
    async fn apply(&self, current: Option<Credential>) -> Result<Option<Credential>, AuthError>;
}

/// Unconditionally store `credential` (the login path).
pub fn set_credential(credential: Credential) -> Arc<dyn CredentialMutation> {
    struct SetCredential(Credential);

    #[async_trait]
    impl CredentialMutation for SetCredential {
        async fn apply(
            &self,
            _current: Option<Credential>,
        ) -> Result<Option<Credential>, AuthError> {
            Ok(Some(self.0.clone()))
        }
    }

    Arc::new(SetCredential(credential))
}

/// Adapt an async closure into a [`CredentialMutation`].
pub fn mutation_fn<F, Fut>(f: F) -> Arc<dyn CredentialMutation>
where
    F: Fn(Option<Credential>) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Option<Credential>, AuthError>> + Send + 'static,
{
    struct FnMutation<F>(F);

    #[async_trait]
    impl<F, Fut> CredentialMutation for FnMutation<F>
    where
        F: Fn(Option<Credential>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Option<Credential>, AuthError>> + Send + 'static,
    {
        async fn apply(
            &self,
            current: Option<Credential>,
        ) -> Result<Option<Credential>, AuthError> {
            (self.0)(current).await
        }
    }

    Arc::new(FnMutation(f))
}

/// App-owned credential storage, keyed by `Provider.id`, one credential per
/// provider. [`modify`](CredentialStore::modify) is the only write path.
#[async_trait]
pub trait CredentialStore: Send + Sync {
    /// Read the stored credential, possibly expired. `Ok(None)` for a missing
    /// entry; errors mean the storage itself failed.
    async fn read(
        &self,
        provider_id: &str,
        signal: Option<AbortSignal>,
    ) -> Result<Option<Credential>, AuthError>;

    /// List credential metadata without exposing secrets. Implementations must
    /// not execute configured API-key commands while listing.
    async fn list(&self, signal: Option<AbortSignal>) -> Result<Vec<CredentialInfo>, AuthError>;

    /// Serialized read-modify-write; the only write path. Resolves with the
    /// post-write credential.
    async fn modify(
        &self,
        provider_id: &str,
        mutation: Arc<dyn CredentialMutation>,
        signal: Option<AbortSignal>,
    ) -> Result<Option<Credential>, AuthError>;

    /// Remove a credential (logout). Serialized against `modify`.
    async fn delete(&self, provider_id: &str, signal: Option<AbortSignal>)
        -> Result<(), AuthError>;

    /// Convenience for the login path.
    async fn set(
        &self,
        provider_id: &str,
        credential: Credential,
    ) -> Result<Option<Credential>, AuthError> {
        self.modify(provider_id, set_credential(credential), None)
            .await
    }
}

fn check_aborted(signal: &Option<AbortSignal>) -> Result<(), AuthError> {
    match signal {
        Some(signal) if signal.is_aborted() => Err(AuthError::Cancelled),
        _ => Ok(()),
    }
}

/// Await `fut`, giving up as soon as `signal` fires.
async fn with_abort<T>(
    signal: &Option<AbortSignal>,
    fut: impl Future<Output = Result<T, AuthError>>,
) -> Result<T, AuthError> {
    match signal {
        Some(signal) if !signal.is_aborted() => {
            tokio::select! {
                biased;
                _ = signal.aborted() => Err(AuthError::Cancelled),
                result = fut => result,
            }
        }
        Some(_) => Err(AuthError::Cancelled),
        None => fut.await,
    }
}

// ---------------------------------------------------------------------------
// In-memory
// ---------------------------------------------------------------------------

/// Default store. Writes are serialized; nothing is persisted.
#[derive(Debug, Default)]
pub struct InMemoryCredentialStore {
    credentials: Mutex<Map<String, Value>>,
    write_lock: tokio::sync::Mutex<()>,
}

impl InMemoryCredentialStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed the store, as upstream's `AuthStorage.inMemory(data)` does.
    pub fn with_credentials(entries: impl IntoIterator<Item = (String, Credential)>) -> Self {
        let mut map = Map::new();
        for (provider_id, credential) in entries {
            map.insert(
                provider_id,
                serde_json::to_value(&credential).expect("credential serializes"),
            );
        }
        Self {
            credentials: Mutex::new(map),
            write_lock: tokio::sync::Mutex::new(()),
        }
    }

    /// The raw `auth.json`-shaped snapshot, for assertions and diagnostics.
    pub fn snapshot(&self) -> Map<String, Value> {
        self.credentials.lock().clone()
    }
}

#[async_trait]
impl CredentialStore for InMemoryCredentialStore {
    async fn read(
        &self,
        provider_id: &str,
        signal: Option<AbortSignal>,
    ) -> Result<Option<Credential>, AuthError> {
        check_aborted(&signal)?;
        let raw = self.credentials.lock().get(provider_id).cloned();
        raw.map(|value| decode_credential(provider_id, &value))
            .transpose()
    }

    async fn list(&self, signal: Option<AbortSignal>) -> Result<Vec<CredentialInfo>, AuthError> {
        check_aborted(&signal)?;
        Ok(collect_info(&self.credentials.lock()))
    }

    async fn modify(
        &self,
        provider_id: &str,
        mutation: Arc<dyn CredentialMutation>,
        signal: Option<AbortSignal>,
    ) -> Result<Option<Credential>, AuthError> {
        check_aborted(&signal)?;
        let guard = with_abort(&signal, async { Ok(self.write_lock.lock().await) }).await?;
        check_aborted(&signal)?;

        let current = self
            .credentials
            .lock()
            .get(provider_id)
            .cloned()
            .map(|value| decode_credential(provider_id, &value))
            .transpose()?;

        let next = with_abort(&signal, mutation.apply(current.clone())).await?;
        check_aborted(&signal)?;

        let result = match next {
            Some(next) => {
                self.credentials.lock().insert(
                    provider_id.to_string(),
                    serde_json::to_value(&next).expect("credential serializes"),
                );
                Some(next)
            }
            None => current,
        };
        drop(guard);
        Ok(result)
    }

    async fn delete(
        &self,
        provider_id: &str,
        signal: Option<AbortSignal>,
    ) -> Result<(), AuthError> {
        check_aborted(&signal)?;
        let guard = with_abort(&signal, async { Ok(self.write_lock.lock().await) }).await?;
        check_aborted(&signal)?;
        self.credentials.lock().shift_remove(provider_id);
        drop(guard);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// auth.json
// ---------------------------------------------------------------------------

/// `PI_CODING_AGENT_DIR` — upstream derives this from `APP_NAME` (`"pi"`).
pub const ENV_AGENT_DIR: &str = "PI_CODING_AGENT_DIR";
const CONFIG_DIR_NAME: &str = ".pi";
const AUTH_FILE_NAME: &str = "auth.json";
const LOCK_STALE: Duration = Duration::from_secs(30);
const LOCK_MAX_DELAY: Duration = Duration::from_millis(1_000);

/// `getAgentDir()`: `$PI_CODING_AGENT_DIR`, else `~/.pi/agent`.
pub fn default_agent_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os(ENV_AGENT_DIR) {
        let dir = dir.to_string_lossy().to_string();
        if !dir.is_empty() {
            return expand_tilde(&dir);
        }
    }
    home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(CONFIG_DIR_NAME)
        .join("agent")
}

/// `getAuthPath()`: `<agent dir>/auth.json`.
pub fn default_auth_path() -> PathBuf {
    default_agent_dir().join(AUTH_FILE_NAME)
}

/// Credential storage backed by `auth.json`.
pub struct FileCredentialStore {
    path: PathBuf,
    write_lock: tokio::sync::Mutex<()>,
    cache: Mutex<Option<(String, Map<String, Value>)>>,
}

impl std::fmt::Debug for FileCredentialStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileCredentialStore")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl FileCredentialStore {
    /// Open the store at the default location (`~/.pi/agent/auth.json`).
    pub fn open_default() -> Self {
        Self::open(default_auth_path())
    }

    pub fn open(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let path = expand_tilde(&path.to_string_lossy());
        Self {
            path,
            write_lock: tokio::sync::Mutex::new(()),
            cache: Mutex::new(None),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Read and parse `auth.json`, reusing the cached parse when the file has
    /// not changed. Reads take no lock: writes land atomically via `rename`.
    fn load(&self) -> Result<Map<String, Value>, AuthError> {
        let revision = file_revision(&self.path);
        if let (Some(revision), Some((cached_revision, data))) =
            (revision.as_ref(), self.cache.lock().as_ref())
        {
            if revision == cached_revision {
                return Ok(data.clone());
            }
        }

        let content = match std::fs::read_to_string(&self.path) {
            Ok(content) => content,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(err) => {
                return Err(AuthError::store(format!(
                    "failed to read {}: {err}",
                    self.path.display()
                )))
            }
        };

        let data = parse_auth_file(&content, &self.path)?;
        if let Some(revision) = revision {
            *self.cache.lock() = Some((revision, data.clone()));
        }
        Ok(data)
    }

    fn commit(&self, data: &Map<String, Value>) -> Result<(), AuthError> {
        let serialized =
            serde_json::to_string_pretty(data).map_err(|e| AuthError::store(e.to_string()))?;
        write_atomic(&self.path, &serialized)?;
        *self.cache.lock() = file_revision(&self.path).map(|r| (r, data.clone()));
        Ok(())
    }

    /// Run `f` with the cross-process lock held and the file freshly parsed.
    async fn with_lock<T>(
        &self,
        signal: &Option<AbortSignal>,
        f: impl FnOnce(&mut Map<String, Value>) -> Result<T, AuthError>,
    ) -> Result<T, AuthError> {
        check_aborted(signal)?;
        let guard = with_abort(signal, async { Ok(self.write_lock.lock().await) }).await?;
        check_aborted(signal)?;

        ensure_parent_dir(&self.path)?;
        let lock = with_abort(signal, DirectoryLock::acquire(self.path.clone())).await?;
        check_aborted(signal)?;

        let mut data = self.load()?;
        let result = f(&mut data);
        drop(lock);
        drop(guard);
        result
    }
}

#[async_trait]
impl CredentialStore for FileCredentialStore {
    async fn read(
        &self,
        provider_id: &str,
        signal: Option<AbortSignal>,
    ) -> Result<Option<Credential>, AuthError> {
        check_aborted(&signal)?;
        let raw = self.load()?.get(provider_id).cloned();
        check_aborted(&signal)?;
        let Some(raw) = raw else { return Ok(None) };
        let credential = decode_credential(provider_id, &raw)?;
        Ok(Some(resolve_stored_key(credential)))
    }

    async fn list(&self, signal: Option<AbortSignal>) -> Result<Vec<CredentialInfo>, AuthError> {
        check_aborted(&signal)?;
        let data = self.load()?;
        check_aborted(&signal)?;
        Ok(collect_info(&data))
    }

    async fn modify(
        &self,
        provider_id: &str,
        mutation: Arc<dyn CredentialMutation>,
        signal: Option<AbortSignal>,
    ) -> Result<Option<Credential>, AuthError> {
        check_aborted(&signal)?;
        let guard = with_abort(&signal, async { Ok(self.write_lock.lock().await) }).await?;
        check_aborted(&signal)?;

        ensure_parent_dir(&self.path)?;
        let lock = with_abort(&signal, DirectoryLock::acquire(self.path.clone())).await?;
        check_aborted(&signal)?;

        // Re-parse under the lock: another process may have rotated the token
        // while this caller was queued. A malformed file fails here, before any
        // write, so a hand-edited auth.json is never clobbered.
        let result = async {
            let mut data = self.load()?;
            let current = data
                .get(provider_id)
                .cloned()
                .map(|value| decode_credential(provider_id, &value))
                .transpose()?;

            let next = with_abort(&signal, mutation.apply(current.clone())).await?;
            check_aborted(&signal)?;

            match next {
                Some(next) => {
                    data.insert(
                        provider_id.to_string(),
                        serde_json::to_value(&next).expect("credential serializes"),
                    );
                    self.commit(&data)?;
                    Ok(Some(next))
                }
                None => Ok(current),
            }
        }
        .await;

        drop(lock);
        drop(guard);
        result
    }

    async fn delete(
        &self,
        provider_id: &str,
        signal: Option<AbortSignal>,
    ) -> Result<(), AuthError> {
        self.with_lock(&signal, |data| {
            data.shift_remove(provider_id);
            self.commit(data)
        })
        .await
    }
}

/// Read one credential out of an `auth.json` without constructing a store and
/// without resolving `$VAR` / `!command` indirections — the port of upstream's
/// `readStoredCredential`.
pub fn read_stored_credential(provider_id: &str, path: Option<&Path>) -> Option<Credential> {
    let path = path
        .map(Path::to_path_buf)
        .unwrap_or_else(default_auth_path);
    let content = std::fs::read_to_string(path).ok()?;
    let data: Map<String, Value> = serde_json::from_str(&content).ok()?;
    let raw = data.get(provider_id)?;
    serde_json::from_value(raw.clone()).ok()
}

fn parse_auth_file(content: &str, path: &Path) -> Result<Map<String, Value>, AuthError> {
    if content.trim().is_empty() {
        return Ok(Map::new());
    }
    match serde_json::from_str::<Value>(content) {
        Ok(Value::Object(map)) => Ok(map),
        Ok(_) => Err(AuthError::store(format!(
            "invalid {}: expected an object",
            path.display()
        ))),
        Err(err) => Err(AuthError::store(format!(
            "invalid {}: {err}",
            path.display()
        ))),
    }
}

fn decode_credential(provider_id: &str, value: &Value) -> Result<Credential, AuthError> {
    serde_json::from_value(value.clone()).map_err(|err| {
        AuthError::store(format!(
            "invalid credential for provider \"{provider_id}\": {err}"
        ))
    })
}

/// Malformed entries are skipped rather than failing the whole listing: status
/// UI should still show the providers that *are* readable.
fn collect_info(data: &Map<String, Value>) -> Vec<CredentialInfo> {
    data.iter()
        .filter_map(|(provider_id, value)| {
            let credential_type = match value.get("type").and_then(Value::as_str)? {
                "api_key" => AuthType::ApiKey,
                "oauth" => AuthType::OAuth,
                _ => return None,
            };
            Some(CredentialInfo {
                provider_id: provider_id.clone(),
                credential_type,
            })
        })
        .collect()
}

/// Expand a stored `$VAR` / `!command` key on read, as the TypeScript store
/// does, so a shared `auth.json` behaves the same in both implementations.
fn resolve_stored_key(credential: Credential) -> Credential {
    let Credential::ApiKey(mut api_key) = credential else {
        return credential;
    };
    if let Some(key) = api_key.key.as_ref() {
        api_key.key = resolve_config_value(key, Some(&api_key.env));
    }
    Credential::ApiKey(api_key)
}

fn ensure_parent_dir(path: &Path) -> Result<(), AuthError> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    if parent.as_os_str().is_empty() || parent.exists() {
        return Ok(());
    }
    std::fs::create_dir_all(parent)
        .map_err(|e| AuthError::store(format!("failed to create {}: {e}", parent.display())))?;
    set_mode(parent, 0o700);
    Ok(())
}

/// Write via temp file + `rename` so readers never see a partial file, and set
/// 0600 before the content lands rather than after.
fn write_atomic(path: &Path, content: &str) -> Result<(), AuthError> {
    use std::io::Write;

    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let temp = parent.join(format!(
        ".{}.{}.{}.tmp",
        path.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));

    let write = || -> std::io::Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp)?;
        file.write_all(content.as_bytes())?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temp, path)
    };

    match write() {
        Ok(()) => {
            set_mode(path, 0o600);
            Ok(())
        }
        Err(err) => {
            let _ = std::fs::remove_file(&temp);
            Err(AuthError::store(format!(
                "failed to write {}: {err}",
                path.display()
            )))
        }
    }
}

fn set_mode(path: &Path, mode: u32) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
    }
}

/// `dev:ino:size:mtime` identity, upstream's `getFileRevision`.
fn file_revision(path: &Path) -> Option<String> {
    let metadata = std::fs::metadata(path).ok()?;
    let mtime = metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some(format!(
            "{}:{}:{}:{mtime}",
            metadata.dev(),
            metadata.ino(),
            metadata.size()
        ))
    }
    #[cfg(not(unix))]
    {
        Some(format!("{}:{mtime}", metadata.len()))
    }
}

/// `<target>.lock` as a directory, the same primitive `proper-lockfile` uses,
/// so this store and the TypeScript one exclude each other across processes.
struct DirectoryLock {
    dir: PathBuf,
}

impl DirectoryLock {
    async fn acquire(target: PathBuf) -> Result<DirectoryLock, AuthError> {
        let dir = PathBuf::from(format!("{}.lock", target.display()));
        let deadline = Instant::now() + LOCK_STALE;
        let mut delay = Duration::from_millis(10);

        loop {
            match std::fs::create_dir(&dir) {
                Ok(()) => return Ok(DirectoryLock { dir }),
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                    if is_stale(&dir) {
                        // The holder died without releasing; proper-lockfile
                        // treats an unrefreshed lock as free after `stale`.
                        let _ = std::fs::remove_dir(&dir);
                        continue;
                    }
                    if Instant::now() >= deadline {
                        return Err(AuthError::store(format!(
                            "timed out waiting for the credential store lock at {}",
                            dir.display()
                        )));
                    }
                    tokio::time::sleep(delay).await;
                    delay = (delay * 2).min(LOCK_MAX_DELAY);
                }
                Err(err) => {
                    return Err(AuthError::store(format!(
                        "failed to lock {}: {err}",
                        dir.display()
                    )))
                }
            }
        }
    }
}

fn is_stale(dir: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(dir) else {
        return false;
    };
    let Ok(modified) = metadata.modified() else {
        return false;
    };
    SystemTime::now()
        .duration_since(modified)
        .map(|age| age > LOCK_STALE)
        .unwrap_or(false)
}

impl Drop for DirectoryLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir(&self.dir);
    }
}
