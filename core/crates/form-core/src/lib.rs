//! form's portable core.
//!
//! Everything that is not rendering lives here: session storage, search, settings, the
//! provider catalog, usage analytics, markdown parsing, context accounting, and (for now) a
//! stub agent harness. macOS talks to it over a C ABI carrying JSON; Windows and Linux
//! clients will reuse it unchanged.
//!
//! Start with `docs/specs/00-protocol.md` — the boundary contract is frozen, and every
//! module below implements a piece of it.

pub mod app;
pub mod catalog;
pub mod context;
pub mod core;
pub mod error;
pub mod events;
pub mod harness;
pub mod markdown;
pub mod protocol;
pub mod settings;
pub mod stats;

pub use crate::core::Core;
pub use error::{CoreError, Result};
pub use protocol::{Command, CoreConfig, Envelope, Event, EventKind, Query, ABI_VERSION};
