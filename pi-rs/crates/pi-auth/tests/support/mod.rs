// Each integration test binary compiles this module separately, so any helper
// a given file does not use reads as dead code there.
#![allow(dead_code)]

//! Test harness shared by the OAuth flow tests.
//!
//! Stands in for the Swift host: canned prompt answers plus a record of every
//! event a real UI would render. No flow under test may reach a real provider —
//! every endpoint is a `wiremock` mock.

use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use pi_auth::{
    AuthError, AuthEvent, AuthInteraction, DeviceCode, LoginContext, SelectPrompt, TextPrompt,
};

/// Computes a prompt answer from what the flow has emitted so far, so a test
/// can echo back the `state` the flow just generated.
pub type Answer = Arc<dyn Fn(&Recorded) -> Result<String, AuthError> + Send + Sync>;

#[derive(Debug, Default, Clone)]
pub struct Recorded {
    pub auth_urls: Vec<String>,
    pub device_codes: Vec<DeviceCode>,
    pub events: Vec<AuthEvent>,
    pub prompts: Vec<String>,
}

impl Recorded {
    /// The most recent authorization URL passed to `open_url`.
    pub fn auth_url(&self) -> &str {
        self.auth_urls.last().expect("an auth url was emitted")
    }

    pub fn query_param(&self, name: &str) -> Option<String> {
        url::Url::parse(self.auth_url())
            .ok()?
            .query_pairs()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.to_string())
    }
}

#[derive(Default)]
pub struct TestInteraction {
    manual: Option<Answer>,
    /// Model a host UI still waiting on the user: the prompt never resolves.
    manual_never_answers: bool,
    text: Option<String>,
    secret: Option<String>,
    select: Option<String>,
    recorded: Mutex<Recorded>,
}

impl TestInteraction {
    pub fn new() -> Self {
        Self::default()
    }

    /// Answer `prompt_manual_code` from what has been emitted so far.
    pub fn with_manual_answer(
        mut self,
        answer: impl Fn(&Recorded) -> Result<String, AuthError> + Send + Sync + 'static,
    ) -> Self {
        self.manual = Some(Arc::new(answer));
        self
    }

    /// Leave `prompt_manual_code` pending forever, so only an out-of-band
    /// event (a loopback callback) can finish the login.
    pub fn with_manual_never_answered(mut self) -> Self {
        self.manual_never_answers = true;
        self
    }

    pub fn with_text(mut self, value: impl Into<String>) -> Self {
        self.text = Some(value.into());
        self
    }

    pub fn with_secret(mut self, value: impl Into<String>) -> Self {
        self.secret = Some(value.into());
        self
    }

    pub fn with_select(mut self, option_id: impl Into<String>) -> Self {
        self.select = Some(option_id.into());
        self
    }

    pub fn recorded(&self) -> Recorded {
        self.recorded.lock().clone()
    }

    /// Wrap in a [`LoginContext`], returning the handle for later assertions.
    pub fn into_context(self) -> (Arc<TestInteraction>, LoginContext) {
        let interaction = Arc::new(self);
        let ctx = LoginContext::new(interaction.clone());
        (interaction, ctx)
    }
}

#[async_trait]
impl AuthInteraction for TestInteraction {
    async fn prompt_text(&self, prompt: TextPrompt) -> Result<String, AuthError> {
        self.recorded
            .lock()
            .prompts
            .push(format!("text: {}", prompt.message));
        self.text
            .clone()
            .ok_or_else(|| AuthError::interaction("unexpected text prompt"))
    }

    async fn prompt_secret(&self, prompt: TextPrompt) -> Result<String, AuthError> {
        self.recorded
            .lock()
            .prompts
            .push(format!("secret: {}", prompt.message));
        self.secret
            .clone()
            .ok_or_else(|| AuthError::interaction("unexpected secret prompt"))
    }

    async fn prompt_select(&self, prompt: SelectPrompt) -> Result<String, AuthError> {
        self.recorded
            .lock()
            .prompts
            .push(format!("select: {}", prompt.message));
        self.select
            .clone()
            .ok_or_else(|| AuthError::interaction("unexpected select prompt"))
    }

    async fn prompt_manual_code(&self, prompt: TextPrompt) -> Result<String, AuthError> {
        self.recorded
            .lock()
            .prompts
            .push(format!("manual_code: {}", prompt.message));
        if self.manual_never_answers {
            std::future::pending::<()>().await;
        }
        let recorded = self.recorded.lock().clone();
        match &self.manual {
            Some(answer) => answer(&recorded),
            None => Err(AuthError::interaction("unexpected manual_code prompt")),
        }
    }

    fn open_url(&self, url: &str, _instructions: Option<&str>) {
        self.recorded.lock().auth_urls.push(url.to_string());
    }

    fn show_device_code(&self, device_code: &DeviceCode) {
        self.recorded.lock().device_codes.push(device_code.clone());
    }

    fn notify(&self, event: &AuthEvent) {
        self.recorded.lock().events.push(event.clone());
    }
}
