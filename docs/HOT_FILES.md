# Hot files — 2-PM sign-off required

This list complements `.github/CODEOWNERS`. A "hot file" is one where
cross-platform regressions hide easily (rendering, input, VT, threading,
or the keymap action enum). PRs touching any file below MUST be reviewed
by BOTH the mac-PM and the win-PM before merge, and MUST include the
§13 GUI smoke result from both platforms.

| File | Reason | Landmine(s) |
|---|---|---|
| `crates/sonicterm-app/src/app/window_event.rs` | Main render loop. AB-BA deadlock if locking discipline slips; bursty-input regressions silent. | LM-001, LM-003, LM-004 |
| `crates/sonicterm-app/src/app/spawn_pane.rs` | PTY-thread redraw coalescer (3 ms + 128 KB). 100% idle CPU regression hides here. | LM-002, LM-003 |
| `crates/sonicterm-app/src/app/event_loop.rs` | `ControlFlow::WaitUntil` vsync pacing. Busy-loop regressions silent until users report fans. | LM-002 |
| `crates/sonicterm-app/src/app/keymap_dispatch.rs` | Every bindable user `Action` arms here. Missing arm = silent no-op. | — |
| `crates/sonicterm-vt/src/vt.rs` | The parser. Whole-prompt-erase class of bugs lives here. | LM-005, LM-006 |
| `crates/sonicterm-grid/src/grid.rs` | Shared mutable state between parser and renderer. Off-by-one cell-buffer bugs are silent until paint. | — |
| `crates/sonicterm-cfg/src/keymap.rs` | Public `Action` enum. Adding a variant without the dispatcher arm fails silently. | — |
| `crates/sonicterm-cfg/src/url_open.rs` | URL validation — security boundary for OSC 8 + cmd.exe re-tokenization. | LM-008 |
| `crates/sonicterm-io/src/pty.rs` | PTY backend + `Drop` kill discipline. Orphan-shell regression class. | LM-007 |
| `crates/sonicterm-shared/src/render/core.rs` | Per-cell bg + rich-text path. Dropped-bg regression (#163) shipped past local gate from here. | — |
| `crates/sonicterm-gpu/src/text_pipeline.rs` | glyphon wiring. wgpu/glyphon/cosmic-text coherence + dropped-bg class. | — |
| `crates/sonicterm-text/src/glyph_atlas.rs` | Texture-page allocation. Atlas-eviction races + HiDPI blur class. | — |
| `crates/sonicterm-text/src/swash_rasterizer.rs` | Primary-only-vs-fallback path that shipped PR #42 CJK tofu. | — |

Adding a file to the hot list requires:
1. A linked PR or issue showing the class of regression that motivated it.
2. A landmine entry (in `landmines.toml`) if a deterministic test can guard it.
3. An entry in `.github/CODEOWNERS` routing it to both PMs.

Removing a file requires:
1. A 90-day window with zero PRs that needed the 2-PM review *for* the
   regression class the entry called out.
2. Sign-off from both PMs in a PR body.
