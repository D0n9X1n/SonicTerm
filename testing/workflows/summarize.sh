#!/usr/bin/env bash
# Summarize a results dir into report.md.
# Usage: summarize.sh <out-dir>
set -euo pipefail
OUT="${1:?out dir required}"

pass=0; fail=0; skip=0; total=0
rows=""
for d in "$OUT"/*/; do
  [[ -d "$d" ]] || continue
  id="$(basename "$d")"
  total=$((total+1))
  status="UNKNOWN"
  [[ -f "$d/status" ]] && status="$(cat "$d/status")"
  case "$status" in
    PASS) pass=$((pass+1)) ;;
    FAIL) fail=$((fail+1)) ;;
    *)    skip=$((skip+1)) ;;
  esac
  shot=""
  [[ -f "$d/screen.png" ]] && shot="$d/screen.png"
  rows+="| $status | $id | $shot |"$'\n'
done

cat <<EOF
# Visual test report

- dir: $OUT
- total: $total
- pass: $pass
- fail: $fail
- skip: $skip

| status | case | screenshot |
|---|---|---|
$rows
EOF
