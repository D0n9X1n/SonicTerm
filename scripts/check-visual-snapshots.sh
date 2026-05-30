#!/usr/bin/env bash
# Runs visual snapshot tests. If they fail (drift), prints which scenes
# drifted + by how many hamming bits, and reminds the user to either
# (a) UPDATE_SNAPSHOTS=1 if intentional change, or (b) investigate as
# render regression.
set -e
if ! cargo test -p sonic-shared --test visual_snapshot 2>&1; then
  echo ""
  echo "Visual snapshot drift detected. Two options:"
  echo "1. Intentional rendering change: UPDATE_SNAPSHOTS=1 cargo test -p sonic-shared --test visual_snapshot"
  echo "   (refresh baselines + commit the .hash files)"
  echo "2. Real regression: investigate before merging — drift could mask P0 like #284 (glyph blur)."
  exit 1
fi
echo "Visual snapshots match baselines (no drift)."
