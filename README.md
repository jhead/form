# form

A macOS client for a coding agent. The interface is SwiftUI. The portable logic is Rust.

## What is here

`form-core` is the Rust core. It holds session storage, full-text search, settings, the model
catalog, usage analytics, markdown parsing, and context accounting. Swift calls it through a
C ABI. A Windows or Linux client can reuse the same core later.

`pi-rs` is a Rust port of the [pi](https://github.com/earendil-works/pi) agent SDK. It lives
in this repo. It is not wired into the app yet.

The agent harness is not implemented. The app runs against a stub harness. The stub emits the
same events a real harness emits. Every session, token count, and cost figure you see is mock
data. You can run the whole app with no API key.

## Build and run

From the command line:

```bash
make          # rust core, swift package, app/build/form.app
make run      # build, then launch
make test     # rust tests and swift tests
make lint     # cargo fmt --check and clippy -D warnings
make cli      # stream a stub run to the terminal, no Swift involved
```

In Xcode:

```bash
make xcode    # generate form.xcodeproj, open it, then press Cmd-R
```

The Xcode project is generated from `project.yml`. It is not committed. A pre-build phase
builds the Rust core first. Both build paths call the same script.

Requires Xcode 16, Swift 6, and a Rust toolchain. The Xcode path also needs `xcodegen`.

## Layout

```
docs/PRD.md      the product spec
docs/specs/      one spec per area. 00-protocol.md is the boundary contract
core/            cargo workspace: form-core, form-ffi, form-cli
app/             swiftpm package: FormCore, FormDesign, FormMarkdown, FormUI, form
pi-rs/           the pi SDK port, not yet integrated
scripts/         app bundling
```

## How the two halves talk

```
form.app  ->  9 C functions carrying JSON  ->  libform_ffi.a  ->  form-core
```

Only JSON crosses the boundary. There are no shared structs. There are no pointers into Rust
memory.

Queries are synchronous reads. Commands are asynchronous. Every command result arrives as an
event, because a Swift caller cannot hold a Rust future. Cancellation is an explicit signal.

Keeping the payloads serialized also keeps a subprocess transport available. Swift talks to a
`CoreTransport` protocol. Today there is one implementation.

See [docs/PRD.md](docs/PRD.md) section 4 for the full reasoning.

## Wiring up pi-rs

`form-core` defines its own copies of the transcript types. They match `pi-core` field for
field. A test proves it. `core/crates/form-core/tests/pi_compat.rs` sends every type and every
streaming event through `pi-core` and fails on any renamed or dropped field. It checks both
directions.

`pi-core` is a dev dependency. No shipping code links it.

[docs/specs/16-pi-integration.md](docs/specs/16-pi-integration.md) has the plan. Read the last
section before starting. Real tool execution changes the in-process tradeoff.

## Tests

```
223 Rust tests
301 Swift tests
```

`form-cli protocol-dump` writes one JSON fixture per protocol variant. The Swift test target
decodes all of them. That is what catches drift between the two languages.
