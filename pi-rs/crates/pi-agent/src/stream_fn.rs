//! Port of `packages/agent/src/stream-fn.ts`.
//!
//! Hosts that own a model runtime can install its stream function here so
//! `Agent` works without the agent crate depending on a provider catalog.

use std::sync::OnceLock;

use parking_lot::RwLock;
use pi_core::StreamFn;

use crate::error::AgentError;

fn slot() -> &'static RwLock<Option<StreamFn>> {
    static SLOT: OnceLock<RwLock<Option<StreamFn>>> = OnceLock::new();
    SLOT.get_or_init(|| RwLock::new(None))
}

/// Install (or clear, with `None`) the process-wide fallback stream function.
pub fn set_default_stream_fn(stream_fn: Option<StreamFn>) {
    *slot().write() = stream_fn;
}

/// The installed fallback, if any.
pub fn default_stream_fn() -> Option<StreamFn> {
    slot().read().clone()
}

/// Resolve an explicit stream function, falling back to the installed default.
pub fn resolve_stream_fn(explicit: Option<StreamFn>) -> Result<StreamFn, AgentError> {
    explicit
        .or_else(default_stream_fn)
        .ok_or(AgentError::NoStreamFn)
}
