# form

A native macOS client for a coding agent. SwiftUI on top, Rust underneath.

Everything portable — session storage, search, settings, the provider catalog, usage
analytics, markdown parsing, context accounting — lives in `form-core` behind a narrow C ABI,
so Windows and Linux clients are a UI port rather than a rewrite.

The agent harness is **not** implemented here. It is being ported in parallel in
[`pi-rs`](../pi-rs). `form` ships against a stub harness that emits the same event protocol,
so the entire UX runs with no LLM backend and no API keys.

## Build

```bash
make          # rust core + swift package + app/build/form.app
make run      # build and launch
make test     # rust tests + swift tests
make lint     # cargo fmt --check + clippy -D warnings
make cli      # stream a stub run to the terminal, no Swift involved
```

Requires Xcode 16+, Swift 6, and a Rust toolchain.

## Layout

```
docs/PRD.md          the product spec
docs/specs/          one spec per workstream; 00-protocol.md is the boundary contract
core/                Cargo workspace: form-core, form-ffi, form-cli
app/                 SwiftPM package: FormCore, FormDesign, FormMarkdown, FormUI, form
scripts/             app bundling
```

## Architecture

```
form.app  ──JSON over 9 C functions──▶  libform_ffi.a  ──▶  form-core
```

Commands are asynchronous and every outcome arrives as an event, because a Swift caller
cannot hold or drop a Rust future. Queries are synchronous reads. Nothing crosses the
boundary except JSON, which is what keeps a subprocess transport available later without
touching the app layer.

See [`docs/PRD.md`](docs/PRD.md) §4 for the full rationale.
