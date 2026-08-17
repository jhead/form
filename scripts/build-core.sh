#!/usr/bin/env bash
# Build the Rust static library.
#
# Shared by `make` and by the Xcode project's pre-build phase, so the two paths cannot drift.
# Xcode invokes it with a sanitized environment, hence the explicit PATH handling.
#
# Usage: build-core.sh [debug|release]
set -euo pipefail

PROFILE="${1:-debug}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CORE="$ROOT/core"

# Xcode's build environment does not inherit the login shell's PATH.
export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"
if ! command -v cargo >/dev/null 2>&1; then
  echo "error: cargo not found. Install Rust from https://rustup.rs" >&2
  exit 1
fi

# Xcode sets these; when it does, SDK/deployment settings leak into cc invocations and
# confuse cargo's build scripts. Clear them so the core builds the same way everywhere.
unset SDKROOT MACOSX_DEPLOYMENT_TARGET CPATH LIBRARY_PATH 2>/dev/null || true

# Only the static library, never the whole workspace. Building `form-cli` here would invoke
# the linker for an executable, and Xcode's exported linker environment breaks that — while
# a staticlib never links at all. `make cli` builds the CLI on its own terms.
PACKAGE="-p form-ffi"

cd "$CORE"

if [[ "$PROFILE" == "release" ]]; then
  # Universal so a release bundle runs on Intel too. Falls back to host-only when the
  # second target is not installed, rather than failing the build.
  HOST_TARGET="$(rustc -vV | awk '/^host:/ {print $2}')"
  OTHER_TARGET="x86_64-apple-darwin"
  [[ "$HOST_TARGET" == "x86_64-apple-darwin" ]] && OTHER_TARGET="aarch64-apple-darwin"

  cargo build $PACKAGE --release --target "$HOST_TARGET"

  if rustup target list --installed 2>/dev/null | grep -qx "$OTHER_TARGET"; then
    cargo build $PACKAGE --release --target "$OTHER_TARGET"
    mkdir -p "target/release"
    lipo -create \
      "target/$HOST_TARGET/release/libform_ffi.a" \
      "target/$OTHER_TARGET/release/libform_ffi.a" \
      -output "target/release/libform_ffi.a"
    echo "built universal libform_ffi.a ($HOST_TARGET + $OTHER_TARGET)"
  else
    mkdir -p "target/release"
    cp "target/$HOST_TARGET/release/libform_ffi.a" "target/release/libform_ffi.a"
    echo "built libform_ffi.a ($HOST_TARGET only — run 'rustup target add $OTHER_TARGET' for a universal build)"
  fi
else
  cargo build $PACKAGE
  echo "built libform_ffi.a (debug, host arch)"
fi
