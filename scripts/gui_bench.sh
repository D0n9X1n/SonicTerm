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

type_str() {
  # Use cliclick if available (no Accessibility prompt for keystrokes),
  # otherwise fall back to osascript.
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
    IDLE=$(sample_cpu 3)
    echo "  \"idle_cpu_pct_3s\": $IDLE," >&2
    ;;
esac

case "$SCENARIO" in
  typing|all)
    frontmost
    sleep 0.3
    T0=$(date +%s%N)
    for i in $(seq 1 60); do
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
