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
16. [Scrollback / select / copy](#16-scrollback--select--copy)
17. [Search overlay](#17-search-overlay)
18. [Resize semantics (SIGWINCH)](#18-resize-semantics-sigwinch)
19. [HiDPI / multi-monitor](#19-hidpi--multi-monitor)
20. [Theme + font live-reload](#20-theme--font-live-reload)
21. [Shell exit / kill behavior](#21-shell-exit--kill-behavior)
22. [Ctrl-letter / modifier key encoding](#22-ctrl-letter--modifier-key-encoding)
23. [Alt-screen round-trip](#23-alt-screen-round-trip)
24. [OSC 8 + URL safety extended](#24-osc-8--url-safety-extended)
25. [Mouse reporting modes](#25-mouse-reporting-modes)
26. [Wide chars / grapheme clusters](#26-wide-chars--grapheme-clusters)
27. [Cursor styles (DECSCUSR)](#27-cursor-styles-decscusr)
28. [Session restore (deferred, N/A)](#28-session-restore-deferred-na)
29. [Crash hygiene](#29-crash-hygiene)
30. [Accessibility](#30-accessibility)
31. [First-run experience](#31-first-run-experience)
32. [Locale / non-UTF8](#32-locale--non-utf8)
33. [Permissions / TCC prompts](#33-permissions--tcc-prompts)
34. [Long-running (1 hour) stability](#34-long-running-1-hour-stability)
35. [Drag-drop edge cases](#35-drag-drop-edge-cases)
36. [Config validation](#36-config-validation)
37. [CLAUDE.md §4 land-mine coverage](#37-claudemd-4-land-mine-coverage)

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

## 16. Scrollback / select / copy

**Setup:** fresh launch; single pane.

**Commands / actions:**
1. `seq 1 5000` to fill scrollback.
2. Scroll back with trackpad two-finger drag / mouse wheel / `Shift+PageUp` to the top.
3. Click at line `123`, shift-click at line `456` to select a multi-row block.
4. `Cmd+C` to copy.
5. In a separate app (TextEdit), `Cmd+V` to paste.
6. Re-select using a triple-click (whole-line selection) and copy.
7. Drag-select across a wrapped line (a line containing `123456789` repeated 30×).
8. `Cmd+Shift+K` (or bound clear-scrollback action) → scrollback flushed.

Screenshots: `/tmp/rel-vX.Y.Z-16-scroll-N.png`.

**Expected outcome:**

- [ ] Scrollback retains all 5000 lines (scroll-to-top shows line 1).
- [ ] Selection highlight uses theme selection bg (not glitchy / not invisible).
- [ ] Copied text matches selection exactly, with correct newlines (LF, no CRLF surprise).
- [ ] Triple-click selects the full logical line including wrapped continuations.
- [ ] Wrapped-line copy yields a single logical line in the clipboard (no spurious newline at the wrap point).
- [ ] Clear-scrollback empties the buffer; scroll-up no longer goes anywhere.

**FAIL → block release.**

---

## 17. Search overlay

**Setup:** fresh launch; populate scrollback with `seq 1 2000; echo NEEDLE; seq 1 500; echo needle; seq 1 200; echo Needle`.

**Actions:**
1. Open search (`Cmd+F` or bound action).
2. Type `needle`.
3. Press `Enter` (or arrow) to step to next match; cycle through all.
4. Toggle case-sensitive (if available); confirm only `needle` lowercase matches.
5. `Esc` to dismiss.
6. Re-open, search for a term that does NOT exist (`zzznomatchzzz`).

Screenshots: `/tmp/rel-vX.Y.Z-17-search-N.png`.

**Expected outcome:**

- [ ] Overlay renders centered or anchored per design; input is focused.
- [ ] All three matches are visibly highlighted in the grid (different bg).
- [ ] Stepping scrolls the viewport so the active match is visible.
- [ ] Active match has a distinct highlight from other matches.
- [ ] Case-sensitive toggle filters matches correctly.
- [ ] No-match state shows a clear "no results" indicator and does not scroll.
- [ ] `Esc` removes overlay and clears highlights.

**FAIL → block release.**

---

## 18. Resize semantics (SIGWINCH)

**Setup:** fresh launch; run `python3 -c "import shutil,time; [print(shutil.get_terminal_size()) or time.sleep(1) for _ in range(60)]"` in a pane.

**Actions:**
1. Drag the window corner to grow ~+200 px in both dimensions.
2. Drag to shrink ~-300 px in both dimensions.
3. Maximize (green button → Zoom, NOT full-screen).
4. Toggle full-screen (`Ctrl+Cmd+F`).
5. Exit full-screen.
6. While running `htop`, resize repeatedly during paint.

Screenshots: `/tmp/rel-vX.Y.Z-18-resize-N.png`.

**Expected outcome:**

- [ ] Each resize prints a new `os.terminal_size(columns=..., lines=...)` matching the visible grid dimensions.
- [ ] No SIGWINCH dropouts (the printout updates within 1 s of resize ending).
- [ ] htop redraws without garbage; no stray cells outside the new viewport.
- [ ] Full-screen toggle does not crash and grid fills the whole display.
- [ ] No panic in `/tmp/gui-smoke.log` referencing surface configure / wgpu textures (CLAUDE.md §4 Suboptimal guard).

**FAIL → block release.**

---

## 19. HiDPI / multi-monitor

**Setup:** machine with at least one Retina display; second external monitor at a different scale factor if available.

**Actions:**
1. Launch Sonic on the Retina display. Confirm text sharpness against Section 1.
2. Drag the window to the external monitor.
3. Drag back to the Retina display.
4. If macOS has "scaled" resolution options, change display scale via System Settings → Displays.
5. Take a 200% zoom screenshot of a glyph (`screencapture -x -R …`).

Screenshots: `/tmp/rel-vX.Y.Z-19-hidpi-N.png`.

**Expected outcome:**

- [ ] Glyphs render at native pixel density on Retina (no upscale blur).
- [ ] Moving between monitors does not leave a stale low-DPI bitmap.
- [ ] Display-scale change refreshes the grid sharply within 2 s.
- [ ] No panic in `/tmp/gui-smoke.log` referencing scale_factor.
- [ ] Cursor and selection highlight align to whole-pixel boundaries.

**FAIL → block release.**

---

## 20. Theme + font live-reload

**Setup:** fresh launch; have the config file path ready (`~/Library/Application Support/Sonic/sonic.toml` on macOS).

**Actions:**
1. Edit config to change theme (e.g. `tokyo-night` → `gruvbox-dark-hard`); save.
2. Within 2 s, confirm main window background swaps.
3. Edit config to change font family to a known-installed font (e.g. `St Helens` → `Menlo`); save.
4. Confirm grid re-shapes with the new font (cell metrics update).
5. Set font_size 14 → 18; save; confirm cell size increases.
6. Introduce a deliberate typo (`themee = "..."`); save; confirm app does NOT crash and surfaces an error (log or in-app banner).

Screenshots: `/tmp/rel-vX.Y.Z-20-livereload-N.png`.

**Expected outcome:**

- [ ] Theme change applies without restart and without window flicker beyond a single repaint.
- [ ] Font family change applies live; no tofu in the post-reload screenshot.
- [ ] Font size change rescales the grid and reflows the active shell (SIGWINCH fires).
- [ ] Bad config does NOT crash Sonic; previous good config remains in effect; error is logged to `/tmp/gui-smoke.log`.

**FAIL → block release.**

---

## 21. Shell exit / kill behavior

**Setup:** fresh launch; 2 tabs, each with 2 panes (4 shells).

**Actions:**
1. In pane 1, type `exit` + Enter.
2. In pane 2, type `kill -9 $$` + Enter.
3. In pane 3, type `sleep 99999` + Enter, then send `Ctrl+C`.
4. In pane 4, type `cat`, then send EOF (`Ctrl+D`) on an empty line.
5. Quit the app (`Cmd+Q`) and check for orphan shells.

Screenshots: `/tmp/rel-vX.Y.Z-21-shellexit-N.png`.

**Expected outcome:**

- [ ] `exit` closes the pane (or replaces with a "process exited" indicator per design); no zombie.
- [ ] `kill -9 $$` likewise — the parent shell death is noticed within 1 s.
- [ ] `Ctrl+C` interrupts the sleep without killing the shell; prompt returns.
- [ ] `Ctrl+D` on empty `cat` terminates cat; shell stays alive.
- [ ] After `Cmd+Q`: `pgrep -fl '/bin/zsh|/bin/bash'` shows none of the Sonic-spawned PIDs (PtyHandle::Drop, CLAUDE.md §4).

**FAIL → block release.**

---

## 22. Ctrl-letter / modifier key encoding

**Setup:** fresh launch; run `cat -v` (so control bytes are visible).

**Actions:** press each in sequence and observe `cat -v` output:
1. `Ctrl+A` through `Ctrl+Z` (expect `^A` … `^Z`, with `^J` = newline behavior).
2. `Ctrl+[` (`^[` = ESC), `Ctrl+\` (`^\`), `Ctrl+]` (`^]`), `Ctrl+/`, `Ctrl+_`.
3. `Alt+A` / `Option+A` — should emit either `^[a` (meta-prefix) or `å` (macOS default), per configured `option_as_meta`.
4. `Shift+Tab` (expect `^[[Z`).
5. Arrow keys (`^[[A` etc.), Home/End, PageUp/PageDown.
6. `Ctrl+Space` (NUL: `^@`).

Exit `cat` with `Ctrl+D` on a fresh line.

Screenshots: `/tmp/rel-vX.Y.Z-22-keys-N.png`.

**Expected outcome:**

- [ ] Every Ctrl+letter emits the canonical control byte exactly once.
- [ ] Option-as-meta works per config (no silently swallowed bindings, no double-emit).
- [ ] Shift+Tab and arrows emit the correct CSI sequence (not raw text).
- [ ] No key combo triggers an unintended app-level action that swallows the input.

**FAIL → block release.**

---

## 23. Alt-screen round-trip

**Setup:** fresh launch.

**Actions:**
1. Run `echo MAIN-LINE-1; echo MAIN-LINE-2`.
2. `vim` (or `less /etc/hosts`).
3. Inside, scroll, type, do anything that paints the alt screen.
4. `:q` (vim) / `q` (less).
5. Confirm the original `MAIN-LINE-1` / `MAIN-LINE-2` output is RESTORED, cursor is on the next prompt line.
6. Re-enter `vim`, then re-enter without exiting (`:vsplit` etc.) — ensure DECSET 1049 idempotence (CLAUDE.md §4 guard).
7. Exit and re-check.

Screenshots: `/tmp/rel-vX.Y.Z-23-altscreen-N.png`.

**Expected outcome:**

- [ ] Entering alt screen hides the main scrollback content.
- [ ] Exiting restores main scrollback exactly; no overwrite, no missing rows.
- [ ] Repeated `?1049h` is a no-op; saved cursor not clobbered (`dec_1049h_repeated_does_not_clobber_saved_cursor` test mirror).
- [ ] Scrollback while in alt screen is either scoped to the alt buffer or disabled per design; never bleeds into main.

**FAIL → block release.**

---

## 24. OSC 8 + URL safety extended

**Setup:** fresh launch.

**Commands:**
```bash
# Allowed schemes
printf '\e]8;;https://example.com/path?q=1\e\\OK-https\e]8;;\e\\\n'
printf '\e]8;;http://example.com\e\\OK-http\e]8;;\e\\\n'

# Denied schemes (must NOT spawn anything)
for url in 'javascript:alert(1)' 'data:text/html,<script>' 'vbscript:msgbox' \
           'ssh://x;rm -rf /' 'http://x.com" && calc.exe' \
           'http://x.com`whoami`' 'http://x.com$(whoami)' \
           "http://x.com'; echo PWN" "http://x.com\nDROP TABLE"; do
  printf '\e]8;;%s\e\\BAD\e]8;;\e\\\n' "$url"
done

# Over-length (>4096) — must be rejected silently
python3 -c "print('\\x1b]8;;' + 'http://x.com/' + 'A'*5000 + '\\x1b\\\\LONG\\x1b]8;;\\x1b\\\\')"
```

**Actions:** `Cmd+click` each link in turn.

Screenshots: `/tmp/rel-vX.Y.Z-24-urlsafety-N.png`.

**Expected outcome:**

- [ ] Only allow-listed schemes (`http`, `https`, `mailto`, `file`) open anything.
- [ ] All denied URLs do nothing (no browser launch, no crash, no shell injection).
- [ ] No process spawned with shell metacharacters in argv (verify via `ps` snapshot).
- [ ] Over-length URI is rejected silently.
- [ ] `crates/sonic-cfg/src/url_open.rs::validate` is the single gatekeeper (CLAUDE.md §4).

**FAIL → block release.**

---

## 25. Mouse reporting modes

**Setup:** fresh launch.

**Commands:**
```bash
# Tmux exercises 1006 (SGR) and 1002 (button-event) modes
tmux
# Inside tmux: split, click panes, drag to resize splits.

# Vim mouse mode
vim
:set mouse=a
# Click to position cursor; drag to visual-select.

# Raw enable/disable
printf '\e[?1000h'   # X10 button reporting
# Click; should see escape sequences in raw echo if you cat -v.
printf '\e[?1000l'
```

Screenshots: `/tmp/rel-vX.Y.Z-25-mouse-N.png`.

**Expected outcome:**

- [ ] Tmux receives clicks; pane focus follows the click.
- [ ] Tmux drag-resizes splits using mouse.
- [ ] Vim cursor jumps to clicked cell; visual mode selects on drag.
- [ ] Mode enable/disable toggles raw mouse reporting in real time.
- [ ] Scroll wheel inside tmux/vim sends arrow keys (or scroll, per app); does NOT touch Sonic scrollback (alt-screen behavior).

**FAIL → block release.**

---

## 26. Wide chars / grapheme clusters

**Setup:** fresh launch.

**Commands:**
```bash
printf 'CJK: 中文测试|\n'
printf 'JP : こんにちは|\n'
printf 'KR : 안녕하세요|\n'
printf 'Emo: 🎉🇺🇸👨‍👩‍👧‍👦|\n'   # last is a ZWJ family
printf 'Flag: 🇯🇵🇰🇷|\n'                 # regional-indicator pairs
printf 'Comb: é (é) à (à)|\n'
printf 'Pwr :    |\n' # Powerline PUA
```

Then test cursor arithmetic: type each char into a `read -r x; echo "$x" | wc -m`.

Screenshots: `/tmp/rel-vX.Y.Z-26-wide-N.png`.

**Expected outcome:**

- [ ] CJK/JP/KR each occupy 2 cells; trailing `|` aligns to a consistent column across rows.
- [ ] Emoji ZWJ family renders as a SINGLE glyph (not 4 separate emoji).
- [ ] Flag pairs render as flag (not as letters R/I + R/I).
- [ ] Combining accents render on the base char (single cell), not as standalone marks.
- [ ] Powerline PUA glyphs render (font: `Rec Mono Casual` fallback per CLAUDE.md §1).
- [ ] Cursor advances by the correct cell count after each char.

**FAIL → block release** (Unicode capability matrix from §11 must also be green).

---

## 27. Cursor styles (DECSCUSR)

**Setup:** fresh launch.

**Commands:**
```bash
printf '\e[0 q'  # default
printf '\e[1 q'  # blinking block
printf '\e[2 q'  # steady block
printf '\e[3 q'  # blinking underline
printf '\e[4 q'  # steady underline
printf '\e[5 q'  # blinking bar
printf '\e[6 q'  # steady bar
```

Type characters between each to observe the cursor shape. Then run `nvim` and observe insert-mode cursor (vim emits `\e[6 q` by default in insert).

Screenshots: `/tmp/rel-vX.Y.Z-27-cursor-N.png`.

**Expected outcome:**

- [ ] Each `q` sequence changes cursor shape live.
- [ ] Blinking variants actually blink (visible toggle at the cursor cell).
- [ ] Steady variants do NOT blink.
- [ ] nvim insert mode shows the bar cursor.
- [ ] Cursor color reads from `theme.colors.cursor` (or per-cell inverse) per design.

**FAIL → block release.**

---

## 28. Session restore (deferred, N/A)

Session restore is explicitly out of scope until post-v1.0 (CLAUDE.md North Star).

**Expected outcome:**

- [x] N/A — feature deferred; no check required for this release. (Pre-checked so the gate does not block. Re-introduce as a real section once the feature lands.)

---

## 29. Crash hygiene

**Setup:** fresh launch.

**Actions:**
1. Inject a deliberate panic via a debug-only `SONIC_PANIC=1` env var if such a path exists; otherwise skip the synthetic injection and rely on observed-in-the-wild crashes.
2. Force-kill the app (`kill -9 $(pgrep sonic-mac)`).
3. Re-launch.
4. Check macOS Console.app for any crash report tagged `sonic-mac`.

**Expected outcome:**

- [ ] Re-launch after kill works without complaint (no half-written state file blocking startup).
- [ ] If a panic occurred, the backtrace is captured (RUST_BACKTRACE-enabled binary in dev; in release at least a one-line panic message in `/tmp/gui-smoke.log` or stderr).
- [ ] No corrupted user config after force-kill (validate by re-reading `sonic.toml` and confirming app starts).
- [ ] No "Sonic quit unexpectedly" dialog from macOS for a normal `Cmd+Q` quit.

**FAIL → block release.**

---

## 30. Accessibility

**Setup:** fresh launch.

**Actions:**
1. Increase OS text size via System Settings → Accessibility → Display → if applicable.
2. Toggle "Increase Contrast" in Accessibility settings.
3. Enable "Reduce Motion".
4. Enable VoiceOver briefly (`Cmd+F5`); navigate around the window with VO+arrows.
5. Use macOS Zoom (`Ctrl+scroll`) into the terminal.

Screenshots: `/tmp/rel-vX.Y.Z-30-a11y-N.png`.

**Expected outcome:**

- [ ] Sonic does not crash under any accessibility toggle.
- [ ] VoiceOver at minimum reads the window title; menu items announce correctly.
- [ ] Increase-contrast does not render the cursor or text invisible.
- [ ] Reduce-motion suppresses any animated overlay if implemented.
- [ ] OS Zoom magnifies cleanly (no garbled glyphs at high zoom).

**FAIL → block release** for crashes. Known-incomplete VoiceOver support may be documented as a v1.x item (note in PR body), not blocking.

---

## 31. First-run experience

**Setup:** `rm -rf ~/Library/Application\ Support/Sonic/` (BACK UP first if you care about your config). Then launch fresh.

**Expected outcome:**

- [ ] First launch creates the config dir without prompting.
- [ ] Default theme + keymap (wezterm) apply (CLAUDE.md §1).
- [ ] No error dialog about missing config.
- [ ] Default font resolves (`St Helens` system, falls back to `Rec Mono Casual` bundled) — no tofu in welcome shell prompt.
- [ ] `sonic.toml` exists after first quit (or is generated lazily on first edit — verify against current design and note which).

**FAIL → block release.**

---

## 32. Locale / non-UTF8

**Setup:** fresh launch.

**Actions:**
1. In a pane: `LANG=C printf '\xe4\xb8\xad\xe6\x96\x87\n'` (UTF-8 bytes for 中文 in a C locale shell).
2. `LANG=zh_CN.UTF-8 printf 'OK 中文\n'`.
3. Pipe a binary file briefly: `head -c 4096 /bin/ls | cat -v` (controls / non-UTF8 bytes).
4. Set `LANG=en_US.UTF-8` and confirm parity with Section 1.

Screenshots: `/tmp/rel-vX.Y.Z-32-locale-N.png`.

**Expected outcome:**

- [ ] UTF-8 byte input still renders correctly regardless of `$LANG` (Sonic treats stream as UTF-8 with replacement for invalid bytes).
- [ ] Binary garbage does not crash the VT parser (`cargo test -p sonic-core --test vt_fuzz` mirror).
- [ ] Invalid UTF-8 bytes show U+FFFD replacement, not silent corruption.
- [ ] Returning to UTF-8 locale produces identical rendering to baseline.

**FAIL → block release.**

---

## 33. Permissions / TCC prompts

**Setup:** clean TCC state if possible (`tccutil reset ScreenCapture com.sonic.terminal` etc., adjusted to actual bundle id).

**Actions:**
1. First launch — observe any TCC prompts (Input Monitoring, Screen Recording, Accessibility, Full Disk Access).
2. Decline a prompt; observe Sonic continues to function for unrelated features.
3. Re-launch after granting; confirm the feature that needed the permission now works.

**Expected outcome:**

- [ ] Sonic only prompts for permissions it actually requires (no unjustified Accessibility / FDA request).
- [ ] Declined permission does not crash the app; degraded feature surfaces a clear message.
- [ ] Granted permission is honored on next launch without re-prompt.

**FAIL → block release** if an unexpected/unjustified prompt appears.

---

## 34. Long-running (1 hour) stability

**Setup:** fresh launch; 2 tabs each with 2 panes.

**Actions:**
1. In one pane: `while true; do date; sleep 1; done`.
2. In another: `tail -F /var/log/system.log` (or any always-growing log).
3. In a third: idle prompt.
4. In the fourth: `htop`.
5. Walk away for 60 minutes.

**Measurement (every 10 min via `launchctl`/`cron` or scripted, or just spot-check at the end):**
```bash
ps -p $(pgrep sonic-mac | head -1) -o rss,vsz,%cpu
```

**Expected outcome:**

- [ ] RSS growth over the hour is < 50 MB (no leak in glyph atlas / scrollback / shape cache).
- [ ] CPU stays bounded (no runaway above 30% sustained without input).
- [ ] No crash, no beachball.
- [ ] After the hour, all four panes still respond to keystrokes within 100 ms.

**FAIL → block release.**

---

## 35. Drag-drop edge cases

**Setup:** fresh launch.

**Actions:**
1. Drag a folder (not file) from Finder onto the terminal.
2. Drag a file whose name contains spaces, an apostrophe (`O'Brien.txt`), unicode (`日本語.txt`), and a `$VAR` looking sequence.
3. Drag multiple files at once (Cmd-click 3 files, drag together).
4. Drag a file into a non-active pane; confirm the path goes to the focused pane only (or to the pane under the cursor, per design — pick one and verify).
5. Drag an image from a browser (cross-app, may be URL or file).

Screenshots: `/tmp/rel-vX.Y.Z-35-drop-N.png`.

**Expected outcome:**

- [ ] Folder path is pasted shell-quoted correctly.
- [ ] Spaces / quote / unicode / `$` chars in names are properly escaped — the shell accepts the line verbatim on Enter.
- [ ] Multiple files yield a single line with all paths space-separated and each individually quoted.
- [ ] Per-pane targeting is consistent with documented behavior; no path written to a wrong pane.
- [ ] Cross-app drop with URL pastes the URL or path (NOT raw HTML payload).

**FAIL → block release.**

---

## 36. Config validation

**Setup:** back up `sonic.toml` then mutate it.

**Cases (one per launch):**
1. Missing top-level table — file with just `# comment`.
2. Unknown key — `nonexistent_option = "x"`.
3. Wrong type — `font_size = "big"` instead of number.
4. Bad theme name — `theme = "doesnotexist"`.
5. Bad keymap binding — `bind = "CmdShiftCtrlAlt+QWERTY"`.
6. Circular include (if includes exist) — `include = ["./sonic.toml"]`.
7. Empty file.
8. UTF-8 BOM at start.

For each: launch Sonic, observe behavior, then restore the good config.

Screenshots: `/tmp/rel-vX.Y.Z-36-config-N.png` per case.

**Expected outcome:**

- [ ] No crash for any malformed config.
- [ ] Missing/unknown keys → default applied + warning logged.
- [ ] Wrong type → clear parse error in log; previous good config OR default in effect; app still launches.
- [ ] Bad theme name → falls back to default theme with a logged warning, not a panic.
- [ ] Bad keymap binding → that binding skipped, others active; warning logged.
- [ ] Circular include is broken with an error (not stack overflow).
- [ ] Empty file is equivalent to "all defaults".
- [ ] BOM is silently tolerated.

**FAIL → block release.**

---

## 37. CLAUDE.md §4 land-mine coverage

Each row in this table maps a CLAUDE.md §4 land-mine to (a) the manual UX
section in this checklist that exercises it end-to-end, and (b) the
automated regression test that pins the contract. The manual section is
listed for human reproducibility; the automated test is the durable
guard. Tick both columns once you have confirmed the manual section
passes AND the named test still exists and runs green.

Some land-mines are purely code-internal (no plausible UX-level repro);
those rely on the automated test alone — that's noted in the row.

| # | Land-mine (CLAUDE.md §4) | Manual section | Automated test |
|---|---|---|---|
| 1 | `try_lock` on parser in render path (no AB-BA deadlock; missed redraw must reschedule) | Sec 12 (idle no-freeze) + Sec 7 (nvim burst) | `crates/sonic-app/tests/pty_multi_round_hang.rs` |
| 2 | 16 ms PTY redraw coalescing (burst output does not freeze UI; vsync bypass via `pty_burst_gen`) | Sec 7 (nvim Ctrl+D burst) + Sec 13 (vtebench) | `crates/sonic-app/tests/vsync_pty_burst_bypass.rs` + `crates/sonic-app/tests/vsync_input_bypass.rs` |
| 3 | CSI `J` (ED) and `K` (EL) honor mode params (J0/J1/J2, K0/K1/K2) | Sec 1 (shell prompt redraw — code-internal beyond that) | `crates/sonic-core/tests/vt.rs::shell_prompt_redraw_preserves_above_cursor` |
| 4 | Repeated DEC `?1049h` is a no-op when already in alt screen (don't clobber `saved_cursor`) | Sec 7 (vim re-entry) | `crates/sonic-core/tests/vt.rs::dec_1049h_repeated_does_not_clobber_saved_cursor` |
| 5 | `wgpu::CurrentSurfaceTexture::Suboptimal(frame)` — drop SurfaceTexture BEFORE `surface.configure(...)` (wgpu 29 panic otherwise) | Sec 1 + window-resize-many (code-internal; manual repro is "resize the window 20× rapidly, no panic") | `crates/sonic-shared/tests/suboptimal_drop_ordering.rs` (landed in #206 — source-level guard that fails if `drop(frame)` is reordered after `surface.configure(...)`) |
| 6 | `set_rich_text` vs `set_text` — per-cell color/weight/style needs `Shaping::Advanced` (cosmic-text 0.18 API) | Sec 8 (per-cell colored bold/italic + per-cell bg) | `crates/sonic-shared/tests/per_cell_bg_renders.rs` + `crates/sonic-shared/tests/unified_font_attrs.rs` + `crates/sonic-shared/tests/user_regressions.rs` (build_tab_title_rich_text_spans) |
| 7 | `PtyHandle::Drop` kills child explicitly (no orphans) | Sec 15 (Cmd+Q leaves no shell PIDs) | `crates/sonic-io/tests/pty_drop_kills_child.rs::pty_drop_kills_child` (merged in #213) |
| 8 | `sonic_cfg::url_open::validate()` rejects shell-metachar / non-allowlisted scheme URIs | Sec 9 (OSC 8 hostile URL is silently rejected) | `crates/sonic-cfg/tests/*` (url_open allow/deny matrix) |

**Sign-off rule:** every row above must have BOTH the manual section
passing AND the named automated test green (`cargo test --workspace`
includes them in the standard floor). If a row is marked
`[ ] needs-test`, the release is blocked until the TODO is closed.

- [ ] All 8 rows of the §4 land-mine table verified (manual section + automated test).

---

## Sign-off

Tag: `v___________`
Run by: `___________`
Date: `___________`
Platform: `___________` (macOS 14.x / Windows 11 23H2)

All 37 sections passing → `bash scripts/check-release-testing.sh && git tag vX.Y.Z && git push origin vX.Y.Z`.
