# Bundled Fonts

SonicTerm ships exactly one monospaced font family:

**Rec Mono St.Helens** — the brand default referenced by
`sonicterm_cfg::config::DEFAULT_FONT_FAMILY`. Four variants are committed
directly to this directory:

- `RecMonoSt.Helens-Regular.ttf`
- `RecMonoSt.Helens-Italic.ttf`
- `RecMonoSt.Helens-Bold.ttf`
- `RecMonoSt.Helens-BoldItalic.ttf`

Each file's name table declares the family `"Rec Mono St.Helens"` (with the
dot) — that's the exact string the config uses.

## How SonicTerm finds these faces

The app passes this directory to its font stack as a font directory
(`asset_dir().join("fonts")` in `crates/sonicterm-app/src/app/mod.rs`), so the
faces load without being installed on the system.

`FontDatabase::with_font_dirs` (`crates/sonicterm-font/src/db.rs`) walks each
font directory, parses every file FreeType can read, and indexes each face by
its path and full name. To resolve a request, `FontDatabase::resolve` keeps the
faces whose family, full name, PostScript name, path, or alias matches the
requested family, and `ParsedFont::best_matching_index` picks among them by
stretch, style, and weight.

A face's style starts from FreeType's italic flag; the words `italic`,
`kursiv`, or `oblique` in its full name refine it. Its weight comes from the
OS/2 `usWeightClass`. The committed files declare:

| File | `usWeightClass` | Italic bit |
| --- | --- | --- |
| `RecMonoSt.Helens-Regular.ttf` | 400 | clear |
| `RecMonoSt.Helens-Italic.ttf` | 400 | set |
| `RecMonoSt.Helens-Bold.ttf` | 600 | clear |
| `RecMonoSt.Helens-BoldItalic.ttf` | 600 | set |

Nothing renames or rewrites these values when the faces load.

## Provisioning

Rec Mono St.Helens is committed in-tree (no fetch step needed).

The bundled `RecMonoSt.Helens-*.ttf` files are **Nerd-Font-patched**,
so Powerline + Nerd Font icon coverage works out of the box with no
system install required. Covered codepoint ranges include Powerline
separators (U+E0B0–U+E0BF), the Nerd Font PUA block
(U+E000–U+F8FF), and Material Design icons (U+F0001+). Characters the
bundled faces do not cover, such as CJK and emoji, come from fallback
faces found through the platform's native font discovery.

## License

Rec Mono St.Helens     — SIL Open Font License 1.1 — built from
                         MOSconfig/recursive-code-config v1.2.2
                         (https://github.com/MOSconfig/recursive-code-config)

The SIL OFL 1.1 permits bundling and redistribution provided the license
text accompanies the font files. Upstream license files are preserved in
the source repository linked above.
