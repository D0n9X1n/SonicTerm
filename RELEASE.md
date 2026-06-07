# SonicTerm 0.9.0 Release Checklist

1. Ensure `Cargo.toml` workspace version is `0.9.0`.
2. Run local checks:

   ```sh
   cargo test --workspace --lib --bins
   cargo build --release -p sonicterm-mac
   bash scripts/test-release-notes.sh
   ```

3. Create and push a version tag:

   ```sh
   git tag v0.9.0
   git push origin v0.9.0
   ```

4. Watch the release workflow:

   ```sh
   gh run list -R D0n9X1n/SonicTerm --workflow release.yml --limit 1
   gh run watch -R D0n9X1n/SonicTerm
   ```

5. Confirm the GitHub Release contains:
   - macOS universal `.dmg`
   - Windows `.msi`
   - `SHA256SUMS.txt`
   - generated release notes summarizing commits since the previous tag

Installers are unsigned for 0.9.0.
