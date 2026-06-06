# SonicTerm User Guide

Everything SonicTerm does today, with the keybinding or config line you need to
make it happen. Keys are written WezTerm-style: `super` is `⌘` on macOS and
`Ctrl` on Windows unless otherwise noted.

> If a default binding clashes with something you already use, edit
> `~/Library/Application Support/SonicTerm/sonicterm.toml` (macOS) or
> `%APPDATA%\SonicTerm\sonicterm.toml` (Windows) — see [Configuration](#configuration).

---

## Table of contents

1. [Performance](#performance)
2. [Rendering](#rendering)
3. [Input](#input)
4. [Search](#search)
5. [Windows, tabs, and panes](#windows-tabs-and-panes)
6. [Configuration](#configuration)
7. [Themes](#themes)
8. [Internationalization (i18n)](#internationalization-i18n)
9. [Command palette and keymap](#command-palette-and-keymap)
10. [Selection, copy, and opacity](#selection-copy-and-opacity)
11. [SSH client (optional)](#ssh-client-optional)
12. [Multiplexer (`sonicterm-mux`)](#multiplexer-sonicterm-mux)
13. [Code signing](#code-signing)
14. [Regression net](#regression-net)
15. [Troubleshooting](#troubleshooting)

---

## Performance

SonicTerm is GPU-native. Every glyph goes through an atlas-backed cache rendered
by `wgpu`, and the event loop only redraws when something actually changed.

- **Idle CPU: 0%.** When the grid hasn't changed, the renderer skips the
  frame entirely. Move the mouse over an idle SonicTerm window in Activity
  Monitor and you'll see it drop to zero.
- **Scroll throughput: ~34k lines/sec** on an M2 Air at 1440p.
- **Mailbox present mode** keeps input-to-screen latency low by always
  presenting the newest frame — no queue buildup when you mash keys.
- **Dirty-row tracking** means a one-line prompt update only re-rasterizes
  one line, not the whole grid.
- **Frame-skip cache** is invalidated by grid revision: when the parser
  bumps the revision (a write to the grid), the next frame is forced; when
  it doesn't, the previous frame is kept.

You don't have to enable any of this. It's how SonicTerm always runs.

---

## Rendering

SonicTerm ships a single font fallback chain that has been tuned by hand for
every script you're likely to type:

| Platform | Chain (in order) |
|---|---|
| macOS  | Rec Mono St.Helens (bundled, Nerd-patched — Powerline + NF PUA) → PingFang SC → Hiragino → Apple SD Gothic Neo → Apple Color Emoji |
| Windows | Rec Mono St.Helens (bundled, Nerd-patched — Powerline + NF PUA) → Microsoft YaHei → MS Gothic → Malgun Gothic → Segoe UI Emoji |

What works out of the box:

- **ASCII, CJK (Chinese / Japanese / Korean), Powerline, wide chars,
  combining marks, Latin diacritics.**
- **Color emoji** — rendered from the system color font as BGRA, with
  premultiplied alpha so they composite cleanly against any background.
- **ZWJ sequences** — family / profession / skin-tone emoji compose into a
  single grapheme cluster. `👨‍👩‍👧‍👦` renders as one glyph, not four.
- **Programming ligatures** — `=>`, `!=`, `>=`, `->`, `===` and friends
  ligate automatically when the font supplies them.

To use a different primary font, set it in `sonicterm.toml`:

```toml
[font]
family = "Fira Code"
size = 14.0
```

The fallback chain still applies for anything your primary font doesn't
cover.

---

## Input

### IME (Pinyin / Japanese / Korean)

SonicTerm implements the full winit IME protocol. The preedit string renders
**at the cursor**, and the OS candidate window is positioned via
`set_ime_cursor_area` so it never floats off to the corner of the screen.

You don't have to enable IME — turn on your usual input method (Pinyin,
Google Japanese Input, 한글 etc.) and start typing.

### Bracketed paste

SonicTerm advertises DECSET 2004, so pasted text from the OS clipboard is
wrapped in `\e[200~ ... \e[201~`. Shells that support it (bash 4+, zsh,
fish, nushell, PowerShell 7) will not run the paste as a command — it
arrives as one literal block.

- Paste: `super+v`.

### Shell integration (OSC 133)

When your shell emits OSC 133 prompt marks, SonicTerm draws a small **gutter
caret** beside each prompt and lets you jump between prompts:

- Previous prompt: `super+shift+up`
- Next prompt: `super+shift+down`

See [`shell-integration.md`](shell-integration.md) for the one-liner you
add to your `~/.zshrc` / bash / fish.

### Hyperlinks (OSC 8)

Any program that emits an OSC 8 hyperlink (e.g. modern `ls`, `gh`,
`cargo`) gets a clickable region. **Cmd+click** opens the URL.

URLs are validated before launch — only `http`, `https`, `mailto`, `file`
schemes are allowed, and shell metacharacters / control chars are
rejected. URLs longer than 4096 bytes are dropped.

---

## Search

| Action | Binding |
|---|---|
| Open search | `super+f` |
| Toggle regex | `super+r` |
| Toggle case-insensitive | `super+i` |
| Next match | `super+g` |
| Previous match | `super+shift+g` |
| Close search | `Esc` |

Search runs over the **visible grid + the full scrollback** and shows an
`N/M` indicator (current / total). It accepts plain substrings or regular
expressions. Matches recompute on every grid revision, so if the shell
echoes new output the indicator updates immediately — no stale results.

---

## Windows, tabs, and panes

### Tabs

| Action | Binding |
|---|---|
| New tab | `super+t` |
| Close tab | `super+w` |
| Next / previous tab | `super+shift+]` / `super+shift+[` |
| Jump to tab _n_ | `super+1` … `super+9` |

### Tab tear-out and merge

- **Drag a tab below the tab bar** to tear it out into a new in-process
  window. The shell keeps running — no PTY restart, no disconnect.
- **Drag a tab from one SonicTerm window onto another's tab bar** to merge
  windows. The source window stays open if it has other tabs; if it was
  the last tab, the source drains rather than exits, so the application
  remains alive.
- **macOS:** tabs can be dragged **across processes** via NSPasteboard —
  drop a SonicTerm tab onto a second SonicTerm.app instance. (Windows support is
  stubbed but not wired yet.)

### Panes

| Action | Binding |
|---|---|
| Split horizontally | `super+shift+d` |
| Split vertically | `super+d` |
| Focus next pane | `super+]` |
| Focus previous pane | `super+[` |
| Close pane | `super+w` |

Each pane owns its own PTY, parser, and grid — closing one cleanly kills
its child shell (no orphans).

---

## Configuration

User config lives at:

- macOS: `~/Library/Application Support/SonicTerm/sonicterm.toml`
- Windows: `%APPDATA%\SonicTerm\sonicterm.toml`

A minimal `sonicterm.toml`:

```toml
[font]
family = "JetBrains Mono"
size = 13.0

[appearance]
theme = "tokyo-night"
opacity = 0.96

[keymap]
preset = "wezterm"

[i18n]
# auto | en | zh-CN | ja
locale = "auto"
```

### Live reload

SonicTerm watches `sonicterm.toml` and the bundled theme / keymap files. Save the
file in your editor and the change applies in the running window:

- Font family / size: re-rasterizes the atlas on next frame.
- Theme: recomputes colors immediately.
- Keymap: rebinds without restart.

Unknown keys are **preserved** rather than erased, so a newer SonicTerm
config opened by an older SonicTerm doesn't lose data.

### Editing configuration

Open the command palette and run `Edit sonicterm.toml` or `Edit keymap.toml`
to open the editable user files in the OS default `.toml` handler. If
`sonicterm.toml` does not exist yet, SonicTerm creates it with a short commented
header first. Saved changes are picked up by the live-reload watcher.

---

## Themes

Bundled themes (selectable from `sonicterm.toml`, for example `[appearance] theme = "…"`).
The default is `wezterm` for out-of-box visual parity with WezTerm:

- `wezterm` (default)
- `tokyo-night`
- `dracula`
- `nord`
- `catppuccin-mocha`
- `gruvbox-dark-hard`

Custom themes can be dropped as `.toml` into the same directory as
`sonicterm.toml` and referenced by filename (without extension).

---

## Internationalization (i18n)

The SonicTerm UI (menu items and command palette labels) is translated via
[Fluent](https://projectfluent.org/). Three locales ship
today:

- `en` — English
- `zh-CN` — 简体中文
- `ja` — 日本語

Locale selection order:

1. `[i18n] locale = "…"` in `sonicterm.toml`
2. `SONIC_LOCALE` environment variable
3. OS locale (`AppleLanguages` on macOS, `GetUserDefaultUILanguage` on
   Windows)
4. Fallback to `en`

Changing `locale` in `sonicterm.toml` switches locale live without a restart.

---

## Command palette and keymap

`super+shift+p` opens the **command palette**: a fuzzy-filterable list of
every bindable action. Type a few letters, press `Enter`, and the action
runs. This is the easiest way to discover features you haven't memorized
a keybinding for.

The default keymap is **WezTerm-compatible** (`assets/keymaps/sonicterm.toml`).
To override one binding without forking the whole map:

```toml
[[keymap.overrides]]
key = "super+k"
action = "ClearScreen"
```

The full list of action names lives in `sonicterm-core::keymap::Action`; the
command palette shows them in their localized form.

---

## Selection, copy, and opacity

- **Click + drag** to select. **Double-click** selects the word;
  **triple-click** selects the line.
- **Copy:** `super+c` (also auto-copies on selection if
  `[appearance] copy_on_select = true`).
- **Paste:** `super+v`.
- **Window opacity:** set `[appearance] opacity = 0.96` in `sonicterm.toml`.
  Values from `0.5` to `1.0`.

---

## SSH client (optional)

SonicTerm includes an in-process SSH client built on
[`russh`](https://crates.io/crates/russh). It is **feature-gated** — build
with:

```bash
cargo build --release -p sonicterm-mac --features ssh
```

Once enabled, open an SSH pane from the command palette
(`SSH: Connect…`). Auth methods:

- Identity file (`~/.ssh/id_ed25519`, `~/.ssh/id_rsa`)
- `ssh-agent` (via `$SSH_AUTH_SOCK`)

Host and user inputs are validated to reject shell metacharacters, so a
malicious URL or pasted string can't smuggle arguments into the connect
command.

---

## Multiplexer (`sonicterm-mux`)

`sonicterm-mux` is a separate daemon binary that owns persistent PTY sessions.
SonicTerm's GUI can attach as a thin client — close the window, reopen it,
and your shells are still running.

```bash
# 1. Start the daemon (foreground or via launchd / systemd)
cargo run --release -p sonicterm-mux -- --socket /tmp/sonic.sock

# 2. Attach the GUI as a client
SONIC_MUX=/tmp/sonic.sock cargo run --release -p sonicterm-mac
```

Implementation notes:

- The socket is created with `0600` perms so only your UID can connect.
- Subscriber channels are **bounded with drop-oldest** semantics — a slow
  client can't OOM the daemon.

---

## Code signing

Code signing is **DEFERRED past v1.0** — cert procurement (Apple
Developer ID, Azure Trusted Signing) has not happened. Release artifacts
are currently unsigned. On first launch macOS users may need to
right-click → Open, and Windows users may need to accept SmartScreen.

See [`release/signing.md`](release/signing.md) (historical) for the
procedure if/when this work is revived.

---

## Regression net

If you're hacking on SonicTerm and want a one-shot sanity check that nothing
visible has regressed:

```bash
# CJK / emoji / Powerline end-to-end through the real grid
cargo build -p sonicterm-app

cargo build -p sonicterm-mac
```

Then launch the mac app and smoke-check the screen manually.

---

## Troubleshooting

**My font looks wrong / I can't see emoji.**
Check `[font] family = "…"` — if you point at a font missing CJK or emoji,
SonicTerm falls back to the per-OS chain, but a non-existent family will be
ignored entirely. Run with `RUST_LOG=sonicterm_shared=info` to see what got
loaded.

**Cmd+click doesn't open links.**
The clicked text must carry an OSC 8 hyperlink. Plain URLs in output are
not auto-detected (yet). Programs like modern `ls`, `cargo`, `gh`, and
`git` emit OSC 8 by default.

**My shell pollutes every prompt with `\[\e]133;A\a\]…`.**
Your shell isn't recognizing the OSC 133 sequence as non-printing. See
[`shell-integration.md`](shell-integration.md) for the per-shell snippet —
the `\[ \]` brackets matter for bash; zsh uses `%{ %}`.

**I see `task tools haven't been used recently` in the log.**
That's a Claude Code reminder, not a SonicTerm message. Ignore.

**How do I edit settings?**
Open the command palette (`super+shift+p`) and type `Edit sonicterm.toml`.
SonicTerm opens the platform config file in your default `.toml` editor.

---

Last reviewed: v0.7. If something on this page doesn't match what you see,
file an issue with the version string from `sonic --version`.
