#!/usr/bin/env bash
# The learning loop (docs/architecture/LEARNING_LOOP.en.md), with a real model
# through `ha exec`, its review run after every turn. Every check fails on
# 0.8.4, which had no lessons and no `ha learning`.
#
# A correction the user makes is recorded by the review as a lesson, with the
# quote that shows it; the same conversation repeating it is not enough, and a
# second conversation makes it a rule by itself: a `constraints` entry of the
# global memory that the next turn's request carries; `ha learning retire`
# removes it. A correction about one project becomes that project's lesson,
# and once a third signal arrives (one seeded from an earlier conversation) a
# decision of the project's markdown store and a line of its index, which the
# lint passes; its retirement strikes them through.
#
#   bash scripts/testing/live-tests/learning.sh
set -u
source "$(dirname "$0")/env.sh"
fresh learning
D=$HA_HOME/data
G=$D/harness
P=$RX/p
project "$P"
cat > "$P/calc.py" <<'EOF'
def add(a, b):
    return a + b


def sub(a, b):
    return a - b
EOF
(cd "$P" && git add -A && git -c user.name=t -c user.email=t@t commit -qm calc)
# Review after every turn, keeping the thinking level fresh() set.
"$PY" - "$HA_HOME/settings.json" <<'EOF'
import json, os, sys
path = sys.argv[1]
settings = json.load(open(path, encoding="utf-8")) if os.path.exists(path) else {}
settings["autoRefine"] = {"turnInterval": 1}
json.dump(settings, open(path, "w", encoding="utf-8"))
EOF
unset HA_AUTO_REFINE

run_exec() {
  local name="$1" dir="$2"; shift 2
  printf '\n## %s (ha exec %s)\n\n> %s\n\n' "$(date +%H:%M:%S)" "$name" "${*: -1}" >> "$LOG"
  (cd "$dir" && "$HA" exec --model "$MODEL" "${TH[@]}" --approval full-auto --output-format json "$@" >"$RX/$name.json" 2>"$RX/$name.err")
  "$PY" -c "import json,sys; print(json.load(open(sys.argv[1],encoding='utf-8')).get('response',''))" "$RX/$name.json" >> "$LOG" 2>/dev/null
  sed 's/^/    /' "$RX/$name.err" >> "$LOG"
}
task_of() { "$PY" -c "import json,sys; print(json.load(open(sys.argv[1],encoding='utf-8'))['task_id'])" "$RX/$1.json"; }
# field <python filter over a lesson x> <python expression over it>
field() { "$HA" learning list --all --json | "$PY" -c "import json,sys; l=[x for x in json.load(sys.stdin) if $1]; print(eval(sys.argv[1], {'x': l[0]}) if l else '')" "$2"; }
# The request a turn sent, found by text in its prompt.
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

state() { field "x['id']=='$1'" "x['state']"; }
signals() { field "x['id']=='$1'" "len(x['signals'])"; }
conversations() { field "x['id']=='$1'" "len({s['conversation'] for s in x['signals'] if not s.get('against')})"; }
kind() { field "x['id']=='$1'" "x['kind']"; }
# The memory entry a lesson's rule is: written by the loop, or the review's own.
placed() { field "x['id']=='$1'" "(x.get('placement') or {}).get('key', '')"; }
placement() { field "x['id']=='$1'" "(x.get('placement') or {}).get('target', '')"; }
entry() { "$HA" memory export | "$PY" -c "import json,sys; e=json.load(sys.stdin)['document']['entries']['memory'].get(sys.argv[1]); print((e or {}).get('title', ''))" "$1"; }
learned_count() { "$HA" memory export | "$PY" -c "import json,sys; print(sum(1 for k in json.load(sys.stdin)['document']['entries']['memory'] if k.startswith('learned_')))"; }

echo "################ 1. a correction in the moment is recorded as a lesson, with its quote"
# Not a rule the user states: a reaction to an answer, which the review keeps
# as a lesson rather than as a preference.
run_exec a1 "$P" "Giải thích thật chi tiết từng dòng của calc.py."
A=$(task_of a1)
run_exec a2 "$P" --resume "$A" "Dài quá, mình đọc không nổi. Hàm sub làm gì? 1-2 câu thôi."
check "the run reviewed its turns" 'grep -q "auto-refine" "$RX/a2.err"'
ID=$(field "x['project'] is None" "x['id']")
check "a lesson about every project was recorded" '[ -n "$ID" ]'
echo "   lesson: $ID $(field "x['id']=='$ID'" "x['text']")"
check "it is a correction" '[ "$(kind "$ID")" = correction ]'
check "its signal quotes the conversation" '"$HA" learning show "$ID" | grep -qi "dài quá\|1-2 câu\|đọc không nổi"'
check "one conversation writes no rule of the loop" '[ "$(learned_count)" -eq 0 ]'

