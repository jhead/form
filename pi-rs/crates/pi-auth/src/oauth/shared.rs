//! Pieces the browser-redirect flows share.

use pi_core::message::now_ms;

use crate::error::AuthError;
use crate::interaction::LoginContext;
use crate::oauth::callback_server::{CallbackParams, LoopbackCallback};
use crate::types::TextPrompt;

/// What a user can paste back: a full redirect URL, a `code=…&state=…` query
/// fragment, a `code#state` pair, or a bare code. Port of the
/// `parseAuthorizationInput` helper duplicated across the upstream flows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuthorizationInput {
    pub code: Option<String>,
    pub state: Option<String>,
}

pub fn parse_authorization_input(input: &str) -> AuthorizationInput {
    let value = input.trim();
    if value.is_empty() {
        return AuthorizationInput::default();
    }

    if let Ok(url) = url::Url::parse(value) {
        // `url::Url` happily parses a bare code with a colon in it; only treat
        // it as a URL when it actually has a scheme with an authority.
        if url.has_authority() {
            let mut parsed = AuthorizationInput::default();
            for (key, value) in url.query_pairs() {
                match key.as_ref() {
                    "code" => parsed.code = Some(value.to_string()),
                    "state" => parsed.state = Some(value.to_string()),
                    _ => {}
                }
            }
            return parsed;
        }
    }

    if let Some((code, state)) = value.split_once('#') {
        return AuthorizationInput {
            code: Some(code.to_string()),
            state: Some(state.to_string()),
        };
    }

    if value.contains("code=") {
        let mut parsed = AuthorizationInput::default();
        for pair in value.split('&') {
            match pair.split_once('=') {
                Some(("code", v)) => parsed.code = Some(v.to_string()),
                Some(("state", v)) => parsed.state = Some(v.to_string()),
                _ => {}
            }
        }
        return parsed;
    }

    AuthorizationInput {
        code: Some(value.to_string()),
        state: None,
    }
}

/// Whichever of the two ways in came first.
#[derive(Debug)]
pub enum RedirectOutcome {
    /// The loopback server received the browser redirect.
    Callback(CallbackParams),
    /// The user pasted the code or redirect URL.
    Manual(AuthorizationInput),
}

/// Race the loopback callback against the host's manual paste-back prompt.
///
/// Upstream wires this up with an extra `AbortController` per prompt so the
/// losing side can be dismissed. Here the loser is simply the dropped branch of
/// the `select!`, which cancels the prompt future — the host sees its
/// `prompt_manual_code` future dropped and can dismiss the UI.
pub async fn await_redirect(
    ctx: &LoginContext,
    callback: Option<&mut LoopbackCallback>,
    prompt: TextPrompt,
) -> Result<RedirectOutcome, AuthError> {
    let Some(callback) = callback else {
        let input = ctx.interaction.prompt_manual_code(prompt).await?;
        return Ok(RedirectOutcome::Manual(parse_authorization_input(&input)));
    };

    tokio::select! {
        received = callback.wait(&ctx.signal) => match received? {
            Some(params) => Ok(RedirectOutcome::Callback(params)),
            None => Err(AuthError::oauth("OAuth callback did not complete.")),
        },
        manual = ctx.interaction.prompt_manual_code(prompt) => {
            Ok(RedirectOutcome::Manual(parse_authorization_input(&manual?)))
        }
    }
}

/// Absolute expiry from a relative `expires_in`, minus a safety skew.
pub fn expires_at(expires_in_seconds: f64, skew_ms: i64) -> i64 {
    now_ms() + (expires_in_seconds * 1000.0) as i64 - skew_ms
}

/// Only http(s) URLs are handed to a host that may open a browser, so a
/// malicious response cannot make it launch something else.
pub fn trusted_http_url(value: &str) -> Option<String> {
    let url = url::Url::parse(value).ok()?;
    matches!(url.scheme(), "http" | "https").then(|| url.to_string())
}

/// Same, https only (xAI).
pub fn trusted_https_url(value: &str) -> Option<String> {
    let url = url::Url::parse(value).ok()?;
    (url.scheme() == "https").then(|| url.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_full_redirect_url() {
        let parsed = parse_authorization_input(
            "http://localhost:53692/callback?code=the-code&state=the-state",
        );
        assert_eq!(parsed.code.as_deref(), Some("the-code"));
        assert_eq!(parsed.state.as_deref(), Some("the-state"));
    }

    #[test]
    fn parses_a_hash_separated_code_and_state() {
        let parsed = parse_authorization_input("the-code#the-state");
        assert_eq!(parsed.code.as_deref(), Some("the-code"));
        assert_eq!(parsed.state.as_deref(), Some("the-state"));
    }

    #[test]
    fn parses_a_bare_query_fragment() {
        let parsed = parse_authorization_input("code=the-code&state=the-state");
        assert_eq!(parsed.code.as_deref(), Some("the-code"));
        assert_eq!(parsed.state.as_deref(), Some("the-state"));
    }

    #[test]
    fn treats_anything_else_as_a_bare_code() {
        let parsed = parse_authorization_input("  the-code  ");
        assert_eq!(parsed.code.as_deref(), Some("the-code"));
        assert_eq!(parsed.state, None);
    }

    #[test]
    fn empty_input_yields_no_code() {
        assert_eq!(
            parse_authorization_input("   "),
            AuthorizationInput::default()
        );
    }

    #[test]
    fn only_http_urls_are_trusted_for_the_browser() {
        assert!(trusted_http_url("https://example.test/device").is_some());
        assert!(trusted_http_url("http://example.test/device").is_some());
        assert!(trusted_http_url("file:///etc/passwd").is_none());
        assert!(trusted_http_url("not a url").is_none());
        assert!(trusted_https_url("http://example.test").is_none());
    }
}
