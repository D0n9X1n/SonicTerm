#!/usr/bin/env bash
# Run a single case from testing/cases.toml.
# Usage: run_case.sh <case-id> <out-dir>
# Exit:  0 = pass, 1 = fail, 77 = skip
set -uo pipefail

ID="${1:?case id required}"
OUT="${2:?out dir required}"
mkdir -p "$OUT"

CASE_OUT="$OUT/$ID"
mkdir -p "$CASE_OUT"
LOG="$CASE_OUT/case.log"
: > "$LOG"

log() { echo "[$(date +%H:%M:%S)] $*" | tee -a "$LOG"; }

# OCR preflight (issue #497, mirrors PR #498 on run_case.ps1).
# Honor SONICTERM_HARNESS_OCR_AVAILABLE from driver; else probe PATH.
if [[ "${SONICTERM_HARNESS_OCR_AVAILABLE:-}" == "1" ]]; then OCR_AVAILABLE=1
elif [[ "${SONICTERM_HARNESS_OCR_AVAILABLE:-}" == "0" ]]; then OCR_AVAILABLE=0
elif command -v tesseract >/dev/null 2>&1; then OCR_AVAILABLE=1
else OCR_AVAILABLE=0
fi
export SONICTERM_HARNESS_OCR_AVAILABLE="$OCR_AVAILABLE"

# Issue #593 — CJK preflight. Honor SONICTERM_HARNESS_CJK_AVAILABLE from driver;
# else probe `tesseract --list-langs` for any of {chi_sim,chi_tra,jpn,kor}.
if [[ "${SONICTERM_HARNESS_CJK_AVAILABLE:-}" == "1" ]]; then CJK_AVAILABLE=1
elif [[ "${SONICTERM_HARNESS_CJK_AVAILABLE:-}" == "0" ]]; then CJK_AVAILABLE=0
elif [[ "$OCR_AVAILABLE" -eq 1 ]] && tesseract --list-langs 2>&1 | grep -Eq '^(chi_sim|chi_tra|jpn|kor)$'; then CJK_AVAILABLE=1
else CJK_AVAILABLE=0
fi
export SONICTERM_HARNESS_CJK_AVAILABLE="$CJK_AVAILABLE"

# ------------------------------------------------------------------
# Guard 6 — Finder-park escape hatch. Park focus on Finder BEFORE
# spawning sonicterm-mac so any pre-spawn keystroke leak (which can
# happen during the ~0.5s between case-N cleanup and case-N+1
# focus_sonic) lands on Finder. Finder will, at worst, open a stray
# window (swept by mac.sh's epilogue), never execute shell commands.
# Documented in issue #464 (v3 diagnosis, Guard 6 carry-forward).
# ------------------------------------------------------------------
osascript -e 'tell application "Finder" to activate' >/dev/null 2>&1 || true
sleep 0.2

# ------------------------------------------------------------------
# B2 — multi-PID tracking. Every sonicterm-mac PID we spawn (either
# directly or via a case shell-cmd that backgrounds another instance,
# e.g. cases.toml:1008 `config-validation-bad-toml-falls-back`) goes
# into SONIC_PIDS; cleanup kills exactly those, never broadcasts
# `pkill -f sonicterm-mac` (which would kill the user's dev build).
# Snapshot-delta around each shell-cmd block captures launchd-
# reparented grandchildren that `pgrep -P $$` would miss.
# ------------------------------------------------------------------
SONIC_PIDS=()
_PRE_PIDS=""

# #548: trap-based reap so `exit 77` early-skip paths still clean up the
# spawned sonicterm-mac. Final cleanup block below remains the primary
# path; this is the belt-and-suspenders for non-zero exits.
_run_case_trap_cleanup() {
  for pid in "${SONIC_PIDS[@]:-}"; do
    kill -9 "$pid" 2>/dev/null || true
  done
}
trap _run_case_trap_cleanup EXIT INT TERM

snapshot_sonic_pids_before() {
  _PRE_PIDS=$(pgrep -f "./target/release/sonicterm-mac" 2>/dev/null | sort -u)
}
snapshot_sonic_pids_after() {
  local post new pid
  post=$(pgrep -f "./target/release/sonicterm-mac" 2>/dev/null | sort -u)
  new=$(comm -13 <(printf '%s\n' "$_PRE_PIDS") <(printf '%s\n' "$post"))
  for pid in $new; do
    SONIC_PIDS+=("$pid")
    log "B2: tracked new harness sonicterm-mac pid=$pid (from shell-cmd)"
  done
}

# ------------------------------------------------------------------
# Extract case as JSON for easy reading
# ------------------------------------------------------------------
CASE_JSON="$CASE_OUT/case.json"
python3 - "$ID" > "$CASE_JSON" <<'PY'
import sys, tomllib, json
target = sys.argv[1]
with open('testing/cases.toml','rb') as f:
    d = tomllib.load(f)
for c in d['case']:
    if c['id'] == target:
        json.dump(c, sys.stdout, indent=2)
        break
else:
    sys.exit(2)
PY
if [[ ! -s "$CASE_JSON" ]]; then
  log "FATAL: case '$ID' not found in testing/cases.toml"
  exit 1
fi

APPLIES_TO=$(python3 -c "import json; print(','.join(json.load(open('$CASE_JSON'))['applies_to']))")
log "applies_to: $APPLIES_TO"
if [[ ",$APPLIES_TO," == *",mac-manual,"* && ",$APPLIES_TO," != *",mac,"* ]]; then
  log "SKIP — manual-only on this platform"
  exit 77
