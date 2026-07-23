#!/usr/bin/env bash
# Frozen PTY-backend feasibility evidence: emit the canonical `pty-backend-v1`
# document and its SHA-256, and verify/update the frozen digest pinned in
# `crates/sonicterm-io/src/pty_backend_feasibility.rs`.
#
# The Rust module is the single source of truth for the evidence bytes; this
# flat script is the authoritative hasher (system Python `hashlib`) so the crate
# needs no crypto dependency. Modes:
#
#   (default) --check  emit hash, compare to the frozen constant, exit nonzero on drift
#   --emit             print the canonical evidence document to stdout
#   --hash             print only the SHA-256 of the canonical document
#   --write            rewrite the frozen constant in the source to the current hash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

source_file="crates/sonicterm-io/src/pty_backend_feasibility.rs"

if command -v python3 >/dev/null 2>&1; then
  python_bin=python3
elif command -v python >/dev/null 2>&1; then
  python_bin=python
else
  echo "python3 or python is required to hash the feasibility evidence" >&2
  exit 1
fi

emit_evidence() {
  # Emit exactly the canonical bytes; keep cargo's own chatter off stdout.
  cargo run --quiet -p sonicterm-io --example pty_backend_feasibility_evidence 2>/dev/null
}

hash_evidence() {
  emit_evidence | "$python_bin" -c 'import sys,hashlib; sys.stdout.write(hashlib.sha256(sys.stdin.buffer.read()).hexdigest())'
}

frozen_constant() {
  "$python_bin" - "$source_file" <<'PY'
import re, sys
text = open(sys.argv[1], encoding="utf-8").read()
m = re.search(r'FROZEN_EVIDENCE_SHA256:\s*&str\s*=\s*"([0-9a-f]{64})"', text)
sys.stdout.write(m.group(1) if m else "")
PY
}

mode="${1:---check}"
case "$mode" in
  --emit)
    emit_evidence
    ;;
  --hash)
    hash_evidence
    echo
    ;;
  --write)
    computed="$(hash_evidence)"
    "$python_bin" - "$source_file" "$computed" <<'PY'
import re, sys
path, digest = sys.argv[1], sys.argv[2]
text = open(path, encoding="utf-8").read()
new = re.sub(r'(FROZEN_EVIDENCE_SHA256:\s*&str\s*=\s*")[0-9a-f]{64}(")', r'\g<1>' + digest + r'\g<2>', text, count=1)
open(path, "w", encoding="utf-8").write(new)
PY
    echo "froze pty-backend-v1 evidence sha256=$computed"
    ;;
  --check)
    computed="$(hash_evidence)"
    frozen="$(frozen_constant)"
    if [[ "$computed" != "$frozen" ]]; then
      echo "pty-backend feasibility evidence drift:" >&2
      echo "  frozen constant: $frozen" >&2
      echo "  computed now:    $computed" >&2
      echo "Run: bash scripts/pty-backend-feasibility.sh --write" >&2
      exit 1
    fi
    echo "pty-backend-v1 evidence sha256=$computed (matches frozen constant)"
    ;;
  *)
    echo "usage: $0 [--check|--emit|--hash|--write]" >&2
    exit 2
    ;;
esac
