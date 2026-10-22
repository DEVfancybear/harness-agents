#!/usr/bin/env bash
# What 0.6.1 records, with a real model: every process's diagnostics log,
# and `ha bug-report` gathering it. A headless run, a background agent, a
# compaction, a provider that refuses, two runs that want the same store and a
# worker killed under its supervisor each must leave records naming what
# happened; the bug report must name the problems and carry no credential.
# Every check prints PASS or FAIL; the evidence stays in $LIVE_ROOT/diag.
#
#   bash scripts/testing/live-tests/diag.sh
set -u
source "$(dirname "$0")/env.sh"
HERE=$(cd "$(dirname "$0")" && (pwd -W 2>/dev/null || pwd))
fresh diag
export HA_PYTHON=${HA_PYTHON:-$PY}
D="$HA_HOME/data/diagnostics"
P="$RX/p"
project "$P"
mkdir -p "$P/gen"
"$PY" - "$P/gen" <<'EOF'
import os, sys
for module in range(1, 5):
    lines = [f"m{module} row {row:04d} " + "alpha beta gamma delta epsilon " * 3 for row in range(300)]
    open(os.path.join(sys.argv[1], f"m{module}.txt"), "w", encoding="utf-8").write("\n".join(lines) + "\n")
EOF
(cd "$P" && git add -A && git -c user.name=t -c user.email=t@t commit -qm inputs)

# records <role> <python condition on r>: how many records of that role's files match.
records() { "$PY" - "$D" "$1" "$2" <<'EOF'
import glob, json, os, sys
d, role, condition = sys.argv[1], sys.argv[2], sys.argv[3]
n = 0
for path in glob.glob(os.path.join(d, f"*-{role}-*.jsonl")):
    for line in open(path, encoding="utf-8"):
        r = json.loads(line)
        f = r.get("fields", {})
        if eval(condition):
            n += 1
print(n)
EOF
}
screen() { "$PY" "$HERE/screen.py" "$1"; }

echo "== a headless run"
answer=$(cd "$P" && "$HA" exec --model "$MODEL" "${TH[@]}" --approval full-auto "What is 17 + 25? Answer with the number only." 2>"$RX/exec.err")
printf '%s\n' "$answer" > "$RX/exec.out"
check "the headless run answers" 'grep -q 42 "$RX/exec.out"'
check "it recorded its start and its exit" '[ "$(records exec "r[\"kind\"]==\"process\" and r[\"event\"] in (\"start\",\"exit\")")" -ge 2 ]'
check "it recorded the turn's start and end, with the conversation" '[ "$(records exec "r[\"kind\"]==\"turn\" and r.get(\"task\",\"\").startswith(\"task_\")")" -ge 2 ]'
check "the turn ended final" '[ "$(records exec "r[\"event\"]==\"end\" and f.get(\"stop\")==\"final\"")" -ge 1 ]'

echo "== a compaction"
(cd "$P" && HA_COMPACT_AT=12000 "$HA" exec --model "$MODEL" "${TH[@]}" --approval full-auto "Read gen/m1.txt, gen/m2.txt, gen/m3.txt and gen/m4.txt one at a time with read_file, whole. Then answer with the number of the last row of m4 only." >"$RX/compact.out" 2>"$RX/compact.err")
check "the compacting run answers" 'grep -q 299 "$RX/compact.out"'
check "a compaction was recorded with its numbers" '[ "$(records exec "r[\"kind\"]==\"compaction\" and (\"before\" in f or \"shortened\" in f or \"covered\" in f or \"after\" in f)")" -ge 1 ]'

echo "== a provider that refuses"
(cd "$P" && "$HA" exec --model "opencode-go/no-such-model-0601" --approval full-auto "hi" >"$RX/refused.out" 2>"$RX/refused.err")
refused=$?
check "the refused run fails" '[ "$refused" -ne 0 ]'
check "the refusal is recorded as an error or warning" '[ "$(records exec "r[\"level\"] in (\"error\",\"warn\") and r[\"kind\"] in (\"provider\",\"process\",\"turn\",\"error\")")" -ge 1 ]'

echo "== two runs that want the same store"
Q="$RX/q"
project "$Q"
(cd "$Q" && "$HA" exec --model "$MODEL" "${TH[@]}" --approval full-auto "Run the shell command: sleep 25 (with bash), then answer: done" >"$RX/holder.out" 2>"$RX/holder.err") &
holder=$!
for _ in $(seq 1 40); do
  ls "$HA_HOME"/data/projects/*/writer.owner.json >/dev/null 2>&1 && break
  sleep 0.5
done
cat "$HA_HOME"/data/projects/*/writer.owner.json > "$RX/owner.json" 2>/dev/null
(cd "$Q" && "$HA" exec --model "$MODEL" "${TH[@]}" --approval full-auto "Answer: second" >"$RX/second.out" 2>"$RX/second.err")
second=$?
wait "$holder"
check "the holder noted itself beside the lock" 'grep -q "\"role\":\"exec\"" "$RX/owner.json"'
check "the holder finished its run" 'grep -qi done "$RX/holder.out"'
check "the second run is refused, naming who holds the store" '[ "$second" -ne 0 ] && grep -q "held by exec pid" "$RX/second.err" "$RX/second.out"'
check "the refused open is recorded" '[ "$(records exec "r[\"kind\"]==\"store\" and r[\"event\"]==\"open_failed\" and \"held by exec pid\" in r.get(\"msg\",\"\")")" -ge 1 ]'
check "the note is gone once the holder ends" '! ls "$HA_HOME"/data/projects/*/writer.owner.json >/dev/null 2>&1'

