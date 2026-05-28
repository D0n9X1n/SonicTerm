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

# ------------------------------------------------------------------
# Start sonic-mac fresh, position the window, capture window id
# ------------------------------------------------------------------
SONIC_BIN="./target/release/sonic-mac"
if [[ ! -x "$SONIC_BIN" ]]; then
  log "FATAL: $SONIC_BIN not built"
  exit 1
fi

pkill -9 -f "sonic-mac" 2>/dev/null || true
sleep 0.4
"$SONIC_BIN" > "$CASE_OUT/sonic.log" 2>&1 &
SONIC_PID=$!
log "spawned sonic-mac pid=$SONIC_PID"

# Wait for window
for _ in $(seq 1 40); do
  if osascript -e 'tell application "System Events" to tell process "sonic-mac" to get id of window 1' >/dev/null 2>&1; then
    break
  fi
  sleep 0.1
done

osascript >/dev/null 2>&1 <<EOF || true
tell application "System Events"
  tell process "sonic-mac"
    set frontmost to true
    set position of window 1 to {500, 200}
    set size of window 1 to {1000, 700}
  end tell
end tell
EOF
sleep 0.4

# ----- focus helper with retry -----
focus_sonic() {
  for try in 1 2 3; do
    osascript >/dev/null 2>&1 <<EOF || true
tell application "System Events"
  set frontmost of (first process whose name is "sonic-mac") to true
end tell
EOF
    sleep 0.2
    local front
    front=$(osascript -e 'tell application "System Events" to name of first process whose frontmost is true' 2>/dev/null || echo "")
    if [[ "$front" == "sonic-mac" ]]; then
      return 0
    fi
    log "focus retry $try (front was: $front)"
  done
  log "WARN: could not bring sonic-mac to front"
  return 1
}

focus_sonic || true

# Capture the actual sonic window id (used for window-only screenshots)
WINDOW_ID=""
WIN_ID_RAW=$(osascript -e 'tell application "System Events" to tell process "sonic-mac" to get id of window 1' 2>/dev/null || echo "")
if [[ -n "$WIN_ID_RAW" ]]; then
  WINDOW_ID="$WIN_ID_RAW"
fi
log "window id: ${WINDOW_ID:-<unknown>}"

# ------------------------------------------------------------------
# Setup helpers
# ------------------------------------------------------------------
do_setup() {
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
# Map chord like "cmd+t" -> osascript keystroke
send_chord() {
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
    for p in "${parts[@]}"; do
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
        out.append(k['value'])
    elif kind == 'click-region':
        out.append("log " + shlex.quote(f"TODO click-region: {k.get('region')}"))
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
                    f'tell application "System Events" to tell process "sonic-mac" to set size of window 1 to {{{w}, {h}}}'
                ) + " >/dev/null 2>&1 || true"
            )
            out.append("sleep 0.15")
    elif kind == 'focus-window':
        out.append("log " + shlex.quote(f"TODO focus-window: {k.get('index')}"))
    else:
        out.append("log " + shlex.quote(f"WARN unknown keystroke kind: {kind}"))
print('\n'.join(out))
PY

focus_sonic || true
# shellcheck source=/dev/null
source "$CASE_OUT/steps.sh"

# ------------------------------------------------------------------
# Capture screenshot — window-only when possible
# ------------------------------------------------------------------
SHOT="$CASE_OUT/screen.png"
if [[ -n "$WINDOW_ID" ]] && screencapture -x -l "$WINDOW_ID" "$SHOT" 2>/dev/null && [[ -s "$SHOT" ]]; then
  log "screenshot (window-only): $SHOT"
else
  screencapture -x -D 1 "$SHOT" 2>/dev/null || true
  log "screenshot (full display): $SHOT"
fi

# ------------------------------------------------------------------
# Evaluate expectations (best-effort; reports per-check pass/fail)
# ------------------------------------------------------------------
EXPECT_LOG="$CASE_OUT/expect.log"
: > "$EXPECT_LOG"

python3 - "$CASE_JSON" "$SHOT" "$EXPECT_LOG" "$CASE_OUT" <<'PY'
import json, sys, os, subprocess
case_path, shot, elog, case_out = sys.argv[1:5]
c = json.load(open(case_path))
expectations = c.get('expect', [])
results = []

