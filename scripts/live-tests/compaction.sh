#!/usr/bin/env bash
# A long task through many compactions with a real model: does the agent
# still know, at the end, what the user asked of it? The compaction point is
# set low (HA_COMPACT_AT) and the task is eight turns of about 8k tokens each,
# so the summary of the earlier turns is written again and again and the turn
# that set the rules is long gone from the replayed history. The user sets
# rules, a decision and a codeword at the start and adds a rule half way.
# Checked at the end: what the agent recalls without reading anything, what it
# left on disk, and what the host kept (the task state, the summaries and the
# user's words in them, the recaps, the request to save, /compact's
# checkpoint). Every check prints PASS or FAIL; the evidence stays in
# $LIVE_ROOT/compaction.
#
#   bash scripts/live-tests/compaction.sh
set -u
source "$(dirname "$0")/env.sh"
fresh compaction
export HA_AUTO_REFINE=off HA_PYTHON=${HA_PYTHON:-$PY}
export HA_COMPACT_AT=${HA_COMPACT_AT:-40000}
P="$RX/p"
project "$P"
mkdir -p "$P/vendor" "$P/gen"
printf 'vendored - do not edit\n' > "$P/vendor/lib.txt"
# Eight inputs of about 7k tokens each, one per turn.
"$PY" - "$P/gen" <<'EOF'
import os, sys
for module in range(1, 9):
    lines = [f"m{module} row {row:04d} " + "alpha beta gamma delta epsilon " * 3 for row in range(250 + module * 10)]
    open(os.path.join(sys.argv[1], f"m{module}.txt"), "w", encoding="utf-8").write("\n".join(lines) + "\n")
EOF
(cd "$P" && git add -A && git -c user.name=t -c user.email=t@t commit -qm inputs)
# What the host kept for this agent's conversation and its requests.
store() { "$PY" - "$HA_HOME/data" "$1" <<'EOF'
import glob, json, os, sqlite3, sys
sys.stdout.reconfigure(encoding="utf-8")
data, what = sys.argv[1], sys.argv[2]
dbs = glob.glob(os.path.join(data, "projects", "*", "harness.sqlite3"))
def requests():
    for db in dbs:
        c = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
        for (text,) in c.execute("select request_json from frozen_requests"):
            yield json.loads(text).get("messages", [])
def setting(key):
    for db in dbs:
        c = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
        for (value,) in c.execute("select setting_value from session_settings where setting_key=?", (key,)):
            return value
    return None
if what == "summaries":
    # Distinct summaries the requests carried: of earlier turns, and folds of a turn's steps.
    seen = set()
    for messages in requests():
        for m in messages:
            text = m.get("content") or ""
            for marker in ("[The conversation before this point was compacted", "[The steps of this turn after this one"):
                if text.startswith(marker):
                    seen.add(text.split("<read-files>")[0].split("<user-messages>")[0])
    print(len(seen))
elif what == "recaps":
    print(sum(1 for messages in requests() for m in messages if (m.get("content") or "").startswith("[task-recap")))
elif what == "flush":
    print(sum(1 for messages in requests() for m in messages if (m.get("content") or "").startswith("[context-note")))
elif what == "fitted":
    print(sum(1 for messages in requests() for m in messages if (m.get("content") or "").startswith("[this earlier tool result was removed") and "\nIt" in (m.get("content") or "")))
elif what == "summary":
    print((json.loads(setting("rolling_summary") or "{}")).get("summary", ""))
elif what == "kept-words":
    # The user's words every kept summary holds: the rolling summary's and each checkpoint's.
    texts = [(json.loads(setting("rolling_summary") or "{}")).get("summary", "")]
    for db in dbs:
        c = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
        texts += [json.loads(t).get("packet", "") for (t,) in c.execute("select content_json from context_checkpoints")]
    for text in texts:
        for block in text.split("<user-messages>")[1:]:
            print(block.split("</user-messages>")[0])
elif what == "checkpoint":
    for db in dbs:
        c = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
        for (text,) in c.execute("select content_json from context_checkpoints order by rowid desc limit 1"):
            content = json.loads(text)
            print(content.get("summary_source", ""))
            print(content.get("packet", ""))
elif what in ("state", "items"):
    states = glob.glob(os.path.join(data, "sessions", "*", "harness", "task_state.json"))
    state = json.load(open(states[0], encoding="utf-8")) if states else {}
    print(len(state.get("items", [])) if what == "items" else json.dumps(state, ensure_ascii=False))
EOF
}

echo "################ 1. the rules, a decision and a codeword"
agent "$P" c1
say c1 "Trước khi làm, ghi nhận các điều sau cho cả task (đừng làm gì khác ở lượt này): (1) tuyệt đối không sửa hay tạo file nào trong thư mục vendor/; (2) code Python mới chỉ dùng tab để thụt lề, không dùng dấu cách; (3) quyết định: dùng SQLite thay vì Postgres, vì sản phẩm phải deploy thành một file binary duy nhất; (4) mật mã của task là BLUE-HERON. Xác nhận ngắn gọn." | tail -3

