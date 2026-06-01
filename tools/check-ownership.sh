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

CODEOWNERS_FAILED=0
if [[ ${#UNOWNED[@]} -eq 0 ]]; then
    echo "✓ all touched files routed by CODEOWNERS"
else
    echo "❌ files touched but not routed by CODEOWNERS:"
    for f in "${UNOWNED[@]}"; do
        echo "  - $f"
    done
    if [[ "$MODE" == "fail" ]]; then
        CODEOWNERS_FAILED=1
    else
        echo "ℹ️  WARN-ONLY (M5). Will flip to FAIL at M8."
    fi
fi
# Hot-file 2-PM co-sign + dev:* label discipline.
#
# Both checks need GitHub PR metadata; if `gh` is missing or we're not
# inside a PR context (no PR_NUMBER), they are skipped with a warning.
# ------------------------------------------------------------------

check_hot_files_and_labels() {
    local pr_number="${PR_NUMBER:-${GITHUB_PR_NUMBER:-}}"
    if [[ -z "$pr_number" ]]; then
        # Try to infer from current branch via gh.
        if command -v gh >/dev/null 2>&1; then
            pr_number=$(gh pr view --json number --jq .number 2>/dev/null || true)
        fi
    fi

    if [[ -z "$pr_number" ]] || ! command -v gh >/dev/null 2>&1; then
        echo "ℹ️  PR metadata unavailable — skipping hot-file co-sign + dev:* label checks"
        return 0
    fi

    local hot_doc=docs/HOT_FILES.md
    local pr_json
    pr_json=$(gh pr view "$pr_number" --json labels,commits,body 2>/dev/null || true)
    if [[ -z "$pr_json" ]]; then
        echo "ℹ️  could not fetch PR #$pr_number metadata — skipping"
        return 0
    fi

    local errs=0

    # --- dev:* label discipline: exactly one dev:* label ---
    local dev_labels
    dev_labels=$(echo "$pr_json" | jq -r '.labels[].name' | grep -c '^dev:' || true)
    if [[ "$dev_labels" -ne 1 ]]; then
        echo "❌ PR #$pr_number has $dev_labels dev:* labels (must be exactly 1)"
        errs=$((errs + 1))
    fi

    # --- hot-file 2-PM co-sign ---
    if [[ -f "$hot_doc" ]]; then
        # Extract hot-file paths from the markdown table (lines matching `crates/...`).
        local hot_files
        hot_files=$(grep -oE '`crates/[^`]+`' "$hot_doc" | tr -d '`' | sort -u)
        local touched_hot=()
        for f in $TOUCHED; do
            while IFS= read -r hf; do
                [[ "$f" == "$hf" ]] && touched_hot+=("$f")
            done <<< "$hot_files"
        done
        if [[ ${#touched_hot[@]} -gt 0 ]]; then
            local has_mac has_win has_requested_by
            has_mac=$(echo "$pr_json" | jq -r '.commits[].messageBody, .commits[].messageHeadline, .body' \
                | grep -c -iE 'Co-Authored-By:.*claude.*\(.*mac.*\)|Co-Authored-By:.*mac-PM' || true)
            has_win=$(echo "$pr_json" | jq -r '.commits[].messageBody, .commits[].messageHeadline, .body' \
                | grep -c -iE 'Co-Authored-By:.*claude.*\(.*win.*\)|Co-Authored-By:.*win-PM' || true)
            has_requested_by=$(echo "$pr_json" | jq -r '.body' \
                | grep -c -iE '^Requested by:[[:space:]]*@' || true)
            if [[ "$has_mac" -lt 1 || "$has_win" -lt 1 ]] && [[ "$has_requested_by" -lt 1 ]]; then
                echo "❌ PR #$pr_number touches hot files (${touched_hot[*]})"
                echo "   but lacks both dev:mac AND dev:win Co-Authored-By trailers"
                echo "   and lacks a 'Requested by: @other-PM' line in the body."
                echo "   See docs/HOT_FILES.md §15 for the 2-PM co-sign rule."
                errs=$((errs + 1))
            fi
        fi
    fi

    if [[ "$errs" -gt 0 ]]; then
        if [[ "$MODE" == "fail" ]]; then
            return 1
        else
            echo "ℹ️  WARN-ONLY (M5). Will flip to FAIL at M8."
            return 0
        fi
    fi
    return 0
}

# Original gate (CODEOWNERS routing) ran inline above. Now run the
# additional gates and combine exit status.
hot_status=0
check_hot_files_and_labels || hot_status=$?
if [[ "$CODEOWNERS_FAILED" -ne 0 || "$hot_status" -ne 0 ]]; then
    exit 1
fi
exit 0