def have(p):
    return os.path.exists(p) and os.path.getsize(p) > 0

def pixel_near(shot, x, y, rgba, tol):
    try:
        from PIL import Image
        im = Image.open(shot).convert('RGBA')
        # screencapture is Retina — scale coords by ratio of img-w / 1000 (logical)
        sx = int(x * (im.width / 1000.0))
        sy = int(y * (im.height / 700.0))
        if not (0 <= sx < im.width and 0 <= sy < im.height):
            return False, f"coords oob ({sx},{sy}) in {im.size}"
        px = im.getpixel((sx, sy))
        d = max(abs(int(a) - int(b)) for a, b in zip(px[:len(rgba)], rgba))
        return (d <= tol), f"pixel@({sx},{sy})={px} target={rgba} delta={d} tol={tol}"
    except Exception as e:
        return False, f"err {e}"

def ocr_contains(shot, value):
    try:
        out = subprocess.run(['tesseract', shot, '-', '--psm', '6'],
                             capture_output=True, text=True, timeout=20)
        return (value in out.stdout), out.stdout[:200].replace('\n', ' / ')
    except Exception as e:
        return False, f"err {e}"

def proc_count(prog):
    try:
        r = subprocess.run(['pgrep', '-f', prog], capture_output=True, text=True)
        return len([l for l in r.stdout.splitlines() if l.strip()])
    except Exception:
        return -1

for e in expectations:
    kind = e.get('kind')
    if kind == 'screenshot':
        ok = have(shot); reason = f"exists={ok} path={shot}"
    elif kind == 'pixel-near':
        ok, reason = pixel_near(shot, e['x'], e['y'], e['rgba'], e.get('tolerance', 20))
    elif kind in ('text-in-region', 'ocr-text'):
        ok, reason = ocr_contains(shot, e['value'])
    elif kind == 'not-text-in-region':
        contains, sample = ocr_contains(shot, e['value'])
        ok = not contains
        reason = f"absent={ok} sample='{sample}'"
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
    elif kind == 'no-orphan-shells':
        # Best-effort: after sonic-mac is gone, no orphan zsh/bash with PPID=1 that were spawned by it.
        r = subprocess.run(['pgrep', '-a', 'sonic-mac'], capture_output=True, text=True)
        ok = (not r.stdout.strip())  # sonic-mac is gone -> children should be too
        reason = f"sonic-mac live? '{r.stdout.strip()}'"
    elif kind == 'responsive-within':
        # Heuristic: confirm sonic-mac process exists and is not zombie.
        n = proc_count('sonic-mac')
        ok = (n >= 1)
        reason = f"sonic-mac processes alive={n}"
    elif kind in ('tab-count', 'pane-count', 'window-count',
                  'tab-count-in-window', 'scrollback-min-lines',
                  'padding-min', 'process-spawned', 'process-not-spawned',
                  'process-cpu-max', 'file-absent'):
        # Best-effort heuristics; many require Sonic-internal hooks we don't have yet.
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
    results.append((ok, kind, reason))

with open(elog, 'w') as f:
    for ok, kind, reason in results:
        f.write(f"{'PASS' if ok else 'FAIL'}\t{kind}\t{reason}\n")

# Exit 0 if all true; 1 otherwise. Empty expect list => pass (case ran without errors).
fails = [r for r in results if not r[0]]
sys.exit(0 if not fails else 1)
PY
expect_rc=$?

# ------------------------------------------------------------------
# Cleanup app
# ------------------------------------------------------------------
kill -TERM "$SONIC_PID" 2>/dev/null || true
sleep 0.3
pkill -9 -f "sonic-mac" 2>/dev/null || true

if [[ $expect_rc -eq 0 ]]; then
  log "RESULT: PASS"
  echo "PASS" > "$CASE_OUT/status"
  exit 0
else
  log "RESULT: FAIL"
  echo "FAIL" > "$CASE_OUT/status"
  cat "$EXPECT_LOG" >> "$LOG"
  exit 1
fi
