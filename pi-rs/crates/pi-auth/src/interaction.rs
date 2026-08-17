//! The one place where a login flow is allowed to talk to a human.
//!
//! This crate never reads stdin, never writes stdout and never launches a
//! browser: the Swift host owns the UI, so every interactive step goes through
//! `Arc<dyn AuthInteraction>`. Upstream models this as one `prompt(AuthPrompt)`
//! plus `notify(AuthEvent)`; the port splits the prompt by kind so a Swift
//! implementation gets typed callbacks instead of a tagged union it has to
//! re-dispatch, and routes the two events a host must *act* on (open a browser,
//! display a device code) to dedicated methods.

use std::sync::Arc;

use async_trait::async_trait;
use pi_core::options::AbortSignal;

use crate::error::AuthError;
use crate::types::{AuthEvent, DeviceCode, SelectPrompt, TextPrompt};

/// Host-provided login UI. Every method has a headless default that either
/// refuses (prompts) or does nothing (notifications), so implementors only
/// override what their UI actually supports.
#[async_trait]
pub trait AuthInteraction: Send + Sync {
    /// Free-text entry, e.g. a GitHub Enterprise domain.
    async fn prompt_text(&self, prompt: TextPrompt) -> Result<String, AuthError> {
        Err(unsupported("text", &prompt.message))
    }

    /// Masked entry for an API key.
    async fn prompt_secret(&self, prompt: TextPrompt) -> Result<String, AuthError> {
        Err(unsupported("secret", &prompt.message))
    }

    /// Choose one of `prompt.options`; returns the chosen option's `id`.
    async fn prompt_select(&self, prompt: SelectPrompt) -> Result<String, AuthError> {
        Err(unsupported("select", &prompt.message))
    }

    /// Paste-back of an authorization code or the full redirect URL, for hosts
    /// that cannot receive the browser redirect themselves.
    ///
    /// Flows that also run a loopback callback server race this against the
    /// callback, so an implementation may block until the user answers; the
    /// flow drops the future when the callback wins.
    async fn prompt_manual_code(&self, prompt: TextPrompt) -> Result<String, AuthError> {
        Err(unsupported("manual_code", &prompt.message))
    }

    /// Show (and optionally open) the provider's authorization URL.
    fn open_url(&self, url: &str, instructions: Option<&str>) {
        let _ = (url, instructions);
    }

    /// Display a device-authorization code and its verification URI.
    fn show_device_code(&self, device_code: &DeviceCode) {
        let _ = device_code;
    }

    /// Informational progress. Never required to make a flow succeed.
    fn notify(&self, event: &AuthEvent) {
        let _ = event;
    }
}

fn unsupported(kind: &str, message: &str) -> AuthError {
    AuthError::interaction(format!(
        "no AuthInteraction available for a {kind} prompt: {message}"
    ))
}

/// Default interaction for headless use: notifications are dropped and every
/// prompt fails with [`AuthError::Interaction`] instead of blocking.
#[derive(Debug, Clone, Copy, Default)]
pub struct HeadlessAuthInteraction;

impl AuthInteraction for HeadlessAuthInteraction {}

impl HeadlessAuthInteraction {
    pub fn shared() -> Arc<dyn AuthInteraction> {
        Arc::new(HeadlessAuthInteraction)
    }
}

/// Everything a provider login needs: the host UI, a cancellation signal, and
/// whether this process may bind a loopback OAuth callback server.
#[derive(Clone)]
pub struct LoginContext {
    pub interaction: Arc<dyn AuthInteraction>,
    pub signal: AbortSignal,
    /// Opt-in. Off by default because the Swift host normally owns the browser
    /// round-trip and hands the redirect back through `prompt_manual_code`.
    pub local_callback_server: bool,
}

impl LoginContext {
    pub fn new(interaction: Arc<dyn AuthInteraction>) -> Self {
        Self {
            interaction,
            signal: AbortSignal::never(),
            local_callback_server: false,
        }
    }

    /// Headless: no prompts can be served, so only flows that need nothing
    /// interactive (or a callback server) can complete.
    pub fn headless() -> Self {
        Self::new(HeadlessAuthInteraction::shared())
    }

    pub fn with_signal(mut self, signal: AbortSignal) -> Self {
        self.signal = signal;
        self
    }

    /// Allow this process to bind `127.0.0.1` and receive the OAuth redirect.
    pub fn with_local_callback_server(mut self, enabled: bool) -> Self {
        self.local_callback_server = enabled;
        self
    }

    pub fn throw_if_aborted(&self) -> Result<(), AuthError> {
        if self.signal.is_aborted() {
            return Err(AuthError::Cancelled);
        }
        Ok(())
    }

    /// Dispatch an upstream `AuthEvent` to the right host callback.
    pub fn emit(&self, event: AuthEvent) {
        match &event {
            AuthEvent::AuthUrl { url, instructions } => {
                self.interaction.open_url(url, instructions.as_deref());
            }
            AuthEvent::DeviceCode(device_code) => {
                self.interaction.show_device_code(device_code);
            }
            _ => {}
        }
        self.interaction.notify(&event);
    }

    pub(crate) fn progress(&self, message: impl Into<String>) {
        self.emit(AuthEvent::Progress {
            message: message.into(),
        });
    }
}

impl std::fmt::Debug for LoginContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoginContext")
            .field("aborted", &self.signal.is_aborted())
            .field("local_callback_server", &self.local_callback_server)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn headless_interaction_refuses_prompts_instead_of_blocking() {
        let interaction = HeadlessAuthInteraction;
        let error = interaction
            .prompt_secret(TextPrompt::new("Enter Anthropic API key"))
            .await
            .expect_err("headless prompt must fail");
        assert_eq!(error.code(), "interaction");

        // Notifications are dropped, not errors.
        interaction.open_url("https://example.test", None);
        interaction.notify(&AuthEvent::Progress {
            message: "working".into(),
        });
    }
}
