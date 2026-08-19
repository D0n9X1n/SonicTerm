#!/usr/bin/env bash
# Generate GitHub Release notes from the validated release-asset manifest.
set -euo pipefail

TAG="${1:-${GITHUB_REF_NAME:-}}"
MANIFEST="${2:-${RELEASE_ASSET_MANIFEST:-dist/release-assets.json}}"
if [[ -z "$TAG" ]]; then
  echo "usage: $0 <tag> [release-assets.json]" >&2
  exit 2
fi
if [[ ! -f "$MANIFEST" ]]; then
  echo "release asset manifest not found: $MANIFEST" >&2
  exit 2
fi

PREVIOUS_TAG="${PREVIOUS_TAG:-}"
if [[ -z "$PREVIOUS_TAG" ]]; then
  PREVIOUS_TAG="$(git describe --tags --abbrev=0 "${TAG}^" 2>/dev/null || true)"
fi

echo "# SonicTerm ${TAG}"
echo
echo "## Downloads"
echo
python3 - "$MANIFEST" "$TAG" <<'PY'
import json
import pathlib
import sys

manifest_path = pathlib.Path(sys.argv[1])
tag = sys.argv[2]
manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
if manifest.get("tag") != tag:
    raise SystemExit(f"manifest tag {manifest.get('tag')!r} does not match {tag!r}")
assets = manifest.get("assets")
if not isinstance(assets, list) or not assets:
    raise SystemExit("release asset manifest has no assets")
for asset in assets:
    print(
        f"- {asset['platform']} / {asset['arch']} / {asset['kind']}: "
        f"download `{asset['name']}`."
    )
PY
echo "- Integrity metadata: \`release-assets.json\` and \`SHA256SUMS.txt\`."
echo "- Release packages are unsigned for ${TAG}; macOS may require right-click → Open."
echo

if [[ -n "$PREVIOUS_TAG" ]]; then
  echo "## Changes since ${PREVIOUS_TAG}"
  echo
  git log --no-merges --pretty=format:'- %s (%h)' "${PREVIOUS_TAG}..${TAG}"
  echo
else
  echo "## Changes"
  echo
  git log --no-merges --pretty=format:'- %s (%h)' "${TAG}" | head -200
  echo
fi

echo
echo "## Verification"
echo
echo "- Unit tests run in CI on macOS, Windows, and Linux."
echo "- The release workflow validates tag/workspace-version consistency and all manifest-registered assets."
echo "- Linux packages pass X11/Xvfb and Wayland/Weston runtime smokes before publication."
