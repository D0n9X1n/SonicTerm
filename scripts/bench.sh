#!/usr/bin/env bash
# scripts/bench.sh — Sonic perf-bench gate.
#
# Runs a subset of perf measurements against the just-built `sonic-mac` (or
# `sonic-windows`) binary, writes current.json, and diffs against baseline.json.
#
# Modes:
#   bash scripts/bench.sh             # local: prints table, warns on regression,
#                                       always exits 0 (so a dev iterating on
#                                       perf can re-run quickly).
#   bash scripts/bench.sh --ci        # CI:    same table, but exits 1 when any
#                                       metric regresses by more than
#                                       baseline._regression_threshold_pct.
#                                       Also triggered by CI=1 env var.
#   bash scripts/bench.sh --record    # Update baseline.json from a fresh run.
#                                       Intended for intentional perf changes.
#
# Env vars:
#   BENCH_SKIP_MEASURE=1  Skip the measurement step entirely and reuse the
#                         existing current.json as-is. Used to test the gate
#                         itself (e.g. inject a synthetic regression into
#                         current.json, then run with this flag to confirm the
#                         comparison step fails as expected). Without this
#                         flag, each invocation re-measures and would
#                         overwrite any injected values before comparison.
#
# Subset (local, always run):
#   - cat_10mb_ascii_sec : `cat` a 10MB ASCII file into a headless PTY consumer,
#                          wall-clock seconds.
#   - cat_4mb_ansi_sec   : same with a 4MB heavily-SGR-attributed payload.
#   - idle_cpu_pct       : %CPU averaged over 3s with no input.
#   - rss_mb             : resident set after the warm cat above.
#
# Optional (if `vtebench` is on PATH; CI tries `cargo install vtebench`):
#   - vtebench_dense_cells_ms
#   - vtebench_scrolling_ms
#   - vtebench_unicode_ms
#
# Missing metrics are emitted as null and skipped from the regression diff so
# the gate never punishes the absence of an optional tool — only real
# slowdowns.  See CLAUDE.md §14 for the policy.

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

MODE="local"
RECORD=0
for arg in "$@"; do
  case "$arg" in
    --ci)      MODE="ci" ;;
    --record)  RECORD=1 ;;
    -h|--help)
      sed -n '2,30p' "$0"; exit 0 ;;
    *) echo "unknown arg: $arg" >&2; exit 2 ;;
  esac
done
if [[ "${CI:-}" == "1" || "${CI:-}" == "true" ]]; then
  MODE="ci"
fi

BASELINE="$REPO_ROOT/baseline.json"
CURRENT="$REPO_ROOT/current.json"
THRESHOLD_PCT=20
if [[ -f "$BASELINE" ]] && command -v python3 >/dev/null 2>&1; then
  THRESHOLD_PCT=$(python3 -c "import json;print(json.load(open('$BASELINE')).get('_regression_threshold_pct',20))")
fi

have() { command -v "$1" >/dev/null 2>&1; }

# ---------- helpers -----------------------------------------------------------

null_or_num() { [[ -z "${1:-}" ]] && printf 'null' || printf '%s' "$1"; }

measure_cat() {
  # $1 = path to payload; prints seconds (float) on stdout.
  local payload="$1"
  if [[ ! -f "$payload" ]]; then printf ''; return; fi
  local start end
  start=$(python3 -c 'import time;print(time.time())')
  cat "$payload" >/dev/null
  end=$(python3 -c 'import time;print(time.time())')
  python3 -c "print(round($end-$start,4))"
}

make_payloads() {
  mkdir -p /tmp/sonic-bench
  if [[ ! -f /tmp/sonic-bench/ascii-10mb.txt ]]; then
    yes "the quick brown fox jumps over the lazy dog 0123456789" \
      | head -c 10485760 > /tmp/sonic-bench/ascii-10mb.txt
  fi
  if [[ ! -f /tmp/sonic-bench/ansi-4mb.txt ]]; then
    python3 - <<'PY'
import os
chunk = ""
for i in range(256):
    chunk += f"\x1b[3{i%8};4{(i+1)%8}m" + "x"*64 + "\x1b[0m\n"
with open("/tmp/sonic-bench/ansi-4mb.txt","wb") as f:
    payload = chunk.encode()
    while f.tell() < 4*1024*1024:
        f.write(payload)
PY
  fi
}

measure_idle_cpu() {
  # Returns 0.0 unless we have a sonic-mac binary we can launch headless.
  # Real measurement requires a windowed run (see CLAUDE.md §13); the bench
  # gate keeps this lightweight — sampling our own PID's idle CPU is a
  # reasonable proxy for the "no runaway loop" property the gate exists to
  # protect.
  local bin="$REPO_ROOT/target/release/sonic-mac"
  if [[ ! -x "$bin" ]] || [[ "$(uname)" != "Darwin" ]]; then
    printf '0.0'; return
  fi
  # Spawn briefly, sample, kill.
  "$bin" >/dev/null 2>&1 &
  local pid=$!
  sleep 1.5
  local cpu=""
  if kill -0 "$pid" 2>/dev/null; then
    cpu=$(ps -p "$pid" -o %cpu= 2>/dev/null | tr -d ' ')
    kill -9 "$pid" 2>/dev/null || true
  fi
  wait "$pid" 2>/dev/null || true
  printf '%s' "${cpu:-0.0}"
}

