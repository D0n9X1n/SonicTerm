# Configuration

[简体中文](Configuration-zh-CN)

### Files and lookup

SonicTerm uses one cross-platform config file:

```text
~/.sonicterm/sonicterm.toml
```

The first launch creates this file and seeds editable examples under
`~/.sonicterm/themes/` and `~/.sonicterm/keymaps/`.

Set `SONICTERM_CONFIG_DIR` to an absolute path to use that directory instead of
`~/.sonicterm` for the config file, themes and keymaps; an empty value is
ignored. It exists for diagnostic runs against a scratch copy of the config:
`HOME` stays unchanged, so the shell inside the terminal behaves as usual. Logs
have their own override, `SONICTERM_LOG_DIR` (see [Logging](Logging)).

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
| `quit_on_last_window_close` | `true` | Accepted for compatibility and ignored. SonicTerm exits when its last window closes, on every platform and whatever the value. |
| `tab_min_width` | `240` | Minimum width of one tab in logical pixels, so a short title such as `A` still gets a comfortable tab while the bar has room. It wins over a smaller `tab_max_width`, and the font/scale-derived readable minimum wins over it. When the tabs do not fit, they shrink below it, down to the readable minimum, before the bar shows an overflow selector, so it never moves the overflow threshold. Non-finite or non-positive values are ignored. |
| `tab_max_width` | `320` | Maximum width of one tab in logical pixels. Each tab sizes to its content, measured in the tab font: the `#N` index and process icon, the privilege marker, the title, and the command-status badge while it is shown. That width is clamped between `tab_min_width` and this maximum; a larger `tab_min_width` wins over a smaller maximum, and the font/scale-derived readable minimum wins over both. A title is cut, with `…` at a grapheme boundary, only when its tab reaches this maximum or the bar is full. When the tabs do not fit, the widest tabs shrink first to one common width, which may go below `tab_min_width` down to the readable minimum, so short titles stay whole; when even the readable minimum does not fit, the bar shows an active-tab segment and an overflow selector, and extremely narrow windows relax the minimum to retain both hit zones. While a tab is pressed or dragged, or the pointer rests on the bar, a changed title, badge or privilege marker keeps the current tab widths; they update once no tab is pressed or dragged and the pointer leaves the bar. A config written by an earlier version still sets `tab_max_width = 240` and keeps that cap, so every tab is then 240 wide while the bar has room; remove the line or raise it to use the new default. Non-finite or non-positive values are ignored. Width-policy changes invalidate retained tab chrome even when terminal content is idle. |

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
| `clickable_bare_names` | `true` | Allows contextual names to resolve against the exact pane’s trusted local OSC 7 working directory. Separator-relative paths require that same trusted pane CWD. Neither resolves on the alternate screen, where a full-screen program may show several panes. It only works when `clickable_local_targets` is also `true`. |
| `clickable_path_max_chars` | `1024` | Longest auto-detected path candidate considered, counted in Unicode characters of the displayed candidate; longer candidates are ignored. Values are clamped to `1`–`1024`, so the default is the maximum. The 16 KiB logical-text cap, eight spaced parts, and 32 rows still apply, as does the scanner’s 4 KiB per-target cap. OSC 8 and file-URI destinations are not limited by it. Reloading a changed value revokes current path results. |
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
| `strong_focus` | `false` | Doubles the thickness of the keyboard focus ring drawn around the focused status line segment. |

#### `[notifications]`

| Key | Default | Behavior |
| --- | --- | --- |
| `long_command` | `false` | Enables long-command desktop notifications on Windows. macOS and Linux currently do not send this notification. |
| `threshold_secs` | `10` | A reported command duration must be greater than this value. |
| `osc9` | `"app"` | Where program notifications (OSC 9 text and OSC 777 `notify`) go: `"app"` shows SonicTerm's in-app notification in the pane's window, `"desktop"` also raises a desktop notification on Windows while that window is not focused (macOS and Linux behave as `"app"`, as for `long_command`), and `"off"` ignores them. Any other value fails the load. |
| `taskbar_progress` | `true` | Mirrors `OSC 9;4` progress to the Windows taskbar, the macOS Dock badge and the Linux launcher entry; see [Platform Integration](Platform-Integration). A reload that sets `false` clears every surface at once. |

