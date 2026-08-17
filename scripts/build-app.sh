#!/usr/bin/env bash
# Assemble form.app from the SwiftPM executable.
#
# No .xcodeproj is committed — SwiftPM is the build, and this script is the bundling step
# Xcode would otherwise own. Keeping it a script is what lets every agent (and CI) produce a
# runnable app headlessly.
set -euo pipefail

PROFILE="${1:-debug}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP_DIR="$ROOT/app"
BIN="$APP_DIR/.build/$PROFILE/form"
BUNDLE="$APP_DIR/build/form.app"

if [[ ! -x "$BIN" ]]; then
  echo "missing $BIN — run make first" >&2
  exit 1
fi

rm -rf "$BUNDLE"
mkdir -p "$BUNDLE/Contents/MacOS" "$BUNDLE/Contents/Resources"

cp "$BIN" "$BUNDLE/Contents/MacOS/form"

# One Info.plist, shared with the Xcode target, so the two build paths produce the same
# bundle metadata.
cp "$APP_DIR/Resources/Info.plist" "$BUNDLE/Contents/Info.plist"

# Ad-hoc signature so the app launches locally without a developer certificate.
codesign --force --sign - "$BUNDLE" 2>/dev/null || echo "note: ad-hoc signing skipped"

echo "built $BUNDLE"
