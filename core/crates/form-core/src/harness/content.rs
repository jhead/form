//! The response corpus (spec 02 §5).
//!
//! Every element of F7 has to appear somewhere in here, because the markdown renderer is
//! exercised by what the harness emits and by nothing else. `corpus_covers_the_markdown_surface`
//! in `tests.rs` is the guard against a body being edited down to something less demanding.
//!
//! Bodies are fixed text with a small number of substitutions rather than a grammar: a
//! generated soup of markdown reads as noise, and the point is to make the chat *look* like a
//! real session.

use rand::Rng;
use rand_chacha::ChaCha8Rng;

use super::rng::pick;

/// How long and how complete a turn's prose is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Shape {
    /// One or two sentences. The whole answer.
    Brief,
    /// A short lead-in before the model reaches for tools.
    Handoff,
    /// The workhorse: headings, lists, a table, several fences.
    Standard,
    /// Several hundred lines — the reflow and scroll-anchoring stress case.
    Long,
    /// Cut off by the output token cap in the middle of a fence (F7.3).
    Truncated,
}

pub(super) struct Response {
    pub text: String,
    /// The stream ends `done { reason: length }` with an open fence.
    pub truncated: bool,
}

pub(super) fn response(rng: &mut ChaCha8Rng, shape: Shape, topic: &str) -> Response {
    let topic = topic_phrase(topic);
    match shape {
        Shape::Brief => Response {
            text: pick(rng, BRIEF).replace("{topic}", &topic),
            truncated: false,
        },
        Shape::Handoff => Response {
            text: pick(rng, HANDOFF).replace("{topic}", &topic),
            truncated: false,
        },
        Shape::Standard => Response {
            text: pick(rng, STANDARD).replace("{topic}", &topic),
            truncated: false,
        },
        Shape::Long => Response {
            text: long_body(rng, &topic),
            truncated: false,
        },
        Shape::Truncated => Response {
            text: TRUNCATED.replace("{topic}", &topic),
            truncated: true,
        },
    }
}

/// Plausible chain-of-thought. Shorter than the answer and never markdown — thinking renders
/// as plain text in its own block (spec 10 §5).
pub(super) fn thinking(rng: &mut ChaCha8Rng, topic: &str) -> String {
    let count = rng.gen_range(2..=5);
    let mut parts: Vec<String> = Vec::with_capacity(count);
    for i in 0..count {
        let pool = if i == 0 { THINKING_OPEN } else { THINKING_MID };
        let line = pick(rng, pool).replace("{topic}", &topic_phrase(topic));
        if !parts.contains(&line) {
            parts.push(line);
        }
    }
    parts.join(" ")
}

/// The user's prompt, squeezed into something that reads well inline.
fn topic_phrase(prompt: &str) -> String {
    let first = prompt
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("this")
        .trim();
    let collapsed = first.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed: String = collapsed.chars().take(64).collect();
    let trimmed = trimmed
        .trim_end_matches(['.', ',', ':', ';', '!', '?'])
        .trim();
    if trimmed.is_empty() {
        "this".to_string()
    } else {
        trimmed.to_string()
    }
}

// ---------------------------------------------------------------- brief

const BRIEF: &[&str] = &[
    "Done. `health_check` is registered before the auth layer now, so unauthenticated probes \
get a `200` instead of a `401`.",
    "That's already handled — `Store::append_entry` takes the lock once and returns the \
minted `Entry`, so there's no read-back race to worry about.",
    "Yes, but only for the *streaming* path. The batch path still buffers the whole response, \
which is why it looks instant in tests and sluggish in the app.",
    "Short answer: it's the `speed` multiplier. Set `harnessSpeed` to `1.0` and the cadence \
matches what you'd see against a real provider.",
    "I'd leave it. The extra indirection buys nothing until there's a second transport, and \
`CoreTransport` already gives you the seam when there is one.",
];

// ---------------------------------------------------------------- handoff

const HANDOFF: &[&str] = &[
    "Let me look at how {topic} is wired up before changing anything.",
    "I'll start by reading the relevant files, then make the change.",
    "Checking the current behaviour first — I want to see the failing path rather than guess \
at it.",
    "Before I touch this, let me confirm where {topic} is actually handled.",
    "Two things to verify first: where the entry point lives, and whether the tests already \
cover it.",
];