#### `[status_line]`

The status line is a strip at the top or bottom of the window that shows SonicTerm's own state: what it detected under the pointer, which input modes are on, how far the focused pane is scrolled back, problems with SonicTerm itself, and its own memory. It shows only what SonicTerm knows and the shell prompt, tmux or the program cannot report. It is off by default.

| Key | Default | Behavior |
| --- | --- | --- |
| `enabled` | `false` | Shows the strip. **Toggle Status Line** (`toggle_status_line`, also in the View menu) changes it for the session. |
| `preset` | `"developer"` | Lists and segment defaults to start from: `"minimal"`, `"developer"` or `"diagnostics"`. |
| `left`, `center`, `right` | the preset's lists | Each list, when present, replaces that list of the preset. An id outside the catalog, an id listed twice, or an id this build does not implement fails the load. |
| `icons` | `"unicode"` | Symbols only: `"unicode"`, `"ascii"` or `"nerd"`. `nerd` uses the whole Unicode set, since this build defines no Nerd Font symbols for its segments; there is no per-glyph fallback. Segments have no tooltips; each has a plain-words spoken name and value for assistive technology. |
| `hide_sensitive` | `false` | Hides every sensitive segment, such as the link under the pointer, for screen sharing. **Toggle Status Line Sensitive Content** (`toggle_status_line_sensitive`) changes it for the session. |
| `position` | `"top"` | The window edge the strip sits on: `"top"`, above the panes, or `"bottom"`, between the panes and the tab bar. Any other value fails the load. |
| `update_interval_ms` | `2000` | Sampling interval of sampled segments (memory), `500` to `60000`. |

A reload applies `enabled` or `hide_sensitive` only when that key's value in the file changed, so a session toggle survives unrelated reloads.

Presets in this build:

| Preset | `left` | `center` | `right` | Segment defaults |
| --- | --- | --- | --- | --- |
| `developer` | `mode`, `scroll`, `input`, `paste` | `link`, `sel` | `alerts`, `clip`, `health`, `glyphs`, `history`, `mem` | — |
| `minimal` | — | — | `alerts`, `clip`, `health`, `mem` | — |
| `diagnostics` | as `developer` | as `developer` | `alerts`, `clip`, `health`, `glyphs`, `history`, `cpu`, `frame`, `out`, `path_check`, `tab_mem`, `mem` | `mem.sparkline = true` |

Every segment except `mem` shows only while it has something to say, so an idle window shows only `Mem: 412 MB`.

