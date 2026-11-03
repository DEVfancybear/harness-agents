#!/usr/bin/env bash
# The background memory passes as a durable queue (0.8.9, "sleep",
# docs/architecture/SLEEP.en.md): a pass whose model call fails is kept and
# runs later, instead of the turns it was to learn from being forgotten.
#   1. `ha exec`: a review whose model call fails stays queued (`ha doctor`
#      says so), and the next run - one that is not due a review of its own -
#      reviews those turns (0.8.8 zeroed the count as it claimed the review);
#   2. a review a session left for later (`refine-pending/`, as 0.8.8 wrote
#      it) whose model call fails is still there, and the next turn runs it
#      (0.8.8 deleted it as it read it);
#   3. two compactions inside the twenty-minute spacing both get their turns
#      reviewed: the second waits, it is not skipped (HA_COMPACT_AT small, the
#      spacing HA_COMPACTION_REVIEW_SECONDS);
#   4. Deep Dream whose model call fails is tried again, not marked done, and
#      after HA_SLEEP_MAX_ATTEMPTS failures gives up and `ha doctor` says so
#      (0.8.8 tried it once an hour, forever, and said nothing);
#   5. the bug report counts the failed upkeep calls, with their class.
# The model's calls go through quota_proxy.py, which answers 503 to a request
# whose body holds a line of its `fail` file (the review gate's or the memory
# curator's system prompt), so one kind of call fails and the turns run. It
# needs MODEL on openai-codex or deepseek. Every check prints PASS or FAIL; the
# evidence stays in $LIVE_ROOT/sleep.
#
#   bash scripts/testing/live-tests/sleep.sh
set -u
source "$(dirname "$0")/env.sh"
fresh sleep
# The provider behind the proxy: its protocol, its origin and its path.
case "${MODEL%%/*}" in
  openai-codex) UP_PROTOCOL=openai_codex UP_ORIGIN=https://chatgpt.com UP_PATH=/backend-api/codex/responses ;;
  deepseek) UP_PROTOCOL=openai_chat UP_ORIGIN=https://api.deepseek.com UP_PATH=/chat/completions ;;
  *)
    echo "SKIP  sleep.sh needs MODEL=openai-codex/... or deepseek/... (the calls go through quota_proxy.py)"
    result
    exit $?
    ;;
esac
unset HA_AUTO_REFINE
export HA_PYTHON=${HA_PYTHON:-$PY} HA_DIAG=debug
# A failed pass is due again fifteen seconds later (not within the run that
# failed), and gives up after two failures.
export HA_SLEEP_RETRY_SECONDS=15 HA_SLEEP_MAX_ATTEMPTS=2
D="$HA_HOME/data"
GATE="You are the automatic /refine review gate of ha"
CURATOR="You are the memory curator of ha"

X="$RX/proxy"
mkdir -p "$X"
"$PY" "$(dirname "$0")/quota_proxy.py" "$X" "$UP_ORIGIN" &
PROXY=$!
trap 'kill $PROXY 2>/dev/null' EXIT
for _ in $(seq 1 50); do [ -s "$X/port" ] && break; sleep 0.2; done
# Every call through the proxy: no --model, which would choose the endpoint.
export HA_PROVIDER_ID="${MODEL%%/*}" HA_PROVIDER_PROTOCOL="$UP_PROTOCOL" \
  HA_PROVIDER_ENDPOINT="http://127.0.0.1:$(cat "$X/port")$UP_PATH" \
  HA_PROVIDER_MODEL="${MODEL#*/}"
now_ms() { date +%s%3N; }
# The proxy's log from `since`, as `ms status phrase...` (the phrases of
# `fail` the request held).
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
        print(row["ms"], row["status"], "|".join(row["body_has"]))
