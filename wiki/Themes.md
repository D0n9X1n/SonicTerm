# Themes

[简体中文](Themes-zh-CN)

### Theme files and selection

SonicTerm themes are TOML files. Bundled themes live in `assets/themes/`:

- `catppuccin-mocha`
- `dracula`
- `gruvbox-dark-hard`
- `monokai-pro`
- `nord`
- `one-dark`
- `solarized-dark`
- `tokyo-night`
- `wezterm`

Editable user themes live in:

```text
~/.sonicterm/themes/
```

Select a theme in `~/.sonicterm/sonicterm.toml`:

```toml
theme = "wezterm"
```

A name first checks `~/.sonicterm/themes/<name>.toml`, then bundled assets. A
path-like value is used directly. See [Configuration](Configuration) for config
reload rules.

### Create and apply a theme

Copy the seeded default, edit it, select it, then reload:

```sh
cp ~/.sonicterm/themes/wezterm.toml ~/.sonicterm/themes/my-theme.toml
```

```toml
theme = "my-theme"
```

Run **Reload Config** after saving. SonicTerm re-reads the selected theme file on
every reload, so its name does not need to change.

An `apply_theme` keymap action can change the active theme for the current
session:

```toml
[[binding]]
keys = "super+shift+1"
action = { apply_theme = "nord" }
```

This action does not write `sonicterm.toml`. The next config reload uses the
theme named in that file. See [Keybindings](Keybindings) for binding syntax.

### Schema

A complete theme has this shape:

```toml
name = "My Theme"
appearance = "dark"

[colors]
background = "#141617"
foreground = "#d5c4a1"
cursor = "#fabd2f"
cursor_text = "#141617"
selection_bg = "#3c3836"
selection_fg = "#d5c4a1"

[colors.ansi]
black = "#1d2021"
red = "#fb4934"
green = "#b8bb26"
yellow = "#fabd2f"
blue = "#83a598"
magenta = "#d3869b"
cyan = "#8ec07c"
white = "#d5c4a1"

[colors.bright]
black = "#665c54"
red = "#fb4934"
green = "#b8bb26"
yellow = "#fabd2f"
blue = "#83a598"
magenta = "#d3869b"
cyan = "#8ec07c"
white = "#fbf1c7"

[colors.tab]
bar_bg = "#141617"
active_bg = "#141617"
active_fg = "#fabd2f"
inactive_bg = "#141617"
inactive_fg = "#928374"
hover_bg = "#1c1f20"
hover_fg = "#d5c4a1"
close_button_fg = "#ff5555"
```

`appearance` accepts `light` or `dark`. It is a palette hint used when SonicTerm
derives UI colors such as hyperlink tint strength. Window transparency and
native materials belong to `[appearance]` in `sonicterm.toml`, not to the theme.

Use six-digit RGB strings in `#rrggbb` form. The TOML loader accepts any string,
but a malformed color renders as black. Missing required fields make the theme
fail to parse. `colors.tab.hover_fg` is the only color slot with a schema
default; when omitted, it becomes `#d5c4a1`.

### Color application

| Slot | Current use |
| --- | --- |
| `background` | Terminal background and the base for application chrome |
| `foreground` | Default terminal and application text |
| `cursor` | Cursor, link underline, and link tint source |
| `cursor_text` | Character under a block cursor |
| `selection_bg` | Selection overlay, drawn at 50% alpha |
| `colors.ansi` | ANSI colors 0–7 and derived UI accents |
| `colors.bright` | ANSI colors 8–15 and derived UI accents |
| `colors.tab.bar_bg` | Tab-bar and search-panel background source |
| `colors.tab.active_bg` / `active_fg` | Active tab |
| `colors.tab.inactive_bg` / `inactive_fg` | Inactive tabs and separators |

Search uses `colors.ansi.yellow` for every match and
`colors.bright.green` for the current match. Both use `background` for the text
on top of the highlight.

Keep `selection_fg`, `colors.tab.hover_bg`, `colors.tab.hover_fg`, and
`colors.tab.close_button_fg` in custom files: the schema requires all except
`hover_fg`, although none currently affects rendering.

`accessibility.high_contrast = true` is applied after theme loading. It replaces
only `foreground` with `#ffffff` and `background` with `#000000`; the ANSI,
cursor, selection, and tab slots remain from the theme.

### Reload and failure behavior

A successful reload applies the palette to every window and pane. It also
updates terminal OSC color replies and invalidates text and line caches so the
next frame uses the new colors.

At startup, a missing, unreadable, or malformed selected theme logs a warning
and falls back to bundled `wezterm`. During **Reload Config** or an
`apply_theme` action, a failed theme load logs the error and leaves the current
rendered theme active.
