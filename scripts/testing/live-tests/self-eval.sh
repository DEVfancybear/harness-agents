#!/usr/bin/env bash
# The self-evaluation (docs/architecture/SELF_EVALUATION.en.md), with a real
# model. Every check fails on 0.8.5, which had no self-evaluation.
#
# A turn that changed three files and ran its check is judged at its end, once,
# and the judgment is kept; a turn the verify reminder sent back is not judged
# too; a one-line change is not judged; HA_SELF_EVAL=off judges nothing;
# `/self-eval` prints ECC's report for the last turn, and `ha learning report`
# shows the last 30 days.
#
#   bash scripts/testing/live-tests/self-eval.sh
set -u
source "$(dirname "$0")/env.sh"
fresh self-eval
export HA_AUTO_REFINE=off
D=$HA_HOME/data
LOGFILE=$D/harness/evaluations.jsonl
P=$RX/p
project "$P"

run_exec() {
  local name="$1"; shift
  printf '\n## %s (ha exec %s)\n\n> %s\n\n' "$(date +%H:%M:%S)" "$name" "${*: -1}" >> "$LOG"
  (cd "$P" && "$HA" exec --model "$MODEL" "${TH[@]}" --approval full-auto --output-format json "$@" >"$RX/$name.json" 2>"$RX/$name.err")
  "$PY" -c "import json,sys; print(json.load(open(sys.argv[1],encoding='utf-8')).get('response',''))" "$RX/$name.json" >> "$LOG" 2>/dev/null
}
# Everything the model was sent in the run's requests, tool results included.
sent() { "$PY" - "$D" <<'EOF'
import glob, os, sqlite3, sys
sys.stdout.reconfigure(encoding="utf-8")
for db in glob.glob(os.path.join(sys.argv[1], "projects", "*", "harness.sqlite3")):
    for (text,) in sqlite3.connect(f"file:{db}?mode=ro", uri=True).execute("select request_json from frozen_requests order by rowid"):
        print(text)
EOF
}
evaluations() { [ -f "$LOGFILE" ] && grep -c . "$LOGFILE" || echo 0; }
# The newest evaluation's field: `latest trigger`.
latest() { tail -1 "$LOGFILE" | "$PY" -c "import json,sys; print(json.load(sys.stdin).get(sys.argv[1]))" "$1"; }

echo "################ 1. a turn that changed three files is judged at its end, once"
run_exec e1 "Tạo ba file Python trong thư mục calc: calc/add.py (hàm add(a, b) trả về a + b), calc/sub.py (hàm sub(a, b) trả về a - b), calc/mul.py (hàm mul(a, b) trả về a * b). Sau khi tạo xong, chạy đúng lệnh: python -c \"import sys; sys.path.insert(0, 'calc'); import add, sub, mul; assert add.add(2, 3) == 5 and sub.sub(5, 3) == 2 and mul.mul(2, 3) == 6; print('CALC-OK')\" và báo kết quả."
check "the three files were made and checked" '[ -f "$P/calc/add.py" ] && [ -f "$P/calc/sub.py" ] && [ -f "$P/calc/mul.py" ] && sent | grep -q "CALC-OK"'
check "one evaluation was kept" '[ "$(evaluations)" -eq 1 ]'
check "judged because it changed three files" '[ "$(latest trigger)" = files ]'
check "its scores are five valid axes" '[ "$(latest valid)" = True ] && tail -1 "$LOGFILE" | "$PY" -c "import json,sys; s=json.load(sys.stdin)[\"scores\"]; sys.exit(0 if len(s)==5 and all(1<=v<=5 for v in s.values()) else 1)"'
check "the diagnostics saw it" 'grep -rh "\"self_eval\"" "$D/diagnostics" 2>/dev/null | grep -q "\"evaluated\""'
weak=$(latest weakest); score=$(tail -1 "$LOGFILE" | "$PY" -c "import json,sys; d=json.load(sys.stdin); print(min(d['scores'].values()))")
echo "   weakest: $weak $score, sent back: $(latest sent_back)"
check "a weak axis sent the turn back, a sound one did not" 'if [ "$score" -le 3 ]; then sent | grep -q "Before you finish: a self-evaluation of this turn scored"; else ! sent | grep -q "Before you finish: a self-evaluation"; fi'

