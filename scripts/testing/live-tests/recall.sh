#!/usr/bin/env bash
# A learned skill is recalled for a request, once per conversation, and the user
# sees it; the review's note is not the user's; a problem that comes back is
# kept by its name. With a real model. After 0.9.7 (2026-11-13: in a real use
# a learned skill went into 283 of 299 turns, 209 of them the same one, and was
# counted used 215 times while the review judged it twice; in a 24-conversation
# run 39 of about 50 review edits were refused for evidence nobody had said).
# `docs/architecture/STRATEGY_AND_RUNTIME.en.md` (recall rules) and
# `docs/architecture/CONTINUAL_LEARNING.en.md` P7 and P8:
#   1. `ha exec` recalls a learned skill (it never did): stderr says `learned
#      skill <name> offered for this request (unproven)`, the turn's request
#      carries the skill whole, and skill-usage counts `offered`, not `use`;
#   2. the same conversation resumed, a prompt the skill fits again: the request
#      carries one line ("The learned skill <name> given earlier in this
#      conversation still fits ...") and not a second whole block, and no second
#      notice; the same in a chat (the app) conversation, three turns;
#   3. a prompt no skill fits gets none (seven skills, so the spread rule
#      applies; the prompts share only common words, or one word that tells
#      skills apart, with them);
#   4. (section 5 below) a goal's continuation asks nothing: a skill that shares words with the
#      host's own continuation text, and none with the goal's, is not recalled
#      for the continuations;
#   5. P7: the automatic review of a conversation in which the user corrects the
#      agent: no edit is refused for evidence the conversation does not hold;
#   6. P8: a returning problem the user names (OPS-4471), solved by a tool
#      result that shows success, is kept by the review under that name.
# Where the model must act, the checks judge the feature only when the session's
# own record shows the model did (a "precondition:" check); a turn that
# did not is tried again, up to three times; a turn the provider refused is its
# own failed precondition by name and a usage limit ends the attempts. A
# precondition that never held is a FAIL by its name, never a pass; the last
# section (P8) ends with PRECONDITION FAILED (exit 2) when no review kept
# anything in three attempts (the model's judgement; nothing for the host to be
# judged on).
# What the model was sent is read from the frozen requests (`ev` below), the
# markers are the host's own strings, quoted in a comment from the Rust source
# that writes them. A comment above a check says what 0.9.7 does with it:
# "0.9.7 FAILS" is the feature, "passes on 0.9.7 too" a precondition or a
# control. Every check prints PASS or FAIL; the conversations and the stores stay
# in $LIVE_ROOT/recall and $LIVE_ROOT/recall-review as evidence.
#
#   bash scripts/testing/live-tests/recall.sh
set -u
source "$(dirname "$0")/env.sh"
fresh recall
export HA_AUTO_REFINE=off HA_SELF_EVAL=off HA_SESSION_LOG=off HA_PYTHON=${HA_PYTHON:-$PY}
D=$HA_HOME/data
P=$RX/p
WD=$P

# One `ha exec` turn in $WD: `run_exec <name> [flags] <prompt>`.
run_exec() {
  local name="$1"; shift
  printf '\n## %s (ha exec %s)\n\n> %s\n\n' "$(date +%H:%M:%S)" "$name" "${*: -1}" >> "$LOG"
  (cd "${WD:-$P}" && "$HA" exec --model "$MODEL" "${TH[@]}" --approval full-auto --output-format json "$@" >"$RX/$name.json" 2>"$RX/$name.err")
  answer "$name" >> "$LOG"
  sed 's/^/    /' "$RX/$name.err" >> "$LOG"
}
answer() { "$PY" -c "import json,sys; print(json.load(open(sys.argv[1],encoding='utf-8')).get('response',''))" "$RX/$1.json" 2>/dev/null; }
# A field of a run's JSON (task_id, session_id, resumed_from).
jfield() { "$PY" -c "import json,sys; print(json.load(open(sys.argv[1],encoding='utf-8')).get(sys.argv[2]) or '')" "$RX/$1.json" "$2" 2>/dev/null | tr -d '\r'; }

