#!/usr/bin/env bash
# visual_diff.sh — reproducible side-by-side capture of WezTerm vs SonicTerm
# for visual-parity regression checking.
#
# Output:
#   /tmp/parity-wezterm.png    full WezTerm window screenshot
#   /tmp/parity-sonic.png      full SonicTerm window screenshot
#   /tmp/parity-wezterm-crop.png   content area (titlebar removed via sips)
#   /tmp/parity-sonic-crop.png     same
#
# Eyeball diff. No imagemagick required.
#
# Requirements (macOS):
#   - wezterm     (brew install --cask wezterm)
#   - sonicterm-mac   (cargo build --release -p sonicterm-mac, then we use the binary)
#   - osascript   (built-in)
#   - screencapture (built-in)
#   - sips        (built-in, optional crop step)
#
# Usage:
#   bash scripts/visual_diff.sh                # uses default payload
#   PAYLOAD_FILE=/path/to/file bash scripts/visual_diff.sh
#
# What this does NOT do:
#   - Pixel-diff. We compare by eye; see docs/VISUAL_PARITY.md for the axes.
#   - Match fonts automatically — set both terminals to JetBrainsMono Nerd Font 14pt
#     in their own configs before running for an apples-to-apples test.

set -euo pipefail

# ---------------------------------------------------------------------------
# Configuration — tweak these if your display is small.
# ---------------------------------------------------------------------------
WIN_W=${WIN_W:-900}
WIN_H=${WIN_H:-600}
WEZ_X=${WEZ_X:-40}
WEZ_Y=${WEZ_Y:-80}
SONIC_X=${SONIC_X:-980}     # WEZ_X + WIN_W + 40 gutter
SONIC_Y=${SONIC_Y:-80}
TITLEBAR_PX=${TITLEBAR_PX:-28}  # macOS standard titlebar height for sips crop

SONIC_BIN=${SONIC_BIN:-"$(pwd)/target/release/sonicterm-mac"}
WEZTERM_BIN=${WEZTERM_BIN:-"$(command -v wezterm || true)"}

PAYLOAD_FILE=${PAYLOAD_FILE:-}

# ---------------------------------------------------------------------------
# Default visual-parity payload. Exercises the axes documented in
# docs/VISUAL_PARITY.md: ANSI colors, bold/italic/underline, CJK wide chars,
# emoji, box-drawing, hyperlinks (OSC 8), and a prompt-style line.
# ---------------------------------------------------------------------------
default_payload() {
  cat <<'PAYLOAD'
clear
printf '\033[1;31mRED-BOLD\033[0m \033[3;32mGREEN-ITALIC\033[0m \033[4;34mBLUE-UL\033[0m\n'
printf 'ANSI 16: '; for c in 30 31 32 33 34 35 36 37; do printf "\033[${c}m##\033[0m"; done; echo
printf 'CJK: 你好世界 こんにちは 안녕하세요  Emoji: 🦀🚀✨\n'
printf 'Box: ┌─┬─┐\n     │ │ │\n     └─┴─┘\n'
printf '\033]8;;https://example.com\033\\hyperlink\033]8;;\033\\\n'
PS1='%~ %% ' ; echo "user@host ~/sonic %"
PAYLOAD
}

PAYLOAD=$(if [[ -n "$PAYLOAD_FILE" ]]; then cat "$PAYLOAD_FILE"; else default_payload; fi)

# Materialize payload as a fixture so we can `bash <file>` it via keystroke
# instead of clobbering the user's pbcopy buffer (see #474 Bug A).
PAYLOAD_FIXTURE=$(mktemp -t visual_diff_payload)
printf %s "$PAYLOAD" > "$PAYLOAD_FIXTURE"
trap 'rm -f "$PAYLOAD_FIXTURE"; pkill -9 -f "$SONIC_BIN" 2>/dev/null || true' EXIT  # #548

# ---------------------------------------------------------------------------
# Guard: refuse if a NON-WezTerm competing terminal is running. WezTerm is
# the intentional comparison target here, but iTerm/kitty/etc. would steal
# keystrokes meant for sonicterm-mac. Override with
# SONICTERM_HARNESS_ALLOW_OTHER_TERMS=1. See issues #464, #473.
# ---------------------------------------------------------------------------
COMPETITOR_RE='^(Terminal|iTerm|iTerm2|kitty|alacritty|ghostty|Hyper|Warp|tabby|rio)$'
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

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------
have() { command -v "$1" >/dev/null 2>&1; }

position_front_window() {
  local app_name="$1" x="$2" y="$3"
  /usr/bin/osascript <<EOF
tell application "System Events"
  tell process "$app_name"
    set frontmost to true
    delay 0.4
    if (count of windows) > 0 then
      set position of front window to {$x, $y}
      set size of front window to {$WIN_W, $WIN_H}
    end if
  end tell
end tell
EOF
}

