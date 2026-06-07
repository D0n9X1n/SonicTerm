#!/usr/bin/env bash
# Generate GitHub Release notes for a tag from commits since the previous tag.
set -euo pipefail

TAG="${1:-${GITHUB_REF_NAME:-}}"
if [[ -z "$TAG" ]]; then
  echo "usage: $0 <tag>" >&2
  exit 2
fi

PREVIOUS_TAG="${PREVIOUS_TAG:-}"
if [[ -z "$PREVIOUS_TAG" ]]; then
  PREVIOUS_TAG="$(git describe --tags --abbrev=0 "${TAG}^" 2>/dev/null || true)"
fi

echo "# SonicTerm ${TAG}"
echo
echo "## Installers"
echo
echo "- macOS: download \`SonicTerm-${TAG}-mac-universal.dmg\` (universal2: Apple Silicon + Intel)."
echo "- Windows: download the \`.msi\` artifact."
echo "- Downloadable files are attached to this GitHub Release, including \`SHA256SUMS.txt\`."
echo "- Both installers are unsigned for ${TAG}; macOS may require right-click → Open."
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
echo "- Unit tests run in CI on macOS and Windows."
echo "- Release workflow builds the macOS dmg and Windows msi from the tagged commit."
