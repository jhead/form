//! The SQLite backend against `pi-session`'s shared conformance suite.
//!
//! Port of `test/conformance.test.ts`. Every case gets its own temp directory,
//! its own database and its own repository; the `TempDir` rides in the
//! fixture's guard so it outlives the case.

use std::sync::Arc;

use pi_session::testing::{
    run_session_backend_conformance, session_backend_conformance_cases, ConformanceFixture,
};
use pi_session::SessionCreateOptions;
use pi_session_sqlite::SqliteSessionRepo;

fn fixture() -> ConformanceFixture {
    let root = tempfile::tempdir().expect("temp dir");
    let cwd = root.path().display().to_string();
    let repository = Arc::new(SqliteSessionRepo::new(root.path().join("sessions.sqlite")));
    ConformanceFixture::new(repository)
        .with_defaults(SessionCreateOptions::new().with_cwd(cwd))
        .with_guard(Box::new(root))
}

#[tokio::test(flavor = "multi_thread")]
async fn sqlite_backend_passes_the_session_conformance_suite() {
    run_session_backend_conformance(&|| Box::pin(async { fixture() })).await;
}

/// The suite is also enumerable, so a failure names the case it came from.
#[tokio::test(flavor = "multi_thread")]
async fn every_conformance_case_runs_in_isolation() {
    let cases = session_backend_conformance_cases();
    assert_eq!(cases.len(), 30, "the upstream suite has 30 cases");
    for case in cases {
        let name = format!("{case:?}");
        let result = std::panic::AssertUnwindSafe(case.run(&|| Box::pin(async { fixture() })));
        futures::FutureExt::catch_unwind(result)
            .await
            .unwrap_or_else(|_| panic!("conformance case failed: {name}"));
    }
}
