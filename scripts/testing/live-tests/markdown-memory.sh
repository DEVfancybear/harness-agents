#!/usr/bin/env bash
# A project's markdown memory store (docs/architecture/MARKDOWN_MEMORY.en.md),
# with a real model through `ha exec`. Every check fails on 0.8.4, which had no
# `ha memory init` or `lint` and put nothing of the store in the turn.
#
# `ha memory init --hook` writes the store and the hook; `ha memory lint` passes
# it; the turn carries the rules and the index; the model answers from the file
# the index routes to; it writes a new fact as a tagged line in a person file
# and routes the index to it; it supersedes a decision by striking the old line
# through; the hook blocks a commit with an untagged fact and lets a tagged one
# through; `ha doctor` reports the store and a dead index path. Section 10
# needs no model: two rules the learning loop places land in two decision
# files, and the index holds a pointer to each, not its text (both fail on
# 0.8.7).
#
#   bash scripts/testing/live-tests/markdown-memory.sh
set -u
source "$(dirname "$0")/env.sh"
fresh markdown-memory
export HA_AUTO_REFINE=off
D=$HA_HOME/data
P=$RX/p
project "$P"
# The hook runs `ha` by name. A `C:/...` entry would split at its colon.
hadir=$(dirname "$HA")
command -v cygpath > /dev/null && hadir=$(cygpath -u "$hadir")
PATH="$hadir:$PATH"
export PATH

run_exec() {
  local name="$1"; shift
  printf '\n## %s (ha exec %s)\n\n> %s\n\n' "$(date +%H:%M:%S)" "$name" "${*: -1}" >> "$LOG"
  (cd "$P" && "$HA" exec --model "$MODEL" "${TH[@]}" --approval full-auto --output-format json "$@" >"$RX/$name.json" 2>"$RX/$name.err")
  "$PY" -c "import json,sys; print(json.load(open(sys.argv[1],encoding='utf-8')).get('response',''))" "$RX/$name.json" >> "$LOG" 2>/dev/null
  sed 's/^/    /' "$RX/$name.err" >> "$LOG"
}
answer() { "$PY" -c "import json,sys; print(json.load(open(sys.argv[1],encoding='utf-8')).get('response',''))" "$RX/$1.json" 2>/dev/null; }
# The request a turn sent, found by text in its prompt: a whole one, not a
# later step's request, which is stored as a delta of it.
request() { "$PY" - "$D" "$1" <<'EOF'
import glob, os, sqlite3, sys
sys.stdout.reconfigure(encoding="utf-8")
found = None
for db in glob.glob(os.path.join(sys.argv[1], "projects", "*", "harness.sqlite3")):
    for (text,) in sqlite3.connect(f"file:{db}?mode=ro", uri=True).execute("select request_json from frozen_requests order by rowid"):
        if sys.argv[2] in text and '"delta_of"' not in text[:40]:
            found = text
print(found or "")
EOF
}
lint() { (cd "$P" && "$HA" memory lint "$@"); }
# Everything the model was sent in the run's requests, tool results included.
sent() { "$PY" - "$D" <<'EOF2'
import glob, os, sqlite3, sys
sys.stdout.reconfigure(encoding="utf-8")
for db in glob.glob(os.path.join(sys.argv[1], "projects", "*", "harness.sqlite3")):
    for (text,) in sqlite3.connect(f"file:{db}?mode=ro", uri=True).execute("select request_json from frozen_requests order by rowid"):
        print(text)
EOF2
}
commit() { (cd "$P" && git add -A && git -c user.name=t -c user.email=t@t commit -qm "$1" "${@:2}"); }

echo "################ 1. init writes the store and the hook; lint passes it"
(cd "$P" && "$HA" memory init --hook) > "$RX/init.out" 2>&1
sed 's/^/   /' "$RX/init.out"
check "the index, the config and the tiers are written" '[ -f "$P/MEMORY.md" ] && [ -f "$P/memory-lint.json" ] && [ -d "$P/memory/people" ] && [ -d "$P/memory/archive" ]'
check "the hook runs the lint on what is staged" 'grep -q "ha memory lint --staged --quiet" "$P/.git/hooks/pre-commit"'
check "a new store passes every check" 'lint > "$RX/lint1.out"; [ $? -eq 0 ] && grep -q "^RESULT PASS (0 of 6 checks failed)" "$RX/lint1.out"'
check "--json is memory-lint's report" 'lint --json | "$PY" -c "import json,sys; r=json.load(sys.stdin); assert r[\"version\"]==\"1.0.0\" and r[\"result\"]==\"PASS\" and [c[\"name\"] for c in r[\"checks\"]]==[\"rom-caps\",\"ram-caps\",\"disk-provenance\",\"disk-history\",\"tape-frozen\",\"index-links\"]"'

