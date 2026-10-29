#!/usr/bin/env bash
# Long-memory evaluation with a real model, after LongMemEval: extraction,
# project isolation, knowledge update, temporal recall through history,
# abstention, duplicates, delete and restore, provenance, renamed ids, judged
# skill outcomes and the harness refinements switch. Several conversations over
# three projects; every check prints PASS or FAIL, and the conversation and
# the state on disk stay in $LIVE_ROOT/memory as evidence.
#
#   bash scripts/testing/live-tests/memory.sh
set -u
source "$(dirname "$0")/env.sh"
fresh memory
export HA_AUTO_REFINE=off
D=$HA_HOME/data
for p in p-shop p-blog p-skill; do project "$RX/$p"; done
# Global entries as "scope|project|kind|id|v|content|origin-via".
entries() {
  for kind in memory prompt skill subagent; do
    "$PY" "$MEMORY_STORE" rows "$D/harness" "$kind" | awk -F'|' -v kind="$kind" 'BEGIN{OFS="|"} {print "global",$5,kind,$1,$6,$4,$7}'
  done
}
# The global store's event log: each change and the version it left.
journal() { "$PY" "$MEMORY_STORE" events "$D/harness"; }
show() { echo "-- global memory on disk:"; entries | sed 's/^/   /'; }

echo "################ 1. extraction: a project fact and a global preference"
agent "$RX/p-shop" s1
say s1 "Ghi nhớ giúp tôi, mỗi điều một entry memory riêng. (1) Riêng cho dự án này: dự án dùng pnpm làm trình quản lý gói. (2) Riêng cho dự án này: deploy lên Fly.io vào thứ Năm hằng tuần. (3) Ở mọi dự án: tôi muốn bạn luôn trả lời bằng tiếng Việt, ngắn gọn." | tail -6
show
check "the pnpm fact is kept for project p-shop" "entries | grep -a '^global|p-shop|memory|' | grep -qi 'pnpm'"
check "the deploy fact is kept for project p-shop" "entries | grep -a '^global|p-shop|' | grep -qi 'fly'"
check "the language preference is global, untagged" "entries | grep -a '^global|-|' | grep -qi 'tiếng việt\|vietnamese'"
check "entries record where they were written" "entries | grep -a 'p-shop' | grep -q '|memory tool$'"
"$HA" shutdown --force >/dev/null 2>&1

echo "################ 2. project isolation: another project never sees p-shop's facts"
agent "$RX/p-blog" b1
say b1 "Chỉ dựa vào memory của bạn (không đọc file, không đoán): dự án này dùng trình quản lý gói nào và deploy ở đâu, khi nào? Và tôi muốn bạn trả lời bằng ngôn ngữ nào?" | tail -6
check "p-blog does not claim pnpm or Fly.io" "! last | grep -qi 'pnpm\|fly'"
check "p-blog still gets the global language preference" "last | grep -qi 'tiếng việt'"
"$HA" shutdown --force >/dev/null 2>&1

echo "################ 3. knowledge update: the deploy day changes, and so does its id"
agent "$RX/p-shop" s3
say s3 "Lịch deploy của dự án này đổi rồi: từ nay deploy vào thứ Sáu chứ không phải thứ Năm nữa. Cập nhật memory cho đúng, kể cả tiêu đề." | tail -4
show
check "the deploy entry now says Friday" "entries | grep -a '^global|p-shop|' | grep -i 'fly\|deploy' | grep -qi 'thứ sáu\|friday'"
check "no p-shop entry still claims Thursday" "! entries | grep -a '^global|p-shop|' | grep -i 'thứ năm\|thursday' | grep -vqi 'thứ sáu\|friday'"
check "the Thursday version is kept in history" "journal | grep -qi 'thứ năm\|thursday'"
check "the id no longer says Thursday" "! entries | grep -a '^global|p-shop|' | cut -d'|' -f4 | grep -qi 'thu_nam\|thursday'"

