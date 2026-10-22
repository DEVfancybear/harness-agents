#!/bin/bash
# Computer use, live: an agent operates this Windows desktop with the `computer`
# tool (screenshots, real mouse and keyboard), and what it did is checked from
# outside the agent, not from what it says.
#
#   1. A window only the screen shows (probe-window.ps1): the agent reads the code
#      in it off a screenshot, clicks into its text box, types Vietnamese and a
#      path, and presses Enter; the window records what arrived.
#   2. The browser: the agent opens Edge, goes to a local form (form_server.py),
#      types a Vietnamese name, picks a colour, submits it and reads back the
#      receipt number the server made at that moment.
#   3. Flappy Bird (C:\Users\<you>\Downloads\flappy-bird): the agent opens it from
#      File Explorer (This PC -> Downloads -> flappy-bird -> index.html) and flies
#      it by hand with Space, flight after flight. Then it reaches 20 gates in one
#      flight with an autopilot it writes itself: code that watches the screen's
#      pixels and presses Space with real input, without touching the game.
#      game-watch.ps1 reads the game's state through UI Automation: it left the
#      ready screen, flights ended, how long each lasted and what it scored, and
#      every score the agent reports must be one it saw. The game's files must be
#      unchanged, and no developer tools may open.
#
# Sections 1, 2 and 3's first turn also read the worker's diagnostics: the turn
# used the `computer` tool and no shell or Python. On 0.7.2 and 0.7.3 there is no
# `computer` tool (the Python skill typed through the input method: `C:/Users`
# arrived as `C:/U Ú Ué ẻ é`), so those checks fail there.
#
# It takes over the screen, mouse and keyboard for up to an hour: leave the
# desktop alone while it runs. Set GAME_DIR to the game's folder if it is not in
# Downloads. The agent runs full-auto against real credentials.
source "$(dirname "$0")/env.sh"
H="$(cd "$(dirname "$0")/computer" && (pwd -W 2>/dev/null || pwd))"
GAME_DIR=${GAME_DIR:-$USERPROFILE/Downloads/flappy-bird}
PS=(powershell -NoProfile -ExecutionPolicy Bypass -File)
export HA_PYTHON=${HA_PYTHON:-$PY} HA_AUTO_REFINE=off HA_COMPUTER_USE=1

fresh computer
D="$HA_HOME/data/diagnostics"
project "$RX/p"
# The windows and tabs open before the test: cleanup closes only what came after.
"${PS[@]}" "$H/cleanup.ps1" -Snapshot "$(cygpath -w "$RX/before.txt" 2>/dev/null || echo "$RX/before.txt")"

cleanup() {
  [ -n "${SERVER:-}" ] && kill "$SERVER" 2>/dev/null
  [ -n "${WATCH:-}" ] && : > "$RX/game.jsonl.stop"
  "${PS[@]}" "$H/cleanup.ps1" -Before "$(cygpath -w "$RX/before.txt" 2>/dev/null || echo "$RX/before.txt")" -Tabs 'Skyline Flap*|ha browser check*' -Folders '*flappy-bird*|*Downloads*|*This PC*' -Windows 'ha live check' 2>/dev/null | sed 's/^/   cleanup: /'
  "$HA" stop cu >/dev/null 2>&1
}
trap cleanup EXIT

