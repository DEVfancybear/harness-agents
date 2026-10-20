#!/usr/bin/env bash
# The fullscreen TUI with a real model: leaving a session for the
# agents view (`/agents`, a bare `/resume`) does not print the conversation
# into the primary screen first, and the mouse wheel moves the window's rows
# with the terminal instead of writing every row again. Every check prints
# PASS or FAIL; the recordings stay in $LIVE_ROOT/tui as evidence.
#
#   bash scripts/live-tests/tui.sh
#
# The wheel needs a ConPTY that passes the mouse through: Windows 10's own
# does not. Point PROBE_WT at a folder holding pty_probe.exe beside a newer
# conpty.dll and Windows Terminal's OpenConsole.exe; without it the wheel
# checks are skipped.
set -u
source "$(dirname "$0")/env.sh"
fresh tui
export HA_AUTO_REFINE=off HA_PYTHON=${HA_PYTHON:-$PY}
HERE=$(cd "$(dirname "$0")" && (pwd -W 2>/dev/null || pwd))
project "$RX/p"
# The screen a recording showed, every row, up to a byte offset.
shot() { "$PY" "$HERE/screen.py" "$1" --all --upto "$2"; }
# The offset of the <n>th mark named <step> (`enter`, `settled`...).
mark() { awk -v step="$2" -v n="$3" '$2 == step { if (++seen == n) { print $1; exit } }' "$1.marks"; }

echo "################ 1. a conversation longer than the screen"
agent "$RX/p" t1
say t1 "Trả lời đúng một dòng: MOC-DAU-TIEN" | tail -1
# Long rows: a window of short ones costs little to write again, whatever
# the wheel does; the conversations that lagged were dense.
say t1 "Viết một danh sách đánh số từ 1 đến 120, mỗi dòng đúng dạng 'muc <số>: ' rồi một câu tiếng Việt dài khoảng 110 ký tự mô tả một món ăn khác nhau, không thêm gì khác." | tail -2
say t1 "Trả lời đúng một dòng: MOC-CUOI" | tail -1
check "the model answered the last turn" 'last | grep -q MOC-CUOI'

echo "################ 2. the agents view opens without printing the conversation"
(cd "$RX/p" && "$PROBE" "$RX/agents.bin" "$HA" "wait:6000,key:/agents,wait:1500,enter,wait:4000" -- attach t1 >/dev/null)
at=$(mark "$RX/agents.bin" enter 1)
shot "$RX/agents.bin" "$at" > "$RX/before-agents.txt"
check "the session showed the newest answer" 'grep -q MOC-CUOI "$RX/before-agents.txt"'
"$PY" - "$RX/agents.bin" "$at" > "$RX/after-agents.txt" <<'EOF'
import sys
data = open(sys.argv[1], "rb").read()[int(sys.argv[2]):]
print(len(data), sum(data.count(f"muc {n}".encode()) for n in range(1, 121)), data.count(b"MOC-DAU-TIEN"))
EOF
read -r written lines first < "$RX/after-agents.txt"
echo "   after /agents: $written bytes, $lines list rows and $first first-answer marks written"
check "no row of the conversation was printed on the way to the view" '[ "$lines" -eq 0 ] && [ "$first" -eq 0 ]'
check "the agents view is on screen" '"$PY" "$HERE/screen.py" "$RX/agents.bin" | grep -q "Agents"'

echo "################ 3. the wheel moves the rows with the terminal"
if [ -n "${PROBE_WT:-}" ]; then
  steps="wait:6000,settle:20000"
  for _ in 1 2; do steps="$steps,wheelup:20,settle:20000"; done
  for _ in 1 2; do steps="$steps,wheeldown:20,settle:20000"; done
  (cd "$RX/p" && "$PROBE_WT/pty_probe.exe" "$RX/wheel.bin" "$HA" "$steps" -- attach t1 >/dev/null)
  start=$(mark "$RX/wheel.bin" "settled" 1)
  up1=$(mark "$RX/wheel.bin" "settled" 2)
  up2=$(mark "$RX/wheel.bin" "settled" 3)
  down2=$(mark "$RX/wheel.bin" "settled" 5)
  shot "$RX/wheel.bin" "$start" > "$RX/wheel-start.txt"
  shot "$RX/wheel.bin" "$up2" > "$RX/wheel-up.txt"
  shot "$RX/wheel.bin" "$down2" > "$RX/wheel-back.txt"
  per_notch=$(( (up2 - start) / 40 ))
  echo "   40 notches up wrote $((up2 - start)) bytes: $per_notch per notch"
  grep -E "settled" "$RX/wheel.bin.marks" | sed 's/^/   /'
  check "scrolled back, older rows are on screen and the newest is not" 'grep -q "muc " "$RX/wheel-up.txt" && ! grep -q MOC-CUOI "$RX/wheel-up.txt"'
  check "a notch writes the rows that came in, not the window (< 2000 bytes)" '[ "$per_notch" -lt 2000 ]'
  check "scrolled down again, the screen is the one it left" 'diff -q "$RX/wheel-start.txt" "$RX/wheel-back.txt" >/dev/null'
else
  echo "SKIP  the wheel checks: PROBE_WT is not set"
fi
"$HA" shutdown --force >/dev/null 2>&1

result
