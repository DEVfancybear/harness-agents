#!/usr/bin/env bash
# What 0.7.4 changed, with a real model:
#   1. children nest past two levels: with no limit set, a grandchild starts a
#      child of its own (depth 3), which 0.7.3 refused with "RLM recursion depth
#      limit reached"; `[agents] max_depth` in the config sets a limit again;
#   2. web_search asks DeepSeek Search with the DeepSeek key before Serper;
#   3. a DeepSeek key the API refuses hands the search to the next API, and the
#      answer says so, naming /login deepseek;
#   4. the Python websearch skill does the same in the REPL;
#   5. a coder's commits carry the name `Harness Agents`, not `harness-p5`;
# and what 0.8.9 changed:
#   6. a child obeys a rule only AGENTS.md states and a constraint only the
#      parent's task state holds (0.8.8 sent it neither);
#   7. a fact a child found in a file only it read, handed back with
#      note_finding, becomes the parent's memory through the parent's review.
# Every check prints PASS or FAIL; the conversation and the diagnostics stay in
# $LIVE_ROOT/nesting as evidence. The DeepSeek key comes from the credential
# file in HA_CREDENTIALS_DIR (`deepseek`), never printed.
#
#   bash scripts/testing/live-tests/nesting.sh
set -u
source "$(dirname "$0")/env.sh"
fresh nesting
export HA_AUTO_REFINE=off HA_PYTHON=${HA_PYTHON:-$PY} HA_DIAG=debug
D="$HA_HOME/data/diagnostics"
project "$RX/p"
# Diagnostic records of one kind, as `event key=value ...` lines.
records() {
  "$PY" - "$D" "$1" <<'EOF'
import glob, json, os, sys
directory, kind = sys.argv[1], sys.argv[2]
rows = []
for path in glob.glob(os.path.join(directory, "*.jsonl")):
    for line in open(path, encoding="utf-8", errors="replace"):
        try:
            record = json.loads(line)
        except ValueError:
            continue
        if record.get("kind") == kind:
            fields = " ".join(f"{k}={v}" for k, v in sorted(record.get("fields", {}).items()))
            rows.append((record.get("ms", 0), f"{record.get('event')} {fields} {record.get('msg', '')}".strip()))
for _, row in sorted(rows):
    print(row)
EOF
}
# What the slash commands answered: their notices, as diagnostics hold them.
notices() { records notice | sed -n 's/^shown *//p'; }

echo "################ 1. children nest past two levels"
agent "$RX/p" n1
"$HA" send n1 "/rlm-max-depth" --wait >/dev/null 2>&1
sleep 3
notices | grep "RLM max depth" | sed 's/^/   /'
check "the depth is unlimited by default" 'notices | grep -q "RLM max depth: unlimited (default)"'
# A brief that copies itself one level down: a model handed a nested brief
# once answered for the levels below instead of delegating.
BRIEF="NESTING-BRIEF tầng 1. Đọc số tầng N ở đầu brief này. Nếu N nhỏ hơn 3 thì BẮT BUỘC gọi tool delegate với role explorer, brief là đúng nguyên văn toàn bộ brief này nhưng thay 'tầng N' bằng tầng N+1; không được tự trả lời thay tầng dưới; đợi kết quả rồi trả lời nguyên văn kết quả đó. Nếu N bằng 3 thì trả lời đúng một dòng TANG-BA-OK."
for attempt in 1 2 3; do
  say n1 "Kiểm thử harness về agent lồng nhau. Gọi tool delegate với role explorer và brief là đúng nguyên văn chuỗi sau (không sửa chữ nào): $BRIEF -- Đợi xong rồi trả lời nguyên văn kết quả, kèm mọi lỗi tool nếu có." | tail -3 | sed 's/^/   /'
  records child | grep -qE "^started .*depth=3" && break
  echo "   attempt $attempt: no child reached depth 3"