echo "################ 4. temporal recall through history"
say s3 "Trước lần đổi vừa rồi, lịch deploy là ngày nào? Hãy kiểm tra memory (history) chứ đừng dựa vào trí nhớ hội thoại này." | tail -4
check "the earlier deploy day is recalled" "last | grep -qi 'thứ năm\|thursday'"
"$HA" shutdown --force >/dev/null 2>&1

echo "################ 5. abstention: what was never said is not invented"
agent "$RX/p-shop" s5
say s5 "Theo memory, dự án này dùng database gì? Nếu memory không có thì nói rõ là không biết." | tail -4
check "it says it does not know" "last | grep -qi 'không có\|không biết\|chưa có\|không tìm thấy\|không nhớ\|chưa được ghi\|không ghi'"
# Naming a database it searched for is not claiming the project uses it.
check "it names no database" "! last | grep -qiE '(dùng|uses|using|sử dụng) (postgres|mysql|sqlite|mongo|redis)'"

echo "################ 6. duplicates: the same fact written again is refused by the tool"
say s5 "Kiểm thử: gọi tool memory action write ngay (không search trước, không truyền id), scope project, tiêu đề 'Trình quản lý gói', nội dung 'Dự án này dùng pnpm làm trình quản lý gói.'. In nguyên văn thông báo tool trả về, rồi làm theo hướng dẫn của nó." | tail -5
show
check "the tool refused it as the same fact" "last | grep -qi 'same fact\|alike\|distinct'"
check "one p-shop entry holds pnpm" "[ \$(entries | grep -a '^global|p-shop|' | grep -ci 'pnpm') -eq 1 ]"

echo "################ 7. delete, then restore"
say s5 "Xoá memory về chuyện deploy lên Fly.io của dự án này." | tail -3
check "the Fly.io entry is gone" "! entries | grep -a '^global|p-shop|' | grep -qi 'fly'"
say s5 "À tôi nhầm, khôi phục lại memory về deploy Fly.io vừa xoá." | tail -3
show
check "the Fly.io entry is back" "entries | grep -a '^global|p-shop|' | grep -qi 'fly'"
check "it came back through restore, at a higher version" "entries | grep -a '^global|p-shop|' | grep -i 'fly' | grep -qv '|v1|'"
"$HA" shutdown --force >/dev/null 2>&1

echo "################ 8. a new conversation in p-shop recalls the current facts"
agent "$RX/p-shop" s8
say s8 "Chỉ dựa vào memory: dự án này dùng package manager gì, deploy ở đâu và vào thứ mấy?" | tail -4
check "it recalls pnpm" "last | grep -qi 'pnpm'"
check "it recalls Fly.io on Friday" "last | grep -qi 'fly' && last | grep -qi 'thứ sáu\|friday'"

echo "################ 9. /refinements off: no saved entry reaches a turn"
say s8 "/refinements off" | tail -2
"$HA" shutdown --force >/dev/null 2>&1
agent "$RX/p-shop" s9
say s9 "Không đọc file, không tìm trong lịch sử hội thoại (history_search). Chỉ từ memory: bạn có nhớ gì về package manager hay lịch deploy của dự án này không? Có tool memory không?" | tail -4
# pnpm-lock.yaml is a file name any answer may cite; the deploy facts are not.
check "with refinements off the facts are not recalled" "! last | grep -qi 'fly\.io\|thứ sáu\|friday'"
check "the saved state is kept" "entries | grep -qi 'pnpm'"
say s9 "/refinements on" | tail -2
"$HA" shutdown --force >/dev/null 2>&1
agent "$RX/p-shop" s10
say s10 "Chỉ dựa vào memory: dự án này dùng package manager gì?" | tail -3
check "back on, the facts are recalled again" "last | grep -qi 'pnpm'"
"$HA" shutdown --force >/dev/null 2>&1

echo "################ 10. a learned skill that fails is judged, with its false assumption"
mkdir -p "$HA_HOME/skills/build-check"
cat > "$HA_HOME/skills/build-check/SKILL.md" <<'EOF'
---
name: build-check
description: Use when the user asks to check that the project builds.
---
# Build check