echo "== a background agent"
agent "$P" a1
say a1 "What is the capital of France? Answer with one word." >/dev/null
check "the agent answers" 'last | grep -qi paris'
check "the worker recorded the agent with its conversation" '[ "$(records worker "r[\"kind\"]==\"agent\" and r[\"event\"]==\"started\" and r.get(\"task\",\"\").startswith(\"task_\") and r.get(\"agent\")")" -ge 1 ]'
check "the worker recorded the agent's turn ending, by agent" '[ "$(records worker "r[\"kind\"]==\"turn\" and r[\"event\"]==\"terminal\" and r.get(\"agent\") and f.get(\"steps\") is not None")" -ge 1 ]'

echo "== /bug-report in the terminal"
(cd "$P" && "$PROBE" "$RX/bugreport.bin" "$HA" "wait:4000,key:/bug-report,enter,wait:15000" -- attach a1 >/dev/null)
screen "$RX/bugreport.bin" > "$RX/bugreport.txt"
# The panel reads `Written  <path>` since 0.7.1, `Written: <path>` before.
check "/bug-report says where it wrote the zip" 'grep -qE "Written:? +[A-Za-z]:" "$RX/bugreport.txt"'
check "the zip is in the data directory" 'ls "$HA_HOME"/data/bug-reports/ha-bug-report-*.zip >/dev/null 2>&1'

echo "== a worker killed under its supervisor"
# The worker serving the agent: its diagnostics file is named after its pid.
worker_pid=$("$PY" - "$D" <<'EOF'
import glob, json, os, sys
starts = []
for path in glob.glob(os.path.join(sys.argv[1], "*-worker-*.jsonl")):
    for line in open(path, encoding="utf-8"):
        r = json.loads(line)
        if r["kind"] == "process" and r["event"] == "start":
            starts.append((r["ms"], r["pid"]))
print(max(starts)[1])
EOF
)
echo "killing worker $worker_pid"
taskkill /F /PID "$worker_pid" >/dev/null 2>&1 || kill -9 "$worker_pid"
for _ in $(seq 1 60); do
  [ "$(records supervisor "r[\"kind\"]==\"worker\" and r[\"event\"]==\"died\"")" -ge 1 ] && break
  sleep 0.5
done
check "the supervisor recorded how the worker died" '[ "$(records supervisor "r[\"kind\"]==\"worker\" and r[\"event\"]==\"died\" and f.get(\"cause\")")" -ge 1 ]'
sleep 3
say a1 "Answer with one word: ok" >/dev/null
check "the agent came back and answers" 'last | grep -qi ok'

echo "== ha bug-report"
"$HA" bug-report --since 1h --out "$RX/report.zip" --json > "$RX/report.json" 2>"$RX/report.err"
check "the report is written" '[ -s "$RX/report.zip" ]'
"$PY" - "$RX/report.zip" "$RX/report" <<'EOF'
import sys, zipfile
z = zipfile.ZipFile(sys.argv[1])
assert z.testzip() is None
z.extractall(sys.argv[2])
EOF
S="$RX/report/SUMMARY.md"
check "the zip opens and SUMMARY.md comes first" '[ -s "$S" ] && "$PY" -c "import zipfile,sys; sys.exit(zipfile.ZipFile(sys.argv[1]).namelist()[0] != \"SUMMARY.md\")" "$RX/report.zip"'
check "it names the worker that died" 'grep -q "worker.died" "$S"'
check "it names the store another run held" 'grep -q "store.open_failed" "$S"'
check "it names the refused provider" 'grep -Eqi "provider|no-such-model" "$S"'
check "its timeline has the turns and the compaction" 'grep -q "turn.start" "$RX/report/timeline.jsonl" && grep -q "compaction\." "$RX/report/timeline.jsonl"'
check "it holds the stores and the workers' logs" 'ls "$RX"/report/store/project-*.json >/dev/null && ls "$RX"/report/workers/*.log >/dev/null'
check "its summary has the turns and the model calls" 'grep -q "^## Turns" "$S" && grep -q "^## Model calls" "$S"'
# The tools table needs a tool call recorded: the turns here may answer without one.
check "its tools table is there when a tool call was recorded" '! grep -qh "\"kind\":\"tool\"" "$RX"/report/diagnostics/*.jsonl || grep -q "^## Tools" "$S"'
check "the model calls carry their tokens and duration" 'grep -q "\"event\":\"call\"" "$RX"/report/diagnostics/*.jsonl && grep -q "duration_ms" "$RX"/report/diagnostics/*.jsonl'
check "no record's time was redacted" '! grep -q "\"ms\":\"\[REDACTED" "$RX"/report/diagnostics/*.jsonl'
check "it holds no credential" '"$PY" - "$HA_CREDENTIALS_DIR" "$RX/report" <<'"'"'EOF'"'"'
import json, os, sys
values = set()
def walk(v):
    if isinstance(v, dict):
        for x in v.values(): walk(x)
    elif isinstance(v, list):
        for x in v: walk(x)
    elif isinstance(v, str) and len(v) >= 16:
        values.add(v)
for name in os.listdir(sys.argv[1]):
    try:
        walk(json.load(open(os.path.join(sys.argv[1], name), encoding="utf-8")))
    except Exception:
        pass
assert values, "no credential to look for"
for root, _, files in os.walk(sys.argv[2]):
    for f in files:
        text = open(os.path.join(root, f), encoding="utf-8", errors="replace").read()
        for v in values:
            if v in text:
                sys.exit(1)
EOF'
check "nothing from private/ is in it" '! find "$RX/report" -path "*private*" | grep -q .'
"$HA" diag timeline --since 1h --level warn > "$RX/timeline.txt" 2>&1
check "ha diag timeline prints the warnings" 'grep -q "worker.died\|open_failed" "$RX/timeline.txt"'

(cd "$P" && "$HA" shutdown --force >/dev/null 2>&1)
result
