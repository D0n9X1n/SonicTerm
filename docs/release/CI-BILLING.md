# CI Billing Block — Diagnosis & Fix Path

**Status (2026-05-26):** All GitHub Actions runs on `D0n9X1n/sonic` are failing
because of an **account billing block**, not because of code regressions. Every
CI run since the block went into effect shows zero step executions and the
annotation:

> The job was not started because recent account payments have failed or your
> spending limit needs to be increased. Please check the 'Billing & plans'
> section in your settings.

This document records how to confirm the diagnosis, how to unblock CI, and the
expected cost of the one-shot `release.yml` run for v0.8.0.

---

## 1. Confirming it's billing (not code)

Run:

```bash
gh run list -R D0n9X1n/sonic --limit 10 \
  --json conclusion,name,databaseId,status,event
gh run view <id> -R D0n9X1n/sonic
gh api repos/D0n9X1n/sonic/actions/runs/<id>/jobs \
  --jq '.jobs[] | {name, conclusion, started_at, completed_at, steps: [.steps[]?.name]}'
gh api repos/D0n9X1n/sonic/actions/permissions
```

You're looking at a billing block (not a code failure) when **all of these are
true**:

1. `gh run view` annotations say "job was not started because recent account
   payments have failed or your spending limit needs to be increased".
2. Every job's `steps` array is empty (`[]`) — the runner never picked the job
   up, so checkout/build/test never ran.
3. `started_at` and `completed_at` are within ~3–5 seconds of each other on
   every job — that's the scheduler giving up, not a real build failing.
4. `gh api .../actions/permissions` returns `"enabled": true` — Actions are
   permitted at the repo level, so the block is purely upstream (account
   billing), not a per-repo toggle.

All four conditions are currently true for this repo. Latest verified run:
[`26441736737`](https://github.com/D0n9X1n/sonic/actions/runs/26441736737).

If any of the four are false, the failure is something else (real code break,
disabled Actions, workflow syntax error) and this doc does not apply.

---

## 2. How to unblock

The fix is **account-level**, not repo-level. The PR pipeline cannot resolve
this — only the account owner (`D0n9X1n`) can.

1. Open **GitHub → Settings (user, not repo) → Billing and plans → Plans and
   usage**:
   <https://github.com/settings/billing>
2. Check the **Payment information** tab for a failed/expired card and update
   it.
3. Check the **Spending limits** section for **GitHub Actions** — if it's set
   to `$0` and the free-tier minute budget for the month is exhausted, either:
   - Raise the spending limit (recommended: `$10–20/month` is plenty for this
     project's current cadence), or
   - Wait for the monthly free-tier reset (1st of the month UTC).
4. Re-run a failed CI job to confirm: `gh run rerun <id> -R D0n9X1n/sonic`.
   The annotation should disappear and steps should populate.

### Free-tier reminder

Public repositories get **unlimited free Actions minutes on standard runners**
regardless of plan. This repo (`D0n9X1n/sonic`) is **public**, so a pure
billing block on it is unusual — it most often means:

- A **failed payment on a paid feature** (Copilot, private repo minutes, Pro
  plan) has cascaded to disable all paid features account-wide, including
  larger-runner / private-repo Actions billing, and the scheduler is rejecting
  jobs defensively. Fixing the payment method unblocks everything.
- Or the account is using a non-standard runner image (it isn't — the matrix is
  `macos-14` + `windows-latest`, both standard).

---

## 3. v0.8.0 release run — cost estimate

The release workflow (`.github/workflows/release.yml`) is triggered by pushing
a tag matching `v[0-9]+.[0-9]+.[0-9]+*`. It runs **two jobs** in parallel:

| Job          | Runner          | Multiplier | Est. wall time | Est. billed minutes |
| ------------ | --------------- | ---------- | -------------- | ------------------- |
| `build-mac`  | `macos-14`      | **10×**    | 15–20 min      | 150–200             |
| `build-win`  | `windows-latest`| **2×**     | 10–15 min      | 20–30               |
| **Total**    |                 |            |                | **170–230**         |

(GitHub's [per-minute multipliers](https://docs.github.com/en/billing/managing-billing-for-your-products/managing-billing-for-github-actions/about-billing-for-github-actions#minute-multipliers):
Linux 1×, Windows 2×, macOS 10×. macOS dominates the cost.)

### Cost on free tier (public repo)

**$0.** Public repos consume **no** billed Actions minutes on standard runners.
A v0.8.0 release on this repo costs nothing once the account-level billing
block is cleared — clearing the block is purely a gate to letting jobs start,
not a per-run charge.

### Cost on private equivalent (for reference, not applicable here)

At GitHub's published rates ($0.08/min Linux, $0.16/min Windows, $0.08/min
macOS for Pro, $0.16/min Windows / $0.08/min Linux / $0.08/min macOS metered):

- macOS: 15 min × $0.08 = **$1.20**
- Windows: 12 min × $0.016 = **$0.19**
- **Total: ~$1.40 per release.**

Even on a metered private account, a full v0.8.0 release fits inside the free
$0–10/mo spend trivially.

### Why the release won't fire today

Even with the tag pushed, the scheduler will reject `build-mac` and
`build-win` with the same "job was not started" annotation until §2 is done.
**Resolve the billing block before tagging v0.8.0**, otherwise the tag will
sit there with a red ✗ release run and no artifacts produced.

---

## 4. Workflow-side mitigations (NOT applied — for future reference)

The local gate (fmt + clippy + test + `pty_dump` e2e + release build) is the
authoritative correctness check per `CLAUDE.md` §2, and PRs are merged with
`gh pr merge --admin` regardless of CI per `CLAUDE.md` §6. So the billing
block is annoying (red badge) but not blocking day-to-day development.

If the block becomes chronic and we need to keep some CI signal, options
include:

- Drop the macOS matrix entry from `ci.yml` (saves the 10× multiplier; Windows
  + local-macOS gate covers most platform skew).
- Move `cargo-deny` to a Linux-only job (already is).
- Add a `concurrency:` group to cancel superseded runs on the same branch
  (already common, worth re-checking).

None of these are applied in this PR — the right fix is to clear the billing
block, not to paper over it by removing coverage.

---

## 5. TL;DR

- ✅ Confirmed billing-blocked (zero steps; explicit annotation; perms enabled).
- ✅ Repo is public → standard-runner minutes are free → fix is account-level
  billing, not repo config.
- ✅ v0.8.0 release will cost **$0** in billed minutes once the block clears.
- ❌ Do not tag v0.8.0 until the billing block is cleared at
  <https://github.com/settings/billing>, or the tag's release run will fail to
  start and no DMG/MSI artifacts will be produced.
