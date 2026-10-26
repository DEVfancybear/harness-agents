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
# P2: the tree, a move that keeps each entry's id, the history feed, a dry run.
# P3: a search by meta alone, a JSONPath predicate, search by meaning against a
# fake embedding server. P4: a knowledge page's history and revert, one search
# over memory and knowledge, history_search by kind and time, history_read's
# neighbors. 0.8.2 (section 11): a name kept through updates and unique per
# tree, a patch that leaves content out, the immutable memory_id, an entry's
# history a page at a time, English stems, lquery's `%`, JSONPath datetimes and
# an unknown method refused, a tree filter on knowledge pages, an old page
# version read whole, status, and a write that moves an entry to another scope.
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
# 0.8.2: replace takes the entry whole, as memory-engine does, so the write
# gives the meta the later searches look for.
run_exec m5 "Dùng tool memory action write với đúng các tham số: tree '/global/team/decisions', name 'queue-choice', title 'Hàng đợi', content 'Đội chuyển sang NATS JetStream làm hàng đợi.', meta {\"type\": \"decision\", \"confidence\": \"high\"}, on_conflict 'replace'. Không đọc trước, không làm gì khác. Trả lời: đã ghi."
check "the entry was replaced in place, at version 2" 'row memory queue-choice | grep -q "|2|Đội chuyển sang NATS JetStream"'
check "and is still one entry" '[ "$("$PY" "$MEMORY_STORE" rows "$G" memory | grep -ac "Hàng đợi\|NATS\|Redis")" -eq 1 ]'

echo "################ 5. a deleted entry comes back under its old id"
before=$(row memory queue-choice | cut -d'|' -f1)
run_exec m6 "Dùng tool memory: đọc entry queue-choice (scope global) để lấy version, rồi xóa nó (action delete). Trả lời: đã xóa."
check "the entry is gone" '[ -z "$(row memory queue-choice)" ]'
run_exec m7 "Dùng tool memory action revert với id 'queue-choice' (scope global, không cần version) để khôi phục entry vừa bị xóa. Trả lời: đã khôi phục."
after=$(row memory queue-choice | cut -d'|' -f1)
check "the entry is back" '[ -n "$after" ]'
check "under the id it had" '[ -n "$before" ] && [ "$after" = "$before" ]'
check "at the next version" 'row memory queue-choice | cut -d"|" -f6 | grep -q "^3$"'

echo "################ 6. each change is an event naming who made it and why"
events | grep -a "queue-choice" | sed 's/^/   /'
check "created by the model" 'events | grep -q "^insert|create|model|queue-choice|v1|"'
check "replaced by the model" 'events | grep -q "^update|update|model|queue-choice|v2|"'
check "deleted by the model" 'events | grep -q "^delete|delete|model|queue-choice|v2|"'
check "reverted by the model" 'events | grep -q "^insert|revert|model|queue-choice|v3|"'

echo "################ 8. P2: the tree, a move that keeps each entry, the history feed"
run_exec m8 "Dùng tool memory action tree (không cần tham số khác) rồi liệt kê ngắn các nhánh cây trong global và mỗi nhánh có bao nhiêu entry."
check "the model read the tree" 'answer m8 | grep -q "team/decisions\|decisions"'
before=$(row memory queue-choice | cut -d'|' -f1)
run_exec m9 "Dùng tool memory action move với source '/global/team', destination '/global/platform', dry_run false. Không làm gì khác. Trả lời: số entry đã chuyển."
check "the entry moved to the new tree" 'row memory queue-choice | grep -q "|/global/platform/decisions|"'
check "and is the same memory" '[ -n "$before" ] && [ "$(row memory queue-choice | cut -d"|" -f1)" = "$before" ]'
check "the move is an event by the model" 'events | grep -q "^update|move|model|queue-choice|"'
run_exec m10 "Dùng tool memory action history với tree '/global/platform' (không truyền id) và cho biết thay đổi mới nhất ở nhánh đó là thao tác gì (operation, cause) và lúc nào."
check "the model read the feed of that tree" 'answer m10 | grep -qi "move\|chuyển\|di chuyển"'
run_exec m11 "Dùng tool memory action delete_tree với tree '/global/platform' và dry_run true, rồi báo bao nhiêu entry sẽ bị xóa. Không xóa thật."
check "a dry run deletes nothing" '[ -n "$(row memory queue-choice)" ]'

