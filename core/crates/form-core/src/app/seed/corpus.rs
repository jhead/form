//! The text the demo corpus is built from.
//!
//! This is product copy, not test scaffolding — it is what a new user reads on first launch
//! and what every Home chart aggregates. Titles read like real sessions, prompts read like
//! real asks, and the replies carry the markdown range spec 05 has to render: headings,
//! fenced code in several languages, tables, task lists, blockquotes.

pub struct Topic {
    pub title: &'static str,
    /// Index into [`GROUPS`], or `None` for an ungrouped session.
    pub group: Option<usize>,
    pub root: Option<&'static str>,
    /// Woven into the shared reply templates so answers stay on-subject.
    pub subject: &'static str,
    pub file: &'static str,
    pub prompts: &'static [&'static str],
}

pub const GROUPS: [&str; 3] = ["Work", "Open source", "Learning"];

pub const ROOT_FORM: &str = "~/dev/form";
pub const ROOT_PI: &str = "~/dev/pi-rs";
pub const ROOT_API: &str = "~/work/atlas-api";
pub const ROOT_WEB: &str = "~/work/atlas-web";

pub const TOOLS: [&str; 9] = [
    "read",
    "edit",
    "write",
    "bash",
    "grep",
    "glob",
    "list",
    "web_search",
    "todo_write",
];