echo "################ 2. a turn the verify reminder sent back is not judged too"
before=$(evaluations)
run_exec e2 "Tạo ba file Python trong thư mục geo: geo/area.py (hàm area(w, h) trả về w * h), geo/perimeter.py (hàm perimeter(w, h) trả về 2 * (w + h)), geo/square.py (hàm square(a) trả về a * a). Chỉ tạo file, lượt này không chạy lệnh nào."
check "the files were made" '[ -f "$P/geo/area.py" ] && [ -f "$P/geo/square.py" ]'
check "the verify reminder came first" 'sent | grep -q "Before you finish: this turn changed"'
check "and the turn was not also judged" '[ "$(evaluations)" -eq "$before" ]'

echo "################ 3. a one-line change is not judged"
before=$(evaluations)
run_exec e3 "Thêm một dòng chú thích '# basic arithmetic' lên đầu calc/add.py bằng edit_file, rồi chạy python -c \"import ast; ast.parse(open('calc/add.py').read()); print('PARSE-OK')\"."
check "the change was made" 'grep -q "basic arithmetic" "$P/calc/add.py"'
check "a small turn is not judged" '[ "$(evaluations)" -eq "$before" ]'

echo "################ 4. HA_SELF_EVAL=off judges nothing"
before=$(evaluations)
HA_SELF_EVAL=off run_exec e4 "Tạo ba file Python trong thư mục text: text/upper.py (hàm up(s) trả về s.upper()), text/lower.py (hàm low(s) trả về s.lower()), text/strip.py (hàm st(s) trả về s.strip()). Sau đó chạy python -c \"import sys; sys.path.insert(0, 'text'); import upper, lower, strip; print('TEXT-OK')\"."
check "the files were made" '[ -f "$P/text/upper.py" ] && [ -f "$P/text/strip.py" ]'
check "and nothing was judged" '[ "$(evaluations)" -eq "$before" ]'

echo "################ 5. /self-eval reports the last turn as ECC's template"
agent "$P" s5
say s5 "Giải thích ngắn gọn calc/add.py làm gì, trong một câu." | tail -2 | sed 's/^/   /'
before=$(evaluations)
"$HA" send s5 "/self-eval" --wait > "$RX/report.out" 2>&1
sed 's/^/   /' "$RX/report.out" | head -4
# The judge is a model call of its own: /self-eval says it started, and the
# report comes when it is done (the report's template is unit-tested).
for _ in $(seq 1 120); do [ "$(evaluations)" -gt "$before" ] && break; sleep 1; done
check "/self-eval says it is judging" 'grep -q "self-evaluation: judging the last turn" "$RX/report.out"'
check "it judged that turn and kept it as a manual evaluation" '[ "$(evaluations)" -gt "$before" ] && [ "$(latest trigger)" = manual ] && [ "$(latest valid)" = True ]'
check "on the five axes" 'tail -1 "$LOGFILE" | "$PY" -c "import json,sys; s=json.load(sys.stdin)[\"scores\"]; sys.exit(0 if sorted(s)==[\"accuracy\",\"actionability\",\"clarity\",\"completeness\",\"conciseness\"] else 1)"'
check "the lookup is traced" 'grep -rh "\"self_eval\"" "$D/diagnostics" | grep -q "\"on_demand\""'
"$HA" shutdown --force >/dev/null 2>&1
"$HA" learning report > "$RX/learning-report.out" 2>&1
check "ha learning report shows the last 30 days" 'grep -q "Self-evaluations, last 30 days" "$RX/learning-report.out"'

result
