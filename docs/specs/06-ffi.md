# Spec 06 — FFI layer and CLI (`form-ffi`, `form-cli`)

> **Workstream W6.** Owns `core/crates/form-ffi/`, `core/crates/form-cli/`,
> `core/include/form.h` (generated), and the protocol fixture tests. Implements
> [spec 00](./00-protocol.md) §2 and §7.

## 1. `form-ffi`

Crate type: `staticlib` + `rlib`. The static library is what the Swift package links.

### Surface

Exactly the nine functions in spec 00 §2. Rules:

- Every `extern "C" fn` body is wrapped in `std::panic::catch_unwind`. A panic becomes an
  error result or a logged no-op — **a panic must never unwind into Swift**, which is
  undefined behaviour.
- `char*` returns are freshly-allocated NUL-terminated UTF-8 from `CString::into_raw`, freed
  only by `form_string_free`. Never return a pointer into a Rust-owned buffer.
- `FormCore*` is an opaque `Box<CoreHandle>` pointer. Null and use-after-free are guarded by
  a magic-number field checked on entry; a bad handle returns an error, not a crash.
- `form_core_query` and `form_core_dispatch` never return null; on internal failure they
  return a serialized `{"ok":false,"error":{…}}`.
- `form_last_error()` returns a thread-local last error for the two calls that use out-params
  (`form_core_new`).

### Runtime and threading

- The handle owns a multi-threaded `tokio::runtime::Runtime`.
- Events funnel through an unbounded MPSC into **one dispatcher thread** that serializes each
  event and invokes the registered callback. Delivery is ordered and never concurrent
  (spec 00 §7).
- `form_core_free` signals shutdown, joins the dispatcher, and drops the runtime **without
  blocking indefinitely** — 2 s timeout, then detach. Freeing while a run is streaming must
  not deadlock; there is a test for exactly this.
- Callback registration is `Arc<ArcSwap<...>>`-style or mutex-guarded such that
  `unsubscribe` cannot race a delivery in flight. After `unsubscribe` returns, the callback
  is guaranteed not to be invoked again.

### Header generation

`cbindgen` via `build.rs` or a `make headers` target, output `core/include/form.h`, with
`FORM_ABI_VERSION` emitted as a `#define`. The header is committed so the Swift package
builds without running cbindgen. A test asserts the committed header matches a fresh
generation (fails the build on drift).

## 2. `form-cli`

A small binary that drives the core with no Swift involved. It is the end-to-end proof and
the fastest debugging loop for every core workstream.

```
form-cli seed                     # create a store with mock data
form-cli sessions                 # list
form-cli chat <session-id> "..."  # dispatch a prompt, print events as they arrive
form-cli stats --range d30        # pretty-print UsageStats
form-cli protocol-dump <dir>      # one instance of every command/query/event as JSON
form-cli bench                    # stats + markdown budgets
```

`chat` must render the live stream to the terminal so cadence and ordering are directly
observable. It links `form-ffi` **through the C API**, not through `form-core` directly —
that is what makes it an FFI test rather than a library test.

## 3. Protocol fixtures (spec 00 §8)

`protocol-dump` writes `core/tests/fixtures/protocol/{commands,queries,events}/*.json`,
one file per variant, using representative non-default values (no all-zeros structs — those
hide field-name typos). The Swift test target decodes every file into the Swift type,
re-encodes, and compares normalized JSON. **This is the tripwire for Swift/Rust drift; it
must be wired into `make test` from day one.**

## 4. Done when

- `cargo test -p form-ffi` covers: ABI version match, new/free, free while streaming, bad
  handle, double free rejection, panic containment, subscribe/unsubscribe race, event
  ordering under two concurrent sessions, and string-free correctness under ASAN if
  available.
- `form-cli chat` streams a stub response with visible cadence.
- `form-cli protocol-dump` produces a fixture for **every** variant in spec 00 — a test
  enumerates the variants and fails if one is missing.
- Header drift test passes.
