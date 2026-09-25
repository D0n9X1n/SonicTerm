# Keybindings

[简体中文](Keybindings-zh-CN)

### Keymap files

SonicTerm keymaps are TOML files. Bundled files live in `assets/keymaps/`.
Editable copies live in:

```text
~/.sonicterm/keymaps/
```

The first launch seeds `sonicterm-macos.toml`, `sonicterm-windows.toml`, and
`sonicterm-linux.toml`. The active file comes from `keymap` in
`~/.sonicterm/sonicterm.toml`. A logical name checks the user directory before
bundled assets; dots are allowed, so `sonicterm-v1.2` remains a name. The portable
alias `keymap = "user"` selects the editable platform-default file on every OS.

Absolute paths and strings containing `/` or `\` are used directly. So are
Windows drive/UNC paths and names whose suffix is `.toml` (case-insensitive).
Relative explicit paths such as `custom.toml`, `./custom`, and `../custom` are
anchored to SonicTerm's process working directory.

**Edit keymap.toml** opens the platform-default user file. If `keymap` names a
different file, edit that file directly. Run **Reload Config** after saving.
SonicTerm has no keymap file watcher.

### Default shortcuts

The macOS application modifier is `Cmd`. Windows and Linux use `Alt`, which
leaves most `Ctrl+<letter>` input available to shells and terminal applications.
The listed `Alt` chords can therefore replace shell Meta bindings.

| Action | macOS | Windows | Linux |
| --- | --- | --- | --- |
| New tab | `Cmd+T`, `Cmd+Shift+T` | `Alt+T`, `Alt+Shift+T`, `Ctrl+T` | `Alt+T`, `Alt+Shift+T`, `Ctrl+T` |
| Close active pane or tab | `Cmd+W` | `Alt+W` | `Alt+W` |
| Next tab | `Cmd+Shift+]`, `Cmd+Right` | `Alt+Shift+]`, `Alt+Right` | `Alt+Shift+]`, `Alt+Right` |
| Previous tab | `Cmd+Shift+[`, `Cmd+Left` | `Alt+Shift+[`, `Alt+Left` | `Alt+Shift+[`, `Alt+Left` |
| Activate tabs 1–8 | `Cmd+1` … `Cmd+8` | `Alt+1` … `Alt+8` | `Alt+1` … `Alt+8` |
| Activate last tab | `Cmd+9` | `Alt+9` | `Alt+9` |
| Split right | `Cmd+D` | `Alt+D` | `Alt+D` |
| Split down | `Cmd+Shift+D` | `Alt+Shift+D` | `Alt+Shift+D` |
| Close pane | `Cmd+Shift+W` | `Alt+Shift+W` | `Alt+Shift+W` |
| Toggle pane zoom | `Cmd+Shift+Z` | `Alt+Shift+Z` | `Alt+Shift+Z` |
| Focus pane left/down/up/right | `Cmd+Shift+H/J/K/L` | `Alt+Shift+H/J/K/L` | `Alt+Shift+H/J/K/L` |
| Resize pane left/right/up/down | `Cmd+Shift+Arrow` | `Alt+Shift+Arrow` | `Alt+Shift+Arrow` |
| Broadcast to current tab | `Cmd+Shift+B` | `Alt+Shift+B` | `Alt+Shift+B` |
| Broadcast to all tabs | `Cmd+Ctrl+Shift+B` | `Ctrl+Alt+Shift+B` | `Ctrl+Alt+Shift+B` |
| Enter READONLY mode | `Cmd+[` | `Alt+[` | `Alt+[` |
| Copy selection | `Cmd+C` | `Alt+C`, `Ctrl+Shift+C` | `Alt+C`, `Ctrl+Shift+C` |
| Paste | `Cmd+V` | `Ctrl+Shift+V` | `Alt+V`, `Ctrl+Shift+V` |
| Increase font size | `Cmd+=`, `Cmd+Shift+=`, `Cmd++` | `Alt+=`, `Alt+Shift+=`, `Alt++` | `Alt+=`, `Alt+Shift+=`, `Alt++` |
| Decrease font size | `Cmd+-` | `Alt+-` | `Alt+-` |
| Reset font size to config | `Cmd+0` | `Alt+0` | `Alt+0` |
| New window | `Cmd+N` | `Alt+N` | `Alt+N` |
| Toggle fullscreen | `Cmd+Shift+F`, `Cmd+Enter` | `Alt+Shift+F`, `Alt+Enter`, `F11` | `Alt+Shift+F`, `Alt+Enter`, `F11` |
| Search | `Cmd+F` | `Alt+F` | `Alt+F` |
| Command palette | `Cmd+Shift+P` | `Alt+Shift+P` | `Alt+Shift+P` |
| Quick-select URLs | `Cmd+Shift+Space` | `Alt+Shift+Space` | `Alt+Shift+Space` |
| Scroll one line | `Cmd+Up`, `Cmd+Down` | `Alt+Up`, `Alt+Down` | `Alt+Up`, `Alt+Down` |
| Scroll one page | `Cmd+PageUp`, `Cmd+PageDown` | `Alt+PageUp`, `Alt+PageDown` | `Alt+PageUp`, `Alt+PageDown` |
| Scroll to top or bottom | `Cmd+Home`, `Cmd+End` | `Alt+Home`, `Alt+End` | `Alt+Home`, `Alt+End` |
| Reload config | `Cmd+R`, `Cmd+Shift+R` | `Alt+R`, `Alt+Shift+R` | `Alt+R`, `Alt+Shift+R` |
| Quit from keyboard | Press `Cmd+Q` twice within 5 seconds | — | — |

On macOS, the first `Cmd+Q` displays **Press ⌘Q one more time to quit**. Auto-repeat
does not confirm the quit. The prompt expires after 5 seconds. The native
**Quit SonicTerm** menu item quits immediately.

Windows deliberately does not bind `Alt+V`; that chord continues to the PTY.
Use `Ctrl+Shift+V` for paste. Linux binds both `Alt+V` and `Ctrl+Shift+V`.

Command-palette hints come from the first matching binding in the live keymap.
macOS displays modifier glyphs such as `⌘⇧P`; Windows uses names such as
`Win+Shift+P`, and Linux uses `Super+Shift+P`. Control and Alt use `Ctrl` and
`Alt` on Windows/Linux. Literal `+` keys remain visible, for example `Alt++`.
This changes hint text only, not key matching or existing search aliases.

Command labels, placeholders, empty-state text, rename/color-picker prompts,
and footer hints follow the active English, Chinese, or Japanese locale. Search
matches localized labels, English labels and aliases, and live shortcut hints.
Missing translations fall back to English. Locale or keymap reload preserves
the query, caret, and selected command when it still matches; rename text and
color-picker selection remain unchanged. Concrete bound actions retain their
literal arguments.

Command rows allocate 25 logical pixels plus 16 pixels for details, with an
8-pixel row gap and 12-pixel horizontal text insets. Label and subtitle form a
vertically centered block with a 4-pixel internal gap. At the 13/12-pixel font
sizes, this leaves equal 6-pixel top and bottom em-box margins. The footer is 42 pixels
high with symmetric 18-pixel insets; the preferred panel height is 440 pixels,
still limited by the viewport. Category/availability subtitles and the footer
use the existing native font one point smaller than the command label. Subtitles,
shortcut hints, and footer text are dimmed; command label size stays unchanged.

With an empty query, commands appear in a fixed category order, preserving their
relative order inside each category. A typed query retains fuzzy-score ranking.
Each command row shows its category and, when disabled, its reason. Unavailable
commands remain searchable; Enter on one keeps the palette open without
executing it. Context follows the palette's attached window, not another
window's tab count. Missing tabs, panes, selection, focus neighbors, and READONLY
restrictions are reported. Copy requires a nonempty selection whose cells still
match; a busy parser temporarily leaves Copy disabled rather than blocking the
UI. Same-value repaints preserve valid selections.

**About SonicTerm** is a palette-only entry with no default shortcut. Search
`about`, `SonicTerm`, `version`, or their localized equivalents, then select the
**About SonicTerm** result and press Enter. This closes the palette and shows
`SonicTerm <version>`, including any prerelease suffix, in
the originating window's existing green notification. It replaces any current
bubble and closes after five seconds or with its close button. The command is
available in READONLY mode and does not send input to the terminal.

**Go to Tab** searches live tabs in the attached window by title or displayed
position, including in READONLY. Selection follows tab identity across reorder
and rename, and is revalidated before activation. A closed or no-longer-matching
target leaves no selection; Enter does nothing until you move or edit the query.
A replacement at the same title/position never inherits selection.

When tabs no longer fit at the font/scale-derived readable width, the strip
shows a segment containing the active tab and a right-edge overflow control.
Click the control to open **All tabs** in the same window. Search a title or
position, use arrows and Enter, or click a row to switch. Existing next/previous
and numbered shortcuts still work; switching reveals the chosen tab. Very
narrow windows retain the active tab and the overflow control with smaller hit
zones. An empty selector explains that no tabs are available. A selector opened
by pointer in READONLY owns its keyboard input in both main and child windows;
Escape closes it without leaving READONLY.

Palette rows activate only when press and release identify the same entry and
its live context still permits execution. Reordering or closing a tab cannot
retarget a held click to a replacement. Clicking outside dismisses on release;
query-field clicks do not execute. Wheel input moves selection without wrapping
at either end and never reaches the terminal while the modal owns the pointer.
IME composition suppresses row activation. A terminal, tab, scrollbar, or divider
gesture that began before the palette opened keeps its paired release.

Tab drops use the visible tabs' absolute positions, not positions within the
segment. Dropping in the trailing visible gap inserts there; dropping a dragged
tab on the overflow control appends to the complete tab list. Ordinary clicks
on that control open the selector instead of starting a tab drag.

### Rename Window

`rename_window` opens **Rename Window** in the current window; it has no default
shortcut. Edit only the custom name, then Enter trims and saves, blank resets,
and Escape cancels. The title is `#N SonicTerm` or `#N Name`.
Unicode/IME and the configured paste shortcut are supported; controls, line breaks,
and names longer than 128 trimmed Unicode scalar values show rejection feedback.
The editor remains bound to its original window and sends nothing to PTYs or
broadcast targets, including in READONLY. Numbers are process-local, never reused,
and not persisted; window names do not follow tabs or terminal output. OS lists
may hide or truncate titles; macOS Cmd+Tab remains application-level. See
[Usage](Usage) for lifecycle and platform display details.

### TOML syntax

A keymap needs a `[meta]` table and zero or more `[[binding]]` tables:

```toml
[meta]
name = "my-keymap"
version = "1.0"

[[binding]]
keys = "super+shift+p"
action = "open_command_palette"

[[binding]]
keys = "super+1"
action = { activate_tab = 0 }
```

Key names are lower case. Modifiers are normalized in this order:

```text
super+ctrl+alt+shift+key
```

| Name | Meaning |
| --- | --- |
| `super` | Command on macOS; the Super/Windows key on Windows and Linux |
| `ctrl` | Control |
| `alt` | Option on macOS; Alt on Windows and Linux |
| `shift` | Shift |

Named keys use `enter`, `backspace`, `tab`, `escape`, `space`, `up`, `down`,
`left`, `right`, `home`, `end`, `pageup`, `pagedown`, `insert`, `delete`,
`menu`, `pause`, `printscreen`, `scrolllock`, `numlock`, `capslock`, and
`f1` through `f35`. Printable keys use their character. Shifted ASCII
punctuation also matches its unshifted spelling, so an event reported as `{`,
`}`, or `+` can satisfy `shift+[`, `shift+]`, or `shift+=` respectively. The
literal shifted spelling remains an alias.

Chord lookup is case-insensitive. If the same chord appears more than once, the
first matching binding wins. Keys with no binding go to the terminal. One
Windows exception is preserved: an `alt+v` binding to `paste_from_clipboard`
still passes through to the terminal.

### Actions

Actions without arguments use a string. The active action names are:

| Group | Actions |
| --- | --- |
| Tabs | `new_tab`, `close_tab`, `close_active_pane_or_tab`, `next_tab`, `prev_tab`, `activate_last_tab` |
| Panes | `split_right`, `split_down`, `close_pane`, `toggle_pane_zoom`, `resize_pane_left`, `resize_pane_right`, `resize_pane_up`, `resize_pane_down` |
| Clipboard and navigation | `copy_to_clipboard`, `paste_from_clipboard`, `enter_copy_mode`, `enter_quick_select` |
| Font | `increase_font_size`, `decrease_font_size`, `reset_font_size`, `increase_font_weight`, `decrease_font_weight`, `reset_font_weight`, `save_current_settings` |
| UI | `toggle_tab_bar`, `rename_tab`, `update_tab_color`, `open_search`, `open_command_palette` |
| Window and app | `new_window`, `rename_window`, `move_tab_to_new_window`, `toggle_fullscreen`, `quit_app` |
| Files and maintenance | `edit_config_file`, `open_keymap_file`, `reload_config`, `check_for_updates` |
| Shell navigation | `scroll_to_prev_prompt`, `scroll_to_next_prompt` |

Parameterized actions use inline TOML tables:

```toml
[[binding]]
keys = "super+3"
action = { activate_tab = 2 }

[[binding]]
keys = "super+shift+h"
action = { focus_pane = "left" }

[[binding]]
keys = "super+shift+right"
action = { resize_pane = { dir = "right", amount = 10 } }

[[binding]]
keys = "super+pageup"
action = { scroll = "page_up" }

[[binding]]
keys = "super+shift+b"
action = { toggle_broadcast = { scope = "tab" } }

[[binding]]
keys = "super+shift+1"
action = { apply_theme = "nord" }
```

`activate_tab` is zero-based. Directions are `left`, `right`, `up`, and `down`.
Each named resize action moves the divider by 5%. `resize_pane.amount` repeats
that 5% step; `0` does nothing. Scroll values are
`line_up`, `line_down`, `page_up`, `page_down`, `to_top`, and `to_bottom`.
Broadcast scopes are `tab` and `all_tabs`.

Font-size actions step by `1` point and clamp the live size to `8..=48`.
Font-weight actions step by `0.25` and clamp to `0.5..=5.0`. Reset returns to
the last loaded or saved config value. The weight and save actions have no
default shortcut, but they are available in the command palette.

### Selection and explicit copy

A normal drag selects cells. A double-click selects a word, and a triple-click
selects a line. Continuing to drag after a double- or triple-click extends by
whole words or lines. Releasing a drag does not copy automatically.

When a terminal application enables mouse tracking, an unmodified left-button
gesture belongs to that application. Start with `Shift+Left` to give the whole
gesture to SonicTerm and select local text instead. The owner is fixed on the
button press, so changing Shift or the application’s tracking mode during the
drag does not transfer it.

`copy_to_clipboard` copies a valid explicit selection. On the primary screen,
the selection remains highlighted after a successful copy. On the alternate
screen, a successful clipboard write clears the explicit selection and redraws
the window immediately. A clipboard failure keeps that valid selection so the
copy can be retried. If selected cells changed before the copy, SonicTerm clears
the stale selection and leaves the clipboard unchanged.

### READONLY and quick select

`enter_copy_mode` opens READONLY mode at the terminal cursor. It blocks new user
input to that window's terminals and does not create a selection. READONLY windows are excluded from broadcast receiving.
The wheel scrolls the local viewport instead of sending mouse reports or arrow
keys; the alternate screen has no local scrollback. New mouse presses and unheld
motion stay local even when the terminal application enables tracking. File drops are consumed
without sending paths to the terminal or broadcast peers.

Key presses already accepted before READONLY keep their original repeat and
release destinations; an earlier pointer gesture keeps its release owner.
Terminal replies and focus reports still reach the PTY. These local controls remain active:

| Key | READONLY action |
| --- | --- |
| `Left` / `h` | Move one cell left |
| `Down` / `j` | Move one row down |
| `Up` / `k` | Move one row up |
| `Right` / `l` | Move one cell right |
| `w` / `b` | Move to the next / previous word |
| `0` / `$` | Move to the start / end of the row |
| `g` / `G` | Move to the top / bottom |
| `Escape` | Exit READONLY mode |

READONLY also permits keymap actions that switch or activate tabs, focus panes,
open search or the command palette, rename windows, check for updates, or save current font settings.
Search text is still editable, including with the configured paste shortcut; in
writable and READONLY windows alike, search paste never reaches a PTY or broadcast
peer. Any other bound action not listed above is consumed without running or
reaching the PTY unless a local text field handles it.

`enter_quick_select` labels up to 26 URLs on the current screen with `a` through
`z`. Press a label to copy that URL and close the overlay. Press `Escape` to
cancel.

### App text fields

Search, command-palette filtering, and tab/window renaming support the same single-line
editing controls. These exact chords work only while an app text field owns
input:

| Key | Action |
| --- | --- |
| `Ctrl+A` / `Ctrl+E` | Move to start / end |
| `Ctrl+B` / `Ctrl+F` | Move left / right by one Unicode character |
| `Ctrl+H` / `Ctrl+D` | Delete backward / forward by one Unicode character |
| `Ctrl+W` | Delete left whitespace, then the previous non-whitespace run |
| `Ctrl+U` / `Ctrl+K` | Delete from start to caret / caret to end |
| `Left`, `Right`, `Home`, `End`, `Delete` | Standard caret movement and forward deletion |

Modified Backspace follows the platform's field-editing convention:

| Platform | Key | Action |
| --- | --- | --- |
| macOS | `Option/Alt+Backspace` | Delete to AppKit's previous word boundary |
| macOS | `Cmd+Backspace` | Delete from line start to the caret; keep the text after it |
| macOS | `Ctrl+Backspace` | Delete one canonical component of the preceding grapheme |
| Windows/Linux | `Ctrl+Backspace` | Delete left whitespace, then the previous non-whitespace run |

Option deletion uses AppKit's string-only word-boundary API, rather than guessing
macOS punctuation and language rules from `Ctrl+W`'s whitespace-only boundary.
With the caret at the end, `foo/bar!!!` becomes `foo/`, and `保留你好` becomes `保留`.
The native UTF-16 boundary is converted exactly to a UTF-8 caret; a boundary inside
a surrogate pair or beyond the caret is refused without deleting text.
Decomposing deletion makes both `é` and `e` followed by a combining acute accent
become `e`; `ấ` becomes `a` plus combining circumflex (`U+0061 U+0302`). It retains
the canonical decomposition without recomposing it and leaves other text unchanged.
As in AppKit, deleting the final pictograph from a joined emoji leaves the preceding
pictograph and trailing `U+200D` joiner; the next decomposing deletion removes that joiner.

Plain Backspace also accepts Shift alone, so deleting just after typing a capital
still removes one character. The command-modifier combinations are exact. Extra Shift, Alt, Control, or Super modifiers
do not inherit another deletion shortcut, and the Windows/Super key does not
inherit macOS Command editing behavior. Active IME composition retains ownership
instead of applying these edits to committed field text.

When no SonicTerm text field owns input, these keys retain their terminal encoding;
SonicTerm does not guess whether the destination is a shell, Vim, or tmux. In default
legacy modes, Option/Alt+Backspace emits Meta-DEL (`ESC` then `0x7f`), Ctrl+Backspace
emits `0x08` (DECBKM reverses the Backspace/Control-Backspace pair), and an unbound
Cmd/Super+Backspace remains a distinct encoded chord. Negotiated Kitty, MOK and Win32
input continue to preserve their protocol semantics. The terminal application decides
what deletion, if any, those bytes perform; they are not remapped to `Ctrl+W` or
`Ctrl+U`. `Ctrl+<letter>` likewise continues to the PTY.

Printable input comes from the operating system's `KeyEvent.text`, so Unicode
keyboard layouts and composed Option/AltGr characters are inserted as produced.
Super and ordinary Control, Alt, or Ctrl+Alt command chords never become field
text. An Option/Alt or AltGr event is accepted as composed input only when its
produced character differs from the layout-resolved unmodified key; the exact
Control editing chords above still take precedence.

Named non-text keys, including arrows and function keys, retain their terminal
protocol encoding even when macOS attaches private-use characters to the native
event. Those characters are neither inserted into app text fields nor reported
as Kitty associated text. Space, composed text, and genuine private-use character
input remain text.

### Numeric keypad

With the default `[terminal].keypad_mode = "auto"`, physical keypad digits that
the operating system resolves as numeric character keys retain ordinary digit
text, even after `ESC =` enables application-keypad mode. Operators, Enter, and
non-text keypad navigation retain their negotiated application-keypad mappings.
OS logical digit text is not a direct measurement of hardware NumLock.

If a shell enables application-keypad mode but does not accept its operator or
Enter sequences, select ordinary numeric input explicitly and reload the config:

```toml
[terminal]
keypad_mode = "numeric"
```

This overrides DECKPAM only for legacy input: operators use normal text rules,
keypad Enter uses Return's modifier/newline rules, and navigation follows the OS
logical key. Digits remain unchanged. Applications that need distinct legacy
keypad mappings should keep `auto`. The preference applies across main/child
windows and broadcast destinations without changing the terminal's stored modes.
Negotiated Kitty encoding is unchanged.

### Load failures

At startup, invalid TOML or a missing `[meta]` table falls back to the bundled
platform keymap. During reload, the current in-memory keymap remains active
instead. A structurally valid keymap handles bad actions per binding: SonicTerm
logs a warning, skips that binding, and keeps the rest. A successful reload also
updates command-palette shortcut hints.