EOF
}
# failed_calls <phrase> [since]: how many requests holding the phrase were refused.
failed_calls() { requests "${2:-0}" | grep " 503 " | grep -cF "$1"; }
# Diagnostic records of one kind, as `ms event task=... key=value ...`.
records() {
  "$PY" - "$D/diagnostics" "$1" <<'EOF'
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
            rows.append((record.get("ms", 0), f"{record.get('ms', 0)} {record.get('event')} task={record.get('task')} {fields}"))
for _, row in sorted(rows):
    print(row)
EOF
}
# run_exec <name> <dir> [args...] <prompt>: a headless run; JSON in $RX/<name>.json.
run_exec() {
  local name="$1" dir="$2"; shift 2
  printf '\n## %s (ha exec %s)\n\n> %s\n\n' "$(date +%H:%M:%S)" "$name" "${*: -1}" >> "$LOG"
  (cd "$dir" && "$HA" exec --approval full-auto --output-format json "$@" >"$RX/$name.json" 2>"$RX/$name.err")
  "$PY" -c "import json,sys; print(json.load(open(sys.argv[1],encoding='utf-8')).get('response',''))" "$RX/$name.json" >> "$LOG" 2>/dev/null
  sed 's/^/    /' "$RX/$name.err" >> "$LOG"
}
json_of() { "$PY" -c "import json,sys; print(json.load(open(sys.argv[1],encoding='utf-8')).get(sys.argv[2],''))" "$RX/$1.json" "$2" 2>/dev/null; }
# An agent in the app, through the proxy.
proxied() {
  (cd "$1" && "$PROBE" "$RX/$2.bin" "$HA" "wait:9000" -- chat --approval full-auto >/dev/null)
  local id
  id=$("$HA" list --json | "$PY" -c "import json,sys; d=json.load(sys.stdin); a=d if isinstance(d,list) else d.get('agents',d); print([x['id'] for x in a if not x.get('name')][0])")
  "$HA" rename "$id" "$2" >/dev/null
}
doctor() { (cd "$1" && "$HA" doctor 2>&1); }
watermark() { cat "$D/sessions/$1/harness/review_watermark" 2>/dev/null; }

echo "################ 1. ha exec: a review whose model call fails is reviewed by the next run"
P1="$RX/p1"
project "$P1"
echo "$GATE" > "$X/fail"
# The precondition: the run made ten tool calls, enough for a review of its
# own with no setting changed (a run that is not due one reviews nothing on
# 0.8.8). A run that made fewer is retried in a new conversation.
T1=""
for attempt in 1 2 3; do
  since=$(now_ms)
  run_exec "e1-$attempt" "$P1" "Kiểm thử harness: gọi tool run_shell đúng 10 lần, lần lượt từng lần một (không gộp), với các lệnh: echo buoc-1, echo buoc-2, echo buoc-3, echo buoc-4, echo buoc-5, echo buoc-6, echo buoc-7, echo buoc-8, echo buoc-9, echo buoc-10 (timeout_ms 30000). Quy ước lâu dài của dự án này, mình muốn được nhớ ở các lần sau: mọi script deploy đặt trong thư mục ops/deploy. Sau đó trả lời đúng một dòng: XONG."
  calls=$(json_of "e1-$attempt" tool_calls)
  echo "   attempt $attempt: $calls tool call(s); refused review calls: $(failed_calls "$GATE" "$since")"
  if [ "${calls:-0}" -ge 10 ] && [ "$(failed_calls "$GATE" "$since")" -ge 1 ]; then
    T1=$(json_of "e1-$attempt" task_id); S1=$(json_of "e1-$attempt" session_id); E1="e1-$attempt"
    break
  fi
done
check "the run made ten tool calls and its review's model call was refused (the precondition)" '[ -n "$T1" ]'
sed 's/^/   /' "$RX/${E1:-e1-1}.err" | head -8
doctor "$P1" > "$RX/doctor1.txt"
grep -i "background memory" "$RX/doctor1.txt" | sed 's/^/   doctor: /'
check "the failed review is kept, and ha doctor says it waits to be tried again" '[ -n "$T1" ] && grep "failed and wait to be tried again" "$RX/doctor1.txt" | grep -q "review of $T1"'
rm -f "$X/fail"
sleep 16
run_exec e2 "$P1" --resume "${T1:-none}" "Lượt này không gọi tool nào. Trả lời đúng một từ: OK"
sed 's/^/   /' "$RX/e2.err" | head -8
echo "   watermark: $(watermark "${T1:-none}")"
check "the next run reviewed them before exiting" 'grep -q "auto-refine: reviewing" "$RX/e2.err"'
check "the conversation is reviewed through the run whose review failed" '[ -n "$T1" ] && [ "$(watermark "$T1")" = "$S1" ]'

