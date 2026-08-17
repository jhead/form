//! Port of the meaningful cases in
//! `.upstream/packages/coding-agent/test/auth-storage.test.ts`, plus the
//! on-disk-format assertions that make a shared `auth.json` safe.

use std::sync::Arc;

use pi_auth::{
    mutation_fn, set_credential, ApiKeyCredential, AuthType, Credential, CredentialInfo,
    CredentialStore, FileCredentialStore, InMemoryCredentialStore, OAuthCredential,
};
use pi_core::options::AbortHandle;
use serde_json::json;
use tempfile::TempDir;

fn temp_store() -> (TempDir, FileCredentialStore) {
    let dir = TempDir::new().expect("temp dir");
    let store = FileCredentialStore::open(dir.path().join("auth.json"));
    (dir, store)
}

fn write_auth_json(store: &FileCredentialStore, value: serde_json::Value) {
    std::fs::write(store.path(), serde_json::to_string(&value).unwrap()).unwrap();
}

fn read_auth_json(store: &FileCredentialStore) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(store.path()).unwrap()).unwrap()
}

fn api_key(key: &str) -> Credential {
    Credential::ApiKey(ApiKeyCredential::new(key))
}

#[tokio::test]
async fn reads_a_stored_api_key_credential() {
    let (_dir, store) = temp_store();
    write_auth_json(
        &store,
        json!({ "anthropic": { "type": "api_key", "key": "sk-stored" } }),
    );

    assert_eq!(
        store.read("anthropic", None).await.unwrap(),
        Some(api_key("sk-stored"))
    );
    assert_eq!(store.read("openai", None).await.unwrap(), None);
}

#[tokio::test]
async fn a_missing_file_reads_as_empty_rather_than_failing() {
    let (_dir, store) = temp_store();
    assert_eq!(store.read("anthropic", None).await.unwrap(), None);
    assert!(store.list(None).await.unwrap().is_empty());
    assert!(!store.path().exists(), "reading must not create the file");
}

#[tokio::test]
async fn resolves_environment_backed_api_key_credentials() {
    let (_dir, store) = temp_store();
    write_auth_json(
        &store,
        json!({ "anthropic": { "type": "api_key", "key": "$SCOPED_KEY",
                               "env": { "SCOPED_KEY": "scoped-value", "REGION": "test-region" } } }),
    );

    let credential = store.read("anthropic", None).await.unwrap().unwrap();
    let api_key = credential.as_api_key().expect("api key credential");
    // The credential-scoped env resolves the key and stays inspectable.
    assert_eq!(api_key.key.as_deref(), Some("scoped-value"));
    assert_eq!(
        api_key.env.get("REGION").map(String::as_str),
        Some("test-region")
    );
}

#[cfg(unix)]
#[tokio::test]
async fn resolves_command_backed_api_key_credentials() {
    let (_dir, store) = temp_store();
    write_auth_json(
        &store,
        json!({ "anthropic": { "type": "api_key", "key": "!printf 'command-key'" } }),
    );

    assert_eq!(
        store.read("anthropic", None).await.unwrap(),
        Some(api_key("command-key"))
    );
}

#[tokio::test]
async fn returns_oauth_credentials_unchanged() {
    let credential = OAuthCredential::new("access-token", "refresh-token", 1_234_567)
        .with_extra("enterpriseUrl", json!("company.ghe.com"));
    let store = InMemoryCredentialStore::with_credentials([(
        "github-copilot".to_string(),
        Credential::OAuth(credential.clone()),
    )]);

    assert_eq!(
        store.read("github-copilot", None).await.unwrap(),
        Some(Credential::OAuth(credential))
    );
}

#[tokio::test]
async fn modify_persists_while_preserving_unrelated_external_edits() {
    let (_dir, store) = temp_store();
    write_auth_json(
        &store,
        json!({ "anthropic": { "type": "api_key", "key": "old" } }),
    );
    // Prime the read cache, then let another process add an entry.
    store.read("anthropic", None).await.unwrap();
    write_auth_json(
        &store,
        json!({
            "anthropic": { "type": "api_key", "key": "old" },
            "openai": { "type": "api_key", "key": "external" },
        }),
    );

    store.set("anthropic", api_key("new")).await.unwrap();

    assert_eq!(
        read_auth_json(&store),
        json!({
            "anthropic": { "type": "api_key", "key": "new" },
            "openai": { "type": "api_key", "key": "external" },
        })
    );
}