echo "################ 9. P3: filters, and search by meaning"
# What the tools answered, as the model read them, in the run whose prompt holds
# $1: the journal's receipts of that session.
cat > "$RX/receipts.py" <<'EOF'
import glob, json, os, sqlite3, sys
sys.stdout.reconfigure(encoding="utf-8")
for db in glob.glob(os.path.join(sys.argv[1], "projects", "*", "harness.sqlite3")):
    rows = sqlite3.connect(f"file:{db}?mode=ro", uri=True).execute(
        "select session_id, event_json from events order by session_id, sequence").fetchall()
    sessions = {session for session, text in rows
                if json.loads(text)["event_type"] == "input.admitted" and sys.argv[2] in text}
    for session, text in rows:
        event = json.loads(text)
        if session in sessions and event["event_type"] == "receipt.recorded":
            print((event["payload"].get("model_view") or {}).get("text", "").replace("\n", "\\n"))
EOF
tool_result() { "$PY" "$RX/receipts.py" "$D" "$1"; }
# Every event of that session, as JSON text.
cat > "$RX/session.py" <<'EOF'
import glob, json, os, sqlite3, sys
sys.stdout.reconfigure(encoding="utf-8")
for db in glob.glob(os.path.join(sys.argv[1], "projects", "*", "harness.sqlite3")):
    rows = sqlite3.connect(f"file:{db}?mode=ro", uri=True).execute(
        "select session_id, event_json from events order by session_id, sequence").fetchall()
    sessions = {session for session, text in rows
                if json.loads(text)["event_type"] == "input.admitted" and sys.argv[2] in text}
    for session, text in rows:
        if session in sessions:
            print(text)
EOF
run_exec m12 "Kiểm thử: gọi tool memory action search đúng một lần với meta {\"type\": \"decision\"} và không có query. Không gọi tool nào khác. MARKER_META. Báo các id tìm được."
check "a search by meta alone browses" 'tool_result MARKER_META | grep -q "\"mode\":\"filter\"" && tool_result MARKER_META | grep -q "queue-choice"'
run_exec m13 "Kiểm thử: gọi tool memory action search đúng một lần với meta_predicate '\$.confidence == \"high\" && !exists(\$.archived)' và không có query. Không gọi tool nào khác. MARKER_PREDICATE. Báo các id tìm được."
check "a JSONPath predicate filters by meta" 'tool_result MARKER_PREDICATE | grep -q "queue-choice"'
cat > "$RX/embed.py" <<'EOF'
import json, sys
from http.server import BaseHTTPRequestHandler, HTTPServer
port_file, log_file = sys.argv[1], sys.argv[2]
def vector(text):
    t = text.lower()
    if "sầu riêng" in t or "gai" in t:
        return [1.0, 0.0, 0.0]
    return [0.0, 0.0, 1.0]
class Fake(BaseHTTPRequestHandler):
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        with open(log_file, "a", encoding="utf-8") as log:
            for text in body["input"]:
                log.write(json.dumps(text, ensure_ascii=False) + "\n")
        data = [{"index": i, "embedding": vector(text)} for i, text in enumerate(body["input"])]
        out = json.dumps({"data": data}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(out)))
        self.end_headers()
        self.wfile.write(out)
    def log_message(self, *args):
        pass
server = HTTPServer(("127.0.0.1", 0), Fake)
open(port_file, "w").write(str(server.server_address[1]))
server.serve_forever()
EOF
"$PY" "$RX/embed.py" "$RX/embed.port" "$RX/embed.log" &
embedder=$!
for _ in $(seq 1 50); do [ -s "$RX/embed.port" ] && break; sleep 0.2; done
mkdir -p "$D/knowledge"
printf '{"schema_version":1,"enabled":true,"endpoint":"http://127.0.0.1:%s/v1/embeddings","model":"fake-embed"}' "$(cat "$RX/embed.port")" > "$D/knowledge/.ha-embedding.json"
"$PY" - "$G" <<'EOF'
import json, os, sys
entry = {"id": "durian", "kind": "memory", "title": "Mùa trái cây", "content": "Sầu riêng Ri6 thu hoạch vào tháng sáu ở miền Tây.",
         "path": "garden", "scope": "global", "version": 1, "source": "agent", "updated_at": "2026-10-01T00:00:00Z"}
