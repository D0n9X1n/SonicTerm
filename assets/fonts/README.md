# Bundled Fonts

Sonic ships with two monospaced font families:

1. **Rec Mono St.Helens** — the brand default referenced by
   `sonic_cfg::config::DEFAULT_FONT_FAMILY`. Four variants are committed
   directly to this directory:
   - `RecMonoSt.Helens-Regular.ttf`
   - `RecMonoSt.Helens-Italic.ttf`
   - `RecMonoSt.Helens-Bold.ttf`
   - `RecMonoSt.Helens-BoldItalic.ttf`

   The family-name registered by fontdb is `"Rec Mono St.Helens"` (with the
   dot) — that's the exact string the config uses.

2. **JetBrainsMono Nerd Font** — JetBrains Mono patched with the Nerd Fonts
   icon set — so terminal icons and prompts (Powerline / Starship / Oh My
   Zsh themes) work out of the box. Serves as the implicit fallback when
   the user's configured family is missing.

## Provisioning

Rec Mono St.Helens is committed in-tree (no fetch step needed).

JetBrainsMono is fetched manually (an automated `build.rs` provisioner is
on the roadmap but not yet wired); one-shot:

```bash
curl -L -o JetBrainsMono.zip \
  https://github.com/ryanoasis/nerd-fonts/releases/latest/download/JetBrainsMono.zip
unzip -j JetBrainsMono.zip 'JetBrainsMonoNerdFont-*.ttf' -d assets/fonts/
```

We only ship variants we actually use (Regular, Bold, Italic, BoldItalic).

## License

Rec Mono St.Helens     — SIL Open Font License 1.1 — built from
                         MOSconfig/recursive-code-config v1.2.2
                         (https://github.com/MOSconfig/recursive-code-config)
JetBrains Mono         — SIL Open Font License 1.1 — https://www.jetbrains.com/lp/mono/
Nerd Fonts patch       — MIT License                — https://github.com/ryanoasis/nerd-fonts

The SIL OFL 1.1 permits bundling and redistribution provided the license
text accompanies the font files. Upstream license files are preserved in
the source repositories linked above.
