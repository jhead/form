//! What is left of this crate's local shims.
//!
//! The rest of it — `constrained_sampling`, `cost`, `estimate`, `hash`,
//! `json_parse`, `sanitize_unicode`, `simple_options` and `transform_messages`
//! — was written here only because `pi-http`, `pi-catalog` and the shared
//! adapter crate were being built in parallel. Those homes exist now:
//! `pi_http::{estimate, hash, json_parse}` and
//! `pi_provider_common::{constrained_sampling, cost, sanitize_unicode,
//! simple_options, transform_messages}`.
//!
//! [`http_stream`] stays: it is a real gap, not a duplicate. `pi-http` has no
//! raw byte-stream accessor alongside `post_sse`, and the Mistral and
//! pi-messages adapters both need one. It should collapse into `pi-http` once
//! `HttpClient::post_bytes` lands (tracked in the cleanup backlog).

pub mod http_stream;