json.dump({"schema": 1, "entries": {"memory": {"durian": entry}}}, open(os.path.join(sys.argv[1], "harness_state.json"), "w", encoding="utf-8"), ensure_ascii=False)
EOF
run_exec m14 "Kiểm thử: gọi tool memory action search đúng một lần với semantic 'loại quả nhiều gai, mùi rất nồng' (không có query). Không gọi tool nào khác. MARKER_SEMANTIC. Báo id và nội dung kết quả đầu tiên."
kill "$embedder" 2>/dev/null
check "the entries were embedded" 'grep -q "Ri6" "$RX/embed.log"'
check "search by meaning found what no word of it named" 'tool_result MARKER_SEMANTIC | grep -q "\"mode\":\"semantic\"" && tool_result MARKER_SEMANTIC | grep -q "durian"'
rm -f "$D/knowledge/.ha-embedding.json"

echo "################ 10. P4: a knowledge page's history and revert, one search over both, history filters"
"$HA" knowledge on --cwd "$P" >/dev/null 2>&1
run_exec m15 "Dùng tool knowledge action write (scope project) với path 'ops/deploy.md', title 'Deploy', summary 'Lịch deploy của cửa hàng', content 'Deploy vào thứ Năm hằng tuần.'. Chỉ ghi."
run_exec m16 "Dùng tool knowledge: đọc ops/deploy.md (scope project) để lấy hash, rồi write cập nhật content thành 'Deploy vào thứ Sáu hằng tuần.' với expected_hash đó. Chỉ cập nhật."
K="$P/.harness/knowledge"
check "the page now says Friday" 'grep -q "thứ Sáu" "$K/ops/deploy.md"'
run_exec m17 "Dùng tool knowledge action history với path 'ops/deploy.md' (scope project), tìm phiên bản đầu tiên (operation insert), đọc trang để lấy hash hiện tại, rồi gọi action revert với path đó, version là id của phiên bản đầu tiên và expected_hash là hash hiện tại. Báo kết quả."
check "the page was reverted to Thursday" 'grep -q "thứ Năm" "$K/ops/deploy.md" && ! grep -q "thứ Sáu" "$K/ops/deploy.md"'
check "its history holds every version" '[ "$("$PY" -c "import sqlite3,sys; print(sqlite3.connect(sys.argv[1]).execute(\"select count(*) from page_event where path = '\''ops/deploy.md'\''\").fetchone()[0])" "$K/.ha-history.sqlite3")" -ge 3 ]'
run_exec m18 "Kiểm thử: gọi tool memory action search đúng một lần với query 'deploy' và sources ['memory', 'knowledge']. Không gọi tool nào khác. MARKER_SOURCES. Báo nguồn của từng kết quả."
check "one search read memory and knowledge" 'tool_result MARKER_SOURCES | grep -q "\"source\":\"knowledge\"" && tool_result MARKER_SOURCES | grep -q "\"source\":\"memory\""'
cat > "$RX/history_results.py" <<'EOF'
import glob, json, os, re, sqlite3, sys
sys.stdout.reconfigure(encoding="utf-8")
# The history tools' receipts in the run whose prompt holds the marker.
results = []
for db in glob.glob(os.path.join(sys.argv[1], "projects", "*", "harness.sqlite3")):
    rows = sqlite3.connect(f"file:{db}?mode=ro", uri=True).execute(
        "select session_id, event_json from events order by session_id, sequence").fetchall()
    sessions = {session for session, text in rows
                if json.loads(text)["event_type"] == "input.admitted" and sys.argv[2] in text}
    for session, text in rows:
        event = json.loads(text)
        if session in sessions and event["event_type"] == "receipt.recorded":
            view = (event["payload"].get("model_view") or {}).get("text", "")
            if view.startswith("history_search") or view.startswith("history_read"):
                results.append(view)
