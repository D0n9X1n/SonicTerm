---
name: sonic-prefs
description: Read, edit, validate, and hot-reload the user's SonicTerm config (sonicterm.toml). Use this whenever the user asks to change SonicTerm's theme, font, keymap, padding, opacity, or any preference.
---

# sonic-prefs — edit your SonicTerm config

Use this skill to read or edit `~/.snoicterm/sonicterm.toml` safely.

## Config file location

- All platforms: `~/.snoicterm/sonicterm.toml`
- Logs: `~/.snoicterm/logs/sonicterm.log`

SonicTerm's `config_watch` (in `crates/sonicterm-app/src/config_watch.rs`) re-reads the file on save, so changes take effect within ~200ms with no restart — **except** for window size/decorations/backdrop and already-spawned shell changes.

## Two interaction modes

### One-line mode — `sonic-prefs set <field> <value>`

For experienced users who know the field. No confirmation, just a diff + write + reload signal.

```
/sonic-prefs set theme dracula
/sonic-prefs set font.size 15.5
/sonic-prefs set window.opacity 0.92
/sonic-prefs set quit_on_last_window_close true
/sonic-prefs get font.family
/sonic-prefs list themes
/sonic-prefs list keymaps
/sonic-prefs reset font.size            # remove override, fall back to default
/sonic-prefs edit                       # open raw toml in $EDITOR
```

### Interactive mode (default when called without args, or with `/sonic-prefs`)

1. Read current `sonicterm.toml` and show a summary of the common knobs.
2. Ask the user what to change (use AskUserQuestion with the **Common fields** below).
3. Show a unified diff of the proposed change.
4. On confirm, write atomically (`tmp + rename`) and confirm hot-reload by tailing `~/.snoicterm/logs/sonicterm.log*` for the `config_watch: reloaded` line.
5. Offer "advanced — open raw toml" as an escape hatch.

## Common fields (the curated set)

These cover ~95% of user requests. Field path uses TOML dot notation.

| Field | Type | Example | Notes |
|---|---|---|---|
| `theme` | string | `"dracula"` | Must be a name from `list themes`. |
| `keymap` | string | `"sonicterm-macos"` | From `list keymaps`. |
| `font.family` | string | `"Rec Mono St.Helens"` | Bundled default. |
| `font.size` | float | `13.0` | Logical px. |
| `font.line_height` | float | `1.3` | |
| `window.cols` | u16 | `120` | Restart required. |
| `window.rows` | u16 | `36` | Restart required. |
| `window.opacity` | float | `1.0` | 0.0–1.0. |
| `window.blur` | bool | `false` | macOS only. |
| `window.backdrop` | enum | `"opaque"` | `opaque` / `translucent` / `vibrant`. |
| `window.decorations` | bool | `true` | Hide titlebar. |
| `window.padding_left/right/top/bottom` | float | `4.0` | Per-edge px. |
| `terminal.shell` | string? | `"/opt/homebrew/bin/fish"` | `null` = system default. |
| `terminal.scrollback` | usize | `10000` | Max lines. |
| `terminal.cursor_blink` | bool | `true` | |
| `terminal.cursor_shape` | enum | `"block"` | `block` / `bar` / `underline`. |
| `appearance.tab_bar_position` | enum | `"bottom"` | `top` / `bottom`. |
| `appearance.opacity` | float | `1.0` | UI chrome alpha. |
| `tab_close_button_color` | string? | `"#ff8888"` | Hex; `null` = theme default. |
| `quit_on_last_window_close` | bool | `true` | Cmd-W on last tab quits. |
| `accessibility.high_contrast` | bool | `false` | |
| `accessibility.reduced_motion` | bool | `false` | Disables animations. |
| `notifications.long_command` | bool | `true` | |
| `notifications.threshold_secs` | u64 | `5` | |
| `locale` | string | `"en"` | |

The full Config struct lives at `crates/sonicterm-cfg/src/config.rs` — anything declared there is a valid field. The skill maps unknown fields to "advanced mode" rather than rejecting.

## Validation before write

1. **Schema check** — parse the proposed new file with `toml::from_str::<Config>` semantics.
2. **Theme exists** — if `theme` changed, confirm `assets/themes/<value>.toml` (or `~/.snoicterm/themes/<value>.toml`) is on disk.
3. **Keymap exists** — same for `assets/keymaps/<value>.toml`.
4. **Font exists** — `fc-list | grep -i "<family>"` on mac/linux, `(New-Object System.Drawing.Text.InstalledFontCollection).Families` on Windows. Warn (don't block) if missing — the renderer will fall back.
5. **Range checks** — opacity ∈ [0,1], size > 0, cols/rows ≥ 20×5.

Refuse on schema error. Warn on missing-asset. Always show a diff before writing.

## Write protocol (atomic)

```bash
TARGET="$HOME/.snoicterm/sonicterm.toml"
TMP="${TARGET}.tmp.$$"
cp "$TARGET" "${TARGET}.bak"                 # one rolling backup
# write new content to $TMP via the same writer (not echo — preserve comments where possible)
mv "$TMP" "$TARGET"                          # atomic rename on same filesystem
```

Preserve unknown / future keys by parsing into `Config` + the `extra: toml::Table` catch-all (already in `crates/sonicterm-cfg/src/config.rs`) and round-tripping.

## Hot-reload confirmation

After write, tail the log for 2 seconds:

```bash
tail -n 0 -F "$HOME/.snoicterm/logs/"sonicterm.log* &
TAIL=$!
sleep 2
kill $TAIL 2>/dev/null
```

Look for `config_watch: reloaded` (or `config_watch: parse error`). If neither, the user may have Sonic closed — that's fine, the change is on disk for next launch. Tell them.

## Hot fields vs cold fields

Live-reload OK:
- `theme`, `keymap`, font (rebuilds atlas), opacity, padding, cursor, scrollback, notifications, accessibility, `tab_close_button_color`, `quit_on_last_window_close`, tab_bar_position.

Restart required:
- `window.cols`, `window.rows`, `window.decorations`, `window.backdrop`, `terminal.shell` (for already-spawned panes).

Tell the user clearly when a restart is needed.

## Examples

```
User: 把主题改成 dracula
→ /sonic-prefs set theme dracula
→ (diff)
   - theme = "tokyo-night"
   + theme = "dracula"
→ confirm → write → tail log → "config_watch: reloaded ✓ (hot)"

User: 我要个透明窗口
→ AskUserQuestion: opacity? (1.0 / 0.95 / 0.90 / 0.80)
→ confirm → write window.opacity = 0.90 → reloaded ✓

User: cmd+w 应该退出最后一个 tab
→ /sonic-prefs set quit_on_last_window_close true
→ already true (default since v0.8.1) — no change. Show current value, done.

User: 我要直接编辑
→ /sonic-prefs edit → opens $EDITOR on a copy → on save, validate → atomic write.
```

## What this skill does NOT do

- **Theme authoring.** To create a new theme, copy `assets/themes/dracula.toml` to `~/.snoicterm/themes/my-theme.toml` and edit. Then `/sonic-prefs set theme my-theme`.
- **Keymap authoring.** Same pattern under `keymaps/`.
- **Restart Sonic.** Tell the user; don't `pkill` their session.
- **Touch git.** This is user-local state.
