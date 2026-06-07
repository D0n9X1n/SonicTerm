# sonicterm-ui

## Purpose
Pure UI state and layout helpers: tabs, tab bar spans, drag chips, command
palette, search overlay, copy/READONLY affordances, selection, IME,
scrollbar, broadcast UI, and shared UI tokens.

## Key files
- `tabs.rs`, `tabbar_view.rs`, `tab_spans.rs`, `tab_title.rs` - tab UI.
- `command_palette.rs`, `command_label.rs`, `cheatsheet.rs` - command UI.
- `search.rs`, `overlays.rs` - search state and overlay layout.
- `selection.rs`, `copy_mode.rs`, `cursor.rs`, `pane.rs` - terminal UI state.
- `ime.rs`, `drag_chip.rs`, `scrollbar.rs`, `broadcast.rs` - interaction UI.
- `i18n.rs`, `ui_tokens.rs` - localized labels and shared constants.

## Local gate
```bash
cargo test -p sonicterm-ui
```

## Guardrails
- Keep this crate renderer-agnostic; it should compute state/layout, not
  issue GPU commands.
- Search remains single-line; IME commit text is accepted, newline input is
  ignored.
- READONLY UI must align with app-level behavior: terminal input blocked,
  search and safe navigation shortcuts allowed.
- Keep localized labels and command labels in sync when adding actions.

## Cross-references
- Consumes: `sonicterm-types`, `sonicterm-cfg`.
- Consumed by: `sonicterm-app`, `sonicterm-render-model`,
  `sonicterm-gpu`.