// ---------------------------------------------------------------- standard

const STANDARD: &[&str] = &[
    STANDARD_ROUTING,
    STANDARD_FRONTEND,
    STANDARD_SWIFT,
    STANDARD_DATA,
];

const STANDARD_ROUTING: &str = r##"## What I found

The handler is registered *after* the auth middleware, so every probe is rejected with a
`401` before it ever reaches `health_check`. Two things need to move.

### Call sites

| File | Line | Role |
|---|---:|---|
| `src/server.rs` | 84 | route table |
| `src/middleware/auth.rs` | 31 | rejects unauthenticated requests |
| `tests/health.rs` | 12 | the failing test |

> The middleware is doing exactly what it was asked to do. The bug is ordering, not logic.

Here is the fix — the public routes have to be merged **before** the layer is applied,
because `tower` layers wrap everything added ahead of them:

```rust
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health_check))
        .route("/health/ready", get(readiness))
        .merge(protected_routes(state.clone()))
        .layer(middleware::from_fn_with_state(state, auth::require_token))
}

async fn health_check() -> impl IntoResponse {
    Json(json!({ "status": "ok", "version": env!("CARGO_PKG_VERSION") }))
}
```

Then confirm it locally:

```bash
cargo test -p api health -- --nocapture
curl -sS localhost:8080/health | jq .
```

**Remaining work**

