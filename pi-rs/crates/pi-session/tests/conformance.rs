//! The shared backend conformance suite, run against both in-tree backends.
//!
//! `pi-session-sqlite` (W12) runs the same `session_backend_conformance_cases()`
//! against its own fixture factory.

use std::sync::Arc;

use pi_session::memory::InMemorySessionRepo;
use pi_session::testing::{
    session_backend_conformance_cases, ConformanceFixture, ConformanceFixtureFactory,
};
use pi_session::types::SessionCreateOptions;
use pi_session::JsonlSessionRepo;

fn in_memory_factory() -> Box<ConformanceFixtureFactory> {
    Box::new(|| Box::pin(async { ConformanceFixture::new(Arc::new(InMemorySessionRepo::new())) }))
}

fn jsonl_factory() -> Box<ConformanceFixtureFactory> {
    Box::new(|| {
        Box::pin(async {
            let root = tempfile::tempdir().expect("temp dir");
            let repository = Arc::new(JsonlSessionRepo::new(root.path()));
            let defaults = SessionCreateOptions::new().with_cwd(root.path().display().to_string());
            ConformanceFixture::new(repository)
                .with_defaults(defaults)
                .with_guard(Box::new(root))
        })
    })
}

async fn run_all(factory: Box<ConformanceFixtureFactory>) {
    for case in session_backend_conformance_cases() {
        // Name the case in the panic message so a failure is identifiable
        // without re-running the whole suite.
        eprintln!("conformance: {} / {}", case.group, case.name);
        case.run(factory.as_ref()).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn in_memory_backend_conformance() {
    run_all(in_memory_factory()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn jsonl_backend_conformance() {
    run_all(jsonl_factory()).await;
}
