#!/usr/bin/env bash
# Driver for testing/cases.toml on macOS.
# Usage: testing/workflows/mac.sh [--case <id>] [--all] [--build]
# Env:   CASE_ID=<id>   run a single case (alternative to --case)
#
# Requires: yq, tesseract, python3 + Pillow, osascript (built-in), screencapture (built-in)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"

# ------------------------------------------------------------------
# Tool checks
# ------------------------------------------------------------------
for tool in yq tesseract python3 osascript screencapture; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "FATAL: missing required tool: $tool" >&2
    echo "Install: brew install yq tesseract && pip3 install Pillow" >&2
    exit 2
  fi
done
if ! python3 -c "from PIL import Image" 2>/dev/null; then
  echo "FATAL: Pillow not installed. Run: pip3 install Pillow" >&2
  exit 2
fi

# ------------------------------------------------------------------
# Pre-flight: refuse to run if another terminal emulator is running.
# AppleScript keystrokes are routed to whatever app is frontmost at the
# moment of dispatch; if any other terminal is around the harness will
# leak keystrokes into it (Cmd+T opens stray tabs, `clear`/`echo` lines
# appear in chat apps, etc.) and screencap pixels come from the wrong
# window. See issue #464.
# ------------------------------------------------------------------
OTHER_TERMS=$(pgrep -lf 'WezTerm|Terminal\.app|iTerm|kitty|alacritty' 2>/dev/null | grep -v 'pgrep' || true)
if [[ -n "$OTHER_TERMS" ]]; then
  echo "FATAL: another terminal emulator is running — quit it before running the harness." >&2
  echo "Detected:" >&2
  echo "$OTHER_TERMS" >&2
  echo "(focus tracking is not perfect; stray keystrokes would leak. See issue #464.)" >&2
  exit 2
fi

# ------------------------------------------------------------------
# Arg parsing
# ------------------------------------------------------------------
DO_BUILD=0
FILTER="${CASE_ID:-all}"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --build) DO_BUILD=1; shift ;;
    --all)   FILTER=all; shift ;;
    --case)  FILTER="$2"; shift 2 ;;
    *)       echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done

SHA="$(git rev-parse --short HEAD 2>/dev/null || echo nogit)"
OUT="testing/results/mac-$SHA"
mkdir -p "$OUT"

# ------------------------------------------------------------------
# Build
# ------------------------------------------------------------------
if [[ $DO_BUILD -eq 1 || ! -x target/release/sonicterm-mac ]]; then
  echo "[build] cargo build --release -p sonicterm-mac"
  cargo build --release -p sonicterm-mac
fi

# ------------------------------------------------------------------
# Enumerate matching ids (using python tomllib — no yq toml support)
# ------------------------------------------------------------------
IDS=()
while IFS= read -r line; do
  [[ -n "$line" ]] && IDS+=("$line")
done < <(python3 - "$FILTER" <<'PY'
import sys, tomllib
flt = sys.argv[1]
with open('testing/cases.toml','rb') as f:
    d = tomllib.load(f)
for c in d['case']:
    if 'mac' not in c.get('applies_to', []):
        continue
    if flt != 'all' and c['id'] != flt:
        continue
    print(c['id'])
PY
)

if [[ ${#IDS[@]} -eq 0 ]]; then
  echo "no matching cases for filter='$FILTER'" >&2
  exit 1
fi

echo "[plan] ${#IDS[@]} case(s) to run; results -> $OUT"

DRIVER_DIR="$(dirname "$0")"
PASS=0
FAIL=0
SKIP=0

for id in "${IDS[@]}"; do
  echo
  echo "=== $id ==="
  if bash "$DRIVER_DIR/run_case.sh" "$id" "$OUT"; then
    PASS=$((PASS+1))
  else
    rc=$?
    if [[ $rc -eq 77 ]]; then
      SKIP=$((SKIP+1))
    else
      FAIL=$((FAIL+1))
    fi
  fi
done

echo
echo "[done] pass=$PASS fail=$FAIL skip=$SKIP / total=${#IDS[@]}"

bash "$DRIVER_DIR/summarize.sh" "$OUT" > "$OUT/report.md"
cat "$OUT/report.md"

[[ $FAIL -eq 0 ]]
