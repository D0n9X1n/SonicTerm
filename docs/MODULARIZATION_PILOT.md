# Modularization pilot — success criteria + exit conditions

This document defines when the v0.9 modularization epic (#429) is
**done** and what would let us call the pilot a success or a failure.
The PILOT itself is a follow-up to v0.9 — this file establishes the
target before we ship the work that lets us claim it.

## What "done" looks like

The v0.9 work is considered complete when ALL of these hold:

### 1. Docs scaffolding (M0–M5, M9, M10 — landed locally)

- [x] `docs/agents/_common.md` exists (≤ 100 lines, single agent-shared doc)
- [x] `docs/CONTRACTS.md` exists, declares the 2-PR deprecation protocol
- [x] Root `CLAUDE.md` ≤ 150 lines with routing table at top
- [x] 17 per-crate `CLAUDE.md` files, each ≤ 80 lines
- [x] `landmines.toml` with the 8 initial LM-001..LM-008 entries
- [x] `tools/check-landmines.sh` + `tools/check-contract-docs.sh` +
  `tools/check-ownership.sh` all wired into CI
- [x] `.github/CODEOWNERS` routes mac-only, win-only, hot-files, contract
- [x] `docs/HOT_FILES.md` lists the 13 hot files with rationale
- [x] `docs/migrations/0.9.0.md` documents the façade → leaf mapping
- [x] All three self-enforcing meta-tests pass:
      `contract_landmine_coverage`, `contract_traits_have_tests`,
      `contract_<trait>` × 5

### 2. Trait seams (M4 — landed locally)

- [x] `crates/sonicterm-types/src/traits/{pty,painter,window,clipboard}.rs`
- [x] `crates/sonicterm-types/api-snapshot.txt` baselined by
      `cargo public-api -p sonicterm-types --simplified`
- [x] Each trait has `contract_<trait>.rs` asserting object-safety + Send + shape

### 3. CI gates (M8 — landed locally)

- [x] `check-contract-docs.sh fail`
- [x] `check-landmines.sh fail`
- [x] `check-ownership.sh fail`

### 4. Façade deprecation (M9 — landed locally)

- [x] Every `pub use` in `sonicterm-core` carries `#[deprecated(since = "0.9.0")]`
- [x] Every `pub use` in `sonicterm-shared` carries `#[deprecated(since = "0.9.0")]`
- [x] Workspace builds + tests + clippy all clean despite deprecations

### 5. Modularization extraction (M6b, M6c, M6d, M7 — DEFERRED past v0.9)

These items require live §13 GUI smoke (mac + win) per PR and are NOT
attempted in the M0–M10 single-session run. They go through the normal
PM-dispatched per-PR cycle post-v0.9:

- [ ] M6b: `sonicterm-mac::main` consumes `sonicterm-app-core` for state
- [ ] M6c: `sonicterm-windows::main` mirrors M6b on the Windows side
- [ ] M6d: delete the old `sonicterm-app` direct-call surface
- [ ] M7: dissolve `sonicterm-shared::render/*` into render-owned crates

## Exit conditions for the pilot

The pilot is **successful** if, 90 days after the last M6/M7 PR lands:

1. **Agent context budget actually fell.** Median per-PR loaded-context
   for an agent (root CLAUDE.md + `_common.md` + crate-local CLAUDE.md
   for touched crates) is ≤ 8 KB on > 80% of PRs in the window.
2. **No silent landmine regression shipped.** Zero P0 issues filed for
   any LM-NNN in landmines.toml during the window.
3. **Contract gate fired and was useful at least once.** At least one
   PR was caught by `check-contract-docs.sh` for genuine snapshot drift
   (false positives don't count) and the catch prevented a downstream
   consumer break.
4. **Ownership gate fired and was useful at least once.** At least one
   new top-level file or crate addition was correctly flagged as
   needing a CODEOWNERS entry.
5. **Crate-local CLAUDE.md stayed within budget.** No per-crate doc
   exceeded 100 lines (the soft cap); any growth past 80 triggered a
   split discussion.
6. **Façade migration converged.** > 80 % of first-party `use sonicterm_core::*`
   and `use sonicterm_shared::*` imports migrated to leaf crates.

## Failure modes that should re-open the design

The pilot is **failed** if any of these is true at 90 days:

1. **Context budget grew.** Median per-PR context > 16 KB — the
   per-folder split didn't actually reduce what agents load.
2. **A P0 landmine regression shipped.** Indicates the diff-scoped
   gate is too narrow OR the landmine entry was rotted out without the
   self-test catching it.
3. **Gate false-positive rate > 20 %.** Agents start adding
   `# allow` overrides or PMs bypass the gates with `--admin`; the
   gate is creating more friction than value.
4. **Crate-local CLAUDE.md stayed empty / boilerplate.** Indicates the
   docs are aspirational rather than load-bearing — agents didn't
   actually use them.
5. **Façade deprecation ignored.** > 50 % of first-party imports still
   on `sonicterm-core` or `sonicterm-shared` at v1.0.0 — the
   deprecation didn't drive migration.

If any failure mode triggers, re-open #429 and reconsider Options A
(named-role agents) or C (monolithic CLAUDE.md with smarter section
loading) from the original three-way comparison.

## Roles during the pilot

- **Tag-owner PM** for the window owns this doc and updates the
  success-criteria table at every release tag in the window.
- **Both PMs** are responsible for migrating their own touched files
  off the façades during the window (a few imports per PR; no
  big-bang migration PR).
- **Any agent** running `tools/check-*.sh` and seeing a false positive
  files an issue with the `modularization-pilot` label and the
  comparison output attached.
