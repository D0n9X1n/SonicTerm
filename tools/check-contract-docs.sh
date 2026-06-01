#!/usr/bin/env bash
# tools/check-contract-docs.sh
#
# Verify that any change to the public API of `sonicterm-types` ships with
# either an updated api-snapshot.txt OR an updated docs/CONTRACTS.md in
# the same diff. Drift without either is a contract regression.
#
# Status: M1 = warn-only (exit 0 with a notice). M8 flips to fail.

set -u

SNAP=crates/sonicterm-types/api-snapshot.txt
DOC=docs/CONTRACTS.md
TMP=$(mktemp)
trap 'rm -f "$TMP"' EXIT

MODE="${1:-warn}"   # "warn" (default, M1) or "fail" (M8+)

if ! command -v cargo-public-api >/dev/null 2>&1; then
    echo "ℹ️  cargo-public-api not installed (cargo install cargo-public-api --locked)."
    echo "    Skipping snapshot diff. TODO: enable once installed in CI."
    exit 0
fi

if [[ ! -f "$SNAP" ]]; then
    echo "ℹ️  $SNAP missing — first run. Generate with:"
    echo "    cargo public-api -p sonicterm-types --simplified > $SNAP"
    exit 0
fi

cargo public-api -p sonicterm-types --simplified > "$TMP" 2>/dev/null || {
    echo "ℹ️  cargo public-api failed (build error?). Skipping."
    exit 0
}

if diff -q "$SNAP" "$TMP" >/dev/null 2>&1; then
    echo "✓ sonicterm-types public-api unchanged vs snapshot"
    exit 0
fi

# Snapshot drifted. Was the snapshot OR CONTRACTS.md updated in the PR?
BASE="${GITHUB_BASE_REF:-origin/main}"
TOUCHED=$(git diff --name-only "$BASE"...HEAD 2>/dev/null || git diff --name-only HEAD)

if echo "$TOUCHED" | grep -qE "^($(echo "$SNAP" | sed 's|/|\\/|g')|$(echo "$DOC" | sed 's|/|\\/|g'))$"; then
    echo "✓ public-api drifted AND snapshot/CONTRACTS.md updated in diff"
    exit 0
fi

echo "❌ sonicterm-types public-api drifted, but neither $SNAP nor $DOC is in the diff"
diff "$SNAP" "$TMP" | head -40

if [[ "$MODE" == "fail" ]]; then
    exit 1
else
    echo ""
    echo "ℹ️  WARN-ONLY (M1). Will flip to FAIL at M8."
    exit 0
fi