mode = sys.argv[3]
if mode == "filtered":
    # A search whose hits are all of one kind, and another whose hits are not.
    kinds = [set(re.findall(r" kind=(\S+) ", r)) for r in results if r.startswith("history_search")]
    sys.exit(0 if any(len(k) == 1 for k in kinds) and any(len(k) > 1 for k in kinds) else 1)
if mode == "linked":
    sys.exit(0 if any(r.startswith("history_read") and (" previous=" in r or " next=" in r) for r in results) else 1)
EOF
run_exec m19 "Kiểm thử history: (1) gọi history_search với query 'deploy' và scope 'project', không bộ lọc, rồi xem các kind trong kết quả; (2) gọi lại history_search với cùng query, scope 'project', kind là kind của kết quả đầu tiên và since là ngày hôm nay (dạng YYYY-MM-DD); (3) gọi history_read với source_id của kết quả đầu tiên (scope 'project'). MARKER_HISTORY. Báo lại số kết quả mỗi lần và previous/next của lần đọc."
# The turn's last request holds every tool result it had.
check "a history search filters by kind and time" '"$PY" "$RX/history_results.py" "$D" MARKER_HISTORY filtered'
check "history_read names the sources around it" '"$PY" "$RX/history_results.py" "$D" MARKER_HISTORY linked'

echo "################ 11. 0.8.2: names per tree, patches, the immutable id, filters at parity"
# Every check here fails on 0.8.1.
run_exec m20 "Dùng tool memory action write với đúng các tham số: tree '/global/infra', name 'jwt-rotation', title 'JWT', content 'JWT keys rotate every 30 days.'. Không làm gì khác. Trả lời: đã ghi."
run_exec m21 "Dùng tool memory: đọc entry jwt-rotation để lấy version, rồi write với id 'jwt-rotation', expected_version đó và content 'JWT keys rotate every 14 days.' (không đổi title, không truyền name). Trả lời: đã cập nhật."
check "an update keeps the name it was given" 'row memory jwt-rotation | grep -q "|/global/infra|.*every 14 days"'
run_exec m22 "Kiểm thử: dùng tool memory: đọc entry jwt-rotation để lấy version, rồi gọi write với id 'jwt-rotation', expected_version đó và chỉ meta {\"owner\": \"platform\"} - KHÔNG truyền content hay title. MARKER_PATCH. Báo kết quả của lần write."
check "an update changes only what it gives" 'row memory jwt-rotation | grep -q "\"owner\": *\"platform\".*every 14 days" && ! "$PY" "$RX/session.py" "$D" MARKER_PATCH | grep -q "needs content"'
run_exec m23 "Dùng tool memory action write với đúng các tham số: tree '/global/apps', name 'jwt-rotation', title 'Mobile login', content 'The mobile app keeps its login in the operating system keychain.', distinct true. Không làm gì khác. Báo id và name mà tool trả về."
apps=$("$PY" "$MEMORY_STORE" named "$G" memory /global/apps jwt-rotation)
check "another tree holds the same name" '[ -n "$apps" ] && [ "$apps" != "jwt-rotation" ]'
run_exec m24 "Kiểm thử: dùng tool memory action search với query 'JWT rotate' để lấy memory_id của entry ở tree /global/infra, rồi gọi read với id là chính memory_id đó (không dùng id thường). MARKER_UUID. Báo memory_id và content đọc được."
check "the immutable id reads an entry" 'tool_result MARKER_UUID | grep -q "\"memory_id\":\"$(row memory jwt-rotation | cut -d"|" -f1)\"" && answer m24 | grep -q "14"'
run_exec m25 "Kiểm thử: gọi tool memory action history với id 'jwt-rotation' và limit 1. MARKER_PAGE. Báo version của phiên bản cũ được trả về và next_offset."
check "an entry's history pages" 'tool_result MARKER_PAGE | grep -q "\"next_offset\":1"'
"$PY" - "$G" <<'EOF'
import json, os, sys
def entry(key, title, content, path, meta=None):
    return {"id": key, "kind": "memory", "title": title, "content": content, "path": path, "scope": "global",
            "version": 1, "source": "agent", "metadata": meta or {}, "updated_at": "2026-10-20T00:00:00Z"}