# refusal <name>: the first stderr line of an `ha exec` turn the provider
# refused - the host prints `<class>: <message>` (for example `rate_limited:
# You have hit your ChatGPT usage limit ...`) and no answer - or nothing when
# the turn answered. A refused turn is not the model skipping a step: the loops
# below name it as its own failed precondition, and stop at a usage limit
# (`rate_limited`), where another attempt only spends the next minutes' quota.
refusal() {
  "$PY" - "$RX/$1.json" "$RX/$1.err" <<'PYEOF' | tr -d '\r'
import json, re, sys
try:
    if "response" in json.load(open(sys.argv[1], encoding="utf-8")):
        sys.exit(0)
except (OSError, ValueError):
    pass
try:
    lines = open(sys.argv[2], encoding="utf-8", errors="replace").read().splitlines()
except OSError:
    lines = []
for line in lines:
    if re.match(r"[a-z][a-z_]*: ", line) and not line.startswith("ha: "):
        print(line[:300])
        break
else:
    print("no answer and no error line")
PYEOF
}
# refused <turn>: sets REFUSED to the refusal line and returns 0 when the
# provider refused the turn; STOP=1 when it was a usage limit.
REFUSED=""; STOP=0
refused() {
  local line
  line=$(refusal "$1")
  [ -z "$line" ] && return 1
  REFUSED=$line
  echo "   the provider refused the turn $1: $line"
  case "$line" in rate_limited:*) STOP=1;; esac
  return 0
}
# A section's loop begins with `REFUSED=""; STOP=0`; when no attempt got its
# precondition, `refusal_check <held>` fails by name if the provider refused.
refusal_check() {
  if [ -z "$1" ] && [ -n "$REFUSED" ]; then
    check "precondition: the provider refused the turn: ${REFUSED:0:200}" false
  fi
}