echo "################ 2. a review a session left for later that fails is still there, and runs later"
P2="$RX/p2"
project "$P2"
HA_AUTO_REFINE=off run_exec a1 "$P2" "Lượt này không gọi tool nào. Quy ước lâu dài của dự án này: tên nhánh git luôn theo dạng feat/<mô-tả-ngắn>. Trả lời đúng một từ: OK"
T2=$(json_of a1 task_id); S2=$(json_of a1 session_id)
# The project's store: the one whose files hold that conversation.
STORE2=$("$PY" - "$D/projects" "$T2" <<'EOF'
import glob, os, sys
for db in glob.glob(os.path.join(sys.argv[1], "*", "harness.sqlite3")):
    data = b"".join(open(path, "rb").read() for path in (db, db + "-wal") if os.path.exists(path))
    if sys.argv[2].encode() in data:
        print(os.path.dirname(db))
        break
EOF
)
mkdir -p "$STORE2/refine-pending"
# As 0.8.8 left a session's end: six turns not yet reviewed.
printf '{"task": "%s", "session": "%s", "turns": 6, "in_play": [], "at": "2026-10-30T00:00:00Z"}' "$T2" "$S2" > "$STORE2/refine-pending/left.json"
check "an ended session's review is left in the project (the precondition)" '[ -n "$T2" ] && [ -f "$STORE2/refine-pending/left.json" ]'
echo "$GATE" > "$X/fail"
proxied "$P2" b
since=$(now_ms)
say b "Lượt này không gọi tool nào. Trả lời đúng một từ: OK" | tail -1 | sed 's/^/   /'
for _ in $(seq 1 60); do [ "$(failed_calls "$GATE" "$since")" -ge 1 ] && break; sleep 3; done
sleep 3
records review | awk -v since="$since" '$1 > since' | sed 's/^/   /'
check "the left review ran and its model call was refused (the precondition)" '[ "$(failed_calls "$GATE" "$since")" -ge 1 ]'
doctor "$P2" > "$RX/doctor2.txt"
grep -i "background memory" "$RX/doctor2.txt" | sed 's/^/   doctor: /'
check "the failed review is still there" 'grep "failed and wait to be tried again" "$RX/doctor2.txt" | grep -q "review of $T2"'
rm -f "$X/fail"
sleep 16
since=$(now_ms)
say b "Lượt này không gọi tool nào. Trả lời đúng một từ: OK" | tail -1 | sed 's/^/   /'
for _ in $(seq 1 60); do [ "$(watermark "$T2")" = "$S2" ] && break; sleep 3; done
records review | awk -v since="$since" '$1 > since' | sed 's/^/   /'
check "the next turn ran it: the ended session is reviewed" '[ "$(watermark "$T2")" = "$S2" ]'
"$HA" shutdown --force >/dev/null 2>&1

echo "################ 3. two compactions inside the spacing both get their turns reviewed"
P3="$RX/p3"
project "$P3"
mkdir -p "$P3/big"
for n in 1 2 3 4 5 6; do
  "$PY" -c "import sys; n=int(sys.argv[1]); print('\n'.join(f'row {i} of part {n}: ' + 'value ' * 10 for i in range(500)))" "$n" > "$P3/big/part$n.txt"