entries = {
    "billing_release": entry("billing_release", "Billing release", "The billing service was deployed on Friday by the payments team.", "ops/release_train"),
    "audit_due": entry("audit_due", "Security audit", "The yearly security audit is due mid November.", "ops/compliance", {"due": "2026-11-15T09:00:00Z"}),
}
json.dump({"schema": 1, "entries": {"memory": entries}}, open(os.path.join(sys.argv[1], "harness_state.json"), "w", encoding="utf-8"))
EOF
run_exec m26 "Kiểm thử: gọi tool memory action search đúng một lần với query 'deploying' và weights {\"semantic\": 0}. Không gọi tool nào khác. MARKER_STEM. Báo id các kết quả."
check "words match by their English stem" 'tool_result MARKER_STEM | grep -q "billing_release"'
run_exec m27 "Kiểm thử: gọi tool memory action search đúng một lần với tree '/global/ops/train%' và không có query. Không gọi tool nào khác. MARKER_WORDS. Báo id các kết quả."
check "an lquery % word matches anywhere in a label" 'tool_result MARKER_WORDS | grep -q "billing_release"'
run_exec m28 "Kiểm thử: gọi tool memory action search đúng một lần với meta_predicate '\$.due.datetime() < \"2026-12-01\".datetime()' và không có query. Không gọi tool nào khác. MARKER_DATETIME. Báo id các kết quả."
check "a JSONPath predicate compares datetimes" 'tool_result MARKER_DATETIME | grep -q "audit_due"'
run_exec m29 "Kiểm thử: gọi tool memory action search đúng một lần với meta_predicate '\$.owner.lowercase() == \"platform\"' và không có query. Không gọi tool nào khác. MARKER_METHOD. Báo nguyên văn kết quả hoặc lỗi."
# A call refused before it runs leaves no receipt: read the whole session.
check "an unknown JSONPath method is refused" '"$PY" "$RX/session.py" "$D" MARKER_METHOD | grep -q "not a method"'
run_exec m30 "Kiểm thử: gọi tool memory action search đúng một lần với query 'deploy', sources ['memory', 'knowledge'] và tree '/knowledge/project/ops'. Không gọi tool nào khác. MARKER_KTREE. Báo nguồn và tree của từng kết quả."
check "a tree filter reaches knowledge pages" 'tool_result MARKER_KTREE | grep -q "\"tree\":\"/knowledge/project/ops\""'
run_exec m31 "Kiểm thử: gọi tool knowledge action history với path 'ops/deploy.md' (scope project), chọn phiên bản có operation update xảy ra sớm nhất, rồi gọi lại knowledge action history với path đó và version là id của phiên bản ấy để đọc nguyên văn trang lúc đó. MARKER_OLDPAGE. Báo nội dung trang ở phiên bản đó."
check "an old version of a page reads whole" 'tool_result MARKER_OLDPAGE | grep -q "thứ Sáu hằng tuần"'
run_exec m32 "Kiểm thử: gọi tool memory action status đúng một lần. Không gọi tool nào khác. MARKER_STATUS. Báo số entry mỗi scope."
check "status says what each scope holds" 'tool_result MARKER_STATUS | grep -q "\"embedding\""'
before=$(row memory "$apps" | cut -d'|' -f1)
run_exec m33 "Dùng tool memory: đọc entry '$apps' để lấy version, rồi write với id '$apps', expected_version đó và chỉ tree '/local/notes' (không truyền content). Không tạo entry mới, không xóa gì. Trả lời: đã chuyển."
check "a write to another scope moves the entry, keeping its id" '[ -n "$before" ] && [ -z "$(row memory "$apps")" ] && "$PY" -c "import glob,os,sqlite3,sys; sys.exit(0 if any(sqlite3.connect(f\"file:{p}?mode=ro\", uri=True).execute(\"select 1 from memory where id = ?\", (sys.argv[2],)).fetchone() for p in glob.glob(os.path.join(sys.argv[1], \"sessions\", \"*\", \"harness\", \"memory.sqlite3\"))) else 1)" "$D" "$before"'

