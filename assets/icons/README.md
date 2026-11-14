# Icons

`source/sonic.svg` is the SonicTerm app icon master; edit only it. Everything in
`exports/` is generated from it and committed:

```bash
brew install librsvg imagemagick   # one-time
bash scripts/bake-icons.sh
```

Commit `source/` and `exports/` together. Packaging ships the committed exports as
they are, so an icon change takes effect only once its exports are committed.

What each export holds, which package uses it, and why the squircle fills 92% of
the canvas are on the [Packaging](../../wiki/Packaging.md#app-icon) wiki page.
