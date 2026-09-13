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

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
if [[ "$(git rev-parse --is-shallow-repository)" != false ]]; then
  echo "release notes require complete history; shallow repositories are not supported" >&2
  exit 2
fi
HEAD_COMMIT="$(git rev-parse --verify --end-of-options "${TAG}^{commit}")"
if [[ "${RELEASE_FIRST:-0}" == 1 && "${PREVIOUS_TAG+x}" == x ]]; then
  echo "RELEASE_FIRST=1 conflicts with PREVIOUS_TAG" >&2
  exit 2
fi
PREVIOUS_TAG="${PREVIOUS_TAG:-}"
if [[ "${RELEASE_FIRST:-0}" != 1 && -z "$PREVIOUS_TAG" ]]; then
  if ! PREVIOUS_TAG="$(git describe --tags --abbrev=0 "${HEAD_COMMIT}^")"; then
    echo "release predecessor lookup failed; fetch complete tags or explicitly set RELEASE_FIRST=1 for the first release" >&2
    exit 2
  fi
fi
# Complete all metadata lookups before emitting notes; an API error is not an empty release.
ISSUES="$(python3 "$ROOT/release-issues.py" \
  --repo "${GITHUB_REPOSITORY:-D0n9X1n/SonicTerm}" \
  --head "$HEAD_COMMIT" --base "$PREVIOUS_TAG")"

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

printf '%s\n\n' "$ISSUES"

if [[ -n "$PREVIOUS_TAG" ]]; then
  echo "## Changes since ${PREVIOUS_TAG}"
  echo
  git log --no-merges --pretty=format:'- %s (%h)' "${PREVIOUS_TAG}..${HEAD_COMMIT}"
  echo
else
  echo "## Changes"
  echo
  git log --no-merges --max-count=200 --pretty=format:'- %s (%h)' "$HEAD_COMMIT"
  echo
fi

echo
echo "## Verification"
echo
echo "- Unit tests run in CI on macOS, Windows, and Linux."
echo "- The release workflow validates tag/workspace-version consistency and all manifest-registered assets."
echo "- Linux packages pass X11/Xvfb and Wayland/Weston runtime smokes before publication."
