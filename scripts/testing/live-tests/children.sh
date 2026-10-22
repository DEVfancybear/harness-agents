#!/usr/bin/env bash
# Delegated children with a real model, as a long session found them:
#   1. /subagent-model moves a child that is already running, from its next
#      model call (it moved only the next child, and the rows kept the old one);
#   2. a file a coder made, read before integrate_children, is said to be in
#      the coder's worktree (the read said only "File not found");
#   3. a goal whose turn ends while a child works waits for the child's report
#      instead of starting another turn at once;
#   4. a review's diagnostic record names the task, not the whole transcript;
#   5. a child started before the user chose full-auto does not ask once the
#      next turn begins;
#   6. every child runs at once (three did, the rest waited shown working).
# Every check prints PASS or FAIL; the conversation and the diagnostics stay in
# $LIVE_ROOT/children as evidence.
#
#   bash scripts/testing/live-tests/children.sh
#
# SUBAGENT_FROM is the model the children start on (default:
# deepseek/deepseek-v4-flash); MODEL is the one /subagent-model moves them to.
set -u
source "$(dirname "$0")/env.sh"
fresh children
export HA_AUTO_REFINE=off HA_PYTHON=${HA_PYTHON:-$PY}
FROM=${SUBAGENT_FROM:-deepseek/deepseek-v4-flash}
D="$HA_HOME/data/diagnostics"
project "$RX/p"
# Each task's model calls, oldest first: `task model model ...` per line.
calls() {
  "$PY" - "$D" <<'EOF'
import glob, json, os, sys
calls = {}
for path in glob.glob(os.path.join(sys.argv[1], "*.jsonl")):
    for line in open(path, encoding="utf-8", errors="replace"):
        try:
            record = json.loads(line)
        except ValueError:
            continue
        if record.get("kind") == "provider" and record.get("event") == "call":
            calls.setdefault(record.get("task"), []).append(
                (record.get("ms", 0), record.get("fields", {}).get("model")))
for task, models in calls.items():
    print(task, " ".join(model for _, model in sorted(models)))
EOF
}
# The notices the worker showed, one per line.
notices() {
  cat "$D"/*.jsonl 2>/dev/null | "$PY" -c "
import json, sys
for line in sys.stdin:
    try: r = json.loads(line)
    except ValueError: continue
    if r.get('kind') == 'notice': print(r.get('msg', ''))"
}

echo "################ 1. a running child follows /subagent-model"
agent "$RX/p" c1
"$HA" send c1 "/subagent-model $FROM" --wait 2>&1 | sed 's/^/   /'
# The child works in many model calls, so it is still running when the
# default changes. Sent without waiting: the change is made mid-turn.
"$HA" send c1 "Dùng tool delegate giao cho đúng một agent role explorer nhiệm vụ sau, giữ nguyên văn: 'Gọi tool run_process đúng 6 lần liên tiếp, mỗi lần một lượt riêng, với executable là python và args là -c và import time; time.sleep(6). Không gọi song song. Xong cả 6 lần thì trả lời đúng một dòng EXPLORER-XONG.' Đợi explorer xong rồi trả lời đúng một dòng: MOC-XONG" >/dev/null 2>&1
started=""
for _ in $(seq 1 60); do
  started=$(calls | awk -v m="${FROM#*/}" '$2 == m { print $1; exit }')
  [ -n "$started" ] && break
  sleep 5
done
echo "   the child's task: ${started:-none yet}"
moved=$("$HA" send c1 "/subagent-model $MODEL" --wait 2>&1)
echo "$moved" | sed 's/^/   /'
for _ in $(seq 1 60); do
  calls | grep -q "^$started .*${MODEL#*/}" && break
  sleep 5
done
calls | sed 's/^/   calls: /'
check "the child started on $FROM" '[ -n "$started" ]'
check "the change named the running child moving to $MODEL" 'echo "$moved" | grep -qE "running children switch at their next model call: explorer-[0-9a-f]+ -> $MODEL"'
check "the same child then called $MODEL" 'calls | grep -qE "^$started( ${FROM#*/})+( ${MODEL#*/})+"'
"$HA" send c1 "/agents" --wait 2>&1 | grep -iE "explorer" | head -3 | sed 's/^/   /'
"$HA" shutdown --force >/dev/null 2>&1

echo "################ 2. a coder's file read before it is integrated"
agent "$RX/p" c2
say c2 "Kiểm thử harness, làm đúng thứ tự: (1) dùng tool delegate giao cho đúng một agent role coder nhiệm vụ: tạo file notes/made.txt chứa đúng một dòng MADE-BY-CODER rồi commit trong worktree của nó; đợi coder xong. (2) CHƯA gọi integrate_children. Gọi tool read_file với path notes/made.txt. (3) In nguyên văn thông báo lỗi read_file trả về, không làm gì thêm." | tail -4 | sed 's/^/   /'
check "the read was refused in this checkout" '[ ! -f "$RX/p/notes/made.txt" ]'
check "the refusal says the coder made it in its worktree" 'ans | grep -qE "coder-[0-9a-f]+ made it in its worktree" || grep -rqE "coder-[0-9a-f]+ made it in its worktree" "$HA_HOME/data/projects"'
"$HA" shutdown --force >/dev/null 2>&1