pub const TOPICS: &[Topic] = &[
    Topic {
        title: "Add a health check endpoint",
        group: Some(0),
        root: Some(ROOT_API),
        subject: "the health check endpoint",
        file: "src/routes/health.rs",
        prompts: &[
            "Add a health check endpoint to the API that reports database and cache connectivity.",
            "Can it return a 503 when the database is unreachable instead of a 200 with a degraded flag?",
            "Add a test for the degraded path.",
            "One more thing — put the git SHA in the response so we can tell which build is live.",
        ],
    },
    Topic {
        title: "Flaky integration test in the payments suite",
        group: Some(0),
        root: Some(ROOT_API),
        subject: "the flaky payments test",
        file: "tests/payments_test.rs",
        prompts: &[
            "The payments integration suite fails about one run in five on CI but never locally. Can you work out why?",
            "That makes sense. Can you make the fixture deterministic rather than adding a retry?",
            "Run it twenty times and confirm it's green.",
        ],
    },
    Topic {
        title: "Migrate the session store to SQLite",
        group: Some(0),
        root: Some(ROOT_API),
        subject: "the SQLite session store",
        file: "src/store/mod.rs",
        prompts: &[
            "We're outgrowing the JSON file store. Migrate sessions to SQLite with a migration runner.",
            "What happens to existing users' data on first launch after the upgrade?",
            "Add a WAL mode pragma and a busy timeout.",
            "Write the migration test that goes from an empty database and from a v0 file store.",
            "Benchmark listing 5,000 sessions before and after.",
        ],
    },
    Topic {
        title: "Rate limiting middleware",
        group: Some(0),
        root: Some(ROOT_API),
        subject: "the rate limiter",
        file: "src/middleware/rate_limit.rs",
        prompts: &[
            "Add a token bucket rate limiter as middleware, keyed by API key with a per-route override.",
            "Use a sliding window instead — the bucket lets through bursts we don't want.",
            "Return Retry-After and document the headers.",
        ],
    },
    Topic {
        title: "Why is the dashboard query slow?",
        group: Some(0),
        root: Some(ROOT_API),
        subject: "the dashboard aggregate query",
        file: "src/queries/dashboard.sql",
        prompts: &[
            "The dashboard takes 4 seconds to load for accounts with a lot of history. Find out where the time goes.",
            "Show me the query plan before and after adding that index.",
            "Can we precompute the daily rollup instead of aggregating on read?",
        ],
    },
    Topic {
        title: "Dark mode for the settings sheet",
        group: Some(0),
        root: Some(ROOT_WEB),
        subject: "dark mode in the settings sheet",
        file: "src/components/SettingsSheet.tsx",
        prompts: &[
            "The settings sheet still has hardcoded light colors. Move them onto the theme tokens.",
            "There's a flash of light background when the sheet opens in dark mode. Fix it.",
            "Add a visual regression test for both modes.",
        ],
    },
    Topic {
        title: "Accessibility audit of the checkout flow",
        group: Some(0),
        root: Some(ROOT_WEB),
        subject: "checkout accessibility",
        file: "src/routes/checkout.tsx",
        prompts: &[
            "Audit the checkout flow for keyboard and screen reader accessibility and list what's broken.",
            "Fix the focus trap in the payment modal first.",
            "Now the form errors — they're announced as one blob.",
        ],
    },
    Topic {
        title: "Upgrade to React 19",
        group: Some(0),
        root: Some(ROOT_WEB),
        subject: "the React 19 upgrade",
        file: "package.json",
        prompts: &[
            "Plan the upgrade to React 19. What breaks?",
            "Start with the codemods, then show me what's left by hand.",
            "The test suite is throwing act() warnings everywhere now.",
        ],
    },
    Topic {
        title: "Refactor the notification pipeline",
        group: Some(0),
        root: Some(ROOT_API),
        subject: "the notification pipeline",
        file: "src/notify/pipeline.rs",
        prompts: &[
            "The notification code has grown three near-identical paths for email, push and webhook. Unify them.",
            "Keep the retry policy per-channel though — webhooks need a much longer backoff.",
            "Add a dead letter queue and a test that proves ordering survives a retry.",
        ],
    },
    Topic {
        title: "Set up CI for the release branch",
        group: Some(0),
        root: Some(ROOT_API),
        subject: "the release CI workflow",
        file: ".github/workflows/release.yml",
        prompts: &[
            "Set up a release workflow that builds, signs and uploads artifacts for macOS and Linux.",
            "Cache the cargo registry between runs — this takes 11 minutes.",
            "Fail the build if the version in Cargo.toml doesn't match the tag.",
        ],
    },
    Topic {
        title: "Port the streaming parser from TypeScript",
        group: Some(1),
        root: Some(ROOT_PI),
        subject: "the streaming parser port",
        file: "crates/pi-core/src/event.rs",
        prompts: &[
            "Port the SSE streaming parser from the TypeScript SDK, keeping the event ordering guarantees.",
            "The partial accumulation differs from upstream when a tool call arrives mid-text. Compare them.",
            "Port the upstream tests for this file too.",
            "What happens on a truncated final chunk?",
        ],
    },
    Topic {
        title: "Wire cbindgen into the build",
        group: Some(1),
        root: Some(ROOT_FORM),
        subject: "the generated C header",
        file: "core/build.rs",
        prompts: &[
            "Generate the C header with cbindgen as part of the build instead of hand-maintaining it.",
            "Add a check that fails if the committed header drifts from the generated one.",
        ],
    },
    Topic {
        title: "Universal binary for the macOS bundle",
        group: Some(1),
        root: Some(ROOT_FORM),
        subject: "the universal static library",
        file: "scripts/build-app.sh",
        prompts: &[
            "The release build should lipo an aarch64 and x86_64 static library into one. Update the script.",
            "Debug builds should stay host-arch only — they take twice as long now.",
            "Verify the exported symbols survive the lipo.",
        ],
    },
    Topic {
        title: "Markdown block tree instead of HTML",
        group: Some(1),
        root: Some(ROOT_FORM),
        subject: "the markdown block tree",
        file: "core/crates/form-core/src/markdown/mod.rs",
        prompts: &[
            "Parse markdown into a typed block tree rather than emitting HTML, so the UI can style it natively.",
            "Unterminated fenced blocks need to degrade gracefully — we render mid-stream.",
            "Add syntax highlighting that returns scope names with ranges, never colors.",
            "Benchmark it at 120 blocks; the budget is 16ms.",
        ],
    },
    Topic {
        title: "Contributing guide and issue templates",
        group: Some(1),
        root: None,
        subject: "the contributing guide",
        file: "CONTRIBUTING.md",
        prompts: &[
            "Write a contributing guide covering the build, the test layout and the review expectations.",
            "Add issue templates for bug reports and feature requests.",
        ],
    },
    Topic {
        title: "Fix the Windows path handling",
        group: Some(1),
        root: Some(ROOT_PI),
        subject: "Windows path handling",
        file: "crates/pi-tools/src/fs.rs",
        prompts: &[
            "Path confinement assumes POSIX separators and breaks on Windows. Make it portable.",
            "UNC paths and drive-relative paths need explicit test cases.",
            "What about case-insensitive volumes on macOS?",
        ],
    },
    Topic {
        title: "Reduce the release binary size",
        group: Some(1),
        root: Some(ROOT_PI),
        subject: "binary size",
        file: "Cargo.toml",
        prompts: &[
            "The release binary is 46 MB. Work out what's in it and get it down.",
            "Try LTO and codegen-units=1 and measure each separately.",
            "Which dependency is pulling in all of that?",
        ],
    },
    Topic {
        title: "Understanding Rust pin and self-referential futures",
        group: Some(2),
        root: None,
        subject: "Pin and self-referential futures",
        file: "notes/pin.md",
        prompts: &[
            "Explain Pin and why self-referential futures need it, with a concrete example I can run.",
            "Show me what actually breaks if I move a self-referential struct.",
            "Where does Unpin fit in?",
        ],
    },
    Topic {
        title: "SQLite FTS5 ranking, in practice",
        group: Some(2),
        root: None,
        subject: "FTS5 ranking",
        file: "notes/fts5.md",
        prompts: &[
            "How does bm25 ranking work in SQLite FTS5, and how do I weight a title column above a body column?",
            "What does snippet() actually return, and how should I hand highlight ranges to a UI?",
            "Is the porter tokenizer the right default for code and prose mixed together?",
        ],
    },
    Topic {
        title: "Swift 6 strict concurrency, gently",
        group: Some(2),
        root: None,
        subject: "Swift 6 strict concurrency",
        file: "notes/swift6.md",
        prompts: &[
            "Walk me through Swift 6 strict concurrency: what actually changed and what I have to do about it.",
            "How do I bridge a C callback that fires on a background thread into an AsyncStream safely?",
            "Is @MainActor on the view type enough, or do I need it on the model too?",
        ],
    },
    Topic {
        title: "How do cache-aware token prices work?",
        group: Some(2),
        root: None,
        subject: "prompt cache pricing",
        file: "notes/pricing.md",
        prompts: &[
            "Explain prompt caching pricing — cache write vs cache read vs plain input — with worked numbers.",
            "At what conversation length does caching start paying for itself?",
        ],
    },
    Topic {
        title: "Reading list for distributed systems",
        group: Some(2),
        root: None,
        subject: "the distributed systems reading list",
        file: "notes/reading.md",
        prompts: &[
            "Put together a reading list for distributed systems, ordered so each paper builds on the last.",
            "I've already read the Dynamo and Raft papers. Skip ahead.",
        ],
    },
    Topic {
        title: "Draft the Q3 engineering update",
        group: None,
        root: None,
        subject: "the Q3 engineering update",
        file: "docs/q3-update.md",
        prompts: &[
            "Draft the quarterly engineering update: what shipped, what slipped, what's next.",
            "Tighten it — it reads like a status report. Lead with the outcome.",
        ],
    },
    Topic {
        title: "Shell one-liner for stale git branches",
        group: None,
        root: None,
        subject: "the stale branch script",
        file: "scripts/stale-branches.sh",
        prompts: &[
            "Give me a shell command that lists git branches with no commits in the last 90 days, sorted by age.",
            "Make it skip anything that's been merged and anything with an open PR.",
        ],
    },
    Topic {
        title: "Explain this stack trace",
        group: None,
        root: Some(ROOT_API),
        subject: "the panic in the worker pool",
        file: "src/worker/pool.rs",
        prompts: &[
            "This panics in production about once a day but I can't reproduce it. What's going on?",
            "So it's a race on shutdown. How do I prove that in a test?",
        ],
    },
    Topic {
        title: "Naming things: the event bus",
        group: None,
        root: Some(ROOT_FORM),
        subject: "event bus naming",
        file: "core/crates/form-core/src/events.rs",
        prompts: &[
            "I keep going back and forth on names for the event bus API. Talk me through the options.",
            "Go with subscribe/unsubscribe then. What should the token type be called?",
        ],
    },
];

