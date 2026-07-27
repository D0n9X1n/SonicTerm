#!/usr/bin/env bash
# Refuses any insertion into `self.windows` outside the single chokepoint that
# registers the window's governor owner.
#
# A window inserted without an owner is absent from hierarchy accounting
# entirely, not merely uncharged, and nothing later recovers it: both
# `reconcile_pane_owners` and `reattribute_pane_owners` skip a window whose
# owner is `None`, so its panes never receive owners either and the periodic
# retention sampler passes over the whole subtree for as long as the window
# lives.
#
# `App::insert_window_registered` makes the insertion and the registration one
# operation, and `register_window_owner` is private so the two cannot drift
# apart. This check enforces what privacy alone cannot: that no site reaches
# past the chokepoint to the map itself. That is exactly how the original gap
# arose — a call site did the insert and simply never registered, and every
# owner test stayed green because none of them could reach the code path.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

CHOKEPOINT="insert_window_registered"

# `crates` without a trailing slash: BSD grep emits `crates//foo` when given
# `crates/`, which would break the path comparisons below.
hits=$(grep -rn --include='*.rs' 'self\.windows\.insert(' crates || true)

if [[ -z "$hits" ]]; then
    echo "check-window-owner-registration: no window insertions found. OK."
    exit 0
fi

fail=0
while IFS= read -r hit; do
    [[ -z "$hit" ]] && continue
    file="${hit%%:*}"
    rest="${hit#*:}"
    line="${rest%%:*}"

    # Walk back from the insertion to the nearest enclosing `fn` and take its
    # name. Anchored on the four-space method indent so a nested closure cannot
    # be mistaken for the enclosing function.
    enclosing=$(awk -v limit="$line" '
        NR <= limit && /^    (pub(\([^)]*\))? )?(async )?fn [a-zA-Z_][a-zA-Z0-9_]*/ {
            match($0, /fn [a-zA-Z_][a-zA-Z0-9_]*/)
            name = substr($0, RSTART + 3, RLENGTH - 3)
        }
        END { print name }
    ' "$file")

    if [[ "$enclosing" != "$CHOKEPOINT" ]]; then
        echo "FORBIDDEN raw window insert: $file:$line (in fn ${enclosing:-<unknown>})"
        echo "  → use App::${CHOKEPOINT}(id, window), which registers the window's"
        echo "    owner at the insertion. A window inserted without one is invisible"
        echo "    to hierarchy accounting for its entire life."
        fail=1
    fi
done <<<"$hits"

if [[ $fail -ne 0 ]]; then
    exit 1
fi
echo "check-window-owner-registration: every window insert goes through ${CHOKEPOINT}. OK."
