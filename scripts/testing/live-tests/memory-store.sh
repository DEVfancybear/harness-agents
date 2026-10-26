#!/usr/bin/env bash
# The memory store and memory-engine parity (docs/architecture/MEMORY_STORE.en.md),
# with a real model through `ha exec`. Every check fails on 0.8.0, which kept
# memory in harness_state.json.
#
# P1, the core: a 0.8 state file and journal migrated into memory.sqlite3 with
# their history; a write with a tree, a name, meta and a temporal period; an
# entry whose period ended left out of the digest yet found by search; a create
# that replaces the entry its name holds; a deleted entry reverted under its old
# id; each change an event naming who made it and why; `ha memory export`.
#
#   bash scripts/testing/live-tests/memory-store.sh
set -u
source "$(dirname "$0")/env.sh"
fresh memory-store
export HA_AUTO_REFINE=off
D=$HA_HOME/data
G=$D/harness
P=$RX/p
project "$P"

run_exec() {
  local name="$1"; shift
  printf '\n## %s (ha exec %s)\n\n> %s\n\n' "$(date +%H:%M:%S)" "$name" "${*: -1}" >> "$LOG"
  (cd "$P" && "$HA" exec --model "$MODEL" "${TH[@]}" --approval full-auto --output-format json "$@" >"$RX/$name.json" 2>"$RX/$name.err")
  "$PY" -c "import json,sys; print(json.load(open(sys.argv[1],encoding='utf-8')).get('response',''))" "$RX/$name.json" >> "$LOG" 2>/dev/null
  sed 's/^/    /' "$RX/$name.err" >> "$LOG"
}
answer() { "$PY" -c "import json,sys; print(json.load(open(sys.argv[1],encoding='utf-8')).get('response',''))" "$RX/$1.json" 2>/dev/null; }
row() { "$PY" "$MEMORY_STORE" row "$G" "$1" "$2"; }
events() { "$PY" "$MEMORY_STORE" events "$G"; }
# The request a turn sent, found by text in its prompt.
request() { "$PY" - "$D" "$1" <<'EOF'
import glob, os, sqlite3, sys
sys.stdout.reconfigure(encoding="utf-8")
found = None
for db in glob.glob(os.path.join(sys.argv[1], "projects", "*", "harness.sqlite3")):
    for (text,) in sqlite3.connect(f"file:{db}?mode=ro", uri=True).execute("select request_json from frozen_requests order by rowid"):
        if sys.argv[2] in text:
            found = text
print(found or "")
EOF
}

echo "################ 1. a 0.8 state file and journal are migrated, history and all"
mkdir -p "$G"
"$PY" - "$G" <<'EOF'
import json, os, sys
g = sys.argv[1]
state = {"schema": 1, "entries": {"memory": {"deploy_day": {
    "id": "deploy_day", "kind": "memory", "title": "Deploy day", "content": "Deploys go out on Fridays.",
    "path": "ops", "scope": "global", "version": 2, "source": "agent",
    "created_at": "2026-09-01T00:00:00Z", "updated_at": "2026-10-01T00:00:00Z"}}, "prompt": {}, "skill": {}, "subagent": {}},
    "refinements": [{"id": "refine_0001", "trigger": "seeded", "changes": []}]}
json.dump(state, open(os.path.join(g, "harness_state.json"), "w", encoding="utf-8"))
old = dict(state["entries"]["memory"]["deploy_day"], content="Deploys go out on Thursdays.", version=1,
           updated_at="2026-09-01T00:00:00Z")
record = {"at": "2026-10-01T00:00:00Z", "action": "update", "kind": "memory", "id": "deploy_day",
          "entry": old, "by": {"conversation": "task_old", "via": "memory tool"}}
open(os.path.join(g, "memory_history.jsonl"), "w", encoding="utf-8").write(json.dumps(record) + "\n")
EOF
run_exec m1 "Dùng tool memory: xem history của entry deploy_day (scope global) và cho biết trước đây lịch deploy là ngày nào, phiên bản hiện tại là ngày nào. Trả lời ngắn."
check "the store exists" '[ -f "$G/memory.sqlite3" ]'
check "the state file was imported and set aside" '[ ! -e "$G/harness_state.json" ] && [ -e "$G/harness_state.json.imported" ]'
check "the journal was imported and set aside" '[ ! -e "$G/memory_history.jsonl" ] && [ -e "$G/memory_history.jsonl.imported" ]'
check "the migrated entry is a row at its version" 'row memory deploy_day | grep -q "|/global/ops|.*|2|Deploys go out on Fridays."'
check "the journal's earlier version is an event" 'events | grep -q "^update|migrate|migration|deploy_day|v1|Deploys go out on Thursdays."'
check "the model read the earlier version through history" 'answer m1 | grep -qi "thursday\|thứ năm\|thứ 5"'

