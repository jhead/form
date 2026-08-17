//! Reusable adapter conformance helpers.
//!
//! Port of `.upstream/packages/telemetry/src/testing/`. This is a normal
//! module rather than a `#[cfg(test)]` one because other crates run the suite
//! against their own adapters, exactly as upstream's `/testing` subpath is
//! consumed.

mod conformance;
mod types;

pub use conformance::{
    run_telemetry_adapter_conformance, telemetry_adapter_conformance,
    InMemoryTelemetryFixtureFactory,
};
pub use types::{
    find_span, BoxFuture, TelemetryAdapterConformanceCase, TelemetryAdapterFixture,
    TelemetryAdapterFixtureFactory,
};