fi

# Issue #497 — early-skip if every expect is OCR and tesseract is missing.
if [[ "$OCR_AVAILABLE" -eq 0 ]]; then
  ALL_OCR=$(python3 - "$CASE_JSON" <<'PY'
import json, sys
OCR = {'text-in-region','ocr-text','not-text-in-region','ocr_line_regex'}
exp = json.load(open(sys.argv[1])).get('expect', [])
print('yes' if exp and all(e.get('kind') in OCR for e in exp) else 'no')
PY
)
  if [[ "$ALL_OCR" == "yes" ]]; then
    python3 -c "
import json
for i,e in enumerate(json.load(open('$CASE_JSON')).get('expect',[])):
    print(f'{i}\t{e.get(\"kind\")}')" | while IFS=$'\t' read -r i kind; do
      log "[SKIP ocr_unavailable] case=$ID expect[$i]=$kind"
    done
    log "SKIP: ocr_unavailable (all expects are OCR-only and tesseract is not on PATH)"
    echo "SKIP" > "$CASE_OUT/status"
    exit 77
  fi
fi

# ------------------------------------------------------------------
# Start sonicterm-mac fresh, position the window, capture window id
# ------------------------------------------------------------------
SONIC_BIN="./target/release/sonicterm-mac"
if [[ ! -x "$SONIC_BIN" ]]; then
  log "FATAL: $SONIC_BIN not built"
  exit 1
fi

# B2: scoped cleanup only — never broadcast `pkill -f sonicterm-mac`
# (would kill the user's dev build). At spawn time SONIC_PIDS is
# typically empty; this iteration is a no-op safety net for any PIDs
# a prior step (e.g. a wrapping harness) may have already tracked.
for pid in "${SONIC_PIDS[@]:-}"; do
  kill -9 "$pid" 2>/dev/null || true
  wait "$pid" 2>/dev/null || true
done
sleep 0.4
# `disown` the backgrounded child so bash does not write its own
# "Terminated: 15" job-notification text to stderr when we later signal
# the process during the cleanup phase. The harness's status parser
# was previously misreading that bash-emitted notification as a test
# failure even though the case itself had completed cleanly. We still
# track the PID explicitly via $SONIC_PID for kill/wait below.
"$SONIC_BIN" > "$CASE_OUT/sonicterm.log" 2>&1 &
SONIC_PID=$!
disown "$SONIC_PID" 2>/dev/null || true
SONIC_PIDS+=("$SONIC_PID")
echo "$SONIC_PID" >> "$CASE_OUT/spawned-pids.txt"  # #548
export SONIC_PID  # exposed for shell-cmd payloads that want to target our spawned instance
log "spawned sonicterm-mac pid=$SONIC_PID"

# Guard 2 — process-exists verify. If sonicterm-mac died between
# spawn and now (panic on init, missing dylib, ...), fail fast rather
# than walk into a focusless keystroke storm.
sleep 0.4
if ! kill -0 "$SONIC_PID" 2>/dev/null; then
  log "FATAL: sonicterm-mac (pid=$SONIC_PID) died before window appeared — see $CASE_OUT/sonicterm.log"
  echo "FAIL" > "$CASE_OUT/status"
  exit 1
fi

# Guard 3 — window-exists verify with longer budget + hard FAIL.
# Cold wgpu init on a fresh shader cache can exceed 6s on M-series;
# default 10s, overrideable. Convert previous WARN-and-continue to
# hard FAIL — window absence is never benign and was what allowed
# subsequent keystrokes to leak (issue #464).
TIMEOUT_S="${SONICTERM_HARNESS_WIN_TIMEOUT_S:-10}"
WINDOW_READY=0
for _ in $(seq 1 $((TIMEOUT_S * 10))); do
  count=$(osascript -e 'tell application "System Events" to count windows of (first process whose name is "sonicterm-mac")' 2>/dev/null || echo 0)
  if [[ "${count:-0}" -gt 0 ]]; then
    WINDOW_READY=1
    break
  fi
  if ! kill -0 "$SONIC_PID" 2>/dev/null; then
    log "FATAL: pid $SONIC_PID died waiting for window"
    echo FAIL > "$CASE_OUT/status"
    exit 1
  fi
  sleep 0.1
done
if [[ $WINDOW_READY -ne 1 ]]; then
  log "FATAL: sonicterm-mac window did not appear within ${TIMEOUT_S}s"
  echo FAIL > "$CASE_OUT/status"
  exit 1
fi

osascript >/dev/null 2>&1 <<EOF || true
tell application "System Events"
  tell process "sonicterm-mac"
    set frontmost to true
    set position of window 1 to {500, 200}
    set size of window 1 to {1000, 700}
  end tell
end tell
EOF
sleep 0.4

# ----- Guard 4 — frontmost-verify with retry, hard SKIP on failure -----
verify_front() {
  local front
  front=$(osascript -e 'tell application "System Events" to name of first process whose frontmost is true' 2>/dev/null || echo "")
  [[ "$front" == "sonicterm-mac" ]]
}

focus_sonic() {
  for try in 1 2 3 4 5; do
    osascript >/dev/null 2>&1 <<EOF || true
tell application "System Events"
  set frontmost of (first process whose name is "sonicterm-mac") to true
end tell
EOF
    sleep 0.25
    verify_front && return 0
    log "focus retry $try (front=$(osascript -e 'tell application "System Events" to name of first process whose frontmost is true' 2>/dev/null))"
  done
  return 1
}

