#!/usr/bin/env bash
# Compare two bench runs side-by-side.
# Usage: scripts/bench_compare.sh before.json after.json
set -euo pipefail
BEFORE="${1:?before.json required}"
AFTER="${2:?after.json required}"

python3 <<EOF
import json
b = json.load(open("$BEFORE"))
a = json.load(open("$AFTER"))
keys = sorted(set(b) | set(a))
print(f"{'metric':40} {'before':>14} {'after':>14} {'Δ':>10}")
print("-" * 82)
for k in keys:
    bv = b.get(k, "—")
    av = a.get(k, "—")
    if isinstance(bv, (int, float)) and isinstance(av, (int, float)) and bv:
        pct = (av - bv) / bv * 100
        marker = "🚀" if pct < -5 else ("⚠️" if pct > 5 else "  ")
        print(f"{k:40} {bv:>14} {av:>14} {pct:>+8.1f}% {marker}")
    else:
        print(f"{k:40} {bv:>14} {av:>14}")
EOF