echo "################ 2. a write with a tree, a name, meta and a temporal period"
run_exec m2 "Dùng tool memory action write, đúng các tham số sau, không làm gì khác. Entry 1: tree '/global/team/decisions', name 'queue-choice', title 'Hàng đợi', content 'Đội chọn Redis Streams làm hàng đợi.', meta {\"type\": \"decision\", \"confidence\": \"high\"}. Entry 2: scope global, title 'Khuyến mãi mùa xuân', content 'Mã SPRING10 giảm 10% cho mọi đơn.', temporal {\"start\": \"2026-03-01\", \"end\": \"2026-04-01\"}. Trả lời: đã ghi."
check "the named entry sits at its tree" 'row memory queue-choice | grep -q "|/global/team/decisions|"'
check "its meta is kept" 'row memory queue-choice | grep -q "\"type\": *\"decision\""'
spring=$("$PY" "$MEMORY_STORE" rows "$G" memory | grep -a SPRING10 | cut -d'|' -f1)
check "the period is kept as its bounds" '[ -n "$spring" ] && row memory "$spring" | grep -q "|2026-03-01T00:00:00.000Z|2026-04-01T00:00:00.000Z|"'

echo "################ 3. an entry whose period ended is left out of the digest, yet search finds it"
run_exec m3 "Lượt này không dùng tool nào. Trả lời đúng một từ: MARKER_DIGEST"
R=$(request "MARKER_DIGEST")
check "the turn's request was found" '[ -n "$R" ]'
check "the digest shows the current entry" 'printf "%s" "$R" | grep -q "Redis Streams"'
check "the digest leaves the ended period out" '! printf "%s" "$R" | grep -q "SPRING10"'
run_exec m4 "Tìm trong memory (tool memory action search) mã khuyến mãi mùa xuân và báo lại mã đó cùng khoảng thời gian áp dụng."
check "search still finds the ended entry" 'answer m4 | grep -q "SPRING10"'

echo "################ 4. a create that replaces the entry its name holds"
run_exec m5 "Dùng tool memory action write với đúng các tham số: tree '/global/team/decisions', name 'queue-choice', title 'Hàng đợi', content 'Đội chuyển sang NATS JetStream làm hàng đợi.', on_conflict 'replace'. Không đọc trước, không làm gì khác. Trả lời: đã ghi."
check "the entry was replaced in place, at version 2" 'row memory queue-choice | grep -q "|2|Đội chuyển sang NATS JetStream"'
check "and is still one entry" '[ "$("$PY" "$MEMORY_STORE" rows "$G" memory | grep -ac "Hàng đợi\|NATS\|Redis")" -eq 1 ]'

echo "################ 5. a deleted entry comes back under its old id"
before=$(row memory queue-choice | cut -d'|' -f1)
run_exec m6 "Dùng tool memory: đọc entry queue-choice (scope global) để lấy version, rồi xóa nó (action delete). Trả lời: đã xóa."
check "the entry is gone" '[ -z "$(row memory queue-choice)" ]'
run_exec m7 "Dùng tool memory action revert với id 'queue-choice' (scope global, không cần version) để khôi phục entry vừa bị xóa. Trả lời: đã khôi phục."
after=$(row memory queue-choice | cut -d'|' -f1)
check "the entry is back" '[ -n "$after" ]'
check "under the id it had" '[ "$after" = "$before" ]'
check "at the next version" 'row memory queue-choice | cut -d"|" -f6 | grep -q "^3$"'

echo "################ 6. each change is an event naming who made it and why"
events | grep -a "queue-choice" | sed 's/^/   /'
check "created by the model" 'events | grep -q "^insert|create|model|queue-choice|v1|"'
check "replaced by the model" 'events | grep -q "^update|update|model|queue-choice|v2|"'
check "deleted by the model" 'events | grep -q "^delete|delete|model|queue-choice|v2|"'
check "reverted by the model" 'events | grep -q "^insert|revert|model|queue-choice|v3|"'

echo "################ 7. ha memory export"
"$HA" memory export > "$RX/export.json" 2>"$RX/export.err"
check "it exports the global store" '"$PY" -c "import json,sys; d=json.load(open(sys.argv[1],encoding=\"utf-8\")); sys.exit(0 if \"queue-choice\" in d[\"document\"][\"entries\"][\"memory\"] and d[\"document\"][\"refinements\"][0][\"id\"] == \"refine_0001\" else 1)" "$RX/export.json"'

result
