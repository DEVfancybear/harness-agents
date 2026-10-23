#!/usr/bin/env bash
# Knowledge, Self-Evolution and Deep Dream with a real model (0.6.2): the
# knowledge catalog ranked by the task, not the message; no secret in a page;
# a refinement's memory write held to the memory tool's checks; Deep Dream's
# guard rails; a review's page that is new, quoted and marked as the review's;
# the next turn told what a review changed. And 0.6.3: `ha exec` reviewing its
# conversation; a review whose apply cannot take the state's lock leaving no
# page, skill or refinement behind; a failed global part leaving the local one
# standing; a page's age in the catalog; a conversation's own memory
# consolidated past forty entries; hybrid search embedding every query variant
# (against a fake embedding server). Every check prints PASS or FAIL;
# the conversation and the state stay in $LIVE_ROOT/knowledge as evidence.
#
#   bash scripts/testing/live-tests/knowledge.sh
set -u
source "$(dirname "$0")/env.sh"
fresh knowledge
export HA_PYTHON=${HA_PYTHON:-$PY}
export HA_AUTO_REFINE=off
D=$HA_HOME/data
P="$RX/p-kb"
project "$P"
K="$P/.harness/knowledge"

# The request a turn sent, found by a phrase of its prompt.
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
# Global memory entries as "id|path|pinned|content".
entries() { "$PY" - "$D" <<'EOF'
import json, os, sys
sys.stdout.reconfigure(encoding="utf-8")
f = os.path.join(sys.argv[1], "harness", "harness_state.json")
s = json.load(open(f, encoding="utf-8")) if os.path.exists(f) else {"entries": {}}
for kind in ("memory", "prompt"):
    for k, e in (s["entries"].get(kind) or {}).items():
        print("|".join([k, str(e.get("path", "general")), str((e.get("metadata") or {}).get("pinned", False)), str(e.get("content")).replace("\n", " ")]))
EOF
}
# Seed global memory entries, each argument "id|title|content|path|pinned".
seed() { "$PY" - "$D" "$@" <<'EOF'
import json, os, sys
path = os.path.join(sys.argv[1], "harness", "harness_state.json")
state = json.load(open(path, encoding="utf-8")) if os.path.exists(path) else {"schema": 1, "entries": {}, "refinements": []}
for line in sys.argv[2:]:
    if not line.strip():
        continue
    key, title, content, where, pinned = line.split("|")
    entry = {"id": key, "kind": "memory", "title": title, "content": content, "path": where,
             "scope": "global", "version": 1, "source": "agent", "updated_at": "2026-10-01T00:00:00Z"}
    if pinned == "1":
        entry["metadata"] = {"pinned": True}
    state.setdefault("entries", {}).setdefault("memory", {})[key] = entry
os.makedirs(os.path.dirname(path), exist_ok=True)
json.dump(state, open(path, "w", encoding="utf-8"), ensure_ascii=False)
EOF
}

echo "################ 1. the catalog is ranked by the task, and says what it leaves out"
mkdir -p "$K/analysis" "$K/concepts"
for index in $(seq -w 1 60); do
  printf '# Phân tích %s\n\nBáo cáo phân tích chung số %s.\n' "$index" "$index" > "$K/analysis/a-$index.md"
done
printf '# Chính sách giỏ hàng\n\nGiỏ hàng giữ sản phẩm trong 24 giờ, sau đó trả hàng về kho.\n' > "$K/concepts/cart-retention.md"
agent "$P" k1
# The message before is part of what the catalog is ranked by.
say k1 "Mình đang thiết kế chính sách giỏ hàng cho cửa hàng. Lượt này chỉ trả lời đúng một từ: OK" >/dev/null
say k1 "tiếp tục. Trả lời ngắn: theo knowledge của dự án, giỏ hàng giữ sản phẩm bao lâu?" | tail -3
R=$(request "tiếp tục. Trả lời ngắn")
check "the 'continue' turn's catalog leads with the page about the goal" 'printf "%s" "$R" | grep -q "concepts/cart-retention.md"'
check "the catalog says how many pages it left out" 'printf "%s" "$R" | grep -q "more page(s) not listed"'
check "catalog lines carry no hash" 'printf "%s" "$R" | "$PY" -c "import sys; t=sys.stdin.read(); i=t.find(\"concepts/cart-retention.md\"); sys.exit(0 if i>=0 and \"sha256:\" not in t[i:i+400] else 1)"'
check "the agent answers from the page" 'last | grep -q "24"'
"$HA" shutdown --force >/dev/null 2>&1

