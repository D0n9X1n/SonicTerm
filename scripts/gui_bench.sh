#!/usr/bin/env bash
# SonicTerm GUI performance harness — drives a real built .app via osascript
# keystrokes and samples its CPU / RSS over time. Outputs JSON-ish summary.
#
# Requires (one-time): grant Terminal.app or whoever is running this
# Accessibility permission in System Settings → Privacy & Security →
# Accessibility, so `osascript keystroke ...` can post to other apps.
#
# Usage:
#   ./scripts/gui_bench.sh                    # default scenario
#   ./scripts/gui_bench.sh idle               # 5s of idle CPU sampling
#   ./scripts/gui_bench.sh typing             # 200 keys, measures CPU + time
#   ./scripts/gui_bench.sh scroll             # huge `seq 5000` burst
#   ./scripts/gui_bench.sh all                # all of the above (default)
#
# Each scenario kills any prior SonicTerm, opens a fresh bundle from the
# repo's `target/release/sonicterm-mac`, focuses it, runs the scenario,
# samples, kills it.

set -euo pipefail
SCENARIO="${1:-all}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$ROOT/target/release/sonicterm-mac"
APP="/tmp/sonic-bench/SonicTerm.app"
BUNDLE_ID="com.d0n9x1n.sonicterm.bench"

if [ ! -x "$BIN" ]; then
  echo "missing $BIN — run: cargo build --release -p sonicterm-mac" >&2
  exit 1
fi

# ----- Guard: refuse to run if a competing terminal is foreground-able. -----
# osascript keystrokes go to whatever app is frontmost — if WezTerm/iTerm/etc.
# are alive AND sonicterm-mac drops focus mid-run, keystrokes leak into them.
# Override with SONICTERM_HARNESS_ALLOW_OTHER_TERMS=1. See issues #464, #473.
COMPETITOR_RE='^(WezTerm|wezterm-gui|Terminal|iTerm|iTerm2|kitty|alacritty|ghostty|Hyper|Warp|tabby|rio)$'
if [[ "${SONICTERM_HARNESS_ALLOW_OTHER_TERMS:-0}" != "1" ]]; then
  hits=$(ps -A -o pid=,comm= 2>/dev/null | awk -v re="$COMPETITOR_RE" '{
    n=split($2,a,"/"); name=a[n]
    if (name ~ re) print $1" "name
  }')
  if [[ -n "$hits" ]]; then
    echo "SKIP: competing terminal detected — keystrokes would leak." >&2
    echo "Quit them, or set SONICTERM_HARNESS_ALLOW_OTHER_TERMS=1 to override." >&2
    echo "$hits" >&2
    exit 77
  fi
fi

# Build a tiny bundle so it gets a Dock icon + can be focused by osascript.
rm -rf /tmp/sonic-bench
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/sonicterm-mac"
cp -r "$ROOT/assets" "$APP/Contents/Resources/" 2>/dev/null || true
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleName</key><string>SonicTermBench</string>
<key>CFBundleIdentifier</key><string>$BUNDLE_ID</string>
<key>CFBundleVersion</key><string>0.0.0</string>
<key>CFBundleShortVersionString</key><string>0.0.0</string>
<key>CFBundleExecutable</key><string>sonicterm-mac</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>LSMinimumSystemVersion</key><string>14.0</string>
<key>NSHighResolutionCapable</key><true/>
<key>NSPrincipalClass</key><string>NSApplication</string>
</dict></plist>
PLIST

pkill -9 -f "SonicTerm" 2>/dev/null || true
sleep 0.3

open "$APP"
sleep 1.5
PID=$(pgrep -f "sonic-bench/SonicTerm.app/Contents/MacOS/sonicterm-mac" | head -1)
if [ -z "$PID" ]; then
  echo '{"error":"sonic failed to launch"}'
  exit 1
fi
echo "{\"pid\":$PID,\"scenario\":\"$SCENARIO\"," >&2

