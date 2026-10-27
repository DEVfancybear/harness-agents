#!/usr/bin/env bash
# What waits for a turn, as a 0.7.4 session found it:
#   1. after a stop (`ha abort`, as Ctrl-C), `/goal resume` carries the goal on
#      in a turn of its own; a child's report that arrives after that turn
#      wakes the session - it was held until the user typed again, and the
#      goal sat idle for fifty minutes;
#   2. a steer sent while a turn summarizes the conversation before its first
#      step reaches that turn - it waited, queued, for the whole turn;
#   3. (bug report 2026-10-26) a message queued while a turn ran waits while
#      the provider's usage limit parks the session - it started a turn 29 ms
#      after the park, which called the provider and parked it again - and
#      runs once the park is over;
#   4. a child told its usage limit resets in hours asks once, not three
#      times a minute apart.
# Sections 3 and 4 need MODEL on openai-codex: the agents reach ChatGPT through
# quota_proxy.py, which answers with a usage limit while a flag file exists.
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
"$HA" send q1 "/goal Kiểm thử harness, làm đúng thứ tự: (1) dùng tool delegate với wait=false giao cho một agent role explorer nhiệm vụ: gọi run_shell đúng một lần với command Start-Sleep -Seconds 40 và timeout_ms 90000 (bắt buộc chạy), rồi trả lời đúng CHILD-DONE. (2) Sau khi giao, tự gọi run_shell với command Start-Sleep -Seconds 60 và timeout_ms 120000. (3) Khi nhận được báo cáo CHILD-DONE của explorer thì gọi goal_complete. Nếu explorer bị dừng thì giao lại đúng nhiệm vụ đó cho một explorer mới, rồi kết thúc lượt và chờ báo cáo." >/dev/null 2>&1
for _ in $(seq 1 60); do
  records child | grep -q " started " && break
  sleep 3
done
sleep 8
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
  # The turn summarizes before its first step; the steer comes in meanwhile.
  "$HA" send "q2-$attempt" "Trả lời đúng một dòng: số dòng của big/part1.txt (không đọc lại file, dùng những gì bạn đã thấy)." --wait > "$RX/q2-$attempt.out" 2>&1 &
  sleep 3
  "$HA" send "q2-$attempt" --steer "Thêm một dòng STEER-SEEN vào cuối câu trả lời của lượt này." 2>&1 | sed 's/^/   steer: /'
  wait
  sleep 10
  summarized=$(records compaction | awk -v since="$since" '$1 > since && ($2 == "auto" || $2 == "earlier_summarized")' | head -1)
  starts_after=$(records turn | grep -c " start ")
  answer=$(cat "$RX/q2-$attempt.out")
  echo "   attempt $attempt: summarized=[${summarized:+yes}] turns $starts_before -> $starts_after; answer: $(echo "$answer" | tr '\n' ' ' | cut -c1-120)"
  "$HA" shutdown --force >/dev/null 2>&1
  # The precondition: this turn summarized before its first step.
  [ -z "$summarized" ] && continue
  if [ $((starts_after - starts_before)) -eq 1 ] && echo "$answer" | grep -q "STEER-SEEN"; then verdict=0; else verdict=2; fi
  break
done
check "the turn summarized before its first step (the precondition)" '[ "$verdict" -ne 1 ]'
check "the steer reached that turn: read in its answer, no turn of its own" '[ "$verdict" -eq 0 ]'

if [ "${MODEL#openai-codex/}" = "$MODEL" ]; then
  echo "SKIP  sections 3 and 4 need MODEL=openai-codex/..."
  result
  exit $?
fi
X="$RX/proxy"
mkdir -p "$X"
"$PY" "$(dirname "$0")/quota_proxy.py" "$X" &
PROXY=$!
trap 'kill $PROXY 2>/dev/null' EXIT
for _ in $(seq 1 50); do [ -s "$X/port" ] && break; sleep 0.2; done
# The session's provider, through the proxy: no --model, which would choose
# the endpoint itself.
export HA_PROVIDER_ID=openai-codex HA_PROVIDER_PROTOCOL=openai_codex \
  HA_PROVIDER_ENDPOINT="http://127.0.0.1:$(cat "$X/port")/backend-api/codex/responses" \
  HA_PROVIDER_MODEL="${MODEL#openai-codex/}"