echo "################ 12. 0.8.3: the memory readout (MEMORILLA, docs/architecture/MEMORY_READOUT.en.md)"
# A fake embedding server placing a text by the themes it names; every check
# here fails on 0.8.2.
cat > "$RX/themes.py" <<'EOF'
import json, sys
from http.server import BaseHTTPRequestHandler, HTTPServer
port_file = sys.argv[1]
THEMES = [
    ("anaemia", "anemia", "iron", "eat", "health", "thiếu máu", "sức khỏe"),
    ("deploy", "release", "freeze"),
    ("password", "mật khẩu", "sign in", "đăng nhập", "credential"),
]
def vector(text):
    t = text.lower()
    v = [1.0 if any(word in t for word in theme) else 0.0 for theme in THEMES]
    return v + [0.0 if any(v) else 1.0]
class Fake(BaseHTTPRequestHandler):
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        data = [{"index": i, "embedding": vector(text)} for i, text in enumerate(body["input"])]
        out = json.dumps({"data": data}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(out)))
        self.end_headers()
        self.wfile.write(out)
    def log_message(self, *args):
        pass
server = HTTPServer(("127.0.0.1", 0), Fake)
open(port_file, "w").write(str(server.server_address[1]))
server.serve_forever()
EOF
"$PY" "$RX/themes.py" "$RX/themes.port" &
themes=$!
for _ in $(seq 1 50); do [ -s "$RX/themes.port" ] && break; sleep 0.2; done
printf '{"schema_version":1,"enabled":true,"endpoint":"http://127.0.0.1:%s/v1/embeddings","model":"fake-themes"}' "$(cat "$RX/themes.port")" > "$D/knowledge/.ha-embedding.json"
"$PY" - "$G" <<'EOF'
import json, os, sys
entry = {"id": "user_health", "kind": "memory", "title": "Health", "content": "The user has iron-deficiency anaemia; they avoid tea with meals.",
         "path": "health", "scope": "global", "version": 1, "source": "agent", "updated_at": "2026-10-20T00:00:00Z"}
json.dump({"schema": 1, "entries": {"memory": {"user_health": entry}}}, open(os.path.join(sys.argv[1], "harness_state.json"), "w", encoding="utf-8"))
EOF
run_exec m34 "Kiểm thử: gọi tool memory action search đúng một lần với mode 'readout', query 'what do I know about the user health and about the release process' và slots 4. Không gọi tool nào khác. MARKER_READOUT. Báo id các entry trong từng slot."
check "a readout reads a two-part request into slots by meaning" 'tool_result MARKER_READOUT | grep -q "\"mode\":\"readout\"" && tool_result MARKER_READOUT | grep -q "\"encoder\":\"embeddings\"" && tool_result MARKER_READOUT | grep -q "user_health" && tool_result MARKER_READOUT | grep -q "deploy_day"'
run_exec m35 "Lượt này không dùng tool nào. Before my long run tomorrow, what should I eat for breakfast? Chỉ trả lời đúng một từ: MARKER_RECALL"
R=$(request "MARKER_RECALL")
check "the digest recalls by meaning an entry no word of the request is in" 'printf "%s" "$R" | grep -q "user_health\] Health ([^)]*) \[recalled by meaning\]"'
run_exec m36 "Ghi nhận (không dùng tool nào): hôm qua không vào được staging vì mật khẩu staging đã hết hạn. Trả lời: đã ghi nhận."
run_exec m37 "Kiểm thử: gọi tool history_search đúng một lần với query 'why could we not sign in to staging', scope 'project' và mode 'readout'. Không gọi tool nào khác. MARKER_HREADOUT. Báo các nguồn và slot của chúng."
check "history_search reads earlier conversations into slots" 'tool_result MARKER_HREADOUT | grep -q "^history_search.*slot=1 attention=" && tool_result MARKER_HREADOUT | grep -q "mật khẩu staging"'
kill "$themes" 2>/dev/null
rm -f "$D/knowledge/.ha-embedding.json"

echo "################ 7. ha memory export"
"$HA" memory export > "$RX/export.json" 2>"$RX/export.err"
check "it exports the global store" '"$PY" -c "import json,sys; d=json.load(open(sys.argv[1],encoding=\"utf-8\")); sys.exit(0 if \"queue-choice\" in d[\"document\"][\"entries\"][\"memory\"] and d[\"document\"][\"refinements\"][0][\"id\"] == \"refine_0001\" else 1)" "$RX/export.json"'

result
