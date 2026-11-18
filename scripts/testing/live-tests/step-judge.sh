#!/usr/bin/env bash
# Each step of a finished turn judged in hindsight (after arXiv 2611.17167;
# docs/architecture/COST_AND_SKILLS.en.md §6), with a real model in `ha exec`:
#
#   1. A turn of six reads or more that ends with an answer is judged: one
#      line in step-judgments.jsonl covers every action once with a score in
#      [0, 1], the run's JSON says `step_judge.judged` with as many actions as
#      the turn made calls, and the judge's call is counted in `upkeep_usage`.
#   2. The reads the answer needed are judged to have helped (their mean is
#      high), and a read whose text the answer never used, when the model made
#      one, is scored below them.
#   3. A turn of five calls or fewer is not judged (`too_few_actions`), and
#      `HA_STEP_JUDGE=off` judges nothing (`off`) and makes no upkeep call.
#   4. `ha learning report` gives the step judgments of the last thirty days.
#
# The review is off (HA_AUTO_REFINE=off), so the only upkeep call is the
# judge's. A precondition the model must meet (it made the calls) is retried
# up to three times. The control is this script with HA=<the previous
# release's ha.exe>: 0.9.11 judges no step, so 1, 2 and 4 fail there.
set -u
source "$(dirname "$0")/env.sh"
fresh step-judge
export HA_DAEMON=off HA_AUTO_REFINE=off HA_SELF_EVAL=off HA_SESSION_LOG=off HA_REPL=off
export HA_CODEMODE=off HA_ACTION_BANK=off
D=$HA_HOME/data
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
run() {
  local name="$1" prompt="$2"; shift 2
  printf '\n## %s (ha exec %s)\n\n> %s\n\n' "$(date +%H:%M:%S)" "$name" "$prompt" >> "$LOG"
  (cd "$P" && "$HA" exec --model "$MODEL" "${TH[@]}" --output-format json --approval full-auto "$@" "$prompt" \
    > "$RX/$name.json" 2> "$RX/$name.err")
  jget "$RX/$name.json" response >> "$LOG"
}
# The judgment of the run whose session is <session>: `judged <session> <python expression over j>`.
judged() {
  py - "$(winpath "$D/harness/step-judgments.jsonl")" "$1" "$2" <<'PY'
import json, sys
path, session, expression = sys.argv[1], sys.argv[2], sys.argv[3]
lines = []
try:
    for line in open(path, encoding="utf-8"):
        try:
            lines.append(json.loads(line))
        except ValueError:
            pass
except OSError:
    pass
mine = [j for j in lines if session and session in json.dumps(j)]
j = mine[-1] if mine else (lines[-1] if lines else {})
try:
    value = eval(expression, {"j": j})
except Exception:
    value = None
print("" if value is None else value)
PY
}

P="$RX/p"
project "$P"
mkdir -p "$P/notes"
py - "$(winpath "$P")" <<'PY'
import random, sys
root = sys.argv[1]
random.seed(20261118)
words = "alpha beta gamma delta epsilon zeta eta theta".split()
for index in range(1, 7):
    lines = [" ".join(random.choice(words) for _ in range(8)) for _ in range(30)]
    lines.insert(random.randint(3, 25), f"weight: {random.randint(10, 99)}")
    open(f"{root}/notes/n{index}.txt", "w", encoding="utf-8").write("\n".join(lines) + "\n")
open(f"{root}/notes/history.txt", "w", encoding="utf-8").write(
    "A note on how this folder began, years ago. It holds no weight and nothing about the notes' contents.\n" * 3)
PY
(cd "$P" && git add -A && git -c user.name=t -c user.email=t@t commit -qm notes)
SUM=$(py - "$(winpath "$P/notes")" <<'PY'
import glob, os, re, sys
total = 0
for path in sorted(glob.glob(os.path.join(sys.argv[1], "n*.txt"))):
    total += int(re.search(r"^weight: (\d+)$", open(path, encoding="utf-8").read(), re.M).group(1))
print(total)
PY
)

# The prompts name no file by its path: ha attaches a named file to the first request.
TASK="The notes folder holds six numbered note files (n1 to n6, .txt) and one other file. Read each of the six numbered notes with its own read_file call, one file per call, find the line 'weight: <number>' in each, and tell me the sum of the six weights. You may also look at the other file in the folder if you like."
SHORT="Read the README of this project with read_file and tell me its first line."