proxied() {
  (cd "$1" && "$PROBE" "$RX/$2.bin" "$HA" "wait:9000" -- chat --approval full-auto >/dev/null)
  local id
  id=$("$HA" list --json | "$PY" -c "import json,sys; d=json.load(sys.stdin); a=d if isinstance(d,list) else d.get('agents',d); print([x['id'] for x in a if not x.get('name')][0])")
  "$HA" rename "$id" "$2" >/dev/null
}
# The proxy's log, as `ms status key body_has...`, from `since`.
requests() {
  "$PY" - "$X/requests.jsonl" "${1:-0}" <<'EOF'
import json, sys
since = int(sys.argv[2])
try:
    rows = [json.loads(line) for line in open(sys.argv[1], encoding="utf-8")]
except OSError:
    rows = []
for row in rows:
    if row["ms"] > since:
        print(row["ms"], row["status"], row["key"], *row["body_has"])
EOF
}
now_ms() { date +%s%3N; }

echo "################ 3. a parked session holds the queue until the park is over"
proxied "$RX/p" q3
say q3 "Trả lời đúng một dòng: PROXY-OK" | tail -1 | sed 's/^/   /'
check "a turn ran through the proxy (the precondition)" 'last | grep -q "PROXY-OK" && requests | grep -q " 200 "'
code="FOLLOW-$RANDOM$RANDOM"
echo "$code" > "$X/watch"
calls=$(requests | wc -l)
"$HA" send q3 "Kiểm thử harness: gọi run_shell đúng một lần với command Start-Sleep -Seconds 25 và timeout_ms 60000, rồi trả lời đúng một dòng: A-DONE." >/dev/null 2>&1
# The first call of the turn asked for the shell; the limit starts while it sleeps.
for _ in $(seq 1 60); do [ "$(requests | wc -l)" -gt "$calls" ] && break; sleep 2; done
sleep 5
touch "$X/limit"
"$HA" send q3 --follow-up "Trả lời đúng một dòng: $code" 2>&1 | sed 's/^/   follow-up: /'
parked=""
for _ in $(seq 1 60); do
  parked=$(records provider | awk '$2 == "quota_parked" {print $1; exit}')
  [ -n "$parked" ] && break
  sleep 2
done
sleep 30
records turn | sed 's/^/   /'
requests "$parked" | sed 's/^/   proxy after the park: /'
check "the usage limit parked the session (the precondition)" '[ -n "$parked" ]'
check "no turn started while the session was parked" '[ -n "$parked" ] && ! (records turn | awk -v since="$parked" "\$1 > since && \$2 == \"start\"" | grep -q .)'
check "no call reached the provider while parked" '[ -n "$parked" ] && [ "$(requests "$parked" | wc -l)" -eq 0 ]'
rm -f "$X/limit"
before=$(now_ms)
say q3 "Trả lời đúng một dòng: USER-OK" | tail -1 | sed 's/^/   /'
# The user's own turn is answered, which ends the park; the queued message
# runs after it.
ran=""
for _ in $(seq 1 40); do
  ran=$(requests "$before" | awk -v code="$code" '$2 == 200 && $0 ~ code' | head -1)
  [ -n "$ran" ] && break
  sleep 3
done
check "the user's turn ran while parked" 'last | grep -q "USER-OK"'
check "the queued message ran once the park was over" '[ -n "$ran" ]'
"$HA" shutdown --force >/dev/null 2>&1

echo "################ 4. a child told its limit resets in hours asks once"
proxied "$RX/p" q4
: > "$X/watch"
"$HA" send q4 "Kiểm thử harness, làm đúng thứ tự: (1) dùng tool delegate với wait=false giao cho một agent role explorer nhiệm vụ: gọi run_shell đúng một lần với command Start-Sleep -Seconds 30 và timeout_ms 90000, rồi trả lời đúng CHILD-DONE. (2) Sau khi giao, tự gọi run_shell với command Start-Sleep -Seconds 120 và timeout_ms 180000. (3) Trả lời đúng một dòng: PARENT-DONE." >/dev/null 2>&1
for _ in $(seq 1 60); do
  records child | grep -q " started " && break
  sleep 3
done
sleep 10
limited=$(now_ms)
touch "$X/limit"
for _ in $(seq 1 50); do
  records child | awk -v since="$limited" '$1 > since && ($2 == "failed" || $2 == "settled")' | grep -q . && break
  sleep 3
done
sleep 5
records child | sed 's/^/   /'
requests "$limited" | sed 's/^/   proxy after the limit: /'
most=$(requests "$limited" | awk '$2 == 429 {n[$3]++} END {m=0; for (k in n) if (n[k] > m) m = n[k]; print m}')
check "the child met the limit and stopped (the precondition)" 'records child | awk -v since="$limited" "\$1 > since && \$2 == \"failed\"" | grep -q .'
check "no conversation asked again after a limit that resets in hours" '[ "$most" -eq 1 ]'
rm -f "$X/limit"
"$HA" shutdown --force >/dev/null 2>&1

result
