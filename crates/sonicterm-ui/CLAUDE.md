# sonicterm-ui

## Purpose
UI widgets and overlays: tabs, tab-bar, pane splits, selection, search,
command palette, IME, cursor, i18n.

## Public surface
- `tabs`, `tabbar_view`, `pane`
- `selection`, `search`
- `command_palette`, `ime`, `cursor`, `i18n`

## Land-mines specific to this crate
None named in §4. Render hot-file rule applies to
`tabbar_view`, `overlays`, `cursor`, `selection`, `search`. Changes need
§13 GUI smoke.

## Test gate (local)
```bash
cargo test -p sonicterm-ui
# Plus §13 GUI smoke if you touched tabbar_view/overlays/cursor/selection/search
```

## Common pitfalls
- IME composition state lost on focus change — `commit_preedit` is
  load-bearing
- Tab-bar drag chip is rendered by `sonicterm-shared::render::drag_chip`,
  NOT here — coordinate model + view crate edits together
- Search overlay z-order: must paint above selection, below command
  palette

## Owning PM(s)
- Primary: either
- Hot-file: yes for tabbar_view + overlays + cursor + selection + search

## Cross-references
- Consumes traits from: `sonicterm-types`, `sonicterm-render-model`
- Consumed by: `sonicterm-shared`, `sonicterm-app`
