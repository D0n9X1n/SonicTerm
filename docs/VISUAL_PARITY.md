# Visual Parity vs WezTerm

This document is the **side-by-side checklist** we walk down after running
`scripts/visual_diff.sh`. The goal is not pixel-identical output — that's
impossible across two renderers — but **no surprising visual gap** when a
user with muscle memory from WezTerm tries Sonic.

Capture method: `bash scripts/visual_diff.sh` (see that script's header
comment for prerequisites). Both terminals are positioned at the same
size and fed the same payload via clipboard paste.

**Expected non-bugs:** macOS titlebar style (traffic lights, title text)
is OS-controlled and will always match the system, not the peer terminal.
Strip the titlebar (`*-crop.png` outputs) before eyeballing.

**Reference theme for both:** Tokyo Night, JetBrainsMono Nerd Font 14pt,
1.0 line-height. Sonic ships this as default; configure WezTerm to match.

---

## Axes

### 1. Background color

| | WezTerm | Sonic |
|---|---|---|
| Hex | `#1a1b26` (Tokyo Night default ported) | `#1a1b26` (`assets/themes/tokyo-night.toml`) |
| Notes | Solid fill, no gradient | Solid fill via wgpu clear color |

**Verify:** sample a pixel in the middle of the content area in both
crops. Tolerance: identical hex.

### 2. Foreground color (default text)

| | WezTerm | Sonic |
|---|---|---|
| Hex | `#c0caf5` | `#c0caf5` |
| Notes | sRGB; no gamma correction in 20240203+ | Currently passes color straight to glyphon (sRGB) — gamma PR pending |

**Known gap:** until the gamma PR lands, thin strokes may look slightly
heavier in Sonic at low DPI. TBD after merge.

### 3. Cell padding (px from cell edge to glyph)

| | WezTerm | Sonic |
|---|---|---|
| Inter-cell horizontal | 0 px (cells abut) | 0 px |
| Window edge → first column | ~4 px (`window_padding.left = 4`) | 8 px (hardcoded in `GpuRenderer::layout`) |
| Window edge → top row | ~4 px (`top = 4`) | tab-bar height + 4 px |

**Known gap:** Sonic's left/right margin is 2× WezTerm's. Track as
follow-up; surface it in `Config` so users can match.

### 4. Line-height

| | WezTerm | Sonic |
|---|---|---|
| Multiplier | 1.0 (configurable via `line_height`) | 1.0 (fixed; metrics from cosmic-text font) |
| Effect | Tight, traditional terminal feel | Same |

### 5. Tab-bar style

| | WezTerm | Sonic |
|---|---|---|
| Bar background | `#16161e` (theme `tab_bar.background`) | `#16161e` (`theme.tab.bar_bg`) |
| Active tab bg | `#1a1b26` (matches content for "no border" look) | `#1a1b26` |
| Active tab indicator | None — active tab simply matches content bg | Same |
| Inactive tab fg | `#565f89` | `#565f89` |
| Close button | `×` glyph on hover | `×` glyph always shown (`tabbar_view.rs`) |
| Height | 28 px (font-size dependent) | 28 px |

**Known gap:** Sonic shows the `×` unconditionally; WezTerm reveals on
hover. Cosmetic, low priority.

### 6. Cursor

| | WezTerm | Sonic |
|---|---|---|
| Default shape | `SteadyBlock` (filled block, no blink) | Filled block (quad pipeline) |
| Color | `#c0caf5` with cursor_text `#1a1b26` (inverted glyph) | Same — `theme.colors.cursor` + `cursor_text` |
| Blink | Optional, off by default | Off (blink not yet implemented) |
| Unfocused | Hollow outline | Hollow outline (`render.rs` checks pane focus) |

### 7. Selection

| | WezTerm | Sonic |
|---|---|---|
| Highlight color | `#283457` | `#283457` (`selection_bg`) |
| Behavior | Click-drag, shift-click extends | Same (sonic-shared/src/selection.rs) |
| Copy on release | Configurable; on by default with WezTerm-compat keymap | Cmd+C only; auto-copy on release not yet wired |

### 8. Scrollbar

| | WezTerm | Sonic |
|---|---|---|
| Visibility | Optional (`enable_scroll_bar = false` default) | Not rendered |
| Style when shown | Thin strip on right edge, theme `scrollbar_thumb` | TBD |

**Known gap:** Sonic has no scrollbar UI; scrollback works via keyboard
(`super+shift+up/down`). Not a bug — matches WezTerm's default — but
worth a config knob in v1.0.

### 9. Underlines & decorations

| | WezTerm | Sonic |
|---|---|---|
| Underline | 1 px solid at descent | 1 px quad at descent (`quad.rs`) |
| Curly/dotted/dashed | Supported (SGR 4:3 etc.) | Solid only; SGR 4:N treated as 4:1 |
| Hyperlink tint | Subtle on hover | Subtle on hover (`hyperlink.rs`) |

### 10. HiDPI / CJK width

| | WezTerm | Sonic |
|---|---|---|
| Wide chars | 2 cells, glyph centered | 2 cells (`grid.rs` width tracking) — visual centering PR pending |
| Retina sharpness | Native @2x | Native @2x (wgpu surface configured with `scale_factor`) |

**Known gap:** CJK PR + HiDPI PR are in flight; this baseline capture
exists specifically so we can diff after they land.

---

## How to update this doc

When you ship a parity fix, move that row out of "Known gap" and update
the column. When you find a NEW divergence during eyeballing, add a row
with both columns filled and a one-line note. Keep the table compact —
one screen tall per axis section is the budget.