/// Reply templates. `{subject}` and `{file}` are substituted per topic.
pub const REPLIES: &[&str] = &[
    "Looked through {file} and the call sites around it.\n\nThe short version: {subject} is doing the right thing for the common case and quietly the wrong thing at the boundaries. Three places need to change:\n\n1. The entry point, which assumes the happy path\n2. The error branch, which swallows the cause\n3. The test, which only covers the happy path\n\nI'll start at the top and work down.",

    "Here's the change to {file}:\n\n```rust\npub fn resolve(&self, key: &str) -> Result<Entry, Error> {\n    let normalized = key.trim().to_ascii_lowercase();\n    self.index\n        .get(&normalized)\n        .cloned()\n        .ok_or_else(|| Error::NotFound(normalized))\n}\n```\n\nNormalizing once at the boundary means every caller below it can assume the invariant instead of re-checking it. The tests pass locally.",

    "Measured before and after on {subject}:\n\n| case | before | after |\n|---|---:|---:|\n| cold start | 412 ms | 118 ms |\n| warm, 1k rows | 84 ms | 11 ms |\n| warm, 50k rows | 3,910 ms | 96 ms |\n\nThe large case was doing a full scan per row. The index turns it into a single seek — the small cases improve for free.",

    "That's a fair point, and I had it backwards.\n\n> the fixture is shared between the two tests\n\nThat's the actual cause: the second test mutates state the first one asserts on, and test order is not guaranteed. A retry would hide it rather than fix it. Giving each test its own fixture is the real change, and it's smaller.",

    "Done. Summary of what changed in {file}:\n\n- [x] Extracted the shared path into one function\n- [x] Kept the per-channel policy as a parameter\n- [x] Added the failing case as a regression test\n- [ ] Docs — I'll do that once the API settles\n\nThe last one is deliberate; the signature may still move.",

    "Three options for {subject}, roughly in order of how much I'd argue for them:\n\n**Do it at read time.** Simplest, no new state, but you pay the aggregation on every request.\n\n**Precompute a daily rollup.** One extra table, a job that fills it, and reads become a single indexed scan. This is what I'd pick.\n\n**Materialized view.** Least code, but the refresh story is awkward and you inherit whatever the database decides about staleness.",

    "```bash\ngit for-each-ref --sort=committerdate refs/heads/ \\\n  --format='%(committerdate:short) %(refname:short)' \\\n  | awk -v cutoff=\"$(date -v-90d +%Y-%m-%d)\" '$1 < cutoff'\n```\n\nThat lists branches whose last commit predates the cutoff, oldest first. On GNU date the cutoff is `date -d '90 days ago' +%F` instead.",

    "The failure is in {file}, and the trace is misleading — the panic surfaces in the worker but the invariant is broken by whoever queued the job.\n\n```\nthread 'worker-3' panicked at src/worker/pool.rs:214:\n  called `Option::unwrap()` on a `None` value\n```\n\nBy the time the worker runs, the sender has already dropped its half of the channel. On a normal shutdown that ordering is guaranteed; under load it isn't.",

    "Short answer: yes, but not for the reason you'd expect.\n\n{subject} matters because the cost is paid once at the boundary and saved on every read after it. The break-even is earlier than most people assume — around the fourth turn in a typical conversation, and immediately if the system prompt is large.\n\nWorked example below, using round numbers so the shape is visible.",

    "Reading {file} now.\n\nThe structure is fine. What I'd change is the naming: `handle` appears four times with four different meanings, and the type that owns the lifetime is called `Manager`, which tells you nothing. Renaming is cheap right now and expensive after the API is public.",

    "Ran the suite twenty times:\n\n```\n$ for i in $(seq 20); do cargo test --test payments -q || break; done\ntest result: ok. 34 passed; 0 failed  (x20)\n```\n\nGreen every time. The old failure rate was about one in five, so twenty clean runs puts the odds of a fluke under one percent.",

    "Here's the sketch in TypeScript, since that's where the original lives:\n\n```typescript\nexport function accumulate(partial: Message, event: StreamEvent): Message {\n  switch (event.type) {\n    case \"text_delta\":\n      return withText(partial, event.contentIndex, event.delta);\n    case \"toolcall_end\":\n      return withToolCall(partial, event.toolCall);\n    default:\n      return partial;\n  }\n}\n```\n\nThe Rust port keeps the same shape; the difference is that `partial` is owned rather than structurally shared, so each step is a move rather than a copy.",
];

/// Short assistant openers used for the first message of a turn that leads with a tool call.
pub const PREAMBLES: &[&str] = &[
    "Let me look at {file} first.",
    "Checking how {subject} is wired up before I change anything.",
    "I'll read the surrounding code, then make the change.",
    "Searching for the other call sites so this doesn't regress.",
];

pub const THINKING: &[&str] = &[
    "The user is asking about {subject}. I should read the relevant file before proposing anything, since the last change in this area moved the boundary.",
    "There are two plausible causes here. The cheap check is to look at the ordering; if that's clean, it's the shared fixture.",
    "This is a design question more than a code question. Worth laying out the options rather than picking one silently.",
];

pub fn fill(template: &str, subject: &str, file: &str) -> String {
    template
        .replace("{subject}", subject)
        .replace("{file}", file)
}