echo "################ 3. a goal waits for a child that is still working"
# The turn that delegates ends while the explorer works; the goal used to start
# another turn at once. Read from the diagnostics: the delegating turn's end,
# the explorer's report, and the turn starts between and after them.
waited() {
  "$PY" - "$D" "$1" <<'PYEOF'
import glob, json, os, sys
directory, start = sys.argv[1], int(sys.argv[2])
records = []
for path in glob.glob(os.path.join(directory, "*.jsonl")):
    for line in open(path, encoding="utf-8", errors="replace"):
        try:
            record = json.loads(line)
        except ValueError:
            continue
        if record.get("ms", 0) >= start:
            records.append(record)
records.sort(key=lambda record: record.get("ms", 0))
settled = [r for r in records if r.get("kind") == "child" and r.get("event") == "settled"]
if not settled:
    print("   no child reported")
    sys.exit(1)
report = settled[0]
task, at = report.get("task"), report["ms"]
turns = [r for r in records if r.get("kind") == "turn" and r.get("task") == task]
for r in sorted(turns + [report], key=lambda r: r["ms"]):
    print(f"   +{(r['ms'] - start) / 1000:6.1f}s {r.get('kind')} {r.get('event')}")
ends = [r["ms"] for r in turns if r.get("event") == "terminal" and r["ms"] < at]
if not ends:
    print("   the child reported before the turn that started it ended")
    sys.exit(2)
between = [r for r in turns if r.get("event") == "start" and ends[-1] < r["ms"] < at]
after = [r for r in turns if r.get("event") == "start" and at <= r["ms"] <= at + 10_000]
sys.exit(0 if not between and after else 3)
PYEOF
}
# A model may skip the sleep, so the explorer reports before the turn ends and
# nothing is shown either way: then the scenario runs again, three times at most.
verdict=1
for attempt in 1 2 3; do
  agent "$RX/p" "c3-$attempt"
  since=$(date +%s%3N)
  "$HA" send "c3-$attempt" "/goal Kiểm thử harness: dùng tool delegate với wait=false giao cho đúng một agent role explorer nhiệm vụ: gọi run_process đúng một lần với executable python, args -c và import time; time.sleep(30) (bắt buộc chạy lệnh này, không được bỏ qua), rồi trả lời WAITED. Sau khi giao thì kết thúc lượt ngay, trả lời đúng một dòng ĐÃ-GIAO. Khi nhận được kết quả WAITED của explorer thì gọi goal_complete." >/dev/null 2>&1
  for _ in $(seq 1 60); do
    waited "$since" >/dev/null 2>&1; [ $? -ne 1 ] && break
    sleep 5
  done
  sleep 15
  echo "   attempt $attempt:"
  waited "$since"; verdict=$?
  "$HA" send "c3-$attempt" "/goal clear" --wait >/dev/null 2>&1
  "$HA" shutdown --force >/dev/null 2>&1
  [ "$verdict" -ne 1 ] && [ "$verdict" -ne 2 ] && break
done
check "the explorer outlived the turn that delegated" '[ "$verdict" -ne 1 ] && [ "$verdict" -ne 2 ]'
check "no turn started while it worked; its report started the next" '[ "$verdict" -eq 0 ]'

