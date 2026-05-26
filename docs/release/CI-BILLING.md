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
regardless of plan. **This repo (`D0n9X1n/sonic`) is currently PRIVATE**
(verified via `gh repo view D0n9X1n/sonic --json visibility,owner` → `PRIVATE`,
owner type `User`), so Actions minutes **are** metered against the personal
account's free-tier quota and any overage is billed. That makes the billing
block the expected failure mode when the quota is exhausted or the payment
method has lapsed.

Common upstream causes:

- A **failed payment on a paid feature** (Copilot, private repo minute
  overage, Pro plan) has cascaded to disable all paid features account-wide,
  and the scheduler rejects jobs defensively. Fixing the payment method
  unblocks everything.
- The personal free-tier minute budget for the month is exhausted (see §3 for
  per-OS quotas) and the spending limit is still `$0`.
- The account is using a non-standard runner image (it isn't — the matrix is
  `macos-14` + `windows-latest`, both standard).

---

## 3. v0.8.0 release run — cost estimate

The release workflow (`.github/workflows/release.yml`) is triggered by pushing
a tag matching `v[0-9]+.[0-9]+.[0-9]+*`. It runs **two jobs** in parallel
(plus a small Linux `ci.yml` lint/test on the tag push).

### Per-minute pricing (personal private repo, metered)

Per GitHub's published rates ([About billing for GitHub Actions](https://docs.github.com/en/billing/managing-billing-for-github-actions/about-billing-for-github-actions)),
the **dollar** price per metered minute is:

| Runner OS       | $/min      |
| --------------- | ---------- |
| Linux 2-core    | **$0.008** |
| Windows 2-core  | **$0.016** |
| macOS 3-core    | **$0.08**  |

The published **multipliers** (Linux 1×, Windows 2×, macOS 10×) apply *only*
to **free-tier quota accounting** — i.e. how fast a minute drains the monthly
free allotment. They do **not** stack on top of the dollar rates above; the
$0.08/min macOS price is the all-in metered cost per wall-clock minute.

### Per-release dollar cost (wall-clock × $/min)

| Job          | Runner            | Wall time  | Dollar cost                |
| ------------ | ----------------- | ---------- | -------------------------- |
| `build-mac`  | `macos-14`        | ~15 min    | 15 × $0.08 = **$1.20**     |
| `build-win`  | `windows-latest`  | ~10 min    | 10 × $0.016 = **$0.16**    |
| `ci` (lint)  | `ubuntu-latest`   | ~5 min     | 5 × $0.008 = **$0.04**     |
| **Total**    |                   |            | **~$1.40 per release**     |

### Free-tier quota accounting (weighted minutes)

Personal accounts get **2,000 free weighted minutes / month** on the Free
plan. Weighted minutes apply the multipliers above:

| Job          | Wall time | Multiplier | Weighted min/release |
| ------------ | --------- | ---------- | -------------------- |
| `build-mac`  | ~15 min   | 10×        | 150                  |
| `build-win`  | ~10 min   | 2×         | 20                   |
| `ci` (lint)  | ~5 min    | 1×         | 5                    |
| **Total**    |           |            | **~175 / release**   |

A representative month — **4 releases + ~10 PR-CI runs / week** — burns
roughly 4 × 175 = 700 weighted min on releases, plus ~40 × 5 ≈ 200 weighted
min on Linux PR CI (if the PR matrix is Linux-only). That's **~900 weighted
min / month, well under the 2,000 free-tier ceiling** → $0 out of pocket. A
full macOS+Windows PR matrix would push past the ceiling, but the bounded
metered cost is still single-digit dollars at this cadence.

### Recommendation — switch repo to public

Making `D0n9X1n/sonic` **public** would:

- Drop billed minutes for all standard-runner jobs to **$0** (public repos get
  unlimited free Actions minutes on standard runners regardless of plan).
- Remove the billing-block failure mode entirely — the scheduler stops
  consulting account billing for public-repo standard-runner jobs.
- Make the release badge / CI status visible to anyone (and reproducible by
  contributors), which aligns with the project's eventual open-source posture.

Worth doing at release time if the user is comfortable with the repo going
public; until then, raise the spending limit to ~$15/month to absorb worst
case.

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
- ⚠️ Repo is **private** (personal account) → standard-runner minutes ARE
  metered against the personal account's free-tier quota. Fix is account-level
  billing (payment method + spending limit at
  <https://github.com/settings/billing>).
- ✅ v0.8.0 release costs **~175 weighted minutes (~$1.40 fully metered:
  $1.20 mac + $0.16 win + $0.04 linux)**, $0 within the 2,000-min free quota.
- 💡 Optional: making the repo **public** drops standard-runner billed minutes
  to $0 and removes the billing-block failure mode entirely. Consider at
  release time.
- ❌ Do not tag v0.8.0 until the billing block is cleared at
  <https://github.com/settings/billing>, or the tag's release run will fail to
  start and no DMG/MSI artifacts will be produced.
