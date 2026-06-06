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
  PREVIOUS_TAG="$(git tag --merged "$TAG" --sort=-v:refname | grep -E '^v[0-9]+' | grep -v "^${TAG}$" | head -1 || true)"
fi

echo "# SonicTerm ${TAG}"
echo
echo "## Installers"
echo
echo "- macOS: download \`SonicTerm-${TAG}-mac-universal.dmg\`."
echo "- Windows: download the \`.msi\` artifact."
echo "- Both installers are unsigned for v1.0.0; macOS may require right-click → Open."
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