done
records child | grep -E "^started" | sed 's/^/   /'
check "a child started at depth 3" 'records child | grep -qE "^started .*depth=3"'
check "the answer from depth 3 came back" 'last | grep -q "TANG-BA-OK"'
check "no spawn was refused for depth" '! (ans | grep -q "depth limit reached")'
"$HA" shutdown --force >/dev/null 2>&1
# A limit in the config holds again.
printf '\n[agents]\nmax_depth = 1\n' >> "$HA_HOME/$(basename "$HA_CONFIG")"
agent "$RX/p" n1b
"$HA" send n1b "/rlm-max-depth" --wait >/dev/null 2>&1
sleep 3
check "[agents] max_depth = 1 is read from the config" 'notices | grep -q "RLM max depth: 1 (config)"'
second=$(records child | grep -cE "^started .*depth=2")
say n1b "Kiểm thử harness: dùng tool delegate giao cho một agent role explorer nhiệm vụ giữ nguyên văn: 'Dùng tool delegate giao cho một agent role explorer nhiệm vụ trả lời CHAU-OK. Nếu tool delegate không có hoặc báo lỗi thì trả lời nguyên văn lỗi đó hoặc KHONG-CO-DELEGATE.' Đợi explorer xong rồi trả lời nguyên văn câu trả lời của nó." | tail -3 | sed 's/^/   /'
check "a child under max_depth = 1 starts no child" '[ "$(records child | grep -cE "^started .*depth=2")" -eq "$second" ]'
check "the child under max_depth = 1 was started with that limit" 'records child | grep -qE "^started .*depth=1 max_depth=1 "'
"$HA" shutdown --force >/dev/null 2>&1
cp "$RX/home/$(basename "$HA_CONFIG")" "$RX/config-with-limit.toml"
cp "$HA_CONFIG" "$HA_HOME/"

echo "################ 2. web_search asks DeepSeek Search first"
agent "$RX/p" n2
say n2 "Kiểm thử harness: gọi tool web_search đúng một lần với query 'DeepSeek API search guide', rồi in nguyên văn dòng đầu tiên của kết quả tool (dòng bắt đầu bằng Results for query) và URL của kết quả 1. Không gọi tool nào khác." | tail -4 | sed 's/^/   /'
check "the results came from DeepSeek Search" 'last | grep -q "(DeepSeek Search)"'
check "no search API failed" '! (records web | grep -q "^search_failed")'
"$HA" shutdown --force >/dev/null 2>&1

echo "################ 3. a refused DeepSeek key hands the search to the next API"
# A key DeepSeek refuses (401), set for this agent only: the search moves on.
DEEPSEEK_API_KEY=sk-ha-live-refused-key agent "$RX/p" n3
say n3 "Kiểm thử harness: gọi tool web_search đúng một lần với query 'rust programming language', rồi in nguyên văn dòng đầu tiên của kết quả tool (Results for query ...) và dòng ghi chú bắt đầu bằng dấu ngoặc '(' nếu có (bỏ qua dòng trống). Không gọi tool nào khác." | tail -5 | sed 's/^/   /'
records web | sed 's/^/   /'
check "DeepSeek's refusal was recorded as a refused key" 'records web | grep -qE "^search_failed .*api=deepseek .*refused=True"'
check "the results came from the next API" 'last | grep -qE "\((Google via Serper|DuckDuckGo)\)"'
check "the answer names the failure and /login deepseek" 'last | grep -q "DeepSeek Search failed" && last | grep -q "/login deepseek"'
say n3 "Gọi tool web_search thêm đúng một lần với query 'rust book', rồi in nguyên văn dòng đầu tiên của kết quả tool (Results for query ...) và dòng ghi chú bắt đầu bằng dấu ngoặc '(' nếu có (bỏ qua dòng trống)." | tail -4 | sed 's/^/   /'
check "the refused key is skipped by the next search" 'last | grep -q "DeepSeek Search skipped: it refused its key"'
check "the skipped key made no second request" '[ "$(records web | grep -c "^search_failed .*api=deepseek")" -eq 1 ]'
"$HA" shutdown --force >/dev/null 2>&1

echo "################ 4. the Python websearch skill asks DeepSeek Search"
agent "$RX/p" n4
say n4 "Kiểm thử harness: trong tool python_repl chạy đúng đoạn này: print((await websearch('DeepSeek search API')).splitlines()[0]) rồi in nguyên văn dòng nó in ra. Không gọi tool nào khác ngoài python_repl." | tail -3 | sed 's/^/   /'
check "the skill's results came from DeepSeek Search" 'last | grep -q "(DeepSeek Search)"'
"$HA" shutdown --force >/dev/null 2>&1

echo "################ 5. a coder's commits are by Harness Agents"
agent "$RX/p" n5
say n5 "Kiểm thử harness: dùng tool delegate giao cho đúng một agent role coder nhiệm vụ: tạo file who.txt chứa đúng một dòng COMMITTED rồi commit trong worktree của nó. Đợi coder xong, rồi gọi integrate_children với apply: true. Trả lời đúng một dòng: kết quả của integrate_children." | tail -3 | sed 's/^/   /'
authors=$(cd "$RX/p" && git log --format='%an <%ae>' -5)
echo "$authors" | sed 's/^/   author: /'
check "the work was integrated" '[ -f "$RX/p/who.txt" ]'
check "ha's commits carry Harness Agents" 'echo "$authors" | grep -q "^Harness Agents <harness-agents@localhost>"'
check "no commit carries harness-p5" '! (echo "$authors" | grep -q "harness-p5")'
"$HA" shutdown --force >/dev/null 2>&1