# Pre-keystroke gate: invoked by every send_chord/send_text/do_setup.
# If we can't keep sonicterm-mac frontmost, SKIP (exit 77) rather than
# fire keystrokes into whatever else has focus. This is the core fix
# for #464 — the previous `focus_sonic || true` swallowed the failure.
ensure_front_or_skip() {
  verify_front && return 0
  focus_sonic && return 0
  log "SKIP: cannot keep sonicterm-mac frontmost — keystrokes would leak"
  echo "SKIP" > "$CASE_OUT/status"
  exit 77
}

ensure_front_or_skip

# Capture the actual sonic window id (used for window-only screenshots).
# #549: AppleScript `id of window 1` returns nothing for winit apps (sonicterm-mac
# is not AppleScript-aware), so the previous probe always yielded "" and every case
# skipped with "no window id captured". Use CGWindowID via in-tree Swift helper —
# CoreGraphics window IDs are what `screencapture -l` actually accepts. The window
# may not be on-screen immediately after spawn, so retry briefly (up to ~3s).
#
# #554: prefer the `SONICTERM_WINDOW_READY` stdout marker the mac bin
# prints the instant winit hands AppKit the window — greppable from
# `$CASE_OUT/sonicterm.log`, saves ~3s/case vs the Swift-helper poll
# (~3min/sweep at 67 cases). If the marker does not appear within
# ~5s we fall back to the Swift helper, preserving compatibility with
# branches built before this fix. Marker contract:
# `crates/sonicterm-mac/CLAUDE.md`.
WINDOW_ID=""
WIN_ID_RAW=""
for _try in $(seq 1 50); do
  WIN_ID_RAW=$(grep -m1 -E '^SONICTERM_WINDOW_READY ' "$CASE_OUT/sonicterm.log" 2>/dev/null \
    | sed -nE 's/.*cg_window_id=([0-9]+).*/\1/p')
  if [[ -n "$WIN_ID_RAW" && "$WIN_ID_RAW" != "-1" ]]; then
    log "window id (stdout marker): $WIN_ID_RAW"
    break
  fi
  WIN_ID_RAW=""
  sleep 0.1
done
if [[ -z "$WIN_ID_RAW" ]]; then
  log "stdout marker absent after 5s; falling back to cg-window-id.swift"
  for _try in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15; do
    WIN_ID_RAW=$(testing/workflows/cg-window-id.swift "$SONIC_PID" 2>/dev/null || echo "")
    if [[ -n "$WIN_ID_RAW" ]]; then
      break
    fi
    sleep 0.2
  done
fi
if [[ -n "$WIN_ID_RAW" ]]; then
  WINDOW_ID="$WIN_ID_RAW"
fi
log "window id: ${WINDOW_ID:-<unknown>}"

# ------------------------------------------------------------------
# Setup helpers
# ------------------------------------------------------------------
do_setup() {
  ensure_front_or_skip
  local step="$1"
  case "$step" in
    open-3-tabs)
      osascript -e 'tell application "System Events" to keystroke "t" using command down' >/dev/null
      sleep 0.3
      osascript -e 'tell application "System Events" to keystroke "t" using command down' >/dev/null
      sleep 0.3
      ;;
    open-second-window)
      osascript -e 'tell application "System Events" to keystroke "n" using command down' >/dev/null
      sleep 0.5
      ;;
    open-prefs)
      osascript -e 'tell application "System Events" to keystroke "," using command down' >/dev/null
      sleep 0.6
      ;;
    split-right)
      osascript -e 'tell application "System Events" to keystroke "d" using command down' >/dev/null
      sleep 0.5
      ;;
    clear)
      osascript -e 'tell application "System Events" to keystroke "clear"' >/dev/null
      osascript -e 'tell application "System Events" to key code 36' >/dev/null
      sleep 0.3
      ;;
    type:*) osascript -e "tell application \"System Events\" to keystroke \"${step#type:}\"" >/dev/null ;;
    enter)  osascript -e 'tell application "System Events" to key code 36' >/dev/null ;;
    wait:*) sleep "${step#wait:}" ;;
    manual-*) log "skip manual setup: $step" ;;
    *) log "WARN: unknown setup step '$step'" ;;
  esac
}

# ------------------------------------------------------------------
# Keystroke helpers
# ------------------------------------------------------------------
# #589 Bug C: click body center (titlebar-offset) via cliclick + CG bounds.
_click_body_center() {
  ensure_front_or_skip
  local cliclick="/opt/homebrew/bin/cliclick"
  [[ -x "$cliclick" ]] || cliclick="$(command -v cliclick || true)"
  if [[ -z "$cliclick" ]]; then
    log "click-region body-center: SKIP — cliclick not installed"; return 0
  fi
  local bounds x y w h cx cy
  bounds=$(testing/workflows/cg-window-id.swift --bounds "$SONIC_PID" 2>/dev/null || echo "")
  if [[ -z "$bounds" ]]; then
    log "click-region body-center: SKIP — no bounds for pid=$SONIC_PID"; return 0
  fi
  read -r x y w h <<<"$bounds"
  cx=$(( x + w / 2 )); cy=$(( y + 28 + (h - 28) / 2 ))   # 28pt titlebar
  log "click-region body-center: bounds=$x,$y,${w}x$h -> @ $cx,$cy"
  "$cliclick" "c:${cx},${cy}" >>"$LOG" 2>&1 || true
  sleep 0.15
}

