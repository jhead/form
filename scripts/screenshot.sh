#!/usr/bin/env bash
# Capture a screenshot of the app for the README.
#
# Runs against a throwaway data directory seeded with the demo corpus, and the stub harness,
# so the shot needs no API key, no network, and never touches the real database. The corpus is
# deterministic, so re-running produces the same content.
#
# Usage: scripts/screenshot.sh [chat|home] [output.png]
set -euo pipefail

VIEW="${1:-chat}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${2:-$ROOT/docs/images/$VIEW.png}"
APP="$ROOT/app/build/form.app"
DATA_DIR="$(mktemp -d "${TMPDIR:-/tmp}/form-shot.XXXXXX")"

cleanup() {
  pkill -f "form.app/Contents/MacOS/form" 2>/dev/null || true
  rm -rf "$DATA_DIR"
}
trap cleanup EXIT

# A locked screen has no window server session: the app runs but never renders, and a capture
# returns the desktop. Say so rather than writing a picture of the wallpaper.
#
# Two traps in this one check, both of which silently disabled it. `ioreg` lives in /usr/sbin,
# which a script's PATH may not include. And `grep -q` exits on its first match, which sends
# SIGPIPE to `ioreg` — under `pipefail` the pipeline then reports 141, so a successful *match*
# read as "not locked". `grep -c` consumes all of the input, so it cannot happen.
LOCKED=$(/usr/sbin/ioreg -n Root -d1 -a 2>/dev/null | grep -c CGSSessionScreenIsLocked || true)
if [ "${LOCKED:-0}" -gt 0 ]; then
  echo "error: the screen is locked, so no window will render. Unlock it and re-run." >&2
  exit 1
fi

echo "==> building"
make -C "$ROOT" >/dev/null

echo "==> launching against a seeded throwaway store"
pkill -f "form.app/Contents/MacOS/form" 2>/dev/null || true
sleep 1
FORM_DATA_DIR="$DATA_DIR" FORM_SEED_MOCK_DATA=1 FORM_HARNESS=stub open -n "$APP"

# Wait for a window rather than sleeping a fixed amount: seeding 120 days of corpus takes
# longer on a cold cache than a warm one.
BOUNDS=""
for _ in $(seq 1 40); do
  BOUNDS=$(osascript <<'EOF' 2>/dev/null || true
tell application "System Events"
  tell (first process whose name is "form")
    set frontmost to true
    set w to first window
    set p to position of w
    set s to size of w
    return (item 1 of p as text) & "," & (item 2 of p as text) & "," & (item 1 of s as text) & "," & (item 2 of s as text)
  end tell
end tell
EOF
)
  [ -n "$BOUNDS" ] && break
  sleep 1
done

if [ -z "$BOUNDS" ]; then
  if ! pgrep -f "form.app/Contents/MacOS/form" >/dev/null; then
    echo "error: the app exited during launch. Run it directly to see why: open $APP" >&2
  else
    echo "error: the app never showed a window. Grant Accessibility permission to your" >&2
    echo "       terminal, or check that the screen is unlocked." >&2
  fi
  exit 1
fi

if [ "$VIEW" = "home" ]; then
  # The sidebar's Home segment; the app opens on the last route, which is Code by default.
  osascript -e 'tell application "System Events" to keystroke "h" using {command down, shift down}' || true
  sleep 2
fi

# Let charts and the transcript settle: both animate in.
sleep 3

mkdir -p "$(dirname "$OUT")"
echo "==> capturing $BOUNDS"
screencapture -x -o -R "$BOUNDS" "$OUT"

# Downscale for the README. The window is captured at Retina density, which is twice the size
# anyone needs in a document.
WIDTH=$(sips -g pixelWidth "$OUT" | awk '/pixelWidth/ {print $2}')
if [ "${WIDTH:-0}" -gt 1600 ]; then
  sips -Z 1600 "$OUT" >/dev/null
fi

echo "wrote $OUT ($(sips -g pixelWidth -g pixelHeight "$OUT" | awk '/pixel/ {printf "%s ", $2}')px)"
