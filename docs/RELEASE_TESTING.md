# Release Testing Checklist

Canonical UX-release gate. **Every `v*` git tag MUST be preceded by a clean
run of this checklist** against a freshly built `--release` binary on the
target OS. The headless local gate (`cargo test` + `pty_dump` e2e) and the
single-pane GUI smoke in `CLAUDE.md` §13 are necessary but NOT sufficient:
historical regressions that shipped past them include

- Ctrl+W double-press required to close tab (#178-class)
- Tab close (×) hit-test off by one pixel (#181-class)
- Click on inactive tab body didn't activate it (#184-class)
- Split-pane content bled into adjacent pane (#189-class)
- Command palette left-padding regression (#198)
- nvim large-file crash (#194)
- Dropped per-cell ANSI background colors (#161 → P0 #163)
- 100% idle CPU sweep (#31)
- Blank window on startup (#36)
- CJK tofu boxes (#42)

This checklist exists so those classes never ship again. Each section gives:
**setup**, **exact keystrokes**, **expected outcome**, **screenshot path**,
and a **FAIL → block release** annotation. Do NOT self-approve a vague item
— if you cannot point at a specific observation (pixel value, process ID,
diff of behavior), the check has failed.

Screenshots use the convention `/tmp/rel-vN.N.N-<id>.png` so a release
auditor can verify post-hoc.

How to use:

1. Build: `cargo build --release -p sonic-mac` (or `-p sonic-windows`).
2. Run each section in order; tick the `[ ]` only after confirming the
   expected outcome with a real observation.
3. When all boxes are `[x]`, run `bash scripts/check-release-testing.sh`
   — it MUST exit 0 before you push the tag.
4. Commit the checked-off file as part of the release commit:
   `chore(release): v0.8.1 — release testing complete`.

---

## Table of contents

1. [Rendering baseline (single pane)](#1-rendering-baseline-single-pane)
2. [Tab operations](#2-tab-operations)
3. [Pane operations](#3-pane-operations)
4. [Command palette](#4-command-palette)
5. [Preferences window](#5-preferences-window)
6. [Tab tear-out](#6-tab-tear-out)
7. [Big-file stress (vim/nvim)](#7-big-file-stress-vimnvim)
8. [Color / ANSI matrix](#8-color--ansi-matrix)
9. [URL handling (OSC 8)](#9-url-handling-osc-8)
10. [IME / CJK input](#10-ime--cjk-input)
11. [Multi-window](#11-multi-window)
12. [Idle CPU](#12-idle-cpu)
13. [vtebench / cat-large-file perf](#13-vtebench--cat-large-file-perf)
14. [Drag-drop file from Finder](#14-drag-drop-file-from-finder)
15. [Clean quit](#15-clean-quit)

---

## 1. Rendering baseline (single pane)

This is the existing `CLAUDE.md` §13 GUI smoke, kept verbatim as a foundation.

**Setup:** kill any prior `sonic-mac`. Launch fresh:
```bash
pkill -9 -f sonic-mac 2>/dev/null; sleep 0.3
./target/release/sonic-mac > /tmp/gui-smoke.log 2>&1 &
sleep 2.5
```

**Keystrokes:**
```bash
osascript -e 'tell application "System Events" to keystroke "printf '"'"'\\033[41mRED-BG\\033[0m echo 中文 🎉 sonic\\n'"'"' && date"'
osascript -e 'tell application "System Events" to key code 36'
sleep 1
screencapture -x -D 1 /tmp/rel-vX.Y.Z-01-baseline.png
```

**Expected outcome — every one must hold:**

- [ ] Window background pixel value matches `theme.colors.background` (sample with Digital Color Meter).
- [ ] `RED-BG` cells render with a red rectangle (per-cell ANSI bg, #163 regression-guard).
- [ ] `中 文` render as glyphs, not `?` and not tofu boxes (PR #42 regression-guard).
- [ ] 🎉 renders in color (not monochrome silhouette).
- [ ] Cursor is visible and blinks (not blank).
- [ ] Text is sharp on Retina (no upscale blur).
- [ ] `ps -p $(pgrep sonic-mac) -o %cpu` stays < 5% during the 5 s window.

**FAIL → block release.** Any single missed check.

---

## 2. Tab operations

**Setup:** fresh launch (single window, one tab).

**Keystrokes / actions:**
1. Press `Cmd+T` five times → 6 tabs total.
2. Click the **body** (not the ×) of tab 1, then tab 3, then tab 6.
3. Click the × on the active tab.
4. Click the × on an inactive tab to the **left** of the active one.
5. Click the × on an inactive tab to the **right** of the active one.
6. With the right-most tab active, click the right-edge of its body (within 4 px of the trailing edge).
7. Press `Ctrl+W` once.
8. (If reorder is implemented) drag tab 2 past tab 4 and drop.

Screenshot after each non-trivial step: `/tmp/rel-vX.Y.Z-02-tabs-N.png`.

**Expected outcome:**

- [ ] Each `Cmd+T` opens exactly one tab; tab bar widths reflow without overlap.
- [ ] Click on tab body (anywhere ≥ 4 px from the ×) activates that tab (#184-class regression-guard).
- [ ] × on active tab closes it; focus moves to neighbor.
- [ ] × on inactive tab (both sides) closes only that tab; active tab stays active.
- [ ] Right-edge body click on rightmost tab activates it (no off-by-one hit-test, #181-class).
- [ ] `Ctrl+W` once closes the active tab (NOT double-press, #178-class regression-guard).
- [ ] Drag-reorder, if implemented, moves the tab to the dropped slot and preserves tab content + PTY.

**FAIL → block release.**

---

## 3. Pane operations

**Setup:** fresh launch.

**Keystrokes:**
1. `Cmd+D` → splits the current pane to the right.
2. `Cmd+Shift+D` → splits the focused pane downward (verify against current keymap; substitute the bound action).
3. In the top-left pane: `echo PANE-A && seq 1 50`.
4. Click into the right pane: `echo PANE-B && seq 100 150`.
5. Click into the bottom-left pane: `echo PANE-C && yes | head -50`.
6. Drag the splitter between top-left and right pane (if implemented).
7. Resize the window (drag corner) to 1400×900 then back to 1000×700.
8. Close one pane (`Cmd+W` or bound close-pane action).

Screenshots: `/tmp/rel-vX.Y.Z-03-panes-N.png`.

**Expected outcome:**

- [ ] Three panes visible with the correct geometry (one right split, one bottom split).
- [ ] Output in pane A does NOT bleed into pane B or C (#189-class regression-guard — character cells stop at the split border).
- [ ] Clicking each pane focuses it (cursor + border highlight moves).
- [ ] Each pane has an independent PTY (`PANE-A`, `PANE-B`, `PANE-C` strings stay in their respective panes).
- [ ] Resizing the window redistributes splits proportionally; no garbled grid, no panic in `/tmp/gui-smoke.log`.
- [ ] Closing one pane reflows the remaining two to fill the freed space.

**FAIL → block release.**

---

## 4. Command palette

**Setup:** fresh launch.

**Keystrokes:**
1. Open palette (`Cmd+Shift+P` or the bound shortcut).
2. Type `the` to filter.
3. Press `↓` three times, then `↑` once.
4. Press `Enter`.
5. Re-open palette; press `Esc`.

Screenshots: `/tmp/rel-vX.Y.Z-04-palette-N.png`.

**Expected outcome:**

- [ ] Palette overlay appears centered, with a visible left padding ≥ 8 px before the prompt glyph (#198 regression-guard — no flush-left text).
- [ ] Typing filters the action list incrementally.
- [ ] Arrow keys move the highlight; the highlight rectangle covers the **full row width** (no partial-row bug).
- [ ] `Enter` executes the highlighted action and dismisses the overlay.
- [ ] `Esc` dismisses the overlay without executing.

**FAIL → block release.**

---

## 5. Preferences window

**Setup:** fresh launch.

**Keystrokes:**
1. `Cmd+,` → preferences window opens as a **separate** OS window.
2. Click each sidebar entry in order: General → Appearance → Font → Keymap → Behavior.
3. In Appearance, change theme to a different bundled one (e.g. `nord` → `dracula`).
4. Click Apply.
5. Switch focus to main window; type `echo themed`.
6. Close prefs window with its red close button.

Screenshots: `/tmp/rel-vX.Y.Z-05-prefs-N.png`.

**Expected outcome:**

- [ ] Prefs window is a distinct OS window (separate entry in Cmd+Tab and Mission Control).
- [ ] Every sidebar entry renders its pane without missing controls or overflow.
- [ ] Theme change takes effect in the main window **without restart** (background pixel changes; sample with Digital Color Meter).
- [ ] Closing prefs window does NOT close the main window.

**FAIL → block release.**

---

## 6. Tab tear-out

**Setup:** fresh launch; open 3 tabs (`Cmd+T` twice).

**Actions:**
1. Drag tab 2 by its body OUT of the tab bar, past the window edge, and release.
2. New window appears containing that tab.
3. In the new window press `Cmd+T`.
4. Close the new window via its red close button.

Screenshots: `/tmp/rel-vX.Y.Z-06-tearout-N.png`.

**Expected outcome:**

- [ ] Dragging spawns a new top-level window owning the torn-out tab (PTY and scrollback intact — confirm by checking last command output is preserved).
- [ ] Original window now has 2 tabs; torn-out tab is no longer in it.
- [ ] `Cmd+T` in the new window opens a tab IN that new window (NOT back in the original).
- [ ] Closing the new window does not kill the original.
- [ ] No orphan PTY: `pgrep -f sonic-mac` count matches visible windows.

**FAIL → block release.**

---

## 7. Big-file stress (vim/nvim)

**Setup:** fresh launch; single pane.

**Commands (inside Sonic):**
```bash
yes "the quick brown fox jumps over the lazy dog 0123456789" | head -2000000 > /tmp/big.txt
ls -lh /tmp/big.txt   # should be ≥ 50 MB
nvim /tmp/big.txt
```

**Inside nvim:**
1. Press `Ctrl+D` 20 times, then `Ctrl+U` 20 times.
2. `gg` to top; `G` to bottom.
3. `:%s/the/THE/g` then `<Enter>`.
4. `:wq!` to save and quit (or `:q!` to discard).

Then in the shell: `ps -p $(pgrep sonic-mac) -o %cpu` and wait 5 s, sample again.

Screenshots: `/tmp/rel-vX.Y.Z-07-nvim-N.png`.

**Expected outcome:**

- [ ] Sonic does NOT crash during open, scroll, or substitution (#194 regression-guard).
- [ ] No tofu boxes appear at any point.
- [ ] Substitution completes (`:%s` reports replacements).
- [ ] After nvim exits, idle CPU returns to < 5% within 5 s.
- [ ] Grid is in a clean state (prompt visible, cursor visible).

**FAIL → block release.**

---

## 8. Color / ANSI matrix

**Setup:** fresh launch.

**Commands:**
```bash
# 256-color demo
for i in {0..255}; do printf "\033[48;5;${i}m %3d \033[0m" $i; (( (i+1) % 16 == 0 )) && echo; done

# Truecolor RGB demo (gradient)
awk 'BEGIN{ for(i=0;i<80;i++){r=255-i*3;g=i*3;b=128; printf "\033[48;2;%d;%d;%dm \033[0m",r,g,b} print ""}'

# Attributes
printf '\033[1mBOLD\033[0m \033[3mITALIC\033[0m \033[4mUNDER\033[0m \033[1;3;4mALL\033[0m\n'

# Per-cell bg (#163 guard)
printf '\033[41m RED-BG \033[42m GREEN-BG \033[44m BLUE-BG \033[0m\n'

# htop-ish stripe — install if missing
htop  # or btop
# In htop: arrow-down to highlight a process row; the row stripe must be its own bg color.
```

Screenshots: `/tmp/rel-vX.Y.Z-08-ansi-N.png`.

**Expected outcome:**

- [ ] 256-color grid renders all 256 cells with distinct bg colors (no gaps, no black squares mid-grid).
- [ ] Truecolor gradient is smooth (no banding beyond 24-bit limit).
- [ ] BOLD weight is visibly heavier; ITALIC is slanted; UNDER has a single-pixel underline; ALL combines.
- [ ] RED-BG / GREEN-BG / BLUE-BG cells render with their explicit per-cell bg color (#163 P0 regression-guard).
- [ ] In htop/btop, the highlighted-row stripe uses the selection bg color, not the theme bg.

**FAIL → block release.**

---

## 9. URL handling (OSC 8)

**Setup:** fresh launch.

**Commands:**
```bash
printf '\e]8;;https://example.com\e\\click-me\e]8;;\e\\\n'
printf '\e]8;;mailto:test@example.com\e\\email-link\e]8;;\e\\\n'
# Should be silently rejected — URL must not open, no crash:
printf '\e]8;;javascript:alert(1)\e\\bad-scheme\e]8;;\e\\\n'
printf '\e]8;;file:///etc/passwd\e\\file-link\e]8;;\e\\\n'
```

**Actions:**
1. Hover over `click-me` — underline tint should brighten.
2. `Cmd+click` `click-me` → opens in default browser.
3. `Cmd+click` `email-link` → opens default mail client.
4. `Cmd+click` `bad-scheme` → nothing happens, no error popup.

Screenshots: `/tmp/rel-vX.Y.Z-09-url-N.png`.

**Expected outcome:**

- [ ] Hyperlink text is visually distinguished (underline or color tint).
- [ ] `Cmd+click` on `https://` opens the browser to example.com.
- [ ] `Cmd+click` on `mailto:` opens the mail client.
- [ ] `Cmd+click` on `javascript:` does NOT execute anything (CLAUDE.md §4 `url_open::validate` guard).
- [ ] `file://` is allowed per allow-list (opens Finder) — confirm against current policy in `crates/sonic-cfg/src/url_open.rs`.
- [ ] No process spawn for denylisted control characters or unknown schemes.

**FAIL → block release.**

---

## 10. IME / CJK input

**Setup:** fresh launch. Add a CJK IME in System Settings → Keyboard → Input Sources (e.g. Pinyin – Simplified or Japanese – Romaji) if not already present.

**Actions:**
1. Switch to the CJK IME (Caps Lock or Ctrl+Space depending on config).
2. In the terminal, type `nihao` (Pinyin) or `konnichiha`.
3. Observe the preedit composition overlay at the cursor.
4. Press space / number to commit.
5. Repeat for one Japanese phrase.

Screenshots: `/tmp/rel-vX.Y.Z-10-ime-N.png`.

**Expected outcome:**

- [ ] Preedit overlay appears at the cursor cell (not at window origin, not at last-click location).
- [ ] Preedit shows the raw romaji + candidate glyphs in the IME's own popover.
- [ ] Committing inserts the CJK glyphs into the grid at the cursor.
- [ ] Backspace deletes the committed glyph by code-point, not by byte.
- [ ] Wide CJK cells occupy 2 cells; cursor advances by 2.

**FAIL → block release.**

---

## 11. Multi-window

**Setup:** fresh launch (1 window).

**Actions:**
1. `Cmd+N` twice → 3 windows total.
2. In each, type a distinct command (`echo W1`, `echo W2`, `echo W3`).
3. Close window 2 with its red close button.
4. Verify W1 and W3 still respond.

Screenshots: `/tmp/rel-vX.Y.Z-11-multiwin-N.png`.

**Expected outcome:**

- [ ] Each `Cmd+N` opens a new OS window with its own PTY + grid (output is independent).
- [ ] Closing window 2 does not affect W1 or W3.
- [ ] `pgrep sonic-mac` count matches visible windows (process model — should still be a single app process, but no orphan PTY children).

**FAIL → block release.**

---

## 12. Idle CPU

**Setup:** fresh launch; single pane; do NOT touch keyboard/mouse for 30 s.

**Measurement:**
```bash
sleep 30
ps -p $(pgrep sonic-mac | head -1) -o %cpu
```

**Expected outcome:**

- [ ] %CPU < 1.0 across 3 successive samples 2 s apart (#31 regression-guard — no idle CPU sweep).

**FAIL → block release.**

---

## 13. vtebench / cat-large-file perf

**Setup:** built release binary; vtebench installed.

**Commands:**
```bash
./scripts/bench_compare.sh   # or scripts/gui_bench.sh, whichever is the perf-gate entry point per #202
```

**Expected outcome:**

- [ ] No benchmark regresses more than **20%** vs the last tagged release baseline recorded in `scripts/baselines/`.
- [ ] No benchmark crashes or hangs.
- [ ] Honest perf-parity note in PR body / release notes references actual numbers (do NOT claim "fast" without measurement, per CLAUDE.md §14).

**FAIL → block release** (or document a per-benchmark exemption with PM sign-off).

---

## 14. Drag-drop file from Finder

**Setup:** fresh launch; focus a shell prompt.

**Action:** drag a file (e.g. `~/Downloads/some file.txt`) from Finder onto the terminal window and release.

**Expected outcome:**

- [ ] Sonic pastes the absolute path of the file at the cursor.
- [ ] Spaces in the path are properly shell-quoted (either single-quoted whole, or backslash-escaped) — the shell must accept the path verbatim on `Enter`.
- [ ] No crash; no path written to a different pane than the focused one.

**FAIL → block release.**

---

## 15. Clean quit

**Setup:** Sonic running with 2 windows × 2 tabs × 2 panes each (8 shells total).

**Actions:**
1. Note shell PIDs: `pgrep -fl '/bin/zsh|/bin/bash' | tee /tmp/rel-shells-before.txt`.
2. `Cmd+Q` to quit.
3. `sleep 1` then check: `pgrep -fl '/bin/zsh|/bin/bash' | tee /tmp/rel-shells-after.txt`.
4. `diff /tmp/rel-shells-before.txt /tmp/rel-shells-after.txt`.

**Expected outcome:**

- [ ] `Cmd+Q` exits the app cleanly within ~1 s (no spinning beachball).
- [ ] All 8 shell PIDs spawned by Sonic are gone from the diff (`PtyHandle::Drop` correctness, CLAUDE.md §4).
- [ ] `pgrep -f sonic-mac` returns nothing.

**FAIL → block release.**

---

## Sign-off

Tag: `v___________`
Run by: `___________`
Date: `___________`
Platform: `___________` (macOS 14.x / Windows 11 23H2)

All 15 sections passing → `bash scripts/check-release-testing.sh && git tag vX.Y.Z && git push origin vX.Y.Z`.
