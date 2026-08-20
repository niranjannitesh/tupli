#!/bin/bash
#
# Start Tupli the way a user starts it.
#
# `cargo run` produces a process with no bundle around it, and macOS decides an
# application's Dock icon, its name in the menu bar and its Keychain identity
# from the bundle rather than from the executable. Running the bare binary is
# therefore not a lighter way of doing the same thing: it is a different
# application, a nameless one with the generic icon. This script is the way to
# launch a development build so that what appears on screen is what ships.
#
# Usage:
#   scripts/run.sh [--release] [--channel development|preview|production] [-- ARGS...]
#
# Anything after `--` is passed to the app, and the environment is inherited,
# so TUPLI_* knobs work exactly as they do when the binary is run directly.

set -euo pipefail

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
channel=${TUPLI_CHANNEL:-development}
bundle_args=(--debug)
app_args=()

while [ $# -gt 0 ]; do
  case $1 in
    --release) bundle_args=(--release); shift ;;
    --debug) bundle_args=(--debug); shift ;;
    --channel) channel=${2:?--channel needs a value}; shift 2 ;;
    --channel=*) channel=${1#*=}; shift ;;
    --) shift; app_args=("$@"); break ;;
    -h|--help) sed -n '2,18p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "run: unknown argument: $1" >&2; exit 2 ;;
  esac
done

app_path=$("$root/scripts/bundle.sh" --channel "$channel" "${bundle_args[@]}" \
  | tee /dev/stderr | sed -n 's/^bundle: \(.*\.app\)$/\1/p' | tail -1)
[ -n "$app_path" ] || { echo "run: bundle.sh did not report an app" >&2; exit 1; }

# The bundle was just replaced under a running instance, so `open` would only
# raise the old process and the new build would never be seen. Ask it to quit
# by bundle identifier rather than killing by name: two channels can be running
# side by side and only one of them is being replaced.
identifier=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' \
  "$app_path/Contents/Info.plist" 2>/dev/null || true)
if [ -n "$identifier" ]; then
  osascript -e "tell application id \"$identifier\" to quit" >/dev/null 2>&1 || true
  for _ in 1 2 3 4 5 6 7 8 9 10; do
    pgrep -qf "$app_path/Contents/MacOS/" || break
    sleep 0.2
  done
  pkill -f "$app_path/Contents/MacOS/" >/dev/null 2>&1 || true
fi

# A bundle launched by Launch Services has no terminal to write to, so its
# output goes to a file next to the bundle rather than nowhere. `open` refuses
# a pipe here (-10810), which rules out forwarding it to whatever started this.
log="$root/target/bundle/$channel.log"
: > "$log"

# `-n` because the point of running this is to see *this* build: without it
# `open` would raise an older copy of the same application and report success.
open -n "$app_path" --stdout "$log" --stderr "$log" \
  ${app_args[0]+--args "${app_args[@]}"}
echo "run: logging to $log"
