#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
prepare="$root/scripts/prepare-release-assets.py"

fail() {
  printf 'release asset test: %s\n' "$1" >&2
  exit 1
}

[[ -x "$prepare" ]] || fail "prepare-release-assets.py is missing or not executable"

release_workflow="$root/.github/workflows/release.yml"
linux_unit_job="$(python3 - "$release_workflow" <<'PY'
import pathlib
import re
import sys

text = pathlib.Path(sys.argv[1]).read_text()
matched = re.search(
    r"(?ms)^  unit-tests-linux:\n.*?(?=^  [a-z][a-z0-9_-]*:\n|\Z)",
    text,
)
if matched is None:
    raise SystemExit("release workflow has no unit-tests-linux job")
print(matched.group(0), end="")
PY
)"
for runtime_dependency in mesa-vulkan-drivers libvulkan1; do
  grep -Fq -- "$runtime_dependency" <<<"$linux_unit_job" || \
    fail "release Linux unit job is missing $runtime_dependency for adapter enumeration"
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

"$prepare" check-version --tag v1.2.5 --repo-root "$root" >/dev/null
if "$prepare" check-version --tag v9.8.7 --repo-root "$root" >"$tmp/version.out" 2>&1; then
  fail "mismatched tag/workspace version was accepted"
fi
grep -Fq 'expects workspace version 9.8.7' "$tmp/version.out" || \
  fail "version mismatch failure was not actionable"

printf 'release asset test: ok\n'