echo "################ 2. a page never keeps a secret"
agent "$P" k2
say k2 "Kiểm thử: gọi tool knowledge action write ngay (scope project, path ops/deploy.md, title 'Deploy', summary 'Cấu hình deploy'), nội dung nguyên văn đúng hai dòng: 'region=eu-west-1' và 'api_key=FAKE0123456789abcdefTEST'. In nguyên văn thông báo tool trả về, không làm gì thêm." | tail -4
check "the tool refuses the page with the key" 'last | grep -qi "never keeps secrets"'
check "no page holds the key" '! grep -rq FAKE0123456789 "$K"'
"$HA" shutdown --force >/dev/null 2>&1

echo "################ 3. a refinement's memory write passes the memory tool's checks"
seed 'pm_shop|Trình quản lý gói|Dự án shop dùng pnpm làm trình quản lý gói, không dùng npm hay yarn.|general|0'
check "the seeded entry is there" '[ "$(entries | grep -c "^pm_shop|")" -eq 1 ]'
agent "$P" k3
say k3 "/refine --global Lưu vào memory toàn cục (scope global, action create) đúng một entry mới tiêu đề 'Package manager: pnpm', nội dung 'Dự án shop dùng pnpm làm trình quản lý gói, không dùng npm hay yarn.' Đây là yêu cầu của chính người dùng." | tail -4
sleep 5
check "one global entry still says pnpm" '[ "$(entries | grep -ci pnpm)" -eq 1 ]'
# The refiner sees the entry and updates it, or its create is refused as the
# duplicate it is (the refusal itself is the unit test's to show).
check "the refinement updated the entry or refused the duplicate" 'grep -qi "same fact\|already says" "$D"/harness/refinements.jsonl || grep "\"action\":\"update\"" "$D"/harness/refinements.jsonl | grep -q "\"id\":\"pm_shop\""'
"$HA" shutdown --force >/dev/null 2>&1

