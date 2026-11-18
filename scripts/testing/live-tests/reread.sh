#!/usr/bin/env bash
# Not sending twice what the model already holds, and what a run cost by part
# (after arXiv 2611.12122; docs/architecture/COST_AND_SKILLS.en.md), with a real
# model in `ha exec`, read from the requests the model was sent
# (`frozen_requests`, where each request stores only what it added):
#
#   1. A file read twice in one turn: its text reaches the model once (the
#      second read is a pointer to the first), and `reread.unchanged` counts it.
#   2. A file read, changed in one line, and read again: the lines that did not
#      change reach the model once (the second read is the changed lines only).
#   3. A command printing one line sixty times: the line reaches the model a
#      few times, not sixty, with the count, and the model still says sixty.
#   4. `HA_REREAD=off` sends the second read whole again.
#   5. Every run's JSON gives `cost_usd` by part (cache read, cache write,
#      uncached input, output) adding up to its total.
#
# A precondition the model must meet (it made the reads the check needs) is
# retried up to three times and named when it never held. Codemode and the
# action bank are off: a program's reads never reach the transcript at all.
# The control is this script with HA=<the previous release's ha.exe>: 0.9.11
# sends the second read whole and has no `cost_usd`, so 1-3 and 5 fail there.
set -u
source "$(dirname "$0")/env.sh"
fresh reread
export HA_DAEMON=off HA_AUTO_REFINE=off HA_SELF_EVAL=off HA_SESSION_LOG=off HA_REPL=off
export HA_CODEMODE=off HA_ACTION_BANK=off HA_STEP_JUDGE=off

winpath() { cygpath -m "$1" 2>/dev/null || printf '%s' "$1"; }
py() { "$PY" "$@" | tr -d '\r'; }
jget() {
  py -c "
import json, sys
try:
    v = json.load(open(sys.argv[1], encoding='utf-8'))
    for k in sys.argv[2].split('.'):
        v = v.get(k) if isinstance(v, dict) else None
except Exception:
    v = None
print('' if v is None else (json.dumps(v) if isinstance(v, (dict, list)) else v))" "$(winpath "$1")" "$2"
}
# One `ha exec` run with its own home: `run <home> <dir> <name> <prompt> [options...]`.
run() {
  local home="$1" dir="$2" name="$3" prompt="$4"; shift 4
  mkdir -p "$home"
  cp "$RX/home/config.toml" "$home/" 2>/dev/null
  cp "$RX/home/settings.json" "$home/" 2>/dev/null
  printf '\n## %s (ha exec %s)\n\n> %s\n\n' "$(date +%H:%M:%S)" "$name" "$prompt" >> "$LOG"
  (cd "$dir" && HA_HOME="$home" "$HA" exec --model "$MODEL" "${TH[@]}" --output-format json "$@" "$prompt" \
    > "$RX/$name.json" 2> "$RX/$name.err")
  jget "$RX/$name.json" response >> "$LOG"
}
# What the requests sent from <home> hold: `requests <home> <text> <evidence>`
# prints "<times text was sent> <read_file calls> <edit_file calls> <run_process calls>".
requests() {
  py - "$(winpath "$1/data")" "$2" "$(winpath "$3")" <<'PY'
import glob, json, os, sqlite3, sys
data, text, evidence = sys.argv[1], sys.argv[2], sys.argv[3]
hits, calls = 0, {"read_file": 0, "edit_file": 0, "run_process": 0}
def walk(value):
    if isinstance(value, dict):
        name = value.get("name")
        if name in calls and "arguments" in value:
            calls[name] += 1
        for item in value.values():
            walk(item)
    elif isinstance(value, list):
        for item in value:
            walk(item)
with open(evidence, "w", encoding="utf-8") as out:
    for db in glob.glob(os.path.join(data, "projects", "*", "harness.sqlite3")):
        connection = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
        for rowid, request in connection.execute("select rowid, request_json from frozen_requests"):
            walk(json.loads(request))
            at = request.find(text)
            while at >= 0:
                hits += 1
                out.write(f"{db} request {rowid}: ...{request[max(0, at - 300):at + 200]}...\n\n")
                at = request.find(text, at + 1)
print(hits, calls["read_file"], calls["edit_file"], calls["run_process"])
PY
}
# Whether a run's `cost_usd` has its four parts adding up to a total above 0.
cost_ok() {
  py - "$(winpath "$1")" <<'PY'
import json, sys
try:
    run = json.load(open(sys.argv[1], encoding="utf-8"))
    cost = run["cost_usd"]
    parts = [cost[k] for k in ("uncached_input", "cache_read", "cache_write", "output")]
    usage = run["usage"]["cost_usd"]
    ok = all(isinstance(p, (int, float)) and p >= 0 for p in parts) and cost["total"] > 0 \
        and abs(sum(parts) - cost["total"]) < 1e-5 and usage["total"] > 0
except Exception:
    ok = False
print("yes" if ok else "no")
PY
}

P="$RX/p"
project "$P"
mkdir -p "$P/notes"
TOKEN=$(py -c "import random; print(random.randint(100000, 999999))")
py - "$(winpath "$P/notes")" "$TOKEN" <<'PY'
import random, sys
random.seed(int(sys.argv[2]))
words = "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda".split()
lines = [" ".join(random.choice(words) for _ in range(10)) for _ in range(120)]
for at in (20, 45, 70, 95):
    lines[at] = f"weight: {random.randint(10, 99)}"
lines[9] = f"FILLER-{sys.argv[2]} a line only this file holds"
lines[104] = "status: draft"
open(f"{sys.argv[1]}/plan.txt", "w", encoding="utf-8", newline="\n").write("\n".join(lines) + "\n")
PY
(cd "$P" && git add -A && git -c user.name=t -c user.email=t@t commit -qm notes)
FILLER="FILLER-$TOKEN"

