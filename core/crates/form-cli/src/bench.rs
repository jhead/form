//! `bench` — the two latency budgets the UI depends on, measured through the C ABI.
//!
//! Spec 03 §4: `getStats` for `all` must return in under 150 ms. Spec 05: a 120-block, 60 KB
//! markdown document must parse in under 16 ms, because a frame is 16.7 ms and the chat view
//! re-renders the tail block on every delta.
//!
//! Debug builds miss both by a wide margin; run this against `--release`.

use std::time::{Duration, Instant};

use serde_json::json;

use crate::api::{data, Core};
use crate::render::{BOLD, DIM, RED, RESET};

struct Budget {
    name: &'static str,
    limit: Duration,
    samples: Vec<Duration>,
}

impl Budget {
    fn percentile(&self, p: f64) -> Duration {
        let mut sorted = self.samples.clone();
        sorted.sort_unstable();
        let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
        sorted.get(idx).copied().unwrap_or_default()
    }

    fn report(&self) -> bool {
        let p50 = self.percentile(0.5);
        let p95 = self.percentile(0.95);
        let ok = p95 <= self.limit;
        println!(
            "  {:<28}p50 {:>7.2}ms  p95 {:>7.2}ms  budget {:>6.1}ms  {}",
            self.name,
            p50.as_secs_f64() * 1e3,
            p95.as_secs_f64() * 1e3,
            self.limit.as_secs_f64() * 1e3,
            if ok {
                format!("{DIM}ok{RESET}")
            } else {
                format!("{RED}OVER{RESET}")
            }
        );
        ok
    }
}

fn measure(name: &'static str, limit: Duration, runs: usize, mut f: impl FnMut()) -> Budget {
    // A few untimed passes so the first sample is not measuring lazy initialization.
    for _ in 0..3 {
        f();
    }
    let samples = (0..runs)
        .map(|_| {
            let started = Instant::now();
            f();
            started.elapsed()
        })
        .collect();
    Budget {
        name,
        limit,
        samples,
    }
}

/// The document spec 05 sets the budget against: ~120 blocks and ~60 KB, mixing the
/// constructs that cost the most — fences the highlighter has to tokenize, tables, and
/// nested lists.
fn big_document() -> String {
    // ~1 KB per paragraph, so 120-ish blocks land near the 60 KB the budget is stated for.
    const PROSE: &str = "The router registers each handler at startup, and the health check \
        is the cheapest possible probe: it touches no database, allocates nothing, and \
        returns a fixed shape so the load balancer can parse it without a schema. \
        *Emphasis*, **strong**, `inline code`, and a [link](https://example.com/health) all \
        appear here so the inline parser has something to do on every paragraph rather than \
        skipping straight to the next block. Readiness and liveness are deliberately \
        separate: liveness answers whether the process should be restarted, readiness \
        whether it should receive traffic, and conflating them turns a slow dependency into \
        a restart loop. The registry holds one closure per subsystem, each with its own \
        timeout, and the aggregate result is the worst of them — a check that cannot answer \
        inside its budget counts as failed rather than as unknown, because an unknown state \
        is not something a load balancer can act on. Every probe response carries the shard \
        identifier so a partial outage is visible in the logs of whatever called it, and \
        the timestamps are milliseconds since the epoch for the same reason they are \
        everywhere else in this system: they survive a JSON round trip without a timezone \
        argument. None of this costs anything at runtime; the whole handler is a few \
        atomics and a serialization, which is the point.";

    let mut out = String::with_capacity(64 * 1024);
    for i in 0..16 {
        out.push_str(&format!("## Section {i}\n\n"));
        out.push_str(PROSE);
        out.push_str("\n\n");
        out.push_str(PROSE);
        out.push_str("\n\n");
        out.push_str(&format!(
            "```rust\n\
             async fn health_{i}(State(app): State<App>) -> impl IntoResponse {{\n\
             \x20   let uptime = app.started.elapsed().as_secs();\n\
             \x20   let checks = app.registry.run_all().await;\n\
             \x20   if checks.iter().any(|c| !c.ok) {{\n\
             \x20       return (StatusCode::SERVICE_UNAVAILABLE, Json(checks)).into_response();\n\
             \x20   }}\n\
             \x20   Json(json!({{ \"ok\": true, \"uptime\": uptime, \"shard\": {i} }})).into_response()\n\
             }}\n```\n\n"
        ));
        out.push_str(
            "| field | type | notes |\n|---|---|---|\n\
             | ok | bool | false when any registered check fails |\n\
             | uptime | u64 | seconds since process start |\n\
             | shard | u32 | which replica answered |\n\n",
        );
        out.push_str(
            "1. Register the route\n   - behind the auth middleware\n   - with a 200 ms timeout\n\
             2. Add the integration test\n   - one healthy case\n   - one degraded case\n\n",
        );
        out.push_str("> Probes must not touch the database, or an outage becomes a stampede.\n\n");
        out.push_str(PROSE);
        out.push_str("\n\n");
    }
    out
}

