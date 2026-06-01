# Cutting a Sonic release

The one-page script for shipping a tagged release. Written against
**v0.8.0** but the steps are version-agnostic — substitute the tag.

---

## Pre-flight (≤ 5 minutes)

From a clean checkout of `main`:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run --example pty_dump -p sonicterm-core --release      # must print [e2e] OK
cargo build --release -p sonicterm-mac                        # confirms fat-LTO build
```

Confirm:
- `CHANGELOG.md` has a dated entry for the version (no `[Unreleased]`
  leftovers belonging to the new tag).
- `docs/ROADMAP.md` marks the version as ✅ shipped.
- Workspace test count is ≥ the floor (currently **171**).
- No open `CHANGES REQUESTED` PRs blocking the release.

---

## Cut the tag

```bash
git tag v0.8.0
git push origin v0.8.0
```

That's it. Pushing the tag is the trigger.

---

## What happens after `git push origin v0.8.0`

[`.github/workflows/release.yml`](.github/workflows/release.yml) fires on
the `v*` tag and:

1. **`macos-14` job**
   - Installs `librsvg` + `imagemagick`.
   - Runs `bash assets/icons/bake-icons.sh` so the bundle carries fresh
     `.icns` / PNG bakes.
   - Builds `sonicterm-mac` with the release profile (fat LTO, 1 codegen
     unit, strip, panic=abort).
   - Packages a **universal `Sonic-vX.Y.Z.dmg`**.
   - As of v0.8 (#39): code-signing + notarization runs **only when the
     `APPLE_*` secrets are present**. Until v1.0 these are intentionally
     absent and the DMG ships unsigned.
2. **`windows-latest` job**
   - Builds `sonicterm-windows` release.
   - Packages an **x64 `Sonic-vX.Y.Z.msi`** via WiX.
   - Signtool runs only when `WINDOWS_CERT_*` secrets are present (also
     v1.0 work).
3. **`release` job** publishes a GitHub Release attached to the tag,
   uploads both artifacts, and pastes the matching CHANGELOG section
   into the release body.

Watch progress with:

```bash
gh run list -R D0n9X1n/sonic --workflow release.yml --limit 1
gh run watch -R D0n9X1n/sonic
```

---

## Post-release checklist

Once the Release page is live:

- [ ] Download the `.dmg` on a clean macOS box, open it, drag to
      `/Applications`, launch. Confirm the window paints, the icon is
      the current one, typing reaches the shell, and `super+T` opens a
      new tab.
- [ ] Download the `.msi` on a clean Windows box, install, launch.
      Same smoke test.
- [ ] On both platforms: run the **WezTerm visual parity recipe**
      (see [`docs/VISUAL_PARITY.md`](docs/VISUAL_PARITY.md)) and
      eyeball the screenshot — must still be within 3 ΔE.
- [ ] Idle the app for 60 seconds with no input; CPU should sit at
      ~0% (regression guard for #37).
- [ ] Verify the GitHub Release body contains the CHANGELOG section
      and that the artifact filenames match `Sonic-v0.8.0.dmg` /
      `Sonic-v0.8.0.msi`.
- [ ] Bump `[Unreleased]` in `CHANGELOG.md` with whatever lands next.
- [ ] Update `docs/ROADMAP.md` "Last updated" line.

---

## If the tag job fails

The tag itself is cheap to delete and re-push:

```bash
git push --delete origin v0.8.0
git tag -d v0.8.0
# fix the issue, commit to main, then re-tag
```

Common failures:
- **Icon bake fails** — `librsvg` / `imagemagick` install changed; see
  `assets/icons/bake-icons.sh`.
- **`cargo deny` fails on Ubuntu** — license/advisory update; fix on
  `main` and re-tag.
- **Signing job fails despite no secrets** — the job should be gated on
  `secrets.APPLE_ID != ''` (#39). If it's running unconditionally, that's
  a workflow bug.

---

## Reference

- Changelog: [`CHANGELOG.md`](CHANGELOG.md)
- Roadmap: [`docs/ROADMAP.md`](docs/ROADMAP.md)
- Signing prep (for v1.0): [`docs/release/signing.md`](docs/release/signing.md)
- CI cost notes: [`docs/release/CI-BILLING.md`](docs/release/CI-BILLING.md)
- Test gate detail: [`docs/TESTING.md`](docs/TESTING.md)
