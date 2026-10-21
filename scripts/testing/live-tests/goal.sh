#!/usr/bin/env bash
# /goal with a real model: a goal the model never completes is continued past
# the old cap of ten turns and stops only when the user pauses it; and the file
# tools' refusal of a path outside the workspace says where such a file goes.
# Every check prints PASS or FAIL; the conversation stays in $LIVE_ROOT/goal.
#
#   bash scripts/testing/live-tests/goal.sh
set -u
source "$(dirname "$0")/env.sh"
fresh goal
export HA_AUTO_REFINE=off HA_PYTHON=${HA_PYTHON:-$PY}
project "$RX/p"
# The number of turns `/goal status` says the goal was continued.
continued() { "$HA" send g1 "/goal status" --wait 2>&1 | sed -n 's/.*Continued: \([0-9]*\).*/\1/p' | head -1; }

echo "################ 1. a goal goes on past ten automatic turns"
agent "$RX/p" g1
# Sent without waiting: the goal never finishes, so a wait for it would not end.
"$HA" send g1 "/goal Đây là bài kiểm thử của harness: mỗi lượt chỉ trả lời đúng một dòng 'CHUA-XONG' rồi kết thúc lượt. Tuyệt đối không gọi goal_complete và không gọi tool nào." >/dev/null 2>&1
turns=0
for _ in $(seq 1 60); do
  turns=$(continued); turns=${turns:-0}
  [ "$turns" -ge 12 ] && break
  sleep 5
done
echo "   continued $turns turn(s)"
"$HA" send g1 "/goal status" --wait 2>&1 | sed 's/^/   /' | head -6
check "the goal was continued past ten turns" '[ "$turns" -ge 12 ]'
check "it is still active, not paused by a count" '"$HA" send g1 "/goal status" --wait 2>&1 | grep -qi "status: *active"'

echo "################ 2. /goal pause stops it"
"$HA" send g1 "/goal pause" --wait >/dev/null 2>&1
sleep 8
paused_at=$(continued)
sleep 20
check "it is paused" '"$HA" send g1 "/goal status" --wait 2>&1 | grep -qi "status: *paused"'
check "no turn ran after the pause" '[ "$(continued)" = "$paused_at" ]'
"$HA" send g1 "/goal clear" --wait >/dev/null 2>&1

echo "################ 3. the workspace refusal says where a file elsewhere goes"
TMPFILE=$(cd "$RX" && (pwd -W 2>/dev/null || pwd))/outside.txt
# A new conversation: g1's still says to answer CHUA-XONG every turn, and a
# model that keeps to it never calls the tool.
"$HA" shutdown --force >/dev/null 2>&1
agent "$RX/p" g3
say g3 "Kiểm thử: gọi tool write_file (không dùng python_repl hay bash) để ghi chữ 'x' vào đường dẫn tuyệt đối $TMPFILE. In nguyên văn thông báo lỗi tool trả về, không làm gì thêm." | tail -3
check "the write outside the workspace was refused" '[ ! -f "$RX/outside.txt" ]'
check "the refusal names bash or python_repl" 'last | grep -qi "bash or python_repl"'
"$HA" shutdown --force >/dev/null 2>&1

result
