# Configuration

[简体中文](Configuration-zh-CN)

### Files and lookup

SonicTerm uses one cross-platform config file:

```text
~/.sonicterm/sonicterm.toml
```

The first launch creates this file and seeds editable examples under
`~/.sonicterm/themes/` and `~/.sonicterm/keymaps/`.

`theme` and `keymap` accept either a name or a TOML path. A named value first
checks the matching user directory, then the bundled `assets/` directory. A
path-like value is used directly.

Unknown TOML keys are inert unless implemented. Runtime saves preserve them,
comments, and formatting. The Rust `Config` serializer is different: it retains
top-level unknown keys but loses comments, formatting, and nested unknown keys.

### Supported keys and defaults

#### Top level

| Key | Default | Behavior |
| --- | --- | --- |
| `theme` | `"wezterm"` | Selects a theme. See [Themes](Themes). |
| `keymap` | `"sonicterm-macos"`, `"sonicterm-windows"`, or `"sonicterm-linux"` | Selects the platform keymap. See [Keybindings](Keybindings). |
| `locale` | `""` | Selects `en`, `zh-CN`, or `ja`. Empty uses `SONIC_LOCALE`, then the OS locale, then `en`. |
| `quit_on_last_window_close` | `true` | On macOS, `false` keeps the process available from the Dock after the last window closes. Other platforms always exit with no windows. |
| `tab_max_width` | `240` | Preferred maximum width of one tab in logical pixels. Non-finite or non-positive values are ignored. The font/scale-derived readable minimum takes precedence over a smaller maximum; crowded strips show an active-tab segment and an overflow selector. Extremely narrow windows relax the minimum to retain both hit zones. Width-policy changes invalidate retained tab chrome even when terminal content is idle. |

#### `[font]`

| Key | Default | Behavior |
| --- | --- | --- |
| `family` | `"Rec Mono St.Helens"` | Primary font family. Missing glyphs use the fallback chain. |
| `size` | `13` | Font size in points. |
| `line_height` | `1.3` | Line-height multiplier. |
| `weight_scale` | `1.0` | Post-selection weight for all monochrome glyphs, including bold, italic, and fallback faces. Valid values are `0.5..=5.0`; other values become `1.0`. At a fixed size/DPI, cell metrics, bitmap dimensions, bearings, and advances do not change. Color artwork is unchanged. |
| `subpixel_aa` | `"off"` | LCD coverage order: `off`, `rgb`, or `bgr`. See the eligibility rules below. |

Font changes apply to terminal text and regular application text. Changes to
`family`, `size`, or `line_height` resize every visible pane and its PTY.
`weight_scale` keeps the existing metrics. `subpixel_aa` is effective only on
Windows when the configured backdrop is `opaque`, effective opacity is `1`, and
either the Windows software presenter is active or the GPU exposes dual-source
blending. Every other combination—including Mica, Acrylic, Tabbed, opacity below
`1`, unsupported GPUs, and non-Windows hosts—uses deterministic grayscale.
`off` preserves the alpha-max grayscale output; `rgb` maps logical red, green,
and blue coverage to matching display channels; `bgr` swaps red and blue.
For shaping and fallback details, see [Rendering and Fonts](Rendering-and-Fonts).

#### `[window]`

| Key | Default | Behavior |
| --- | --- | --- |
| `cols` | `100` | Initial columns for a new window. |
| `rows` | `30` | Initial rows for a new window. |
| `padding_left` | `12` | Left content padding in logical pixels. |
| `padding_right` | `12` | Right content padding in logical pixels. |
| `padding_top` | `8` | Top content padding in logical pixels. |
| `padding_bottom` | `4` | Bottom content padding in logical pixels. |
| `decorations` | `true` | Enables native title-bar decorations for new windows. |
| `warm_window_pool` | `1` | Number of hidden child windows kept for fast tab tear-out. `0` disables the pool. Hardware rendering caps it at `5`; software rendering caps any nonzero value at `1`. |

