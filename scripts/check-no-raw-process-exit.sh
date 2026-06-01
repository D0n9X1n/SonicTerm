#!/usr/bin/env bash
# Refuses raw `std::process::exit(` calls in production crate sources.
# All shipped binaries must funnel through `sonicterm_logging::exit_with(code, reason)`
# so the exit reason lands in sonicterm.log. See CLAUDE.md "exit and crash coverage"
# and `crates/sonicterm-logging/src/exit_trace.rs`.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

ALLOWLIST="scripts/process-exit-allowlist.txt"

# Find every occurrence in crates/, then strip allowlisted files.
# Note: macOS BSD grep emits `crates//foo/...` (double slash) when given
# `crates/` as the path arg; pass `crates` (no trailing /) so paths come
# out as `crates/foo/...` consistently with the allowlist prefixes.
hits=$(grep -rn --include='*.rs' 'std::process::exit\|process::exit(' crates || true)

if [[ -z "$hits" ]]; then
    echo "check-no-raw-process-exit: 0 occurrences. OK."
    exit 0
fi

# Build a regex of allowlist patterns (one path prefix per line, # = comment).
patterns=$(grep -v -E '^\s*(#|$)' "$ALLOWLIST" 2>/dev/null || true)

fail=0
while IFS= read -r line; do
    file="${line%%:*}"
    allowed=0
    while IFS= read -r pat; do
        [[ -z "$pat" ]] && continue
        if [[ "$file" == $pat* ]]; then
            allowed=1
            break
        fi
    done <<<"$patterns"
    # Also allow the canonical helper that wraps process::exit by design.
    if [[ "$file" == "crates/sonicterm-logging/src/exit_trace.rs" ]]; then
        allowed=1
    fi
    if [[ $allowed -eq 0 ]]; then
        echo "FORBIDDEN raw process::exit: $line"
        echo "  → use sonicterm_logging::exit_with(code, reason) or add the file to $ALLOWLIST with justification."
        fail=1
    fi
done <<<"$hits"

if [[ $fail -ne 0 ]]; then
    exit 1
fi
echo "check-no-raw-process-exit: all occurrences allowlisted. OK."
