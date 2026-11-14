#!/usr/bin/env bash
# A new conversation is shown the session logs that name what its request
# names, the log states the finding itself, and the logs a conversation was
# given are revised by its outcome. With a real model. After 0.9.8 (the
# Continual-ARC benchmark, 6 tasks x 4 visits: every log carried the task id in
# its keywords, but a visit was shown only the newest log - an unrelated task's
# - and the logs said "its exact grid appears in the conversation" instead of
# the finding). After ReHarness (arXiv 2610.12668); the design is in
# `docs/architecture/CONTINUAL_LEARNING.en.md` (session logs) and the rules are
# in crates/harness-cli/src/host/memory/session_log.rs:
#   1. forward read: ten session logs are written by hand under the project's
#      .harness/sessions (two name the made-up ticket OPS-7302 and hold the
#      flag that "fixed" it, eight are about other things; a word tells logs
#      apart when at most a quarter of them hold it, so eight others make two
#      a quarter). `ha exec` with a prompt naming OPS-7302 gets a
#      `<session-log conversation="...">` for both ticket logs and for no other,
#      the finding's words, the lead that frames them as historical reference,
#      and the `session_log` / `related` diagnostics record (count 2 of 10
#      candidates); a prompt that names nothing the logs hold gets none (count
#      0); the same in the chat (the app), which builds its start blocks apart
#      from `ha exec`;
#   2. backward revision: that conversation goes on (`ha exec --resume`, which
#      makes the second user turn a log needs), the user saying the flag did NOT
#      work and what did. At exit the conversation's log is written with the
#      two ticket logs in front of the model, and the log it writes says in
#      `## Earlier Logs Revisited` what held of them; each line naming a log the
#      conversation was given is appended to THAT log under `## Later
#      Conversations`, as `- <date> (<conversation>): <note>`, and to no other.
#      The conversation's own log puts the ticket first in its keywords.
# The seed is data the host reads, not the model's work, so the checks of the
# request and of the host's note are exact. Where the model must act - write a
# revised line, put the name first - the check is judged only when its
# precondition held (a "precondition:" check, from the files the run left),
# and the conversation is run again, up to three times; a turn the provider
# refused is its own failed precondition by name and a usage limit ends the
# attempts. A precondition that never held is a FAIL by its name, never a pass.
# A comment above a check says what 0.9.8 does with it: "0.9.8 FAILS" is the
# feature, "passes on 0.9.8 too" a precondition or a control. Every check
# prints PASS or FAIL; the conversations and the stores stay in
# $LIVE_ROOT/related-logs as evidence.
#
#   bash scripts/testing/live-tests/related-logs.sh
set -u
source "$(dirname "$0")/env.sh"
fresh related-logs
export HA_AUTO_REFINE=off HA_SELF_EVAL=off HA_PYTHON=${HA_PYTHON:-$PY}
# The logs of the chat's conversations are written when it goes quiet; out of
# reach, so only the exits of `ha exec` write one here.
export HA_IDLE_REVIEW_SECONDS=3600
unset HA_SESSION_LOG
D=$HA_HOME/data
P=$RX/p
WD=$P
S=$P/.harness/sessions

# Two valid task ids (UUIDv7) for the ticket logs, eight for the others: the
# conversations are not in any store, and the selection must not need them to be.
T1=task_00000000-0000-7000-8000-00000000a701
T2=task_00000000-0000-7000-8000-00000000a702
OTHERS=(b01 b02 b03 b04 b05 b06 b07 b08)
other_id() { echo "task_00000000-0000-7000-8000-00000000${1}0"; }

# One `ha exec` turn in $WD: `run_exec <name> [flags] <prompt>`.
run_exec() {
  local name="$1"; shift
  printf '\n## %s (ha exec %s)\n\n> %s\n\n' "$(date +%H:%M:%S)" "$name" "${*: -1}" >> "$LOG"
  (cd "${WD:-$P}" && "$HA" exec --model "$MODEL" "${TH[@]}" --approval full-auto --output-format json "$@" >"$RX/$name.json" 2>"$RX/$name.err")
  answer "$name" >> "$LOG"
  sed 's/^/    /' "$RX/$name.err" >> "$LOG"
}
answer() { "$PY" -c "import json,sys; print(json.load(open(sys.argv[1],encoding='utf-8')).get('response',''))" "$RX/$1.json" 2>/dev/null; }
jfield() { "$PY" -c "import json,sys; print(json.load(open(sys.argv[1],encoding='utf-8')).get(sys.argv[2]) or '')" "$RX/$1.json" "$2" 2>/dev/null | tr -d '\r'; }