fn block_count(document: &str) -> usize {
    document
        .split("\n\n")
        .filter(|b| !b.trim().is_empty())
        .count()
}

pub fn run(core: &Core) -> bool {
    let document = big_document();
    let blocks = block_count(&document);
    println!(
        "{BOLD}bench{RESET} {DIM}markdown document: {} KB, ~{} blocks{RESET}\n",
        document.len() / 1024,
        blocks
    );

    let mut budgets = Vec::new();

    budgets.push(measure(
        "renderMarkdown (complete)",
        Duration::from_millis(16),
        50,
        || {
            core.query(json!({ "type": "renderMarkdown", "text": document, "complete": true }));
        },
    ));

    // The streaming case is the one that runs per delta, so it is the one that matters.
    let tail = &document[..document.len() * 3 / 4];
    budgets.push(measure(
        "renderMarkdown (streaming)",
        Duration::from_millis(16),
        50,
        || {
            core.query(json!({ "type": "renderMarkdown", "text": tail, "complete": false }));
        },
    ));

    budgets.push(measure(
        "getStats all",
        Duration::from_millis(150),
        20,
        || {
            core.query(json!({ "type": "getStats", "range": "all", "tz": "UTC" }));
        },
    ));

    budgets.push(measure(
        "listSessions",
        Duration::from_millis(16),
        100,
        || {
            core.query(json!({ "type": "listSessions" }));
        },
    ));

    // Boundary overhead on its own: the cheapest possible query, so what is left is
    // marshalling. If this is not microseconds, the FFI layer has a problem.
    budgets.push(measure(
        "getSettings (ffi overhead)",
        Duration::from_millis(1),
        200,
        || {
            core.query(json!({ "type": "getSettings" }));
        },
    ));

    let mut all_ok = true;
    for budget in &budgets {
        all_ok &= budget.report();
    }

    // A `not_implemented` handler is fast for the wrong reason; say so rather than claiming a
    // pass. Several of these land with W3 and W5.
    let unimplemented: Vec<&str> = [
        (
            "renderMarkdown",
            json!({ "type": "renderMarkdown", "text": "x" }),
        ),
        (
            "getStats",
            json!({ "type": "getStats", "range": "all", "tz": "UTC" }),
        ),
    ]
    .into_iter()
    .filter(|(_, q)| data(&core.query(q.clone())).is_err())
    .map(|(name, _)| name)
    .collect();
    if !unimplemented.is_empty() {
        println!(
            "\n{DIM}note: {} still returns an error — those numbers measure the error path{RESET}",
            unimplemented.join(", ")
        );
    }

    if !all_ok && cfg!(debug_assertions) {
        println!(
            "{DIM}debug build — rerun with `cargo run --release --bin form-cli -- bench`{RESET}"
        );
    }
    all_ok
}
