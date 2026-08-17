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

cat > "$BUNDLE/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>                  <string>form</string>
    <key>CFBundleDisplayName</key>           <string>form</string>
    <key>CFBundleExecutable</key>            <string>form</string>
    <key>CFBundleIdentifier</key>            <string>dev.jhead.form</string>
    <key>CFBundlePackageType</key>           <string>APPL</string>
    <key>CFBundleShortVersionString</key>    <string>0.1.0</string>
    <key>CFBundleVersion</key>               <string>1</string>
    <key>LSMinimumSystemVersion</key>        <string>14.0</string>
    <key>NSHighResolutionCapable</key>       <true/>
    <key>NSSupportsAutomaticGraphicsSwitching</key> <true/>
    <!-- Unsandboxed by design for the MVP: the agent needs arbitrary workspace roots. -->
    <key>NSPrincipalClass</key>              <string>NSApplication</string>
</dict>
</plist>
PLIST

# Ad-hoc signature so the app launches locally without a developer certificate.
codesign --force --sign - "$BUNDLE" 2>/dev/null || echo "note: ad-hoc signing skipped"

echo "built $BUNDLE"
