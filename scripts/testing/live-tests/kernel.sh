#!/usr/bin/env bash
# The Python kernel with a real model: rlm.harness kept by the Rust host, the
# memory tool and the kernel seeing each other's writes, the factory behind
# /factory, prime-agent's newer rlm (collect's answer_text, rename, bash(),
# mcp), a namespace that survives a restart, and /refinements off refusing the
# kernel's harness. Every check prints PASS or FAIL; the conversation and the
# state on disk stay in $LIVE_ROOT/kernel as evidence.
#
#   bash scripts/testing/live-tests/kernel.sh
set -u
source "$(dirname "$0")/env.sh"
fresh kernel
export HA_AUTO_REFINE=off HA_PYTHON=${HA_PYTHON:-$PY}
D=$HA_HOME/data
project "$RX/p"
# What the kernel printed, from the journal's tool results: the model retells
# a print in its own words (`**KERNEL_LIST:** ...`), the journal keeps it.
printed() { "$PY" - "$D" "$1" <<'EOF'
import glob, os, sqlite3, sys
sys.stdout.reconfigure(encoding="utf-8")
for db in glob.glob(os.path.join(sys.argv[1], "projects", "*", "harness.sqlite3")):
    for (text,) in sqlite3.connect(f"file:{db}?mode=ro", uri=True).execute("select event_json from events"):
        at = text.find(sys.argv[2] + "[")
        if at >= 0:
            print(text[at:at + 200])
EOF
}
state() { "$PY" "$MEMORY_STORE" entries "$D"; }

echo "################ 1. rlm.harness through the host: create, update by global: id, search, overview"
agent "$RX/p" r1
say r1 "Dùng tool ipython (không dùng tool memory). Trong một cell: e = rlm.harness.create_memory('Ngôn ngữ trả lời', 'Người dùng muốn trả lời bằng tiếng Việt', global_=True); rồi u = rlm.harness.update_memory('global:' + e.id, 'Ngôn ngữ trả lời', 'Người dùng muốn trả lời bằng tiếng Việt, ngắn gọn'); in ra e.id, u.version, u.scope, [x.id for x in rlm.harness.search('tiếng Việt', global_=True)] và dòng đầu của rlm.harness.overview(global_=True). Báo lại đúng những gì cell in ra." | tail -8
echo "-- state on disk:"; state
check "kernel write landed in the global store, updated to v2" "state | grep -q 'global memory ngon_ngu_tra_loi v2 | Người dùng muốn trả lời bằng tiếng Việt, ngắn gọn'"
check "the kernel no longer creates a lock directory" "[ ! -d '$D/harness/memory.lock' ] && [ ! -d '$D/harness/harness_state.json.lock' ]"
check "the store keeps no state file beside it" "[ ! -e '$D/harness/harness_state.json' ]"
check "the kernel's writes are events by the kernel" "\"\$PY\" \"\$MEMORY_STORE\" events '$D/harness' | grep -q '^update|update|kernel|ngon_ngu_tra_loi|v2|'"

echo "################ 2. the memory tool and rlm.harness see each other's writes"
say r1 "Dùng tool memory (action write, scope local) ghi một entry tiêu đề 'Nhánh làm việc', nội dung 'Làm trên nhánh feature/rlm'. Sau đó dùng ipython chạy print('KERNEL_LIST=' + str([(x.id, x.content) for x in rlm.harness.list('memory')])). Rồi dùng ipython tạo rlm.harness.create_prompt_note('Định dạng commit', 'Commit message dạng loai: mo ta') (local), và dùng tool memory action list để liệt kê. Báo lại hai danh sách." | tail -10
check "memory tool entry visible in the local store" "state | grep -q 'local memory nhanh_lam_viec'"
check "kernel prompt note visible in the local store" "state | grep -q 'local prompt dinh_dang_commit'"
check "the kernel listed the tool's entry" "printed KERNEL_LIST= | grep -q \"KERNEL_LIST=\[('nhanh_lam_viec'\""

