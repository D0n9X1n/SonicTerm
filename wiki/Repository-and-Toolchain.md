# Repository and Toolchain

[简体中文](Repository-and-Toolchain-zh-CN)

This page describes the repository layout, the toolchain, build entry points,
code conventions, and maintenance of the vendored native sources.

```text
Cargo.toml     workspace members, shared package metadata, dependencies, profiles, lints
crates/        23 first-party Rust crates
assets/        fonts, themes, keymaps, icons, localization, screenshots
wiki/          canonical bilingual documentation
scripts/       flat first-party shell and PowerShell automation
.github/       CI, release, wiki publication, issue, PR, and dependency automation
```

The workspace uses resolver 2, Rust edition 2021, and minimum Rust 1.95.
`rust-toolchain.toml` selects stable with rustfmt and clippy. The authoritative
version is `Cargo.toml [workspace.package].version`; every workspace package and
internal path requirement uses it.

Build or run the platform entry point on its native host:

```sh
cargo build
cargo run -p sonicterm-mac       # macOS
cargo run -p sonicterm-windows   # Windows
cargo run -p sonicterm-linux     # Linux; executable name: sonicterm
```

Every first-party workspace crate has a local `CLAUDE.md`. Unit-test files are flat siblings
named after the source that declares them: `foo.rs` owns `foo_tests.rs`, or
`foo_<topic>_tests.rs` when a suite needs topical splits to stay below 1,000 lines. Each is
explicitly declared with `#[cfg(test)] #[path = "foo_tests.rs"] mod foo_tests;`, substituting
the topical filename when needed. Platform conditions must still require `test`.
`lib.rs`, `main.rs` and `mod.rs` follow their own stems. A test-only source can own another
suite under its full stem: `foo_tests.rs` declares `foo_tests_fixture_tests.rs`. Module
identifiers need not change with filenames, preserving subprocess filters and `use super::*`.
Each test file has one declaring owner; overlapping names resolve to the longest matching
sibling source stem. Inline unit-test modules, generic `tests.rs`, and nested test folders
are not used. `tests/` remains reserved for public-API or cross-crate integration.

The `source-size` gate validates these names against actual test-gated `#[path]` declarations,
not text inside comments, strings or macros. Generic `test.rs`/`tests.rs` and singular
`*_test.rs` files fail even when orphaned. It also resolves file-scope external modules by
explicit or default Rust paths, regardless of their gate. A file containing `#[test]` or
`#[bench]` is a suite even under an arbitrary filename; its declaration must require `test`,
so an absent gate or `cfg(any(test, feature = "fixtures"))` fails. A file-scope test-gated
inline module containing these attributes also fails. Helper-only modules and build code
imported for testing, with no such attributes of their own, keep their implementation filenames.

This source scan does not expand `cfg_attr(test, path = …)` or `include!`, resolve declarations
nested inside an inline module, or expand `macro_rules!`-generated tests. Those forms need
manual review; a green check does not establish their ownership or exempt them from the rule.
It reads all tracked first-party Rust, including
generated crate roots that declare authored tests, but excludes native upstream trees.
The `sonicterm-ui` and `sonicterm-render-model` crate-root suites also require each direct
source module to have an exact primary sibling declaration or a non-empty explicit exemption.
That sibling must exist and contain a `#[test]`; source-directory modules fail their flat
inventory. An exemption becomes stale when the module gains its primary sibling suite.

Tests that assert on captured tracing output use `sonicterm_logging::test_capture`.
Its `with_default` wrapper keeps each test's subscriber, filter, and sink while a
silent process-global dispatcher prevents an uncaptured first reach from disabling
the call site. Threads outside a capture still admit no events. Do not combine
this helper with production logging initialization in the same test process.

Names say what they hold. Variables, parameters, closures, loop bindings,
fields, functions and constants are never one character, a letter followed by
digits, or two letters outside the `allowed-idents-below-min-chars` list in
`clippy.toml`, in production code and in tests; name the quantity and its
unit, such as `row_count`, `timeout_s` or `width_px`. That list replaces
Clippy's default allowlist, which admits `i`, `x` and `y`, and holds
conventional abbreviations, real words, heading, size and version tokens,
comparison-trait method names and the lifetimes `'a` and `'_`. It also holds
`vt`, the name of the `sonicterm-vt` terminal module (`pub mod vt`): renaming
that module would change its default log targets, and no attribute exempts only
a module's name, because an `allow` on `pub mod vt` turns the lint off for all
of `vt.rs`. Generic type
parameters, lifetimes, const generics, `_`, vendored code, generated FFI
bindings, `extern` declarations and `#[repr(C)]` fields that copy a C header,
and names fixed by an external contract (serde keys, log fields, config keys,
CLI flags) are exempt. Clippy's `min_ident_chars` enforces the rule in each
crate that enables it. `scripts/regenerate-freetype.sh` and
`scripts/regenerate-harfbuzz.sh` rewrite `crates/sonicterm-freetype/src/lib.rs`,
`crates/sonicterm-freetype/src/types.rs` and
`crates/sonicterm-harfbuzz/src/lib.rs`, so those generated bindings carry no
naming-lint attribute; the hand-written modules in those two crates enable the
lint with an inner `#![warn(clippy::min_ident_chars)]`. For scripts, the `script-identifiers` gate step runs
`scripts/check-script-identifiers.py` over the tracked `scripts/*.py` files:
it checks assignment, `for`, comprehension, `with ... as` and `except ... as`
targets, function and lambda parameters, function and class names, and
import aliases; skips `_`, `_`-prefixed names, `self` and `cls`; prints each
finding as `path:line name`; and exits 1 when any remain, or 2 when it cannot
read `clippy.toml`, the tracked-file list or a script.

