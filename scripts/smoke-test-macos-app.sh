#!/usr/bin/env bash
# Open the app from its disk image the way a user does (`open`: LaunchServices and
# launchd's environment, not the CI shell's) and check that it is still running and
# has opened a window: catches a crash, an early exit or a hang at startup.
#
# Usage: scripts/smoke-test-macos-app.sh <dmg> <app name> <executable> [crash log]
set -euo pipefail

dmg="$1"
name="$2"
exe="$3"
crash_log="${4:-}"
WAIT=30

# Real path: the process runs from /private/var/..., not the /var/... symlink.
work=$(cd "$(mktemp -d)" && pwd -P)
mnt="$work/mnt"
hdiutil attach -nobrowse -readonly -mountpoint "$mnt" "$dmg" > /dev/null
cp -R "$mnt/$name.app" "$work/"
hdiutil detach "$mnt" > /dev/null
app="$work/$name.app"
bin="$app/Contents/MacOS/$exe"

# Number of on-screen windows owned by a process (no permission needed for that).
cat > "$work/windows.swift" <<'SWIFT'
import CoreGraphics
let pid = Int(CommandLine.arguments[1])!
let windows = CGWindowListCopyWindowInfo(.optionOnScreenOnly, kCGNullWindowID) as? [[String: Any]] ?? []
print(windows.filter { ($0[kCGWindowOwnerPID as String] as? Int) == pid }.count)
SWIFT
swiftc -O -o "$work/windows" "$work/windows.swift"

fail() {
  echo "::error::$1"
  [ -n "$crash_log" ] && [ -f "$crash_log" ] && cat "$crash_log"
  for report in $(ls -t "$HOME/Library/Logs/DiagnosticReports" 2> /dev/null | grep -i "$exe" | head -2); do
    head -60 "$HOME/Library/Logs/DiagnosticReports/$report"
  done
  exit 1
}

open "$app"
pid=""
windows=0
for _ in $(seq "$WAIT"); do
  sleep 1
  pid=$(pgrep -f "^$bin" | head -1 || true)
  if [ -z "$pid" ]; then
    continue
  fi
  windows=$("$work/windows" "$pid")
  [ "$windows" -gt 0 ] && break
done

[ -n "$pid" ] || fail "$name is not running ${WAIT}s after being opened"
[ "$windows" -gt 0 ] || fail "$name runs (pid $pid) but opened no window in ${WAIT}s"
echo "OK: $name runs (pid $pid) with $windows window(s)"
kill "$pid"