# What the model was sent and what the tools answered, read from the frozen
# requests: `ev <marker> root|child <python expression>` (the expression is the
# fourth argument, or read from stdin). It looks at the first conversation
# whose messages hold <marker> and judges the expression, true or false. In
# the expression: `msgs` (every message of it, once each: a request is stored as
# a delta of the one before, and a resumed one can repeat the history), `text`
# (all of them and the tool calls, joined), `results` (the tool results), `calls`
# ([(tool, arguments)]), `n` (how many conversations), `content(m)`, `turn(marker)`
# (the user message of the turn whose prompt holds the marker: the context
# blocks of a turn, such as a recalled skill, are in it), `re`, `json`, `os`.
write_ev() {
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
    picked = picked[:1]
msgs, seen = [], set()
for t in picked:
    for m in tasks[t]:
        key = json.dumps(m, sort_keys=True)
        if key not in seen:
            seen.add(key)
            msgs.append(m)
def arguments(call):
    try:
        return json.loads(call.get("arguments") or "{}")
    except ValueError:
        return {}
calls = [(c.get("name"), arguments(c)) for m in msgs for c in m.get("tool_calls") or []]
results = [content(m) for m in msgs if m.get("role") == "tool"]
text = "\n".join([content(m) for m in msgs] + [f"{c.get('name')} {c.get('arguments')}" for m in msgs for c in m.get("tool_calls") or []])
n = len(picked)
def turn(marker):
    return "\n".join(content(m) for m in msgs if m.get("role") == "user" and marker in content(m))
sys.exit(0 if eval("(" + expr.strip() + ")") else 1)
PYEOF
}
write_ev
ev() { "$PY" "$RX/ev.py" "$D" "$@"; }
# The `again` of each learned-skill recall since a time: `false,true`.
now_ms() { "$PY" -c "import time; print(int(time.time()*1000))"; }
recall_again() {
  cat "$D"/diagnostics/*.jsonl 2>/dev/null | "$PY" -c "
import json, sys
since = int(sys.argv[1])
found = []
for line in sys.stdin:
    try:
        r = json.loads(line)
    except ValueError:
        continue
    if r.get('kind') == 'skill' and r.get('event') == 'recalled' and r.get('ms', 0) >= since:
        found.append((r['ms'], str(r['fields'].get('again')).lower()))
print(','.join(a for _, a in sorted(found)))
" "$1" | tr -d '\r'
}
# usage_is <skill> <python expression over u, its skill-usage record>.
usage_is() {
  "$PY" - "$D/skill-usage/global.json" "$1" "$2" <<'PYEOF'
import json, sys
try:
    u = json.load(open(sys.argv[1], encoding="utf-8"))["skills"].get(sys.argv[2], {})
except (OSError, ValueError, KeyError):
    sys.exit(1)
sys.exit(0 if eval(sys.argv[3]) else 1)
PYEOF
}
# The sum of `offered` over every skill of the usage record.
offered_total() {
  "$PY" -c "
import json, sys
try:
    skills = json.load(open(sys.argv[1], encoding='utf-8')).get('skills', {})
except (OSError, ValueError):
    skills = {}
print(sum(s.get('offered', 0) for s in skills.values()))" "$D/skill-usage/global.json" | tr -d '\r'
}

echo "################ 0. seven learned skills in the user's library"
# The user's global library ($HA_HOME/skills), skills as ha writes them. Their
# descriptions share the words of "Use when asked to the ..." (common words that
# tell no skill apart); the rest is each skill's own. The last one shares words
# with the host's goal-continuation text and none with the goal of section 4.
skill() { # skill <name> <description> <body>
  mkdir -p "$HA_HOME/skills/$1"
  printf -- '---\nname: %s\ndescription: %s\n---\n%s\n' "$1" "$2" "$3" > "$HA_HOME/skills/$1/SKILL.md"
  echo '{"created_by": "ha", "created": "2026-10-01T00:00:00Z"}' > "$HA_HOME/skills/$1/.ha-learned.json"
}
skill db-migration "Use when asked to run the database migration against staging." "Write the database migration, check that staging answers its health page, then run the migration against staging with the flag --safe-order and report the rows it changed."
skill release-notes "Use when asked to write release notes or a changelog entry." "Add the entry at the top of the changelog, one bullet per change, in the words of the people who asked for it."
skill image-resize "Use when asked to resize product images for the storefront." "Resize every product image to 800 pixels wide, keep the aspect ratio, and save the thumbnails beside the originals."
skill invoice-export "Use when asked to export invoices to the accounting system." "Export the invoices of the closed month as a CSV file, check the totals against the ledger, then upload the file to the accounting system."
skill log-rotation "Use when asked to rotate and compress the server logs." "Rotate the server logs at midnight, compress the old files with gzip, and delete archives older than thirty days."
skill cert-renewal "Use when asked to renew the TLS certificate for the proxy." "Request a new TLS certificate, install it on the proxy, reload the proxy, and check the expiry date in the browser."
skill audit-requirements "Use when asked to audit the current state against every requirement and report the remaining proof." "Audit the state of the work against each requirement, list the proof for each one, and report the status and the remaining tokens of effort."
project "$P"
echo "   skills: $(ls "$HA_HOME/skills" | tr '\n' ' ')"

echo "################ 1. ha exec recalls the skill that fits, shows the notice and counts an offer"
since1=$(now_ms)
run_exec r1 "MARK-R1. Run the database migration against staging. Reply with the single word OK-R1 and call no tools."
echo "   $(answer r1 | head -c 200)"
r=$(refusal r1)
# The recall is asked before the model is; a refusal of the model's call changes none of these checks, only
# the resume of section 2, which needs the run's task.
echo "   (provider: ${r:-answered})"
T1=$(jfield r1 task_id)
# Passes on 0.9.7 too: the turn's request was stored, and the model did not itself activate the skill
# (which would count as a use).
check "precondition: the turn's request was found" 'ev MARK-R1 root "n == 1"'
check "precondition: the model did not activate a skill itself" 'ev MARK-R1 root "not any(name == \"activate_skill\" for name, args in calls)"'
# skill_recall.rs `notice`; headless.rs prints it on stderr. 0.9.7 FAILS: ha exec recalled no learned skill.
check "recall: the notice 'learned skill db-migration offered for this request (unproven)' is on stderr" 'grep -qF "learned skill db-migration offered for this request (unproven)" "$RX/r1.err"'
# skill_recall.rs `block`. 0.9.7 FAILS: no block on exec.
E_R1='"A learned skill fits this request: db-migration" in turn("MARK-R1") and "<skill name=\"db-migration\">" in turn("MARK-R1") and turn("MARK-R1").count("A learned skill fits") == 1'
check "recall: the turn's request carries the skill whole, and only that one" 'ev MARK-R1 root "$E_R1"'
# skill-usage `offered` / `use` (learned.rs UseKind). 0.9.7 FAILS: nothing was recorded on exec.
U1='u.get("offered", 0) >= 1 and u.get("use", 0) == 0'
check "recall: skill-usage counts the skill offered, and not used" 'usage_is db-migration "$U1"'

echo "################ 2. the same conversation, a prompt the skill fits again: one line, not a second block"
if [ -n "$T1" ]; then
  run_exec r2 --resume "$T1" "MARK-R2. Run the database migration against staging again. Reply with the single word OK-R2 and call no tools."
  echo "   $(answer r2 | head -c 200)"
else
  echo "   (the first run left no task to resume; the provider: ${r:-answered})"
  : > "$RX/r2.json"; : > "$RX/r2.err"
fi
check "precondition: the first run's task was known, and the second run resumed it" '[ -n "$T1" ] && [ -n "$(jfield r2 resumed_from)" ]'
check "precondition: the second turn request was found" 'ev MARK-R2 root "n == 1"'
check "precondition: the model did not activate a skill itself in the second turn" 'ev MARK-R2 root "not any(name == \"activate_skill\" for name, args in calls)"'
# skill_recall.rs `again`, `given_in`; "No second notice" - offer_skill returns none for it.
# 0.9.7: n/a on exec (no recall at all).
check "recall once: the second turn shows no notice" '! grep -q "offered for this request" "$RX/r2.err"'
E_R2='"The learned skill db-migration given earlier in this conversation still fits" in turn("MARK-R2") and "<skill name=\"db-migration\">" not in turn("MARK-R2") and text.count("<skill name=\"db-migration\">") <= 1'
check "recall once: the second turn carries one line saying it was given earlier, not the block again" 'ev MARK-R2 root "$E_R2"'
U2='u.get("offered", 0) == 1 and u.get("use", 0) == 0'
check "recall once: the second turn did not count another offer" 'usage_is db-migration "$U2"'

echo "################ 3. a prompt no skill fits gets none"
# The first shares one word that tells skills apart with the library
# ("staging", of db-migration only) and common ones; the second only common
# words ("use", "when", "asked", "to", "the"). One such word is not a fit.
E_NONE='"A learned skill fits" not in turn("$MARK") and "given earlier in this conversation" not in turn("$MARK") and "<skill name=\"" not in turn("$MARK")'
for variant in 1 2; do
  if [ "$variant" = 1 ]; then
    MARK=MARK-R3A; prompt="$MARK. When asked to plan the party, use the staging area for the cake. Reply with the single word OK-R3A and call no tools."
  else
    MARK=MARK-R3B; prompt="$MARK. When asked to sing, use the song of autumn. Reply with the single word OK-R3B and call no tools."
  fi
  run_exec r3$variant "$prompt"
  e=${E_NONE//\$MARK/$MARK}
  check "precondition ($variant): the turn's request was found" 'ev "$MARK" root "n == 1"'
  # Passes on 0.9.7 too on exec (no recall there); on the app path 0.9.7 recalled a skill for most turns.
  check "no recall ($variant): no skill block and no line in the turn" 'ev "$MARK" root "$e"'
  check "no recall ($variant): no notice on stderr" '! grep -q "offered for this request" "$RX/r3$variant.err"'
done
check "no recall: the offers counted are still the one of section 1" '[ "$(offered_total)" = 1 ]'

echo "################ 4. the chat (the app): whole once, one line after it, nothing for a turn that fits none"
# The same function asks in the app (service.rs run_turn): 0.9.7 put the skill
# whole into every turn it fitted, and its words were scored against any
# input, the host's own included.
P4=$RX/p4
project "$P4"
since4=$(now_ms)
agent "$P4" app1
out1=$(say app1 "MARK-R4A. Run the database migration against staging. Reply with the single word OK-R4A and call no tools."); echo "$out1" | tail -2 | sed 's/^/   /'
out2=$(say app1 "MARK-R4B. Run the database migration against staging again. Reply with the single word OK-R4B and call no tools."); echo "$out2" | tail -2 | sed 's/^/   /'
out3=$(say app1 "MARK-R4C. When asked to sing, use the song of autumn. Reply with the single word OK-R4C and call no tools."); echo "$out3" | tail -2 | sed 's/^/   /'
"$HA" shutdown --force >/dev/null 2>&1
check "precondition: three app turns were answered and their requests found" 'ev MARK-R4A root "n == 1 and turn(\"MARK-R4B\") != \"\" and turn(\"MARK-R4C\") != \"\""'
# 0.9.7 FAILS: the block came whole in every turn the skill fitted.
E_R4='"A learned skill fits this request: db-migration" in turn("MARK-R4A") and "A learned skill fits" not in turn("MARK-R4B") and "<skill name=\"db-migration\">" not in turn("MARK-R4B") and "The learned skill db-migration given earlier in this conversation still fits" in turn("MARK-R4B") and text.count("<skill name=\"db-migration\">") == 1'
check "app: the first turn carries the skill whole, the second one line, the conversation one block" 'ev MARK-R4A root "$E_R4"'
E_R4C='"A learned skill fits" not in turn("MARK-R4C") and "given earlier in this conversation" not in turn("MARK-R4C")'
check "app: a turn that fits no skill carries nothing" 'ev MARK-R4A root "$E_R4C"'
# The diagnostics record every recall with `again` (service.rs offer_skill).
check "app: the recalls were whole, then given earlier: again=false,true" '[ "$(recall_again "$since4")" = "false,true" ]'
# The notice is a session event of the app; `ha send --wait` is assumed to print it with the answer.
check "app: the notice is shown for the first turn" 'printf "%s" "$out1" | grep -qF "learned skill db-migration offered for this request"'
check "app: and not for the second" '! printf "%s" "$out2" | grep -qF "offered for this request"'

echo "################ 5. a goal's continuation asks nothing"
# The goal is none of the skills'; the host's continuation text ("Continue
# working toward the active thread goal. ... audit the current state against
# every requirement ... remaining tokens ... status ...") is audit-requirements'.
# 0.9.7 scored the newest input, the host's own included.
P5=$RX/p5
project "$P5"
agent "$P5" g1
# Sent without waiting: the goal never finishes.
"$HA" send g1 "/goal MARK-R5. Harness test: each turn answer with only the line NOT-DONE-R5 and end the turn. Never finish this goal. Do not use any tool." >/dev/null 2>&1
continued() { "$HA" send g1 "/goal status" --wait 2>&1 | sed -n 's/.*Continued: \([0-9]*\).*/\1/p' | head -1; }
wait_s=${RECALL_GOAL_WAIT_S:-600}
started=$SECONDS
turns=0
while [ $((SECONDS - started)) -lt "$wait_s" ]; do
  turns=$(continued); turns=${turns:-0}
  [ "$turns" -ge 2 ] && break
  sleep 5
done
echo "   continued $turns turn(s) in $((SECONDS - started)) s"
"$HA" send g1 "/goal pause" --wait >/dev/null 2>&1
sleep 5
"$HA" shutdown --force >/dev/null 2>&1
E_CONT='sum(1 for m in msgs if m.get("role") == "user" and "Continue working toward the active thread goal." in content(m)) >= 2'
check "precondition: the goal was continued at least twice, and the continuations are in the record" 'ev MARK-R5 root "$E_CONT"'
# goal.rs CONTINUATION_LEAD; skill_recall.rs `users_words`. 0.9.7 FAILS if its recall scored the
# continuation text (expected, not certain: it depends on that text standing out for the one skill).
E_NOASK='not any("A learned skill fits" in content(m) or "given earlier in this conversation" in content(m) for m in msgs if m.get("role") == "user")'
check "a goal's continuations carry no learned skill: not whole, not as a line" 'ev MARK-R5 root "$E_NOASK"'

echo "################ 6. P7: the review's refusals (a conversation in which the user corrects the agent)"
# CONTINUAL_LEARNING P7, refine.rs `plan` / REVIEW_NOTE_HEADER: the gate's note
# reaches the planner as a <review_note>, nobody's request and no evidence.
# 0.9.7 gave it as the user's <user_refine_instructions>; the planner wrote that
# the user asked and quoted the gate's sentence, which no message holds, and the
# host refused the edit ("the evidence is not a verbatim quote ...": 39 of ~50 edits
# in a 24-conversation run; one in the continual.sh run on 0.9.7). Not certain
# on 0.9.7, which is why this judges only a review that proposed an edit, and it
# is a control on the number of such refusals, not on one.
# A fresh store reviewed after every turn.
setup_review() { # setup_review <name>
  fresh "$1"
  D=$HA_HOME/data
  P=$RX/p
  WD=$P
  write_ev
  project "$P"
  cat > "$P/calc.py" <<'EOF'
def add(a, b):
    return a + b


def sub(a, b):
    return a - b
EOF
  mkdir -p "$P/tools"
  cat > "$P/tools/sync.py" <<'EOF'
import sys
if "--lock-wait" not in sys.argv:
    sys.exit("sync: the lock is held by the nightly job; retry with --lock-wait <seconds>")
print("sync ok: 12 rows")
EOF
  (cd "$P" && git add -A && git -c user.name=t -c user.email=t@t commit -qm fixtures)
  # Review after every turn, keeping the thinking level fresh() set.
  "$PY" - "$HA_HOME/settings.json" <<'PYEOF'
import json, os, sys
path = sys.argv[1]
settings = json.load(open(path, encoding="utf-8")) if os.path.exists(path) else {}
settings["autoRefine"] = {"turnInterval": 1}
json.dump(settings, open(path, "w", encoding="utf-8"))
PYEOF
  unset HA_AUTO_REFINE
}
# The edits the reviews of this home recorded (appliedEdits of every refinements.jsonl).
edits_recorded() { "$PY" - "$D" <<'PYEOF'
import glob, json, os, sys
count = 0
for path in glob.glob(os.path.join(sys.argv[1], "**", "refinements.jsonl"), recursive=True):
    for line in open(path, encoding="utf-8", errors="replace"):
        try:
            count += len(json.loads(line).get("appliedEdits") or [])
        except ValueError:
            pass
print(count)
PYEOF
}
# refused_unheld: how many edits were refused as "the evidence is not a verbatim quote ..." whose
# evidence is in no message of the home's conversations (prompts, answers, tool results).
refused_unheld() { "$PY" - "$D" <<'PYEOF'
import glob, json, os, re, sqlite3, sys
data = sys.argv[1]
def norm(text):
    return re.sub(r"\s+", " ", str(text)).strip().lower()
held = []
for db in glob.glob(os.path.join(data, "projects", "*", "harness.sqlite3")):
    conn = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
    try:
        for (raw,) in conn.execute("select request_json from frozen_requests"):
            for m in json.loads(raw).get("messages", []):
                held.append(m.get("content") or "")
    except sqlite3.Error:
        pass
held = norm(" ".join(held))
count = 0
for path in glob.glob(os.path.join(data, "**", "refinements.jsonl"), recursive=True):
    for line in open(path, encoding="utf-8", errors="replace"):
        try:
            record = json.loads(line)
        except ValueError:
            continue
        for edit in record.get("appliedEdits") or []:
            if edit.get("applied") is False and str(edit.get("error", "")).startswith("the evidence is not a verbatim quote"):
                evidence = norm(edit.get("evidence") or (edit.get("lesson") or {}).get("evidence") or "")
                if evidence and evidence not in held:
                    count += 1
print(count)
PYEOF
}
# Whether any frozen request holds the planner's prompt: the upkeep calls leave only their usage in the
# diagnostics (`refine call`, purpose review_plan), so this is expected to find nothing.
planner_seen() { "$PY" - "$D" <<'PYEOF'
import glob, os, sqlite3, sys
review = instructions = False
for db in glob.glob(os.path.join(sys.argv[1], "projects", "*", "harness.sqlite3")):
    conn = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
    try:
        for (raw,) in conn.execute("select request_json from frozen_requests"):
            review = review or "\\n<review_note>\\n" in raw
            instructions = instructions or "\\n<user_refine_instructions>\\n" in raw
    except sqlite3.Error:
        pass
print(("note " if review else "") + ("instructions" if instructions else ""))
PYEOF
}
ok7=""; REFUSED=""; STOP=0
for attempt in 1 2 3; do
  [ "$attempt" -gt 1 ] && { rm -rf "$RX-attempt-$((attempt - 1))"; cp -r "$RX" "$RX-attempt-$((attempt - 1))"; }
  setup_review recall-review
  WD=$P run_exec a1 "MARK-R7-$attempt. Giải thích thật chi tiết từng dòng của calc.py."
  if refused a1; then [ "$STOP" = 1 ] && break; continue; fi
  WD=$P run_exec a2 --resume "$(jfield a1 task_id)" "Dài quá, mình đọc không nổi. Hàm sub làm gì? 1-2 câu thôi."
  if refused a2; then [ "$STOP" = 1 ] && break; continue; fi
  edits=$(edits_recorded)
  echo "   attempt $attempt: the review proposed $edits edit(s); refused for evidence nobody holds: $(refused_unheld)"
  if [ "${edits:-0}" -ge 1 ]; then ok7=$attempt; break; fi
done
refusal_check "$ok7"
# Passes on 0.9.7 too: a review ran (it is the model's to propose an edit) and recorded at least one.
check "precondition: the automatic review proposed at least one edit (attempt ${ok7:-none})" '[ -n "$ok7" ]'
# 0.9.7 FAILS when its planner quotes the gate's sentence (common, not certain).
check "P7: no edit was refused for evidence that no message of the conversation holds" '[ -n "$ok7" ] && [ "$(refused_unheld)" = 0 ]'
seen=$(planner_seen)
if [ -z "$seen" ]; then
  echo "   (not observable: no frozen request holds the planner's prompt, so <review_note> is not checked; the refusals above are)"
else
  # 0.9.7 FAILS: the note came as <user_refine_instructions>.
  check "P7: the planner's prompt carries <review_note> and no <user_refine_instructions>" '[ "$seen" = note ]'
fi

echo "################ 7. P8: a problem that comes back is kept by its name"
# CONTINUAL_LEARNING P8, refine.rs AUTO_SCOPE_POLICY and the gate's prompt: the
# conversation names the problem (OPS-4471) and says it comes back, a tool
# result shows what worked, the user confirms; the review keeps what worked
# under that name - a memory entry's title, a learned skill's name or a
# knowledge page's title carries it. 0.9.7's review kept one generic rule or
# nothing (not certain: it may have named it anyway).
# kept_by_name: lines naming a store entry whose title/name carries OPS-4471
# (case and punctuation aside).
kept_by_name() { "$PY" - "$D" "$HA_HOME" "$P" <<'PYEOF'
import glob, json, os, re, sqlite3, subprocess, sys
data, home, project = sys.argv[1:4]
def key(text):
    return re.sub(r"[^a-z0-9]", "", str(text).lower())
want = "ops4471"
found = []
for db in glob.glob(os.path.join(data, "harness", "memory.sqlite3")) + glob.glob(os.path.join(data, "sessions", "*", "harness", "memory.sqlite3")):
    try:
        conn = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
        column = "key" if "key" in [c[1] for c in conn.execute("pragma table_info(memory)")] else "name"
        for kind, name, doc in conn.execute(f"select kind, {column}, doc from memory"):
            entry = json.loads(doc)
            if want in key(name) or want in key(entry.get("title")):
                found.append(f"memory {kind} {name}")
    except sqlite3.Error:
        pass
for root in (os.path.join(home, "skills"), os.path.join(project, ".harness", "skills")):
    for folder in glob.glob(os.path.join(root, "*")):
        text = os.path.basename(folder)
        try:
            head = open(os.path.join(folder, "SKILL.md"), encoding="utf-8", errors="replace").read(600)
        except OSError:
            head = ""
        if want in key(text) or want in key(head.split("---")[1] if head.startswith("---") and "---" in head[3:] else ""):
            found.append(f"skill {text}")
for root in (os.path.join(project, ".harness", "knowledge"), os.path.join(data, "knowledge")):
    for path in glob.glob(os.path.join(root, "**", "*.md"), recursive=True):
        try:
            first = open(path, encoding="utf-8", errors="replace").readline()
        except OSError:
            continue
        if want in key(os.path.basename(path)) or want in key(first):
            found.append(f"knowledge {os.path.relpath(path, root)}")
print("\n".join(found))
PYEOF
}
# applied_total / refused_with_name: the reviews' applied edits, and the edits
# carrying the name that the host refused (the review tried).
applied_total() { "$PY" - "$D" <<'PYEOF'
import glob, json, os, sys
count = 0
for path in glob.glob(os.path.join(sys.argv[1], "**", "refinements.jsonl"), recursive=True):
    for line in open(path, encoding="utf-8", errors="replace"):
        try:
            count += sum(1 for edit in json.loads(line).get("appliedEdits") or [] if edit.get("applied"))
        except ValueError:
            pass
print(count)
PYEOF
}
refused_with_name() { "$PY" - "$D" <<'PYEOF'
import glob, json, os, re, sys
for path in glob.glob(os.path.join(sys.argv[1], "**", "refinements.jsonl"), recursive=True):
    for line in open(path, encoding="utf-8", errors="replace"):
        try:
            record = json.loads(line)
        except ValueError:
            continue
        for edit in record.get("appliedEdits") or []:
            if edit.get("applied") is False and "ops4471" in re.sub(r"[^a-z0-9]", "", json.dumps(edit, ensure_ascii=False).lower()):
                print(str(edit.get("error"))[:200])
PYEOF
}
E_SOLVED='any("sync ok: 12 rows" in r and "exit=Some(0)" in r for r in results)'
ok8=""; tried=""; kept_other=""; named=""; REFUSED=""; STOP=0
for attempt in 1 2 3; do
  [ "$attempt" -gt 1 ] && { rm -rf "$RX-attempt-$((attempt - 1))"; cp -r "$RX" "$RX-attempt-$((attempt - 1))"; }
  setup_review recall-review
  WD=$P run_exec b1 "MARK-R8-$attempt. Ticket OPS-4471: python tools/sync.py fails in this project. Run it with run_process from the project root, make it work, and tell me the command that worked. This ticket comes back every month."
  if refused b1; then [ "$STOP" = 1 ] && break; continue; fi
  if ! ev "MARK-R8-$attempt" root "$E_SOLVED"; then
    echo "   attempt $attempt: no tool result shows the sync working (the model did not solve it as asked); retrying"
    continue
  fi
  WD=$P run_exec b2 --resume "$(jfield b1 task_id)" "Yes, that worked, thank you. OPS-4471 will be back next month."
  if refused b2; then [ "$STOP" = 1 ] && break; continue; fi
  ok8=$attempt
  named=$(kept_by_name)
  total=$(applied_total)
  echo "   attempt $attempt: the review applied $total edit(s); kept by the name: ${named:-nothing}"
  [ -n "$named" ] && break
  tried=$(refused_with_name | head -1)
  [ "${total:-0}" -ge 1 ] && kept_other=$attempt
  [ -n "$tried" ] && break
done
refusal_check "$ok8"
# Passes on 0.9.7 too: the model solved it (a tool result shows it), and the user confirmed.
check "precondition: the problem was solved with a tool result that shows it and the user confirmed (attempt ${ok8:-none})" '[ -n "$ok8" ]'
if [ -n "$named" ]; then
  # 0.9.7 FAILS (not certain): its review kept a generic rule, or nothing.
  check "P8: the review kept what solved OPS-4471 under that name: ${named//$'\n'/; }" true
elif [ -n "$tried" ]; then
  check "P8: the review proposed it under that name and the host refused it: $tried" false
elif [ -n "$kept_other" ]; then
  check "P8: the review kept something, but nothing titled or named OPS-4471 (attempt $kept_other)" false
fi
if [ -z "$named" ] && [ -z "$tried" ] && [ -z "$kept_other" ] && [ -n "$ok8" ]; then
  echo "################ RESULT: $pass passed, $fail failed"
  echo "PRECONDITION FAILED: in 3 attempts the review applied no edit for OPS-4471 (see $RX-attempt-*, $RX); the model judged nothing worth keeping, nothing to judge the host on; rerun"
  exit 2
fi

echo "evidence: $LIVE_ROOT/recall, $LIVE_ROOT/recall-review (conversation.md, the *.json answers, the stores in home/data)"
result
