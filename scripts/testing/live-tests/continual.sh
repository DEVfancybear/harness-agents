#!/usr/bin/env bash
# What survives a conversation, with a real model. After Continual-ARC (arXiv
# 2611.14505, `docs/architecture/CONTINUAL_LEARNING.en.md`): the same task
# comes back in a fresh conversation, and 0.9.7 kept almost nothing of it for
# the model to use. Six parts, each with a check that fails on 0.9.7:
#   P1. the digest says who sees an entry (global, project, local), and a
#       project fact the automatic review observed (a quote, `about: project`,
#       no scope) is kept for the project, not only for the conversation;
#   P2. the one entry that fits the request is in the digest whole (to 1,200
#       characters), where 0.9.7 cut every entry at 180;
#   P3. a goal that ends verified asks, in the result of goal_complete, what to
#       keep for the task's return;
#   P4. a learned skill's script that ran with exit 0 is recorded as tested
#       (`scripts` in the skill usage), and the recalled skill says when a
#       script ran successfully, to run it first, or that it has not run yet;
#   P5. a history_search of this conversation that finds nothing says when an
#       earlier conversation of the project holds the words (`earlier`);
#   P6. a child receives the text the user appended to the system prompt
#       (`--append-system-prompt`, or APPEND_SYSTEM.md in the config folder).
# Where the model must act, the turn asks for one exact call, and the checks
# judge the feature only when the session's own record shows the model made it
# (a "precondition:" check); a turn whose precondition did not hold is tried
# again, up to three times. A precondition that never held is a FAIL by its
# name, never a pass. What the model was sent is read from the frozen requests
# (`ev` below), the markers are the host's own strings, quoted in a comment
# from the Rust source that writes them.
# Every check prints PASS or FAIL; the conversation and the store stay in
# $LIVE_ROOT/continual as evidence. A comment above a check says what 0.9.7 does
# with it: "0.9.7 FAILS" is the feature, "passes on 0.9.7 too" a precondition or
# a control.
#
#   bash scripts/testing/live-tests/continual.sh
set -u
source "$(dirname "$0")/env.sh"
fresh continual
export HA_AUTO_REFINE=off HA_SELF_EVAL=off HA_SESSION_LOG=off HA_PYTHON=${HA_PYTHON:-$PY}
D=$HA_HOME/data
G=$D/harness
P=$RX/p
project "$P"
WD=$P

# One `ha exec` turn in $WD (REVIEW=1: with the automatic review, which runs
# before the process exits): `run_exec <name> [flags] <prompt>`.
run_exec() {
  local name="$1"; shift
  local pre=()
  if [ "${REVIEW:-}" = 1 ]; then pre=(env -u HA_AUTO_REFINE); fi
  printf '\n## %s (ha exec %s)\n\n> %s\n\n' "$(date +%H:%M:%S)" "$name" "${*: -1}" >> "$LOG"
  (cd "${WD:-$P}" && "${pre[@]}" "$HA" exec --model "$MODEL" "${TH[@]}" --approval full-auto --output-format json "$@" >"$RX/$name.json" 2>"$RX/$name.err")
  answer "$name" >> "$LOG"
  sed 's/^/    /' "$RX/$name.err" >> "$LOG"
}
answer() { "$PY" -c "import json,sys; print(json.load(open(sys.argv[1],encoding='utf-8')).get('response',''))" "$RX/$1.json" 2>/dev/null; }

# What the model was sent and what the tools answered, read from the frozen
# requests: `ev <marker> root|child <python expression>`. It looks at the
# conversations whose messages hold <marker> (a root's, or a delegated child's)
# and judges the expression, true or false (a root is the first conversation
# whose messages hold it). In the expression: `msgs` (every
# message of those conversations, once: a request is stored as a delta of the
# one before), `text` (all of them and the tool calls, joined), `results` (the
# tool results), `systems` (the system messages), `calls` ([(tool, arguments)]),
# `n` (how many conversations), `content(m)`, `re`, `json`, `os`.
cat > "$RX/ev.py" <<'PYEOF'
import glob, json, os, re, sqlite3, sys
sys.stdout.reconfigure(encoding="utf-8")
data, marker, kind = sys.argv[1:4]
expr = sys.argv[4] if len(sys.argv) > 4 else sys.stdin.read()
tasks, kids = {}, set()
for db in glob.glob(os.path.join(data, "projects", "*", "harness.sqlite3")):
    conn = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
    try:
        kids |= {task for (task,) in conn.execute("select task_id from session_settings where setting_key='delegated_child'")}
        rows = conn.execute("select task_id, request_json from frozen_requests order by rowid").fetchall()
    except sqlite3.Error:
        continue
    for task, raw in rows:
        tasks.setdefault(task, []).extend(json.loads(raw).get("messages", []))