# A project with a history the model has not seen: one project file, routed by the index.
cat > "$P/memory/projects/orion.md" <<'EOF'
# Orion

## Decisions

- [stated] Orion ships on the first Tuesday of each month. (2026-10-01)

## Facts

- [observed] Orion's release checklist lives in docs/release.md. (2026-10-02)
EOF
"$PY" - "$P/MEMORY.md" <<'EOF'
import sys
p = sys.argv[1]
text = open(p, encoding="utf-8").read()
row = "|---------|-------------|------------------|\n"
text = text.replace(row, row + "| Orion | `memory/projects/orion.md` | orion, release, ship |\n", 1)
open(p, "w", encoding="utf-8", newline="\n").write(text)
EOF
commit "seed the store"
check "the seeded store passes and commits through the hook" '[ -n "$(cd "$P" && git log --oneline -1 --grep "seed the store")" ]'

echo "################ 2. the turn carries the rules and the index; the model reads where it routes"
run_exec m1 "Theo memory của project này, Orion ship vào ngày nào trong tháng? Trả lời ngắn và nêu file nguồn."
R=$(request "Orion ship")
check "the turn's request was found" '[ -n "$R" ]'
check "the request carries the store's rules" 'printf "%s" "$R" | grep -q "Make memory reliable enough that the user never has to remind you"'
check "the request carries the index with its route" 'printf "%s" "$R" | grep -q "memory/projects/orion.md"'
check "the model answered from the project file" 'answer m1 | grep -qi "tuesday\|thứ ba\|thứ 3"'
check "and named it" 'answer m1 | grep -q "orion.md"'