paste_payload() {
  local app_name="$1"
  # Pre-keystroke focus gate — abort the paste if $app_name isn't frontmost
  # after up to 5 retries, instead of leaking keystrokes into whatever else
  # has focus. See issue #473. (See PR #523 for the clipboard + screencap fixes.)
  local front try
  for try in 1 2 3 4 5; do
    /usr/bin/osascript >/dev/null 2>&1 <<EOF || true
tell application "System Events"
  set frontmost of (first process whose name is "$app_name") to true
end tell
EOF
    sleep 0.25
    front=$(/usr/bin/osascript -e 'tell application "System Events" to name of first process whose frontmost is true' 2>/dev/null || echo "")
    [[ "$front" == "$app_name" ]] && break
  done
  if [[ "$front" != "$app_name" ]]; then
    echo "warn: $app_name not frontmost (front=$front); skipping paste" >&2
    return 0
  fi
  /usr/bin/osascript <<EOF
tell application "System Events"
  tell process "$app_name"
    set frontmost to true
    delay 0.3
    keystroke "bash $PAYLOAD_FIXTURE"
    delay 0.2
    key code 36   -- return
  end tell
end tell
EOF
  sleep 1.2
}

# Look up the front window id for a given process; empty on failure.
window_id_for() {
  local app_name="$1"
  /usr/bin/osascript -e "tell application \"System Events\" to tell process \"$app_name\" to get id of front window" 2>/dev/null || true
}

# Window-local capture (mirrors testing/workflows/run_case.sh; see #474 Bug B).
shoot_window() {
  local out="$1" win_id="$2"
  if [[ -z "$win_id" ]]; then
    echo "warn: no window id; skipping screencap for $out" >&2
    return 0
  fi
  /usr/sbin/screencapture -x -l "$win_id" "$out"
}

crop_titlebar() {
  local in="$1" out="$2"
  if have sips; then
    # height = WIN_H * scale - TITLEBAR_PX * scale; sips uses pixel dims so
    # we let it figure scaling out by passing fractions of total height.
    local h
    h=$(sips -g pixelHeight "$in" | awk '/pixelHeight/ {print $2}')
    local w
    w=$(sips -g pixelWidth "$in" | awk '/pixelWidth/ {print $2}')
    local scale=$(( h / WIN_H ))
    [[ $scale -lt 1 ]] && scale=1
    local crop_top=$(( TITLEBAR_PX * scale ))
    local new_h=$(( h - crop_top ))
    sips --cropToHeightWidth "$new_h" "$w" --cropOffset "$crop_top" 0 "$in" --out "$out" >/dev/null
  else
    cp "$in" "$out"
  fi
}

# ---------------------------------------------------------------------------
# Sanity checks
# ---------------------------------------------------------------------------
if [[ -z "$WEZTERM_BIN" ]]; then
  echo "warn: wezterm not installed; will only capture SonicTerm" >&2
fi
if [[ ! -x "$SONIC_BIN" ]]; then
  echo "error: sonic binary not found at $SONIC_BIN" >&2
  echo "       build with: cargo build --release -p sonicterm-mac" >&2
  exit 1
fi

# ---------------------------------------------------------------------------
# Launch
# ---------------------------------------------------------------------------
if [[ -n "$WEZTERM_BIN" ]]; then
  "$WEZTERM_BIN" start --always-new-process >/dev/null 2>&1 &
  sleep 1.5
  position_front_window "WezTerm" "$WEZ_X" "$WEZ_Y" || true
  paste_payload "WezTerm" || true
fi

"$SONIC_BIN" >/dev/null 2>&1 &
SONIC_PID=$!
sleep 1.5
# SonicTerm's process name in System Events is "sonicterm-mac"
position_front_window "sonicterm-mac" "$SONIC_X" "$SONIC_Y" || true
paste_payload "sonicterm-mac" || true

# Give both a beat to settle
sleep 0.8

# ---------------------------------------------------------------------------
# Capture
# ---------------------------------------------------------------------------
if [[ -n "$WEZTERM_BIN" ]]; then
  WEZ_WIN_ID="$(window_id_for WezTerm)"
  if [[ -z "$WEZ_WIN_ID" ]]; then
    echo "warn: no WezTerm front window id; soft-skipping visual_diff" >&2
    exit 77
  fi
  shoot_window /tmp/parity-wezterm.png "$WEZ_WIN_ID"
  crop_titlebar /tmp/parity-wezterm.png /tmp/parity-wezterm-crop.png
fi
SONIC_WIN_ID="$(window_id_for sonicterm-mac)"
if [[ -z "$SONIC_WIN_ID" ]]; then
  echo "warn: no sonicterm-mac front window id; soft-skipping visual_diff" >&2
  exit 77
fi
shoot_window /tmp/parity-sonic.png "$SONIC_WIN_ID"
crop_titlebar /tmp/parity-sonic.png /tmp/parity-sonic-crop.png

echo "Captured:"
ls -l /tmp/parity-*.png

cat <<MSG

Next: open the two crop PNGs side-by-side (Preview) and walk down
docs/VISUAL_PARITY.md. Differences in titlebar style are EXPECTED
(OS-controlled). Anything below the titlebar is a real parity gap.

To clean up the running terminals:
  kill $SONIC_PID 2>/dev/null
  osascript -e 'tell application "WezTerm" to quit' 2>/dev/null
MSG
