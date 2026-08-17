# form — working agreement

Read [`docs/PRD.md`](docs/PRD.md), then your workstream's spec in [`docs/specs/`](docs/specs).
[`docs/specs/00-protocol.md`](docs/specs/00-protocol.md) is frozen and binding on everyone.
[`docs/specs/15-build-and-conventions.md`](docs/specs/15-build-and-conventions.md) has the
build commands and code conventions.

## The one rule that matters

**Directory ownership is exclusive.** Fourteen workstreams run in parallel in this tree. Edit
only the files your spec assigns you.

Files nobody may edit without asking the orchestrator:

- `core/Cargo.toml`, `core/crates/*/Cargo.toml` — every dependency you need is already
  declared. If one is missing, say so in your report.
- `core/crates/form-core/src/protocol/**` — the frozen contract.
- `core/crates/form-core/src/lib.rs`, `core/crates/form-core/src/core.rs` — the façade and
  its routing. If your module needs to be reachable from `query`/`dispatch`, implement your
  module's function and **report the one-line wiring change you need**; do not make it.
- `app/Package.swift` — all targets are already declared.
- `Makefile`, `scripts/`, `docs/`.

## Before you report done

```bash
make lint      # cargo fmt --check + clippy -D warnings
make test      # rust tests + swift tests
make           # builds form.app
```

All three must be clean. "Done" with a failing test is not done.

## What a good report looks like

- What you built, in a few lines.
- The exact commands you ran and their results.
- Anything you did **not** finish, and why.
- Any assumption you made that someone else might contradict.
- Any change you need in a file you do not own.

Do not report success you have not verified. If you got blocked, say where.

## Conventions in brief

**Rust** — follow [`pi-rs/AGENTS.md`](../pi-rs/AGENTS.md) for anything on the boundary: no
lifetimes or generics in public signatures, owned `'static + Send + Sync` types,
`#[serde(rename_all = "camelCase")]`, flat error enums with `code()`, never leak `anyhow`.

**Swift** — Swift 6 strict concurrency, `@Observable` not `ObservableObject`, `@MainActor` on
UI types, no force-unwraps outside tests, a `#Preview` for every view that works against
`MockTransport` with no Rust build, and **no color, font, spacing, radius or duration literal
outside `FormDesign`**.

**Both** — match the surrounding code's density and idiom. Comment the *why* of non-obvious
decisions; do not narrate the obvious. Tests are part of done.

## The product, in one line

`form` — always lowercase, wordmark in a serif face. A native macOS coding-agent client whose
portable logic lives in Rust so Windows and Linux are a UI port later. The harness is a stub;
the real one is being ported in parallel in [`pi-rs`](../pi-rs).