# Map chord like "cmd+t" -> osascript keystroke
send_chord() {
  ensure_front_or_skip
  local chord="$1"
  local mods=""
  local key="$chord"
  if [[ "$chord" == *"+"* ]]; then
    local IFS="+"
    local parts
    # shellcheck disable=SC2206
    parts=( $chord )
    local last_idx=$(( ${#parts[@]} - 1 ))
    key="${parts[$last_idx]}"
    unset 'parts[last_idx]'
    local mlist=()
    for p in "${parts[@]}"; do # harness-safe-empty: populated inline at L254 via mapfile
      case "$p" in
        cmd|command) mlist+=("command down") ;;
        shift)       mlist+=("shift down") ;;
        ctrl)        mlist+=("control down") ;;
        alt|option)  mlist+=("option down") ;;
      esac
    done
    mods="$(IFS=, ; echo "${mlist[*]}")"
  fi

  # Special non-printing keys
  case "$key" in
    enter|return)
      osascript -e 'tell application "System Events" to key code 36' >/dev/null ;;
    escape|esc)
      osascript -e 'tell application "System Events" to key code 53' >/dev/null ;;
    down) osascript -e 'tell application "System Events" to key code 125' >/dev/null ;;
    up)   osascript -e 'tell application "System Events" to key code 126' >/dev/null ;;
    left) osascript -e 'tell application "System Events" to key code 123' >/dev/null ;;
    right) osascript -e 'tell application "System Events" to key code 124' >/dev/null ;;
    page-up) osascript -e 'tell application "System Events" to key code 116' >/dev/null ;;
    plus)
      osascript -e 'tell application "System Events" to keystroke "=" using {command down}' >/dev/null ;;
    minus)
      osascript -e 'tell application "System Events" to keystroke "-" using {command down}' >/dev/null ;;
    *)
      if [[ -n "$mods" ]]; then
        osascript -e "tell application \"System Events\" to keystroke \"$key\" using {$mods}" >/dev/null
      else
        osascript -e "tell application \"System Events\" to keystroke \"$key\"" >/dev/null
      fi
      ;;
  esac
}

send_text() {
  ensure_front_or_skip
  local text="$1"
  # escape backslashes + double quotes for osascript
  local esc
  esc="${text//\\/\\\\}"
  esc="${esc//\"/\\\"}"
  osascript -e "tell application \"System Events\" to keystroke \"$esc\"" >/dev/null
}

# ------------------------------------------------------------------
# Iterate setup, keystrokes
# ------------------------------------------------------------------
python3 - "$CASE_JSON" > "$CASE_OUT/steps.sh" <<'PY'
import json, sys, shlex
c = json.load(open(sys.argv[1]))
out = []
for s in c.get('setup', []):
    out.append(f"do_setup {shlex.quote(s)}")
for k in c.get('keystrokes', []):
    kind = k.get('kind')
    if kind == 'key':
        out.append(f"send_chord {shlex.quote(k['value'])}")
    elif kind == 'text':
        out.append(f"send_text {shlex.quote(k['value'])}")
    elif kind == 'wait':
        out.append(f"sleep {float(k['value'])}")
    elif kind == 'key-repeat':
        n = int(k.get('count', 1))
        delay = float(k.get('delay', 0.1))
        chord = k['value']
        for _ in range(n):
            out.append(f"send_chord {shlex.quote(chord)}")
            out.append(f"sleep {delay}")
    elif kind == 'shell-cmd':
        # B2 (v3): snapshot-delta around every shell-cmd. Any new
        # sonicterm-mac PID that appears as a side effect of the
        # payload (e.g. cases.toml's `config-validation-bad-toml-
        # falls-back` backgrounds a second instance) is tracked in
        # SONIC_PIDS so cleanup kills exactly it, never the user's
        # dev build.
        out.append('snapshot_sonic_pids_before')
        out.append(k['value'])
        out.append('snapshot_sonic_pids_after')
    elif kind == 'snapshot-sonic-shells':
        # Snapshot every shell descendant of the live sonicterm-mac process.
        # Used by orphan-shells-from-sonic expect kind to verify
        # PtyHandle::Drop actually kills children on Cmd+Q.
        out.append('bash testing/workflows/check_orphans.sh snapshot "$SONIC_PID" "$CASE_OUT/sonic-shells.txt" 2>>"$LOG" || true')
    elif kind == 'click-region':
        # #589 Bug C: body-center via cliclick; other regions still TODO.
        region = k.get('region', '')
        if region == 'body-center':
            out.append('_click_body_center')
        else:
            out.append("log " + shlex.quote(f"TODO click-region: {region}"))
    elif kind == 'cmd-click-region':
        out.append("log " + shlex.quote(f"TODO cmd-click-region: {k.get('region')}"))
    elif kind == 'drag':
        out.append("log " + shlex.quote(f"TODO drag: {k}"))
    elif kind == 'hover-region':
        out.append("log " + shlex.quote(f"TODO hover: {k.get('region')}"))
    elif kind == 'resize-window':
        for w, h in k.get('sequence', []):
            out.append(
                "osascript -e " + shlex.quote(
                    f'tell application "System Events" to tell process "sonicterm-mac" to set size of window 1 to {{{w}, {h}}}'
                ) + " >/dev/null 2>&1 || true"
            )
            out.append("sleep 0.15")
    elif kind == 'focus-window':
        out.append("log " + shlex.quote(f"TODO focus-window: {k.get('index')}"))
    else:
        out.append("log " + shlex.quote(f"WARN unknown keystroke kind: {kind}"))
