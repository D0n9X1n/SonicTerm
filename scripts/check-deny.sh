#!/usr/bin/env bash
# Wrapper for cargo-deny that the local gate (CLAUDE.md §2) and the future
# CI workflow both call. Keeping the invocation in a script lets us tune
# flags in one place without touching .github/workflows/ (currently being
# refactored in a parallel PR).
set -euo pipefail

if ! command -v cargo-deny >/dev/null 2>&1; then
    echo "cargo-deny not installed. Run: cargo install cargo-deny --locked" >&2
    exit 127
fi

exec cargo deny --workspace check --hide-inclusion-graph "$@"
