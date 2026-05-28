# Code Signing & Notarization — DEFERRED

> **Status: DEFERRED past v1.0.** Cert procurement (Apple Developer ID,
> Azure Trusted Signing) has not happened. The active release pipeline
> ships UNSIGNED artifacts (see `docs/RELEASE.md`). The text below is
> retained as a historical reference for whoever revives this work; it
> does NOT reflect the current workflow.

This document describes how to flip on signing for Sonic's release pipeline once
the appropriate certificates are in hand. The release workflow
(`.github/workflows/release.yml`) is already wired to detect signing secrets
and produce signed + notarized artifacts when they exist; if any secret is
absent the corresponding job logs an "unsigned build" notice and continues.

## macOS

Apple Developer Program enrollment is required (`$99/yr`). You will need a
**"Developer ID Application"** certificate (for distribution outside the App
Store) and an app-specific password for `notarytool`.

### Required GitHub Actions secrets

| Secret | Value |
|---|---|
| `MACOS_CERT_P12_BASE64` | Base64 of the `.p12` export of the Developer ID Application certificate (private key included). |
| `MACOS_CERT_PASSWORD` | The password used when exporting the `.p12`. |
| `MACOS_SIGNING_IDENTITY` | The full identity string, e.g. `Developer ID Application: Your Name (TEAMID12345)`. |
| `MACOS_NOTARY_USER` | Apple ID email used for notarization. |
| `MACOS_NOTARY_PASSWORD` | App-specific password generated at <https://appleid.apple.com>. |
| `MACOS_NOTARY_TEAM_ID` | 10-character team ID (e.g. `TEAMID12345`). |

### Generating the secrets

1. In **Keychain Access**, export your "Developer ID Application: …"
   certificate (with its private key) as a `.p12`. Pick a strong password.
2. Base64-encode it:
   ```bash
   base64 -i DeveloperID.p12 | pbcopy
   ```
   Paste into `MACOS_CERT_P12_BASE64`.
3. Look up your signing identity:
   ```bash
   security find-identity -v -p codesigning
   ```
   The full quoted string after the hash is `MACOS_SIGNING_IDENTITY`.
4. Look up your team ID at <https://developer.apple.com/account> → Membership.
5. Generate an app-specific password at <https://appleid.apple.com> →
   Sign-In and Security → App-Specific Passwords.

### What the workflow does

When `MACOS_CERT_P12_BASE64` is set:
- `apple-actions/import-codesign-certs@v3` imports the cert into a temporary
  keychain on the runner.
- `packaging/mac/make-dmg.sh` sees `MACOS_SIGNING_IDENTITY` in its env and runs
  `codesign --deep --force --options runtime --timestamp --sign "$IDENTITY"`
  on `Sonic.app` before wrapping it in the DMG.

When `MACOS_NOTARY_USER` is also set:
- After the DMG is built, the workflow runs
  `xcrun notarytool submit <dmg> --apple-id … --password … --team-id … --wait`
  and on success staples the ticket with `xcrun stapler staple`.

### Verifying a signed build locally

```bash
# Should print "Developer ID Application: …" and "satisfies its Designated Requirement"
codesign -dv --verbose=4 /Applications/Sonic.app
spctl --assess --type execute --verbose /Applications/Sonic.app    # accepted
xcrun stapler validate Sonic-<version>-mac-universal.dmg            # validated
```

## Windows

Authenticode signing requires a code-signing certificate from a trusted CA
(Sectigo, DigiCert, SSL.com, etc.). An EV cert (`$200-400/yr`) gives instant
SmartScreen reputation; an OV cert is cheaper but takes a few thousand
installs to build reputation.

### Required GitHub Actions secrets

| Secret | Value |
|---|---|
| `WINDOWS_CERT_PFX_BASE64` | Base64 of the `.pfx` (PKCS#12) export of the code-signing certificate (private key included). |
| `WINDOWS_CERT_PASSWORD` | The password used when exporting the `.pfx`. |

For EV certs that live on a hardware token (typical for Sectigo EV), you'll
need a cloud signing service (e.g. Azure Key Vault + `AzureSignTool`) instead
of a `.pfx` upload — open a follow-up issue when that day comes.

### Generating the secrets

1. Export your code-signing cert + private key from the Windows certificate
   store as a `.pfx` (or convert PEM with `openssl pkcs12 -export`).
2. Base64-encode it:
   ```powershell
   [Convert]::ToBase64String([IO.File]::ReadAllBytes("sonic-signing.pfx")) | Set-Clipboard
   ```
   Paste into `WINDOWS_CERT_PFX_BASE64`.

### What the workflow does

When `WINDOWS_CERT_PFX_BASE64` is set:
- The `.pfx` is decoded to a temp file on the runner.
- `signtool.exe` (auto-detected under the Windows 10 SDK) signs every
  `dist/*.msi` with SHA-256, RFC 3161 timestamping via
  `http://timestamp.sectigo.com`, and the description `Sonic Terminal`.
- `signtool verify /pa /v` confirms the signature chain.
- The `.pfx` is deleted.

### Verifying a signed build locally

```powershell
# /pa = use default Authenticode policy; /v = verbose
signtool verify /pa /v Sonic-<version>.msi

# Or via PowerShell
Get-AuthenticodeSignature .\Sonic-<version>.msi
# Status should be "Valid", SignerCertificate should match your cert
```

## Switching the lights on

1. Add the secrets above in **Settings → Secrets and variables → Actions**.
2. Cut a tagged release (`git tag v1.0.0 && git push origin v1.0.0`).
3. Verify the artifacts as above. If anything trips, the workflow logs each
   step verbatim (look for `codesign`, `notarytool`, `signtool` output).
