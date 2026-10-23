#!/usr/bin/env bash
# The fullscreen TUI with a real model: leaving a session for the
# agents view (`/agents`, a bare `/resume`) does not print the conversation
# into the primary screen first, and the mouse wheel moves the window's rows
# with the terminal instead of writing every row again, and Delete removes a
# conversation from the agents view and /resume. Every check prints
# PASS or FAIL; the recordings stay in $LIVE_ROOT/tui as evidence.
#
#   bash scripts/testing/live-tests/tui.sh
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

echo "################ 4. the agents view keeps up with a forest of subagents"
# Children with long briefs, as a real run's are: the view copied every brief
# into every row of every frame, and redrew four times a second.
task=$(for _ in $(seq 1 40); do printf 'Đọc README.md và kể lại dòng đầu tiên của nó trong một câu ngắn. '; done)
say t1 "Dùng tool delegate với wait=false giao cho 6 agent role explorer; mỗi brief chép nguyên văn nhiệm vụ sau: $task Giao xong thì trả lời ngay đúng một dòng MOC-GIAO, không đợi kết quả." | tail -1
check "the model delegated and answered" 'last | grep -q MOC-GIAO'
"$HA" list --json > "$RX/forest.json"
children=$("$PY" -c "import json,sys; d=json.load(open(sys.argv[1],encoding='utf-8')); a=d if isinstance(d,list) else d.get('agents',d); print(max(len(x.get('subagents') or []) for x in a))" "$RX/forest.json")
echo "   t1 has $children subagents"
steps="wait:6000,key:/agents,enter,until:subagents:20000,settle:3000"
steps="$steps,down,settle:3000,enter,settle:3000,down,settle:3000,down,settle:3000,down,settle:3000,up,settle:3000"
steps="$steps,esc,until:MOC-GIAO:20000,settle:3000"
(cd "$RX/p" && "$PROBE" "$RX/forest.bin" "$HA" "$steps" -- attach t1 >/dev/null)
sed 's/^/   /' "$RX/forest.bin.marks" | grep -E 'until|settled'
opened=$(awk '/until found subagents/ { print $(NF-1) }' "$RX/forest.bin.marks")
# The keys pressed in the view, after the frame that opened it (timed by the
# check before) and up to Esc: back in the session, its running children
# keep the spinner turning, which is not a key's frame.
slowest=$(awk '$2 == "esc" { exit } /settled/ { if (++seen > 1 && $3 > max) max = $3 } END { print max + 0 }' "$RX/forest.bin.marks")
echo "   the list showed after ${opened:-?} ms; the slowest key in the view settled ${slowest} ms after it"
check "the agents view listed the forest within 3 s" '[ -n "$opened" ] && [ "$opened" -lt 3000 ]'
check "every key in the view settled within 400 ms" '[ "$slowest" -lt 400 ]'
check "Esc went back to the session" 'grep -q "until found MOC-GIAO" "$RX/forest.bin.marks"'

echo "################ 5. the reference panels line up"
# Each panel is read while it is open: a wait of its own length marks it.
# Seen in 0.7.2: a key longer than the column ran into its description
# (`Alt+Enternew line`), a wrapped row of commands started at the panel's
# edge, and /context listed its blocks as unaligned text.
panels=(/hotkeys /help /context)
steps="wait:6000"
n=0
for panel in "${panels[@]}"; do
  n=$((n + 1))
  steps="$steps,key:$panel,enter,wait:$((2500 + n)),esc,key:$(printf '\025'),wait:600"
done
(cd "$RX/p" && "$PROBE" "$RX/panels.bin" "$HA" "$steps" -- attach t1 >/dev/null)
n=0
for panel in "${panels[@]}"; do
  n=$((n + 1))
  at=$(awk -v w="wait:$((2500 + n))" 'found { print $1; exit } $2 == w { found = 1 }' "$RX/panels.bin.marks")
  shot "$RX/panels.bin" "$at" > "$RX/panel-${panel#/}.txt"
done
grep -E "Alt\+Enter|Shift\+Alt\+M" "$RX/panel-hotkeys.txt" | sed 's/^/   /'
check "/hotkeys keeps every key apart from its description" \
  'grep -q "Alt+Enter  .*new line" "$RX/panel-hotkeys.txt" && ! grep -qE "Alt\+Enternew|Shift\+Alt\+Mnext" "$RX/panel-hotkeys.txt"'
# The first row of a group and the row its commands wrap to begin at the same
# column: `│ │  Run      /goal` and then `│ │           /steer`.
check "/help wraps a group's commands under its column" \
  '"$PY" - "$RX/panel-help.txt" <<'"'"'EOF'"'"'
import re, sys
rows = open(sys.argv[1], encoding="utf-8").read().splitlines()
at = next((i for i, row in enumerate(rows) if re.search(r"Run +/goal", row)), None)
if at is None or at + 1 >= len(rows) or "/" not in rows[at + 1]:
    sys.exit(1)
# The Run group runs past one row at this width; its next row is its rest.
sys.exit(0 if rows[at + 1].index("/") == rows[at].index("/goal") else 1)
EOF'
grep -E "tokens ·" "$RX/panel-context.txt" | head -3 | sed 's/^/   /'
check "/context lists its blocks as fields, with their tokens" \
  'grep -q "Packet" "$RX/panel-context.txt" && grep -qE "  [a-z_-]+ +[0-9.k]+ tokens · " "$RX/panel-context.txt"'
"$HA" shutdown --force >/dev/null 2>&1

echo "################ 6. Delete removes a conversation from the agents view and /resume"
# (0.7.9) The agents view had Ctrl+X only, and on an idle agent it stopped
# it: a conversation the user wanted gone stayed. Delete (or Ctrl+X) twice on
# an idle agent stops it and deletes its conversation; /resume no longer has it.
agent "$RX/p" t9
say t9 "Trả lời đúng một dòng: MOC-XOA" | tail -1
t9=$("$HA" list --json | "$PY" -c "import json,sys; d=json.load(sys.stdin); a=d if isinstance(d,list) else d.get('agents',d); print([x.get('conversation') or '' for x in a if x.get('name')=='t9'][0])")
echo "   t9's conversation: $t9"
(cd "$RX/p" && "$PROBE" "$RX/delete.bin" "$HA" "wait:6000,key:t9,wait:1500,delete,wait:600,delete,until:deleted:8000,wait:1500,esc,esc,wait:1000" -- agents >/dev/null)
check "the second Delete deleted it" 'grep -q "until found deleted" "$RX/delete.bin.marks"'
check "the agent was stopped" '! "$HA" list --json | grep -q "\"name\": *\"t9\""'
(cd "$RX/p" && "$PROBE" "$RX/after-delete.bin" "$HA" "wait:6000,key:MOC,wait:1500,esc,esc,wait:1000" -- agents >/dev/null)
"$PY" "$HERE/screen.py" "$RX/after-delete.bin" --upto "$(mark "$RX/after-delete.bin" esc 1)" > "$RX/after-delete.txt"
check "the agents view no longer lists it" '! grep -q "t9" "$RX/after-delete.txt"'
check "it is in the list of deleted conversations" '[ -n "$t9" ] && grep -rq "$t9" "$RX/home/data" --include=hidden-conversations.json'
"$HA" shutdown --force >/dev/null 2>&1

result
