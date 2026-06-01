#!/usr/bin/env bash
# tools/check-landmines.sh
#
# Diff-scoped landmine gate. For every file in the PR diff, look up the
# matching landmines.toml entries and run their required_test_paths.
# Fast — only runs the tests for the rules whose globs match a touched
# file. Pre-commit hook + CI.
#
# Status: M2 = warn-only. M8 flips to fail (toggle via $MODE).

set -u

MODE="${1:-warn}"   # "warn" (default, M2) or "fail" (M8+)
TOML=landmines.toml

if [[ ! -f "$TOML" ]]; then
    echo "ℹ️  $TOML missing — skipping"
    exit 0
fi

# Parser: bash + grep, no yq dependency (CI runner may lack it).
# Format of landmines.toml is fixed (see file), so this is robust.
python3 - "$TOML" <<'PY' > /tmp/landmines.parsed
import sys, re
try:
    import tomllib
except ImportError:
    import tomli as tomllib
with open(sys.argv[1], "rb") as f:
    data = tomllib.load(f)
for lm in data.get("landmine", []):
    globs = "|".join(lm.get("file_globs", []))
    tests = "|".join(lm.get("required_test_paths", []))
    print(f"{lm['id']}\t{globs}\t{tests}")
PY

BASE="${GITHUB_BASE_REF:-origin/main}"
TOUCHED=$(git diff --name-only "$BASE"...HEAD 2>/dev/null || git diff --name-only HEAD)
if [[ -z "$TOUCHED" ]]; then
    TOUCHED=$(git diff --name-only --cached)
fi

FAIL=0
TESTS_TO_RUN=""

while IFS=$'\t' read -r id globs tests; do
    [[ -z "$globs" ]] && continue
    IFS='|' read -ra GARR <<< "$globs"
    HIT=0
    for f in $TOUCHED; do
        for g in "${GARR[@]}"; do
            # Simple shell-glob match (exact path for our current set)
            if [[ "$f" == "$g" ]]; then
                HIT=1
                break
            fi
        done
        [[ $HIT -eq 1 ]] && break
    done
    if [[ $HIT -eq 1 && -n "$tests" ]]; then
        echo "ℹ️  $id triggered (touched file matches glob) — queueing tests"
        IFS='|' read -ra TARR <<< "$tests"
        for t in "${TARR[@]}"; do
            TESTS_TO_RUN+="$t "
        done
    fi
done < /tmp/landmines.parsed

if [[ -n "$TESTS_TO_RUN" ]]; then
    echo "▶ cargo test --workspace -- $TESTS_TO_RUN"
    if ! cargo test --workspace -- $TESTS_TO_RUN; then
        FAIL=1
    fi
fi

if [[ $FAIL -ne 0 ]]; then
    echo "❌ landmine guard failed"
    if [[ "$MODE" == "fail" ]]; then
        exit 1
    else
        echo "ℹ️  WARN-ONLY (M2). Will flip to FAIL at M8."
        exit 0
    fi
fi

echo "✓ landmine gate clean"
exit 0
