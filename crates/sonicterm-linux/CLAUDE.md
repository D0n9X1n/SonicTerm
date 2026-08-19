# sonicterm-linux

## Purpose
Linux shipping binary. It loads config, packaged themes/keymaps/fonts, logging,
session state, and breadcrumbs, then runs `sonicterm_app::LinuxShell` on X11 or
Wayland.

## Key files
- `src/main.rs` - startup, Linux capability normalization, and clean shutdown.
- `resources/` - desktop entry and AppStream package metadata.

## Local gate
```bash
cargo test -p sonicterm-linux
cargo clippy -p sonicterm-linux --all-targets -- -D warnings
```

## Guardrails
- The shipping binary name is `sonicterm`.
- Keep X11 WM class, Wayland app ID, desktop entry, and AppStream ID equal to
  `com.d0n9x1n.SonicTerm`.
- Linux package assets resolve through `sonicterm_cfg::assets::asset_dir`; do not
  add a binary-local lookup order.
- Unsupported native menu, notification, backdrop, and cross-process tab-drag
  integrations must degrade without blocking the event loop.

## Cross-references
- Consumes: `sonicterm-app-core`, `sonicterm-app`, `sonicterm-cfg`,
  `sonicterm-engine`, `sonicterm-logging`.
- Consumed by: Linux package and release workflows.
