//! Port of `packages/ai/src/auth/context.ts` plus `utils/provider-env.ts`.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use async_trait::async_trait;
use pi_core::options::ProviderEnv;

/// Environment access for auth resolution. Injectable for tests.
#[async_trait]
pub trait AuthContext: Send + Sync {
    /// Read an environment variable. Blank values read as absent, matching
    /// upstream's `value.trim().length > 0` guard.
    async fn env(&self, name: &str) -> Option<String>;

    /// Whether a file exists. Supports a leading `~`.
    async fn file_exists(&self, path: &str) -> bool;
}

/// Process environment plus the real filesystem.
#[derive(Debug, Clone, Copy, Default)]
pub struct DefaultAuthContext;

impl DefaultAuthContext {
    pub fn shared() -> Arc<dyn AuthContext> {
        Arc::new(DefaultAuthContext)
    }
}

#[async_trait]
impl AuthContext for DefaultAuthContext {
    async fn env(&self, name: &str) -> Option<String> {
        non_blank(std::env::var(name).ok())
    }

    async fn file_exists(&self, path: &str) -> bool {
        tokio::fs::metadata(expand_tilde(path)).await.is_ok()
    }
}

/// Fixed environment for tests and for hosts that carry their own env map.
#[derive(Debug, Clone, Default)]
pub struct MapAuthContext {
    vars: BTreeMap<String, String>,
    files: BTreeSet<String>,
}

impl MapAuthContext {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_var(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.vars.insert(name.into(), value.into());
        self
    }

    pub fn with_file(mut self, path: impl Into<String>) -> Self {
        self.files.insert(path.into());
        self
    }

    pub fn shared(self) -> Arc<dyn AuthContext> {
        Arc::new(self)
    }
}

#[async_trait]
impl AuthContext for MapAuthContext {
    async fn env(&self, name: &str) -> Option<String> {
        non_blank(self.vars.get(name).cloned())
    }

    async fn file_exists(&self, path: &str) -> bool {
        self.files.contains(path)
    }
}

/// `overlayEnvAuthContext`: provider-scoped overrides win over the base context.
pub fn overlay_env(base: Arc<dyn AuthContext>, env: ProviderEnv) -> Arc<dyn AuthContext> {
    Arc::new(OverlayAuthContext { base, env })
}

struct OverlayAuthContext {
    base: Arc<dyn AuthContext>,
    env: ProviderEnv,
}

#[async_trait]
impl AuthContext for OverlayAuthContext {
    async fn env(&self, name: &str) -> Option<String> {
        match non_blank(self.env.get(name).cloned()) {
            Some(value) => Some(value),
            None => self.base.env(name).await,
        }
    }

    async fn file_exists(&self, path: &str) -> bool {
        self.base.file_exists(path).await
    }
}

fn non_blank(value: Option<String>) -> Option<String> {
    value.filter(|v| !v.trim().is_empty())
}

/// `~` / `~/x` expansion against `$HOME` (`%USERPROFILE%` on Windows).
pub(crate) fn expand_tilde(path: &str) -> std::path::PathBuf {
    let Some(rest) = path.strip_prefix('~') else {
        return std::path::PathBuf::from(path);
    };
    let Some(home) = home_dir() else {
        return std::path::PathBuf::from(path);
    };
    if rest.is_empty() {
        return home;
    }
    let rest = rest.trim_start_matches(['/', '\\']);
    if rest.is_empty() {
        home
    } else {
        home.join(rest)
    }
}

pub(crate) fn home_dir() -> Option<std::path::PathBuf> {
    #[cfg(windows)]
    let key = "USERPROFILE";
    #[cfg(not(windows))]
    let key = "HOME";
    std::env::var_os(key).map(std::path::PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn blank_environment_values_read_as_absent() {
        let ctx = MapAuthContext::new()
            .with_var("BLANK", "   ")
            .with_var("SET", "v");
        assert_eq!(ctx.env("BLANK").await, None);
        assert_eq!(ctx.env("SET").await.as_deref(), Some("v"));
    }

    #[tokio::test]
    async fn provider_env_overlay_wins_over_the_base_context() {
        let base = MapAuthContext::new()
            .with_var("A", "base-a")
            .with_var("B", "base-b")
            .shared();
        let mut overrides = ProviderEnv::new();
        overrides.insert("A".into(), "override-a".into());
        let ctx = overlay_env(base, overrides);

        assert_eq!(ctx.env("A").await.as_deref(), Some("override-a"));
        assert_eq!(ctx.env("B").await.as_deref(), Some("base-b"));
    }
}
