# Sonic Terminal — Brand Guide

This document is the **single source of truth** for the Sonic identity:
icon, color palette, typography, usage rules. Anything ambiguous, default
to this file.

## 1. Logo

### 1.1 Concept
A terminal window in profile, racing through cyan light trails. The window
contains the iconic shell prompt `>_`. Speed + craft + the unmistakable
shape of a CLI.

### 1.2 Master files
All masters are SVG, hand-authored, version-controlled, **never raster
edited**. Live in `assets/icons/source/`:

| File | Purpose |
|---|---|
| `sonic.svg` | Full-color app icon (squircle background) — primary mark |
| `sonic-mono.svg` | Monochrome variant (uses `currentColor`) — menu bar, badges, dark/light UI |
| `sonic-glyph.svg` | Color glyph **without** squircle — for use on custom backgrounds |

### 1.3 Generated exports
`assets/icons/bake-icons.sh` produces every consumable format into
`assets/icons/exports/`. Never edit anything in `exports/` by hand —
regenerate from source.

| Output | Use |
|---|---|
| `exports/png/sonic-{16..1024}.png` | UI bitmaps |
| `exports/png/sonic-{16..512}@2x.png` | Retina pairs |
| `exports/png/sonic-mono-{16..64}.png` | Menu bar / docs |
| `exports/sonic.icns` | macOS app bundle (`Sonic.app/Contents/Resources/`) |
| `exports/sonic.ico` | Windows installer + .exe resource |

To regenerate (requires `librsvg` and, for `.ico`, ImageMagick):
```bash
bash assets/icons/bake-icons.sh
```

## 2. Color palette

| Role | Hex | Notes |
|---|---|---|
| **Background — deep** | `#070D1C` | Squircle base, bottom of gradient |
| **Background — mid** | `#0F1B36` | Squircle top of gradient |
| **Accent — cyan light** | `#7FE8FF` | Outline highlight, trail start |
| **Accent — cyan mid** | `#5BD3F8` | Window dots, mid-stroke |
| **Accent — cyan deep** | `#3DB8FE` | Stroke mid-tone |
| **Accent — blue** | `#1E6EE6` | Stroke shadow / trail end |
| **Inner highlight** | `#F4FBFF` | Inner stroke crispening + chevron highlight |
| **Outer rim** | `#FFFFFF @ 8%` | Subtle squircle definition |

> Trails fade from `#7FE8FF` (head, sharp) to `#1E90F0` (tail, dissolving).
> Use the `trail` gradient in the SVG; don't reinvent it.

## 3. Geometry

- **Master canvas**: 1024 × 1024
- **Squircle**: iOS-style superellipse, ~40 % corner radius
- **Safe area**: keep important glyph elements within a centered 880 × 880 box
- **Stroke width**: 14 px @ 1024 (≈ 1.4 % of canvas)
- **Glow**: two-pass Gaussian blur (`stdDeviation` 8 + 18) merged with source

## 4. Sizing rules

| Size | Use | Format |
|---|---|---|
| 16, 32 | Favicon, menu bar | PNG (full-color or mono) |
| 48, 64 | List items, jumplists | PNG |
| 128, 256 | Dock at 1×, Finder previews | PNG / ICNS slot |
| 512, 1024 | App Store, Dock @2x, README hero | PNG / ICNS slot |

Below 16 px the icon stops being legible — fall back to the mono glyph
without the squircle frame.

## 5. Usage rules

**Do**
- Use the full-color icon for app launchers, README, App Store, splash.
- Use the mono variant for menu bars and inline body text.
- Pair the icon with the wordmark **Sonic** in JetBrains Mono Bold when
  used as a header lock-up.

**Don't**
- Re-color the squircle background. The dark navy is part of the identity.
- Add drop shadows on top of the icon — the SVG already carries its glow.
- Stretch / shear non-uniformly.
- Place the icon on a low-contrast background without the squircle.

## 6. Typography

- **Wordmark**: JetBrains Mono Bold, tracking `-2 %`, baseline-aligned with
  the chevron midline.
- **UI text inside the product**: per-user (config).
- **Docs / README headings**: GitHub default system stack.

## 7. Where the icon ships from

| Platform | Path | Built by |
|---|---|---|
| `.app` bundle (macOS) | `Sonic.app/Contents/Resources/sonic.icns` | `packaging/mac/make-dmg.sh` |
| `.msi` (Windows) | `sonic-windows/wix/main.wxs` → `sonic.ico` | `cargo wix` |
| README hero | `assets/icons/exports/png/sonic-256.png` | Hand-linked |
| GitHub social preview | `assets/icons/exports/png/sonic-1024.png` | Uploaded to repo Settings |

## 8. Changelog

| Version | Date | Change |
|---|---|---|
| 2.1 | 2026-05-25 | User-provided final master (`sonic_terminal_icon_checked.svg`) — silver squircle border, radial navy bg, refined chevron + cursor, trimmed speed streaks |
| 2.0 | 2026-05-25 | First terminal-window mark draft |
| 1.0 | 2026-05-24 | Initial placeholder (stylized hedgehog) — superseded |
