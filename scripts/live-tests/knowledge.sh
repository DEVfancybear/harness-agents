#!/usr/bin/env bash
# Knowledge, Self-Evolution and Deep Dream with a real model (0.6.2): the
# knowledge catalog ranked by the task, not the message; no secret in a page;
# a refinement's memory write held to the memory tool's checks; Deep Dream's
# guard rails; a review's page that is new, quoted and marked as the review's;
# the next turn told what a review changed. Every check prints PASS or FAIL;
# the conversation and the state stay in $LIVE_ROOT/knowledge as evidence.
#
#   bash scripts/live-tests/knowledge.sh
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
check "the refinement recorded the duplicate it refused" 'cat "$D"/harness/refinements.jsonl "$D"/sessions/*/harness/refinements.jsonl 2>/dev/null | grep -qi "same fact\|already says"'
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

echo "################ 7. the bug report carries the reviews"
(cd "$P" && "$HA" bug-report --since 2h --out "$RX/report.zip" --json > "$RX/report.json" 2>&1)
"$PY" - "$RX/report.zip" "$RX/report" <<'EOF'
import sys, zipfile
zipfile.ZipFile(sys.argv[1]).extractall(sys.argv[2])
EOF
check "the report records the reviews and the consolidation" 'grep -q "review.applied\|review.consolidated" "$RX/report/timeline.jsonl"'
check "it holds the new Deep Dream marks" '[ -f "$RX/report/harness/global/memory_changes" ]'

result
