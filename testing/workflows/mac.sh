#!/usr/bin/env bash
# Driver for testing/cases.toml on macOS.
# Usage: testing/workflows/mac.sh [--case <id>] [--all] [--build]
# Env:   CASE_ID=<id>   run a single case (alternative to --case)
#
# Requires: yq, tesseract, python3 + Pillow, osascript (built-in), screencapture (built-in)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"

# #548: reap any leaked sonicterm-mac processes on exit (catches subshell + early-skip leaks).
# Path-scoped per #464 — only kills this repo's release binary, not user's dev builds elsewhere.
REPO_ROOT="${REPO_ROOT:-$(git rev-parse --show-toplevel)}"
SONIC_BIN_PATH="$REPO_ROOT/target/release/sonicterm-mac"
cleanup_sonic_leaks() {
  pkill -9 -f "$SONIC_BIN_PATH" 2>/dev/null || true
}
trap cleanup_sonic_leaks EXIT INT TERM

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
# Guard 1 — pre-flight: refuse to start if a competing terminal is
# running. macOS UI keystrokes go to whatever app is frontmost at the
# moment; if a competitor (WezTerm, iTerm, kitty, ...) is alive AND
# sonicterm-mac drops focus mid-case, our `osascript ... keystroke`
# calls land in that competitor instead. Documented in issue #464.
# Override with SONICTERM_HARNESS_ALLOW_OTHER_TERMS=1 when running the
# harness FROM one of these terminals during dev (the dev's source
# terminal sits behind sonicterm-mac and won't steal focus unless
# something more dramatic goes wrong, in which case Guard 4 catches it).
# ------------------------------------------------------------------
# Match the EXECUTABLE basename only (not full cmdline), to avoid
# false-positives like `rio` matching `--no-periodic-tasks` in Teams
# crashpad cmdlines. Use BSD `ps` then awk over the binary name.
COMPETITOR_RE='^(WezTerm|wezterm-gui|Terminal|iTerm|iTerm2|kitty|alacritty|ghostty|Hyper|Warp|tabby|rio)$'
if [[ "${SONICTERM_HARNESS_ALLOW_OTHER_TERMS:-0}" != "1" ]]; then
  hits=$(ps -A -o pid=,comm= 2>/dev/null | awk -v re="$COMPETITOR_RE" '{
    # comm field can be a path (e.g. /Applications/WezTerm.app/Contents/MacOS/wezterm-gui)
    n=split($2,a,"/"); name=a[n]
    if (name ~ re) print $1" "name
  }')
  if [[ -n "$hits" ]]; then
    echo "FATAL: competing terminal(s) running — keystrokes will leak." >&2
    echo "Quit them, or set SONICTERM_HARNESS_ALLOW_OTHER_TERMS=1 to override." >&2
    echo "$hits" >&2
    exit 2
  fi
fi

# ------------------------------------------------------------------
# B2 boundary-verify support: snapshot the user's pre-existing
# sonicterm-mac PIDs once. Anything OUTSIDE this set after a run is
# either a harness-tracked PID we failed to reap (warn + force-kill)
# or a user-launched instance mid-run (log only — not ours to kill).
# Exported so run_case.sh can read it.
# ------------------------------------------------------------------
export PRE_RUN_USER_PIDS
PRE_RUN_USER_PIDS=$(pgrep -f "./target/release/sonicterm-mac" 2>/dev/null | sort -u || true)

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

for id in "${IDS[@]}"; do # harness-safe-empty: non-empty asserted upstream at mac.sh:113
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

# ------------------------------------------------------------------
# Guard 6 epilogue — close any stray Finder windows opened by leaked
# keystrokes that hit Finder during the Finder-park escape hatch (e.g.
# a stray `t` from Cmd+T landed on Finder mid-case and opened a
# Finder window). Harmless if there are none.
# ------------------------------------------------------------------
osascript -e 'tell application "Finder" to close every window' >/dev/null 2>&1 || true

[[ $FAIL -eq 0 ]]
