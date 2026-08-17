//! The optional loopback OAuth callback server.
//!
//! Upstream spins up a `node:http` server inside every browser-redirect flow.
//! Here it is opt-in: the Swift host usually owns the browser round-trip and
//! hands the redirect URL back through
//! [`AuthInteraction::prompt_manual_code`](crate::AuthInteraction::prompt_manual_code),
//! so a flow only binds a socket when the caller sets
//! [`LoginContext::local_callback_server`](crate::LoginContext).
//!
//! It speaks just enough HTTP/1.1 to read one request line and write one
//! response, which is all an OAuth redirect needs — no server framework, and
//! nothing that can outlive the login: the listener task stops when the
//! [`LoopbackCallback`] is dropped.

use std::collections::BTreeMap;

use pi_core::options::AbortSignal;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};

use crate::error::AuthError;
use crate::oauth::oauth_page::{oauth_error_html, oauth_success_html};

/// `PI_OAUTH_CALLBACK_HOST`, defaulting to loopback.
pub fn callback_host() -> String {
    std::env::var("PI_OAUTH_CALLBACK_HOST")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "127.0.0.1".to_string())
}

/// Query parameters from the redirect.
pub type CallbackParams = BTreeMap<String, String>;

/// A one-shot loopback listener for an OAuth redirect.
pub struct LoopbackCallback {
    /// The `redirect_uri` to send to the authorization server.
    redirect_uri: String,
    received: mpsc::Receiver<CallbackParams>,
    _shutdown: oneshot::Sender<()>,
    task: tokio::task::JoinHandle<()>,
}

impl std::fmt::Debug for LoopbackCallback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoopbackCallback")
            .field("redirect_uri", &self.redirect_uri)
            .finish_non_exhaustive()
    }
}

impl Drop for LoopbackCallback {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// How the callback page describes a successful login.
pub struct CallbackPage {
    pub success_message: String,
    /// Validated against the redirect's `state`; a mismatch is rejected in the
    /// browser and never handed to the flow.
    pub expected_state: Option<String>,
}

impl LoopbackCallback {
    /// Bind `host:port` (port 0 picks a free one) and serve `path`.
    pub async fn bind(
        host: &str,
        port: u16,
        path: &str,
        page: CallbackPage,
    ) -> Result<Self, AuthError> {
        let listener = TcpListener::bind((host, port)).await.map_err(|e| {
            AuthError::oauth(format!(
                "could not bind the OAuth callback server on {host}:{port}: {e}"
            ))
        })?;
        let local_addr = listener.local_addr().map_err(|e| {
            AuthError::oauth(format!("could not read the OAuth callback port: {e}"))
        })?;

        // `localhost` rather than the bind address: providers pin the
        // registered redirect_uri literally, and upstream registers localhost.
        let redirect_uri = format!("http://{host}:{}{path}", local_addr.port());

        let (tx, received) = mpsc::channel(1);
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
        let path = path.to_string();

        let task = tokio::spawn(async move {
            loop {
                let accepted = tokio::select! {
                    biased;
                    _ = &mut shutdown_rx => return,
                    accepted = listener.accept() => accepted,
                };
                let Ok((stream, _)) = accepted else { return };
                if serve_one(stream, &path, &page, &tx).await {
                    return;
                }
            }
        });

        Ok(Self {
            redirect_uri,
            received,
            _shutdown: shutdown_tx,
            task,
        })
    }

    /// Bind the default loopback host on `port`.
    pub async fn bind_default(
        port: u16,
        path: &str,
        page: CallbackPage,
    ) -> Result<Self, AuthError> {
        Self::bind(&callback_host(), port, path, page).await
    }

    pub fn redirect_uri(&self) -> &str {
        &self.redirect_uri
    }