def content(m):
    return m.get("content") or ""
def blob(m):
    return content(m) + " " + " ".join(str(c.get("arguments")) for c in m.get("tool_calls") or [])
picked = [t for t, ms in tasks.items() if ((t in kids) == (kind == "child")) and any(marker in blob(m) for m in ms)]
if kind != "child":
    # A later conversation may carry the marker too (an earlier state brief):
    # the root is the first that does.
    picked = picked[:1]
msgs = [m for t in picked for m in tasks[t]]
def arguments(call):
    try:
        return json.loads(call.get("arguments") or "{}")
    except ValueError:
        return {}
calls = [(c.get("name"), arguments(c)) for m in msgs for c in m.get("tool_calls") or []]
results = [content(m) for m in msgs if m.get("role") == "tool"]
systems = [content(m) for m in msgs if m.get("role") == "system"]
text = "\n".join([content(m) for m in msgs] + [f"{c.get('name')} {c.get('arguments')}" for m in msgs for c in m.get("tool_calls") or []])
n = len(picked)
sys.exit(0 if eval("(" + expr.strip() + ")") else 1)
PYEOF
ev() { "$PY" "$RX/ev.py" "$D" "$@"; }
# Whether the review kept a memory entry holding $1, and for whom: prints
# `global <project name>` (a global-store entry, tagged with its project),
# `global ` (untagged) or `local ` (this conversation's only), one per entry.
landed() {
  local dir scope
  for dir in "$G" "$D"/sessions/*/harness; do
    [ -d "$dir" ] || continue
    scope=local; [ "$dir" = "$G" ] && scope=global
    "$PY" "$MEMORY_STORE" document "$dir" 2>/dev/null | "$PY" -c "
import json, sys
scope, token = sys.argv[1], sys.argv[2]
for entry in json.load(sys.stdin).get('entries', {}).get('memory', {}).values():
    if token in str(entry.get('content', '')) and not entry.get('superseded_by'):
        project = entry.get('project')
        print(scope, project.get('name', '') if isinstance(project, dict) else '')
" "$scope" "$1" | tr -d '\r'
  done
}
# Trust a project in this home's config, so its [verify] checks load (as strategy.sh).
trust() {
  "$PY" - "$HA_HOME/config.toml" "$1" <<'PYEOF'
import re, sys, tomllib
path, project = sys.argv[1], sys.argv[2]
text = open(path, encoding="utf-8").read()
quoted = '"' + project.replace("\\", "\\\\") + '"'
if re.search(r"(?m)^\[trust\]", text):
    if re.search(r"(?ms)^\[trust\][^\[]*?^projects\s*=\s*\[", text):
        text = re.sub(r"(?ms)(^\[trust\][^\[]*?^projects\s*=\s*\[)", lambda m: m.group(1) + quoted + ", ", text, count=1)
    else:
        text = re.sub(r"(?m)^\[trust\]\s*$", "[trust]\nprojects = [" + quoted + "]", text, count=1)
else:
    text += "\n[trust]\nprojects = [" + quoted + "]\n"
tomllib.loads(text)
open(path, "w", encoding="utf-8").write(text)
PYEOF
}

echo "################ 1. P1 and P2: the digest says who sees an entry, and shows the entry that fits whole"
# A global memory of ten entries: nine notes about the weather and one task
# whose last step lies past the 180 characters 0.9.7 cut an entry at. It is a
# harness_state.json the store imports when ha first opens it.
"$PY" - "$G" <<'PYEOF'
import json, os, sys
directory = sys.argv[1]
os.makedirs(directory, exist_ok=True)
def entry(key, title, content, path):
    return {"id": key, "kind": "memory", "title": title, "content": content, "path": path,
            "scope": "global", "version": 1, "source": "agent", "updated_at": "2026-10-01T00:00:00Z"}
memory = {}
for number in range(9):
    memory[f"weather-{number}"] = entry(f"weather-{number}", f"Weather note {number}",
        f"Weather note {number}: the wind at station {number} turns north after noon.", "weather")
steps = "then check the berth codes and the currency column. " * 8
memory["harbor-ledger"] = entry("harbor-ledger", "Harbor ledger reconcile task",
    "Harbor ledger reconcile task: read the sample months first, " + steps +
    "Last step: run python tools/finalize_ledger_zk.py --apply and open the report it writes.", "ops")
json.dump({"schema": 1, "entries": {"memory": memory}, "refinements": []},
          open(os.path.join(directory, "harness_state.json"), "w", encoding="utf-8"))
PYEOF
run_exec t1 "MARK-P2A. Do the harbor ledger reconcile task again. Reply with the single word OK-WHOLE and call no tools."
echo "   $(answer t1 | head -c 200)"
# Passes on 0.9.7 too: the turn ran, and the store holds the entry whole.
check "precondition: the turn's request was found" 'ev MARK-P2A root "n == 1"'
check "precondition: the entry reached the store whole, its last step in it" '"$PY" "$MEMORY_STORE" rows "$G" memory | grep -q finalize_ledger_zk'
# The digest lists the entry at all (cut, it still starts with its title).
check "precondition: the digest lists the entry that fits the request" 'ev MARK-P2A root "\"Harbor ledger reconcile task: read the sample months first\" in text"'
# memory/harness.rs SCOPE_REACH. 0.9.7 FAILS: its header said to default to local.
E_REACH='"Who sees an entry you write: global, every conversation; project, this project" in text and "conversations; local, only this one - and it is gone when this conversation ends." in text and "What a later conversation of this task or project would need" in text'
check "P1: the digest says who sees an entry: global, project, local and that local is gone when the conversation ends" 'ev MARK-P2A root "$E_REACH"'
# 0.9.7 FAILS: the line "Default to local continual harness refinement ..." is the one P1 replaced.
check "P1: the digest no longer says to default to local" 'ev MARK-P2A root "\"Default to local continual harness refinement\" not in text"'
# memory/harness.rs whole_entry, WHOLE_CONTENT_LIMIT. 0.9.7 FAILS: the entry is cut at 180 characters.
check "P2: the entry that fits the request is shown whole: its last step is in the digest" 'ev MARK-P2A root "\"finalize_ledger_zk\" in text"'
run_exec t2 "MARK-P2B. Paint the kitchen wall. Reply with the single word OK-CUT and call no tools."
# Passes on 0.9.7 too (a control): a request the entry does not fit gets it cut, as before.
check "P2 control: a request that shares nothing with it gets the entry cut, not whole" 'ev MARK-P2B root "n == 1 and \"finalize_ledger_zk\" not in text"'

echo "################ 2. P1: a project fact the review observed is kept for the project"
# The automatic review of an `ha exec` turn (turnInterval 1 makes one due) is
# the model's judgement of what the turn showed; the host decides the scope
# (refine.rs `observed_project_fact`: a new memory entry, `about: project`, a
# quote, no scope, is written with scope project, tagged with its project).
"$PY" - "$HA_HOME/settings.json" <<'PYEOF'
import json, os, sys
path = sys.argv[1]
settings = json.load(open(path, encoding="utf-8")) if os.path.exists(path) else {}
settings["autoRefine"] = {"turnInterval": 1}
json.dump(settings, open(path, "w", encoding="utf-8"))
PYEOF
P1B=$RX/orion-sync
project "$P1B"
mkdir -p "$P1B/ops"
jobs=(ledger invoice tariff)
kept=""; keptat=""; forproject=""
for a in 1 2 3; do
  job=${jobs[$((a-1))]}; tok="inbox-4$a"
  printf '# Settings of the nightly %s sync, run %s\nschedule = nightly\ninput_dir = spool/%s\nretries = 3\n' "$job" "$a" "$tok" > "$P1B/ops/sync-$a.cfg"
  printf 'The nightly %s sync is configured in ops/sync-%s.cfg.\n' "$job" "$a" > "$P1B/ops/README-$a.txt"
  (cd "$P1B" && git add -A && git -c user.name=t -c user.email=t@t commit -qm "sync $a")
  WD=$P1B REVIEW=1 run_exec p1b$a "MARK-P1B-$a. In this project the nightly $job sync is set up somewhere under ops/. Find out which folder it reads its input from (use search_text and read_file only) and answer in one sentence."
  where=$(landed "$tok")
  echo "   attempt $a: the review kept the fact as: ${where:-nowhere}"
  [ -z "$where" ] && continue
  [ -z "$kept" ] && { kept=1; keptat=$a; }
  if printf '%s\n' "$where" | grep -qx "global orion-sync"; then forproject=$a; break; fi
done
# Passes on 0.9.7 too: the review ran and kept the fact somewhere. When it
# kept it nowhere in three turns, the model judged it not worth keeping and
# the feature was never reached.
check "precondition: the review kept the fact in some memory store (attempt ${keptat:-none})" '[ -n "$kept" ]'
# 0.9.7 FAILS: its review kept a fact it called about the project in the
# conversation's own store (or untagged), where the project's next conversation
# never saw it.
check "P1: a fact about the project the review observed is kept for the project (a global entry tagged orion-sync, attempt ${forproject:-none})" '[ -n "$forproject" ]'

echo "################ 3. P3: a goal that ends verified asks what to keep for the task's return"
# The check the goal's end must pass: a line in result.txt.
doneat=""
for a in 1 2 3; do
  tok="DONE-3$a"; Pg=$RX/p3-$a
  project "$Pg"
  mkdir -p "$Pg/.harness"
  cat > "$Pg/.harness/config.toml" <<EOF
schema_version = 2
[verify]
judge = false
[[verify.checks]]
name = "result"
command = "git grep --untracked -q $tok -- result.txt"
hint = "result.txt must hold $tok"
EOF
  trust "$(cd "$Pg" && (pwd -W 2>/dev/null || pwd))"
  agent "$Pg" "g$a"
  "$HA" send "g$a" "/goal Harness test, run unattended: do not ask the user anything. Write the single line $tok into result.txt with write_file, then call goal_complete with a one-line summary." >/dev/null 2>&1
  # goal_complete runs the project's check; its result is in the next request.
  for _ in $(seq 1 60); do
    ev "$tok" root 'any("The goal is complete and verified" in r for r in results)' && break
    sleep 10
  done
  "$HA" shutdown --force >/dev/null 2>&1
  if ev "$tok" root 'any("The goal is complete and verified" in r for r in results)'; then doneat=$a; break; fi
  echo "   attempt $a: the goal did not end verified"
done
GT="DONE-3${doneat:-1}"
check "precondition: a goal_complete was verified and its result reached the model (attempt ${doneat:-none})" '[ -n "$doneat" ]'
# goal.rs KeepAsk::paragraph. 0.9.7 FAILS: the result ended at "The goal is complete and verified: <summary>".
E_KEEP='any("The goal is complete and verified" in r and "If this task may come back, save now what would let you do it at once next time." in r for r in results)'
check "P3: the verified result asks what to keep if the task may come back" '[ -n "$doneat" ] && ev "$GT" root "$E_KEEP"'
E_KEEP2='any("The goal is complete and verified" in r and "memory tool, scope project" in r and "learned skill" in r and "Memory written with scope local is gone with this conversation." in r for r in results)'
check "P3: it names the memory tool at scope project, the steps for a learned skill, and that local memory is gone" '[ -n "$doneat" ] && ev "$GT" root "$E_KEEP2"'
E_ONCE='all(r.count("If this task may come back") <= 1 for r in results)'
check "P3: it is asked once in a result, not repeated" '[ -n "$doneat" ] && ev "$GT" root "$E_ONCE"'

echo "################ 4. P4: the recalled skill says which of its scripts ran, and to run the tested one first"
# A learned skill with three scripts: one a run vouched for (the mark holds the
# file's SHA-256), one never run, one whose mark is of another file's content.
S4=$HA_HOME/skills/harbor-tally
mkdir -p "$S4/scripts"
cat > "$S4/SKILL.md" <<'EOF'
---
name: harbor-tally
description: Use when asked to tally the harbor berth fees.
---
# Harbor tally

Run the script under scripts/ that totals the berth fees, then report the total.
EOF
echo '{"created_by": "ha", "created": "2026-10-01T00:00:00Z"}' > "$S4/.ha-learned.json"
printf 'print("berth fees: 4120")\n' > "$S4/scripts/solve.py"
printf 'print("draft")\n' > "$S4/scripts/draft.py"
printf 'print("older")\n' > "$S4/scripts/stale.py"
seed4() {
  "$PY" - "$S4" "$D/skill-usage/global.json" <<'PYEOF'
import hashlib, json, os, sys
skill, usage = sys.argv[1:3]
sha = lambda name: hashlib.sha256(open(os.path.join(skill, "scripts", name), "rb").read()).hexdigest()
marks = {"scripts/solve.py": {"at": "2026-11-10T09:00:00Z", "sha": sha("solve.py"), "evidence": "berth fees: 4120"},
         "scripts/stale.py": {"at": "2026-11-10T09:05:00Z", "sha": "0" * 64, "evidence": "older"}}
os.makedirs(os.path.dirname(usage), exist_ok=True)
json.dump({"skills": {"harbor-tally": {"uses": 1, "worked": 1, "trial_good": 1, "scripts": marks}}}, open(usage, "w", encoding="utf-8"))
PYEOF
}
seed4
P4=$RX/p4
project "$P4"
WD=$P4 run_exec t4 "MARK-P4A. Tally the harbor berth fees again. Reply with the single word OK-TALLY and call no tools."
# Passes on 0.9.7 too: the skill that fits was recalled for the turn.
check "precondition: the learned skill that fits the request was recalled" 'ev MARK-P4A root "\"A learned skill fits this request: harbor-tally\" in text"'
# skill_recall.rs scripts_note. 0.9.7 FAILS: the block listed no scripts.
check "P4: the recalled skill says its tested script ran successfully on the day of the run" 'ev MARK-P4A root "re.search(r\"scripts/solve\.py: ran successfully on 2026-11-10\", text)"'
check "P4: a script never run, and one changed since its run, are 'not run yet'" 'ev MARK-P4A root "re.search(r\"scripts/draft\.py: not run yet\", text) and re.search(r\"scripts/stale\.py: not run yet\", text)"'
E_FIRST='re.search(r"Run .+?/scripts/solve\.py first, before redoing the work by hand; if it fails, fix the script and say so\.", text) and not re.search(r"scripts/(draft|stale)\.py first", text)'
check "P4: it says to run the tested script first, and names no untested one" 'ev MARK-P4A root "$E_FIRST"'

echo "################ 5. P4: a script that ran with exit 0 is recorded as tested (the host checks the quote and the call)"
# The conversation writes and runs a one-line script; /refine then keeps it as
# a learned skill and quotes the run (script_runs). The host marks it tested
# only when the quote is in a tool result that reports exit=Some(0)
# timed_out=false and the call that made it names tally.py (refine.rs
# `confirmed_script_runs`, learned.rs `record_script_runs`).
pre5=""
for a in 1 2 3; do
  Pb=$RX/p4b-$a; sk="order-tally-$a"
  project "$Pb"
  mkdir -p "$Pb/data"
  printf 'id,status\n1,paid\n2,open\n3,paid\n4,paid\n5,paid\n' > "$Pb/data/orders.csv"
  (cd "$Pb" && git add -A && git -c user.name=t -c user.email=t@t commit -qm orders)
  agent "$Pb" "a$a"
  LINE="import csv; rows = list(csv.DictReader(open('data/orders.csv', encoding='utf-8'))); print('paid=%d' % sum(1 for row in rows if row['status'] == 'paid'))"
  say "a$a" "MARK-P4B-$a. Create tools/tally.py with write_file, holding exactly this one line, the text between LINE-BEGIN and LINE-END and not those markers: LINE-BEGIN $LINE LINE-END. Then run it once with run_process (executable python, args [\"tools/tally.py\"]) from the project root and tell me what it printed. Do nothing else." | tail -2 | sed 's/^/   /'
  say "a$a" "/refine Keep what this conversation solved as a learned skill: one skill intent, action create, name $sk, level global, a SKILL.md (front matter name: $sk, description: Use when asked to tally the paid orders, category: ops) that says to run scripts/tally.py from the project root, files {\"scripts/tally.py\": the exact one line of tools/tally.py from this conversation}, and script_runs [{\"path\": \"scripts/tally.py\", \"evidence\": \"paid=4\"}]. Propose no memory edits." | tail -3 | sed 's/^/   /'
  "$HA" shutdown --force >/dev/null 2>&1
  ran=$(ev "MARK-P4B-$a" root 'any(name == "run_process" and "tally.py" in json.dumps(args) for name, args in calls) and any("exit=Some(0) timed_out=false" in r and "paid=4" in r for r in results)' && echo yes)
  if [ "$ran" = yes ] && [ -f "$HA_HOME/skills/$sk/scripts/tally.py" ]; then pre5=$a; break; fi
  echo "   attempt $a: ran the script with exit 0: ${ran:-no}; the skill has the script: $([ -f "$HA_HOME/skills/$sk/scripts/tally.py" ] && echo yes || echo no)"
done
# Passes on 0.9.7 too: the conversation ran the script (exit=Some(0) timed_out=false is the host's
# own line) and /refine wrote the skill with the script in it.
check "precondition: the script was run with exit 0 and a learned skill carries it (attempt ${pre5:-none})" '[ -n "$pre5" ]'
j_mark() {
  "$PY" - "$HA_HOME/skills/order-tally-$pre5/scripts/tally.py" "$D/skill-usage/global.json" "order-tally-$pre5" <<'PYEOF'
import hashlib, json, sys
file, usage, skill = sys.argv[1:4]
try:
    sha = hashlib.sha256(open(file, "rb").read()).hexdigest()
    mark = json.load(open(usage, encoding="utf-8"))["skills"][skill]["scripts"]["scripts/tally.py"]
except (OSError, KeyError, ValueError):
    sys.exit(1)
sys.exit(0 if mark.get("sha") == sha and str(mark.get("at", ""))[:2] == "20" else 1)
PYEOF
}
# 0.9.7 FAILS: it had no `scripts` in the skill usage.
check "P4: the script is recorded as tested in the skill usage, with the SHA-256 of the file that ran" '[ -n "$pre5" ] && j_mark'

echo "################ 6. P5: a search here that finds nothing says when an earlier conversation holds the words"
P5=$RX/p5
project "$P5"
mkdir -p "$P5/notes"
printf 'The quillfeather marmoset tally closes on day 41.\n' > "$P5/notes/harbor-tally.txt"
(cd "$P5" && git add -A && git -c user.name=t -c user.email=t@t commit -qm notes)
WD=$P5 run_exec t5a "MARK-P5A. Read notes/harbor-tally.txt with read_file and tell me in one line on which day the tally closes."
# The kind of the journal event that holds the three words in that
# conversation (the file's result): a search restricted to it cannot match the
# next conversation's own user message or call, which hold the words as well.
KIND5=$("$PY" - "$D" <<'PYEOF' | tr -d '\r'
import glob, json, os, sqlite3, sys
kinds = set()
for db in glob.glob(os.path.join(sys.argv[1], "projects", "*", "harness.sqlite3")):
    for (raw,) in sqlite3.connect(f"file:{db}?mode=ro", uri=True).execute("select event_json from events"):
        if all(word in raw for word in ("quillfeather", "marmoset", "tally")):
            kinds.add(json.loads(raw)["event_type"])
kinds.discard("input.admitted")
print("receipt.recorded" if "receipt.recorded" in kinds else (sorted(kinds) or [""])[0])
PYEOF
)
export KIND5
echo "   the earlier conversation holds the words in a journal event of kind: ${KIND5:-none}"
check "precondition: the earlier conversation read the file and a journal event holds its words" 'ev MARK-P5A root "any(\"quillfeather marmoset tally\" in r for r in results)" && [ -n "$KIND5" ]'
# history_search's default scope is this conversation; the words are in no
# source of kind $KIND5 of it, only of the earlier conversation.
E_SEARCHED='any(name == "history_search" and "quillfeather" in str(args.get("query")) and os.environ["KIND5"] in str(args.get("kind")) and args.get("scope") in (None, "", "task") for name, args in calls)'
E_EARLIER='any("history_search" in r and re.search(r"nothing here holds all your words, but [1-9][0-9]* match\(es\) in earlier conversations of this project do - search again with scope project", r) for r in results)'
E_NOHITS='any("history_search" in r and "0 hit(s)" in r for r in results)'
asked5=""
for a in 1 2 3; do
  WD=$P5 run_exec t5b$a "MARK-P5B-$a. Call history_search exactly once with query \"quillfeather marmoset tally\" and kind \"$KIND5\" (leave scope out, so that it searches this conversation only). Then reply with the single word DONE-P5."
  if [ -n "$KIND5" ] && ev "MARK-P5B-$a" root "$E_SEARCHED"; then asked5=$a; break; fi
  echo "   attempt $a: the model did not make the call as asked"
done
check "precondition: history_search was called with the words and the kind, at the default scope (attempt ${asked5:-none})" '[ -n "$asked5" ]'
# turn_driver.rs render_tool_output. 0.9.7 FAILS: the result said "0 hit(s)" and nothing of the earlier conversation.
check "P5: the empty search says earlier conversations of the project hold the words, and to search with scope project" '[ -n "$asked5" ] && ev "MARK-P5B-$asked5" root "$E_EARLIER"'
# Passes on 0.9.7 too (a control): the earlier conversation's hits are not returned unasked.
check "P5 control: the search returned no hit of its own, and none of the earlier conversation" '[ -n "$asked5" ] && ev "MARK-P5B-$asked5" root "$E_NOHITS"'

echo "################ 7. P6: a child receives the text the user appended to the system prompt"
P6=$RX/p6
project "$P6"
# resources.rs appended_system: `--append-system-prompt` when given, else
# APPEND_SYSTEM.md of the config folder. delegation.rs inherited_system puts it
# at the end of the child's system policy (the message that opens "You are a
# child agent").
child6() { # child6 <name> <marker> [flags]: one delegate that only answers OK
  local name="$1" marker="$2"; shift 2
  WD=$P6 run_exec "$name" "$@" "MARK-$marker. Call the delegate tool exactly once: role explorer, brief exactly 'CHILD-$marker. Reply with the single word OK.' Wait for the child, then reply with the single word DONE-P6."
}
E_ROOT6='"$WORD" in text'
E_CALLED6='any(name == "delegate" and "CHILD-$MARK" in json.dumps(args) for name, args in calls)'
E_BRIEF6='n >= 1 and not any("$WORD" in content(m) for m in msgs if m.get("role") == "user")'
E_CHILD6='any("$WORD" in content(m) and "You are a child agent" in content(m) for m in msgs)'
sub() { # sub <expression>: with $WORD and $MARK filled in
  local e="$1"; e=${e//\$WORD/$WORD}; e=${e//\$MARK/$MARK}; printf '%s' "$e"
}
for variant in flag file; do
  if [ "$variant" = flag ]; then tag=A WORD=LANTERNWICK-27; else tag=B WORD=PLOVERMARK-58; fi
  ok6=""
  for attempt in 1 2 3; do
    MARK="P6$tag$attempt"
    if [ "$variant" = flag ]; then
      child6 "t6$tag$attempt" "$MARK" --append-system-prompt "House style for every agent of this run: end each final answer with the word $WORD."
    else
      printf 'House style for every agent of this run: end each final answer with the word %s.\n' "$WORD" > "$HA_HOME/APPEND_SYSTEM.md"
      child6 "t6$tag$attempt" "$MARK"
      rm -f "$HA_HOME/APPEND_SYSTEM.md"
    fi
    if ev "MARK-$MARK" root "$(sub "$E_CALLED6")" && ev "CHILD-$MARK" child 'n >= 1'; then ok6=1; break; fi
    echo "   $variant, attempt $attempt: the model did not delegate as asked"
  done
  # Passes on 0.9.7 too: the root has the text in its system prompt, a child ran on the exact brief.
  check "precondition ($variant): the root's system prompt carries the appended text" 'ev "MARK-$MARK" root "$(sub "$E_ROOT6")"'
  check "precondition ($variant): the root delegated once as asked and a child ran, its brief free of the appended text" '[ -n "$ok6" ] && ev "CHILD-$MARK" child "$(sub "$E_BRIEF6")"'
  # 0.9.7 FAILS: a child's system policy was its role text alone.
  check "P6 ($variant): the child's system policy carries the user's appended text" '[ -n "$ok6" ] && ev "CHILD-$MARK" child "$(sub "$E_CHILD6")"'
done

echo "evidence: $RX (conversation.md, the *.json answers, the store in home/data)"
result