# The tools a turn called, from the worker's diagnostics: `name ok|error` per
# line, for calls from <since> ms (and before <until>, when given).
tools_since() {
  cat "$D"/*-worker-*.jsonl 2>/dev/null | "$PY" -c "
import json, sys
since = int(sys.argv[1])
until = int(sys.argv[2]) if len(sys.argv) > 2 and sys.argv[2] else None
for line in sys.stdin:
    try:
        record = json.loads(line)
    except ValueError:
        continue
    ms = record.get('ms', 0)
    if record.get('kind') == 'tool' and ms >= since and (until is None or ms < until):
        print(record.get('fields', {}).get('tool'), record.get('event'))
" "$1" "${2:-}"
}
now_ms() { "$PY" -c "import time; print(int(time.time() * 1000))"; }
# used <since> <tool> [until]: how many calls of <tool> in that time.
used() { tools_since "$1" "${3:-}" | awk -v t="$2" '$1 == t' | wc -l; }
# Calls that run commands: the desktop must be driven through the screen.
shell_calls() { tools_since "$1" "${2:-}" | grep -cE '^(run_shell|bash|python_repl|ipython|run_process|delegate) '; }

agent "$RX/p" cu
sleep 5
"$HA" list --json | grep -q '"name": "cu"' || { sleep 20; "$HA" list --json | grep -q '"name": "cu"'; } || {
  # The worker can take longer than the probe's wait to start in a fresh home.
  id=$("$HA" list --json | "$PY" -c "import json,sys; d=json.load(sys.stdin); print([x['id'] for x in d['agents'] if not x.get('name')][0])")
  "$HA" rename "$id" cu >/dev/null
}
NO_SHELL="Use only the computer tool for this: no shell, no Python, no Run dialog, no other tool."

echo "== 1. the screen, a click, typing and a key"
# Letters and digits no font confuses (no 0/O, 1/I/L, 5/S, 8/B, 2/Z).
CODE=$("$PY" -c "import secrets; a = 'ACDEFGHJKMNPQRTUVWXY34679'; print(''.join(secrets.choice(a) for _ in range(3)) + '-' + ''.join(secrets.choice(a) for _ in range(4)))")
printf '%s' "$CODE" > "$RX/code.txt"
PHRASE='Xin chào, ha! C:/Users/test 0.7.4'
"${PS[@]}" "$H/probe-window.ps1" -CodeFile "$(cygpath -w "$RX/code.txt" 2>/dev/null || echo "$RX/code.txt")" -Out "$(cygpath -w "$RX" 2>/dev/null || echo "$RX")" &
PROBE_WINDOW=$!
for _ in $(seq 1 30); do [ -s "$RX/window.txt" ] && break; sleep 1; done
T1=$(now_ms)
say cu "$NO_SHELL On this Windows desktop there is a window titled \"ha live check\" with a line \"CODE: ...\". Take a screenshot and tell me the code exactly as shown. Then click into that window's text box, type exactly the text between these double quotes, without the quotes: \"$PHRASE\" Then press Enter. Report the code in the form CODE=<code>." > /dev/null
A1=$(last)
echo "$A1" | tail -5 | sed 's/^/   /'
TYPED=$(cat "$RX/typed.txt" 2>/dev/null)
echo "   typed: $TYPED"
echo "   keys: $(cat "$RX/keys.txt" 2>/dev/null)"
echo "   code shown: $CODE"
check "the code it reported is the one only the screen showed" 'echo "$A1" | grep -qF "$CODE"'
check "the text box holds the text exactly, Vietnamese and the path included" '[ "$TYPED" = "$PHRASE" ]'
check "Enter reached the text box after the text" 'grep -qxF "ENTER $PHRASE" "$RX/keys.txt"'
check "the turn used the computer tool" '[ "$(used "$T1" computer)" -ge 2 ]'
check "and no shell or Python" '[ "$(shell_calls "$T1")" -eq 0 ]'
kill "$PROBE_WINDOW" 2>/dev/null
"${PS[@]}" "$H/cleanup.ps1" -Before "$(cygpath -w "$RX/before.txt" 2>/dev/null || echo "$RX/before.txt")" -Windows 'ha live check' > /dev/null 2>&1

echo "== 2. the browser"
PORT=$("$PY" -c "import socket; s = socket.socket(); s.bind(('127.0.0.1', 0)); print(s.getsockname()[1])")
"$PY" "$H/form_server.py" "$PORT" "$RX/form.log" &
SERVER=$!
sleep 1
NAME='Nguyễn Văn Bình'
T2=$(now_ms)
say cu "$NO_SHELL Open Microsoft Edge on this desktop, go to http://127.0.0.1:$PORT/form , type $NAME in the Name field, choose green as the Colour, and press Submit. Tell me the receipt number the page shows after it is submitted, as RECEIPT=<number>. Then close that browser tab with ctrl+w." > /dev/null
A2=$(last)
echo "$A2" | tail -5 | sed 's/^/   /'
sed 's/^/   form.log: /' "$RX/form.log" 2>/dev/null
POSTED=$("$PY" -c "
import json, sys
rows = [json.loads(line) for line in open(sys.argv[1], encoding='utf-8') if line.strip()]
posts = [row for row in rows if row['request'].startswith('POST')]
print(json.dumps(posts[-1], ensure_ascii=False) if posts else '{}')
" "$RX/form.log" 2>/dev/null)
field() { "$PY" -c "import json, sys; print(json.loads(sys.argv[1]).get(sys.argv[2], ''))" "$POSTED" "$1"; }
check "Edge opened the form" 'grep -q "\"GET /form\".*Edg/" "$RX/form.log"'
check "the form arrived with the Vietnamese name exactly" '[ "$(field name)" = "$NAME" ]'
check "and green chosen" '[ "$(field colour)" = "green" ]'
check "the receipt it reported is the one the server made" '[ -n "$(field receipt)" ] && echo "$A2" | grep -qF "$(field receipt)"'
check "the turn used the computer tool, no shell or Python" '[ "$(used "$T2" computer)" -ge 3 ] && [ "$(shell_calls "$T2")" -eq 0 ]'

echo "== 3. Flappy Bird: opened from File Explorer, flown by hand, then by an autopilot to 20"
if [ ! -f "$GAME_DIR/index.html" ]; then
  echo "FAIL  the game is not at $GAME_DIR (set GAME_DIR)"; fail=$((fail + 1))
else
  game_files() { (cd "$GAME_DIR" && find . -type f -print0 | sort -z | xargs -0 sha256sum); }
  game_files > "$RX/game-files.before"
  "${PS[@]}" "$H/cleanup.ps1" -Before "$(cygpath -w "$RX/before.txt" 2>/dev/null || echo "$RX/before.txt")" -Tabs 'Skyline Flap*' > /dev/null 2>&1
  : > "$RX/game.jsonl"
  "${PS[@]}" "$H/game-watch.ps1" -Out "$(cygpath -w "$RX/game.jsonl" 2>/dev/null || echo "$RX/game.jsonl")" -Seconds 5400 &
  WATCH=$!
  # 3a. The model itself: open the game through the desktop and fly with its own
  # key presses. Each look takes it far longer than the bird takes to fall, so it
  # flies blind between looks; a flight kept up past the unflapped fall is its doing.
  T3=$(now_ms)
  say cu "$NO_SHELL Open a new File Explorer window (Win+E), go to This PC, then Downloads, then the flappy-bird folder, and open index.html (it opens in the browser). Then fly two flights yourself with the computer tool: start a flight and press Space to flap. The bird falls while you look and think, so send each flight's flaps as one computer call, key presses with waits between them, filmed (the computer tool's film option, on the game canvas) so you see what the bird did, and with the window option set to the game's title so no key goes to another window. Each Space sets the bird's upward speed, and between presses it falls faster and faster: use the first film to find the spacing that keeps it level. After a crash, R or the restart button starts the next flight. Tell me the score each flight ended with, as shown on the screen, as SCORES=<a>,<b>. Leave the game open." > /dev/null
  A3=$(last)
  echo "$A3" | tail -4 | sed 's/^/   /'
  # 3b. Twenty gates need a reflex no model call has: the agent builds one. It
  # may write code, but only to watch the screen and press keys, as a person
  # writing a macro would. The game's own score, read by the watcher, is the judge.
  T3B=$(now_ms)
  say cu "Now get a score of at least 20 in a single flight of this game. You cannot react fast enough by looking and deciding call by call (the bird hits the ground about a second after a flap), so do what a person would: write a small autopilot (you have python_repl, with Pillow and pyautogui) that watches the game on the screen, from the pixels of the canvas you located in your screenshots, and presses Space with real keyboard input when the bird should flap to pass through the next gap. Rules: do not change the game in any way: no editing its files, no developer tools, no JavaScript or URL tricks in the page, no browser automation protocol, no changing the score or the stored best. Use the computer tool to look at the game and to start flights. Keep improving the autopilot until a flight ends with 20 or more on the FLIGHT ENDED panel, then stop the autopilot and leave the game open. Tell me that flight's score as BEST=<n>." > /dev/null
  A3B=$(last)
  echo "$A3B" | tail -4 | sed 's/^/   /'
  : > "$RX/game.jsonl.stop"
  wait "$WATCH" 2>/dev/null
  WATCH=
  game_files > "$RX/game-files.after"
  # Flights as the watcher saw them: `flight <start ms> <seconds> <final score>`,
  # from the first look running to the first look ended.
  "$PY" - "$RX/game.jsonl" > "$RX/flights.txt" <<'EOF'
import json, sys
looks = []
for line in open(sys.argv[1], encoding="utf-8-sig"):
    line = line.strip()
    if line:
        try:
            looks.append(json.loads(line))
        except ValueError:
            pass
flights, start = [], None
for look in looks:
    if not look.get("game"):
        continue
    running = not look.get("ready") and not look.get("ended")
    if running and start is None:
        start = look["t"]
    elif look.get("ended") and start is not None:
        flights.append((start, (look["t"] - start) / 1000, look.get("final") or ""))
        start = None
def explorers(look):
    found = look.get("explorer") or []
    return set([found] if isinstance(found, str) else found)
# Only what appeared during the turn counts: a window or a game tab the person
# already had open proves nothing about the agent.
before = explorers(looks[0]) if looks else set()
new = {window for look in looks for window in explorers(look)} - before
def score(text):
    return int(text) if str(text or "").isdigit() else 0
print("explorer", int(any(("flappy-bird" in window or "Downloads" in window) for window in new)))
print("game at start", int(bool(looks) and bool(looks[0].get("game"))))
print("opened", int(any(look.get("game") for look in looks[1:])))
print("ready", int(any(look.get("game") and look.get("ready") for look in looks)))
print("devtools", int(any(look.get("devtools") for look in looks)))
print("readout max", max([score(look.get("score")) for look in looks] + [0]))
for flight in flights:
    print("flight", flight[0], flight[1], flight[2])
EOF
  grep -v '^flight' "$RX/flights.txt" | sed 's/^/   watch: /'
  hand=$(awk -v t="$T3B" '/^flight/ && $2 < t' "$RX/flights.txt")
  auto=$(awk -v t="$T3B" '/^flight/ && $2 >= t' "$RX/flights.txt")
  echo "   by hand: $(echo "$hand" | awk 'NF { printf "%.1fs=%s ", $3, $4 }')"
  echo "   autopilot: $(echo "$auto" | grep -c .) flights, scores $(echo "$auto" | awk 'NF { printf "%s ", $4 + 0 }')"
  check "a File Explorer window it opened went to Downloads or flappy-bird" 'grep -qx "explorer 1" "$RX/flights.txt"'
  check "the game was not on screen before, then opened on its ready screen" 'grep -qx "game at start 0" "$RX/flights.txt" && grep -qx "opened 1" "$RX/flights.txt" && grep -qx "ready 1" "$RX/flights.txt"'
  check "by hand: it started flights and they ended (at least 2)" '[ "$(echo "$hand" | grep -c .)" -ge 2 ]'
  # Unflapped, a flight lasts about 1.2 s (the start's own flap, then the fall):
  # a longer one was kept up by the model's own flaps.
  check "by hand: a flight lasted over 2 s, kept up by its flaps" 'echo "$hand" | awk "\$3 > 2.0 { found = 1 } END { exit !found }"'
  # Every score the agent reported is one a flight ended with, as the watcher saw it.
  scores_seen() {
    local reported score
    reported=$(echo "$A3" | grep -oE "SCORES=[0-9, ]+" | tail -1 | sed "s/SCORES=//; s/ //g")
    [ -n "$reported" ] || return 1
    for score in ${reported//,/ }; do
      echo "$hand" | awk -v s="$score" 'NF { if ($4 + 0 == s + 0) found = 1 } END { exit !found }' || return 1
    done
  }
  check "by hand: every score it reported is one a flight ended with" scores_seen
  check "by hand: only the computer tool, no shell or Python" '[ "$(used "$T3" computer "$T3B")" -ge 5 ] && [ "$(shell_calls "$T3" "$T3B")" -eq 0 ]'
  best=$(echo "$auto" | awk 'NF { if ($4 + 0 > b) b = $4 + 0 } END { print b + 0 }')
  echo "   autopilot best: $best; the score readout's highest: $(awk '/^readout max/ { print $3 }' "$RX/flights.txt")"
  check "autopilot: a flight ended with 20 or more, on the game's own panel" '[ "$best" -ge 20 ]'
  check "autopilot: the score it reported is one a flight ended with" 'reported=$(echo "$A3B" | grep -oE "BEST=[0-9]+" | tail -1 | cut -d= -f2); [ -n "$reported" ] && echo "$auto" | awk -v s="$reported" "NF { if (\$4 + 0 == s + 0) found = 1 } END { exit !found }"'
  check "the game's files are unchanged" 'cmp -s "$RX/game-files.before" "$RX/game-files.after"'
  check "no developer tools window was opened" 'grep -qx "devtools 0" "$RX/flights.txt"'
  check "autopilot: the agent still looked with the computer tool" '[ "$(used "$T3B" computer)" -ge 2 ]'
fi

# Evidence: the screenshots the tool kept, the game's looks, the form, the diagnostics.
mkdir -p "$RX/shots"
cp "$HA_HOME"/data/computer-use/*/*.jpg "$RX/shots/" 2>/dev/null
echo "   evidence: $RX ($(ls "$RX/shots" | wc -l) screenshots)"
echo "   computer calls: $(used 0 computer), all tools: $(tools_since 0 | awk '{print $1}' | sort | uniq -c | tr '\n' ' ')"
result