| Segment | Shows | Default priority | Default click |
| --- | --- | --- | --- |
| `alerts` | `Alerts: #2 #3`, the numbers of this window's tabs that need you, in tab order: a bell rang there, or a command that ran at least `min_secs` finished there (OSC 133 shell integration), while the tab was not active. More than four read `#2 #3 #5 #6 +2`; marks in other windows add `+1 window`. A marked tab also shows `●` before its title in the tab bar. Visiting the tab clears its mark, and a bell or command end that happened while you were looking at the tab never marks it later. The spoken value names each source ("bell", "finished after 2m14s"); it never says a program is waiting for input. The `notify` source is accepted but marks nothing until SonicTerm parses program notifications; ConEmu OSC 9 subcommands such as `9;4` progress are never notifications. Kept when the window is narrow. | 990 | Goes to the next marked tab after the active one, wrapping, then to the next window with marks. |
| `clip` | For `hold_secs` after a program sent OSC 52 clipboard requests, what SonicTerm did with the last one and which tab sent it: `Clip: 2.10 KB by #2` (written), `Clip: ✗ failed by #2` (the native clipboard refused the write), `Clip: ⚠ read refused by #2` (reads are never answered), or, dim, `refused (selection p)`, `refused (too large)` (over 512 KiB decoded) or `refused (invalid)` (empty, malformed Base64 or not UTF-8). `×N` counts the requests the hold covers. The source reads `by another window` or `by a closed tab` when the tab is not in this window. It never shows clipboard text and does not claim the clipboard still holds what was written. Inside tmux the source is the tab running tmux. Refusals are logged at debug, at most one line per pane per second. | 760 | Goes to the source tab in its window. |
| `cpu` | `CPU: 14%`, SonicTerm's own CPU use (user plus system time of every thread; child processes excluded) over the last `update_interval_ms`. `core` makes 100% one busy core, as Activity Monitor's `% CPU` and `top`'s `%CPU` show it; `total` makes 100% every logical core, as Task Manager's `CPU` column shows it. It appears at `min_percent` and hides only after `hold_samples` consecutive samples below `min_percent × hide_ratio`. The first sample after enabling or waking shows nothing. The strip's own redraws are part of what it measures. Only in `diagnostics`. | 380 | Opens the Memory Usage report. |
| `frame` | `Frame: 4.1 ms · GPU`: the mean CPU-side time, from the start of collection to settlement, of the content frames this window presented in the last interval, and the presenter (`GPU`, `GPU (software)` for wgpu on a CPU adapter, or `GDI`, only on Windows). It is not GPU completion time. A frame counts only when it settled a change other than the strip or the cursor blink, so a blinking cursor in an idle shell reads as idle; a strip-only attempt that has to retry does not count, and a content attempt that retries counts once. The frame rate is in the spoken value. It never requests a frame to measure. With `always` an idle window reads `idle · GPU`. Only in `diagnostics`. | 360 | None. |
| `out` | `Out: 14.0 MB/s`, PTY output bytes received before parsing over the last interval, for the focused pane (`scope = "pane"`) or every pane of the window. Counting runs only while a shown strip lists `out`. A pane moved to another window keeps its count, so a move is never counted twice. Same hysteresis as `cpu`. Only in `diagnostics`. | 340 | None. |
| `path_check` | `Path check: 3s`: the oldest path probe or path open that has run past `show_after_ms`, timed from when the worker started it (queue wait is not counted), with `· 2 queued` while more checks wait behind it. While checks wait, it rechecks every `show_after_ms` until the queue drains. After every check ends and none waits it stays for `hold_ms`; a check that starts during the hold continues the display. Past `warn_after_secs` it turns amber with `⚠`, so a dead network volume shows a growing time. A check submitted to a stopped worker is not counted, and a stopped worker has no running check. Only in `diagnostics`. | 700 | None. |
| `tab_mem` | `Tab mem: 38 MB`: the terminal state SonicTerm keeps for the active tab's panes (grid, history, hyperlinks, inline media), as last charged by the 30 s retention pass. **It is not the program's memory.** It never measures a pane itself. When some pane's charge is older than two passes it reads `84 MB · 40s ago`; when a pane was never charged it hides, because no sum covers the tab. Only in `diagnostics`. | 330 | None. |
| `glyphs` | `Glyphs: ⚠ 3 missing`, dim: the number of distinct characters this window's presented terminal rows draw as boxes because no font SonicTerm can find covers them (the configured family, `font_dirs`, the built-in faces and the platform's system fallback all ran and none covered them). A character still waiting for fallback, one whose lookup failed, a glyph that rasterized nothing, a tile too large to place, whitespace, chrome text and rows not on screen are not counted; it counts characters, not cells. Past a cap (64 per row, 1,000 per window) it reads `≥ 1,000 missing`; when the unresolved set itself overflowed it reads `count capped`. Install a covering font or choose another `[font].family`; see [Rendering and Fonts](Rendering-and-Fonts). | 320 | Shows one notification listing up to 16 codepoints as `U+0378`, with the font hint; never writes the clipboard. |
| `health` | Only when something is wrong: `Config: ✗ line 12` (also `Theme:`, `Keymap:`) for the last failed load of each file, at startup or on a reload, kept until that file reloads cleanly; `Render: ⚠ software (GDI)`, `software adapter` or `software (forced)`; `GPU: ⚠ recovering`. | 950 | With a reload error, opens the config file; otherwise none. |
| `mem` | SonicTerm's own process memory without GPU memory where the OS separates it, `Mem: 412 MB`, in binary units with three significant figures, with `↑` or `↓` when it moved by at least 1% since the previous sample. Amber with `⚠` at `warn_mib`, red with `✗` at `critical_mib`. | 900 | Opens the Memory Usage report. |
| `mode` | Chips: broadcast input `⇶ 3` with its receiver count, `COPY` for copy mode, `MOUSE` when a program captured the mouse. | 800 | None. |
| `input` | Input SonicTerm refused or blocked for a pane in this window, for `hold_secs`: `Input: ✗ not sent (busy)` (the pane's input queue was full), `not sent (closed)` (its writer had stopped), `not sent (too large)` (over 16 MiB), `Input: ✗ write failed (3 not sent)` (the writer stopped on a native error with that many accepted writes undelivered), `Input: ⚠ paste refused` (`paste refused (2/5)` with broadcast: refused and attempted panes), dim `Input: ⊘ blocked (read-only)` (READONLY consumed a paste, drop or write; `blocked 2 panes` when it skipped broadcast receivers) or, on Windows, `Input: ✗ key not sent`. While the oldest accepted write to a pane has been outstanding for `held_after_ms`, `Input: ⚠ held 4s` shows its age, with `· 12.0 MB queued` from 1 MiB queued; it hides when the write completes. `held` states only that one accepted write has not completed: it never claims the program is hung or waiting, and inside tmux the writer is feeding tmux. `write failed` replaces `held`, and `held` outranks other records. Notifications are unchanged; no input bytes are kept, drawn, logged or spoken. Typing arms one check per pane per `held_after_ms`, which finds nothing outstanding and draws nothing. | 780 | None. |
| `link` | What detection found under the pointer, shown only while the open modifier is held (Cmd on macOS, Ctrl on Windows and Linux, the same key that underlines and opens a target), so ordinary hovering adds nothing to the strip: `Link: github.com/…`; for an OSC 8 link whose visible text names another host, or whose text or target carries userinfo, `Link: ⚠ evil.example ≠ bank.example` (a heuristic warning, not spoof protection); `Link: … · label not checked` when the text reaches the view's top or bottom row, passes 32 fragments or 2 KiB, so it was not read whole and is never reported as matching; for a local path, only a failed detection: `Path: ✗ ~/proj/src · not found`, or `Path: ⚠ … · blocked` with its reason. A path that resolves is already underlined and one still being checked shows nothing. | 600 | None. |
| `sel` | The size of what copying the window's selection (or copy mode's range) would put on the clipboard: `Sel: 3 lines`, or `Sel: 142 chars` for one line, counted with the copy's own rules (wrapped rows join, trailing padding and a detached right frame are dropped). A selection over 50,000 rows, or one whose walk would visit more than 200,000 cells, shows its exact row span instead: `Sel: 12,345 rows · count capped`. After `copy_to_clipboard` or copy mode's `y`/Enter it shows `Sel: ✓ copied` or, in red, `Sel: ✗ copy failed` for `copied_secs`, even when the copy cleared the selection. Copies a program makes with OSC 52 are not shown. The count is taken in the frame pass and reused until the selection's pane, endpoints, screen or scrollback eviction change; for copy mode's range, also until a row inside it changes. | 520 | Copies the window's selection; inert while a copy result is shown. |
| `paste` | `Paste: ⚠ raw` in amber when a paste into the focused pane would be unbracketed: bracketed paste is off, the pane is on its primary screen, and its shell reported a prompt with OSC 133 (see [Terminal IO and VT](Terminal-IO-and-VT)). A pane without OSC 133, or one running a command, never shows it. Its spoken value is "paste is unbracketed and may run lines": SonicTerm cannot know what the shell does with a newline. `cmd.exe` sends no OSC 133, so it never shows the warning even though its pastes always run line by line; PowerShell shows it only with a prompt that sends OSC 133. | 560 | None. |
| `ime` | Only an input-method problem: `IME: ⚠ discarded (copy mode)` for `hold_secs` after copy mode dropped a commit, or `IME: → search` / `IME: → palette` while a composition will commit to the search field or the command palette instead of the pane. An ordinary composition into the pane shows nothing. Where the platform reports no composition events (some Wayland setups), only the copy-mode warning can appear. Not in a preset. | 500 | None. |
| `scroll` | `Scroll: ↑ 340` while the focused pane is scrolled back. | 400 | Returns the pane to the bottom. |
| `history` | `History: trimmed` in amber after SonicTerm dropped rows from the focused pane's scrollback because its history passed the grid's memory budget; the spoken value gives the row count. Scrolling past the `[terminal] scrollback` limit, a clear, a resize or a lowered limit never shows it. It stays until a click on it or a program clears scrollback (`CSI 3 J`, as `clear` does in most shells), and shows again after a later trim. SonicTerm has no clear-scrollback action, so those are the two ways to acknowledge it. | 300 | Acknowledges the trim. |
| `font` | Opt-in. `Font: 16pt (+2)` or `Font: 13.5pt (−0.5)` while the live font size differs from the configured size; hidden when they match. The size is app-wide, so every window shows the same value. | 280 | Resets the font size (`reset_font_size`). |
| `zoom` | Opt-in. A `ZOOM` chip, with no name, while a split pane of the active tab is zoomed to fill it. | 260 | Toggles pane zoom (`toggle_pane_zoom`). |
| `size` | Opt-in. `Size: 120×40`, the focused pane's columns and rows, for `hold_ms` after its grid was resized (a window resize, a split, a font-size or display-scale change). A pane's first frame, a new tab, a restored session, a tear-out and a focus change to another pane show nothing. | 80 | None. |

