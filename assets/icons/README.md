# Icons

Live SVG masters + baked exports for the Sonic Terminal icon.

## Layout

```
assets/icons/
├── source/                   ← edit these (SVG, hand-authored)
│   ├── sonic.svg             full-color squircle app icon
│   ├── sonic-mono.svg        monochrome glyph (currentColor)
│   └── sonic-glyph.svg       color glyph without squircle
├── exports/                  ← generated, do not edit
│   ├── png/                  multi-size PNGs (1× and @2x)
│   ├── sonic.icns            macOS bundle
│   └── sonic.ico             Windows multi-res
└── bake-icons.sh             regenerator
```

## Regenerating

```bash
brew install librsvg imagemagick     # one-time
bash assets/icons/bake-icons.sh
```

Outputs are reproducible — the same source SVG always produces the same
bytes (within rsvg-convert's anti-aliasing tolerance). If you change a
master SVG, re-run `bake-icons.sh` and commit both `source/` and
`exports/` together.

## CI

The release workflow runs `bake-icons.sh` on every tag so the published
`.dmg` / `.msi` always carry the freshest icon, even if a contributor
forgets to re-bake locally.
