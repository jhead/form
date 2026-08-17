# form

A macOS client for a coding agent in Swift and Rust.

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
