#!/usr/bin/env bash
# Local gate for scripts/soak-harness.py: unit tests plus an end-to-end control
# run. This is the documented cross-platform control gate for #867 (WP-SOAK) on
# Unix-like hosts; on Windows run the same checks via the unit module directly:
#   python -m unittest -v soak-harness_tests   (from the scripts/ directory)
# The unit suite embeds the cross-platform determinism assertions (pinned golden
# SHA-256 and byte-for-byte golden-file match), so both paths verify the same
# contract. The control scenario is fully synthetic and virtual-clocked, so it
# is bounded (<1s) and free of wall-clock timing assumptions.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
HARNESS="$ROOT/scripts/soak-harness.py"
GOLDEN="$ROOT/scripts/soak-harness.golden.json"

if command -v python3 >/dev/null 2>&1; then
  PY=python3
elif command -v python >/dev/null 2>&1; then
  PY=python
else
  echo "python3 or python is required to run the soak harness" >&2
  exit 1
fi

TMP="$(mktemp -d "${TMPDIR:-/tmp}/sonic-soak.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT

# 1. Byte-compile the harness and its tests (cheap syntax gate).
"$PY" -m py_compile "$HARNESS" "$ROOT/scripts/soak-harness_tests.py"

# 2. Unit suite (includes deterministic slope/plateau and cross-platform
#    golden-hash assertions).
( cd "$ROOT/scripts" && "$PY" soak-harness_tests.py )

# 3. Control run produces a well-formed, ok-status artifact.
"$PY" "$HARNESS" --scenario control --out "$TMP/control.json"
"$PY" - "$TMP/control.json" <<'PYEOF'
import json, sys
result = json.load(open(sys.argv[1], encoding="utf-8"))
assert result["schema_version"] == "soak-harness/1", result["schema_version"]
assert result["status"] == "ok", result["status"]
assert result["scenario"] == "control", result["scenario"]
assert result["sample_count"] == 64, result["sample_count"]
rss = result["analysis"]["fields"]["rss_bytes"]
assert rss["available"] is True
assert rss["plateau_reached"] in (True, False)
print("control artifact OK: {} samples".format(result["sample_count"]))
PYEOF

# 4. Determinism: two canonical runs must be byte-identical to each other and to
#    the committed golden reference (the shared cross-OS fixture).
"$PY" "$HARNESS" --scenario control --canonical --out "$TMP/a.json"
"$PY" "$HARNESS" --scenario control --canonical --out "$TMP/b.json"
cmp "$TMP/a.json" "$TMP/b.json"
cmp "$TMP/a.json" "$GOLDEN"
echo "determinism OK: canonical runs match committed golden"

# 5. Hard-stop path still emits an artifact and reports the ceiling reason.
"$PY" "$HARNESS" --scenario control --max-samples 4 --out "$TMP/stop.json"
"$PY" - "$TMP/stop.json" <<'PYEOF'
import json, sys
result = json.load(open(sys.argv[1], encoding="utf-8"))
assert result["status"] == "hard_stop", result["status"]
assert result["stop_reason"] == "max_samples", result["stop_reason"]
assert result["sample_count"] == 4, result["sample_count"]
print("hard-stop artifact OK: {}".format(result["stop_reason"]))
PYEOF

# 6. --fail-on-hard-stop maps a ceiling stop to a non-zero exit (3).
set +e
"$PY" "$HARNESS" --scenario control --max-samples 4 --fail-on-hard-stop \
  --out "$TMP/stop2.json" >/dev/null
code=$?
set -e
if [[ "$code" -ne 3 ]]; then
  echo "expected exit 3 for --fail-on-hard-stop, got $code" >&2
  exit 1
fi
echo "fail-on-hard-stop exit code OK: $code"

echo "soak-harness.sh test passed"