- [x] Move the route above the auth layer
- [ ] Add a readiness probe that checks the connection pool
- [ ] Document the endpoint in [the ops runbook](https://example.com/runbook)

One caveat: the load balancer's health check interval is 5s, so a deploy that takes longer
than 15s to bind will still flap. That's worth a follow-up, but it isn't this bug.
"##;

const STANDARD_FRONTEND: &str = r##"### The re-render, and why it happens

`useTranscript` returns a new array identity on every event, so `<Transcript>` re-renders
even when nothing visible changed. With ~40 events per second that's the entire scroll
container reconciling per frame.

The fix is to key off the *entry id* and let the tail block own its own subscription:

```typescript
export function useTranscript(sessionId: string): TranscriptView {
  const entries = useSyncExternalStore(
    (cb) => store.subscribe(sessionId, cb),
    () => store.snapshot(sessionId),
  );

  return useMemo(
    () => ({ entries, tail: entries.at(-1) ?? null }),
    [entries],
  );
}
```

The measurements, before and after:

| Scenario | Before | After |
|---|---:|---:|
| Idle transcript | 0.4 ms | 0.4 ms |
| Streaming, 120 blocks | 31.2 ms | 5.8 ms |
| Streaming + resize | 74.9 ms | 9.1 ms |

![Frame time before and after the change](https://example.com/frame-time.png)

---

The config that goes with it — note `flushSync` is ~~required~~ *not* required once the
store is external:

```json
{
  "transcript": {
    "debounceMs": 16,
    "maxBlocks": 400,
    "incrementalParse": true,
    "flushSync": false
  }
}
```

Worth reading the [React 18 tearing notes][1] before changing the store shape again.[^1]

[1]: https://example.com/react-tearing
[^1]: `useSyncExternalStore` is the only hook that is safe here; `useEffect` + `setState`
      reintroduces the tear under concurrent rendering.
"##;

const STANDARD_SWIFT: &str = r##"# Streaming into the transcript

The view can't reconcile against the deltas alone — it needs the `partial` snapshot, because
a dropped event would otherwise desynchronise the rendered text from the model's own idea of
the message. So: render from the deltas, *reconcile* against `partial`.

```swift
@MainActor
@Observable
final class TranscriptStore {
    private(set) var entries: [Entry] = []

    func apply(_ event: CoreEvent) {
        switch event {
        case let .messageUpdate(_, entryId, inner):
            guard let index = entries.firstIndex(where: { $0.id == entryId }) else { return }
            if let partial = inner.partial {
                entries[index].message = .assistant(partial)
            }
        case let .messageEnd(_, entry):
            upsert(entry)
        default:
            break
        }
    }
}
```

The diff against what's there today:

```diff
-        case let .messageUpdate(_, entryId, inner):
-            append(delta: inner.delta, to: entryId)
+        case let .messageUpdate(_, entryId, inner):
+            guard let index = entries.firstIndex(where: { $0.id == entryId }) else { return }
+            if let partial = inner.partial {
+                entries[index].message = .assistant(partial)
+            }
```

Three things follow from this:

1. `Entry` has to be `Identifiable` on `id`, not on array position.
2. The `AsyncStream` must be unbounded, or a slow frame drops events.
3. Nothing in `FormUI` may hold a reference to the previous partial — it is replaced whole.

> Reconciliation is cheap because `AssistantMessage` is a value type. It is *not* cheap if
> you keep a parsed markdown tree alongside it; parse the tail block only.
"##;

const STANDARD_DATA: &str = r##"## Backfill plan

The migration is safe to run online — every write goes through `record_turn`, and the new
columns are nullable until the backfill finishes.

### Steps

1. Add the columns, no default, no rewrite
2. Deploy the writer that populates them
3. Backfill in batches of 5,000 by `started_at`
4. Add the `NOT NULL` constraint in a second migration

```python
def backfill(conn, batch: int = 5_000) -> int:
    total = 0
    while True:
        rows = conn.execute(
            """
            SELECT id, input, output FROM turns
             WHERE total_tokens IS NULL
             ORDER BY started_at
             LIMIT ?
            """,
            (batch,),
        ).fetchall()
        if not rows:
            return total
        conn.executemany(
            "UPDATE turns SET total_tokens = ? WHERE id = ?",
            [(r["input"] + r["output"], r["id"]) for r in rows],
        )
        conn.commit()
        total += len(rows)
```

Run it with:

```bash
python -m tools.backfill --batch 5000 --sleep 0.2 \
  | tee /tmp/backfill.log
```

**Things that will bite you**

- `sqlite3` will hold a write lock for the whole `executemany` — keep the batch small
- The index on `turns(started_at)` is what makes the `ORDER BY` cheap; don't drop it
- Timestamps are stored in **UTC milliseconds**; bucketing happens at read time in the
  caller's timezone

| Batch | Rows/s | Lock held |
|---:|---:|---:|
| 1,000 | 4,100 | 240 ms |
| 5,000 | 9,800 | 510 ms |
| 20,000 | 11,200 | 1,900 ms |
"##;

// ---------------------------------------------------------------- truncated

/// Ends inside an open fence so F7.3 is exercised at rest as well as mid-stream. This is
/// what a real response that hits `max_output` looks like.
const TRUNCATED: &str = r##"### Walking through {topic}

There are three layers involved, and the interesting one is the middle. The transport hands
the core a JSON string, the core routes it, and the result comes back on the event stream
rather than as a return value — which is what makes cancellation expressible at all.

The full dispatcher looks like this:

```rust
pub fn dispatch(&self, command: Command, command_id: String) -> Result<()> {
    match command {
        Command::SendPrompt { session_id, text, .. } => {
            let signal = AbortSignal::new();
            self.active.lock().unwrap().insert(session_id.clone(), signal.clone());
            self.runtime.spawn(async move {
                harness.run(request, ctx, signal).await;
"##;

// ---------------------------------------------------------------- thinking

const THINKING_OPEN: &[&str] = &[
    "The user is asking about {topic}.",
    "Let me think about {topic} carefully before answering.",
    "So the question is about {topic} — I should check what the code actually does rather \
than assume.",
    "Okay, {topic}. There are a couple of ways this could go wrong.",
];

const THINKING_MID: &[&str] = &[
    "The obvious answer is that the handler is missing, but the route table says otherwise, \
so it's more likely an ordering problem.",
    "I should read the file before proposing a change — guessing at line numbers has burned \
me here before.",
    "There are two call sites, and only one of them is covered by a test. That asymmetry is \
probably where the bug lives.",
    "Worth checking whether this is the streaming path or the batch path; they diverge and \
only one of them is instrumented.",
    "If I change the signature I have to update every caller, so a wrapper is the smaller \
change even if it is slightly uglier.",
    "The test would pass either way, which means the test is not actually asserting the thing \
we care about.",
    "Let me make sure I am not about to reintroduce the bug that the previous commit fixed.",
    "Cheapest thing that could work: read the two files, confirm the ordering, make one edit.",
];

// ---------------------------------------------------------------- long

const LONG_SECTIONS: &[(&str, &str)] = &[
    (
        "Where the work happens",
        "Most of the cost is in the reconciliation pass, not in the \
parse. The parser is incremental and only ever touches the tail block; the reconciliation \
walks the whole array because it has no way to know what changed.",
    ),
    (
        "The event stream",
        "Events arrive in order on one dispatcher thread, and the bridge is \
the single place that hops to the main actor. Everything downstream of that hop can assume \
serial delivery, which removes most of the locking you would otherwise need.",
    ),
    (
        "Partial accumulation",
        "Each non-terminal event carries the accumulated message. That \
looks wasteful — and it is, in bytes — but it is what makes a dropped or coalesced event \
survivable, and it is the reason the renderer never has to replay a transcript.",
    ),
    (
        "Abort semantics",
        "Cancellation is an explicit signal rather than a dropped future, \
because the caller lives on the other side of a C ABI and cannot drop anything. The run polls \
the signal between events, so an abort during a four-second tool execution still lands \
promptly.",
    ),
    (
        "Tool execution",
        "Tool calls stream their arguments as fragments, which is deliberate: \
it is the only thing that exercises partial-JSON rendering, and partial-JSON rendering is \
where the argument summary row gets its shape.",
    ),
    (
        "Usage accounting",
        "Token counts are estimated from the generated content and priced \
from the catalog, so the number in the turn footer, the number in the context ring, and the \
number on the dashboard are all the same number.",
    ),
    (
        "Caching",
        "The cacheable prefix is everything except the freshest message. On the first \
turn that is a cache write; on every later turn it is a cache read plus a small write for the \
increment. The ratio between them is the whole of the cache-effectiveness chart.",
    ),
    (
        "What is left",
        "The parts that are still stubbed are the parts that need a real \
provider: authentication, rate limiting, and retry. None of them change the event protocol, \
which is the point of having frozen it first.",
    ),
];

const LONG_CODE: &[(&str, &str)] = &[
    (
        "rust",
        r##"pub async fn run(&mut self) -> RunOutcome {
    self.emit(EventKind::RunStart {
        session_id: self.session_id.clone(),
        run_id: self.run_id.clone(),
    });
    let mut outcome = RunOutcome::Completed;
    while let Some(turn) = self.next_turn() {
        match self.execute(turn).await {
            Step::Continue => continue,
            Step::Aborted => { outcome = RunOutcome::Aborted; break }
            Step::Failed => { outcome = RunOutcome::Failed; break }
        }
    }
    outcome
}"##,
    ),
    (
        "swift",
        r##"func stream(_ session: SessionID) -> AsyncStream<CoreEvent> {
    AsyncStream(bufferingPolicy: .unbounded) { continuation in
        let token = transport.subscribe { event in
            guard event.sessionID == session else { return }
            continuation.yield(event)
        }
        continuation.onTermination = { _ in transport.unsubscribe(token) }
    }
}"##,
    ),
    (
        "typescript",
        r##"export async function* deltas(stream: EventStream): AsyncGenerator<string> {
  for await (const event of stream) {
    if (event.type === "text_delta") {
      yield event.delta;
    }
    if (event.type === "done" || event.type === "error") {
      return;
    }
  }
}"##,
    ),
    (
        "python",
        r##"def percentile(values: list[int], p: float) -> int:
    if not values:
        return 0
    ordered = sorted(values)
    index = min(len(ordered) - 1, int(round((len(ordered) - 1) * p)))
    return ordered[index]"##,
    ),
    (
        "json",
        r##"{
  "range": "d30",
  "headline": { "sessions": 24, "turns": 318, "totalTokens": 4128900 },
  "cache": { "read": 2841100, "write": 391200, "savingsUsd": 12.83 }
}"##,
    ),
    (
        "bash",
        r##"set -euo pipefail
cargo test -p form-core harness:: -- --nocapture
cargo clippy -p form-core --all-targets -- -D warnings
cargo fmt --all -- --check"##,
    ),
    (
        "diff",
        r##"@@ -186,9 +186,13 @@ impl Core {
-        if active.contains_key(&session_id) {
-            return Err(CoreError::RunAlreadyActive(session_id));
-        }
+        if active.contains_key(&session_id) {
+            self.queued
+                .lock()
+                .unwrap()
+                .entry(session_id)
+                .or_default()
+                .push_back(text);
+            return Ok(());
+        }"##,
    ),
];

fn long_body(rng: &mut ChaCha8Rng, topic: &str) -> String {
    let mut out = String::new();
    out.push_str(&format!("# {topic}\n\n"));
    out.push_str(
        "This is longer than it needs to be, but you asked for the whole picture, so here is \
the whole picture — the layers, why each one exists, and what I would change first.\n\n",
    );
    out.push_str("**Contents**\n\n");
    for (i, (title, _)) in LONG_SECTIONS.iter().enumerate() {
        out.push_str(&format!("{}. {}\n", i + 1, title));
    }
    out.push('\n');

    let start = rng.gen_range(0..LONG_CODE.len());
    // Two passes over the section list: one to state the shape, one to work through the
    // consequences. That is what gets this to the ~400 lines F7.3 needs to be tested against.
    for (i, (title, body)) in LONG_SECTIONS.iter().chain(LONG_SECTIONS).enumerate() {
        let second_pass = i >= LONG_SECTIONS.len();
        out.push_str(&format!(
            "## {}. {}{}\n\n{}\n\n",
            i + 1,
            title,
            if second_pass { ", in practice" } else { "" },
            body
        ));
        out.push_str(&format!("{}\n\n", LONG_ASIDES[i % LONG_ASIDES.len()]));

        let (lang, code) = &LONG_CODE[(start + i) % LONG_CODE.len()];
        out.push_str(&format!("```{lang}\n{code}\n```\n\n"));

        out.push_str("Notes on the above:\n\n");
        for note in LONG_NOTES.iter().skip(i % 3).take(4) {
            out.push_str(&format!("- {note}\n"));
        }
        out.push('\n');

        if i % 3 == 1 {
            out.push_str(
                "> Worth stating plainly: none of this is load-bearing until the real \
provider lands. It is scaffolding that happens to be honest about its shape.\n\n",
            );
        }
        if i % 4 == 2 {
            out.push_str("| Metric | Before | After |\n|---|---:|---:|\n");
            for (name, before, after) in LONG_METRICS {
                out.push_str(&format!("| {name} | {before} | {after} |\n"));
            }
            out.push('\n');
        }
        out.push_str("---\n\n");
    }

    out.push_str("## Where I would start\n\n");
    out.push_str("- [x] Freeze the event protocol\n");
    out.push_str("- [x] Make the stub emit it exactly\n");
    out.push_str("- [ ] Swap in the real agent behind the same trait\n");
    out.push_str("- [ ] Delete this file\n\n");
    out.push_str(
        "If only one of those happens this week, make it the third — everything else is \
already provably in place.\n",
    );
    out
}

const LONG_ASIDES: &[&str] = &[
    "The cost of getting this wrong is not a crash — it is a transcript that looks right and \
is subtly out of order, which is far harder to notice in review.",
    "None of this needs to be fast. It needs to be *predictable*, because the thing consuming \
it is a renderer that cannot ask for a replay.",
    "Two teams have now independently reinvented this, which is usually a sign that the seam \
is in the wrong place. It is not, in this case; it is just genuinely fiddly.",
    "If you only remember one thing from this section, make it the ordering rule: the \
terminal event is last, and nothing about it is negotiable.",
];

const LONG_NOTES: &[&str] = &[
    "the `partial` snapshot is the source of truth; the delta is an optimisation",
    "ordering is guaranteed, concurrency is not — do not assume a second thread",
    "`content_index` is an index into `content`, never a running counter",
    "the terminal event is emitted exactly once, and nothing follows it",
    "errors are values on the stream, not exceptions across the boundary",
    "every id is minted in Rust; the client never constructs one",
];

const LONG_METRICS: &[(&str, &str, &str)] = &[
    ("p50 frame", "31.2 ms", "5.8 ms"),
    ("p99 frame", "74.9 ms", "9.1 ms"),
    ("allocations / event", "412", "6"),
];