    /// Wait for the redirect. `Ok(None)` means the listener stopped without a
    /// usable callback; abort is reported as [`AuthError::Cancelled`].
    pub async fn wait(
        &mut self,
        signal: &AbortSignal,
    ) -> Result<Option<CallbackParams>, AuthError> {
        if signal.is_aborted() {
            return Err(AuthError::Cancelled);
        }
        tokio::select! {
            biased;
            _ = signal.aborted() => Err(AuthError::Cancelled),
            params = self.received.recv() => Ok(params),
        }
    }
}

/// Handle one connection. Returns true when the login callback was claimed and
/// the listener should stop.
async fn serve_one(
    mut stream: TcpStream,
    path: &str,
    page: &CallbackPage,
    tx: &mpsc::Sender<CallbackParams>,
) -> bool {
    let Some(request_line) = read_request_line(&mut stream).await else {
        return false;
    };
    let Some(target) = request_line.split_whitespace().nth(1) else {
        respond(
            &mut stream,
            400,
            &oauth_error_html("Malformed request.", None),
        )
        .await;
        return false;
    };

    let (request_path, query) = match target.split_once('?') {
        Some((p, q)) => (p, q),
        None => (target, ""),
    };
    if request_path != path {
        respond(
            &mut stream,
            404,
            &oauth_error_html("Callback route not found.", None),
        )
        .await;
        return false;
    }

    let params = parse_query(query);

    if let Some(expected) = page.expected_state.as_ref() {
        if params.get("state").map(String::as_str) != Some(expected.as_str()) {
            respond(&mut stream, 400, &oauth_error_html("State mismatch.", None)).await;
            return false;
        }
    }

    if let Some(error) = params.get("error") {
        let description = params
            .get("error_description")
            .cloned()
            .unwrap_or_else(|| error.clone());
        respond(
            &mut stream,
            400,
            &oauth_error_html("Authorization was denied.", Some(&description)),
        )
        .await;
        let _ = tx.try_send(params);
        return true;
    }

    if !params.contains_key("code") {
        respond(
            &mut stream,
            400,
            &oauth_error_html("Missing authorization code.", None),
        )
        .await;
        return false;
    }

    respond(&mut stream, 200, &oauth_success_html(&page.success_message)).await;
    let _ = tx.try_send(params);
    true
}

/// Read up to the end of the request line. Bounded so a stray connection
/// cannot make the login allocate without limit.
async fn read_request_line(stream: &mut TcpStream) -> Option<String> {
    let mut buffer = Vec::with_capacity(1024);
    let mut chunk = [0u8; 512];
    loop {
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(end) = buffer.windows(2).position(|w| w == b"\r\n") {
            return Some(String::from_utf8_lossy(&buffer[..end]).to_string());
        }
        if buffer.len() > 8192 {
            return None;
        }
    }
    (!buffer.is_empty()).then(|| String::from_utf8_lossy(&buffer).to_string())
}

async fn respond(stream: &mut TcpStream, status: u16, html: &str) {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        _ => "Internal Server Error",
    };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\nCache-Control: no-store\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{html}",
        html.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.flush().await;
}

fn parse_query(query: &str) -> CallbackParams {
    let mut params = CallbackParams::new();
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        params.insert(percent_decode(key), percent_decode(value));
    }
    params
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
                match hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    Some(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    None => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_parsing_decodes_percent_and_plus_escapes() {
        let params = parse_query("code=abc%2Fdef&state=a+b&empty=");
        assert_eq!(params.get("code").map(String::as_str), Some("abc/def"));
        assert_eq!(params.get("state").map(String::as_str), Some("a b"));
        assert_eq!(params.get("empty").map(String::as_str), Some(""));
    }

    async fn get(url: &str) -> (u16, String) {
        let response = reqwest::get(url).await.expect("callback request");
        let status = response.status().as_u16();
        (status, response.text().await.unwrap_or_default())
    }

    #[tokio::test]
    async fn serves_the_success_page_and_yields_the_code() {
        let mut callback = LoopbackCallback::bind(
            "127.0.0.1",
            0,
            "/callback",
            CallbackPage {
                success_message: "Done.".into(),
                expected_state: Some("expected".into()),
            },
        )
        .await
        .unwrap();
        let url = format!("{}?code=the-code&state=expected", callback.redirect_uri());
        let signal = AbortSignal::never();

        let (request, wait) = tokio::join!(get(&url), callback.wait(&signal));
        let (status, body) = request;
        assert_eq!(status, 200);
        assert!(body.contains("Authentication successful"));

        let params = wait.unwrap().expect("callback params");
        assert_eq!(params.get("code").map(String::as_str), Some("the-code"));
    }

    #[tokio::test]
    async fn rejects_a_state_mismatch_without_completing_the_login() {
        let mut callback = LoopbackCallback::bind(
            "127.0.0.1",
            0,
            "/callback",
            CallbackPage {
                success_message: "Done.".into(),
                expected_state: Some("expected".into()),
            },
        )
        .await
        .unwrap();
        let bad = format!("{}?code=the-code&state=forged", callback.redirect_uri());
        let good = format!("{}?code=the-code&state=expected", callback.redirect_uri());

        let (status, body) = get(&bad).await;
        assert_eq!(status, 400);
        assert!(body.contains("State mismatch"));

        // The listener stays up for the real redirect.
        let signal = AbortSignal::never();
        let (request, wait) = tokio::join!(get(&good), callback.wait(&signal));
        assert_eq!(request.0, 200);
        assert!(wait.unwrap().is_some());
    }

    #[tokio::test]
    async fn unknown_routes_get_a_404_and_do_not_settle_the_login() {
        let callback = LoopbackCallback::bind(
            "127.0.0.1",
            0,
            "/callback",
            CallbackPage {
                success_message: "Done.".into(),
                expected_state: None,
            },
        )
        .await
        .unwrap();
        let base = callback
            .redirect_uri()
            .trim_end_matches("/callback")
            .to_string();

        let (status, body) = get(&format!("{base}/nope")).await;
        assert_eq!(status, 404);
        assert!(body.contains("Callback route not found"));
    }

    #[tokio::test]
    async fn an_aborted_wait_reports_cancellation() {
        let mut callback = LoopbackCallback::bind(
            "127.0.0.1",
            0,
            "/callback",
            CallbackPage {
                success_message: "Done.".into(),
                expected_state: None,
            },
        )
        .await
        .unwrap();

        let (handle, signal) = pi_core::options::AbortHandle::new();
        handle.abort();
        assert!(callback.wait(&signal).await.unwrap_err().is_cancelled());
    }
}
