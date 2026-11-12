#!/usr/bin/env bash
# Dropping tool results without losing the cache, with a real model
# (HA_CONTEXT_DROP=keep:2,tool; AKV, arXiv 2610.28577, on a provider that
# caches a request by its start). One turn reads six files, so the rolling
# drop keeps its newest two results and drops older ones; the next turn must
# replay the dropped ones as they were sent, not whole; a third turn has the
# model drop its own results with `drop_context`. Checked: the drops are
# recorded, a result not yet read is never dropped, what was dropped stays
# dropped in the next turn's request, the request that rewrote what was sent
# is recorded as such, and the agent still answers from what it read. Every
# check prints PASS or FAIL; the evidence stays in $LIVE_ROOT/context-drop.
#
#   bash scripts/testing/live-tests/context-drop.sh
set -u
source "$(dirname "$0")/env.sh"
fresh context-drop
export HA_PYTHON=${HA_PYTHON:-$PY}
export HA_CONTEXT_DROP=keep:2,tool
P="$RX/p"
project "$P"
mkdir -p "$P/gen"
# Eight files, each with its own marker on the first line and enough lines
# that a dropped result is far shorter than a whole one.
"$PY" - "$P/gen" <<'EOF'
import os, sys
for n in range(1, 9):
    lines = [f"MARK-F{n}-{n * 37}"] + [f"f{n} row {row:04d} alpha beta gamma delta" for row in range(120)]
    open(os.path.join(sys.argv[1], f"f{n}.txt"), "w", encoding="utf-8").write("\n".join(lines) + "\n")
