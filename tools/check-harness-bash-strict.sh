#!/usr/bin/env bash
# Lint gate for testing/workflows/*.sh AND scripts/*.sh: every `"${ARR[@]}"`
# expansion under `set -u` (bash 3.2 on macOS) must either be guarded with a
# `:-` default or be annotated with a `# harness-safe-empty:` sentinel
# justifying why the array is provably non-empty at that point.
# See issues #520 and #473.
set -euo pipefail

bash -n testing/workflows/*.sh
bash -n scripts/*.sh

hits=$(grep -nE '"\$\{[A-Za-z_]+\[@\]\}"' testing/workflows/*.sh scripts/*.sh \
  | grep -v 'harness-safe-empty' \
  | grep -v ':-}' || true)

if [[ -n "$hits" ]]; then
  echo "Unguarded array expansion in testing/workflows/ or scripts/ — add :- guard or harness-safe-empty sentinel:"
  echo "$hits"
  exit 1
fi