echo "################ 4. Deep Dream keeps its guard rails"
lines=('img_rule|Quy tắc ảnh|Mọi ảnh mới phải là WebP.|constraints|0' 'reply_lang|Ngôn ngữ trả lời|Luôn trả lời bằng tiếng Việt, ngắn gọn.|general|1')
for pair in "font|Font chữ của blog là Inter" "color|Màu chủ đạo của blog là xanh navy" "host|Blog được host trên Netlify" "cms|Bài viết blog soạn bằng Markdown trong thư mục content" "seo|Mỗi bài blog cần thẻ meta description" "rss|Blog có RSS feed ở /rss.xml"; do
  key=${pair%%|*}; fact=${pair#*|}
  lines+=("${key}_a|${key} (1)|${fact}.|blog|0" "${key}_b|${key} (2)|${fact} - ghi lại lần nữa.|blog|0")
done
seed "${lines[@]}"
echo 20 > "$D/harness/memory_changes"
before=$(entries | wc -l)
unset HA_AUTO_REFINE
agent "$P" k4
say k4 "Chào bạn, trả lời đúng một từ: OK" >/dev/null
for _ in $(seq 1 30); do [ -f "$D/harness/memory_consolidated_changes" ] && break; sleep 10; done
after=$(entries | wc -l)
echo "   entries: $before -> $after"
entries | sed 's/^/   /'
check "the consolidation ran" '[ -f "$D/harness/memory_consolidated_changes" ]'
check "it merged: fewer entries" '[ "$after" -lt "$before" ]'
check "at most half were deleted" '[ "$after" -ge $((before / 2)) ]'
check "the constraint is still under constraints" 'entries | grep -i webp | grep -q "|constraints"'
check "the pinned entry is still pinned" 'entries | grep -i "tiếng việt" | grep -q "|True|"'
"$HA" shutdown --force >/dev/null 2>&1
export HA_AUTO_REFINE=off

echo "################ 5. a review's page is a new subject, quoted, and marked as the review's"
for index in $(seq -w 1 90); do
  printf '# Ghi chú %s\n\nGhi chú vận hành chung số %s.\n' "$index" "$index" > "$K/analysis/note-$index.md"
done
printf '# Retry backoff\n\nExponential backoff for retries of the API client: double the wait each time.\n' > "$K/concepts/retry-backoff.md"
echo '{"autoRefine": {"turnInterval": 1}}' > "$HA_HOME/settings.json"
unset HA_AUTO_REFINE
agent "$P" k5
touch "$RX/review5.marker"
say k5 "Lượt này bạn đừng tự gọi tool memory hay knowledge, chỉ trả lời. Một quy ước lâu dài của dự án này mà mình muốn được nhớ ở các lần sau: tên nhánh git luôn theo dạng feat/<mô-tả-ngắn>. Client API của mình gọi lại khi lỗi 503: mình chốt là chờ 1 giây, rồi gấp đôi mỗi lần (exponential backoff), tối đa 30 giây và 6 lần thử. Giải thích ngắn vì sao nên thêm jitter ngẫu nhiên vào thời gian chờ." | tail -4
for _ in $(seq 1 18); do find "$D/sessions" -name review_watermark -newer "$RX/review5.marker" | grep -q . && break; sleep 10; done
sleep 5
check "the review ran" 'find "$D/sessions" -name review_watermark -newer "$RX/review5.marker" | grep -q .'
echo "   pages about backoff: $(grep -rli backoff "$K" | sed "s#$K/##" | tr '\n' ' ')"
check "no second page on the backoff subject without an origin" '[ "$(grep -rli backoff "$K" | grep -v log.md | wc -l)" -eq 1 ] || grep -rl backoff "$K" | grep -v "retry-backoff\|log.md\|index.md" | xargs grep -l "\"origin\":\"review\"" | grep -q .'
check "a page the review wrote is marked so in the log" '! grep -q "\"origin\":\"review\"" -r "$K" || grep -q " by review" "$K/log.md"'

echo "################ 6. the next turn is told what the review changed"
if cat "$D"/sessions/*/harness/refinements.jsonl 2>/dev/null | grep -q '"applied":true'; then
  say k5 "Lần review chạy nền vừa rồi đã thay đổi gì trong harness? Trả lời ngắn." | tail -4
  R=$(request "Lần review chạy nền vừa rồi")
  check "the turn's request carries what the review changed" 'printf "%s" "$R" | grep -q "a review that ran beside the conversation changed the harness"'
  check "with the id that undoes it" 'printf "%s" "$R" | grep -q "refine --rollback"'
else
  say k5 "Theo bạn exponential backoff có nhược điểm gì? Trả lời một câu." >/dev/null
  sleep 60
  say k5 "Lần review chạy nền vừa rồi đã thay đổi gì trong harness? Trả lời ngắn." | tail -4
  R=$(request "Lần review chạy nền vừa rồi")
  check "the turn's request carries what the review changed" 'printf "%s" "$R" | grep -q "a review that ran beside the conversation changed the harness"'
  check "with the id that undoes it" 'printf "%s" "$R" | grep -q "refine --rollback"'
fi
"$HA" shutdown --force >/dev/null 2>&1
rm -f "$HA_HOME/settings.json"

# ---- 0.6.3 ---------------------------------------------------------------
# run_exec <name> [--resume TASK] <prompt>: a headless run in $P; its JSON in
# $RX/<name>.json, its stderr (where the review reports) in $RX/<name>.err.
run_exec() {
  local name="$1"; shift
  printf '\n## %s (ha exec %s)\n\n> %s\n\n' "$(date +%H:%M:%S)" "$name" "${*: -1}" >> "$LOG"
  (cd "$P" && "$HA" exec --model "$MODEL" "${TH[@]}" --approval full-auto --output-format json "$@" >"$RX/$name.json" 2>"$RX/$name.err")
  "$PY" -c "import json,sys; print(json.load(open(sys.argv[1],encoding='utf-8')).get('response',''))" "$RX/$name.json" >> "$LOG" 2>/dev/null
  sed 's/^/    /' "$RX/$name.err" >> "$LOG"
}
task_of() { "$PY" -c "import json,sys; print(json.load(open(sys.argv[1],encoding='utf-8'))['task_id'])" "$RX/$1.json"; }
# hold <lock file> <seconds>: hold a harness state lock from another process,
# as a writer that never lets go would. Sets HOLDER to its pid.
hold() {
  rm -f "$1.held"
  "$PY" "$RX/hold.py" "$1" "$2" &
  HOLDER=$!
  for _ in $(seq 1 50); do [ -f "$1.held" ] && break; sleep 0.2; done
}
cat > "$RX/hold.py" <<'EOF'
import msvcrt, os, sys, time
path = sys.argv[1]
os.makedirs(os.path.dirname(path), exist_ok=True)
fd = os.open(path, os.O_RDWR | os.O_CREAT)
msvcrt.locking(fd, msvcrt.LK_NBLCK, 1)
open(path + ".held", "w").close()
time.sleep(float(sys.argv[2]))
EOF
# Memory entries and prompt notes in a scope's state file.
count_entries() { "$PY" -c "import json,sys,os; p=sys.argv[1]; s=json.load(open(p,encoding='utf-8')) if os.path.exists(p) else {}; print(sum(len((s.get('entries') or {}).get(k) or {}) for k in ('memory','prompt')))" "$1"; }
history_lines() { if [ -f "$1" ]; then wc -l < "$1" | tr -d ' '; else echo 0; fi; }

echo '{"autoRefine": {"turnInterval": 1}}' > "$HA_HOME/settings.json"
unset HA_AUTO_REFINE

echo "################ 7. ha exec reviews its conversation, as the app does"
touch "$RX/exec7.marker"
run_exec e7 "Lượt này đừng gọi tool memory hay knowledge, chỉ trả lời. Quy ước lâu dài của dự án này: mọi migration SQL đặt trong thư mục db/migrations, tên file dạng YYYYMMDD_mo-ta.sql. Giải thích ngắn vì sao nên đặt ngày ở đầu tên file."
T=$(task_of e7)
L="$D/sessions/$T/harness"
check "the run says it reviews before exiting" 'grep -q "auto-refine: reviewing the last 1 turn" "$RX/e7.err"'
check "the review ran and marked the conversation reviewed" '[ "$L/review_watermark" -nt "$RX/exec7.marker" ]'
check "the run's count restarted with the review" 'grep -q "\"turns\":0" "$L/review_cadence.json"'
check "the answer is in the result" '"$PY" -c "import json,sys; sys.exit(0 if json.load(open(sys.argv[1],encoding=\"utf-8\"))[\"response\"] else 1)" "$RX/e7.json"'

echo "################ 8. a review that cannot take the lock leaves no page or skill behind"
before=$(history_lines "$L/refinements.jsonl")
touch "$RX/exec8.marker"
hold "$L/harness_state.json.lock" 600
run_exec e8 --resume "$T" "Lượt này đừng gọi tool memory hay knowledge, chỉ trả lời. Mình vừa chốt cách tính phí ship của cửa hàng: đơn dưới 300 nghìn tính 30 nghìn, từ 300 nghìn đến dưới 1 triệu tính 15 nghìn, từ 1 triệu trở lên miễn phí; vùng xa cộng thêm 20 nghìn. Đây là quy tắc lâu dài của cửa hàng, hãy giải thích ngắn quy tắc này cho khách."
kill "$HOLDER" 2>/dev/null; wait "$HOLDER" 2>/dev/null
check "the review's apply timed out on the lock" 'grep -q "refine failed: waiting for another memory write timed out" "$RX/e8.err"'
check "no refinement was recorded" '[ "$(history_lines "$L/refinements.jsonl")" -eq "$before" ]'
check "no pending record is left" '[ -z "$(ls -A "$L/pending_refinements" 2>/dev/null)" ]'
check "no page the review wrote is left" '[ -z "$(find "$K" -name "*.md" -newer "$RX/exec8.marker" ! -name log.md ! -name index.md -exec grep -l "\"origin\":\"review\"" {} +)" ]'
check "what it wrote, it undid" '! grep -q "could not undo" "$RX/e8.err"'

echo "################ 9. a global part that fails leaves the local refinement standing"
before=$(history_lines "$L/refinements.jsonl")
touch "$RX/exec9.marker"
hold "$D/harness/harness_state.json.lock" 600
run_exec e9 --resume "$T" "Lượt này đừng gọi tool memory hay knowledge, chỉ trả lời. Từ nay về sau, ở mọi dự án và mọi cuộc trò chuyện, mình muốn bạn luôn trả lời bằng tiếng Việt, xưng 'mình' và gọi mình là 'bạn'. Còn trong cuộc trò chuyện này: bước tiếp theo là viết test cho hàm tính phí ship. Trả lời: đã rõ."
kill "$HOLDER" 2>/dev/null; wait "$HOLDER" 2>/dev/null
check "the global part's failure is reported" 'grep -q "the global memory part failed: waiting for another memory write timed out" "$RX/e9.err"'
check "the local refinement is recorded" '[ "$(history_lines "$L/refinements.jsonl")" -gt "$before" ]'
check "and the conversation is marked reviewed" '[ "$L/review_watermark" -nt "$RX/exec9.marker" ]'

echo "################ 10. a page knows its age"
export HA_AUTO_REFINE=off
printf '# Quy trình hoàn tiền\n\nKhách được hoàn tiền trong 7 ngày nếu hàng lỗi.\n' > "$K/concepts/refund-policy.md"
touch -d "12 days ago" "$K/concepts/refund-policy.md"
run_exec e10 "Theo knowledge của dự án, hàng lỗi được hoàn tiền trong bao nhiêu ngày? Sau đó gọi tool knowledge action write (scope project, path ops/runbook.md, title 'Runbook', summary 'Các bước vận hành') với nội dung đúng một dòng 'Khởi động lại worker trước khi xả hàng đợi.' Trả lời ngắn."
R=$(request "hàng lỗi được hoàn tiền trong bao nhiêu ngày")
check "the catalog shows a hand-written page's age from its file" 'printf "%s" "$R" | grep -q "refund-policy.md[^}]*updated[^}]*12d ago"'
check "a page the store writes records its time" 'head -1 "$K/ops/runbook.md" | grep -q "\"updated_at\":\"20"'
unset HA_AUTO_REFINE

echo "################ 11. a conversation's own memory is consolidated past forty entries"
"$PY" - "$L/harness_state.json" <<'EOF'
import json, os, sys
path = sys.argv[1]
state = json.load(open(path, encoding="utf-8")) if os.path.exists(path) else {"schema": 1, "entries": {}, "refinements": []}
memory = state.setdefault("entries", {}).setdefault("memory", {})
for n in range(45):
    key = f"ship_step_{n}"
    memory[key] = {"id": key, "kind": "memory", "title": f"Tiến độ phí ship {n}", "content": f"Đang làm phí ship: đã xong bước {n} trên 45.", "path": "progress", "scope": "local", "version": 1, "source": "agent", "updated_at": f"@{1790000000 + n * 60}"}
json.dump(state, open(path, "w", encoding="utf-8"), ensure_ascii=False)
EOF
seeded=$(count_entries "$L/harness_state.json")
run_exec e11 --resume "$T" "Lượt này đừng gọi tool memory hay knowledge. Trả lời đúng một từ: OK"
after=$(count_entries "$L/harness_state.json")
echo "   local entries: $seeded -> $after"
# The review runs first and may merge the 45 step entries itself (DeepSeek
# did: one entry saying steps 0-44 were done), which leaves nothing for the
# consolidation past forty. Either way the store shrinks; the half rule is
# the consolidation's own guard rail.
if grep -q "This conversation.s memory: [0-9]* -> [0-9]* entries" "$RX/e11.err"; then
  check "the consolidation ran on this conversation's memory" true
  check "at most half were deleted" '[ "$after" -ge $((seeded / 2)) ]'
else
  check "the review merged the step entries into one it created" 'grep -q "Created local memory" "$RX/e11.err" && grep -q "Deleted local memory .ship_step_" "$RX/e11.err"'
  check "and left the store under the consolidation's threshold" '[ "$after" -le 40 ]'
fi
check "it left fewer entries" '[ "$after" -lt "$seeded" ]'

echo "################ 12. hybrid search embeds every query variant"
export HA_AUTO_REFINE=off
cat > "$RX/embed.py" <<'EOF'
import json, sys
from http.server import BaseHTTPRequestHandler, HTTPServer
port_file, log_file = sys.argv[1], sys.argv[2]
def vector(text):
    t = text.lower()
    if "sầu riêng" in t or "có gai" in t:
        return [1.0, 0.0, 0.0]
    if "zzqx" in t:
        return [0.0, 1.0, 0.0]
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
printf '# Sầu riêng\n\nSầu riêng thu hoạch vào tháng sáu ở miền Tây.\n' > "$K/concepts/durian.md"
printf '{"schema_version":1,"enabled":true,"endpoint":"http://127.0.0.1:%s/v1/embeddings","model":"fake-embed"}' "$(cat "$RX/embed.port")" > "$K/.ha-embedding.json"
run_exec e12 "Kiểm thử: gọi tool knowledge action search đúng một lần, với queries là hai biến thể: 'zzqx' và 'trái cây có gai'. Không gọi tool nào khác. In nguyên văn path và matched_queries của từng kết quả."
# (0.7.9) History search by meaning with the same configuration: a turn names
# a durian without any word of the later query, and the query finds it.
run_exec e13 "Trả lời đúng một dòng: Món tôi thích nhất là sầu riêng Ri6."
run_exec e14 "Kiểm thử: gọi tool history_search đúng một lần với query 'trái cây có gai' và scope 'project'. Không gọi tool nào khác. In nguyên văn toàn bộ kết quả tool."
kill "$embedder" 2>/dev/null
check "the second variant was embedded" 'grep -q "\"trái cây có gai\"" "$RX/embed.log"'
check "history search embedded the turn that named the durian" 'grep -q "Ri6" "$RX/embed.log"'
check "history search found it by meaning, with none of the query words" 'grep -q "matched=0/" "$RX/e14.json" && grep -q "Ri6" "$RX/e14.json"'
request "trái cây có gai" > "$RX/e12.request"
cat > "$RX/matched.py" <<'EOF'
import json, sys
# The search result, as the next request carried it: the durian page is in it,
# found by the second variant (index 1).
found = False
def walk(value):
    global found
    if isinstance(value, str):
        # A tool result reads "knowledge: {...}".
        start = value.find("{")
        if start < 0:
            return
        try:
            value = json.JSONDecoder().raw_decode(value[start:])[0]
        except ValueError:
            return
    if isinstance(value, dict):
        if value.get("retrieval") == "hybrid":
            for hit in value.get("results", []):
                if hit.get("path") == "concepts/durian.md" and 1 in hit.get("matched_queries", []):
                    found = True
        for item in value.values():
            walk(item)
    elif isinstance(value, list):
        for item in value:
            walk(item)
text = open(sys.argv[1], encoding="utf-8").read().strip()
if text:
    walk(json.loads(text))
sys.exit(0 if found else 1)
EOF
check "the search ran hybrid, and found the page only the second variant's vector is near" '"$PY" "$RX/matched.py" "$RX/e12.request"'
rm -f "$K/.ha-embedding.json" "$HA_HOME/settings.json"
unset HA_AUTO_REFINE

echo "################ 13. the bug report carries the reviews"
(cd "$P" && "$HA" bug-report --since 2h --out "$RX/report.zip" --json > "$RX/report.json" 2>&1)
"$PY" - "$RX/report.zip" "$RX/report" <<'EOF'
import sys, zipfile
zipfile.ZipFile(sys.argv[1]).extractall(sys.argv[2])
EOF
check "the report records the reviews and the consolidation" 'grep -q "review.applied\|review.consolidated" "$RX/report/timeline.jsonl"'
check "it holds the new Deep Dream marks" '[ -f "$RX/report/harness/global/memory_changes" ]'

result
