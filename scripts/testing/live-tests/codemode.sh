#!/usr/bin/env bash
# Codemode (docs/architecture/CODEMODE.en.md), with a real model, in `ha exec`:
#
#   1. With HA_CODEMODE=on, a task over twelve files (sum the `weight:` lines,
#      name the heaviest file) done as one codemode program: the program made
#      the twelve reads (a `codemode/run` record), the answer is right, and no
#      request the model was sent holds a line only the files hold - their
#      contents stayed out of the context. The program stores the sum.
#   2. The conversation resumes in a new process (`ha exec --resume`) and a
#      second program loads the stored sum: the store survives.
#   3. In `--approval auto-edit` a program's `run_process` call still meets
#      the approval gate: nobody can answer in `ha exec`, so it is refused and
#      the program sees the refusal.
#
# A precondition the model must meet (it used codemode the way the check
# needs) is retried up to three times and named when it never held.
# CONTROL=<an older ha.exe> runs check 1 on that binary as well: 0.9.10 has
# no codemode, so HA_CODEMODE=on offers no codemode tool and runs nothing.
#
#   CONTROL=/path/to/ha-0.9.10.exe bash scripts/testing/live-tests/codemode.sh
set -u
source "$(dirname "$0")/env.sh"
fresh codemode
export HA_DAEMON=off HA_AUTO_REFINE=off HA_SELF_EVAL=off HA_SESSION_LOG=off HA_REPL=off
D="$HA_HOME/data"

