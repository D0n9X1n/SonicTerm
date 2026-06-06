#!/usr/bin/env bash
# Legacy compatibility wrapper.
#
# The old hot-file ownership document was removed for SonicTerm 1.0. Ownership is now described
# by crate-local CLAUDE.md files and normal CI runs unit tests only.
set -euo pipefail
echo "check-ownership: skipped (1.0 removed hot-file ownership docs)"