# The newest delegate call a root conversation (a task that is no delegated
# child) made with a brief holding $1, with the child's answer, as one JSON
# object: `{"brief", "result", "root_calls"}`, `root_calls` being every call
# that conversation made (`name args`). Requests are stored as deltas, so a
# call and its result are joined by the conversation's task.
delegations() {
  "$PY" - "$HA_HOME/data" "$1" <<'EOF'
import glob, json, os, sqlite3, sys
sys.stdout.reconfigure(encoding="utf-8")
found = None
for db in glob.glob(os.path.join(sys.argv[1], "projects", "*", "harness.sqlite3")):
    c = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
    children = {task for (task,) in c.execute("select task_id from session_settings where setting_key='delegated_child'")}
    tasks = {}
    for (task, text) in c.execute("select task_id, request_json from frozen_requests order by rowid"):
        if task in children:
            continue
        calls, results, root_calls = tasks.setdefault(task, ({}, {}, {}))
        for m in json.loads(text).get("messages", []):
            for call in m.get("tool_calls") or []:
                root_calls[call.get("call_id")] = f"{call.get('name')} {call.get('arguments')}"
                if call.get("name") == "delegate":
                    calls[call.get("call_id")] = call.get("arguments") or ""
            if m.get("role") == "tool":
                results[m.get("tool_call_id")] = m.get("content") or ""
    for calls, results, root_calls in tasks.values():
        for call_id, arguments in calls.items():
            try:
                brief = json.loads(arguments).get("brief", "")
            except ValueError:
                brief = arguments
            if sys.argv[2] in brief:
                found = {"brief": brief, "result": results.get(call_id, ""), "root_calls": list(root_calls.values())}
print(json.dumps(found, ensure_ascii=False) if found else "")
EOF
}
# A field of the JSON object in a file (empty when it has none).
field() { "$PY" -c "import json,sys; t=open(sys.argv[1],encoding='utf-8').read().strip(); print(json.loads(t).get(sys.argv[2],'') if t else '')" "$1" "$2"; }