#[tokio::test]
async fn modify_returning_nothing_leaves_the_credential_unchanged() {
    let (_dir, store) = temp_store();
    write_auth_json(
        &store,
        json!({ "anthropic": { "type": "api_key", "key": "stored" } }),
    );

    let result = store
        .modify(
            "anthropic",
            mutation_fn(|_current| async { Ok(None) }),
            None,
        )
        .await
        .unwrap();

    assert_eq!(result, Some(api_key("stored")));
    assert_eq!(
        store.read("anthropic", None).await.unwrap(),
        Some(api_key("stored"))
    );
}

#[tokio::test]
async fn the_mutation_sees_the_current_credential() {
    let (_dir, store) = temp_store();
    write_auth_json(
        &store,
        json!({ "anthropic": { "type": "api_key", "key": "stored" } }),
    );

    let result = store
        .modify(
            "anthropic",
            mutation_fn(|current| async move {
                let previous = current
                    .and_then(|c| c.as_api_key().and_then(|k| k.key.clone()))
                    .unwrap_or_default();
                Ok(Some(Credential::ApiKey(ApiKeyCredential::new(format!(
                    "{previous}+next"
                )))))
            }),
            None,
        )
        .await
        .unwrap();

    assert_eq!(result, Some(api_key("stored+next")));
}

#[tokio::test]
async fn serializes_concurrent_modifications_across_store_instances() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("auth.json");
    let first: Arc<dyn CredentialStore> = Arc::new(FileCredentialStore::open(&path));
    let second: Arc<dyn CredentialStore> = Arc::new(FileCredentialStore::open(&path));

    let (a, b) = tokio::join!(
        first.set("anthropic", api_key("anthropic-key")),
        second.set("openai", api_key("openai-key")),
    );
    a.unwrap();
    b.unwrap();

    let store = FileCredentialStore::open(&path);
    assert_eq!(
        read_auth_json(&store),
        json!({
            "anthropic": { "type": "api_key", "key": "anthropic-key" },
            "openai": { "type": "api_key", "key": "openai-key" },
        })
    );
}

#[tokio::test]
async fn delete_removes_one_credential_while_preserving_others() {
    let (_dir, store) = temp_store();
    write_auth_json(
        &store,
        json!({
            "anthropic": { "type": "api_key", "key": "anthropic-key" },
            "openai": { "type": "api_key", "key": "openai-key" },
            "google": { "type": "api_key", "key": "external-key" },
        }),
    );

    store.delete("anthropic", None).await.unwrap();

    // Listing preserves the file's insertion order, as the JSON object does.
    assert_eq!(
        store.list(None).await.unwrap(),
        vec![
            CredentialInfo {
                provider_id: "openai".into(),
                credential_type: AuthType::ApiKey,
            },
            CredentialInfo {
                provider_id: "google".into(),
                credential_type: AuthType::ApiKey,
            },
        ]
    );
    assert_eq!(store.read("anthropic", None).await.unwrap(), None);
    assert_eq!(
        store.read("openai", None).await.unwrap(),
        Some(api_key("openai-key"))
    );
}

#[tokio::test]
async fn does_not_overwrite_a_malformed_auth_file() {
    let (_dir, store) = temp_store();
    write_auth_json(
        &store,
        json!({ "anthropic": { "type": "api_key", "key": "stored" } }),
    );
    store.read("anthropic", None).await.unwrap();
    std::fs::write(store.path(), "{invalid-json").unwrap();

    let error = store.set("openai", api_key("new")).await.unwrap_err();
    assert_eq!(error.code(), "store");
    assert_eq!(
        std::fs::read_to_string(store.path()).unwrap(),
        "{invalid-json",
        "a hand-edited file must survive a failed write"
    );
}

#[tokio::test]
async fn a_pre_aborted_operation_neither_creates_the_file_nor_runs_the_mutation() {
    let (_dir, store) = temp_store();
    let (handle, signal) = AbortHandle::new();
    handle.abort();

    let error = store
        .modify(
            "openai",
            mutation_fn(|_| async { panic!("mutation must not run") }),
            Some(signal),
        )
        .await
        .unwrap_err();

    assert!(error.is_cancelled());
    assert!(!store.path().exists());
}

#[tokio::test]
async fn the_file_is_written_with_owner_only_permissions() {
    let (_dir, store) = temp_store();
    store.set("anthropic", api_key("sk-test")).await.unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(store.path())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "auth.json must not be group/world readable"
        );
    }
}