echo "################ 2. a second conversation makes it a rule, kept once"
run_exec b1 "$P" "Giải thích hàm add trong calc.py."
run_exec b2 "$P" --resume "$(task_of b1)" "Lại dài quá rồi, như lần trước. Ngắn thôi, 1-2 câu: calc.py có mấy hàm?"
check "the review named the open lesson" '[ "$(conversations "$ID")" -ge 2 ]'
check "the lesson is a rule" '[ "$(state "$ID")" = promoted ]'
KEY=$(placed "$ID")
echo "   placed as: $(placement "$ID") $KEY"
check "its rule is one memory entry that exists" '[ -n "$KEY" ] && [ -n "$(entry "$KEY")" ]'
check "the loop wrote it only when the review had not" '[ "$(placement "$ID")" = covered ] && [ "$(learned_count)" -eq 0 ] || { [ "$(placement "$ID")" = memory ] && [ "$(learned_count)" -eq 1 ]; }'
check "a promotion the loop made said so" '[ "$(placement "$ID")" != memory ] || grep -q "learning loop: learned rule $ID" "$RX/b1.err" "$RX/b2.err"'
"$HA" learning report --html "$RX/report.html" > "$RX/report.txt"
sed 's/^/   /' "$RX/report.txt" | head -20
check "the report lists the rule and what it did since" 'grep -A3 "^## Rules (" "$RX/report.txt" | grep -q "$ID.*since it became a rule"'
check "and writes it as a page" 'grep -q "<h2>Rules" "$RX/report.html"'

echo "################ 3. the next turn carries the rule"
run_exec c1 "$P" "Trả lời đúng một từ: MARKER_LEARNED"
R=$(request "MARKER_LEARNED")
TITLE=$(entry "$KEY")
check "the turn's request was found" '[ -n "$R" ]'
check "the request carries the rule" '[ -n "$TITLE" ] && printf "%s" "$R" | grep -qF "$TITLE"'

echo "################ 4. a rule the user states outright is kept once"
before=$(learned_count)
run_exec d1 "$P" "Từ nay ở mọi project và mọi cuộc trò chuyện: khi mình chỉ hỏi về code thì bạn chỉ trả lời, không được tự sửa file nào. Giờ cho mình biết calc.py có mấy dòng."
check "calc.py was not edited" '[ -z "$(cd "$P" && git status --porcelain calc.py)" ]'
check "the loop wrote no second entry for it" '[ "$(learned_count)" -eq "$before" ]'
SAVED=$(field "any('sửa file' in s['evidence'] or 'chỉ trả lời' in s['evidence'] for s in x['signals'])" "x['id']")
echo "   its lesson, if reported: ${SAVED:-none}"
check "a lesson it became is the review's own entry" '[ -z "$SAVED" ] || [ "$(state "$SAVED")" = candidate ] || [ "$(placement "$SAVED")" = covered ]'

echo "################ 5. retiring the lesson undoes the loop's rule only"
"$HA" learning retire "$ID" --reason "live test" | sed 's/^/   /'
check "the lesson is kept, retired" '[ "$(state "$ID")" = retired ]'
check "an entry the loop wrote is gone, the review's own stays" '{ [ "$(placement "$ID")" = memory ] && [ -z "$(entry "$KEY")" ]; } || { [ "$(placement "$ID")" = covered ] && [ -n "$(entry "$KEY")" ]; }'

echo "################ 6. a project's lesson becomes a decision of its markdown store"
Q=$RX/q
project "$Q"
(cd "$Q" && "$HA" memory init >/dev/null && git add -A && git -c user.name=t -c user.email=t@t commit -qm store)
run_exec q1 "$Q" "Không, project này không build bằng cargo build: lệnh build của project này là make ci. Mình đã phải nói điều này rồi. Giờ liệt kê các file ở gốc project."
QID=$(field "x['project'] is not None" "x['id']")
pname() { field "x['id']=='$1'" "x['project']['name']"; }
check "a lesson about this project was recorded" '[ -n "$QID" ] && [ "$(pname "$QID")" = q ]'
# One earlier conversation's signal, as the review would have recorded it.
"$PY" - "$G" "$QID" <<'EOF'
import datetime, json, os, sys
path = os.path.join(sys.argv[1], "lessons.json")
data = json.load(open(path, encoding="utf-8"))
at = (datetime.datetime.now(datetime.timezone.utc) - datetime.timedelta(days=2)).isoformat()
for lesson in data["lessons"]:
    if lesson["id"] == sys.argv[2]:
        lesson["signals"].insert(0, {"at": at, "conversation": "task_earlier", "against": False, "evidence": "this project builds with make ci"})
json.dump(data, open(path, "w", encoding="utf-8"), indent=2)
EOF
check "two signals from two conversations are not yet a rule" '[ "$(state "$QID")" = candidate ]'
run_exec q2 "$Q" --resume "$(task_of q1)" "Nhắc lại: build bằng make ci, đừng dùng cargo build. Project có file README không?"
check "the review named the project's lesson" '[ "$(signals "$QID")" -ge 3 ]'
check "it was promoted" '[ "$(state "$QID")" = promoted ]'
DEC=$(cd "$Q" && ls memory/decisions/*-learned-*.md 2>/dev/null | head -1)
[ -n "$DEC" ] && sed 's/^/   /' "$Q/$DEC"
check "into a decision file of the project's store" '[ -n "$DEC" ] && grep -q "\[inferred\] Rule: " "$Q/$DEC"'
check "and a line of its index" '[ -n "$DEC" ] && grep -q "$DEC" "$Q/MEMORY.md"'
check "not into the global memory" '! "$HA" memory export | grep -q "learned_${QID#les_}"'
check "the store passes the lint" '(cd "$Q" && "$HA" memory lint --quiet)'
"$HA" learning retire "$QID" --reason "live test" | sed 's/^/   /'
check "retiring strikes the rule through" '[ -n "$DEC" ] && grep -q "~~\[inferred\] Rule:" "$Q/$DEC" && grep "$DEC" "$Q/MEMORY.md" | grep -q "~~"'
check "and the store still passes the lint" '(cd "$Q" && "$HA" memory lint --quiet)'

result
