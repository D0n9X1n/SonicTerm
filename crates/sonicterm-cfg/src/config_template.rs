//! The commented starter configuration written for a new installation.

use super::*;

/// Starter config written when the user config file does not exist.
pub fn default_config_template() -> String {
    let cfg = Config::default();
    format!(
        r#"# SonicTerm configuration.
# Path: {config_dir}/sonicterm.toml
# Edit this file and use Reload Config from the command palette to apply changes.
#
# Custom themes:
#   - Put named themes in "{config_dir}/themes/<name>.toml" and set theme = "<name>"
#   - Or set theme = "/absolute/path/to/theme.toml"
#
# Custom keymaps:
#   - Put named keymaps in "{config_dir}/keymaps/<name>.toml" and set keymap = "<name>"
#   - Or set keymap = "/absolute/path/to/keymap.toml"

# Active color theme. Bundled themes live in assets/themes/.
theme = "{theme}"

# Active keymap. The default is platform-specific:
#   macOS   -> sonicterm-macos
#   Windows -> sonicterm-windows
#   Linux   -> sonicterm-linux
keymap = "{keymap}"

# UI language. Empty string means auto-detect from the OS.
locale = ""

# Accepted for compatibility and ignored: SonicTerm exits when its last window
# closes, on every platform and whatever this value is.
quit_on_last_window_close = true

# Minimum width of a single tab, in logical pixels, so a short title still
# gets a comfortable tab; crowded tabs shrink below it before the bar
# overflows. Default 240.
tab_min_width = {tab_min_width}

# Maximum width of a single tab, in logical pixels. Each tab sizes to its
# title between tab_min_width and this width and cuts a longer title; when
# the tabs do not fit, the widest tabs shrink first. Default 320.
tab_max_width = {tab_max_width}

[font]
# Font family and metrics. Unset, SonicTerm uses the platform's own monospace face:
# Menlo on macOS, Cascadia Mono (or Consolas) on Windows, fontconfig's monospace on Linux.
# Name another installed monospace family to use it instead.
# family = "{font_family}"
# Font size in logical pixels.
size = {font_size}
# Line-height multiplier. 1.1 is close to WezTerm's default terminal spacing.
line_height = {line_height}
# Post-selection monochrome weight, range 0.5..=5.0; 1.0 preserves native coverage.
# Below 1.0 thins; above 1.0 thickens, including bold, italic, and fallback faces.
# Cell metrics, bitmap dimensions, bearings, and color artwork stay unchanged.
weight_scale = {weight_scale}
# LCD coverage order: "off" keeps grayscale AA; "rgb" and "bgr" select the
# physical stripe order. Effective only on Windows with an opaque final target
# and a capable GPU or the software presenter; every other case uses grayscale.
subpixel_aa = "{subpixel_aa}"

[window]
# Default terminal grid size for NEW windows. These are character cells, not px.
# The first window is roughly:
#   width  = cols * cell_width  + padding_left + padding_right
#   height = rows * cell_height + padding_top  + padding_bottom + tab/title UI
#
# Window layout (terminal content):
#
#   +--------------------------------------------------+
#   | tab/title UI                                     |
#   +--------------------------------------------------+
#   | padding_top                                      |
#   |  +--------------------------------------------+  |
#   |  | terminal grid: cols x rows                 |  |
#   |  |                                            |  |
#   |  +--------------------------------------------+  |
#   | padding_bottom                                   |
#   +--------------------------------------------------+
#      ^ padding_left                 padding_right ^
cols = {cols}
rows = {rows}

# Padding around the terminal text area, in physical pixels. This affects the
# distance between terminal content and the window/pane edge.
padding_left = {padding_left}
padding_right = {padding_right}
padding_top = {padding_top}
padding_bottom = {padding_bottom}

# Native OS window decorations. Changing this usually requires restart.
decorations = true

# Legacy values accepted while reading old configs. Current startup/reload
# behavior uses [appearance].opacity and [appearance].backdrop instead.
opacity = 1.0
blur = false

# Hidden pre-created child windows kept warm for instant tab tear-out.
# 0 disables; hardware honors 1..=5; software adapters cap nonzero values at 1.
warm_window_pool = 1

[terminal]
# Shell to spawn. Leave commented to auto-detect (Windows: PowerShell 7
# `pwsh.exe`, including the Microsoft Store install, falling back to Windows
# PowerShell then cmd.exe; macOS/Linux: $SHELL). Set an explicit path/name to
# override, e.g. shell = "pwsh.exe" or shell = "powershell.exe".
# shell = "pwsh.exe"

# TERM_PROGRAM passed to child PTYs. Some tools, such as Copilot, do not
# recognize SonicTerm yet; setting term_program = "WezTerm" can bypass their
# terminal checks and enable their WezTerm/new terminal UI path.
term_program = "{term_program}"

# Scrollback line limit per pane.
scrollback = {scrollback}

# auto preserves application-keypad mappings; numeric makes operators/Enter use
# ordinary text/Return. Numeric digits and negotiated Kitty input are unchanged.
keypad_mode = "auto"

# Cmd/Ctrl-click safe local files and directories after asynchronous validation.
clickable_local_targets = true
# Also resolve whole bare tokens (such as `ls` names) against trusted pane CWD.
clickable_bare_names = true
# Longest detected path in characters (1-1024). The shortest existing match wins.
clickable_path_max_chars = {clickable_path_max_chars}

# Cursor behavior. Shape: "block", "bar", or "underline".
cursor_blink = false
cursor_shape = "block"

[logging]
# Logs are written to {config_dir}/logs/sonicterm.log.
# Levels: error, warn, info, debug. Debug includes performance/render timing.
# SonicTerm automatically removes logs and crash dumps older than 2 days.
level = "warn"
max_file_size_mb = 10
max_rotated_files = 3
max_age_days = 2
max_crash_dumps = 10
max_crash_age_days = 2
max_crash_bytes = 10485760
max_breadcrumb_files = 10
max_breadcrumb_age_days = 2
max_breadcrumb_bytes = 1048576

[appearance]
# Window/backdrop/compositor appearance. This is separate from [window] padding:
# - [window].padding_* controls terminal text margins.
# - panel_padding controls the inside padding of pop-up panels (command palette).
#
# Panel layout (for command palette):
#
#   +---------------- floating panel ----------------+
#   | panel_padding                                  |
#   |  +------------------------------------------+  |
#   |  | panel content: search box, rows, hints   |  |
#   |  +------------------------------------------+  |
#   | panel_padding                                  |
#   +------------------------------------------------+
backdrop = "opaque"
opacity = 1.0
scrollbar = "auto"
panel_padding = {panel_padding}
# No-GPU handling. When wgpu falls back to a CPU rasterizer (WARP / llvmpipe,
# common over RDP / in VMs), every frame is drawn on the CPU. "auto" detects
# this and lowers the frame cap + disables per-frame fade animation to stay
# usable; "force" always degrades; "off" never does.
software_render_mode = "auto"

[render]
# Reserved compatibility key; the current renderer does not read it.
alt_screen_bg_fill = "v2"

[accessibility]
# Presentation preferences.
high_contrast = false
reduced_motion = false
strong_focus = false

[notifications]
# Desktop notification for long-running commands.
long_command = false
threshold_secs = 10

[status_line]
# A strip with SonicTerm's own state: memory, modes, scroll position,
# the link under the pointer, and problems.
enabled = false
# Window edge: "top" (above the panes) or "bottom" (above the tab bar).
position = "top"
# Text size as a share of the terminal font size, 0.3-1.0.
font_scale = 0.8
# Presets: "minimal", "developer", or "diagnostics".
preset = "developer"
# Symbols: "unicode", "ascii", or "nerd".
icons = "unicode"
# Hide sensitive segments such as the link under the pointer (screen sharing).
hide_sensitive = false
# Sampling interval of sampled segments such as memory, 500-60000 ms.
update_interval_ms = 2000
"#,
        config_dir = default_config_dir()
            .map(|dir| dir.display().to_string())
            .unwrap_or_else(|| "<config-dir>".to_string()),
        theme = cfg.theme,
        keymap = cfg.keymap,
        font_family = cfg.font.family,
        font_size = cfg.font.size,
        line_height = cfg.font.line_height,
        weight_scale = cfg.font.weight_scale,
        subpixel_aa = cfg.font.subpixel_aa.as_str(),
        cols = cfg.window.cols,
        rows = cfg.window.rows,
        padding_left = cfg.window.padding_left,
        padding_right = cfg.window.padding_right,
        padding_top = cfg.window.padding_top,
        padding_bottom = cfg.window.padding_bottom,
        panel_padding = cfg.appearance.panel_padding,
        term_program = cfg.terminal.term_program,
        scrollback = cfg.terminal.scrollback,
        clickable_path_max_chars = cfg.terminal.clickable_path_max_chars,
        tab_min_width = cfg.tab_min_width,
        tab_max_width = cfg.tab_max_width,
    )
}

#[cfg(test)]
#[path = "config_template_tests.rs"]
mod config_template_tests;
