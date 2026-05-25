# Bundled Fonts

Sonic ships with **JetBrainsMono Nerd Font** — JetBrains Mono patched with the
Nerd Fonts icon set — so terminal icons and prompts (Powerline / Starship / Oh My Zsh
themes) work out of the box.

## Provisioning

This directory is empty in source control. The font is fetched manually
(an automated `build.rs` provisioner is on the roadmap but not yet wired);
one-shot:

```bash
curl -L -o JetBrainsMono.zip \
  https://github.com/ryanoasis/nerd-fonts/releases/latest/download/JetBrainsMono.zip
unzip -j JetBrainsMono.zip 'JetBrainsMonoNerdFont-*.ttf' -d assets/fonts/
```

We only ship variants we actually use (Regular, Bold, Italic, BoldItalic).

## License

JetBrains Mono — SIL Open Font License 1.1 — https://www.jetbrains.com/lp/mono/
Nerd Fonts patch    — MIT License                  — https://github.com/ryanoasis/nerd-fonts
