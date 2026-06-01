# Releasing SonicTerm

Tagging `vX.Y.Z` triggers `.github/workflows/release.yml`, which builds a
macOS universal `.dmg` and a Windows x64 `.msi` and publishes a GitHub
Release.

> **Signing status: DEFERRED.** Per CLAUDE.md §9, code signing (macOS
> Developer ID notarization, Windows Azure Trusted Signing) is wired in
> spirit but the actual cert procurement is a deferred operational step.
> The v1.0 release ships **unsigned** artifacts. Users may need to
> right-click → Open on macOS or accept SmartScreen on Windows.

## Cut a release

```bash
git tag v0.X.0 && git push origin v0.X.0
```

Watch the workflow at
`https://github.com/D0n9X1n/sonic/actions/workflows/release.yml`.

## Historical signing notes

Earlier revisions of this doc described the Apple Developer ID +
notarization flow and Azure Trusted Signing setup for Windows. Those
instructions have been removed from the active workflow until certs are
actually procured. See `docs/release/signing.md` (marked DEFERRED) for
the historical procedure if/when this work is revived.
