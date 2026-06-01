#!/usr/bin/env bash
# SonicTerm headless-friendly GUI perf harness.
#
# Unlike `gui_bench.sh`, this script does NOT require `cliclick`. It uses
# `osascript`'s "System Events keystroke" path, which still needs
# Accessibility permission to *deliver* keystrokes — but if that's
# blocked, the script still produces idle-CPU + frame-skip numbers
# (the typing/scroll fields are reported as `null`).
#
# Usage:
#   cargo build --release -p sonicterm-mac
#   scripts/bench_headless_gui.sh
#
# Output: single JSON line on stdout, e.g.
#   {"idle_cpu_pct":0.12,"scroll_cpu_pct":3.4,"frames_skipped":284,"frames_rendered":17}

set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$ROOT/target/release/sonicterm-mac"
LOG="/tmp/sonic-headless-bench.log"

if [ ! -x "$BIN" ]; then
  echo "missing $BIN — run: cargo build --release -p sonicterm-mac" >&2
  exit 1
fi

pkill -9 -f "target/release/sonicterm-mac" 2>/dev/null || true
sleep 0.3
rm -f "$LOG"

# Launch in background with trace logging so we can count skipped frames.
RUST_LOG=sonicterm_shared::render=trace "$BIN" >"$LOG" 2>&1 &
PID=$!
sleep 2

if ! kill -0 "$PID" 2>/dev/null; then
  echo '{"error":"sonicterm-mac failed to launch","log":"'"$LOG"'"}'
  exit 1
fi

sample_cpu_avg() {
  # Sample %cpu every 200ms for $1 seconds, average via awk.
  local secs="$1"
  local end=$(( $(date +%s) + secs ))
  local samples=()
  while [ "$(date +%s)" -lt "$end" ]; do
    local v
    v=$(ps -p "$PID" -o %cpu= 2>/dev/null | tr -d ' ' || echo "")
    [ -n "$v" ] && samples+=("$v")
    sleep 0.2
  done
  printf '%s\n' "${samples[@]:-0}" | awk 'BEGIN{s=0;n=0}{s+=$1;n++}END{if(n==0)print 0;else printf "%.2f",s/n}'
}

# ---- idle sample ---------------------------------------------------------
IDLE=$(sample_cpu_avg 5)

# ---- attempt scroll burst via osascript (graceful if blocked) ------------
SCROLL="null"
TYPING_OK=0
if osascript -e 'tell application "System Events" to keystroke ""' >/dev/null 2>&1; then
  # Bring sonic to the front by PID
  osascript >/dev/null 2>&1 <<EOF || true
tell application "System Events"
  set frontmost of (first process whose unix id is $PID) to true
end tell
EOF
  sleep 0.4
  osascript >/dev/null 2>&1 <<'EOF' || true
tell application "System Events"
  keystroke "seq 1 2000"
  key code 36
end tell
EOF
  TYPING_OK=1
  sleep 0.5
  SCROLL=$(sample_cpu_avg 10)
fi

# ---- parse frame-skip lines from the trace log --------------------------
# The "skipped" field in the trace line is a cumulative counter; strip
# ANSI color codes first since tracing-subscriber emits them on a TTY.
FRAMES_SKIPPED=$(sed 's/\x1b\[[0-9;]*m//g' "$LOG" 2>/dev/null | grep -o "skipped=[0-9]*" | tail -1 | cut -d= -f2 || true)
: "${FRAMES_SKIPPED:=0}"
FRAMES_RENDERED=$(sed 's/\x1b\[[0-9;]*m//g' "$LOG" 2>/dev/null | grep -Ec "rendered frame|frame rendered|drawing frame" || true)
: "${FRAMES_RENDERED:=0}"

kill -9 "$PID" 2>/dev/null || true
wait "$PID" 2>/dev/null || true
set +e

if [ "$TYPING_OK" -eq 0 ]; then
  SCROLL_JSON="null"
else
  SCROLL_JSON="$SCROLL"
fi

printf '{"idle_cpu_pct":%s,"scroll_cpu_pct":%s,"frames_skipped":%s,"frames_rendered":%s,"typing_delivered":%s}\n' \
  "$IDLE" "$SCROLL_JSON" "$FRAMES_SKIPPED" "$FRAMES_RENDERED" \
  "$( [ "$TYPING_OK" -eq 1 ] && echo true || echo false )"