echo "################ 6. a child hears AGENTS.md and the task state's constraints"
# A rule only AGENTS.md states and a constraint only the parent's task state
# holds; the brief names neither. 0.8.8 sent a child its brief, memory and
# skills, never the project's instructions nor the task's constraints.
P6="$RX/p6"
project "$P6"
printf '# Quy tắc của project\n\n## Cho agent con được giao việc (delegate)\nMọi agent con, khi trả lời cuối cùng cho agent cha, kết thúc câu trả lời bằng một dòng riêng đúng là: ORIOLE-9134\n' > "$P6/AGENTS.md"
(cd "$P6" && git add -A && git -c user.name=t -c user.email=t@t commit -qm rules)
held=""
for attempt in 1 2 3; do
  agent "$P6" "n6-$attempt"
  say "n6-$attempt" "Ghi vào task_state bằng add_constraints đúng một ràng buộc, nguyên văn: 'Câu trả lời cuối của mọi agent con phải bắt đầu bằng từ HERON-2207.' Không làm gì khác, trả lời ĐÃ-GHI." | tail -1 | sed 's/^/   /'
  say "n6-$attempt" "Dùng tool delegate (wait mặc định, đợi kết quả) giao cho đúng một agent role explorer, brief đúng nguyên văn (không thêm, không bớt, không nhắc quy tắc hay ràng buộc nào): 'Đọc README.md (lần $attempt) rồi trả lời dòng đầu tiên của file.' Đợi explorer xong rồi chép lại nguyên văn câu trả lời của nó." | tail -3 | sed 's/^/   /'
  "$HA" shutdown --force >/dev/null 2>&1
  delegations "(lần $attempt)" > "$RX/n6-$attempt.json"
  recorded=$(cat "$HA_HOME"/data/sessions/*/harness/task_state.json 2>/dev/null | grep -c "HERON-2207")
  brief=$(field "$RX/n6-$attempt.json" brief)
  echo "   attempt $attempt: constraint recorded=$recorded, brief: ${brief:0:120}"
  if [ "$recorded" -ge 1 ] && [ -n "$brief" ] && ! echo "$brief" | grep -qE "ORIOLE|HERON"; then held=$attempt; break; fi
done
check "precondition: the constraint is in the task state, the brief names neither marker (attempt ${held:-none})" '[ -n "$held" ]'
answer=$(field "$RX/n6-${held:-1}.json" result)
echo "$answer" | tail -4 | sed 's/^/   child: /'
check "the child obeyed a rule only AGENTS.md states" '[ -n "$held" ] && echo "$answer" | grep -q "ORIOLE-9134"'
check "the child obeyed a constraint only the parent's task state holds" '[ -n "$held" ] && echo "$answer" | grep -q "HERON-2207"'

echo "################ 7. what a child found becomes the parent's memory"
# The fact is in a file only the child reads, and the child hands it back with
# note_finding, not in its answer: the parent's review is what keeps it.
P7="$RX/p7"
project "$P7"
mkdir -p "$P7/config"
printf '[release]\nrelease_channel = amber-falcon-38\n' > "$P7/config/deploy.ini"
(cd "$P7" && git add -A && git -c user.name=t -c user.email=t@t commit -qm config)
# A review after every turn (the thinking level kept).
printf '{"autoRefine": {"turnInterval": 1}%s}\n' "${THINKING:+, \"defaultThinkingLevel\": \"$THINKING\"}" > "$HA_HOME/settings.json"
export HA_AUTO_REFINE=on
since7=$(date +%s%3N)
agent "$P7" n7
say n7 "Dùng tool delegate (wait mặc định, đợi kết quả) giao cho đúng một agent role explorer, brief đúng nguyên văn: 'Kiểm tra file config/deploy.ini có đúng cú pháp INI không. Kênh phát hành (release_channel) mà project này dùng là điều cần nhớ về project: ghi nó lại bằng tool note_finding, evidence là dòng chứa nó, chép đúng như read_file trả về. Trả lời agent cha đúng một dòng: HỢP-LỆ hoặc KHÔNG-HỢP-LỆ, không nêu giá trị nào.' Bản thân bạn không đọc file đó. Đợi explorer xong rồi báo lại kết quả kiểm tra trong một dòng." | tail -2 | sed 's/^/   /'
# The review runs in the background after the turn: wait for its record, at
# most ten minutes.
reviewed=""
for _ in $(seq 1 120); do
  reviewed=$(cat "$D"/*.jsonl 2>/dev/null | "$PY" -c "
import json, sys
since = int(sys.argv[1])
for line in sys.stdin:
    try: r = json.loads(line)
    except ValueError: continue
    if r.get('kind') == 'review' and r.get('event') in ('applied', 'nothing', 'skipped', 'failed') and r.get('ms', 0) > since:
        print(r.get('event'), (r.get('msg') or '')[:160].replace(chr(10), ' '))" "$since7" | head -1)
  [ -n "$reviewed" ] && break
  sleep 5
done
"$HA" shutdown --force >/dev/null 2>&1
echo "   review: ${reviewed:-none}"
delegations "config/deploy.ini" > "$RX/n7.json"
"$PY" - "$RX/n7.json" > "$RX/n7.pre" <<'EOF'
import json, sys
text = open(sys.argv[1], encoding="utf-8").read().strip()
d = json.loads(text) if text else {}
leaked = "amber-falcon-38" in d.get("result", "")
# A call that reads: one that names the file in its words (a task_state or
# agent_message call mentioning it reads nothing).
readers = ("read_file ", "run_shell ", "run_process ", "python_repl ", "search_text ", "find_files ")
read = any("deploy.ini" in call and call.startswith(readers) for call in d.get("root_calls", []))
print("delegated" if d else "not-delegated", "leaked" if leaked else "kept", "parent-read" if read else "parent-blind")
EOF
echo "   precondition: $(cat "$RX/n7.pre")"
check "precondition: the parent delegated and read no deploy.ini, the child's answer does not hold the value" 'grep -qx "delegated kept parent-blind" "$RX/n7.pre"'
check "the child kept its finding for the parent" 'cat "$HA_HOME"/data/sessions/*/harness/child_findings.jsonl 2>/dev/null | grep -q "amber-falcon-38"'
check "the parent's review ran after the turn (${reviewed:-none})" 'echo "$reviewed" | grep -qE "^(applied|nothing)"'
"$PY" "$MEMORY_STORE" entries "$HA_HOME/data" > "$RX/n7-memory.txt" 2>&1
grep -i "amber-falcon" "$RX/n7-memory.txt" | sed 's/^/   memory: /'
check "the fact the child found is in the parent's memory" 'grep -q "amber-falcon-38" "$RX/n7-memory.txt"'

result