The memory figure starts from the one the Memory Usage report shows and leaves out GPU memory where the operating system separates it. On macOS it is the physical footprint (Activity Monitor's Memory column) minus the graphics memory charged to that footprint (`graphics in footprint` in the report's `GPU (OS)` row), never below zero; graphics memory not charged to the footprint was never in it. On Windows it is the private committed bytes (Task Manager's **Commit size** column), unchanged: DXGI reports video memory by segment group, not as a share of private commit, so SonicTerm cannot tell how much of the commit is graphics and subtracts nothing. On Linux it is the resident set (`top`'s RES), unchanged, because no graphics figure is available. The trend arrow, the sparkline, `warn_mib` and `critical_mib` all follow this figure. When the graphics query gives no figure, the footprint is shown as is. Child processes are not counted. A failed query shows `n/a`.

Each `[status_line.segment.<id>]` table takes:

| Key | Values | Behavior |
| --- | --- | --- |
| `show` | `"always"` or `"active"` | `always` keeps the slot with idle text (`n/a`, `bottom`, `normal`, `none`); `active` hides the segment when it has nothing to say. `alerts`, `clip`, `glyphs`, `input`, `path_check`, `tab_mem`, `link`, `paste`, `ime`, `history`, `font`, `zoom` and `size` accept only `active`. |
| `priority` | `0` to `1000` | Higher survives longer when the window is narrow. |
| `click` | `"default"`, `"none"` or an action name | `default` runs the segment's own click; `none` makes it inert; a keymap action that takes no arguments, such as `"show_memory_usage"`, runs for the clicked window. `paste`, `ime` and `size` take only `default` or `none`, both inert. |

