#!/usr/bin/env bash
# Delegated children with a real model, as a long session found them:
#   1. /subagent-model moves a child that is already running, from its next
#      model call (it moved only the next child, and the rows kept the old one);
#   2. a file a coder made, read before integrate_children, is said to be in
#      the coder's worktree (the read said only "File not found");
#   3. a goal whose turn ends while a child works waits for the child's report
#      instead of starting another turn at once;
#   4. a review's diagnostic record names the task, not the whole transcript.
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
agent "$RX/p" c3
"$HA" send c3 "/goal Kiểm thử harness: dùng tool delegate với wait=false giao cho đúng một agent role explorer nhiệm vụ: gọi run_process một lần với executable python, args -c và import time; time.sleep(60), rồi trả lời WAITED. Sau khi giao thì kết thúc lượt ngay, trả lời đúng một dòng ĐÃ-GIAO. Khi nhận được kết quả WAITED của explorer thì gọi goal_complete." >/dev/null 2>&1
for _ in $(seq 1 60); do
  notices | grep -q "child(ren) still working; their reports carry the goal on" && break
  sleep 5
done
for _ in $(seq 1 60); do
  "$HA" send c3 "/goal status" --wait 2>&1 | grep -qiE "status: *(complete|done)|no goal" && break
  sleep 5
done
"$HA" send c3 "/goal status" --wait 2>&1 | head -4 | sed 's/^/   /'
notices | grep -iE "goal" | sed 's/^/   notice: /' | head -8
check "the goal waited while the explorer worked" 'notices | grep -q "child(ren) still working; their reports carry the goal on"'
check "no turn ran just to nudge it: not continued while it worked" '! notices | grep -q "goal not complete yet; continuing (turn 2)"'
check "the explorer's report carried the goal to its end" '"$HA" send c3 "/goal status" --wait 2>&1 | grep -qiE "status: *(complete|done)|no goal"'
"$HA" shutdown --force >/dev/null 2>&1

echo "################ 4. a review's record names its task"
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

result
