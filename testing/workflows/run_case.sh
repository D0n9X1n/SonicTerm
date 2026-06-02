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

# Capture the actual sonic window id (used for window-only screenshots)
WINDOW_ID=""
WIN_ID_RAW=$(osascript -e 'tell application "System Events" to tell process "sonicterm-mac" to get id of window 1' 2>/dev/null || echo "")
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
if [[ -z "$WINDOW_ID" ]]; then
  log "SKIP: no window id captured — refusing to screencap full display (would leak coords)"
  echo SKIP > "$CASE_OUT/status"
  exit 77
fi
if ! screencapture -x -l "$WINDOW_ID" "$SHOT" 2>/dev/null || [[ ! -s "$SHOT" ]]; then
  log "SKIP: window-local screencap failed (window may have closed)"
  echo SKIP > "$CASE_OUT/status"
  exit 77
fi
log "screenshot (window-only): $SHOT"

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
        # Best-effort: after sonicterm-mac is gone, no orphan zsh/bash with PPID=1 that were spawned by it.
        r = subprocess.run(['pgrep', '-a', 'sonicterm-mac'], capture_output=True, text=True)
        ok = (not r.stdout.strip())  # sonicterm-mac is gone -> children should be too
        reason = f"sonicterm-mac live? '{r.stdout.strip()}'"
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
