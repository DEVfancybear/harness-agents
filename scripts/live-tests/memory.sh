#!/usr/bin/env bash
# Long-memory evaluation with a real model, after LongMemEval: extraction,
# project isolation, knowledge update, temporal recall through history,
# abstention, duplicates, delete and restore, provenance, renamed ids, judged
# skill outcomes and the harness refinements switch. Several conversations over
# three projects; every check prints PASS or FAIL, and the conversation and
# the state on disk stay in $LIVE_ROOT/memory as evidence.
#
#   bash scripts/live-tests/memory.sh
set -u
source "$(dirname "$0")/env.sh"
fresh memory
export HA_AUTO_REFINE=off
D=$HA_HOME/data
for p in p-shop p-blog p-skill; do project "$RX/$p"; done
# Global entries as "scope|project|kind|id|v|content|origin-via".
entries() { "$PY" - "$D" <<'EOF'
import json,os,sys
f=os.path.join(sys.argv[1],"harness","harness_state.json")
s=json.load(open(f,encoding="utf-8")) if os.path.exists(f) else {"entries":{}}
for kind,records in s["entries"].items():
    for k,e in records.items():
        p=(e.get("project") or {}).get("name","-") if isinstance(e.get("project"),dict) else "-"
        print("|".join(["global",p,kind,k,"v%s"%e.get("version"),str(e.get("content")).replace("\n"," "),str((e.get("origin") or {}).get("via","-"))]))
EOF
}
journal() { cat "$D/harness/memory_history.jsonl" 2>/dev/null; }
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

result