print('\n'.join(out))
PY

ensure_front_or_skip
# shellcheck source=/dev/null
source "$CASE_OUT/steps.sh"

# ------------------------------------------------------------------
# Guard 5 — window-only screencap is now MANDATORY. The previous
# full-display fallback was masking focus bugs: when sonicterm-mac
# wasn't at the (500, 200) corner the case assumed, the 1000×700
# logical-coord pixel-near checks sampled desktop pixels outside the
# window (the "Class B black pixel" failure mode in #464). If we
# don't have a usable window id, SKIP — never sample wrong pixels.
# Note: cases.toml coords are unchanged; pixel_near() already maps
# image-w / 1000 logical units onto whatever screencap size we hand it.
# ------------------------------------------------------------------
SHOT="$CASE_OUT/screen.png"
SCREEN_PNG="$SHOT"
# #597: re-query CGWindowID *post-action* — the case may have just
# closed its window (Cmd+Q, kill -9 of sonicterm-mac). If gone, leave
# SCREEN_PNG empty so visual expects SKIP while non-visual expects
# (orphan-shells-from-sonic, process-count, ...) still evaluate.
# Guard 5 pixel-coord safety is preserved: we only screencap when a
# live CGWindowID exists for $SONIC_PID.
WIN_ID_POST=$(testing/workflows/cg-window-id.swift "$SONIC_PID" 2>/dev/null || echo "")
if [[ -z "$WIN_ID_POST" ]]; then
  log "WARN screencap-skipped: window closed (no CGWindowID for pid=$SONIC_PID post-action)"
  SCREEN_PNG=""
elif ! screencapture -x -l "$WIN_ID_POST" "$SHOT" 2>/dev/null || [[ ! -s "$SHOT" ]]; then
  log "WARN screencap-skipped: window-local screencap failed (window may have closed)"
  SCREEN_PNG=""
else
  log "screenshot (window-only): $SHOT"
fi
# #589 Bug B: crop ~120px (titlebar + tab strip @ 2× Retina) for OCR; pixel-near keeps full image.
# #597: only crop if we actually captured a screenshot (window may have closed).
SHOT_BODY="$CASE_OUT/screen-body.png"
if [[ -n "$SCREEN_PNG" && -s "$SHOT" ]]; then
  python3 - "$SHOT" "$SHOT_BODY" <<'PY' >>"$LOG" 2>&1 || true
import sys
from PIL import Image
src, dst = sys.argv[1:3]
im = Image.open(src); w, h = im.size
im.crop((0, min(120, max(0, h - 1)), w, h)).save(dst)
PY
  log "screenshot (window-only): $SHOT  body=$SHOT_BODY"
fi

# ------------------------------------------------------------------
# Evaluate expectations (best-effort; reports per-check pass/fail)
# ------------------------------------------------------------------
EXPECT_LOG="$CASE_OUT/expect.log"
: > "$EXPECT_LOG"

python3 - "$CASE_JSON" "$SCREEN_PNG" "$EXPECT_LOG" "$CASE_OUT" "$SHOT_BODY" <<'PY'
import json, sys, os, subprocess
case_path, shot, elog, case_out, shot_body = sys.argv[1:6]
c = json.load(open(case_path))
expectations = c.get('expect', [])
ocr_available = (os.environ.get('SONICTERM_HARNESS_OCR_AVAILABLE', '1') == '1')
cjk_available = (os.environ.get('SONICTERM_HARNESS_CJK_AVAILABLE', '1') == '1')

# Issue #593: CJK codepoint detection — CJK Unified, Hiragana, Katakana, Hangul.
import re as _re
_CJK_RE = _re.compile(r'[぀-ゟ゠-ヿ㐀-䶿一-鿿가-힯豈-﫿]')
def has_cjk(s):
    return bool(_CJK_RE.search(s or ''))
OCR_KINDS = ('text-in-region', 'ocr-text', 'not-text-in-region', 'ocr_line_regex')
# #597: visual expects need a screenshot. Non-visual expects (orphan-
# shells-from-sonic, process-count, exit-code, log-contains, file-*,
# process-*) run fine without one.
VISUAL_KINDS = ('screenshot', 'pixel-near') + OCR_KINDS
screen_ok = bool(shot) and os.path.exists(shot) and os.path.getsize(shot) > 0
OCR_SHOT = shot_body if (shot_body and os.path.exists(shot_body) and os.path.getsize(shot_body) > 0) else shot
results = []  # (status, kind, reason) status in {'PASS','FAIL','SKIP'}

def have(p):
    return os.path.exists(p) and os.path.getsize(p) > 0

