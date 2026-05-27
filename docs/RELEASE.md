# Releasing Sonic

Tagging `vX.Y.Z` triggers `.github/workflows/release.yml`, which builds a
macOS universal `.dmg` and a Windows x64 `.msi`, signs them when the
appropriate secrets are configured, and publishes a GitHub Release.

The workflow is a **no-op signer** when secrets are absent — unsigned
artifacts still build and upload, so you can cut pre-release tags before
all the certs are in place.

## Required GitHub Secrets

### macOS — Apple Developer ID + notarytool

| Secret | Value |
|---|---|
| `MACOS_CERT_P12_BASE64` | `base64 -i DeveloperID.p12` of exported cert+key |
| `MACOS_CERT_PASSWORD` | password used during `.p12` export |
| `MACOS_SIGNING_IDENTITY` | e.g. `Developer ID Application: Your Name (TEAMID)` |
| `MACOS_NOTARY_USER` | Apple ID email |
| `MACOS_NOTARY_PASSWORD` | app-specific password from appleid.apple.com |
| `MACOS_NOTARY_TEAM_ID` | 10-char Team ID from developer.apple.com/account |

### Windows — Azure Trusted Signing

We use [Azure Trusted Signing](https://learn.microsoft.com/azure/trusted-signing/)
(≈$10/mo) via [`azure/trusted-signing-action`](https://github.com/Azure/trusted-signing-action)
instead of an EV cert + signtool — far cheaper and CI-friendly on hosted
runners.

| Secret | Value |
|---|---|
| `AZURE_TENANT_ID` | from Azure AD app registration |
| `AZURE_CLIENT_ID` | service principal client ID |
| `AZURE_CLIENT_SECRET` | service principal secret (or use OIDC) |
| `AZURE_TS_ENDPOINT` | e.g. `https://eus.codesigning.azure.net` |
| `AZURE_TS_ACCOUNT` | Trusted Signing account name |
| `AZURE_TS_PROFILE` | certificate profile name |

## How to set the secrets

1. **macOS** (~1 day, mostly waiting for Apple):
   - Enroll at https://developer.apple.com/programs/ ($99/yr).
   - In Xcode → Settings → Accounts → Manage Certificates, create a
     **Developer ID Application** cert. Export from Keychain Access as
     `DeveloperID.p12` with a strong password.
   - Generate an **App-Specific Password** at https://appleid.apple.com.
   - Set the six `MACOS_*` secrets above in
     `Repo → Settings → Secrets and variables → Actions`.

2. **Windows** (~3–5 days; identity validation is the slow part):
   - Azure portal → create a **Trusted Signing Account** (region e.g.
     East US → endpoint `https://eus.codesigning.azure.net`).
   - Create an **Identity Validation** request (individual or
     organization). Wait 1–3 business days for approval.
   - Create a **Certificate Profile** (Public Trust).
   - Azure AD → App registrations → new app → assign the
     `Trusted Signing Certificate Profile Signer` role on the account.
   - Copy Tenant ID / Client ID / Secret + endpoint / account name /
     profile name into the six `AZURE_*` secrets above.

3. **Cut a release**:
   ```bash
   git tag v0.X.0 && git push origin v0.X.0
   ```
   Watch the workflow at
   `https://github.com/D0n9X1n/sonic/actions/workflows/release.yml`.

## Cost summary (USD)

| Item | Cost |
|---|---|
| Apple Developer Program | $99/yr |
| Azure Trusted Signing (Basic) | ~$120/yr |
| Azure identity verification | ~$30 one-time |
| **Total Year 1** | **≈ $250** |
| **Steady state** | **≈ $220/yr** |