done
# Four minutes apart instead of twenty, so the second compaction falls inside
# the spacing and its review can be waited for.
verdict=1
for attempt in 1 2; do
  HA_COMPACT_AT=30000 HA_COMPACTION_REVIEW_SECONDS=240 proxied "$P3" "c-$attempt"
  first=$(now_ms)
  say "c-$attempt" "Đọc lần lượt big/part1.txt, big/part2.txt và big/part3.txt bằng read_file (đọc toàn bộ mỗi file), rồi trả lời đúng một dòng: ĐÃ-ĐỌC-1." | tail -1 | sed 's/^/   /'
  # The first compaction's review, which starts the spacing.
  for _ in $(seq 1 60); do
    records review | awk -v since="$first" '$1 > since' | grep -q . && break
    sleep 5
  done
  compacted1=$(records compaction | awk -v since="$first" '$1 > since && ($2 == "auto" || $2 == "earlier_summarized")' | head -1)
  reviewed1=$(records review | awk -v since="$first" '$1 > since' | head -1)
  second=$(now_ms)
  say "c-$attempt" "Đọc lần lượt big/part4.txt, big/part5.txt và big/part6.txt bằng read_file (đọc toàn bộ mỗi file), rồi trả lời đúng một dòng: ĐÃ-ĐỌC-2." | tail -1 | sed 's/^/   /'
  compacted2=$(records compaction | awk -v since="$second" '$1 > since && ($2 == "auto" || $2 == "earlier_summarized")' | head -1)
  echo "   attempt $attempt: first compaction [${compacted1:+yes}] reviewed [${reviewed1:+yes}], second compaction [${compacted2:+yes}]"
  if [ -z "$compacted1" ] || [ -z "$reviewed1" ] || [ -z "$compacted2" ]; then
    "$HA" shutdown --force >/dev/null 2>&1
    continue
  fi
  at2=${compacted2%% *}
  # Its review waits out the spacing (four minutes from the first's start).
  reviewed2=""
  for _ in $(seq 1 84); do
    reviewed2=$(records review | awk -v since="$at2" '$1 > since' | head -1)
    [ -n "$reviewed2" ] && break
    sleep 5
  done
  records sleep | awk -v since="$first" '$1 > since' | sed 's/^/   sleep: /'
  records review | awk -v since="$first" '$1 > since' | sed 's/^/   review: /'
  # That review is the second compaction's own job: the span its compaction
  # enqueued, claimed no sooner than the spacing after the first one's claim,
  # and done.
  span() { grep -o 'span=[^ ]*' | head -1 | cut -d= -f2; }
  started1=$(records sleep | awk -v since="$first" '$1 > since && $2 == "claimed" && /trigger=compaction/ {print $1; exit}')
  span2=$(records sleep | awk -v since="$second" '$1 > since && $2 == "enqueued" && /trigger=compaction/' | head -1 | span)
  claimed2=$(records sleep | awk -v since="$second" -v s="span=$span2" '$1 > since && $2 == "claimed" && index($0, s) {print $1; exit}')
  done2=$(records sleep | awk -v since="$second" -v s="span=$span2" '$1 > since && $2 == "done" && index($0, s) {print $1; exit}')
  echo "   first compaction's review claimed at ${started1:-?}; second span ${span2:-?} claimed at ${claimed2:-?}, done at ${done2:-?}"
  if [ -n "$reviewed2" ] && [ -n "$span2" ] && [ -n "$started1" ] && [ -n "$claimed2" ] && [ -n "$done2" ] \
    && [ "$claimed2" -ge $((started1 + 240000 - 2000)) ]; then verdict=0; else verdict=2; fi
  "$HA" shutdown --force >/dev/null 2>&1
  break
done
check "two turns compacted, the first one's review ran (the precondition)" '[ "$verdict" -ne 1 ]'
check "the second compaction's own job reviewed its span, no sooner than the spacing after the first" '[ "$verdict" -eq 0 ]'