echo "################ 2. eight turns of work, a rule added half way"
for i in 1 2 3 4 5 6 7 8; do
  if [ "$i" -eq 1 ]; then
    plan="Đầu tiên ghi kế hoạch 8 module (m1..m8) thành checklist bằng task_state. "
  else
    plan=""
  fi
  say c1 "${plan}Module $i: đọc toàn bộ gen/m$i.txt bằng read_file, rồi viết src/m$i.py có hàm count_m$i() trả về số dòng của file đó (con số bạn đếm được, viết thẳng vào code). Cập nhật checklist bằng task_state. Trả lời ngắn: XONG m$i." | tail -1
  if [ "$i" -eq 4 ]; then
    say c1 "Thêm một quy tắc cho task (áp dụng cả cho các module đã viết và sắp viết): dòng đầu tiên của mỗi file src/m<i>.py phải là comment '# owner: heron-team'. Sửa các module đã viết luôn." | tail -1
  fi
done
say c1 "Kiểm tra: dùng python_repl import lần lượt src/m1.py..src/m8.py và in kết quả count_m<i>() của từng module, rồi cho biết có module nào sai số dòng so với gen/m<i>.txt không." | tail -4
SUMMARIES=$(store summaries); RECAPS=$(store recaps); FLUSH=$(store flush); FITTED=$(store fitted)
echo "   summaries=$SUMMARIES recaps=$RECAPS flush-notes=$FLUSH shortened-with-head=$FITTED"
check "the earlier turns were summarized again and again ($SUMMARIES)" "[ $SUMMARIES -ge 3 ]"
check "the task was recited after a summary ($RECAPS)" "[ $RECAPS -ge 1 ]"
check "the model was asked to save before a summary ($FLUSH)" "[ $FLUSH -ge 1 ]"
STATE=$(store state)
echo "   task_state: ${STATE:0:300}"
ITEMS=$(store items)
check "the task state holds the checklist ($ITEMS items)" "[ $ITEMS -ge 8 ]"
store summary > "$RX/summary.md"
store kept-words > "$RX/kept-words.md"
check "the summaries keep the user's words: the codeword" "grep -q 'BLUE-HERON' '$RX/kept-words.md'"
check "the summaries keep the user's words: the rule added half way" "grep -q 'heron-team' '$RX/kept-words.md'"
check "the kept words are what the user typed, not the composed prompt" "! grep -q 'attached file content follows\|\[python-state\]' '$RX/kept-words.md'"

echo "################ 3. on disk"
for i in 1 2 3 4 5 6 7 8; do [ -f "$P/src/m$i.py" ] || echo "   missing src/m$i.py"; done
check "all eight modules exist" "[ \$(ls '$P'/src/m[1-8].py 2>/dev/null | wc -l) -eq 8 ]"
check "vendor/ is untouched" "[ -z \"\$(cd '$P' && git status --porcelain vendor)\" ]"
check "no module indents with spaces" "! grep -nE '^ +[^ ]' '$P'/src/m*.py"
check "every module starts with the rule added half way" "[ \$(head -qn1 '$P'/src/m[1-8].py 2>/dev/null | grep -c '# owner: heron-team') -eq 8 ]"

echo "################ 4. recall after the summaries, without reading anything"
say c1 "Chỉ trả lời từ những gì bạn đang biết (không đọc file, không chạy lệnh, không dùng history_search hay memory): (a) liệt kê các quy tắc tôi đặt cho task, kể cả quy tắc tôi thêm giữa chừng; (b) quyết định về database và lý do; (c) mật mã của task; (d) những module nào đã xong." | tail -14
check "it recalls vendor/" "last | grep -qi 'vendor'"
check "it recalls tabs" "last | grep -qi 'tab'"
check "it recalls SQLite and why" "last | grep -qi 'sqlite' && last | grep -qi 'binary\|nhị phân\|một file'"
check "it recalls the codeword" "last | grep -q 'BLUE-HERON'"
check "it recalls the rule added half way" "last | grep -qi 'heron-team\|owner'"
check "it knows all eight modules are done" "last | grep -q 'm8\|8 module\|1.*8\|tất cả'"

echo "################ 5. /compact writes a model summary of the whole conversation"
say c1 "/compact" | tail -2
store checkpoint > "$RX/checkpoint.md"
head -1 "$RX/checkpoint.md"
check "the checkpoint is the model's summary, not the fallback" "head -1 '$RX/checkpoint.md' | grep -qx 'model'"
check "the checkpoint keeps the user's words" "grep -q 'BLUE-HERON' '$RX/checkpoint.md'"
"$HA" shutdown --force >/dev/null 2>&1

result