`[status_line.segment.mem]` also takes `show_trend` (`true`), `sparkline` (`false`, `true` in `diagnostics`: eight dim cells of the last two minutes after the figure, each the largest sample of a wall-clock 15 s bucket, scaled between the drawn cells' smallest and largest; only completed buckets are drawn, so it changes only when a bucket ends, and a span with no sample, such as a covered window or a sleeping machine, stays blank), `warn_mib` (`1024`) and `critical_mib` (`2048`, at least `warn_mib`), each `1` to `1048576`. `[status_line.segment.sel]` also takes `copied_secs` (`2`, `1` to `10`), how long a copy result stays shown. `[status_line.segment.paste]` takes `paste` (`"when_off"`, the default, or `"never"`). `[status_line.segment.ime]` takes `hold_secs` (`8`, `1` to `60`), how long the copy-mode warning stays shown. `[status_line.segment.size]` takes `hold_ms` (`1500`, `250` to `10000`). `[status_line.segment.clip]` takes `hold_secs` (`10`, `1` to `600`). `[status_line.segment.path_check]` takes `show_after_ms` (`300`, `100` to `5000`), `hold_ms` (`1000`, `0` to `10000`) and `warn_after_secs` (`10`, `1` to `600`). `[status_line.segment.cpu]` takes `scale` (`"auto"`: `"total"` on Windows, `"core"` elsewhere), `min_percent` (`5`, `0` to `1000`), `hide_ratio` (`0.7`, `0.1` to `1.0`) and `hold_samples` (`2`, `0` to `10`). `[status_line.segment.out]` takes `scope` (`"pane"`, the focused pane, or `"window"`), `min_bytes_per_sec` (`1048576`, `1024` to `1073741824`), `hide_ratio` and `hold_samples` as for `cpu`. `[status_line.segment.glyphs]` takes `min_count` (`1`, `1` to `1000`), below which it hides, and `ignore_private_use` (`false`; `true` stops counting Private Use Area codepoints such as Nerd Font icons). `[status_line.segment.input]` takes `hold_secs` (`8`, `1` to `600`), how long a refusal or failure stays shown, and `held_after_ms` (`1500`, `250` to `60000`), how long an accepted write must stay outstanding before `held` shows. `[status_line.segment.alerts]` takes `min_secs` (`10`, `0` to `86400`) and `sources` (`["bell", "notify", "finished"]`; listing fewer turns the others off). Unknown keys under `[status_line]` and inside segment tables are ignored and logged once per load as one `sonicterm-cfg` warning that names them all.

When the window is too narrow, the strip first uses compact forms (memory drops its arrow and sparkline, `mode` drops `MOUSE`), then shortens a link's path in the middle, then drops whole segments, lowest priority first. Health errors, the broadcast chip and a link's destination host go last.

The strip is 0.6 times the tab bar's height, at least 20 logical pixels, and follows font zoom and display scale. It shares the tab bar's background, with a thin line only on its terminal side. With `position = "top"` (the default) it sits above the panes, below the macOS titlebar band, and its line is on its lower edge; with `"bottom"` it sits between the panes and the tab bar, on the bottom edge when the tab bar is hidden, and its line is on its upper edge. Torn-out windows place it the same way. Turning it on or off, or a reload that changes `position`, resizes the panes once; a window opened with `[window]` `rows` keeps that many rows. A press or wheel over the strip never reaches a pane or the program. It appears in terminal windows only, not in the Memory Usage report.

#### Choosing segments while SonicTerm runs

**Status Line: Choose Segments** (`choose_status_line_segments`), or a right-click on the strip, opens a checklist in the palette of that window. A right-click highlights the segment under the pointer, clickable or not, or the first row between segments; a right-click outside the strip does nothing, as before, and a right-click on the strip never reaches a pane or its program. On macOS a Control-click is not a right-click; use the command or a key binding. The command is allowed in READONLY panes.

The checklist lists **Show status line**, **Hide sensitive segments**, every implemented segment under its group heading (Core, Pointer, Input, Display, Other tabs, Diagnostics, Program-reported; a group with no segment is not shown), and **Reset to configuration**. A shown segment has a check mark and names its list; a sensitive one is tagged `sensitive`, and `sensitive · hidden` while sensitive segments are hidden. Typing filters the segment rows by id and by localized name. Enter or a click toggles the highlighted row and keeps the page open; on a shown segment Left and Right move it to the neighbouring list (`right`, `center`, `left`), at the end of that list; Escape closes the page. Each change applies at once to every terminal window.

Enabling a segment adds it to the list the active preset puts it in, else to its home list, after the file's segments on that side; disabling removes it wherever it is. Enabling a segment the file lists, after you disabled it, returns it to its place in the file: only segments the file does not list are appended. Moving a segment with Left or Right puts it at the end of the target list, even when that is the list the file gives it. These are session choices on top of the file, kept per segment, never as a copy of the lists: segments you did not touch follow the file's order, and the ones you placed follow them in the order you placed them. A reload keeps a choice unless the file changed what that choice is about: a hidden segment's choice only when the file adds or removes it, a placed segment's choice when the file adds, removes or moves it (a `preset` change counts for the segments it changes); `enabled` and `hide_sensitive` choices are dropped only when their own key changed. Reordering the file keeps every choice, a choice the file now satisfies is dropped, and a failed reload keeps everything. **Reset to configuration** drops every choice.

The strip does not keep the event loop awake: memory is sampled only while a terminal window that can draw frames shows `mem`, and a window redraws only when its strip's pixels change. On the hardware path such a frame damages only the strip's band, because the strip's glyphs and missing-glyph boxes are clipped to it; software rendering (including every GDI frame) and frames with an open overlay repaint the whole window as before.

#### `[status_line.accessibility]`

| Key | Default | Behavior |
| --- | --- | --- |
| `expose` | `true` | Exposes the strip to screen readers. The accessibility adapter is installed in every terminal window regardless; with `false` the window's tree has no status line group, and turning it back on restores the group without a new window. |
| `announce` | `["health", "alerts"]` | Catalog ids allowed to announce through the announcement node. `health` announces when it enters a warning or an error and once when it clears; `alerts` announces a new bell, notification or finished-command mark. Other segments never announce, and `mem`, `cpu` and the other figures are never live. |
| `announce_min_interval_ms` | `5000` | At most one announcement per segment in this many milliseconds, `1000` to `60000`; identical text is never repeated. |

An unknown id in `announce` or an interval out of range fails the load. Unknown keys in the table are ignored and named in the `[status_line]` unknown-keys warning.

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
- scrollback, keypad mode, and local-target policy, including the path length cap;
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

### Saving current settings

**Save Current Settings** writes the live font values in
`~/.sonicterm/sonicterm.toml`:

```toml
[font]
size = 13
weight_scale = 1.0
```

Save writes the live font size and effective `weight_scale`, preserving comments,
order, line endings, and every other key. It also writes the status line the
session shows, compared with the file on disk under the save lock: `[status_line]`
`enabled`, `hide_sensitive`, `position`, and `left`, `center` and `right` as explicit lists,
each only when its value differs from what the file resolves to. `preset` and
per-segment keys are never written, so a save that changed only the font adds no
`[status_line]` key, and a key edited in the file since the last reload is kept
when the session agrees with it. The saved document must load and resolve to
exactly what the session shows, or nothing is written. A list that is already an
array is edited in place, and Save removes items but never comments: the items that
stay keep their comments, and every comment of a removed item is kept as its own
line before the closing `]`, in its original order, even when the list becomes
empty. The written `[status_line]`
becomes the baseline for session choices, so every choice it now holds is dropped
and a following reload changes nothing. It saves no theme or other runtime state
and does not reload values that are already active.

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