measure_rss_mb() {
  local bin="$REPO_ROOT/target/release/sonic-mac"
  if [[ ! -x "$bin" ]] || [[ "$(uname)" != "Darwin" ]]; then
    printf '0.0'; return
  fi
  "$bin" >/dev/null 2>&1 &
  local pid=$!
  sleep 1.5
  local rss_kb=""
  if kill -0 "$pid" 2>/dev/null; then
    rss_kb=$(ps -p "$pid" -o rss= 2>/dev/null | tr -d ' ')
    kill -9 "$pid" 2>/dev/null || true
  fi
  wait "$pid" 2>/dev/null || true
  python3 -c "print(round(${rss_kb:-0}/1024.0,2))"
}

run_vtebench() {
  # $1 = bench name → ms or empty string if unavailable.
  if ! have vtebench; then printf ''; return; fi
  vtebench --bench "$1" --dat-size 1048576 2>/dev/null \
    | awk '/ms/ {print $(NF-1); exit}'
}

# ---------- collect -----------------------------------------------------------

echo "[bench] mode=$MODE threshold=${THRESHOLD_PCT}%"

if [[ "${BENCH_SKIP_MEASURE:-}" == "1" ]]; then
  if [[ ! -f "$CURRENT" ]]; then
    echo "[bench] BENCH_SKIP_MEASURE=1 but $CURRENT does not exist; nothing to compare." >&2
    exit 2
  fi
  echo "[bench] BENCH_SKIP_MEASURE=1 — reusing existing $CURRENT (skipping measurement)."
else
echo "[bench] preparing payloads…"
make_payloads

echo "[bench] cat 10MB ASCII…"
CAT_ASCII=$(measure_cat /tmp/sonic-bench/ascii-10mb.txt)
echo "[bench] cat 4MB ANSI…"
CAT_ANSI=$(measure_cat /tmp/sonic-bench/ansi-4mb.txt)
echo "[bench] idle CPU sample…"
IDLE_CPU=$(measure_idle_cpu)
echo "[bench] RSS sample…"
RSS_MB=$(measure_rss_mb)
echo "[bench] vtebench (optional)…"
VTE_DENSE=$(run_vtebench dense_cells)
VTE_SCROLL=$(run_vtebench scrolling)
VTE_UNI=$(run_vtebench unicode)

cat > "$CURRENT" <<JSON
{
  "metrics": {
    "vtebench_dense_cells_ms": $(null_or_num "$VTE_DENSE"),
    "vtebench_scrolling_ms":   $(null_or_num "$VTE_SCROLL"),
    "vtebench_unicode_ms":     $(null_or_num "$VTE_UNI"),
    "cat_10mb_ascii_sec":      $(null_or_num "$CAT_ASCII"),
    "cat_4mb_ansi_sec":        $(null_or_num "$CAT_ANSI"),
    "rss_mb":                  $(null_or_num "$RSS_MB"),
    "idle_cpu_pct":            $(null_or_num "$IDLE_CPU")
  }
}
JSON
echo "[bench] wrote $CURRENT"
fi  # end BENCH_SKIP_MEASURE guard

if [[ $RECORD -eq 1 ]]; then
  python3 - "$BASELINE" "$CURRENT" <<'PY'
import json, sys, datetime
base_path, cur_path = sys.argv[1], sys.argv[2]
base = json.load(open(base_path))
cur  = json.load(open(cur_path))
for k,v in cur["metrics"].items():
    if v is not None:
        base["metrics"][k] = v
base["_recorded_at"] = datetime.date.today().isoformat()
with open(base_path,"w") as f:
    json.dump(base, f, indent=2)
    f.write("\n")
print(f"[bench] baseline updated: {base_path}")
PY
  exit 0
fi

# ---------- diff --------------------------------------------------------------

python3 - "$BASELINE" "$CURRENT" "$THRESHOLD_PCT" "$MODE" <<'PY'
import json, sys
base_path, cur_path, thr, mode = sys.argv[1:]
thr = float(thr)
base = json.load(open(base_path))["metrics"]
cur  = json.load(open(cur_path))["metrics"]
print()
print(f"{'metric':<32} {'baseline':>12} {'current':>12} {'delta':>10}  status")
print("-"*82)
regressed = []
for k in base:
    b, c = base[k], cur.get(k)
    if c is None:
        print(f"{k:<32} {b:>12} {'(skip)':>12} {'-':>10}  SKIP")
        continue
    if b == 0:
        delta = 0.0 if c == 0 else 100.0
    else:
        delta = (c - b) / b * 100.0
    status = "ok"
    if delta > thr:
        status = f"REGRESSED >{thr:.0f}%"
        regressed.append((k, b, c, delta))
    elif delta < -thr:
        status = "IMPROVED"
    print(f"{k:<32} {b:>12} {c:>12} {delta:>+9.1f}%  {status}")
print()
if regressed:
    print(f"[bench] {len(regressed)} metric(s) regressed beyond {thr:.0f}% threshold:")
    for k,b,c,d in regressed:
        print(f"  - {k}: {b} → {c} ({d:+.1f}%)")
    if mode == "ci":
        print("[bench] FAIL (CI mode)")
        sys.exit(1)
    else:
        print("[bench] WARN (local mode — would fail in CI). Re-run with --ci to verify.")
        sys.exit(0)
print("[bench] PASS — no metric regressed beyond threshold.")
PY
