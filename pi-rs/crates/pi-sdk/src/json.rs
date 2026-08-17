//! JSON-in / JSON-out wrappers.
//!
//! The narrowest possible FFI surface: strings across the boundary, everything
//! else handled here. A host that would rather bridge one function than thirty
//! can drive the SDK entirely through [`Request`] and [`Response`].
//!
//! Every payload uses the same `camelCase` wire shape as the TypeScript
//! implementation, so a host that already speaks to upstream `pi` does not need
//! a second serialization path.

use serde::{Deserialize, Serialize};

use crate::SdkError;

/// A request from the host.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum Request {
    /// List every model in the catalog.
    ListModels,
    /// List models matching a provider id.
    ListProviderModels { provider: String },
    /// Resolve `"provider/model-id"` to a full model descriptor.
    ResolveModel { reference: String },
    /// List provider descriptors.
    ListProviders,
}

/// A response to the host. Errors are values, not exceptions: an FFI boundary
/// cannot carry a Rust panic, so failure is always `Response::Error`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum Response {
    Ok { result: serde_json::Value },
    Error { code: String, message: String },
}

impl Response {
    pub fn ok<T: Serialize>(value: T) -> Self {
        match serde_json::to_value(value) {
            Ok(result) => Response::Ok { result },
            Err(err) => Response::Error {
                code: "serialization".into(),
                message: err.to_string(),
            },
        }
    }

    pub fn error(err: &SdkError) -> Self {
        Response::Error {
            code: err.code().to_string(),
            message: err.message(),
        }
    }

    /// Serialize, falling back to a hand-built error object if that fails —
    /// this function must never itself return an error to the host.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|err| {
            format!(
                r#"{{"status":"error","code":"serialization","message":{}}}"#,
                serde_json::Value::String(err.to_string())
            )
        })
    }
}

impl crate::Pi {
    /// Handle a JSON request, returning a JSON response.
    ///
    /// Never returns `Err`: a malformed request is a `Response::Error`, so the
    /// host has exactly one thing to parse.
    pub async fn handle_json(&self, request: &str) -> String {
        let parsed: Request = match serde_json::from_str(request) {
            Ok(parsed) => parsed,
            Err(err) => {
                return Response::Error {
                    code: "invalid_request".into(),
                    message: err.to_string(),
                }
                .to_json()
            }
        };
        self.handle(parsed).await.to_json()
    }

    /// Typed form of [`Pi::handle_json`](crate::Pi::handle_json).
    pub async fn handle(&self, request: Request) -> Response {
        match request {
            Request::ListModels => Response::ok(self.models()),
            Request::ListProviderModels { provider } => Response::ok(
                self.models()
                    .into_iter()
                    .filter(|m| m.provider == provider)
                    .collect::<Vec<_>>(),
            ),
            Request::ResolveModel { reference } => match self.resolve_model(&reference).await {
                Ok(model) => Response::ok(model),
                Err(err) => Response::error(&err),
            },
            Request::ListProviders => Response::ok(self.registry().providers()),
        }
    }
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn resolves_a_model_over_the_json_boundary() {
        let pi = crate::Pi::builder().build().unwrap();
        let out = pi
            .handle_json(r#"{"op":"resolveModel","reference":"anthropic/claude-sonnet-4-5"}"#)
            .await;
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["status"], "ok");
        assert_eq!(parsed["result"]["provider"], "anthropic");
        // camelCase on the wire, matching the TypeScript implementation.
        assert!(parsed["result"]["contextWindow"].is_number());
    }

    #[tokio::test]
    async fn a_malformed_request_is_an_error_value_not_a_panic() {
        let pi = crate::Pi::builder().build().unwrap();
        let out = pi.handle_json("not json").await;
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["status"], "error");
        assert_eq!(parsed["code"], "invalid_request");
    }

    #[tokio::test]
    async fn an_unknown_model_reports_a_coded_error() {
        let pi = crate::Pi::builder().build().unwrap();
        let out = pi
            .handle_json(r#"{"op":"resolveModel","reference":"nope/nope"}"#)
            .await;
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["status"], "error");
        assert_eq!(parsed["code"], "catalog");
    }
}