# The prompts name no file by its path: ha attaches a named file to the first
# request, which would put its text in the context before any read.
TASK1="There is one text file in the notes folder. Read it whole with read_file. Then call read_file on the same file a second time, the same way (do it even though it is redundant: it is part of the test). Then tell me how many lines start with 'weight:'."
TASK2="There is one text file in the notes folder. Read it whole with read_file. Then change its line 'status: draft' to 'status: final' with edit_file. Then read the whole file again with read_file to confirm the change, and tell me the line that now starts with 'status:'."
TASK3="Run exactly this with run_process: executable \"python\", args [\"-c\", \"for i in range(60): print('tick-$TOKEN')\"]. Then tell me how many lines it printed."

# --- 1. An unchanged re-read is sent once.
held=0
for attempt in 1 2 3; do
  H="$RX/h1-$attempt"
  run "$H" "$P" "read-twice-$attempt" "$TASK1" --approval full-auto
  read -r hits reads _ _ < <(requests "$H" "$FILLER" "$RX/evidence-1-$attempt.txt")
  if [ "$reads" -ge 2 ]; then held=1; R1="$RX/read-twice-$attempt.json"; break; fi
done
check "precondition: the model read the file twice in one turn ($reads read_file calls; attempts: $attempt)" '[ "$held" = 1 ]'
if [ "$held" = 1 ]; then
  check "the file's text reached the model once, not once per read ($hits; $RX/evidence-1-$attempt.txt)" '[ "$hits" = 1 ]'
  check "ha exec counts the pointer and the bytes it saved (reread: $(jget "$R1" reread))" '[ "$(jget "$R1" reread.unchanged)" -ge 1 ] 2>/dev/null && [ "$(jget "$R1" reread.saved_bytes)" -gt 1000 ] 2>/dev/null'
  check "the answer is right: 4 lines start with weight:" 'jget "$R1" response | grep -q "4\|four"'
  check "the run's cost is given by part, and the parts add up (cost_usd: $(jget "$R1" cost_usd))" '[ "$(cost_ok "$R1")" = yes ]'
fi

# --- 2. A re-read after a one-line change sends the changed lines.
cp -r "$P" "$RX/p2"
held=0
for attempt in 1 2 3; do
  (cd "$RX/p2" && git checkout -q -- . )
  H="$RX/h2-$attempt"
  run "$H" "$RX/p2" "read-edit-read-$attempt" "$TASK2" --approval full-auto
  read -r hits reads edits _ < <(requests "$H" "$FILLER" "$RX/evidence-2-$attempt.txt")
  if [ "$reads" -ge 2 ] && [ "$edits" -ge 1 ] && grep -q "status: final" "$RX/p2/notes/plan.txt"; then held=1; R2="$RX/read-edit-read-$attempt.json"; break; fi
done
check "precondition: the model read, changed and read the file again ($reads reads, $edits edits; attempts: $attempt)" '[ "$held" = 1 ]'
if [ "$held" = 1 ]; then
  check "the lines that did not change reached the model once ($hits; $RX/evidence-2-$attempt.txt)" '[ "$hits" = 1 ]'
  check "ha exec counts the changed-lines read (reread: $(jget "$R2" reread))" '[ "$(jget "$R2" reread.delta)" -ge 1 ] 2>/dev/null'
  check "the answer names the changed line" 'jget "$R2" response | grep -q "status: final"'
fi

# --- 3. A line printed sixty times reaches the model with its count.
held=0
for attempt in 1 2 3; do
  H="$RX/h3-$attempt"
  run "$H" "$P" "ticks-$attempt" "$TASK3" --approval full-auto
  read -r hits _ _ runs < <(requests "$H" "tick-$TOKEN" "$RX/evidence-3-$attempt.txt")
  if [ "$runs" -ge 1 ]; then held=1; R3="$RX/ticks-$attempt.json"; break; fi
done
check "precondition: the model ran the command ($runs run_process calls; attempts: $attempt)" '[ "$held" = 1 ]'
if [ "$held" = 1 ]; then
  check "the repeated line reached the model a few times, not sixty ($hits; $RX/evidence-3-$attempt.txt)" '[ "$hits" -le 4 ]'
  check "ha exec counts the collapsed output (reread: $(jget "$R3" reread))" '[ "$(jget "$R3" reread.collapsed)" -ge 1 ] 2>/dev/null'
  check "the model still says sixty lines" 'jget "$R3" response | grep -q "60\|sixty"'
fi

# --- 4. HA_REREAD=off sends the second read whole.
held=0
for attempt in 1 2 3; do
  H="$RX/h4-$attempt"
  export HA_REREAD=off
  run "$H" "$P" "off-$attempt" "$TASK1" --approval full-auto
  unset HA_REREAD
  read -r hits reads _ _ < <(requests "$H" "$FILLER" "$RX/evidence-4-$attempt.txt")
  if [ "$reads" -ge 2 ]; then held=1; R4="$RX/off-$attempt.json"; break; fi
done
check "precondition: with HA_REREAD=off the model read the file twice ($reads; attempts: $attempt)" '[ "$held" = 1 ]'
if [ "$held" = 1 ]; then
  check "with HA_REREAD=off the file's text reached the model once per read ($hits)" '[ "$hits" -ge 2 ]'
  check "with HA_REREAD=off nothing is counted as saved" '[ "$(jget "$R4" reread.unchanged)" = 0 ] && [ "$(jget "$R4" reread.saved_bytes)" = 0 ]'
fi

echo "   ha: $HA ($(sha256sum "$HA" | cut -c1-16))"
result