sample_cpu() {
  # Average %CPU over `$1` seconds using 100ms granularity.
  local secs="$1" total=0 n=0
  local stop=$(($(date +%s) + secs))
  while [ "$(date +%s)" -lt "$stop" ]; do
    local cpu
    cpu=$(ps -p "$PID" -o %cpu= 2>/dev/null | tr -d ' ' || echo 0)
    total=$(echo "$total + $cpu" | bc -l 2>/dev/null || echo 0)
    n=$((n + 1))
    sleep 0.1
  done
  echo "scale=2; $total / $n" | bc -l
}

frontmost() {
  osascript >/dev/null 2>&1 <<EOF
tell application "System Events"
  set frontmost of (first process whose bundle identifier is "$BUNDLE_ID") to true
end tell
EOF
}

# ----- Pre-keystroke focus gate (mirrors testing/workflows/run_case.sh) -----
verify_front() {
  local front
  front=$(osascript -e 'tell application "System Events" to name of first process whose frontmost is true' 2>/dev/null || echo "")
  [[ "$front" == "sonicterm-mac" ]]
}
ensure_front_or_skip() {
  for try in 1 2 3 4 5; do
    verify_front && return 0
    frontmost
    sleep 0.25
  done
  echo "SKIP: cannot keep sonicterm-mac frontmost — keystrokes would leak." >&2
  pkill -9 -f "SonicTerm" 2>/dev/null || true
  exit 77
}

type_str() {
  # Use cliclick if available (no Accessibility prompt for keystrokes),
  # otherwise fall back to osascript.
  ensure_front_or_skip
  if command -v cliclick >/dev/null 2>&1; then
    cliclick -w 0 "t:$1" "kp:return" >/dev/null
  else
    osascript >/dev/null 2>&1 <<EOF
tell application "System Events" to keystroke "$1"
tell application "System Events" to key code 36
EOF
  fi
}

case "$SCENARIO" in
  idle|all)
    frontmost
    sleep 0.3
    ensure_front_or_skip
    IDLE=$(sample_cpu 3)
    echo "  \"idle_cpu_pct_3s\": $IDLE," >&2
    ;;
esac

case "$SCENARIO" in
  typing|all)
    frontmost
    sleep 0.3
    ensure_front_or_skip
    T0=$(date +%s%N)
    for i in $(seq 1 60); do
      ensure_front_or_skip
      if command -v cliclick >/dev/null 2>&1; then
        cliclick -w 0 "t:a" >/dev/null
      else
        osascript -e 'tell application "System Events" to keystroke "a"' >/dev/null 2>&1
      fi
      sleep 0.01
    done
    T1=$(date +%s%N)
    MS_PER_KEY=$(echo "scale=2; ($T1 - $T0) / 60 / 1000000" | bc -l)
    echo "  \"typing_60_keys_total_ms\": $(echo "scale=1; ($T1 - $T0)/1000000" | bc -l)," >&2
    echo "  \"typing_avg_ms_per_key\": $MS_PER_KEY," >&2
    TYPE_CPU=$(sample_cpu 1)
    echo "  \"typing_cpu_pct_after\": $TYPE_CPU," >&2
    ensure_front_or_skip
    if command -v cliclick >/dev/null 2>&1; then
      cliclick -w 0 "kp:return" >/dev/null
    else
      osascript -e 'tell application "System Events" to key code 36' >/dev/null 2>&1
    fi
    ;;
esac

case "$SCENARIO" in
  scroll|all)
    frontmost
    sleep 0.3
    ensure_front_or_skip
    # Force a heavy burst: 5000 lines via `yes | head -5000`
    type_str 'yes hello | head -5000'
    sleep 0.4
    SCROLL_CPU=$(sample_cpu 3)
    echo "  \"scroll_cpu_pct_3s\": $SCROLL_CPU," >&2
    ;;
esac

# Final RSS sample
RSS=$(ps -p "$PID" -o rss= | tr -d ' ')
echo "  \"final_rss_kb\": $RSS" >&2
echo "}" >&2

# Cleanup
pkill -9 -f "SonicTerm" 2>/dev/null || true
