# OSC Support

[中文](OSC-Support-zh-CN)

Programs talk to the terminal with Operating System Command (OSC) sequences: `ESC ] <number> ; <payload>`, ended by `ESC \`
or BEL. This page lists every OSC that WezTerm, Ghostty or kitty implements, and what SonicTerm does with each one.
SonicTerm implements every form that at least two of those terminals implement, and any form only one of them implements
when common programs send it. The rest are listed under [Not supported](#not-supported-and-why) with the reason.

A sequence SonicTerm does not support is read to its terminator and dropped, so its payload never prints. Parsing details,
size limits and the exact reply bytes are under [Terminal IO and VT](Terminal-IO-and-VT). Clipboard policy keys are under
[Configuration](Configuration).

In the "Also in" column, W is WezTerm, G is Ghostty and K is kitty.

## Supported

| Sequence | Meaning | Also in | Notes |
|---|---|---|---|
| `0`, `2` | Window title | W G K | `0` also clears the tab name set by `1`. |
| `1` | Tab name | W | Shown as the tab label until `0`, an empty or bare `1`, or a reset (RIS); oh-my-zsh and fish send it. The window title is unchanged, and a tab you renamed keeps your name. |
| `4 ; n ; spec` / `?` | Set or query palette entry `n` | W G K | Several pairs per sequence; replies `rgb:rrrr/gggg/bbbb`. |
| `104 [; n…]` | Reset palette entries, or all | W G K | |
| `10`/`11`/`12` `; spec` / `?` | Foreground, background, cursor color | W G K | Each further value moves to the next color. |
| `17`/`19` `; spec` / `?` | Selection background, foreground | W K | Selected text takes the foreground color only after a program sets `19`. |
| `110`, `111`, `112`, `117`, `119` | Reset those colors | W K (G: 110–112) | |
| `21 ; key=value;…` | kitty color protocol | G K | Palette, `foreground`, `background`, `cursor`, `cursor_text`, `selection_*`; unknown keys answer `unknown=`. |
| `22 ; [=><?] names` | Pointer shape, stack and query | G K | CSS names and X11 aliases; separate stacks for the main and alternate screen. |
| `7 ; file://host/path` | Working directory | W G K | `kitty-shell-cwd://` too. Only a local host authorizes local paths; empty clears. |
| `9 ; 9 ; path` | Working directory (ConEmu) | G K | An absolute path, optionally in one pair of quotes; used like a local OSC 7. |
| `1337 ; CurrentDir=path` | Working directory (iTerm2) | G | A local `file://` or `kitty-shell-cwd://` URI is used like OSC 7; a bare path is only displayed. |
| `8 ; params ; uri` | Hyperlink | W G K | |
| `9 ; text` | Notification | W G K | |
| `777 ; notify ; title [; body]` | Notification | W G K | With no body, the text is shown as the body. |
| `99 ; …` | kitty notification protocol | K | |
| `9 ; 4 ; state [; percent]` | Progress (Dock, taskbar, launcher) | W G K | Clears after 60 s without an update, or 5 s after 100 %. |
| `52 ; kinds ; base64` / `?` | Clipboard write or read | W G K (W: writes only) | `p` is the primary selection on Linux. `52;c` with no data, or with an empty one, clears. Writes follow `osc52`, reads follow `osc52_read` (`ask` by default, which opens a consent page). |
| `1337 ; Copy=:base64` | Clipboard write (iTerm2) | G | Same policy as OSC 52 writes; an empty or `?` value writes nothing. |
| `5522 ; …` | kitty clipboard protocol | G K | `write`, `wdata`, `walias` and `read`, type listing (`.`), and session grants. D-6d: omit unmappable or over-cap names; only Windows replaces invalid UTF-8 and uses registered PNG without DIB conversion. X11 program offers expire after one hour; native Wayland needs XWayland. Per-platform writes, reads and listings are under [Terminal IO and VT](Terminal-IO-and-VT). |
| `133 ; A/B/C/D/L/N/P/I` | Shell prompt marks | W G K | A click in the prompt moves the cursor: as an SGR press when the prompt sets `click_events`, else as arrow keys (`special_key=1` sends kitty's keys) under `[terminal] click_to_move`, never on the alternate screen. `redraw=0` turns off keeping the cursor row on a rows shrink, which is not done on Windows. `C;cmdline_url=` feeds the long-command notification when that is enabled. |
| `9 ; 12` | Prompt start (ConEmu) | G | |
| `1337 ; File=…` | Inline images | W | |
| `1337 ; SetUserVar=name=base64` | User variable | W K | Shown to agents in `list_panes`. |
| `7501 ; state=…` / `?` | Program status (working, blocked, done, error, idle) | K | Shown as a tab marker and in `list_panes`; `state=clear` removes it. Claude Code sends it. |
| DECSET 1005, 1006, 1015, 1016 | Mouse encodings | W G K (1015: G K) | 1016 reports zero-based pixels, as Ghostty and kitty do; WezTerm adds 1. 1005 stops at column and row 2015. |
| DECSET 5522 | Paste events for the kitty clipboard protocol | G K | DECRST and RIS reset it; DECRQM reports it. |

## Not supported, and why

| Sequence | Reason |
|---|---|
| `66` text sizing, `72` drag and drop, `5113` file transfer, `30001`/`30101` color stack | Only kitty implements them, and common programs do not depend on them. File transfer writes files on request. |
| `21` `visual_bell`, `transparent_background_color1..8` | SonicTerm has no such setting; queries answer empty. |
| `1337` `ReportCellSize`, `UnicodeVersion` | Only WezTerm. For the cell size, use `CSI 16 t`. |
| `1337 ; Copy=;base64` | WezTerm parses it but never acts on it; use `Copy=:`. |
| Other `1337` keys (`SetMark`, `RemoteHost`, `SetBadgeFormat`, `RequestAttention`, …) | None of WezTerm, Ghostty or kitty acts on them. |
| `13`–`16`, `18`, `113`–`116`, `118`, `5`, `6`, `105`, `106`, `3`, `46`, `50`, `51`, `60`, `61` | Pointer, Tektronix, special colors, X properties, logging and fonts; ignored by all three. |
| ConEmu `9;1`–`9;3`, `9;5`–`9;8`, `9;10`, `9;11`; other `777` subcommands | Not acted on by any of the three. |
| `3008`, `176`, `440`, `633`, `697` and other vendor numbers | Ignored by all three. |
| The "mouse left the window" report | Only kitty. |

## Windows

On Windows a program's output passes through ConPTY first, so a sequence ConPTY does not forward never reaches SonicTerm.
The installed SonicTerm (MSI) ships its own packaged ConPTY runtime and always uses it; a packaged runtime that fails its
checks stops pane startup rather than falling back. Only a source build with no `conpty` folder uses the system ConPTY
([Terminal IO and VT](Terminal-IO-and-VT) describes the selection).

A real-ConPTY test on Windows CI ([Local Gate](Local-Gate)) prints sample forms through both runtimes and records, for
each direction, whether the bytes arrive unchanged. It checks transport only: what SonicTerm then does is covered by
the unit tests.

| Forms tested | Packaged ConPTY | System ConPTY |
|---|---|---|
| OSC 4, 10–12, 17, 19, 21, 22, 104, 110–119 set, reset and query; 7501; OSC 52 and 5522 writes and reads; DECSET 5522 paste events; 9;9, 7 (`file://`, `kitty-shell-cwd://`), 9;4, 777, OSC 1 | arrive unchanged, replies too | dropped |
| OSC 133 marks and options, OSC 9;12, `1337 ; SetUserVar`, `CurrentDir` and `Copy=:` | arrive unchanged | arrive unchanged |
| DECSET 1005, 1006, 1015, 1016 | arrive unchanged; reports too | the mode request is dropped, so the program never gets these reports |
| DECRQM 5522 | arrives unchanged, reply too | request dropped |
| Prompt click events | arrive unchanged | arrive unchanged |
| Rows shrink at a prompt | ConPTY repaints the screen itself, so SonicTerm receives the console's repaint, not the rows the program printed | same as packaged |

Forms not in the test, such as OSC 0, 2, 8, 9, 99 and `1337 ; File=`, have no recorded Windows result.
