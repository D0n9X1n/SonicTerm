#!/usr/bin/env bash
# Regression test for issue #497 — tesseract missing → SKIP not FAIL.
# Bash port of Test-OcrSkip.ps1 (PR #498). Drives the embedded Python
# evaluator from run_case.sh with synthesized inputs.
set -uo pipefail
REPO_ROOT=$(cd "$(dirname "$0")/../.." && pwd)
SANDBOX=$(mktemp -d -t sonic-497-XXXXXX); trap 'rm -rf "$SANDBOX"' EXIT
PY_FILE="$SANDBOX/expect_eval.py"
awk '/^python3 - "\$CASE_JSON" "\$SHOT" "\$EXPECT_LOG" "\$CASE_OUT" <<.PY.$/{f=1;next} /^PY$/{f=0} f' "$REPO_ROOT/testing/workflows/run_case.sh" > "$PY_FILE"
[[ -s "$PY_FILE" ]] || { echo "FAIL: extract python"; exit 1; }
PNG="$SANDBOX/fake.png"
python3 -c "from PIL import Image; Image.new('RGBA',(1000,700),(0,0,0,255)).save('$PNG')"
fails=0
run_eval() {
  local cj="$SANDBOX/c.json" el="$SANDBOX/e.log"
  printf '%s' "$1" > "$cj"; rm -f "$el"
  SONICTERM_HARNESS_OCR_AVAILABLE="$2" python3 "$PY_FILE" "$cj" "$PNG" "$el" "$SANDBOX" >/dev/null 2>&1
  local rc=$?; printf '%s|%s' "$rc" "$([[ -f $el ]] && cat $el)"
}
ck() { if [[ "$1" == "$2" ]]; then echo "  PASS: $3"; else echo "  FAIL: $3 (want '$2' got '$1')"; fails=$((fails+1)); fi; }
echo "[1/4] OCR-only + no tesseract -> 77"
out=$(run_eval '{"id":"a","expect":[{"kind":"text-in-region","value":"x"},{"kind":"ocr-text","value":"y"}]}' "0")
ck "${out%%|*}" "77" "all-OCR + no-OCR -> 77"
echo "${out#*|}" | grep -q $'^SKIP\ttext-in-region' && echo "  PASS: SKIP line present" || { echo "  FAIL: SKIP missing"; fails=$((fails+1)); }
echo "[2/4] mixed pixel-pass + OCR-skip -> 77"
out=$(run_eval '{"id":"b","expect":[{"kind":"pixel-near","x":100,"y":100,"rgba":[0,0,0,255],"tolerance":10},{"kind":"text-in-region","value":"x"}]}' "0")
ck "${out%%|*}" "77" "pixel-pass + OCR-skip -> 77"
echo "${out#*|}" | grep -q $'^PASS\tpixel-near' && echo "  PASS: PASS pixel-near" || { echo "  FAIL: PASS missing"; fails=$((fails+1)); }
echo "[3/4] mixed pixel-fail + OCR-skip -> 1 (fail wins)"
out=$(run_eval '{"id":"c","expect":[{"kind":"pixel-near","x":100,"y":100,"rgba":[255,255,255,255],"tolerance":5},{"kind":"text-in-region","value":"x"}]}' "0")
ck "${out%%|*}" "1" "pixel-fail + OCR-skip -> 1"
echo "[4/4] OCR available, pixel-only pass -> 0"
out=$(run_eval '{"id":"d","expect":[{"kind":"pixel-near","x":100,"y":100,"rgba":[0,0,0,255],"tolerance":10}]}' "1")
ck "${out%%|*}" "0" "OCR-avail + pixel pass -> 0"
echo "[preflight] PATH=/nonexistent -> tesseract missing"
if PATH=/nonexistent command -v tesseract >/dev/null 2>&1; then av=1; else av=0; fi
ck "$av" "0" "PATH=/nonexistent -> not found"
(( fails > 0 )) && { echo; echo "FAILED: $fails"; exit 1; }; echo; echo "ALL PASS"