1. From the project root, run `node scripts/build.js`: it is the project's build entry point.
2. Report whether the build succeeded.
EOF
echo '{"created_by": "ha", "created": "2026-10-01T00:00:00Z"}' > "$HA_HOME/skills/build-check/.ha-learned.json"
(cd "$RX/p-skill" && printf '{"name":"p-skill","scripts":{"build":"node build/index.js"}}\n' > package.json && mkdir -p build && echo 'console.log("BUILD OK")' > build/index.js && git add -A && git -c user.name=t -c user.email=t@t commit -qm build)
echo '{"autoRefine": {"turnInterval": 1}}' > "$HA_HOME/settings.json"
unset HA_AUTO_REFINE
agent "$RX/p-skill" k1
say k1 "Dùng skill build-check để kiểm tra dự án này build được không. Làm đúng theo skill trước, nếu hỏng thì tìm cách build đúng của dự án." | tail -6
sleep 90
U=$D/skill-usage/global.json
echo "-- skill usage:"; "$PY" -c "import json,sys; d=json.load(open(sys.argv[1],encoding='utf-8')); print(json.dumps(d.get('skills',{}).get('build-check',{}),ensure_ascii=False,indent=1))" "$U" | sed 's/^/   /'
check "the review judged the skill failed" "\"$PY\" -c \"import json,sys; s=json.load(open(sys.argv[1],encoding='utf-8'))['skills']['build-check']; exit(0 if s.get('failed',0)>=1 else 1)\" '$U'"
check "the failure keeps the false assumption" "\"$PY\" -c \"import json,sys; f=json.load(open(sys.argv[1],encoding='utf-8'))['skills']['build-check'].get('failures',[]); exit(0 if f and f[-1]['assumption'].strip() else 1)\" '$U'"
"$HA" shutdown --force >/dev/null 2>&1

# Sections 11-13 run without the automatic review; 14 is about it.
rm -f "$HA_HOME/settings.json"
export HA_AUTO_REFINE=off

echo "################ 11. a fact a longer entry already holds is refused, by the tool and by the kernel"
agent "$RX/p-blog" b11
say b11 "Ghi vào memory (scope local) MỘT entry duy nhất, tiêu đề 'Hạ tầng blog', nội dung: 'Blog dùng Hugo để build, deploy lên Netlify mỗi tối, ảnh lưu trên Cloudinary.'" | tail -2
say b11 "Kiểm thử: gọi tool memory action write ngay (không search trước, không truyền id), scope local, tiêu đề 'Công cụ build', nội dung 'Blog dùng Hugo để build, deploy lên Netlify mỗi tối.'. In nguyên văn thông báo tool trả về, không làm gì thêm." | tail -4
check "the tool refuses a fact a longer entry already holds" "last | grep -qi 'already says\|same fact'"
say b11 "Kiểm thử kernel: trong python_repl chạy đúng một cell: rlm.harness.create_memory(title='Hosting', content='Blog dùng Hugo để build, deploy lên Netlify mỗi tối, ảnh lưu trên Cloudinary.') rồi in nguyên văn kết quả hoặc lỗi, không làm gì thêm." | tail -4
check "the kernel refuses the same fact too" "last | grep -qi 'already says\|same fact'"
"$HA" shutdown --force >/dev/null 2>&1

echo "################ 12. a constraint reaches every turn, ahead of the ranked entries"
# Ten notes whose grouping sorts before 'constraints': ranked by nothing (the
# request names none of them), they would fill the six shown.
"$PY" - "$D" <<'EOF'
import json, os, sys
path = os.path.join(sys.argv[1], "harness", "harness_state.json")
state = json.load(open(path, encoding="utf-8")) if os.path.exists(path) else {"schema": 1, "entries": {}}
topics = ["mercury", "venus", "earth", "mars", "jupiter", "saturn", "uranus", "neptune", "pluto", "ceres"]
for index, topic in enumerate(topics):
    state.setdefault("entries", {}).setdefault("memory", {})[f"agenda_{topic}"] = {
        "id": f"agenda_{topic}", "kind": "memory", "title": f"Agenda {topic}",
        "content": f"The {topic} meeting is in room {index + 10}.", "path": "agenda",
        "scope": "global", "version": 1, "source": "agent",
    }