echo "################ 3. a rejected write is refused with the host's words"
say r1 "Dùng ipython chạy đúng: rlm.harness.create_skill('Deploy', 'Chạy deploy', reference={'type':'python','import':'deploy'}) và in ra exception (type và message)." | tail -4
check "skill without a callable refused" "ans | grep -q 'callable or call_pattern'"

echo "################ 4. the factory is off until /factory on, then a small machine runs"
say r1 "Dùng ipython: thử rlm.harness.create_factory('Hai bước', 'demo', dag={'nodes':[{'id':'a','prompt':'x'}]}) và in exception message." | tail -3
check "factory refused while disabled, naming /factory on" "ans | grep -q 'run /factory on'"
say r1 "/factory on" | tail -2
check "/factory on saved factory.enabled" "grep -q '\"enabled\": true' '$HA_HOME/settings.json'"
say r1 "Dùng ipython. Đọc rlm.factory.help() để biết cú pháp dag. Tạo một factory bằng rlm.harness.create_factory với dag 2 node: node 'viet' (agent con ghi file a.txt nội dung A vào thư mục dự án) và node 'kiem' phụ thuộc 'viet' (agent con ghi file b.txt nội dung B). Chạy bằng await rlm.factory.run(<id>), rồi lặp await rlm.factory.status(run_id) mỗi 15 giây cho tới khi run kết thúc (tối đa 6 phút). Báo trạng thái cuối và nội dung a.txt, b.txt." | tail -12
check "factory stored as a factory entry" "state | grep -q ' factory '"
check "both factory states ran (a.txt and b.txt exist)" "[ -f '$RX/p/a.txt' ] && [ -f '$RX/p/b.txt' ]"

echo "################ 5. prime's newer rlm: collect's answer_text, rename, bash(), mcp, list_subagents"
say r1 "Dùng ipython: h = await rlm.spawn('Trả lời đúng một từ: PONG', name='ping'); r = await rlm.collect([h], timeout_ms=180000); print('ANSWER_TEXT=' + str(r[0].answer_text)). Sau đó await rlm.rename('ping2', session_id=h) và in [s.session_name for s in await rlm.list_subagents()]. Rồi print('BASH=' + (await bash('echo from' + '-bash')).output.strip()) và print('MCP=' + str(await mcp.list_connections())). Báo nguyên văn output." | tail -10
check "collect returned the child's full answer_text" "ans | grep -q 'ANSWER_TEXT=PONG'"
check "bash() ran" "ans | grep -q 'BASH=from-bash'"

echo "################ 6. the kernel namespace survives a restart"
say r1 "Dùng ipython: bien_kiem_tra = 4000 + 242; in bien_kiem_tra." | tail -2
"$HA" shutdown --force >/dev/null 2>&1
(cd "$RX/p" && "$PROBE" "$RX/resume.bin" "$HA" "wait:10000" -- chat --resume latest --model "$MODEL" "${TH[@]}" --approval full-auto >/dev/null)
say r1 "Dùng ipython chạy print('REVIVED=' + str(bien_kiem_tra)) (không gán lại). Báo nguyên văn output." | tail -3
check "variable revived after restart" "ans | grep -q 'REVIVED=4242'"

echo "################ 7. /refinements off: the kernel's harness refuses"
say r1 "/refinements off" | tail -2
say r1 "Dùng ipython chạy một cell: thử gọi rlm.harness.list('memory'); nếu nó ném exception e thì print('REFUSED=' + str(e)). Báo nguyên văn output." | tail -3
check "rlm.harness refused while refinements are off" "ans | grep -q 'REFUSED=Harness refinements are disabled'"
say r1 "/refinements on" | tail -2
"$HA" shutdown --force >/dev/null 2>&1

echo "################ 8. a new conversation's digest carries the kernel's global memory"
agent "$RX/p" r8
say r8 "Không dùng tool nào. Theo memory bạn có, tôi muốn được trả lời bằng ngôn ngữ nào và như thế nào?" | tail -4
check "the next conversation knows the preference" "last | grep -qi 'ngắn gọn\|ngan gon'"
"$HA" shutdown --force >/dev/null 2>&1

result