`cols` and `rows` set startup size and the fallback when no usable source window exists.
New and torn-out windows inherit the initiating window's logical client size,
including warm-pool adoption. Size is captured before deferred creation; later
focus changes do not replace it. Maximized/fullscreen state is not inherited.
A missing, minimized, or zero-size source uses the configured fallback. Destination
DPI, native minimums, and accepted resize dimensions still apply; moving a tab into
an existing window does not resize that window. Every native terminal window has a
hard, non-configurable minimum inner size equal to 30 columns by 10 rows. The
pixel floor is recomputed from the live font, DPI, padding, titlebar, and tab-bar
geometry, including after live font/padding reloads and tab-bar visibility
changes.

Grid dimensions are never allowed to allocate without bounds. Each axis is at
most `4096`, the visible grid is at most `524288` cells, and the complete grid
including history is at most `1048576` cells.

#### `[terminal]`

| Key | Default | Behavior |
| --- | --- | --- |
| `shell` | omitted | Shell for new panes. Windows tries `pwsh.exe` from `PATH`, registered PowerShell 7, the real Microsoft Store package, Windows PowerShell, then `cmd.exe`. Unix tries an executable `$SHELL`, the current user’s executable passwd shell, then `/bin/sh`. An explicit non-empty value wins. |
| `term_program` | `"SonicTerm"` | `TERM_PROGRAM` for new child PTYs. `TERM_PROGRAM_VERSION` is SonicTerm’s version, except `term_program = "WezTerm"` advertises `20230712-072601`. |
| `scrollback` | `1000` | Requested history rows per pane. `0` disables history. Grid and retained-byte budgets may lower the effective limit. |
| `keypad_mode` | `"auto"` | `auto` preserves negotiated legacy keypad mappings and OS-resolved digit text. Opt-in `numeric` makes keypad operators and Enter use normal text/Return rules regardless of DECKPAM, and navigation follows its logical key. Kitty input is unchanged. See [Keybindings](Keybindings). |
| `clickable_local_targets` | `true` | Allows validated local directories to open and files to be selected in their containing folder on every platform. Includes local file URIs and native-path OSC 8 links; web/mail links are independent. |
| `clickable_bare_names` | `true` | Allows contextual names to resolve against the exact pane’s trusted local OSC 7 working directory. Separator-relative paths require that same trusted pane CWD. It only works when `clickable_local_targets` is also `true`. |
| `cursor_blink` | `false` | Enables cursor blinking. |
| `cursor_shape` | `"block"` | Accepts `block`, `bar`, or `underline`. |

The scrollback row setting and the memory budget both apply. Rich rows can hit
the byte budget before the row count. See [Memory](Memory).

#### `[appearance]`

| Key | Default | Behavior |
| --- | --- | --- |
| `backdrop` | `"opaque"` | Accepts `opaque`, `mica`, `acrylic`, or `tabbed`. Windows applies the named DWM material on a best-effort basis. Linux normalizes every startup and explicit reload to `opaque`, warning once when another value is requested. macOS treats non-opaque values as alpha-capable windows; the Windows material names do not select a macOS material. |
| `opacity` | `1.0` | Terminal background opacity, clamped to `0.0..=1.0`. |
| `scrollbar` | `"auto"` | Accepts `auto`, `always`, or `never`. `always` is still hidden when there is no history to scroll. |
| `panel_padding` | `2.0` | Inner padding for floating panels in logical pixels. Negative values act as `0`. |
| `software_render_mode` | `"auto"` | `auto` degrades when the adapter is software-rendered, `force` always degrades, and `off` never degrades. |

Software degradation lowers frame and animation cost. On Windows,
`software_render_mode = "force"` also makes new windows opaque because the
software presenter cannot composite transparency. If a non-opaque backdrop was
configured, SonicTerm logs a warning with the configured and applied values.
`auto` does not override the configured backdrop.

The scrollbar thumb can be dragged. Clicking its track moves one viewport.
`auto` shows it during scrolling, dragging, or pointer proximity to the pane’s
right edge.

#### `[accessibility]`

