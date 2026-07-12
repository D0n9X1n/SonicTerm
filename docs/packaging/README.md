# Packaging

Developer documentation: [Architecture](../ARCHITECTURE.md) · [Modules](../MODULES.md) · [Logging](../LOGGING.md) · **Packaging**

Maintained local packaging guidance:

- [macOS](macos.md) — build and package an architecture-specific DMG.
- [Windows](windows.md) — build the x64 binary and MSI.

Packaging executables live with the other first-party entry points in
`scripts/`. Pushing a `v*` tag runs the release workflow and publishes assets;
local packaging commands only create files under `dist/`.
