#!/usr/bin/env bash
# Legacy compatibility wrapper.
#
# SonicTerm 1.0 removed the old contract-doc gate and normal CI now runs unit tests only.
# Keep this script as a no-op so older local workflows do not fail.
set -euo pipefail
echo "check-contract-docs: skipped (1.0 uses unit tests + crate CLAUDE docs)"