os.makedirs(os.path.dirname(path), exist_ok=True)
json.dump(state, open(path, "w", encoding="utf-8"))
EOF
agent "$RX/p-blog" b12
say b12 "Ghi vào memory (scope project) một quy tắc với path đúng là 'constraints': tiêu đề 'Quy tắc ảnh', nội dung 'Mọi ảnh mới của blog phải ở định dạng WebP.' Chỉ ghi, không làm gì khác." | tail -2
check "the rule is kept under constraints" "entries | grep -a '^global|p-blog|memory|' | grep -qi 'webp'"
"$HA" shutdown --force >/dev/null 2>&1
agent "$RX/p-blog" b13
say b13 "Viết một câu chào ngắn bằng tiếng Việt." | tail -2
# The request that turn sent, found by its prompt: its digest.
digest() { "$PY" - "$D" <<'EOF'
import glob, json, os, sqlite3, sys
sys.stdout.reconfigure(encoding="utf-8")
for db in glob.glob(os.path.join(sys.argv[1], "projects", "*", "harness.sqlite3")):
    for (text,) in sqlite3.connect(f"file:{db}?mode=ro", uri=True).execute("select request_json from frozen_requests order by rowid"):
        if "Viết một câu chào ngắn" in text:
            print(text)
            raise SystemExit
EOF
}
check "an unrelated turn's digest carries the constraint" "digest | grep -q 'WebP'"
"$HA" shutdown --force >/dev/null 2>&1

echo "################ 13. a new conversation knows the task the last one left unfinished"
agent "$RX/p-blog" b14
say b14 "Dùng tool task_state ghi kế hoạch cho việc 'Chuyển blog sang Astro' gồm 3 mục: (1) dựng khung Astro - done, (2) chuyển các bài viết - in_progress, (3) đổi cấu hình deploy Netlify - pending. Chỉ ghi task_state, không làm gì khác." | tail -2
"$HA" shutdown --force >/dev/null 2>&1
agent "$RX/p-blog" b15
say b15 "Không đọc file, không dùng history_search hay memory: ở dự án này tôi đang làm dở việc gì, và còn những bước nào?" | tail -8
check "a new conversation knows the unfinished task" "last | grep -qi 'astro'"
check "and the steps left" "last | grep -qi 'bài viết\|deploy'"
"$HA" shutdown --force >/dev/null 2>&1

echo "################ 14. a session that goes quiet is reviewed"
unset HA_AUTO_REFINE
export HA_IDLE_REVIEW_SECONDS=20
mkdir -p "$RX/p-shop/notes"
for index in 01 02 03 04 05 06 07 08 09 10 11 12; do echo "note $index: the cart keeps item $index for a day" > "$RX/p-shop/notes/n$index.txt"; done
agent "$RX/p-shop" q1
touch "$RX/quiet.marker"
say q1 "Đọc từng file trong thư mục notes bằng read_file, mỗi lần một file (12 lần gọi), rồi tóm tắt chúng trong một câu." | tail -2
# Quiet for HA_IDLE_REVIEW_SECONDS, then a review of its own model calls: wait
# for its watermark rather than a fixed time, as a long turn ends it later.
for _ in $(seq 1 240); do
  find "$D/sessions" -name review_watermark -newer "$RX/quiet.marker" | grep -q . && break
  sleep 1
done
check "the quiet session's work was reviewed" "find '$D/sessions' -name review_watermark -newer '$RX/quiet.marker' | grep -q ."
"$HA" shutdown --force >/dev/null 2>&1
unset HA_IDLE_REVIEW_SECONDS

result
