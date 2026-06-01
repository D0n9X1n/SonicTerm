#!/usr/bin/env bash
# tools/check-ownership.sh
#
# Verify that every file touched in the PR diff matches at least one
# entry in .github/CODEOWNERS. Catches new top-level directories or
# crates that were added without routing them to an owner.
#
# Status: M5 = warn-only. M8 flips to fail.

set -u

MODE="${1:-warn}"
CO=.github/CODEOWNERS

if [[ ! -f "$CO" ]]; then
    echo "ℹ️  $CO missing — skipping"
    exit 0
fi

BASE="${GITHUB_BASE_REF:-origin/main}"
TOUCHED=$(git diff --name-only "$BASE"...HEAD 2>/dev/null || git diff --name-only HEAD)
[[ -z "$TOUCHED" ]] && TOUCHED=$(git diff --name-only --cached)

if [[ -z "$TOUCHED" ]]; then
    echo "✓ no files touched"
    exit 0
fi

# Pre-parse CODEOWNERS: extract patterns (skip blanks and comments).
PATTERNS=()
while IFS= read -r line; do
    line="${line%%#*}"
    line="${line#"${line%%[![:space:]]*}"}"   # ltrim
    [[ -z "$line" ]] && continue
    pat="${line%% *}"
    PATTERNS+=("$pat")
done < "$CO"

# Match function: CODEOWNERS uses glob-like patterns with some
# git-attr semantics; we approximate:
#   `*`               → matches anything
#   `/foo/bar`        → matches files under `foo/bar/`
#   `/foo/bar.rs`     → exact match
#   `*.ext`           → any file with that extension at any depth
match_file() {
    local file="$1" pat
    for pat in "${PATTERNS[@]}"; do
        if [[ "$pat" == "*" ]]; then
            return 0
        fi
        # leading slash = repo-root anchored
        if [[ "$pat" == /* ]]; then
            local rel="${pat#/}"
            if [[ "$pat" == */ ]] || [[ "$pat" == */* && "$pat" != *.* ]]; then
                # directory prefix
                local prefix="${rel%/}"
                if [[ "$file" == "$prefix"/* || "$file" == "$prefix" ]]; then
                    return 0
                fi
            else
                # exact file
                if [[ "$file" == "$rel" ]]; then
                    return 0
                fi
            fi
        else
            # bare glob like *.toml — match basename
            local base="${file##*/}"
            # shellcheck disable=SC2053
            if [[ "$base" == $pat ]]; then
                return 0
            fi
        fi
    done
    return 1
}

UNOWNED=()
for f in $TOUCHED; do
    if ! match_file "$f"; then
        UNOWNED+=("$f")
    fi
done

if [[ ${#UNOWNED[@]} -eq 0 ]]; then
    echo "✓ all touched files routed by CODEOWNERS"
    exit 0
fi

echo "❌ files touched but not routed by CODEOWNERS:"
for f in "${UNOWNED[@]}"; do
    echo "  - $f"
done

if [[ "$MODE" == "fail" ]]; then
    exit 1
else
    echo "ℹ️  WARN-ONLY (M5). Will flip to FAIL at M8."
    exit 0
fi
