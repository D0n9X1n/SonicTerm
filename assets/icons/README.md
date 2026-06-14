# Icons

Live master + baked exports for the SonicTerm Terminal icon.

## Layout

```
assets/icons/
├── source/                   ← edit this master asset
│   └── sonic.png             full-color squircle app icon
├── exports/                  ← generated, do not edit
│   ├── png/                  multi-size PNGs (1× and @2x)
│   ├── sonic.icns            macOS bundle
│   └── sonic.ico             Windows multi-res
└── bake-icons.sh             regenerator
```

## Design

The master uses an opaque black rounded background with the Sonic art inset to
about **84% of the canvas**. The black ring gives the mark breathing room so it
no longer touches the icon edge, while still keeping the shape large enough for
Windows taskbar / Start-menu slots. If you replace the master, preserve a visible
black background ring and avoid transparent edge-only padding that makes the icon
look undersized on Windows.

## Regenerating

```bash
brew install imagemagick     # one-time, for .ico and preferred resizing
bash assets/icons/bake-icons.sh
```

Outputs are reproducible — the same source master always produces the same
bytes (within renderer anti-aliasing tolerance). If you change a
master asset, re-run `bake-icons.sh` and commit both `source/` and
`exports/` together.

## CI

The release workflow runs `bake-icons.sh` on every tag so the published
`.dmg` / `.msi` always carry the freshest icon, even if a contributor
forgets to re-bake locally.