echo "################ 3. a new fact is a tagged line in a person file, routed by the index"
run_exec m2 "Ghi nhớ giúp tôi về Lan, một người trong team: Lan là reviewer chính của team, và Lan thích review PR vào buổi sáng. Lưu vào memory của project này theo đúng quy tắc của nó."
lan=$(cd "$P" && ls memory/people/*.md 2>/dev/null | grep -i lan | head -1)
check "a person file for Lan was written" '[ -n "$lan" ]'
check "its fact line carries a provenance tag" '[ -n "$lan" ] && grep -E "^\s*[-*+] .*\[(stated|observed|inferred|suggested)[^]]*\].*" "$P/$lan" | grep -qi "review\|duyệt"'
check "the index routes to it" '[ -n "$lan" ] && grep -q "$lan" "$P/MEMORY.md"'
check "the change passes the lint" 'lint --quiet > "$RX/lint2.out"; r=$?; cat "$RX/lint2.out" | sed "s/^/   /"; [ $r -eq 0 ]'
commit "lan" 2>&1 | sed 's/^/   /'

echo "################ 4. a changed decision is struck through, not deleted"
run_exec m3 "Quyết định mới, do tôi chốt hôm nay: từ giờ Orion ship vào thứ Năm đầu tiên của mỗi tháng, thay cho thứ Ba. Cập nhật memory của project này theo đúng quy tắc của nó."
O="$P/memory/projects/orion.md"
sed 's/^/   /' "$O"
check "the old decision is kept, struck through" 'grep "~~" "$O" | grep -qi "tuesday\|thứ ba"'
check "the new decision is an un-struck tagged line" 'grep -v "~~" "$O" | grep -i "thursday\|thứ năm\|thứ 5" | grep -q "\[stated"'
check "the edit passes the lint, history included" 'lint --quiet > "$RX/lint3.out"; r=$?; sed "s/^/   /" "$RX/lint3.out"; [ $r -eq 0 ]'
commit "thursday" 2>&1 | sed 's/^/   /'

echo "################ 5. the hook blocks an untagged fact and passes a tagged one"
printf -- '- Minh owns the Orion billing module.\n' > "$P/memory/people/minh.md"
(cd "$P" && git add -A && git -c user.name=t -c user.email=t@t commit -qm untagged) > "$RX/hook-block.out" 2>&1
blocked=$?
sed 's/^/   /' "$RX/hook-block.out"
check "the untagged fact's commit is blocked" '[ $blocked -ne 0 ] && grep -q "FAIL disk-provenance" "$RX/hook-block.out"'
printf -- '- [stated] Minh owns the Orion billing module. (2026-10-28)\n' > "$P/memory/people/minh.md"
check "the tagged fact commits" 'commit tagged >/dev/null 2>&1'

echo "################ 6. ha doctor reports the store and a dead index path"
(cd "$P" && "$HA" doctor) > "$RX/doctor1.out" 2>&1
check "doctor passes the index links" 'grep -q "markdown memory index-links" "$RX/doctor1.out" && ! grep "markdown memory" "$RX/doctor1.out" | grep -qi "fail\|✗"'
printf '\n- Gone: `memory/projects/gone.md`\n' >> "$P/MEMORY.md"
(cd "$P" && "$HA" doctor) > "$RX/doctor2.out" 2>&1
grep "markdown memory" "$RX/doctor2.out" | sed 's/^/   /'
check "doctor names the dead path" 'grep "markdown memory index-links" "$RX/doctor2.out" | grep -q "memory/projects/gone.md"'

echo "################ 7. the examples sit beside the tiers"
E=$RX/e
project "$E"
(cd "$E" && "$HA" memory init --examples) > "$RX/examples.out" 2>&1
check "the four examples are written" '[ -f "$E/memory/templates/PERSON.md" ] && [ -f "$E/memory/templates/PROJECT.md" ] && [ -f "$E/memory/templates/DECISION.md" ] && [ -f "$E/memory/templates/DAILY.md" ]'
check "and the store with them passes the lint" '(cd "$E" && "$HA" memory lint --quiet)'

echo "################ 8. a flat MEMORY.md is migrated into the store"
M=$RX/m
project "$M"
cat > "$M/MEMORY.md" <<'EOF'
# Memory

- Lan is the main reviewer of the Vega project; every Vega PR needs her approval (she said so on 2026-10-02).
- Vega deploys from the main branch through CI only, decided 2026-09-15 after a manual deploy broke production.
- The Vega API used REST until 2026-08-20, when the team switched it to tRPC.
- Lan works from Hanoi and prefers reviews in the morning.
EOF
(cd "$M" && git add -A && git -c user.name=t -c user.email=t@t commit -qm flat)
(cd "$M" && "$HA" memory migrate --model "$MODEL" "${TH[@]}") > "$RX/migrate.out" 2>&1
migrated=$?
tail -12 "$RX/migrate.out" | sed 's/^/   /'
check "the migration finished and the store passes the lint" '[ $migrated -eq 0 ] && grep -q "^RESULT PASS" "$RX/migrate.out"'
# The lint is the migration's check: a self-evaluation that saw only the last
# tool results sent a whole migration back on 0.8.7's first candidate.
check "no self-evaluation judges the migration" '! grep -q "self-evaluation: judging" "$RX/migrate.out"'
check "the flat file is kept whole in the archive" 'grep -lq "every Vega PR needs her approval" "$M"/memory/archive/*.flat.md'
lanfile=$(cd "$M" && ls memory/people/*.md 2>/dev/null | grep -i lan | head -1)
check "Lan has a person file with tagged facts" '[ -n "$lanfile" ] && grep -E "^\s*[-*+] \[(stated|observed|inferred|suggested)" "$M/$lanfile" | grep -qi "review"'
check "Vega has a project file" 'ls "$M"/memory/projects/*.md 2>/dev/null | grep -qi vega'
check "the REST to tRPC change is struck through, not lost" 'grep -rh "~~" "$M/memory/projects" "$M/memory/decisions" 2>/dev/null | grep -qi "rest"'
check "the index routes to the new files" '[ -n "$lanfile" ] && grep -q "$lanfile" "$M/MEMORY.md" && ! grep -qi "every Vega PR needs her approval" "$M/MEMORY.md"'

echo "################ 9. the memory tool keeps no project fact beside the store"
run_exec m9 "Kiểm thử harness: gọi tool memory với action write, scope project, title Package manager, content: Orion dùng pnpm. Nếu tool từ chối thì làm đúng theo lời từ chối của nó, rồi báo lại nguyên văn lời từ chối."
check "the project entry was refused, naming the store" 'sent | grep -q "this project keeps its memory in markdown files"'
check "no project entry was kept" '! "$HA" memory export | "$PY" -c "import json,sys; d=json.load(sys.stdin)[\"document\"][\"entries\"][\"memory\"]; sys.exit(0 if any(\"pnpm\" in (e.get(\"content\") or \"\") and e.get(\"project\") for e in d.values()) else 1)"'
check "the fact went into the store, tagged" 'grep -rE "^\s*[-*+] \[(stated|observed|inferred|suggested)[^]]*\].*pnpm" "$P/memory" | grep -q .'
check "and the store passes the lint" 'lint --quiet > /dev/null'

echo "################ 10. the learning loop writes a pointer, and two rules of one day two files"
# No model: `ha learning import` places what passes the gate into the store of
# the project it runs in. Two rules, seen in two conversations each, in
# Chinese: no ASCII letter names either, and both are longer than an index
# title. On 0.8.7 both went to memory/decisions/<today>-learned-lesson.md, the
# second over the first, and each index line held the rule's text.
L=$RX/l
project "$L"
(cd "$L" && "$HA" memory init) > /dev/null 2>&1
"$PY" - "$RX/rules.json" "$(basename "$L")" <<'EOF'
import datetime, json, sys
at = lambda days: (datetime.datetime.now(datetime.timezone.utc) - datetime.timedelta(days=days)).isoformat()
signals = [{"at": at(3), "conversation": "task_a", "against": False, "evidence": "总结的时候请先给出结论再说细节"},
           {"at": at(2), "conversation": "task_b", "against": False, "evidence": "又说一遍：总结时先给出结论再列细节"},
           {"at": at(1), "conversation": "task_b", "against": False, "evidence": "请记住，每次总结都先给出结论再列细节"}]
first = {"id": "les_zh1", "kind": "correction", "domain": "communication", "scope": "project", "project": sys.argv[2], "signals": signals,
         "text": "当用户要求总结一份很长的文档时，先用一两句话给出结论，然后再分点列出细节，最后附上每一条结论所依据的原文段落和对应的页码，方便用户自己逐条核对"}
second = dict(first, id="les_zh2", domain="workflow",
              text="修改任何界面之后，必须先在手机和平板两种尺寸的屏幕上分别实际运行并检查一遍布局与交互，确认没有错位、遮挡或者无法点击的按钮，然后才能向用户报告这项工作已经完成")
json.dump({"schema": 1, "kind": "ha-lessons", "lessons": [first, second]}, open(sys.argv[1], "w", encoding="utf-8"), ensure_ascii=False)
EOF
(cd "$L" && "$HA" learning import "$RX/rules.json") > "$RX/rules-import.out" 2>&1
sed 's/^/   /' "$RX/rules-import.out"
# Each rule placed in this store, as `<id>\t<decision file>\t<its text>`.
"$HA" learning list --all --json > "$RX/rules-list.json"
"$PY" - "$RX/rules-list.json" "$(basename "$L")" > "$RX/rules.tsv" <<'EOF'
import json, os, sys
sys.stdout.reconfigure(encoding="utf-8", newline="\n")
for x in json.load(open(sys.argv[1], encoding="utf-8")):
    p = x.get("placement") or {}
    if p.get("target") == "markdown" and os.path.basename(os.path.normpath(p.get("root", ""))) == sys.argv[2]:
        print(x["id"], p["file"], x["text"], sep="\t")
EOF
sed 's/^/   /' "$RX/rules.tsv"
# each_rule <test>: <test> <id> <file> <text> holds for every rule placed.
each_rule() { local id file text; while IFS=$'\t' read -r id file text; do "$1" "$id" "$file" "$text" || return 1; done < "$RX/rules.tsv"; }
holds_rule() { grep -qF "$3" "$L/$2" && grep -qF "$1" "$L/$2"; }
pointed_once() { [ "$(grep -cF "\`$2\`" "$L/MEMORY.md")" -eq 1 ]; }
pointer_only() { ! grep -F "\`$2\`" "$L/MEMORY.md" | grep -qF "$3"; }
check "both rules passed the gate into the store" '[ "$(wc -l < "$RX/rules.tsv")" -eq 2 ]'
check "two rules of one day land in two decision files" '[ "$(cut -f2 "$RX/rules.tsv" | sort -u | wc -l)" -eq 2 ] && [ "$(ls "$L"/memory/decisions/*-learned-*.md | wc -l)" -eq 2 ]'
check "each decision file holds its own rule" 'each_rule holds_rule'
check "the index points at each file once" 'each_rule pointed_once'
check "a learned rule's index line holds no rule text" 'each_rule pointer_only'
check "the store passes the lint after the loop wrote it" '(cd "$L" && "$HA" memory lint --quiet)'
RID=$(head -1 "$RX/rules.tsv" | cut -f1)
RFILE=$(head -1 "$RX/rules.tsv" | cut -f2)
"$HA" learning retire "$RID" --reason "live test" > /dev/null 2>&1
check "retiring strikes the pointer and the rule" '[ -n "$RFILE" ] && grep -F "\`$RFILE\`" "$L/MEMORY.md" | grep -q "~~" && grep -q "~~\[inferred\] Rule:" "$L/$RFILE"'
check "and the store still passes the lint" '(cd "$L" && "$HA" memory lint --quiet)'

result
