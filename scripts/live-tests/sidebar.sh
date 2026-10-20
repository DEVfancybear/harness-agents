#!/usr/bin/env bash
# herdr's sidebar with a real model: while a turn runs the session's row says
# `working`; a delegate the turn starts gets its own row, and Alt+its number
# opens its pane beside the conversation with what it called and answered;
# the project's other agents are listed, and Alt+their number switches the
# terminal to one of them. Every check prints PASS or FAIL; the recordings and
# the screens stay in $LIVE_ROOT/sidebar as evidence.
#
#   bash scripts/live-tests/sidebar.sh
#
# The clicks are not driven: Windows 10's own ConPTY does not pass the mouse
# through (see tui.sh); the keys reach the same rows.
set -u
source "$(dirname "$0")/env.sh"
fresh sidebar
export HA_AUTO_REFINE=off HA_PYTHON=${HA_PYTHON:-$PY}
HERE=$(cd "$(dirname "$0")" && (pwd -W 2>/dev/null || pwd))
project "$RX/p"
# The screen a recording showed, every row, up to a byte offset.
shot() { "$PY" "$HERE/screen.py" "$1" 40 140 --all --upto "$2"; }
# The offset of the <n>th mark of a step (`until`, `settled`, `alt:2`...).
mark() { awk -v step="$2" -v n="$3" '$2 == step { if (++seen == n) { print $1; exit } }' "$1.marks"; }
# The id of the agent no name was given yet.
unnamed() {
  "$HA" list --json | "$PY" -c "import json,sys; d=json.load(sys.stdin); a=d if isinstance(d,list) else d.get('agents',d); print([x['id'] for x in a if not x.get('name')][0])"
}

echo "################ 1. another agent of the project"
agent "$RX/p" other
say other "Trả lời đúng một dòng: MOC-KHAC" | tail -1
check "the other agent answered" 'last | grep -q MOC-KHAC'

echo "################ 2. a turn that delegates, seen from the sidebar"
# No comma: the probe's steps are a comma list. The idle dock's hints
# (`Enter gửi`) come back only once the turn has ended: a sidebar row that
# moves down is drawn again, so its words cannot say that.
prompt="Dùng tool delegate giao cho đúng một agent role explorer nhiệm vụ: đọc README.md và báo lại dòng đầu tiên của nó. Đợi kết quả của explorer rồi trả lời đúng một dòng: MOC-XONG"
steps="wait:9000,key:$prompt,enter,until:working:30000,settle:1500"
steps="$steps,until:Enter g:400000,settle:4000,alt:2,settle:3000,alt:1,settle:3000"
(cd "$RX/p" && "$PROBE" "$RX/run.bin" "$HA" "$steps" -- chat --model "$MODEL" --approval full-auto >/dev/null)
sed 's/^/   /' "$RX/run.bin.marks" | grep -E 'until|settled'
"$HA" rename "$(unnamed)" main1 >/dev/null
shot "$RX/run.bin" "$(mark "$RX/run.bin" settled 1)" > "$RX/1-working.txt"
shot "$RX/run.bin" "$(mark "$RX/run.bin" alt:2 1)" > "$RX/2-done.txt"
shot "$RX/run.bin" "$(mark "$RX/run.bin" alt:1 1)" > "$RX/3-pane.txt"
shot "$RX/run.bin" "$(mark "$RX/run.bin" settled 4)" > "$RX/4-closed.txt"
check "the run's turn ended" 'grep -q "until found Enter g" "$RX/run.bin.marks"'
check "while it ran, the session's row said working" 'grep -qE "1 [^ ]+ main +working" "$RX/1-working.txt"'
check "the workspace names the project and its branch" 'grep -q " p " "$RX/1-working.txt" && grep -qE "^ +(master|main) " "$RX/1-working.txt"'
check "the model delegated: an explorer row, done" 'grep -qE "2 [^ ]+ explorer +done" "$RX/2-done.txt"'
check "the other agent is listed under other agents" 'grep -q "other agents" "$RX/2-done.txt" && grep -qE "[0-9] [^ ]+ other +(idle|done)" "$RX/2-done.txt"'
check "Alt+2 opened the explorer's pane" 'grep -q "explorer · done" "$RX/3-pane.txt" && grep -q "alt+1 back" "$RX/3-pane.txt"'
# What the pane's columns hold: every row from its top-left corner's column.
pane_text() { "$PY" -c "import sys; rows=open(sys.argv[1],encoding='utf-8').read().splitlines(); col=next(r.index('╭ 2 explorer') for r in rows if '╭ 2 explorer' in r); print('\n'.join(r[col:] for r in rows))" "$1"; }
check "the pane shows the call the explorer made" 'pane_text "$RX/3-pane.txt" | grep -qE "read_file|search_text|list_files|python"'
check "Alt+1 closed it" '! grep -q "alt+1 back" "$RX/4-closed.txt"'
check "the conversation came back to its full width" 'grep -q "MOC-XONG" "$RX/4-closed.txt"'

echo "################ 3. Alt and the other agent's number switches to it"
number=$(grep -oE "[0-9] [^ ]+ other +(idle|done)" "$RX/2-done.txt" | head -1 | cut -d" " -f1)
echo "   the other agent is row $number"
steps="wait:6000,settle:3000,alt:$number,until:MOC-KHAC:20000,settle:3000"
(cd "$RX/p" && "$PROBE" "$RX/switch.bin" "$HA" "$steps" -- attach main1 >/dev/null)
sed 's/^/   /' "$RX/switch.bin.marks" | grep -E 'until|settled'
shot "$RX/switch.bin" "$(mark "$RX/switch.bin" settled 2)" > "$RX/5-switched.txt"
check "the terminal now draws the other agent's conversation" 'grep -q MOC-KHAC "$RX/5-switched.txt" && ! grep -q MOC-XONG "$RX/5-switched.txt"'
check "and lists the first session among the others" 'grep -qE "[0-9] [^ ]+ main1 +(idle|done)" "$RX/5-switched.txt"'
"$HA" shutdown --force >/dev/null 2>&1

result
