//! Fixture and case types for the adapter conformance suite.
//!
//! Port of `.upstream/packages/telemetry/src/testing/types.ts`. Upstream's
//! `AsyncDisposable` fixture becomes an async [`TelemetryAdapterFixture::close`];
//! `getSpans()` stays async so an adapter can flush an exporter before
//! returning normalized snapshots.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;

use crate::{RecordedTelemetrySpan, TelemetryContext};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A fresh adapter instance and normalized snapshot reader owned by one case.
#[async_trait]
pub trait TelemetryAdapterFixture: Send + Sync {
    fn context(&self) -> Arc<dyn TelemetryContext>;

    /// Finished and open spans as normalized snapshots, in span-start order.
    async fn spans(&self) -> Vec<RecordedTelemetrySpan>;

    /// Release adapter resources. The default does nothing.
    async fn close(&self) {}
}

/// Creates an isolated adapter fixture for one conformance case.
#[async_trait]
pub trait TelemetryAdapterFixtureFactory: Send + Sync + 'static {
    async fn create(&self) -> Box<dyn TelemetryAdapterFixture>;
}

type CaseRunner = Arc<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>;

/// A runner-independent case that can be registered with any test framework.
#[derive(Clone)]
pub struct TelemetryAdapterConformanceCase {
    pub group: &'static str,
    pub name: &'static str,
    runner: CaseRunner,
}

impl TelemetryAdapterConformanceCase {
    pub(crate) fn new(group: &'static str, name: &'static str, runner: CaseRunner) -> Self {
        Self {
            group,
            name,
            runner,
        }
    }

    /// Run the case. Panics on failure, like any Rust assertion.
    pub fn run(&self) -> BoxFuture<'static, ()> {
        (self.runner)()
    }
}

impl std::fmt::Debug for TelemetryAdapterConformanceCase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TelemetryAdapterConformanceCase")
            .field("group", &self.group)
            .field("name", &self.name)
            .finish()
    }
}

/// Look up a recorded span by name, panicking with a useful message when the
/// adapter did not record it.
pub fn find_span<'a>(spans: &'a [RecordedTelemetrySpan], name: &str) -> &'a RecordedTelemetrySpan {
    spans
        .iter()
        .find(|span| span.name == name)
        .unwrap_or_else(|| {
            let recorded: Vec<&str> = spans.iter().map(|span| span.name.as_str()).collect();
            panic!("expected recorded span `{name}`, got {recorded:?}")
        })
}
