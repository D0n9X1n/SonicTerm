# Bundled Fonts

Sonic ships exactly one monospaced font family:

**Rec Mono St.Helens** — the brand default referenced by
`sonicterm_cfg::config::DEFAULT_FONT_FAMILY`. Four variants are committed
directly to this directory:

- `RecMonoSt.Helens-Regular.ttf`
- `RecMonoSt.Helens-Italic.ttf`
- `RecMonoSt.Helens-Bold.ttf`
- `RecMonoSt.Helens-BoldItalic.ttf`

The family-name registered by fontdb is `"Rec Mono St.Helens"` (with the
dot) — that's the exact string the config uses.

## Filename routing (why this matters)

The upstream St.Helens TTFs ship with broken OS/2 metadata:

- every face's `fsSelection` Italic bit is set, so fontdb classifies all
  four variants as Italic;
- the Bold variants report `usWeightClass = 600`, not 700.

A naive `(family, style, weight)` query against fontdb therefore returns
either the wrong face or `None`. WezTerm dodges this by routing by
filename / PostScript name; we do the same in
`sonicterm_text::load_font_data_with_sonic_overrides`, which patches the
`FaceInfo` style+weight at load time using the (correct) PostScript name
as the source of truth. The override path also drops any system-installed
St.Helens copies whose metadata we cannot fix, so a user with the font
installed system-wide doesn't get the broken copy preferred over our
patched bundled one.

Context: https://github.com/D0n9X1n/sonic/issues/419

## Provisioning

Rec Mono St.Helens is committed in-tree (no fetch step needed).

If you need Nerd Font / Powerline PUA coverage, install a Nerd Font
system-wide (e.g. JetBrainsMono Nerd Font, Symbols Nerd Font Mono); the
platform fallback chain in `sonicterm_text::swash_rasterizer` resolves
through it automatically. The previously-bundled JetBrainsMono Nerd Font
TTFs and Rec Mono Casual were dropped in R1 of the rename epic (#419).

## License

Rec Mono St.Helens     — SIL Open Font License 1.1 — built from
                         MOSconfig/recursive-code-config v1.2.2
                         (https://github.com/MOSconfig/recursive-code-config)

The SIL OFL 1.1 permits bundling and redistribution provided the license
text accompanies the font files. Upstream license files are preserved in
the source repository linked above.