# refusal <name>: the first stderr line of an `ha exec` turn the provider
# refused, or nothing when the turn answered (as recall.sh).
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

# What the model was sent, read from the frozen requests: `ev <marker> root
# <python expression>` (see recall.sh for the names in scope: `turn(marker)` is
# the user message of the turn whose prompt holds the marker, where the
# context blocks of a turn are, `text`, `calls`, `results`, `n`).
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
text = "\n".join([content(m) for m in msgs] + [f"{c.get('name')} {c.get('arguments')}" for m in msgs for c in m.get("tool_calls") or []])
n = len(picked)
def turn(marker):
    return "\n".join(content(m) for m in msgs if m.get("role") == "user" and marker in content(m))
sys.exit(0 if eval("(" + expr.strip() + ")") else 1)
PYEOF
ev() { "$PY" "$RX/ev.py" "$D" "$@"; }
now_ms() { "$PY" -c "import time; print(int(time.time()*1000))"; }

# related_since <ms>: `count/candidates` of each `session_log` / `related`
# diagnostics record since a time, oldest first: `2/10,0/10`.
related_since() {
  cat "$D"/diagnostics/*.jsonl 2>/dev/null | "$PY" -c "
import json, sys
since = int(sys.argv[1])
found = []
for line in sys.stdin:
    try:
        r = json.loads(line)
    except ValueError:
        continue
    if r.get('kind') == 'session_log' and r.get('event') == 'related' and r.get('ms', 0) >= since:
        f = r.get('fields', {})
        found.append((r['ms'], '%s/%s' % (f.get('count'), f.get('candidates'))))
print(','.join(a for _, a in sorted(found)))
" "$1" | tr -d '\r'
}

echo "################ 0. ten session logs, written by hand"
project "$P"
# seed_log <stem> <conversation> <topic> <keywords> <outcome> <learning> <minutes ago>
seed_log() {
  local written
  written=$(date -u -d "-$7 minutes" +%Y-%m-%dT%H:%M:%SZ)
  mkdir -p "$S"
  printf '%s\n' \
    "# Session Log: $(date +%Y-%m-%d) - $3" "" \
    "## Quick Reference (for AI scanning)" \
    "**Conversation:** $2" \
    "**Through:** session_00000000-0000-7000-8000-00000000c001" \
    "**Written:** $written" \
    "**Confidence keywords:** $4" \
    "**Projects:** worker" \
    "**Outcome:** $5" "" \
    "## Key Learnings" "- $6" "" \
    "---" "" \
    "## Quick Resume Context" "$5" > "$S/$(date +%Y-%m-%d-%H_%M)-$1.md"
}
# The ten logs, again: a run that adds a log of its own is wiped by the next attempt.
seed_all() {
  rm -rf "$S"
  seed_log ops-7302-first "$T1" "Worker deploy failing" "OPS-7302, worker, deploy, exit code 137" \
    "OPS-7302 was fixed by draining the worker before the deploy." \
    "OPS-7302: the nightly deploy failed with exit code 137. Starting the worker with the flag --drain-first fixed it." 600
  seed_log ops-7302-second "$T2" "Nightly job killed" "OPS-7302, nightly, queue" \
    "OPS-7302 came back and the same flag held." \
    "OPS-7302 again: the queue stopped growing once the worker ran with --drain-first." 300
  local i=0
  for topic in "Parser rewrite|tokenizer, grammar|The grammar now has a start rule.|Rules are tried in order." \
               "Changelog pass|changelog, tags|The changelog lists every merged change.|Entries are one bullet each." \
               "Image sizes|thumbnails, storefront|Thumbnails are now 800 pixels wide.|The aspect ratio is kept." \
               "Certificate renewal|certificate, proxy|The proxy has a new certificate.|The expiry is in ninety days." \
               "Log rotation|archives, gzip|Old archives are compressed.|Rotation runs at midnight." \
               "Invoice export|ledger, accounting|The month closes with a CSV file.|Totals match the ledger." \
               "Cache tuning|eviction, memory|The cache keeps the newest entries.|Eviction is least recently used." \
               "Docs pass|readme, headings|The readme has clearer headings.|Each page starts with a summary."; do
    IFS='|' read -r t k o l <<< "$topic"
    seed_log "other-$i" "$(other_id "${OTHERS[$i]}")" "$t" "$k" "$o" "$l" $((900 + i * 10))
    i=$((i + 1))
  done
}
seed_all
echo "   seeded: $(ls "$S" | wc -l) logs"
check "precondition: ten logs were seeded" '[ "$(ls "$S" | wc -l)" -eq 10 ]'

# The rule of the host, restated for the seed: a word of the prompt tells when
# at most a quarter of the logs hold it; a log is related when its topic, its
# keywords or its projects hold one (not its outcome, a sentence).
# `seed_related <prompt>` prints the logs it relates (by their first keyword), so a seed that cannot give the answer the
# checks expect is a failed precondition, not a failed feature.
seed_related() {
  "$PY" - "$S" "$1" <<'PYEOF' | tr -d '\r'
import glob, os, re, sys
def words(text):
    return {w for w in re.split(r"[^0-9a-zÀ-￿]+", text.lower()) if len(w) >= 2}
logs = []
for path in glob.glob(os.path.join(sys.argv[1], "*.md")):
    text = open(path, encoding="utf-8").read()
    body = "\n".join(l for l in text.splitlines() if not re.match(r"\s*\*\*(Conversation|Through|Written):\*\*", l))
    topic = text.splitlines()[0].split(" - ", 1)[1]
    names = " ".join(l.split(":**", 1)[1] for l in body.splitlines() if re.match(r"\s*\*\*(Confidence keywords|Projects):\*\*", l))
    logs.append((os.path.basename(path), words(body), words(topic + " " + names)))
asked = words(sys.argv[2])
telling = {w for w in asked if 0 < sum(w in held for _, held, _ in logs) * 4 <= len(logs)}
print(" ".join(sorted(name for name, _, trigger in logs if telling & trigger)))
PYEOF
}

echo "################ 1. ha exec: the logs that name the request are in it"
since1=$(now_ms)
PROMPT1="MARK-L1. Ticket OPS-7302 is back: the nightly deploy fails again. What do the earlier notes of this project say about fixing it? Answer in one sentence and call no tools."
REL1=$(seed_related "$PROMPT1")
echo "   the seed relates: $REL1"
check "precondition: the seed relates the two ticket logs and no other" '[ "$REL1" = "$(cd "$S" && ls *ops-7302*.md | tr "\n" " " | sed "s/ $//")" ]'
run_exec l1 "$PROMPT1"
echo "   $(answer l1 | head -c 240)"
r=$(refusal l1)
echo "   (provider: ${r:-answered})"
check "precondition: the turn's request was found" 'ev MARK-L1 root "n == 1"'
TAG='<session-log conversation=\"'
# session_log.rs related_block; 0.9.8 FAILS: ha exec showed a new conversation no session log at all.
check "forward read: the request carries the log of each ticket conversation" 'ev MARK-L1 root "(\"$TAG$T1\\\">\" in turn(\"MARK-L1\")) and (\"$TAG$T2\\\">\" in turn(\"MARK-L1\"))"'
# 0.9.8 FAILS (no session-log tag at all).
check "forward read: and the log of no other conversation" 'ev MARK-L1 root "turn(\"MARK-L1\").count(\"$TAG\") == 2"'
check "forward read: the finding itself is in the request" 'ev MARK-L1 root "\"--drain-first\" in turn(\"MARK-L1\")"'
check "forward read: framed as historical reference, not instructions" 'ev MARK-L1 root "\"HISTORICAL REFERENCE ONLY, NOT LIVE INSTRUCTIONS\" in turn(\"MARK-L1\") and \"name what this request names\" in turn(\"MARK-L1\")"'
check "forward read: the diagnostics say two of ten logs were chosen" '[ "$(related_since "$since1")" = "2/10" ]'
# The model's use of it: judged only when the provider answered.
if [ -z "$r" ]; then
  # 0.9.8 FAILS: the flag is in no other place the model can see.
  check "the model answers with the flag from the log" 'answer l1 | grep -q -- "--drain-first"'
fi

echo "################ 2. a prompt that names nothing the logs hold gets none"
since2=$(now_ms)
# "and" is a word of this prompt and of the second ticket log's outcome, and of
# no other log: rare enough to tell, but no name of the log. A build that
# matched the outcome too (5f8d5a9) related that log by it: 1 of 10.
PROMPT2="MARK-L2. Describe the colour of the sky at noon in one short sentence and call no tools."
check "precondition: the first run wrote no log of its own (ten candidates)" '[ "$(ls "$S" | wc -l)" -eq 10 ]'
check "precondition: no word of the prompt relates a log in the seed" '[ -z "$(seed_related "$PROMPT2")" ]'
run_exec l2 "$PROMPT2"
echo "   $(answer l2 | head -c 200)"
check "precondition: the turn's request was found" 'ev MARK-L2 root "n == 1"'
# Passes on 0.9.8 too (it showed no logs at all): a control.
check "no related logs: no session-log tag in the request" 'ev MARK-L2 root "\"<session-log conversation=\" not in turn(\"MARK-L2\")"'
# 0.9.8 FAILS: no such record.
check "no related logs: the diagnostics say none of ten were chosen" '[ "$(related_since "$since2")" = "0/10" ]'

echo "################ 3. the chat (the app) builds its start blocks apart from ha exec"
P3=$RX/p3
project "$P3"
S_SAVED=$S; S=$P3/.harness/sessions
seed_all
S=$S_SAVED
since3=$(now_ms)
agent "$P3" rel1
out3=$(say rel1 "MARK-L3. Ticket OPS-7302 is back: the nightly deploy fails again. What do the earlier notes of this project say about fixing it? Answer in one sentence and call no tools."); echo "$out3" | tail -2 | sed 's/^/   /'
"$HA" shutdown --force >/dev/null 2>&1
check "precondition: the chat turn's request was found" 'ev MARK-L3 root "n == 1"'
# service.rs run_turn -> previous_conversation_blocks; 0.9.8 FAILS: the chat showed only the newest log.
check "app: the request carries the log of each ticket conversation and of no other" 'ev MARK-L3 root "(\"$TAG$T1\\\">\" in turn(\"MARK-L3\")) and (\"$TAG$T2\\\">\" in turn(\"MARK-L3\")) and turn(\"MARK-L3\").count(\"$TAG\") == 2"'
check "app: the finding itself is in the request" 'ev MARK-L3 root "\"--drain-first\" in turn(\"MARK-L3\")"'
check "app: the diagnostics say two of ten logs were chosen" '[ "$(related_since "$since3")" = "2/10" ]'

echo "################ 4. the conversation goes on and says the flag did not work: the logs it was given are revised"
# Up to three attempts at what the model must do: write the section that names
# a log it was given. Each attempt is a new conversation on the seed as it was.
HELD=""; NOTED_ID=""; CONV=""; CLOG=""
BEFORE_T1=""; BEFORE_T2=""
for attempt in 1 2 3; do
  REFUSED=""; STOP=0
  seed_all
  mark=MARK-B$attempt
  run_exec b${attempt}a "$mark-1. Ticket OPS-7302 is back: the nightly deploy fails again. The earlier notes of this project name a flag; say which one in one sentence and call no tools."
  if refused b${attempt}a; then [ "$STOP" -eq 1 ] && break; continue; fi
  CONV=$(jfield b${attempt}a task_id)
  [ -n "$CONV" ] || continue
  run_exec b${attempt}b --resume "$CONV" "$mark-2. I ran it with the flag --drain-first and it did NOT work: the deploy still failed with exit code 137. What fixed OPS-7302 was starting the worker with --max-conns=40 instead. Say in one sentence which of the two held, and call no tools."
  if refused b${attempt}b; then [ "$STOP" -eq 1 ] && break; continue; fi
  CLOG=$(grep -l "^\*\*Conversation:\*\* $CONV" "$S"/*.md 2>/dev/null | head -1)
  # The model must have written the section and named one of the two logs it was given.
  if [ -n "$CLOG" ] && grep -q "^## Earlier Logs Revisited" "$CLOG" && sed -n '/^## Earlier Logs Revisited/,/^## \|^---/p' "$CLOG" | grep -qE "$T1|$T2"; then
    HELD=$attempt
    break
  fi
  echo "   attempt $attempt: the model's log did not revise a log it was given; trying again"
done
echo "   conversation: ${CONV:-none}; log: ${CLOG:-none}"
[ -n "$CLOG" ] && sed 's/^/   | /' "$CLOG" | sed -n '1,40p'
if [ -z "$HELD" ] && [ -n "$REFUSED" ]; then
  check "precondition: the provider refused the turn: ${REFUSED:0:200}" false
fi
check "precondition: the second turn's request was found, and the first turn's carried both ticket logs" 'ev "MARK-B${HELD:-1}-2" root "n == 1 and turn(\"MARK-B${HELD:-1}-1\").count(\"$TAG\") == 2"'
check "precondition: the conversation's log was written, and it revises a log it was given" '[ -n "$HELD" ]'
if [ -n "$HELD" ]; then
  # Which of the two ids the log names.
  NAMED=$(sed -n '/^## Earlier Logs Revisited/,/^## \|^---/p' "$CLOG" | grep -oE "$T1|$T2" | sort -u | tr '\n' ' ')
  NOTE_LINE() { sed -n '/^## Earlier Logs Revisited/,/^## \|^---/p' "$CLOG" | grep -E "^[-*] \`?\**$1" | head -1 | sed -E 's/^[-*] [^:]*: *//'; }
  echo "   the log revises: $NAMED"
  gained() { [ -n "$(sed -n '/^## Later Conversations/,/^## \|^---/p' "$S"/*"$1"*.md 2>/dev/null | grep -F "($CONV):")" ]; }
  # session_log.rs revise / with_later_note; 0.9.8 FAILS: a log was never changed by a later conversation.
  for id in $NAMED; do
    stem=$([ "$id" = "$T1" ] && echo ops-7302-first || echo ops-7302-second)
    check "revision: the log $stem gained a note of this conversation under Later Conversations" 'gained "$stem"'
    check "revision: the note is dated and names the conversation, on one line" 'grep -qE "^- [0-9]{4}-[0-9]{2}-[0-9]{2} \($CONV\): .+" "$S"/*"$stem"*.md'
    note=$(NOTE_LINE "$id")
    # The note is the model's line, redacted and bounded: its first words are the line's.
    check "revision: the note is what the new log said of it" '[ -n "$note" ] && grep -qF "($CONV): ${note:0:40}" "$S"/*"$stem"*.md'
    check "revision: the log it revised keeps its own finding (Key Learnings) and its place before the resume" 'grep -q -- "--drain-first" "$S"/*"$stem"*.md && [ "$(grep -n "^## Later Conversations" "$S"/*"$stem"*.md | cut -d: -f1)" -lt "$(grep -n "^## Quick Resume Context" "$S"/*"$stem"*.md | cut -d: -f1)" ]'
  done
  # Only a log the model named, and the conversation was given, gets a note.
  for stem in ops-7302-first ops-7302-second; do
    id=$([ "$stem" = ops-7302-first ] && echo "$T1" || echo "$T2")
    case " $NAMED" in *" $id "*) ;; *)
      check "revision: the log $stem, which the new log did not name, gained nothing" '! grep -q "^## Later Conversations" "$S"/*"$stem"*.md';;
    esac
  done
  check "revision: none of the eight other logs gained a note" '! grep -l "^## Later Conversations" "$S"/*other-*.md 2>/dev/null | grep -q .'
  # The model's judgement, tied to what the user said: the note says what did work.
  check "revision (the model's judgement): a note keeps what the user said fixed it" 'cat "$S"/*ops-7302-*.md | sed -n "/^## Later Conversations/,/^## \|^---/p" | grep -qi -- "max-conns"'
  # prompt(): the first keyword; 0.9.8 may pass by the model's own choice (nothing asked it to).
  check "forward write (the model's judgement): the log puts the ticket first in its keywords" 'sed -n "s/^\*\*Confidence keywords:\*\* //p" "$CLOG" | head -1 | cut -d, -f1-3 | grep -qi "OPS-7302"'
  # INSTRUCTIONS; 0.9.8 may pass: it did not forbid the pointer, a model rarely wrote it.
  check "forward write (the model's judgement): the log does not send a later session to the conversation for the finding" '! grep -qiE "(appears?|is|are) (shown |written |given )?in the conversation" "$CLOG"'
  # The conversation's own log is not itself given to anyone, so it has no Later Conversations of its own.
  check "the new log has no Later Conversations of its own" '! grep -q "^## Later Conversations" "$CLOG"'
fi
"$HA" shutdown --force >/dev/null 2>&1

result