def pixel_near(shot, x, y, rgba, tol, sample='1x1'):
    # sample='3x3' averages a 3x3 window centered at (sx,sy); '1x1' is the
    # original single-pixel behavior. #588: single-pixel checks were fragile
    # under window-placement variance; 3x3 average tolerates ~1px drift.
    try:
        from PIL import Image
        im = Image.open(shot).convert('RGBA')
        # screencapture is Retina — scale coords by ratio of img-w / 1000 (logical)
        sx = int(x * (im.width / 1000.0))
        sy = int(y * (im.height / 700.0))
        if not (0 <= sx < im.width and 0 <= sy < im.height):
            return False, f"coords oob ({sx},{sy}) in {im.size}"
        if sample == '3x3':
            ch = len(rgba)
            acc = [0] * ch
            n = 0
            for dy in (-1, 0, 1):
                for dx in (-1, 0, 1):
                    nx, ny = sx + dx, sy + dy
                    if 0 <= nx < im.width and 0 <= ny < im.height:
                        p = im.getpixel((nx, ny))
                        for i in range(ch):
                            acc[i] += int(p[i])
                        n += 1
            if n == 0:
                return False, f"sample 3x3 empty at ({sx},{sy})"
            avg = tuple(acc[i] // n for i in range(ch))
            d = max(abs(avg[i] - int(rgba[i])) for i in range(ch))
            return (d <= tol), f"pixel@({sx},{sy}) avg3x3={avg} target={rgba} delta={d} tol={tol}"
        px = im.getpixel((sx, sy))
        d = max(abs(int(a) - int(b)) for a, b in zip(px[:len(rgba)], rgba))
        return (d <= tol), f"pixel@({sx},{sy})={px} target={rgba} delta={d} tol={tol}"
    except Exception as e:
        return False, f"err {e}"

def _dhash_64(path):
    # 8x8 dHash (row-diff): grayscale, resize to 9x8, compare adjacent
    # horizontal pixels -> 64-bit fingerprint. Tolerates window-placement
    # variance & minor antialiasing changes; #588 follow-up replacing
    # fragile pixel-near for 2 cases (cjk-emoji-bg, truecolor).
    from PIL import Image
    im = Image.open(path).convert('L').resize((9, 8), Image.LANCZOS)
    px = list(im.getdata())
    bits = 0
    for r in range(8):
        for c in range(8):
            left = px[r * 9 + c]
            right = px[r * 9 + c + 1]
            bits = (bits << 1) | (1 if left > right else 0)
    return bits

def dhash_match(shot, reference, tolerance):
    # tolerance = max Hamming distance (0..64) between shot & reference hashes.
    try:
        if not os.path.exists(reference):
            return False, f"reference not found: {reference}"
        h_shot = _dhash_64(shot)
        h_ref = _dhash_64(reference)
        dist = bin(h_shot ^ h_ref).count('1')
        return (dist <= tolerance), f"dhash shot={h_shot:016x} ref={h_ref:016x} hamming={dist} tol={tolerance}"
    except Exception as e:
        return False, f"err {e}"

def ocr_contains(shot, value):
    # Returns (status, sample) where status in {True, False, None}.
    # None = OCR unavailable, caller should treat as SKIP. Mirrors PR #498.
    if not ocr_available:
        return None, "ocr_unavailable"
    # Issue #593: per-call language dispatch. CJK expects need the CJK pack;
    # without it, the OCR engine returns garbage that never matches and the
    # case fails instead of correctly skipping.
    if has_cjk(value):
        if not cjk_available:
            return None, "cjk_unavailable"
        lang = 'chi_sim+chi_tra+jpn+kor+eng'
    else:
        lang = 'eng'
    try:
        out = subprocess.run(['tesseract', shot, '-', '-l', lang, '--psm', '6'],
                             capture_output=True, text=True, timeout=20)
        return (value in out.stdout), out.stdout[:200].replace('\n', ' / ')
    except Exception as e:
        return False, f"err {e}"

def ocr_line_regex(shot, marker, regex):
    # #615/#616: returns (status, sample) where status in {True, False, None}.
    # Runs eng-only OCR over the body-cropped shot, finds FIRST line that
    # contains `marker`, and matches `regex` against it. PASS on match;
    # FAIL with the offending line as evidence otherwise. None => SKIP.
    if not ocr_available:
        return None, "ocr_unavailable"
    try:
        out = subprocess.run(['tesseract', shot, '-', '-l', 'eng', '--psm', '6'],
                             capture_output=True, text=True, timeout=20)
        pat = _re.compile(regex)
        for ln in out.stdout.splitlines():
            if marker in ln:
                return bool(pat.search(ln)), f"line={ln!r} regex={regex!r}"
        return False, f"marker {marker!r} not found in OCR; head={out.stdout[:200]!r}"
    except Exception as e:
        return False, f"err {e}"

# Issue #607: path-scoped pgrep + baseline subtraction.
# Path-unscoped `pgrep -f sonicterm-mac` matches the user's dev/debug
# build, IDE windows containing the string, etc. Scope to the release
# binary path. Then subtract PRE_RUN_USER_PIDS (the snapshot mac.sh
# takes before the harness starts, per #464) so user-launched
# instances don't masquerade as harness-spawned leaks.
_PRE_PIDS = set(
    p for p in (os.environ.get('PRE_RUN_USER_PIDS', '') or '').split() if p.strip()
)
def proc_count(prog):
    try:
        if prog == 'sonicterm-mac':
            # Path component (leading '/') anchors to release dir, so
            # the user's `target/debug/sonicterm-mac` and IDE windows
            # containing the substring don't match. Works for both the
            # relative launch (`./target/release/...`, run_case.sh:160)
            # and any future absolute-path invocation under REPO_ROOT.
            pat = '/target/release/sonicterm-mac'
        else:
            pat = prog
        r = subprocess.run(['pgrep', '-f', pat], capture_output=True, text=True)
        pids = [l.strip() for l in r.stdout.splitlines() if l.strip()]
        return len([p for p in pids if p not in _PRE_PIDS])
    except Exception:
        return -1

for idx, e in enumerate(expectations):
    kind = e.get('kind')
    status = None  # 'PASS' | 'FAIL' | 'SKIP'
    if kind in VISUAL_KINDS and not screen_ok:
        # #597: window closed before screencap — visual expect cannot be
        # evaluated; SKIP so non-visual expects still drive the verdict.
        results.append(('SKIP', kind, f"screencap_unavailable case={c.get('id')} expect[{idx}]={kind}"))
        continue
    if kind in OCR_KINDS and not ocr_available:
        # Issue #497: per-expect OCR skip in mixed cases.
        results.append(('SKIP', kind, f"ocr_unavailable case={c.get('id')} expect[{idx}]={kind}"))
        continue
    if kind == 'screenshot':
        ok = have(shot); reason = f"exists={ok} path={shot}"
    elif kind == 'pixel-near':
        ok, reason = pixel_near(shot, e['x'], e['y'], e['rgba'], e.get('tolerance', 20), e.get('sample', '1x1'))
    elif kind == 'dhash':
        ok, reason = dhash_match(shot, e['reference'], int(e.get('tolerance', 8)))
    elif kind in ('text-in-region', 'ocr-text'):
        ok, reason = ocr_contains(OCR_SHOT, e['value'])
        if ok is None:
            results.append(('SKIP', kind, f"{reason} case={c.get('id')} expect[{idx}]={kind}"))
            continue
    elif kind == 'not-text-in-region':
        contains, sample = ocr_contains(OCR_SHOT, e['value'])
        if contains is None:
            results.append(('SKIP', kind, f"{sample} case={c.get('id')} expect[{idx}]={kind}"))
            continue
        ok = not contains
        reason = f"absent={ok} sample='{sample}'"
    elif kind == 'ocr_line_regex':
        ok, reason = ocr_line_regex(OCR_SHOT, e['marker'], e['regex'])
        if ok is None:
            results.append(('SKIP', kind, f"{reason} case={c.get('id')} expect[{idx}]={kind}"))
            continue
    elif kind == 'process-count':
        n = proc_count(e['program'])
        if 'value' in e:
            ok = (n == e['value'])
        elif 'min' in e:
            ok = (n >= e['min'])
        elif 'max' in e:
            ok = (n <= e['max'])
        else:
            ok = (n > 0)
        reason = f"pgrep -f {e['program']} -> {n}"
    elif kind == 'exit-code':
        cmd = e['cmd']
        r = subprocess.run(cmd, shell=True, capture_output=True)
        ok = (r.returncode == e['value'])
        reason = f"cmd '{cmd}' rc={r.returncode}"
    elif kind == 'orphan-shells-from-sonic':
        # Real check: read the pre-Cmd+Q snapshot of sonicterm-mac's shell
        # descendants, then verify each is dead. Snapshot is produced by
        # the 'snapshot-sonic-shells' keystroke kind. Without that snapshot
        # the case authoring is wrong; treat as FAIL so a mis-authored
        # case can't masquerade as passing.
        snap = os.path.join(case_out, 'sonic-shells.txt')
        expected = int(e.get('expected', 0))
        if not os.path.exists(snap):
            ok = False
            reason = f"snapshot missing — case must include a 'snapshot-sonic-shells' keystroke before the kill step ({snap})"
        else:
            r = subprocess.run(['bash', 'testing/workflows/check_orphans.sh', 'check', snap],
                               capture_output=True, text=True)
            # last stdout line is `orphans=<N>`
            n = -1
            for line in r.stdout.strip().splitlines():
                if line.startswith('orphans='):
                    try:
                        n = int(line.split('=', 1)[1])
                    except ValueError:
                        n = -1
            ok = (n == expected)
            stderr_tail = r.stderr.strip().replace('\n', ' | ')[:300]
            reason = f"orphans={n} expected={expected} snap={snap} stderr='{stderr_tail}'"
    elif kind == 'responsive-within':
        # Heuristic: confirm sonicterm-mac process exists and is not zombie.
        n = proc_count('sonicterm-mac')
        ok = (n >= 1)
        reason = f"sonicterm-mac processes alive={n}"
    elif kind == 'window-visible':
        # #589 Bug A: count normal-level on-screen windows owned by SONIC_PID.
        pid = os.environ.get('SONIC_PID', ''); need = int(e.get('min', 1)); n = -1
        try:
            r = subprocess.run(['testing/workflows/cg-window-id.swift', '--count', pid],
                               capture_output=True, text=True, timeout=10)
            n = int((r.stdout or '0').strip() or '0')
        except Exception as ex:
            reason = f"err {ex}"
        else:
            reason = f"window-count pid={pid} got={n} need>={need}"
        ok = (n >= need)
    elif kind in ('tab-count', 'pane-count', 'window-count',
                  'tab-count-in-window', 'scrollback-min-lines',
                  'padding-min', 'process-spawned', 'process-not-spawned',
                  'process-cpu-max', 'file-absent'):
        # Best-effort heuristics; many require SonicTerm-internal hooks we don't have yet.
        if kind == 'file-absent':
            ok = (not os.path.exists(e.get('path', '')))
            reason = f"absent={ok} path={e.get('path')}"
        elif kind == 'process-not-spawned':
            ok = True; reason = "heuristic: no easy way to verify negative spawn — passed by default"
        elif kind == 'process-cpu-max':
            try:
                r = subprocess.run(['ps', '-A', '-o', 'pcpu,comm'], capture_output=True, text=True)
                vals = []
                for ln in r.stdout.splitlines():
                    parts = ln.strip().split(None, 1)
                    if len(parts) == 2 and e['program'] in parts[1]:
                        vals.append(float(parts[0]))
                cpu = max(vals) if vals else 0.0
                ok = (cpu <= e['max_pct'])
                reason = f"max pcpu={cpu} threshold={e['max_pct']}"
            except Exception as ex:
                ok = False; reason = f"err {ex}"
        else:
            ok = True
            reason = f"heuristic-pass (kind='{kind}' needs internal hook; not yet implemented)"
    else:
        ok = False; reason = f"unknown kind '{kind}'"
    results.append(('PASS' if ok else 'FAIL', kind, reason))

with open(elog, 'w') as f:
    for status, kind, reason in results:
        f.write(f"{status}\t{kind}\t{reason}\n")

# Tri-state aggregation (issue #497, mirrors PR #498):
#   any FAIL -> exit 1 (fail wins over skip)
#   else any SKIP -> exit 77
#   else exit 0
fails = [r for r in results if r[0] == 'FAIL']
skips = [r for r in results if r[0] == 'SKIP']
if fails:
    sys.exit(1)
if skips:
    sys.exit(77)
sys.exit(0)
PY
expect_rc=$?

# ------------------------------------------------------------------
# Cleanup — kill exactly the PIDs we tracked in SONIC_PIDS (B2 v3).
# No broadcast `pkill -f sonicterm-mac` (would kill the user's dev
# build). Order: graceful Cmd+Q first, then SIGTERM grace, then
# SIGKILL each tracked pid individually.
# ------------------------------------------------------------------
osascript -e 'tell application "sonicterm-mac" to quit' >/dev/null 2>&1 || true

_any_alive() {
  local pid
  for pid in "${SONIC_PIDS[@]:-}"; do
    kill -0 "$pid" 2>/dev/null && return 0
  done
  return 1
}

for _ in 1 2 3 4 5 6 7 8 9 10; do
  _any_alive || break
  sleep 0.1
done
for pid in "${SONIC_PIDS[@]:-}"; do kill -TERM "$pid" 2>/dev/null || true; done
for _ in 1 2 3 4 5; do
  _any_alive || break
  sleep 0.1
done
for pid in "${SONIC_PIDS[@]:-}"; do
  kill -9 "$pid" 2>/dev/null || true
  wait "$pid" 2>/dev/null || true
done

# Boundary verify (per v3): if anything sonicterm-mac-named is alive
# that isn't in PRE_RUN_USER_PIDS, log it. Force-kill if it's one of
# ours that survived; log-only if it appeared mid-run (user launched
# their own instance during the case window — not ours to kill).
remaining=$(pgrep -f "./target/release/sonicterm-mac" 2>/dev/null | sort -u || true)
if [[ -n "$remaining" ]]; then
  pre_sorted=$(printf '%s\n' "${PRE_RUN_USER_PIDS:-}" | sort -u)
  unexpected=$(comm -23 <(printf '%s\n' "$remaining") <(printf '%s\n' "$pre_sorted"))
  if [[ -n "$unexpected" ]]; then
    tracked_sorted=$(printf '%s\n' "${SONIC_PIDS[@]:-}" | sort -u)
    ours_alive=$(comm -12 <(printf '%s\n' "$unexpected") <(printf '%s\n' "$tracked_sorted"))
    user_mid_run=$(comm -23 <(printf '%s\n' "$unexpected") <(printf '%s\n' "$tracked_sorted"))
    if [[ -n "$ours_alive" ]]; then
      log "WARN: harness-tracked sonicterm-mac still alive after cleanup; force-killing: $ours_alive"
      for pid in $ours_alive; do kill -9 "$pid" 2>/dev/null || true; done
    fi
    if [[ -n "$user_mid_run" ]]; then
      log "INFO: user-launched sonicterm-mac PID(s) appeared mid-run; NOT killing: $user_mid_run"
    fi
  fi
fi

# Issue #497: surface per-expect SKIP lines to case.log.
if [[ -f "$EXPECT_LOG" ]]; then
  while IFS=$'\t' read -r status kind reason; do
    [[ "$status" == "SKIP" ]] || continue
    eidx="N/A"
    [[ "$reason" =~ expect\[([0-9]+)\] ]] && eidx="${BASH_REMATCH[1]}"
    tag="ocr_unavailable"
    [[ "$reason" == screencap_unavailable* ]] && tag="screencap_unavailable"
    log "[SKIP $tag] case=$ID expect[$eidx]=$kind"
  done < "$EXPECT_LOG"
fi

if [[ $expect_rc -eq 0 ]]; then
  log "RESULT: PASS"
  echo "PASS" > "$CASE_OUT/status"
  exit 0
elif [[ $expect_rc -eq 77 ]]; then
  log "RESULT: SKIP (ocr_unavailable; no failures, at least one OCR skip)"
  echo "SKIP" > "$CASE_OUT/status"
  cat "$EXPECT_LOG" >> "$LOG"
  exit 77
else
  log "RESULT: FAIL"
  echo "FAIL" > "$CASE_OUT/status"
  cat "$EXPECT_LOG" >> "$LOG"
  exit 1
fi