| Key | Default | Behavior |
| --- | --- | --- |
| `high_contrast` | `false` | Replaces the active theme foreground and background with `#ffffff` and `#000000`. |
| `reduced_motion` | `false` | Parsed and retained, but currently has no presentation effect. |
| `strong_focus` | `false` | Parsed and retained, but currently has no presentation effect. |

#### `[notifications]`

| Key | Default | Behavior |
| --- | --- | --- |
| `long_command` | `false` | Enables long-command desktop notifications on Windows. macOS and Linux currently do not send this notification. |
| `threshold_secs` | `10` | A reported command duration must be greater than this value. |

#### `[logging]`

| Key | Default |
| --- | --- |
| `level` | `"warn"` |
| `max_file_size_mb` | `10` |
| `max_rotated_files` | `3` |
| `max_age_days` | `2` |
| `max_crash_dumps` | `10` |
| `max_crash_age_days` | `2` |
| `max_crash_bytes` | `10485760` |
| `max_breadcrumb_files` | `10` |
| `max_breadcrumb_age_days` | `2` |
| `max_breadcrumb_bytes` | `1048576` |

`level` accepts `error`, `warn`, `info`, or `debug`. Logging is initialized at
startup, so logging changes require a restart. For file locations, cleanup
rules, and diagnostics, see [Logging](Logging).

### Editing and reloading

Use **Edit sonicterm.toml** in the command palette to open the standard config
file. SonicTerm reads it at startup and when you run **Reload Config**. There is
no file watcher.

A reload always re-reads the selected theme and keymap files, even when their
names did not change. The following settings apply to existing windows:

- theme, keymap, and locale;
- font family, size, line height, weight, and LCD subpixel mode;
- content padding, opacity, scrollbar, and panel padding;
- cursor shape and blink;
- scrollback, keypad mode, and local-target policy;
- tab width, warm-window target, software degradation, accessibility, and
  notification settings.

Some settings affect only objects created after the reload:

- `cols`, `rows`, `decorations`, and the native `backdrop` affect new windows;
- `shell` and `term_program` affect new panes;
- logging settings require a restart.

Platform capability normalization runs before a startup or reload config becomes
the session baseline. On Linux, an unsupported backdrop is therefore never stored:
existing state and every later warm, new, or torn-out window read `opaque` from the
normalized config. The warning appears once on the pass that changes the value;
normalizing the already-opaque result is silent.

Changing `backdrop` or `software_render_mode` can involve native window setup.
Restart SonicTerm when you need the complete native-window change, not only the
live renderer policy. A live `subpixel_aa` change invalidates and redraws the
presented frame without rebuilding fonts, raster tiles, or either atlas.

### Saving current font settings

**Save Current Settings** changes only these two values in
`~/.sonicterm/sonicterm.toml`:

```toml
[font]
size = 13
weight_scale = 1.0
```

Save writes the live font size and effective `weight_scale`, preserving comments,
order, line endings, and every other key. It saves no theme or other runtime
state and does not reload values that are already active.

If the file is missing, SonicTerm creates the starter file first. A
process-local lock and the persistent `sonicterm.toml.save.lock` sidecar prevent
two SonicTerm saves from running together. SonicTerm also compares the exact
file bytes again before replacement. A concurrent editor change, malformed
TOML, invalid font value, or lock conflict refuses the write. The existing file
and reset baselines remain unchanged, and an Error notification appears.

A successful save writes a temporary file in the config directory and replaces
the config atomically. An Info notification confirms the save. This guarantees
that readers see a complete old or new file; it does not guarantee survival of
a sudden power loss.

### Errors and recovery

At startup, an unreadable or malformed config logs a warning and uses defaults
so SonicTerm can still open. An invalid selected theme falls back to bundled
`wezterm`. An invalid selected keymap falls back to the bundled platform
keymap.

During **Reload Config**, an unreadable or malformed existing `sonicterm.toml`
leaves the entire current config active. A missing file loads defaults instead.
If the config itself parses but its theme or keymap fails, SonicTerm keeps the
current theme or keymap, logs the error, and
applies the other valid settings. A structurally valid keymap skips only
bindings whose action cannot be parsed; the other bindings remain active.
