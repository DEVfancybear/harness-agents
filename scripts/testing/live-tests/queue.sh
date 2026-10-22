#!/usr/bin/env bash
# What waits for a turn, as a 0.7.4 session found it:
#   1. after a stop (`ha abort`, as Ctrl-C), `/goal resume` carries the goal on
#      in a turn of its own; a child's report that arrives after that turn
#      wakes the session - it was held until the user typed again, and the
#      goal sat idle for fifty minutes;
#   2. a steer sent while a turn summarizes the conversation before its first
#      step reaches that turn - it waited, queued, for the whole turn.
# Every check prints PASS or FAIL; the conversation and the diagnostics stay in
# $LIVE_ROOT/queue as evidence.
#
#   bash scripts/testing/live-tests/queue.sh
set -u
source "$(dirname "$0")/env.sh"
fresh queue
export HA_AUTO_REFINE=off HA_PYTHON=${HA_PYTHON:-$PY} HA_DIAG=debug
D="$HA_HOME/data/diagnostics"
project "$RX/p"
# Diagnostic records of one kind with their time, as `ms event key=value ...`.
records() {
  "$PY" - "$D" "$1" <<'EOF'
import glob, json, os, sys
directory, kind = sys.argv[1], sys.argv[2]
rows = []
for path in glob.glob(os.path.join(directory, "*.jsonl")):
    for line in open(path, encoding="utf-8", errors="replace"):
        try:
            record = json.loads(line)
        except ValueError:
            continue
        if record.get("kind") == kind:
            fields = " ".join(f"{k}={v}" for k, v in sorted(record.get("fields", {}).items()))
            rows.append((record.get("ms", 0), f"{record.get('ms', 0)} {record.get('event')} {fields}"))
for _, row in sorted(rows):
    print(row)
EOF
}

echo "################ 1. a report after /goal resume wakes the session"
agent "$RX/p" q1
"$HA" send q1 "/goal Kiểm thử harness, làm đúng thứ tự: (1) dùng tool delegate với wait=false giao cho một agent role explorer nhiệm vụ: gọi run_process đúng một lần với executable python, args -c và import time; time.sleep(30) (bắt buộc chạy), rồi trả lời đúng CHILD-DONE. (2) Sau khi giao, tự gọi run_process với executable python, args -c và import time; time.sleep(120). (3) Khi nhận được báo cáo CHILD-DONE của explorer thì gọi goal_complete. Nếu explorer bị dừng thì giao lại đúng nhiệm vụ đó cho một explorer mới, rồi kết thúc lượt và chờ báo cáo." >/dev/null 2>&1
for _ in $(seq 1 60); do
  records child | grep -q " started " && break
  sleep 3
done
sleep 5
"$HA" abort q1 2>&1 | sed 's/^/   abort: /'
sleep 5
records turn | sed 's/^/   /'
check "the stopped turn ended canceled" 'records turn | grep -q "terminal .*outcome=canceled"'
resumed=$(date +%s%3N)
"$HA" send q1 "/goal resume" >/dev/null 2>&1
# The goal's turn starts the child again and ends; the child's report then
# starts a turn of its own, with nobody typing.
woke=""
for _ in $(seq 1 80); do
  woke=$(records turn | awk -v since="$resumed" '$1 > since && $2 == "start" && ($0 ~ /by=child_report/ || $0 ~ /by=agent_message/) {print; exit}')
  [ -n "$woke" ] && break
  sleep 5
done
records turn | awk -v since="$resumed" '$1 > since' | sed 's/^/   after resume: /'
records goal | sed 's/^/   /'
check "the goal turn started after /goal resume" 'records turn | awk -v since="$resumed" "\$1 > since && \$2 == \"start\" && /by=goal/" | grep -q .'
check "a child's report started a turn with nobody typing" '[ -n "$woke" ]'
"$HA" send q1 "/goal clear" --wait >/dev/null 2>&1
"$HA" shutdown --force >/dev/null 2>&1

echo "################ 2. a steer sent while the turn summarizes reaches that turn"
# A small compaction threshold, so the second message summarizes first.
mkdir -p "$RX/p/big"
for n in 1 2 3; do
  "$PY" -c "import sys; n=int(sys.argv[1]); print('\n'.join(f'row {i} of part {n}: ' + 'value ' * 10 for i in range(500)))" "$n" > "$RX/p/big/part$n.txt"
done
verdict=1
for attempt in 1 2 3; do
  HA_COMPACT_AT=30000 agent "$RX/p" "q2-$attempt"
  say "q2-$attempt" "Đọc lần lượt big/part1.txt, big/part2.txt và big/part3.txt bằng read_file (đọc toàn bộ mỗi file), rồi trả lời đúng một dòng: ĐÃ-ĐỌC." | tail -1 | sed 's/^/   /'
  starts_before=$(records turn | grep -c " start ")
  since=$(date +%s%3N)
  "$HA" send "q2-$attempt" "Trả lời đúng một dòng: số dòng của big/part1.txt (không đọc lại file, dùng những gì bạn đã thấy)." >/dev/null 2>&1 &
  sleep 3
  "$HA" send "q2-$attempt" --steer "Thêm một dòng STEER-SEEN vào cuối câu trả lời của lượt này." >/dev/null 2>&1
  wait
  for _ in $(seq 1 60); do
    records turn | awk -v since="$since" '$1 > since && $2 == "terminal"' | grep -q . && break
    sleep 3
  done
  sleep 10
  summarized=$(records compaction | awk -v since="$since" '$1 > since && $2 == "earlier_summarized"' | head -1)
  starts_after=$(records turn | grep -c " start ")
  answer=$("$HA" send "q2-$attempt" "Lượt trước bạn có thấy yêu cầu STEER-SEEN trong chính lượt đó không? Trả lời CO hoặc KHONG." --wait 2>&1)
  echo "   attempt $attempt: summarized=[${summarized:+yes}] turns $starts_before -> $starts_after; $answer" | head -3
  "$HA" shutdown --force >/dev/null 2>&1
  # The precondition: this turn summarized before its first step.
  [ -z "$summarized" ] && continue
  if [ $((starts_after - starts_before)) -eq 1 ]; then verdict=0; else verdict=2; fi
  break
done
check "the turn summarized before its first step (the precondition)" '[ "$verdict" -ne 1 ]'
check "the steer reached that turn: no turn of its own" '[ "$verdict" -eq 0 ]'

result