#[tokio::test]
async fn the_parent_directory_is_created_on_demand_with_owner_only_permissions() {
    let dir = TempDir::new().unwrap();
    let agent_dir = dir.path().join("nested").join("agent");
    let path = agent_dir.join("auth.json");
    let store = FileCredentialStore::open(&path);

    store.set("anthropic", api_key("sk-test")).await.unwrap();
    assert!(path.exists());

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&agent_dir).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o077,
            0,
            "a created agent dir must not be group/world accessible"
        );
    }
}

/// The exact bytes matter: the TypeScript CLI writes
/// `JSON.stringify(data, null, 2)` with no trailing newline, and may read this
/// file on the same machine.
#[tokio::test]
async fn the_serialized_format_matches_the_typescript_writer() {
    let (_dir, store) = temp_store();
    store
        .set(
            "anthropic",
            Credential::OAuth(OAuthCredential::new("a-token", "r-token", 1700000000000)),
        )
        .await
        .unwrap();

    assert_eq!(
        std::fs::read_to_string(store.path()).unwrap(),
        "{\n  \"anthropic\": {\n    \"type\": \"oauth\",\n    \"refresh\": \"r-token\",\n    \"access\": \"a-token\",\n    \"expires\": 1700000000000\n  }\n}"
    );
}

/// Entries this crate does not model must survive a write, or a Rust login
/// would silently drop a provider the TypeScript side still uses.
#[tokio::test]
async fn unknown_entries_are_preserved_across_writes() {
    let (_dir, store) = temp_store();
    write_auth_json(
        &store,
        json!({
            "future-provider": { "type": "device_bound", "handle": "opaque" },
            "anthropic": { "type": "api_key", "key": "old" },
        }),
    );

    store.set("anthropic", api_key("new")).await.unwrap();

    let written = read_auth_json(&store);
    assert_eq!(
        written.get("future-provider"),
        Some(&json!({ "type": "device_bound", "handle": "opaque" }))
    );
    // …and an unreadable entry does not break listing the readable ones.
    assert_eq!(
        store.list(None).await.unwrap(),
        vec![CredentialInfo {
            provider_id: "anthropic".into(),
            credential_type: AuthType::ApiKey,
        }]
    );
}

#[tokio::test]
async fn a_second_instance_sees_writes_made_by_the_first() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("auth.json");
    let first = FileCredentialStore::open(&path);
    let second = FileCredentialStore::open(&path);

    // Prime the second instance's cache before the write lands.
    assert_eq!(second.read("anthropic", None).await.unwrap(), None);
    first.set("anthropic", api_key("written")).await.unwrap();

    assert_eq!(
        second.read("anthropic", None).await.unwrap(),
        Some(api_key("written"))
    );
}

#[tokio::test]
async fn in_memory_storage_implements_the_same_behavior() {
    let store =
        InMemoryCredentialStore::with_credentials([("anthropic".to_string(), api_key("initial"))]);

    assert_eq!(
        store.read("anthropic", None).await.unwrap(),
        Some(api_key("initial"))
    );
    store
        .modify("anthropic", set_credential(api_key("updated")), None)
        .await
        .unwrap();
    assert_eq!(
        store.read("anthropic", None).await.unwrap(),
        Some(api_key("updated"))
    );
    store.delete("anthropic", None).await.unwrap();
    assert!(store.list(None).await.unwrap().is_empty());
}

#[tokio::test]
async fn in_memory_mutations_are_serialized() {
    let store = Arc::new(InMemoryCredentialStore::new());
    let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));

    let tasks: Vec<_> = (0..8)
        .map(|_| {
            let store = store.clone();
            let counter = counter.clone();
            tokio::spawn(async move {
                store
                    .modify(
                        "provider",
                        mutation_fn(move |current| {
                            let counter = counter.clone();
                            async move {
                                // Two overlapping mutations would both observe
                                // the same starting value and lose an update.
                                let seen = current
                                    .and_then(|c| c.as_api_key().and_then(|k| k.key.clone()))
                                    .unwrap_or_else(|| "0".into())
                                    .parse::<usize>()
                                    .unwrap();
                                tokio::task::yield_now().await;
                                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                                Ok(Some(Credential::ApiKey(ApiKeyCredential::new(
                                    (seen + 1).to_string(),
                                ))))
                            }
                        }),
                        None,
                    )
                    .await
                    .unwrap();
            })
        })
        .collect();

    for task in tasks {
        task.await.unwrap();
    }

    assert_eq!(counter.load(std::sync::atomic::Ordering::SeqCst), 8);
    assert_eq!(
        store.read("provider", None).await.unwrap(),
        Some(api_key("8"))
    );
}