## Native dependency maintenance

`scripts/native-dependencies.json` is the machine-readable inventory for the
embedded native libraries and the pinned winit source. Each entry pins an upstream release commit,
archive checksum, explicit source subset, the upstream fixes carried on top of
that release, and the complete imported tree checksum. An `upstream_fixes` record
is provenance only: a full upstream revision and the URL to read it. The
repository keeps no local patch files, because the vendored sources — not an
archive plus a patch series — are the source of truth for what SonicTerm builds.
Required sources, headers, licenses, and changelogs are retained; unneeded
upstream demos and CI trees are not build inputs. This is separate from
Cargo.lock and from platform-provided Cairo/Fontconfig.

`crates/sonicterm-winit` retains the Windows/macOS/Linux source subset of upstream
winit 0.30.13, with the Windows-only native-key metadata extension. Its package
identity remains `winit`, and it is excluded from first-party workspace membership;
the directory name does not make it a SonicTerm-versioned package. Cargo pins that
version and patches it to this local source. Shared code, desktop backends, required
fixture data, unit tests, Send/Sync/serde integration tests and the Apache-2.0 license
are retained. Examples, example-only development dependencies, historical documentation,
and Android/iOS/Web/Redox backends are omitted; unsupported targets are rejected.

The source inventory keeps the original archive checksum and upstream revision,
records the imported subset, and pins a digest over every file in the local tree.
The subset list does not hide extra files from verification. Modified upstream
files carry local change notices, not entries claiming upstream fixes. The
Apache-2.0 license ships with each desktop package; see [Packaging](Packaging).

Preserved winit source is excluded from the first-party authored-comment scan.
The authored keyboard sibling test remains checked; reviewers inspect every changed
hunk in mixed upstream files for purpose, safety and control-flow rationale.
A matching tree digest detects source drift but does not replace that review.
On every desktop host the gate explicitly runs the dependency's unit and integration
tests with `serde`, plus warnings-denied Rustdoc, because workspace exclusion also
excludes it from workspace tests and documentation. Windows includes the native
metadata unit tests. Both invocations use `--locked` with the subset's independently
pruned `Cargo.lock`; checks of the workspace lockfile alone do not cover that graph.
Build output goes to the repository target directory, not the pinned source tree.

The verifier is Python-standard-library-only, offline, and check-only:

```sh
python3 scripts/native-dependencies.py check
python3 scripts/native-dependencies.py check --library freetype
python3 scripts/native-dependencies_tests.py
```

`check` rejects missing, modified, or additional vendor files, unpinned tree
hashes, manifest entries that still declare the retired `patches` key, and
`upstream_fixes` records that omit a revision or URL or name a local file.
Source paths and raw bytes form the hash; executable modes and empty directories
do not. `.gitattributes` disables newline conversion for vendor trees so Windows
checks the same upstream bytes.

A matching digest proves the working copy still holds the reviewed, committed
bytes. It does not establish publisher identity and does not prove absence of
vulnerabilities; that evidence comes from the recorded base release and from
reading each recorded upstream fix at its source. There is no command that
reconstructs a locally patched tree, and the tool does not pretend otherwise.

For an update, use one reviewable library change at a time:

1. Review the official release and advisories, including regressions in the new
   release. Verify the publisher/source and available signature or independently
   published checksum; record any verification limitation. A newer version is not
   automatically a fixed version. Avoid branch-head or unattended imports.
2. Update that manifest entry with the immutable release identity and verified
   archive hash. Record every required upstream fix as a full revision and URL,
   and inspect its prerequisites upstream. Drop a recorded fix only after proving
   the selected release contains it. Keep unrelated native features enabled.
3. Download the recorded archive separately and expand it outside the repository.
   Import the recorded source subset by hand and apply each recorded upstream fix
   from its upstream commit, comparing that commit against the release you are
   importing rather than against a stored copy. Diff the result against the
   current vendor tree, replace only that clean vendor directory, then record the
   new `tree_sha256` from `check`'s reported digest after reviewing the diff.
   Review added/removed C/C++ files against `build.rs`; an import is not a
   compilation result.
4. For FreeType/HarfBuzz, use `cargo install bindgen-cli --version 0.71.1 --locked`
   and run `bash scripts/regenerate-freetype.sh` or
   `bash scripts/regenerate-harfbuzz.sh`. `BINDGEN` can select a separately installed
   executable of that exact version. Both scripts compile the small
   `scripts/freetype-config.rs` helper to reuse the build's configured header.
   Review ABI and behavioral changes, not just
   version strings; regeneration must preserve crate-owned modules and tests.
5. Run the offline checks, native crate tests, full local gate, and native
   rendering checks under normal colors with isolated config/log roots and HOME
   preserved. Compare variable/color fonts, CJK, emoji, ligatures, fallback and
   raster output. Require exact-head platform CI; local macOS tests do not validate
   Windows code or Intel binaries. Update both wiki language files in the same PR.

The vendored FreeType, HarfBuzz and libpng are plain upstream releases with no
carried fixes. The vendored zlib carries the invalid-distance decoding fix and the
related gzip-write corrections after 1.3.2. Those fixes are already present in the
imported sources, and the manifest records the full upstream commits for each one.
These choices do not establish that every advisory is reachable through SonicTerm
or that every remaining upstream defect is fixed. Recheck upstream
releases/advisories when preparing each dependency update and before a release;
updates remain reviewed changes rather than automatic merges.
