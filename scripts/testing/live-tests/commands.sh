#!/usr/bin/env bash
# The app's commands as trying them found them (2026-11-11), with a real model:
#   1. a command sent with `ha send` that opens a selector answers in words -
#      `/tree` and `/fork` with the numbered turns, `/btw` with its answer,
#      `/plugins` with the services - and leaves no selector open for the
#      terminal that attaches next (0.9.5 printed nothing, and the selector it
#      left took the next terminal's keys: its Enter connected a service);
#   2. `--thinking` is the session's level, not the default for new sessions
#      (0.9.5 saved `ha chat --thinking low` as `defaultThinkingLevel`);
#   3. a clone is named "(clone)", and a clone of a fork carries one mark
#      (0.9.5 named it "... (fork) (fork)").
# Every check prints PASS or FAIL; the conversation, the screens and the
# diagnostics stay in $LIVE_ROOT/commands as evidence.
#
#   bash scripts/testing/live-tests/commands.sh
set -u
source "$(dirname "$0")/env.sh"
fresh commands
export HA_AUTO_REFINE=off HA_PYTHON=${HA_PYTHON:-$PY}
HERE="$(cd "$(dirname "$0")" && (pwd -W 2>/dev/null || pwd))"
project "$RX/p"
shot() { "$PY" "$HERE/screen.py" "$1" 40 140; }
sent() { "$HA" send c "$1" --wait 2>&1; }

echo "################ 1. a sent selector command answers in words and leaves no selector"
agent "$RX/p" c
say c "Ghi nhớ mã TREE-4471. Trả lời đúng một dòng: DA-NHO-1" | tail -1 | sed 's/^/   /'
say c "Ghi nhớ thêm mã TREE-9025. Trả lời đúng một dòng: DA-NHO-2" | tail -1 | sed 's/^/   /'
check "precondition: two turns answered" 'ans | grep -q DA-NHO-1 && ans | grep -q DA-NHO-2'
tree=$(sent "/tree"); echo "$tree" | sed 's/^/   tree: /' | head -6
check "/tree answers with the numbered turns" 'echo "$tree" | grep -q "^1\. " && echo "$tree" | grep -q "/tree <number>"'
fork=$(sent "/fork"); echo "$fork" | sed 's/^/   fork: /' | head -4
check "/fork answers with the numbered messages" 'echo "$fork" | grep -q "^1\. " && echo "$fork" | grep -q "/fork <number>"'
side=$(sent "/btw Mã đầu tiên tôi nhờ nhớ là gì? Chỉ trả lời mã."); echo "$side" | sed 's/^/   btw: /' | head -4
check "/btw answers with the side answer" 'echo "$side" | grep -q "TREE-4471"'
plugins=$(sent "/plugins"); echo "$plugins" | sed 's/^/   plugins: /' | head -3
check "/plugins answers with the services and how to connect one" 'echo "$plugins" | grep -q "/plugins connect <id>"'
# The terminal that attaches next finds the conversation, not a selector.
(cd "$RX/p" && "$PROBE" "$RX/attach.bin" "$HA" "wait:7000,settle:1500" -- attach c >/dev/null 2>&1)
shot "$RX/attach.bin" > "$RX/attach.txt" 2>&1
check "the next terminal finds no selector left open" '[ -s "$RX/attach.txt" ] && grep -q "Yêu cầu" "$RX/attach.txt" && ! grep -qE "cây hội thoại|fork từ tin nhắn|conversation tree|/btw ·|CÂY ·|FORK ·" "$RX/attach.txt"'

echo "################ 2. --thinking is the session's level, not the default"
check "precondition: the default is $THINKING" '[ -n "$THINKING" ] && grep -q "\"defaultThinkingLevel\": *\"$THINKING\"" "$HA_HOME/settings.json"'
(cd "$RX/p" && "$PROBE" "$RX/low.bin" "$HA" "wait:9000" -- chat --model "$MODEL" --thinking low --approval full-auto >/dev/null 2>&1)
sed 's/^/   settings: /' "$HA_HOME/settings.json"; echo
check "a session started with --thinking low leaves the default at $THINKING" 'grep -q "\"defaultThinkingLevel\": *\"$THINKING\"" "$HA_HOME/settings.json"'

echo "################ 3. a clone of a fork carries one mark, naming what made it"
sent "/name nhanh-goc" >/dev/null
# Before the second message, so the fork keeps the first (before the first,
# a fork starts afresh); a branch takes its title at its first turn.
sent "/fork 2" | sed 's/^/   /' | head -2
say c "Trả lời đúng một dòng: TRONG-FORK" | tail -1 | sed 's/^/   /'
sent "/clone" | sed 's/^/   /' | head -2
say c "Trả lời đúng một dòng: TRONG-CLONE" | tail -1 | sed 's/^/   /'
# Every conversation's title, as the project's store keeps it.
titles() {
  PYTHONIOENCODING=utf-8 "$PY" - "$HA_HOME/data" <<'EOF'
import glob, os, sqlite3, sys
for db in glob.glob(os.path.join(sys.argv[1], "projects", "*", "harness.sqlite3")):
    c = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
    for (title,) in c.execute("select setting_value from session_settings where setting_key = 'title' order by updated_at"):
        print(title)
EOF
}
titles > "$RX/titles.txt"
sed 's/^/   title: /' "$RX/titles.txt"
check "precondition: the fork is named after its conversation" 'grep -qx "nhanh-goc (fork)" "$RX/titles.txt"'
check "the clone of the fork is named (clone), with one mark" 'grep -qx "nhanh-goc (clone)" "$RX/titles.txt" && ! grep -q "(fork) (fork)" "$RX/titles.txt"'

"$HA" shutdown --force >/dev/null 2>&1
result
