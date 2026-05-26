#!/usr/bin/env bash
# visual_diff.sh — reproducible side-by-side capture of WezTerm vs Sonic
# for visual-parity regression checking.
#
# Output:
#   /tmp/parity-wezterm.png    full WezTerm window screenshot
#   /tmp/parity-sonic.png      full Sonic window screenshot
#   /tmp/parity-wezterm-crop.png   content area (titlebar removed via sips)
#   /tmp/parity-sonic-crop.png     same
#
# Eyeball diff. No imagemagick required.
#
# Requirements (macOS):
#   - wezterm     (brew install --cask wezterm)
#   - sonic-mac   (cargo build --release -p sonic-mac, then we use the binary)
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

SONIC_BIN=${SONIC_BIN:-"$(pwd)/target/release/sonic-mac"}
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
  printf %s "$PAYLOAD" | /usr/bin/pbcopy
  /usr/bin/osascript <<EOF
tell application "System Events"
  tell process "$app_name"
    set frontmost to true
    delay 0.3
    keystroke "v" using {command down}
    delay 0.2
    key code 36   -- return
  end tell
end tell
EOF
  sleep 1.2
}

# screencapture -R x,y,w,h of region; macOS scales for Retina automatically
shoot_region() {
  local out="$1" x="$2" y="$3"
  /usr/sbin/screencapture -x -R "${x},${y},${WIN_W},${WIN_H}" "$out"
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
  echo "warn: wezterm not installed; will only capture Sonic" >&2
fi
if [[ ! -x "$SONIC_BIN" ]]; then
  echo "error: sonic binary not found at $SONIC_BIN" >&2
  echo "       build with: cargo build --release -p sonic-mac" >&2
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
# Sonic's process name in System Events is "sonic-mac"
position_front_window "sonic-mac" "$SONIC_X" "$SONIC_Y" || true
paste_payload "sonic-mac" || true

# Give both a beat to settle
sleep 0.8

# ---------------------------------------------------------------------------
# Capture
# ---------------------------------------------------------------------------
if [[ -n "$WEZTERM_BIN" ]]; then
  shoot_region /tmp/parity-wezterm.png "$WEZ_X" "$WEZ_Y"
  crop_titlebar /tmp/parity-wezterm.png /tmp/parity-wezterm-crop.png
fi
shoot_region /tmp/parity-sonic.png "$SONIC_X" "$SONIC_Y"
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