# --- 1, 2. A turn of six reads or more is judged, every action once.
held=0
for attempt in 1 2 3; do
  run "judged-$attempt" "$TASK"
  calls=$(jget "$RX/judged-$attempt.json" tool_calls)
  if [ "${calls:-0}" -ge 6 ] 2>/dev/null; then held=1; R="$RX/judged-$attempt.json"; break; fi
done
check "precondition: the turn made six calls or more ($calls; attempts: $attempt)" '[ "$held" = 1 ]'
if [ "$held" = 1 ]; then
  session=$(jget "$R" task_id)
  check "the run's JSON says the turn was judged, with as many actions as calls ($(jget "$R" step_judge))" '[ "$(jget "$R" step_judge.judged)" = True ] && [ "$(jget "$R" step_judge.actions)" = "$calls" ]'
  check "step-judgments.jsonl holds the turn, every action once, each scored in [0, 1]" '[ "$(judged "$session" "sorted(s[\"i\"] for s in j[\"steps\"]) == list(range(1, j[\"actions\"] + 1)) and all(0 <= s[\"helped\"] <= 1 for s in j[\"steps\"]) and j.get(\"valid\", True) is not False")" = True ]'
  check "the judge's call is counted in upkeep_usage ($(jget "$R" upkeep_usage.model_calls) calls)" '[ "$(jget "$R" upkeep_usage.model_calls)" -ge 1 ] 2>/dev/null'
  check "the answer is right: the sum is $SUM" 'jget "$R" response | grep -q "$SUM"'
  reads_mean=$(judged "$session" "round(sum(s['helped'] for s in j['steps'] if 'n' in s.get('args','') and 'history' not in s.get('args','') and s['tool']=='read_file') / max(1, sum(1 for s in j['steps'] if 'n' in s.get('args','') and 'history' not in s.get('args','') and s['tool']=='read_file')), 2)")
  check "the reads the answer needed are judged to have helped (mean $reads_mean)" 'py -c "import sys; sys.exit(0 if float(sys.argv[1] or 0) >= 0.6 else 1)" "$reads_mean"'
  history=$(judged "$session" "[s['helped'] for s in j['steps'] if 'history' in s.get('args','')]")
  echo "   the read of the file the answer did not need: ${history:-[] (the model did not read it)}"
  if [ -n "$history" ] && [ "$history" != "[]" ]; then
    check "a read whose text the answer never used is scored below the reads it needed ($history < $reads_mean)" 'py -c "import sys, ast; sys.exit(0 if max(ast.literal_eval(sys.argv[1])) < float(sys.argv[2]) else 1)" "$history" "$reads_mean"'
  fi
fi

# --- 3. Too few calls, and the switch.
run short "$SHORT"
check "a turn of five calls or fewer is not judged ($(jget "$RX/short.json" step_judge))" '[ "$(jget "$RX/short.json" step_judge.reason)" = too_few_actions ]'
held=0
for attempt in 1 2 3; do
  export HA_STEP_JUDGE=off
  run "off-$attempt" "$TASK"
  unset HA_STEP_JUDGE
  calls=$(jget "$RX/off-$attempt.json" tool_calls)
  if [ "${calls:-0}" -ge 6 ] 2>/dev/null; then held=1; R="$RX/off-$attempt.json"; break; fi
done
check "precondition: with HA_STEP_JUDGE=off the turn made six calls or more ($calls)" '[ "$held" = 1 ]'
if [ "$held" = 1 ]; then
  check "HA_STEP_JUDGE=off judges nothing ($(jget "$R" step_judge)) and makes no upkeep call ($(jget "$R" upkeep_usage.model_calls))" '[ "$(jget "$R" step_judge.reason)" = off ] && [ "$(jget "$R" upkeep_usage.model_calls)" = 0 ]'
fi

# --- 4. The report.
(cd "$P" && "$HA" learning report > "$RX/report.txt" 2>&1)
check "ha learning report gives the step judgments ($RX/report.txt)" 'grep -qi "step" "$RX/report.txt" && grep -qiE "[1-9][0-9]* turns? judged" "$RX/report.txt"'

echo "   ha: $HA ($(sha256sum "$HA" | cut -c1-16))"
result
