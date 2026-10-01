# Usage

[简体中文](Usage-zh-CN)

### Install and first launch

Download the package for your platform from
[GitHub Releases](https://github.com/D0n9X1n/SonicTerm/releases):

- macOS Apple Silicon: `SonicTerm-<tag>-mac-aarch64.dmg`
- macOS Intel: `SonicTerm-<tag>-mac-x86_64.dmg`
- Windows x64: `SonicTerm-<tag>-windows-x86_64.msi`
- Linux x86_64: `SonicTerm-<tag>-linux-x86_64.deb` or
  `SonicTerm-<tag>-linux-x86_64.tar.gz`

On macOS, open the DMG and move `SonicTerm.app` to Applications. Release builds
are ad-hoc signed but do not have an Apple Developer ID signature or
notarization. If macOS blocks the first launch, use Finder’s **Open** context-menu
action. Apple Silicon packages require macOS 14.0+, and Intel packages require
macOS 15.0+. Cairo and its native libraries are included; Homebrew is not required.

On Windows, run the MSI. It installs SonicTerm per machine under Program Files
and adds a Start-menu shortcut. It also registers SonicTerm as an available
handler for supported script files without changing the current default app.

Install the Debian package with:

```sh
sudo apt install ./SonicTerm-<tag>-linux-x86_64.deb
```

Linux packages require x86_64 and glibc 2.35 or newer. The
`.deb` installs the required linked libraries and desktop metadata. For the
portable archive, extract it and run `sonicterm` from the extracted payload so
its adjacent `assets/` directory remains available. The host must supply the
runtime libraries; X11 needs `libxkbcommon-x11.so.0`. Both X11 and Wayland are
supported.

The first normal launch creates `~/.sonicterm/`, writes `sonicterm.toml`, and
seeds editable theme and keymap examples. For local package building and release
asset details, see [Packaging](Packaging).

### Common workflows

The command palette is the quickest way to discover actions:

- macOS: `Cmd+Shift+P`
- Windows and Linux: `Alt+Shift+P`

Search **About SonicTerm** or **version**, select the **About SonicTerm** result,
and press Enter to close the palette and show `SonicTerm <version>` in the green notification in that
window. The version comes from the running build. The notification replaces any
current bubble and closes after five seconds or with its close button. The command
works in READONLY mode and does not query releases online or send input to the shell.

Common defaults are:

| Action | macOS | Windows and Linux |
| --- | --- | --- |
| New tab | `Cmd+T` | `Alt+T` or `Ctrl+T` |
| Close active pane or tab | `Cmd+W` | `Alt+W` |
| Split right / down | `Cmd+D` / `Cmd+Shift+D` | `Alt+D` / `Alt+Shift+D` |
| Focus panes | `Cmd+Shift+H/J/K/L` | `Alt+Shift+H/J/K/L` |
| Search | `Cmd+F` | `Alt+F` |
| READONLY mode | `Cmd+[` | `Alt+[` |
| Quick-select URLs | `Cmd+Shift+Space` | `Alt+Shift+Space` |
| Broadcast to current tab | `Cmd+Shift+B` | `Alt+Shift+B` |
| Reload config | `Cmd+R` | `Alt+R` |

Each pane owns a separate child PTY. A tab can contain a split-pane tree. You can
reorder tabs, drag them between SonicTerm windows, or drag a tab away to create
a window. The live pane and PTY move with the tab; the shell does not restart.
Closing a split closes its PTY. Closing the final pane closes the tab.
Splitting a zoomed pane exits zoom, restores the split layout, and focuses the
new pane in both main and torn-out windows. A refused split leaves zoom and
focus unchanged; refusal in a live child window never redirects the split to
the main window.

Broadcast mode mirrors source-pane input to the other panes in the current tab
or in all tabs. Every participant—the fixed source and each eligible receiver—has
a thin red border on all four sides in main and torn-out windows. The top edge is
2 physical pixels, like the other edges; no banner or warning text covers the
terminal. A lone source is still marked while broadcast is armed. Disabling
broadcast or closing its source clears the highlights. The source is excluded
from mirrored delivery, so it does not receive input twice. READONLY windows are
excluded from broadcast receiving. Use broadcast carefully: input reaches every
receiver, with key encoding following each pane's negotiated terminal modes.

For the complete default map, action names, and customization syntax, see
[Keybindings](Keybindings).

### Read scrollback

Scrolling back with the mouse wheel, the scrollbar, the scroll actions, prompt
navigation, search, or copy mode pins the row at the top of the view. While a
command keeps printing, the same text stays at the top even after history is full
and each new line evicts the oldest row. Lowering `terminal.scrollback` on reload
keeps the pinned text too. If the pinned row itself is dropped, the view moves to
the oldest row still retained; when no history remains, as after `CSI 3 J` erases
it (many `clear` commands send it), the view follows live output again.

A full-screen program on the alternate screen shows its own screen. When it exits,
the view returns to the pinned row, shifted by any history dropped in the meantime.
The pinned row travels with its pane into other tabs and windows. Scrolling to the
bottom, or submitting input with Enter, follows live output again.

### Search retained output

Search scans the active pane's retained scrollback and current screen, not just
its visible rows. After editing the query, the first match in the current viewport
is selected; otherwise the next match below it, or the last preceding match, is
selected. The counter uses the full match list: four earlier matches, the current
one, and two later matches show `5/7`.

Search reads each cell's full stored text, including combining marks and other
zero-width characters. Substring mode converts the query and each cell's text
to NFC and then lowercases both unless the search is case-sensitive, so a
precomposed `é` and `e` followed by U+0301 find each other. Normalization is
per cell, and matching runs on the normalized text: a cell that stores `e`
followed by U+0301 holds a single `é` after NFC, so neither `e` nor U+0301 alone
finds it. Regex mode matches the raw characters without normalizing the pattern
or the text; a match that includes a combining mark highlights the cell that
holds it, and matches that start in the same cell count once.

Typing, IME commits, and pasting update the selection without scrolling. Enter or
Down moves forward; Shift+Enter or Up moves backward, wrapping at either end.
If the selected match is offscreen, the first navigation press reveals it without
skipping it. Visible results do not recenter the viewport. Search paste stays in
the query and never reaches the shell or broadcast peers; control characters are
removed. Select query text with Shift+arrows or mouse dragging, then use the configured
Copy/Paste actions; Windows Ctrl+A selects the whole query. Moving the text selection
does not change the current search result. These field controls also apply to the
command palette and tab/window rename editors; see [Keybindings](Keybindings).
History already evicted by retention limits and other panes are not searched.

Long query fields keep their displayed horizontal position while the caret remains
fully visible, including during reverse selection. A partly clipped edge character
snaps pointer selection to a nearby query boundary where the caret fits; dragging
outside either edge does not automatically scroll. Text or layout changes reset this
position, and a changed search match counter resets it too. The prompt and counter
are never part of the selectable query.

### Window names and numbers

Terminal windows receive process-local numbers starting at 1: `#1 SonicTerm`.
Use **Rename Window** in the command palette to set a custom name, for example
`#2 Work`. Edit only the name: Enter trims surrounding whitespace
and saves, blank input resets the numbered default, and Escape cancels. Names
support Unicode and IME, with at most 128 Unicode scalar values after trimming;
control characters, line breaks, and overlong input are rejected with feedback.
The configured paste shortcut replaces selected name text, or inserts at the caret,
never into the shell. Copy uses only this field's selection; invalid replacement
text leaves the existing name text and selection unchanged.
On macOS this also covers **Edit > Paste** and Cmd+V when the receiving window
owns the rename editor. The name field takes priority over search underneath it
and READONLY. Rejected, empty, or unavailable clipboard text never falls through
to search, a shell, or broadcast peers; paste is ignored during IME composition.
Pasting in another window follows that window's own input routing and does not
edit the name field left open in the first window.

Numbers are never reused or reassigned within a process. New windows and torn-out
tabs get fresh numbers and blank names; moving tabs into an existing window or
hiding/restoring a retained window preserves its identity. Warm helper windows
remain unnumbered until adopted. Names and numbers are not saved across restarts;
separate processes may each start at 1. Tab names, shell commands, OSC titles,
working directories, focus changes, and config reload do not rename windows.
The editor targets the window where it opened and cancels when that window closes.
Rename Window and the command palette remain available in READONLY mode; terminal
input and unsafe commands remain blocked.

Titles appear in applicable OS window lists, previews, and switchers. Windows
uses taskbar previews and Alt+Tab; macOS uses window lists, Dock window menus,
and Mission Control, not application-level Cmd+Tab. Linux display depends on the
X11/Wayland desktop. The OS may hide or truncate labels; app grouping, icons,
application IDs, and the Dock application label are unchanged.

### Select and copy text

Drag to select cells. Double-click to select a word. Triple-click to select a
line. Continue dragging after a double- or triple-click to extend by whole words
or lines. SonicTerm does not auto-copy when the button is released.

A drag belongs to the pane where it started. Over another pane, a gap, or outside
the window, the selection extends only to that pane's nearest cell. If the parser
is busy when SonicTerm captures the press's selection snapshot, or a resize removed
the pressed cell before the window redrew, the press starts no new selection and
keeps the current selection and pane focus. This snapshot does not wait for the
parser; the earlier terminal mouse-profile read can still wait. Switching tabs,
closing the pane or tab, or a resize that changes the pane's rows or columns ends
the drag.

Mouse-aware terminal applications can request the left button and drag motion.
In such a TUI, start with **Shift-drag** to bypass mouse reporting and make a
local SonicTerm selection. The choice is made on the initial button press and
lasts until release.

Use the platform copy shortcut after selecting. A successful explicit copy on
the alternate screen clears that selection and removes its highlight. A failed
clipboard write leaves a still-valid selection in place so you can retry. A
primary-screen selection remains after a successful copy. Repainting selected
cells to the same complete character/style/hyperlink/wide/combining identity
keeps the selection; an actual selected-cell change clears it before copy, and so
does a change to where automatic wrapping joins the selected rows.
Terminal applications may also write UTF-8 text through OSC 52 target `c` up to
512 KiB. Clipboard reads/queries, malformed Base64, other selection targets, and
oversized writes are ignored.

Copy follows the wraps the terminal recorded. A row that continues the previous
one because output reached the right edge joins it with no line break, and a
space at the wrap point is kept, so a long command, path, or URL pastes as one
line. A real line break copies as a newline, with trailing spaces trimmed there
and at the end of the selection. A row stops counting as wrapped, and copies as
its own line, when an edit changes the last column of the row above it, or when
a line-feed control, scrolling, an inserted or deleted line, a screen erase, or a
resize reaches it. Rewriting or erasing the continuation row itself from its first
column keeps the wrap, as xterm and WezTerm do, so a shell that redraws a wrapped
command line, such as zsh with syntax highlighting, still copies it as one line.
A wide character that does not fit in the last column wraps early and leaves that
column blank; the blank copies as a space.

READONLY mode blocks terminal input while you inspect history. Arrow keys or
`h/j/k/l` move its reading cursor; `w/b`, `0/$`, and `g` / `G` move by word, line, and buffer. Press `Escape` to exit. READONLY does not create a text
selection. Search, tab switching, pane focus, update checks, and saving current
font settings, the command palette, and window renaming remain available. See [Keybindings](Keybindings) for the exact
controls and whitelist.

### Paste text and drop files

A paste goes to the active pane. When that pane is the broadcast source, the
paste also goes to each broadcast receiver. Dropped files go to the active pane
of the window they land on, not to the pane under the pointer, and to that
pane's broadcast receivers when it is the broadcast source. SonicTerm encodes
the paste separately for each pane that receives it.

Pasted text is sent unchanged. A pane whose program has turned on bracketed
paste gets the text inside bracketed-paste markers; other panes get plain text.

Files dropped together become one line of quoted paths, separated by spaces.
SonicTerm adds no Enter, so you can check the line before you run it. Each pane
quotes the paths for the shell it was started with:

| Shell the pane was started with | Path quoting |
| --- | --- |
| `sh`, `bash`, `zsh`, `dash`, `ksh` | POSIX shell quoting |
| `pwsh`, `powershell` | PowerShell single quotes; a single quote inside a path, including a typographic one, is doubled |
| `cmd` | Double quotes |
| Any other shell, such as `fish` | POSIX shell quoting |

Names match with or without `.exe`, in any letter case. Quoting follows the
shell the pane was started with: a shell you start inside it, or one you reach
over SSH, does not change it.

A pane refuses a paste rather than send input that could be misread or cut off:

- a dropped path that is not valid Unicode, or that contains a control
  character;
- for `cmd`, a dropped path that contains `"`, `%`, or `!`;
- a paste larger than 16 MiB after encoding, counting quotes, spaces, and
  bracketed-paste markers.

One refused path refuses the whole drop for that pane, and a refusal in one pane
does not stop the others. The window where you pasted or dropped shows one
warning. It says how many of the receiving panes refused the paste, names each
reason with the number of panes it affected, and, for an oversized paste, gives
the size it needed and the limit. The warning never shows the paths or the
pasted text.

In a READONLY window, pastes and file drops send nothing to the terminal. While
search is open in either a writable or READONLY window, clipboard paste belongs
only to that window's search query and never reaches the shell or broadcast peers.
If its active pane is temporarily unavailable, the query is unchanged and the paste
is consumed; see Search retained output. On Linux X11, if any dropped file name is
not valid UTF-8, the drop delivers no files.

### Terminal multiplexers

tmux, rmux, GNU screen and Zellij decide which links, working directories, keys
and clipboard writes reach SonicTerm. See [Terminal Multiplexers](Terminal-Multiplexers)
for what each one passes through, recommended tmux and rmux settings, and how
links, paths, the mouse and the clipboard behave in panes.

### Open URLs and local targets

Hold `Cmd` on macOS or `Ctrl` on Windows and Linux while pointing at a target.
A valid target becomes underlined; click it to open. OSC 8 links and plain-text
`http://`, `https://`, `mailto:`, and `file://` URIs take priority over raw
filesystem detection. Unrelated terminal output and same-value repaints do not
blink an unchanged target; changing the pointed row, target, CWD, viewport, or
openability identity revokes authorization and requires a fresh probe.

Plain hover underlines both detected URLs and OSC 8 labels with the theme's yellow
hint; the open modifier switches to the action accent. Plain hover leaves glyph
foreground colors unchanged. A temporarily busy parser does not remove an
unchanged hint. A busy hover lookup requests a later coherent frame, so moving
the pointer or changing Cmd/Ctrl does not leave feedback waiting for unrelated
terminal output. This applies to main and child windows in GPU and software
rendering; clicks still require fresh target validation. OSC 8 coverage follows the
contiguous label across recorded automatic wraps, including wide cells. On the
alternate screen it also continues into the next row's fragment of the same link
when, inside one pane, at most two blank cells follow the upper fragment before the
pane's right edge and only blank indentation of at most eight cells precedes the
lower fragment from the pane's left edge; a rule or border glyph there is not blank. This covers multiplexer pane edges and
apps such as Claude Code that wrap a long link with a hanging indent and a small
right margin. Repeated short links on consecutive rows, which leave more blank cells
after the upper one, keep separate underlines. On the primary screen only a recorded
soft wrap continues an underline. Coverage never crosses other hard line breaks or
gaps into another occurrence, and a click always opens the stored destination. At
most 32 visible fragments are painted, always retaining the pointed fragment of an
overlong label.
URLs inside prose parentheses or square brackets are detected without including
the surrounding wrappers in the destination or underline. Plain-text URLs also
join across recorded automatic margin wraps: pointing at any fragment resolves
the complete destination and highlights every fragment. Reconstruction requires
the complete logical line to remain visible, within 32 rows and 16 KiB;
incomplete or oversized chains are inert rather than opening a truncated prefix.
An application-hard-wrapped HTTP(S) URL can also join when `(` or `[` directly
precedes its scheme and the matching closer is visible. Until the next whitespace
or row edge, only ordinary sentence punctuation may follow that closer; adjacent
URL text makes the boundary ambiguous and prevents reconstruction. Its complete authority
and first path slash must appear before the first break; every non-final fragment
must reach the right edge, and continuation rows must have the same indentation
of at most eight ASCII spaces. The same 32-row limit applies, and the joined URL
is capped at 4 KiB. Only the
indentation and line boundaries are removed; query text, percent escapes, and
hyphens remain literal. Whitespace inside a fragment, nested wrappers, unsafe
cells, mixed wrap kinds, and multiple schemes prevent reconstruction. Incomplete
recognized fragments never fall back to a truncated URL. Unwrapped hard rows and
local paths are not joined; applications can use OSC 8 for arbitrary label layouts.
On the alternate screen, a full-screen program such as a multiplexer can continue a
line on the next row with a cursor move that records no wrap. There, a pane segment
whose last column holds text also continues into the next visible row when that row
has a segment with the same left and right pane edges that starts with text, unless
a wrap the terminal recorded between those two rows joins different panes. Such a chain stops at
32 rows and at the top and bottom of the view. These joins carry both paths and plain
URLs. tmux separates rows with CR LF when it redraws a pane (on refresh, resize, or a
window switch), so a long URL in tmux relies on them. A path that crosses a join is
offered only when the joined file exists. Unrelated rows that exactly fill a pane edge
can join into a longer URL; the modifier-hover preview shows the full destination
before anything opens. A candidate that reaches
a pane edge where the chain stopped is refused, since its text may continue out of
view; when the unspaced text under the pointer reaches such an edge, nothing there is
linked. A bracketed URL still joins as above in a full-width pane, but not in a split
pane, where the next row begins with another pane's text. The primary screen joins
only recorded wraps, so full-width `ls` columns never join; see
[Terminal Multiplexers](Terminal-Multiplexers).

Modifier-hover shows a local destination only after its current filesystem probe
validates it. Only the selected existing path is underlined and previewed. Pending,
missing, blocked, or rejected local targets have no preview, so directory-listing
columns are never shown as unverified paths. It adds no action labels or
error messages, and does not change the clipboard or authorize navigation. It uses
the same placement, escaping, wrapping, and dismissal as URL previews.

Holding the same modifier over any URL shows its destination beside the pointer,
including labeled OSC 8 links, links whose label already equals the destination,
and auto-detected plain-text URLs. Click while holding the modifier to open it.
Query separators such as `&` are preserved unchanged, so links with multiple query
parameters (including repository file and line links) can be opened. Scheme,
length, control-character, and other forbidden-character checks still apply.

Terminal underline styling continues across explicitly printed spaces that carry
the same underline style and color. Unstyled cells remain gaps; clearing cells
does not paint new underline ink even when underline mode is active.

The preview does not fetch a website or authorize navigation. Local file URIs
require the same path validation; control and directional formatting characters are escaped for display.
Failures are explained in click-triggered error notifications, not in the preview. Long destinations wrap,
with an explicit ellipsis if they cannot fit. Release the modifier or leave the
link to hide it; focus, modal, pane, viewport, and content changes refresh or clear
it. GPU and Windows software rendering use the same preview overlay.

Raw local targets include:

- native absolute paths such as `/usr/local/etc`, `C:/Users/name`, and
  `C:\\Users\\name`;
- current-user home paths such as `~/notes` and Windows `~\\notes`;
- separator-relative paths such as `src/main.rs` and Windows `src\\main.rs`;
- explicit relative paths such as `./file`, `../file`, and `../../file`;
- contextual names such as `sonicterm`, `.DS_Store`, or `My Folder`.

These forms can contain ordinary spaces. Relative and contextual forms require
the exact pane to report a trustworthy absolute local working directory through
OSC 7. A missing, malformed, or foreign-host OSC 7 value fails closed. SonicTerm
never substitutes the process working directory, another pane’s directory, or a
named user’s home.

On the alternate screen, relative and contextual forms are not linked: a
full-screen program such as a multiplexer can show several panes while it
reports one directory, and the screen cannot show which pane holds the text. The
rule covers every full-screen program; see
[Terminal Multiplexers](Terminal-Multiplexers).

The background probe checks at most 37 candidates, and each candidate spans at
most eight non-space parts. Candidates longer than
`terminal.clickable_path_max_chars` (1024 by default), counted in Unicode
characters of the displayed text, are ignored. Logical display-line reconstruction
is capped at 16 KiB and 32 consecutive rows, so a path or URL of up to 1024
characters can be detected and underlined at ordinary widths (about 13 rows at 80
columns); the scanner still caps each target at 4 KiB. On the primary screen,
SonicTerm joins path fragments only across recorded automatic margin wraps, and a
hard line break is never joined; the alternate screen also joins the pane-edge
continuations described above. The complete chain must remain visible. Every
fragment then shares one authorization and underline. A 33rd recorded-wrap row,
an offscreen edge, or an evicted predecessor leaves the chain inert.

A candidate must contain the pointed cell. It ends at whitespace or at trimmed
punctuation, including full-width `，。；：`, never in the middle of a word.
SonicTerm probes candidates shortest first, by the number of Unicode characters in
the displayed candidate; candidates of equal length are tried from the earlier
start. A length whose candidates all name no file is skipped. At the first length
with any existing candidate, a blocked candidate ends the probe with the blocked
refusal; otherwise the earliest actionable candidate wins. Directories count. With
both `/usr/A` and `/usr/A B` present, pointing inside `/usr/A` opens `/usr/A`. For a
path ending in prose punctuation such as `src/main.rs,`, the candidate without the
trailing comma, semicolon, period, colon, exclamation mark, or question mark is
shorter, so it wins when it exists; the underline then excludes the prose
punctuation. Within one space-delimited token beginning with a native absolute,
current-home, or dot-relative path, the first eligible Unicode Other Punctuation
character can also separate a leading path from prose. For example,
`~/.claude/settings.json，将` opens `~/.claude/settings.json`. The active span
excludes the separator and prose, but their cells remain part of safety validation.
Unicode filename characters are preserved; path syntax characters are not
separators. This does not add prose splitting for spaced paths or change the OSC 7
requirement for `./` and `../`. Current-home paths do not require OSC 7.

Shortest first can choose a shorter existing prefix: pointing at `OneDrive` in
`OneDrive - Microsoft` opens `OneDrive` when a folder of that name exists. Point at
a later word, or use an explicit path or an OSC 8 link, to reach the longer name.

When you click, the open worker repeats the selection over every candidate at or
before the selected length. It opens only when that still yields the same path and
action, so a shorter or earlier file that appeared or became blocked since hover
stops the open, and the failure is reported in the window. A new candidate at or
before the selected length also drops a cached authorization.

A complete standalone single-quoted contextual name, such as `'My Folder'`
from `ll`, is treated as `My Folder`. Explicit paths also accept one complete
single-quote, double-quote, or backtick pair, including paths with spaces such as
`'C:\work\My Folder'`. The quotes are excluded from the active span but included
in cell safety checks. Quote contents are literal: no shell unescaping or variable
expansion. Unmatched/mixed quotes, concatenated text, padded contents, `$`/`%`
expansions, other quoted bare names, `ls -F` suffixes (`*`, `@`, `=`, `|`), and
raw paths containing broken wide-cell pairs, combining extras, control characters, or OSC 8-owned cells remain inert. Valid wide-character pairs retain their exact filename and cell span.

Terminal messages can contain actionable file references, including a balanced
identifier-style tool heading such as `Update(src/main.rs)` or `Read(./notes.txt)`.
The detected inner path excludes the heading and its enclosing parentheses.
Wrapped paths and source locations also accept following sentence punctuation,
such as `(src/main.rs:97).`, `[src/main.rs:97:4],`, `{src/main.rs};`, or
`Read(src/main.rs:97–100)!`. The wrapper and outer punctuation are excluded from
the active span; punctuation inside the wrapper still follows shortest-first probing.
Paired structures are recognized before surrounding prose: `(reports/flight.html)，内容`
and `【reports/flight.html】，内容` keep the same inner target. Supported pairs include
`()`/`[]`/`{}`, ASCII quotes/backticks, `（）`/`【】`/`《》`/`「」`/`『』`,
`“”`/`‘’`/`«»`, and identifier-style calls. Outer separators use Unicode's
Other Punctuation and Dash Punctuation categories, not a language-specific list.
Path separators, mismatched closers, direct concatenation, and dot/colon suffix
continuations such as `(src/main.rs).bak` remain ambiguous and inert. This does
not interpret raw Markdown or strip invisible characters. Valid wide characters
retain both cells in safety validation, including filename content. A missing or
unsafe boundary never permits a shorter inner fragment.

Rooted log-field values such as `path=C:\work\file.exe` and
`file="C:\My Folder\report.md"` exclude the key and matching quotes from their
active span. Relative assignments and concatenated or incomplete quotes do not
receive this rule. Existing `=` characters inside a filename remain literal.

Unwrapped lists such as `src/a.rs、b.rs` offer the pointed file member and the
complete literal filename. The member is shorter, so it wins when it exists, even
if the literal exists too; the literal wins only when the member names no file.
The second name resolves only against this pane's CWD, never an inferred `src`
directory. Hyphens are not list separators.
In prose such as `src/main.rs and focused tests/main.rs. Require stable`, point
at either filename to resolve it independently. `and` is not a reserved word:
existing filenames containing spaces or parentheses are still found by probing the
filesystem. Pending or blocked contextual paths use the pointed filename for
feedback, not an unverified multiword prose guess. Unverified rooted spaced paths retain
spaces only when the final component has an extension and no earlier filename-like
word makes their extent ambiguous; other multiword guesses require validation.
Validated files always retain their complete path.

Source references such as `install.sh:889–919`, `src/main.rs:12`, and
`src/main.rs:12:4` retain the full underline but resolve only the filename.
Line and column values must be positive; ranges accept `-` or `–` and must not
run backwards. Relative source names still require the exact pane's trusted CWD.
Windows, macOS, and Linux select the referenced file in its containing folder.
Line metadata does not launch an editor or restrict the file's type or contents.
Grouped citations such as `(src/main.rs:924, :934, :375).` share one explicit path
and accept up to eight validated locations. Pointing at a location selects its
metadata; pointing at the filename selects the first location. The complete group
is underlined and validated together; separators and outer punctuation do not
initiate navigation. Malformed members invalidate the whole group. Group anchors
use unquoted paths without spaces; quoted individual references support spaces.
A spaced anchor carrying grouped locations is entirely inert, including the
anchor itself, rather than falling back to a shorter filename. Unwrapped groups
start a segment or follow another complete group; after prose, use `()`/`[]`/`{}`
to make the anchor boundary explicit. A wrapper after plain words is read as
prose; it is not a continuation of a spaced relative filename.

### PowerShell directory links

Interactive PowerShell 7.2 or newer started by SonicTerm adds full local file-URI
links to the default directory display. A name split across display rows retains
the same target on each fragment; padding and following entries are not linked.
Mode, date, length, and name colors are preserved. `ls` remains `Get-ChildItem` and
object pipelines remain unchanged. Explicit `Format-Table` keeps its native view.
Plain-text/redirection output omits terminal decoration.

The integration is process-local, embedded in SonicTerm, and writes no profile or
format file. Custom file-name getters or nonstandard file views are left alone;
legacy PowerShell and constrained-language hosts keep their original formatting.
Existing output is not rewritten. Local-target validation still applies; clicking
an executable reveals it rather than runs it.

### Manual link checks

From the repository root, run `./scripts/test-local-link-actions.ps1` inside
SonicTerm on Windows. It prints numbered cases with expected previews and click
results, and creates inert files in a unique temporary directory. It does not
open targets, change configuration, or write the clipboard. Delete the printed
fixture directory after testing.

Use `-Group Web`, `Osc8`, `Paths`, `Wrappers`, `Source`, `Negative`, `Wrapping`,
or `KnownGaps` to inspect one group at a time; the default is `All`. Known gaps
are labeled separately, not represented as supported behavior. For wrapping
checks, resize the window and keep the complete target visible. Report the group,
case ID, preview, and observed click result. This manual matrix complements the
scanner/app regression tests; it is not exhaustive proof over arbitrary text.

### Local target behavior

Use Ctrl+click on Windows/Linux or Cmd+click on macOS.

| Target or state | Expected behavior |
| --- | --- |
| Existing ordinary directory | Navigate into the directory in the file manager. |
| Existing regular file, including scripts, executables, installers, and shortcut files | Open its containing folder and select the file itself. Never execute it, follow a shortcut, or invoke its associated application. |
| macOS application/package directory | Select the package in Finder without launching it. |
| Existing bare filename in a listing or prose | Resolve against the exact pane's trusted local working directory; underline and select only the validated filename, including spaces. |
| Unverified bare name or ordinary text | No preview, file action, error notification, or clipboard write. Listing metadata is not a filepath. |
| Detected path text whose candidates all name no file | No underline or preview. A modifier-click is an ordinary terminal click: no notification and no clipboard write. A debug log line is still written. |
| Explicit filepath clicked before validation finishes | Show the filepath and the validation-pending reason on modifier-click. Do not navigate or copy on the first click. |
| Local file URI or OSC 8 link naming no file | Show the filepath and the missing reason on modifier-click. Do not navigate or copy on the first click. |
| Explicit blocked or rejected filepath | Show its filepath and refusal reason; never bypass identity or locality checks. |
| First file-manager action failure | Show the attempted filepath, reason, and second-click copy instruction. Leave the clipboard unchanged. |
| Second modifier-click on the same failed filepath while its error is visible | Copy that filepath instead of retrying, and report copy success or failure. |
| Dismissed, expired, or replaced error | A subsequent click starts a new attempt rather than confirming the previous error. |
| HTTP/HTTPS or mail URL | Keep URL preview and browser/mail navigation. |

Explicit filepaths include native absolute paths, `./`/`../`/`~/` paths,
relative paths containing separators, source-location references, and local file-URI
or native-path OSC 8 destinations. Explicit paths retain spaces in their filenames.
For unverified source references with a bare filename, failure feedback excludes
surrounding prose; spaced bare filenames require filesystem validation or an
explicit path/OSC 8 destination. Bare names become filepath targets only after
filesystem validation. File extensions, executable permissions, and file contents
do not prevent selection. On Windows, SonicTerm must be able to read each part of the
path or, where reading is denied, execute it (traverse it, for a folder); a part that
allows neither, or one another program holds without read sharing, is refused, and while
a check runs, other programs cannot rename or delete the parts it holds. Symlinks anywhere
in the path are followed, and the target they resolve to decides whether a file is
selected or a folder navigated; a dangling or looping link is refused. On Windows,
resolving a link to a network share contacts that server, so paths are checked part by
part from a drive letter that maps exactly to a `\Device\HarddiskVolume<N>` volume whose
root reports a local disk, holding each checked part open; a symlink or junction is
followed only between local fixed disks, and no remote volume is opened. Paths on mapped
network drives, `subst` drives, optical or RAM drives, and dynamic-disk volumes or shadow
copies mounted on a letter, and paths through a folder where a volume is mounted by GUID,
are not linked, and a path on a letter that names no drive is refused rather than reported
missing. Links to a UNC path or a device path, links on removable drives, and other
reparse points such as cloud-file placeholders are refused before anything they name is
opened. At dispatch, Windows walks the path again and hands the shell the link-free
path it walked while every part stays held, so no folder above the final file or folder
can be renamed, removed or turned into a link, and a file reached through a link is
selected in its real folder. The shell, Explorer and the file's handler then open that
path themselves: the final part can still change in place, and nothing is held once the
shell call returns. A process in your own logon session can redefine drive letters and
reach the network directly, so it is out of scope. Special devices and unsupported
remote/network paths remain protected. Every platform revalidates target identity and kind, following the whole link
chain again, immediately before dispatch.
All platforms navigate directories and reveal files with the file selected,
without invoking the file's application. Windows selects files through
`SHOpenFolderAndSelectItems`; Finder uses `/usr/bin/open -R -- <target>`; Linux
uses `org.freedesktop.FileManager1.ShowItems`. Unavailable or rejected selection
is reported in the requesting window without falling back to file opening or
parent-only navigation. Only a validated local target invokes a native file action.
Explicit filepaths that are pending, blocked, or rejected, and local links that name
no file, receive click-triggered feedback; detected text that names no file and
guessed bare-name spans do not.
The first failed action shows the attempted filepath, reason, and an instruction to
click the same link again while the error is visible. It does not change the clipboard.
The second modifier-click on that same failed filepath copies it instead of retrying
navigation, then reports copy success or failure. Dismissed, expired, or replaced
notifications and clicks on another target do not confirm a previous failure.
Short notifications fit their longest shaped line. Notifications preserve newlines
and wrap Unicode graphemes; content exceeding the viewport is marked with an ellipsis.
Hover never copies. Native file-manager errors return only while the initiating
pane still belongs to its window. A click without the modifier keeps normal selection.
Directory navigation retains each platform's ordinary directory opener. Path-based
file-manager requests still have the normal pathname race after revalidation.

OSC 8 destinations with native absolute paths (including Windows `C://…`),
Windows drive-rooted `file:c://…` links, and local `file://` URIs enter the same
filesystem authorization path. Local `#3`, `#L3`, and ascending `#L3-L7` fragments
are source-line metadata, not filename content; selection uses the file alone.
This convention also applies to native absolute OSC 8 destinations. For a literal
filename ending in `#3` or `#L3`, use a file URI with `%23` for the hash. Plain-text
native filename scanning retains its existing literal-hash behavior. File URI escapes
are decoded once; native-path percent characters remain literal. Remote authorities,
UNC/device paths and malformed destinations are rejected without using the displayed
label as a substitute. HTTP/HTTPS and mail links retain their existing behavior. The
URI opener itself accepts only `http`, `https`, and `mailto` and refuses every
`file:` URI, so a file link reaches the filesystem only through this authorization path.

Opening a URI on Windows takes the same shell-free boundary as a validated
local target: `ShellExecuteExW` receives the URI as one NUL-terminated UTF-16
string, so no command interpreter parses it. Environment substitution stays disabled, so a
percent-delimited URI such as `https://example.com/%20space` or one containing
`%USERNAME%` reaches your browser or mail client exactly as shown on screen
rather than expanding to an environment value.

Set `terminal.clickable_bare_names = false` to disable contextual names. Set
`terminal.clickable_local_targets = false` to disable every raw local target.
Set `terminal.clickable_path_max_chars` to change the longest detected candidate.
The local-target setting also applies to local file URIs and native-path OSC 8 links;
web/mail URI links remain enabled. For exact defaults and reload
behavior, see [Configuration](Configuration).

### Open script files as drafts

Installed macOS builds can appear in Finder’s **Open With** menu for `.sh`,
`.command`, and `.tool`. The Windows MSI registers SonicTerm as an available
handler for `.ps1`, `.cmd`, `.bat`, and `.sh`. Installation does not replace the
current default handler.

Opening a supported file creates a tab whose working directory is the file’s
parent. SonicTerm safely quotes an absolute path and inserts a command at the
prompt **without Enter or another control byte**:

- POSIX `sh`, `bash`, `zsh`, `dash`, or `ksh`: `.sh`, `.command`, `.tool`;
- PowerShell or `pwsh`: `.ps1`, `.cmd`, `.bat`;
- Command Prompt: `.cmd`, `.bat`, provided the path contains none of `%`, `!`,
  or `"`.

Review, edit, submit, or clear the draft yourself. An unknown shell, unsupported
shell/script pair, relative or non-Unicode path, control character, or unsafe
Command Prompt path still opens the tab but shows a warning and inserts no
command.

This is a draft-input feature, not a sandbox. Shell startup files run first and
can read PTY input. A startup profile that reads and evaluates input can execute
or consume the draft without Enter. Do not select SonicTerm as a script handler
when your shell startup code does that. SonicTerm itself does not run the script
or interpreter for the open action.

On Windows, each file-association invocation starts a new SonicTerm process. On
macOS, later open requests go to the running app and add tabs.

### Configure and troubleshoot

Use these canonical pages instead of duplicating their detailed rules here:

- Preferences, defaults, reload, and save: [Configuration](Configuration)
- Shortcuts, actions, and READONLY controls: [Keybindings](Keybindings)
- Theme schema and colors: [Themes](Themes)
- Logs, crash files, and diagnostics: [Logging](Logging)
