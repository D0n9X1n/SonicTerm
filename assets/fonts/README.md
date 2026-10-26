# Bundled Fonts

SonicTerm bundles no text face. The terminal and its chrome draw with the
platform's own monospace family unless `[font] family` names another one:

| Platform | Default family |
| --- | --- |
| macOS | Menlo |
| Windows | Cascadia Mono, then Consolas when Cascadia Mono is not installed |
| Linux | the face fontconfig resolves for `monospace` |

The one bundled font is an icon font:

- `SymbolsNerdFontMono-Regular.ttf` — family `"Symbols Nerd Font Mono"`, from
  [Nerd Fonts](https://github.com/ryanoasis/nerd-fonts) v3.5.1
  (`NerdFontsSymbolsOnly`), SHA-256
  `fe471e538392f51910faab985fa8e192a39dd3426125edd15b71b3680df0e749`.
- `LICENSE-SymbolsNerdFont.txt` — the MIT license that release ships with it.

## How SonicTerm finds this font

The app passes this directory to every font stack as a font directory
(`asset_dir().join("fonts")` in `crates/sonicterm-app/src/app/session.rs`), so
the font loads without being installed on the system. On macOS the bundle's
`ATSApplicationFontsPath` also registers it with CoreText.

Every style's fallback chain is the configured family, then the platform
default families above, then Noto Color Emoji and Symbols Nerd Font Mono, and
only then system fallback. Powerline separators (U+E0A0–U+E0D7), the Nerd Font
private-use icons (U+E000–U+F8FF), and Material Design icons (U+F0001 and up)
therefore resolve from this file even when the text face lacks them. A copy
of the same family in this directory wins over an installed copy, because
configured font directories are searched before native discovery.

## License

Symbols Nerd Font Mono — MIT License (see `LICENSE-SymbolsNerdFont.txt`).
The icon sets the font aggregates keep their upstream licenses, which the
Nerd Fonts repository lists.

Tests that need a fixed, host-independent text face use a subset of Rec Mono
St.Helens under `crates/sonicterm-font/test-fonts/rec-mono/`. That fixture is
never packaged.
