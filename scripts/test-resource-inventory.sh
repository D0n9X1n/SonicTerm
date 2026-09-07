#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
GENERATOR="$ROOT/scripts/resource-inventory.py"
OUTPUT="$ROOT/target/v1.2.0-baseline"

if command -v python3 >/dev/null 2>&1; then
  PY=python3
elif command -v python >/dev/null 2>&1; then
  PY=python
else
  echo "python3 or python is required to generate the resource inventory" >&2
  exit 1
fi

rm -rf "$OUTPUT"
"$PY" "$GENERATOR"

"$PY" - "$GENERATOR" <<'PY'
import runpy
import sys

inventory = runpy.run_path(sys.argv[1])
validate = inventory["validate_rows"]
root = inventory["repository_root"]()
for column, value in ((3, "WP-INVENTED owner"), (11, "WP-INVENTED")):
    rows = list(inventory["ROWS"])
    row = list(rows[0])
    row[column] = value
    rows[0] = tuple(row)
    validate.__globals__["ROWS"] = tuple(rows)
    try:
        validate(root)
    except ValueError as error:
        assert "is not a formal package: WP-INVENTED" in str(error), error
    else:
        raise AssertionError(f"invented package was accepted in column {column}")
PY

first_hashes="$("$PY" - "$OUTPUT" <<'PY'
import hashlib
import pathlib
import sys

output = pathlib.Path(sys.argv[1])
for path in sorted(output.iterdir()):
    print(f"{path.name}:{hashlib.sha256(path.read_bytes()).hexdigest()}")
PY
)"

"$PY" "$GENERATOR"
second_hashes="$("$PY" - "$OUTPUT" <<'PY'
import hashlib
import pathlib
import sys

output = pathlib.Path(sys.argv[1])
for path in sorted(output.iterdir()):
    print(f"{path.name}:{hashlib.sha256(path.read_bytes()).hexdigest()}")
PY
)"

test "$first_hashes" = "$second_hashes"
"$PY" "$GENERATOR" --check

"$PY" - "$OUTPUT" <<'PY'
import hashlib
import json
import pathlib
import sys

output = pathlib.Path(sys.argv[1])
manifest = {}
for line in (output / "resource-inventory.sha256").read_text(encoding="utf-8").splitlines():
    digest, filename = line.split("  ", 1)
    manifest[filename] = digest

for filename in ("resource-inventory.json", "resource-inventory.md"):
    actual = hashlib.sha256((output / filename).read_bytes()).hexdigest()
    assert manifest.get(filename) == actual, (filename, manifest.get(filename), actual)

document = json.loads((output / "resource-inventory.json").read_text(encoding="utf-8"))
assert document["row_count"] == 5
assert len(document["schema"]) == 12
assert len(document["rows"]) == 5
PY

printf '\n' >> "$OUTPUT/resource-inventory.md"
if "$PY" "$GENERATOR" --check >/dev/null 2>&1; then
    printf 'resource-inventory.py --check accepted stale output\n' >&2
    exit 1
fi

"$PY" "$GENERATOR"
"$PY" "$GENERATOR" --check