# A path as Windows programs read it (Git Bash's /f/... is not one).
winpath() { cygpath -m "$1" 2>/dev/null || printf '%s' "$1"; }
# Python's output without the `\r` Windows ends its lines with.
py() { "$PY" "$@" | tr -d '\r'; }
now_ms() { py -c "import time; print(int(time.time()*1000))"; }
# Records of one kind and event since a time, one JSON per line, from the
# data directory DD (default: this test's home).
records() {
  cat "${DD:-$D}"/diagnostics/*.jsonl 2>/dev/null | py -c "
import json, sys
kind, event, since = sys.argv[1], sys.argv[2], int(sys.argv[3])
for line in sys.stdin:
    try:
        r = json.loads(line)
    except ValueError:
        continue
    if r.get('kind') == kind and r.get('event') == event and r.get('ms', 0) >= since:
        print(json.dumps(r, ensure_ascii=False))
" "$1" "$2" "$3"
}
count() { records "$@" | wc -l | tr -d ' '; }
# The largest value of a field over the records: `most codemode run <since> calls`.
most() {
  records "$1" "$2" "$3" | py -c "
import json, sys
values = [json.loads(l)['fields'].get(sys.argv[1]) for l in sys.stdin if l.strip()]
values = [v for v in values if isinstance(v, (int, float))]
print(max(values) if values else 0)" "$4"
}
# A field of a run's JSON by a dotted path.
jget() {
  py -c "
import json, sys
try:
    v = json.load(open(sys.argv[1], encoding='utf-8'))
    for k in sys.argv[2].split('.'):
        v = v.get(k) if isinstance(v, dict) else None
except Exception:
    v = None
print('' if v is None else v)" "$(winpath "$1")" "$2"
}
# One `ha exec` run in <dir>: `run <dir> <name> <prompt> [ha exec options...]`.
run() {
  local dir="$1" name="$2" prompt="$3"; shift 3
  printf '\n## %s (ha exec %s)\n\n> %s\n\n' "$(date +%H:%M:%S)" "$name" "$prompt" >> "$LOG"
  (cd "$dir" && "${HAX:-$HA}" exec --model "$MODEL" "${TH[@]}" --output-format json "$@" "$prompt" \
    > "$RX/$name.json" 2> "$RX/$name.err")
  jget "$RX/$name.json" response >> "$LOG"
}
# How many times the requests the model was sent (in data directory <data>)
# hold <text>; each place goes to <evidence>: `sent <data> <text> <evidence>`.
sent() {
  py - "$(winpath "$1")" "$2" "$(winpath "$3")" <<'PY'
import glob, os, sqlite3, sys
data, text, evidence = sys.argv[1], sys.argv[2], sys.argv[3]
hits = 0
with open(evidence, "w", encoding="utf-8") as out:
    for db in glob.glob(os.path.join(data, "projects", "*", "harness.sqlite3")):
        connection = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
        for rowid, request in connection.execute("select rowid, request_json from frozen_requests"):
            at = request.find(text)
            while at >= 0:
                hits += 1
                out.write(f"{db} request {rowid}: ...{request[max(0, at - 600):at + 120]}...\n\n")
                at = request.find(text, at + 1)
print(hits)
PY
}

# The fixture: twelve notes of filler with CRLF line ends, as Python writes
# text on Windows, each with one weight and one line only it holds.
P1="$RX/p1"
project "$P1"
mkdir -p "$P1/notes"
py - "$(winpath "$P1/notes")" <<'PY'
import random, sys
random.seed(20261116)
words = "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda".split()
for index in range(1, 13):
    weight = random.randint(10, 99)
    lines = [" ".join(random.choice(words) for _ in range(12)) for _ in range(60)]
    lines.insert(random.randint(5, 50), f"weight: {weight}")
    lines.insert(random.randint(5, 50), f"MARK-{index:02d}-{random.randint(100000, 999999)} only this file holds this line")
    open(f"{sys.argv[1]}/n{index:02d}.txt", "w", encoding="utf-8", newline="\r\n").write("\n".join(lines) + "\n")
PY
(cd "$P1" && git add -A && git -c user.name=t -c user.email=t@t commit -qm notes)
read -r SUM HEAVIEST < <(py - "$(winpath "$P1/notes")" <<'PY'
import glob, os, re, sys
weights = {}
for path in sorted(glob.glob(os.path.join(sys.argv[1], "*.txt"))):
    weights[os.path.basename(path)] = int(re.search(r"^weight: (\d+)\r?$", open(path, encoding="utf-8", newline="").read(), re.M).group(1))
print(sum(weights.values()), max(weights, key=weights.get))
PY
)
echo "   fixture: sum of weights $SUM, heaviest $HEAVIEST"
# The prompt names no file by its path: ha attaches a file a prompt names to
# the first request, which would put a file's text in the context before any
# program ran.
TASK1="Use the codemode tool: write one program that reads each of the twelve files in the notes folder (named n, then a two-digit number from 01 to 12, then .txt) with read_file, finds the line 'weight: <number>' in each, stores the sum with store('weights', sum), and prints the sum of the weights and the name of the file with the largest weight. Do not read the files with your own read_file calls; one codemode program does all of it. Then tell me the sum and the file."

# --- 1. One program reads twelve files; only its output reaches the model.
held=0
for attempt in 1 2 3; do
  since=$(now_ms)
  HA_CODEMODE=on run "$P1" "task1-$attempt" "$TASK1" --approval full-auto
  if [ "$(most codemode run "$since" calls)" -ge 12 ]; then held=1; T1="$RX/task1-$attempt.json"; S1=$since; break; fi
done
check "precondition: the model ran a codemode program that made the twelve reads (attempts: $attempt)" '[ "$held" = 1 ]'
if [ "$held" = 1 ]; then
  response=$(jget "$T1" response)
  check "the answer is right: the sum $SUM and the heaviest file $HEAVIEST" 'printf "%s" "$response" | grep -q "$SUM" && printf "%s" "$response" | grep -q "${HEAVIEST%.txt}"'
  check "the program's calls all succeeded, and it ended without an error" '[ "$(records codemode run "$S1" | py -c "import json,sys; r=[json.loads(l)[\"fields\"] for l in sys.stdin if l.strip()]; print(any(f.get(\"calls\",0)>=12 and f.get(\"failed_calls\",1)==0 and \"error\" not in f for f in r))")" = True ]'
  marks=$(sent "$D" "MARK-" "$RX/marks-codemode.txt")
  check "the files' contents stayed out of the context: no request holds a line only a file holds ($marks found; $RX/marks-codemode.txt)" '[ "$marks" = 0 ]'
  check "the model made fewer of its own tool calls than the program made reads ($(jget "$T1" tool_calls) against 12)" '[ "$(jget "$T1" tool_calls)" -lt 12 ]'

  # --- 2. A new process resumes the conversation; the store survives.
  session=$(jget "$T1" session_id)
  since=$(now_ms)
  HA_CODEMODE=on run "$P1" resume "Use the codemode tool again: one program that prints load('weights'). Tell me exactly what it printed." --approval full-auto --resume "$session"
  ran=$(count codemode run "$since")
  check "precondition: the resumed conversation ran a codemode program ($ran)" '[ "$ran" -ge 1 ]'
  check "the second program, in a new process, loaded the sum the first one stored ($SUM)" '[ "$ran" -ge 1 ] && jget "$RX/resume.json" response | grep -q "$SUM"'
  check "the store is on disk under the data directory" 'grep -rqs "\"weights\"" "$D/codemode"'
fi

# --- 3. A program's command still meets the approval gate.
P3="$RX/p3"
project "$P3"
TASK3="Use the codemode tool: one program that calls tools.run_process({ executable: 'git', args: ['status', '--short'] }) inside try/catch, and prints either its output or 'refused: ' followed by the error message. Then tell me what it printed."
held=0
for attempt in 1 2 3; do
  since=$(now_ms)
  HA_CODEMODE=on run "$P3" "gate-$attempt" "$TASK3" --approval auto-edit
  if [ "$(most codemode run "$since" calls)" -ge 1 ]; then held=1; S3=$since; T3="$RX/gate-$attempt.json"; break; fi
done
check "precondition: a codemode program called run_process (attempts: $attempt)" '[ "$held" = 1 ]'
if [ "$held" = 1 ]; then
  check "in auto-edit the program's command was refused by the gate (failed_calls >= 1)" '[ "$(most codemode run "$S3" failed_calls)" -ge 1 ]'
  check "the program saw the refusal and the model reported it" 'jget "$T3" response | grep -qi "refus\|denied\|approv\|not run"'
fi

# --- Control: an older ha with HA_CODEMODE=on has no codemode. It runs in a
# home of its own, so its records and requests are its own.
if [ -n "${CONTROL:-}" ]; then
  PC="$RX/pc"
  CH="$RX/control-home"
  rm -rf "$PC" "$CH"; cp -r "$P1" "$PC"; mkdir -p "$CH"
  cp "$HA_HOME"/config.toml "$CH/" 2>/dev/null
  cp "$HA_HOME"/settings.json "$CH/" 2>/dev/null
  since=$(now_ms)
  HA_HOME="$CH" HAX="$CONTROL" HA_CODEMODE=on run "$PC" control "$TASK1" --approval full-auto
  check "control ($CONTROL): no codemode program ran" '[ "$(DD="$CH/data" count codemode run "$since")" = 0 ] && [ -s "$RX/control.json" ]'
  echo "   control: answered \"$(jget "$RX/control.json" response | head -c 200)\""
  echo "   control: the model's requests held $(sent "$CH/data" "MARK-" "$RX/marks-control.txt") lines only a file holds"
fi

result
