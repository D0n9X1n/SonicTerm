# Visual parity — SonicTerm vs Windows Terminal

Target: SonicTerm should render the same visual output as Windows Terminal for the same shell + font + DPI combination. WT is the reference because it's the default Windows terminal experience and is what users compare against.

## Reference setup
- Font: `Rec Mono St.Helens` (bundled Nerd-patched in SonicTerm; available system-wide via the same TTF)
- Size: 14pt (SonicTerm default) — 11.25pt + cellHeight=20px (WT default profile)
- DPI: any (test 100%, 125%, 150%, 175%)
- Shell: PowerShell with oh-my-posh prompt + Claude Code TUI

## Parity matrix (#461 tracking)

| Category | Codepoint(s) | WT renders | SonicTerm renders | Status |
|---|---|---|---|---|
| Block elements (Claude logo) | U+2580–U+259F | ✅ proper sub-cell rects | ✅ since PR #463 | OK |
| Powerline chevrons | U+E0B0–U+E0BF | ✅ butt-up edge-to-edge | ⚠️ 1-device-px gap at fractional DPI | #470 open |
| NF icons | U+E000–U+F8FF | ✅ fills cell | ✅ since PR #468 | OK |
| Plane-1 NF (Material) | U+F0000–U+FFFFD | ✅ fills cell | ✅ since PR #468 | OK |
| Black filled arrows | U+25B6–U+25C1 | ✅ filled triangles | ✅ classified IconCellFit | OK |
| **Black MEDIUM arrows** | **U+23F5–U+23F8** | ✅ filled triangles | ❌ tofu (`[]`) — resolve_slot=None | PR-B2c TODO |
| CJK / emoji | various | ✅ via fallback chain | ✅ via PLATFORM_FALLBACK_CHAIN | OK |
| Box drawing | U+2500–U+257F | ✅ proper | ✅ via Natural placement | OK |

## Confirmed parity
- Claude logo block elements (after PR #463)
- All NF PUA icons render at full cell width (after PR #468)
- Tab title NF glyphs render correctly
- Powerline chevrons render correctly at integer DPI scales (1.0, 2.0)

## Known gaps
1. **U+23F5 family tofu** — Claude Code uses these for bypass-mode arrows; WT renders them, SonicTerm shows `[]`. Bundled St.Helens cmap byte-pair search found U+23F5 present, but our `resolve_slot` returns None. Investigation in PR-B2c.
2. **Powerline 1-device-pixel gap** at fractional DPI (1.25/1.5/1.75) — issue #470. WT uses fixed integer cell pitch; SonicTerm uses fractional logical cell_w + per-cell device-pixel snap that doesn't guarantee adjacent-cell butt-up.
3. **LCD subpixel AA** — deferred. WT uses ClearType; SonicTerm uses grayscale alpha. Tracked in #388 (closed deferred).

## How to verify parity
See `docs/WINDOWS_TESTING.md` for the full PM recipe. Quick version: open SonicTerm + WT side-by-side at the same DPI, run the same shell session, compare screenshots cell-by-cell. The instrumented build (`RUST_LOG=sonic::render::glyph=debug`) captures per-glyph emit data so divergences show up in the log as `classify` + `resolve_slot` + `final_rect` mismatches against expected behavior.