EOF
(cd "$P" && git add -A && git -c user.name=t -c user.email=t@t commit -qm inputs)
# What the requests held and what the model called.
store() { "$PY" - "$HA_HOME/data" "$1" <<'EOF'
import glob, json, os, sqlite3, sys
sys.stdout.reconfigure(encoding="utf-8")
data, what = sys.argv[1], sys.argv[2]
NOTE = "[earlier tool result shortened to "
requests = []
for db in glob.glob(os.path.join(data, "projects", "*", "harness.sqlite3")):
    c = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
    for (text,) in c.execute("select request_json from frozen_requests order by rowid"):
        requests.append(json.loads(text).get("messages", []))
def results(messages):
    return [str(m.get("content") or "") for m in messages if m.get("role") == "tool"]
def calls(messages, name):
    return sum(1 for m in messages for call in m.get("tool_calls") or [] if call.get("name") == name)
# A step's request is recorded as what it added to the one before, and whole
# only when it rewrote it: the calls of a turn are counted over all of them.
def distinct(name):
    return len({call.get("id") or call.get("arguments") for m in requests for message in m
                for call in message.get("tool_calls") or [] if call.get("name") == name})
last = requests[-1] if requests else []
if what == "reads":
    print(distinct("read_file"))
elif what == "dropped-last":
    print(sum(1 for text in results(last) if text.startswith(NOTE)))
elif what == "whole-last":
    print(sum(1 for text in results(last) if "row 0001" in text))
elif what == "whole-ever":
    print(max((sum(1 for text in results(m) if "row 0001" in text) for m in requests), default=0))
elif what == "drop-calls":
    print(distinct("drop_context"))
elif what == "offered":
    print(sum(1 for db in glob.glob(os.path.join(data, "projects", "*", "harness.sqlite3"))
              for (text,) in sqlite3.connect(f"file:{db}?mode=ro", uri=True).execute("select request_json from frozen_requests")
              if '"drop_context"' in text))
EOF
}
diag() { cat "$HA_HOME"/data/diagnostics/*.jsonl 2>/dev/null | grep "\"kind\":\"$1\"" | grep "\"event\":\"$2\""; }

agent "$P" d1

echo "################ 1. a turn keeps its newest results and drops the older ones"
say d1 "Đọc lần lượt gen/f1.txt, gen/f2.txt, gen/f3.txt, gen/f4.txt, gen/f5.txt, gen/f6.txt bằng read_file, mỗi lần gọi một file và đọc cả file (không gọi song song, không dùng tool khác, không giới hạn số dòng). Mỗi file có một mã ở dòng đầu dạng MARK-F<n>-<số>; ghi nhớ từng mã ngay khi đọc. Cuối cùng liệt kê đủ sáu mã." | tail -3
READS=$(store reads)
echo "   read_file calls in the turn=$READS"
check "the turn read the six files one at a time (the precondition: $READS)" "[ $READS -ge 6 ]"
# A model that reads only each file's first lines leaves nothing whole to
# keep: the checks on what stays whole are judged only when it read them whole.
EVER=$(store whole-ever)
check "it read the files whole (the precondition: $EVER whole in one request)" "[ $EVER -ge 2 ]"
KEEP=$(diag context dropped | grep -c '"by":"keep"')
echo "   rolling drops recorded=$KEEP"
check "the rolling drop was made and recorded ($KEEP)" "[ $KEEP -ge 1 ]"
check "it answers with the marks it read before the drops" "last | grep -q 'MARK-F1-37' && last | grep -q 'MARK-F6-222'"
REWRITTEN=$(diag cache prefix_rewritten | grep -c '"what":"tool_result"')
echo "   requests recorded as rewriting a sent tool result=$REWRITTEN"
check "the request that dropped sent results is recorded as a rewrite ($REWRITTEN)" "[ $REWRITTEN -ge 1 ]"

echo "################ 2. the next turn replays the dropped results as they were sent"
say d1 "Trả lời đúng một từ: OK" | tail -1
DROPPED=$(store dropped-last); WHOLE=$(store whole-last)
echo "   in the next turn's request: dropped=$DROPPED whole=$WHOLE"
check "the dropped results are still the note in the next turn ($DROPPED)" "[ $DROPPED -ge 2 ]"
check "the newest results are still whole there ($WHOLE)" "[ $WHOLE -ge 1 ]"
check "the dropped ones did not come back: fewer whole than read ($WHOLE < $READS)" "[ $WHOLE -lt $READS ]"

echo "################ 3. the model drops its own results with drop_context"
check "drop_context is offered to the model" "[ \$(store offered) -ge 1 ]"
say d1 "Đọc gen/f7.txt rồi gen/f8.txt bằng read_file, mỗi lần một file. Sau khi đã đọc xong cả hai, gọi tool drop_context với keep_last = 0 (một lần gọi riêng, không kèm tool khác). Rồi trả lời hai mã MARK ở dòng đầu của hai file đó và chữ XONG-DROP." | tail -2
CALLS=$(store drop-calls)
check "the model called drop_context (the precondition: $CALLS)" "[ $CALLS -ge 1 ]"
TOOL=$(diag context dropped | grep -c '"by":"tool"')
echo "   drops by the model recorded=$TOOL"
check "its drop was made and recorded ($TOOL)" "[ $TOOL -ge 1 ]"
check "it still answers from what it read before dropping" "last | grep -q 'MARK-F7-259' && last | grep -q 'MARK-F8-296' && last | grep -q 'XONG-DROP'"

echo "################ 4. the bug report names the drops and the rewrites"
"$HA" bug-report --since 1h --out "$RX/report.zip" >/dev/null 2>&1
"$PY" - "$RX/report.zip" > "$RX/summary.md" <<'EOF'
import sys, zipfile
sys.stdout.reconfigure(encoding="utf-8")
print(zipfile.ZipFile(sys.argv[1]).read("SUMMARY.md").decode("utf-8"))
EOF
grep -n "tool results dropped by\|requests that rewrote" "$RX/summary.md"
check "the report counts the rolling drop" "grep -q 'tool results dropped by the rolling drop' '$RX/summary.md'"
check "the report counts the model's drop" "grep -q \"tool results dropped by the model's drop_context\" '$RX/summary.md'"
check "the report counts the requests that rewrote what was sent" "grep -q 'requests that rewrote what was already sent' '$RX/summary.md'"
"$HA" shutdown --force >/dev/null 2>&1

result
