#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
prepare="$root/scripts/prepare-release-assets.py"

python3 "$root/scripts/test-release-tool-pins.py"
if command -v pwsh >/dev/null 2>&1; then
  pwsh -NoLogo -NoProfile -File "$root/scripts/validate-windows-msi_tests.ps1"
fi

fail() {
  printf 'release asset test: %s\n' "$1" >&2
  exit 1
}

[[ -x "$prepare" ]] || fail "prepare-release-assets.py is missing or not executable"

release_workflow="$root/.github/workflows/release.yml"
linux_package_job="$(python3 - "$release_workflow" <<'PY'
import pathlib
import re
import sys

text = pathlib.Path(sys.argv[1]).read_text()
matched = re.search(
    r"(?ms)^  package-linux:\n.*?(?=^  [a-z][a-z0-9_-]*:\n|\Z)",
    text,
)
if matched is None:
    raise SystemExit("release workflow has no package-linux job")
print(matched.group(0), end="")
PY
)"
for runtime_dependency in mesa-vulkan-drivers libvulkan1; do
  grep -Fq -- "$runtime_dependency" <<<"$linux_package_job" || \
    fail "release Linux package job is missing $runtime_dependency for adapter enumeration"
done

tmp="$(mktemp -d "${TMPDIR:-/tmp}/sonic-release-assets.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT
dist="$tmp/dist"
mkdir -p "$dist"

assets=(
  'SonicTerm-v9.8.7-mac-aarch64.dmg macos aarch64 dmg'
  'SonicTerm-v9.8.7-mac-x86_64.dmg macos x86_64 dmg'
  'SonicTerm-v9.8.7-windows-x86_64.msi windows x86_64 msi'
  'SonicTerm-v9.8.7-linux-x86_64.deb linux x86_64 deb'
  'SonicTerm-v9.8.7-linux-x86_64.tar.gz linux x86_64 tar.gz'
  'SonicTerm-v9.8.7-linux-x86_64.symbols.zip linux x86_64 symbols'
)

for row in "${assets[@]}"; do
  read -r name platform arch kind <<<"$row"
  printf 'fixture for %s\n' "$name" > "$dist/$name"
  "$prepare" fragment \
    --tag v9.8.7 \
    --asset "$dist/$name" \
    --platform "$platform" \
    --arch "$arch" \
    --kind "$kind" \
    --output "$dist/${platform}-${arch}-${kind//./-}.asset.json"
done

"$prepare" consolidate --tag v9.8.7 --dist "$dist"
python3 - "$dist" <<'PY'
import hashlib
import json
import pathlib
import sys

root = pathlib.Path(sys.argv[1])
manifest = json.loads((root / "release-assets.json").read_text())
assert manifest["tag"] == "v9.8.7"
assert len(manifest["assets"]) == 6
assert any(asset["kind"] == "symbols" for asset in manifest["assets"])
assert manifest["assets"] == sorted(
    manifest["assets"],
    key=lambda asset: (asset["platform"], asset["arch"], asset["kind"], asset["name"]),
)

checksums = (root / "SHA256SUMS.txt").read_text().splitlines()
assert len(checksums) == 7
for line in checksums:
    digest, name = line.split("  ", 1)
    assert digest == hashlib.sha256((root / name).read_bytes()).hexdigest()

uploads = (root / "release-upload-paths.txt").read_text().splitlines()
assert len(uploads) == 8
assert uploads[-2].endswith("/release-assets.json")
assert uploads[-1].endswith("/SHA256SUMS.txt")
assert not any("release-upload-paths.txt" in path for path in uploads)
PY

printf stray > "$dist/SonicTerm-v9.8.7-unregistered.deb"
if "$prepare" consolidate --tag v9.8.7 --dist "$dist" >"$tmp/unregistered.out" 2>&1; then
  fail "unregistered release-like file was accepted"
fi
grep -Fq 'unregistered release-like files' "$tmp/unregistered.out" || \
  fail "unregistered asset failure was not actionable"
rm "$dist/SonicTerm-v9.8.7-unregistered.deb"

"$prepare" check-version --tag v1.2.8 --repo-root "$root" >/dev/null
if "$prepare" check-version --tag v9.8.7 --repo-root "$root" >"$tmp/version.out" 2>&1; then
  fail "mismatched tag/workspace version was accepted"
fi
grep -Fq 'expects workspace version 9.8.7' "$tmp/version.out" || \
  fail "version mismatch failure was not actionable"

fixture_repo="$tmp/repository"
git init -q "$fixture_repo"
printf 'release commit\n' > "$fixture_repo/source"
git -C "$fixture_repo" add source
git -C "$fixture_repo" -c user.name=fixture -c user.email=fixture@example.invalid \
  commit -q -m fixture
fixture_commit="$(git -C "$fixture_repo" rev-parse HEAD)"
git -C "$fixture_repo" -c user.name=fixture -c user.email=fixture@example.invalid \
  tag -a v9.8.7 -m fixture
fixture_tag_object="$(git -C "$fixture_repo" rev-parse v9.8.7)"
[[ "$fixture_tag_object" != "$fixture_commit" ]] || \
  fail "annotated-tag fixture did not create a distinct tag object"
resolved_commit="$(
  "$prepare" resolve-commit --revision "$fixture_tag_object" --repo-root "$fixture_repo"
)"
[[ "$resolved_commit" == "$fixture_commit" ]] || \
  fail "annotated tag object did not resolve to its release commit"

ci_sha='aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'
successful_ci='{"workflow_runs":[{"id":42,"head_sha":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","head_branch":"main","event":"push","status":"completed","conclusion":"success","html_url":"https://example.invalid/actions/runs/42"}]}'
printf '%s\n' "$successful_ci" | \
  "$prepare" check-main-ci --sha "$ci_sha" >"$tmp/main-ci.out"
grep -Fq "$ci_sha" "$tmp/main-ci.out" || \
  fail "successful main CI evidence did not name the exact SHA"

for invalid_ci in \
  '{"workflow_runs":[{"id":43,"head_sha":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","head_branch":"main","event":"push","status":"completed","conclusion":"success"}]}' \
  '{"workflow_runs":[{"id":44,"head_sha":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","head_branch":"main","event":"push","status":"completed","conclusion":"failure"}]}' \
  '{"workflow_runs":[{"id":45,"head_sha":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","head_branch":"main","event":"pull_request","status":"completed","conclusion":"success"}]}' \
  '{"workflow_runs":[{"id":46,"head_sha":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","head_branch":"feature","event":"push","status":"completed","conclusion":"success"}]}'
do
  if printf '%s\n' "$invalid_ci" | \
    "$prepare" check-main-ci --sha "$ci_sha" >"$tmp/main-ci-invalid.out" 2>&1
  then
    fail "non-matching CI evidence was accepted for the release SHA"
  fi
  grep -Fq "no completed successful main push CI run for $ci_sha" \
    "$tmp/main-ci-invalid.out" || \
    fail "invalid main CI evidence failure was not actionable"
done

printf 'release asset test: ok\n'