echo "################ 4. a review's record names its task"
# A review after every turn, as knowledge.sh asks for one.
"$PY" -c "import json,os,sys; p=sys.argv[1]; d=json.load(open(p)) if os.path.exists(p) else {}; d['autoRefine']={'turnInterval': 1}; json.dump(d, open(p,'w'))" "$HA_HOME/settings.json"
(cd "$RX/p" && HA_AUTO_REFINE=on "$HA" exec --model "$MODEL" "${TH[@]}" --approval full-auto "Trả lời đúng một dòng: MOC-REVIEW" > "$RX/exec.out" 2>&1)
tail -3 "$RX/exec.out" | sed 's/^/   /'
reviews() {
  cat "$D"/*.jsonl 2>/dev/null | "$PY" -c "
import json, sys
for line in sys.stdin:
    try: r = json.loads(line)
    except ValueError: continue
    if r.get('kind') == 'review': print(len(line), r.get('event'), str(r.get('task'))[:60])"
}
reviews | sed 's/^/   review: /'
check "the exec ran a review and recorded it" '[ -n "$(reviews)" ]'
check "every review record names a task id and stays small" 'reviews | awk "{ if (\$1 > 8000 || \$3 !~ /^task_/) bad=1 } END { exit bad }"'

echo "################ 5. a child of an earlier turn keeps the mode the user chose"
# The turn starts asking; the user switches to full-auto while it runs, and it
# delegates under the switch. Its child kept that turn's policy (ask), and the
# next turn cleared the switch: the child's later read asked the user and
# never finished. Shown only when the child reads after the second turn has
# begun; a model that skips the wait runs the scenario again (three at most).
records() {
  cat "$D"/*.jsonl 2>/dev/null | "$PY" -c "
import json, sys
since, kind, event = int(sys.argv[1]), sys.argv[2], sys.argv[3]
by = sys.argv[4] if len(sys.argv) > 4 else None
for line in sys.stdin:
    try: r = json.loads(line)
    except ValueError: continue
    if r.get('ms', 0) >= since and r.get('kind') == kind and r.get('event') == event \
            and (by is None or r.get('fields', {}).get('by') == by):
        print(r['ms'], r.get('msg', '').replace(chr(10), ' ')[:200])" "$since" "$@"
}
verdict=inconclusive
for attempt in 1 2 3; do
  (cd "$RX/p" && "$PROBE" "$RX/c5-$attempt.bin" "$HA" "wait:9000" -- chat --model "$MODEL" "${TH[@]}" --approval ask >/dev/null)
  id=$("$HA" list --json | "$PY" -c "import json,sys; d=json.load(sys.stdin); a=d if isinstance(d,list) else d.get('agents',d); print([x['id'] for x in a if not x.get('name')][0])")
  "$HA" rename "$id" "c5-$attempt" >/dev/null
  since=$(date +%s%3N)
  "$HA" send "c5-$attempt" "Kiểm thử harness: dùng tool delegate với wait=false giao cho đúng một agent role explorer nhiệm vụ: trước hết gọi run_process đúng một lần với executable python, args -c và import time; time.sleep(60) (bắt buộc chạy, không bỏ qua); sau đó gọi read_file với path README.md và kết thúc bằng câu trả lời là đúng dòng đầu tiên của file. Giao xong thì kết thúc lượt ngay, trả lời đúng một dòng ĐÃ-GIAO." >/dev/null 2>&1
  sleep 2
  "$HA" send "c5-$attempt" "/permissions full-auto" --wait 2>&1 | sed 's/^/   /'
  for _ in $(seq 1 60); do
    [ -n "$(records turn terminal)" ] && break
    sleep 3
  done
  say "c5-$attempt" "Trả lời đúng một dòng: LUOT-HAI" | tail -1 | sed 's/^/   /'
  second=$(records turn start user | sed -n 2p | cut -d' ' -f1)
  report=""
  for _ in $(seq 1 40); do
    report=$( (records child settled; records turn start agent_message) | sort -n | head -1)
    [ -n "$report" ] && break
    sleep 5
  done
  reported=${report%% *}
  echo "   attempt $attempt: second turn at ${second:-?}, the explorer reported at ${reported:-never}: ${report#* }"
  "$HA" shutdown --force >/dev/null 2>&1
  if [ -z "$report" ]; then verdict=asked; break; fi
  if [ -n "$second" ] && [ "$reported" -gt "$second" ]; then verdict=finished; break; fi
done
check "the second turn ran while the explorer worked" 'last | grep -q LUOT-HAI'
check "the explorer finished its read after the next turn began, asking no one" '[ "$verdict" = finished ]'
check "no approval expired" '[ -z "$(records approval expired)" ]'

echo "################ 6. every child runs at once"
# Three ran at once and the rest waited, shown working with no call made. Five
# explorers that each wait: more than three must be in their calls together.
agent "$RX/p" c6
since=$(date +%s%3N)
say c6 "Kiểm thử harness: dùng tool delegate với wait=false giao cùng lúc cho đúng 5 agent role explorer, mỗi agent cùng một nhiệm vụ: gọi run_process đúng một lần với executable python, args -c và import time; time.sleep(40) (bắt buộc chạy), rồi trả lời DONE. Giao xong cả 5 thì trả lời đúng một dòng ĐÃ-GIAO-5." | tail -1 | sed 's/^/   /'
sleep 60
together=$("$PY" - "$D" "$since" <<'PYEOF'
import glob, json, os, sys
directory, since = sys.argv[1], int(sys.argv[2])
spans = {}
root = None
for path in glob.glob(os.path.join(directory, "*.jsonl")):
    for line in open(path, encoding="utf-8", errors="replace"):
        try:
            r = json.loads(line)
        except ValueError:
            continue
        if r.get("ms", 0) < since:
            continue
        if r.get("kind") == "child" and r.get("event") == "settled":
            root = r.get("task")
        if r.get("kind") == "provider" and r.get("event") == "call":
            end = r["ms"]
            start = end - r.get("fields", {}).get("duration_ms", 0)
            low, high = spans.get(r.get("task"), (start, end))
            spans[r.get("task")] = (min(low, start), max(high, end))
children = {task: span for task, span in spans.items() if task != root}
# The root's own calls are the earliest task's: drop the task of the first call.
first = min(spans, key=lambda task: spans[task][0]) if spans else None
children.pop(first, None)
points = sorted([(low, 1) for low, _ in children.values()] + [(high, -1) for _, high in children.values()])
most = running = 0
for _, step in points:
    running += step
    most = max(most, running)
print(len(children), most)
PYEOF
)
echo "   children, most at once: $together"
check "five explorers ran" '[ "${together%% *}" -ge 5 ]'
check "more than three ran at once" '[ "${together##* }" -gt 3 ]'
"$HA" shutdown --force >/dev/null 2>&1

result