echo "################ 4. Deep Dream that fails is tried again, and gives up after so many failures"
P4="$RX/p4"
project "$P4"
# Enough global memory, changed enough, for Deep Dream to be due (as
# knowledge.sh seeds it): a harness_state.json the store imports.
"$PY" - "$D" <<'EOF'
import json, os, sys
path = os.path.join(sys.argv[1], "harness", "harness_state.json")
state = {"schema": 1, "entries": {"memory": {}}, "refinements": []}
facts = ["Font chữ của blog là Inter", "Màu chủ đạo của blog là xanh navy", "Blog được host trên Netlify",
         "Bài viết blog soạn bằng Markdown trong thư mục content", "Mỗi bài blog cần thẻ meta description",
         "Blog có RSS feed ở /rss.xml", "Ảnh của blog là WebP"]
for index, fact in enumerate(facts):
    for copy in ("a", "b"):
        key = f"fact{index}_{copy}"
        state["entries"]["memory"][key] = {"id": key, "kind": "memory", "title": f"Blog {index} ({copy})",
            "content": fact + ("." if copy == "a" else " - ghi lại lần nữa."), "path": "blog", "scope": "global",
            "version": 1, "source": "agent", "updated_at": "2026-10-01T00:00:00Z"}
os.makedirs(os.path.dirname(path), exist_ok=True)
json.dump(state, open(path, "w", encoding="utf-8"), ensure_ascii=False)
EOF
echo 20 > "$D/harness/memory_changes"
echo "$CURATOR" > "$X/fail"
proxied "$P4" d
since=$(now_ms)
for turn in 1 2 3; do
  say d "Lượt này không gọi tool nào. Trả lời đúng một từ: OK-$turn" | tail -1 | sed 's/^/   /'
  # Each turn's pass, when one runs, before the next turn.
  for _ in $(seq 1 20); do [ "$(failed_calls "$CURATOR" "$since")" -ge "$turn" ] && break; sleep 3; done
  echo "   after turn $turn: $(failed_calls "$CURATOR" "$since") refused consolidation call(s)"
  sleep 16
done
records sleep | awk -v since="$since" '$1 > since' | sed 's/^/   sleep: /'
check "the consolidation ran and its model call was refused (the precondition)" '[ "$(failed_calls "$CURATOR" "$since")" -ge 1 ]'
check "a failed consolidation is tried again, not marked done" '[ "$(failed_calls "$CURATOR" "$since")" -ge 2 ] && [ ! -f "$D/harness/memory_consolidated_changes" ]'
check "after HA_SLEEP_MAX_ATTEMPTS (2) failures it is not tried again" '[ "$(failed_calls "$CURATOR" "$since")" -eq 2 ]'
doctor "$P4" > "$RX/doctor4.txt"
grep -i -A1 "background memory" "$RX/doctor4.txt" | sed 's/^/   doctor: /'
check "ha doctor says it gave up" 'grep -q "gave up after failing again and again: consolidation" "$RX/doctor4.txt"'
rm -f "$X/fail"
"$HA" shutdown --force >/dev/null 2>&1

echo "################ 5. the bug report counts the failed upkeep calls"
"$HA" bug-report --since 3h --out "$RX/report.zip" > /dev/null 2>&1
"$PY" - "$RX/report.zip" "$RX/report" <<'EOF'
import sys, zipfile
zipfile.ZipFile(sys.argv[1]).extractall(sys.argv[2])
EOF
S="$RX/report/SUMMARY.md"
sed -n '/^## Memory/,/^Read by the agent/p' "$S" | sed 's/^/   /'
sed -n '/^## Sleep/,/^## [A-Z]/p' "$S" | head -20 | sed 's/^/   /'
check "the upkeep table counts the review gate's failed calls, with their class" 'grep -Eq "^\| review_gate \| [0-9]+ \| [1-9][0-9]* \([a-z_]+ [0-9]+" "$S"'
check "and Deep Dream's" 'grep -Eq "^\| deep_dream \| [0-9]+ \| [1-9][0-9]* \([a-z_]+ [0-9]+" "$S"'
check "the Sleep section lists the pass that gave up" 'sed -n "/^## Sleep/,/^## [A-Z]/p" "$S" | grep -q "gave_up \`consolidation\`"'

result
