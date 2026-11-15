#!/usr/bin/env bash
# A skill the model activates reaches the model. With a real model. After 0.9.9
# (2026-11-15: in a benchmark's `ha exec` conversation of 32 requests the model
# called `activate_skill` for one skill more than 20 times and the skill's text
# was in none of them: the result said "its instructions are now in this
# conversation's context", and they were in a block of the request's rules that a
# turn does not send after its first step; every `--resume` is a new process, so
# nothing carried over either). `docs/architecture/STRATEGY_AND_RUNTIME.en.md`
# (the catalog and `activate_skill`):
#   1. `ha exec` with the learned skills in the catalog (the prompt is in
#      Vietnamese and the skills in English, so no skill is recalled whole) and a
#      prompt that tells the model to activate one: the result of its
#      `activate_skill` call holds the skill's text (a code word that is only in
#      the skill's body), and the model answers with the code word;
#   2. the same conversation resumed (a new process), a prompt that tells the
#      model to activate the skill once more: the catalog does not list it, and
#      `activate_skill` answers that it is already in the conversation instead of
#      loading it again.
# Where the model must act, the checks judge the feature only when the frozen
# requests show it did (a "precondition:" check); a turn that did not is tried
# again, up to three times; a turn the provider refused is its own failed
# precondition by name and a usage limit ends the attempts.
# What the model was sent and what the tools answered is read from the frozen
# requests (`ev` below, as in recall.sh). A comment above a check says what 0.9.9
# does with it: "0.9.9 FAILS" is the feature, "passes on 0.9.9 too" a
# precondition or a control. The conversations and the stores stay in
# $LIVE_ROOT/skill-activate as evidence.
#
#   bash scripts/testing/live-tests/skill-activate.sh
set -u
source "$(dirname "$0")/env.sh"
fresh skill-activate
export HA_AUTO_REFINE=off HA_SELF_EVAL=off HA_SESSION_LOG=off HA_PYTHON=${HA_PYTHON:-$PY}
D=$HA_HOME/data
P=$RX/p
WD=$P
TOKEN=QZ-7741-LIME
SKILL=kiln-burner-calibration

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
# refused (`<class>: <message>`, and no answer), or nothing when it answered.
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
refusal_check() {
  if [ -z "$1" ] && [ -n "$REFUSED" ]; then
    check "precondition: the provider refused the turn: ${REFUSED:0:200}" false
  fi
}

# What the model was sent and what the tools answered, read from the frozen
# requests: `ev <marker> root <python expression>`. It looks at the first
# conversation whose messages hold <marker> and judges the expression. In the
# expression: `msgs` (every message of it, once each), `text`, `results` (the
# tool results), `calls` ([(tool, arguments)]), `n`, `content(m)`, `turn(marker)`
# (the user message of the turn whose prompt holds the marker), `re`, `json`.
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

# The messages of the turn whose prompt holds $1, and the ones after it: for
# the checks of a resumed turn, which must not read the first turn's results.
since_turn() { printf '%s' 'msgs[[i for i, m in enumerate(msgs) if m.get("role") == "user" and "'"$1"'" in content(m)][0]:]'; }

echo "################ 0. six learned skills in the user's library"
# The user's global library ($HA_HOME/skills), skills as ha writes them. The
# kiln skill holds the code word in its BODY; its description says only what it
# is for, in English, and the prompts are Vietnamese: words recall no skill whole
# (the turn carries the catalog), and the model has to activate the skill itself.
skill() { # skill <name> <description> <body>
  mkdir -p "$HA_HOME/skills/$1"
  printf -- '---\nname: %s\ndescription: %s\n---\n%s\n' "$1" "$2" "$3" > "$HA_HOME/skills/$1/SKILL.md"
  echo '{"created_by": "ha", "created": "2026-10-01T00:00:00Z"}' > "$HA_HOME/skills/$1/.ha-learned.json"
}
skill "$SKILL" "Use when asked to calibrate the pressure of the kiln burner." "Set the burner dial to the painted mark, wait for the gauge to settle, then read the plate. The code word on the plate is $TOKEN; state it when you report the calibration."
skill release-notes "Use when asked to write release notes or a changelog entry." "Add the entry at the top of the changelog, one bullet per change, in the words of the people who asked for it."
skill image-resize "Use when asked to resize product images for the storefront." "Resize every product image to 800 pixels wide, keep the aspect ratio, and save the thumbnails beside the originals."
skill invoice-export "Use when asked to export invoices to the accounting system." "Export the invoices of the closed month as a CSV file, check the totals against the ledger, then upload the file to the accounting system."
skill log-rotation "Use when asked to rotate and compress the server logs." "Rotate the server logs at midnight, compress the old files with gzip, and delete archives older than thirty days."
skill cert-renewal "Use when asked to renew the TLS certificate for the proxy." "Request a new TLS certificate, install it on the proxy, reload the proxy, and check the expiry date in the browser."
project "$P"
echo "   skills: $(ls "$HA_HOME/skills" | tr '\n' ' ')"

echo "################ 1. the model activates a skill: its text is the result, and the answer holds the code word"
# The prompt tells the model what to do in words that are no skill's: "activate,
# with activate_skill, the learned skill about calibrating the kiln burner's
# pressure, then answer with its code word, nothing else".
E_ACT='any(name == "activate_skill" and args.get("name") == "'$SKILL'" for name, args in calls)'
E_CAT='"<learned_skills>" in turn("$M") and "<skill name=\"'$SKILL'\">" not in turn("$M")'
ok1=""; M1=""; REFUSED=""; STOP=0
for attempt in 1 2 3; do
  M=MARK-SA1-$attempt
  run_exec s1_$attempt "$M. Hãy dùng công cụ activate_skill để kích hoạt kỹ năng đã học về việc hiệu chuẩn áp suất của đầu đốt lò nung, rồi trả lời đúng một dòng: mã mà hướng dẫn của kỹ năng đó ghi. Không làm gì khác."
  echo "   attempt $attempt: $(answer s1_$attempt | head -c 200)"
  if refused s1_$attempt; then [ "$STOP" = 1 ] && break; continue; fi
  if ! ev "$M" root "$E_ACT"; then
    echo "   attempt $attempt: the model did not call activate_skill for $SKILL; retrying"
    continue
  fi
  ok1=$attempt; M1=$M
  break
done
refusal_check "$ok1"
# Passes on 0.9.9 too: the model called activate_skill for the skill (attempt ${ok1:-none}).
check "precondition: the model called activate_skill for $SKILL (attempt ${ok1:-none})" '[ -n "$ok1" ]'
if [ -n "$ok1" ]; then
  T1=$(jfield s1_$ok1 task_id)
  # Passes on 0.9.9 too: no recall put the skill whole into the turn, the catalog was there.
  check "precondition: the turn carried the catalog and not the skill whole" 'ev "$M1" root "${E_CAT//\$M/$M1}"'
  # turn_driver.rs `render_skill_activated`: the result opens "activated skill <name>: ..." and holds
  # the skill in its `<skill name>` tag. 0.9.9 FAILS: the result was one line saying the instructions
  # were in context ("activated skill skill:<name>@<version>: ..."), and the code word was in no request.
  E_RES='any(r.startswith("activated skill '$SKILL': ") and "<skill name=\"'$SKILL'\">" in r and "'$TOKEN'" in r for r in results)'
  check "the result of activate_skill holds the skill's text, with the code word that only its body states" 'ev "$M1" root "$E_RES"'
  # Fixed build: the code word is in a request the model was sent. 0.9.9 FAILS: it was in none.
  check "the code word is in the requests the model was sent" 'ev "$M1" root "\"$TOKEN\" in text.replace(\"$M1\", \"\")"'
  # 0.9.9 FAILS: the model never saw the code word (unless it read the skill's file by other means).
  check "the model's answer holds the code word" 'answer s1_$ok1 | grep -qF "$TOKEN"'
fi

echo "################ 2. the conversation resumed in a new process: the skill is held, not loaded again"
# `ha exec --resume` is a new process: the dispatcher's active map is empty, so only the
# conversation's own record (the earlier tool result) can say the skill is there.
ok2=""; M2=""; REFUSED=""; STOP=0
if [ -n "${T1:-}" ]; then
  for attempt in 1 2 3; do
    M=MARK-SA2-$attempt
    run_exec s2_$attempt --resume "$T1" "$M. Hãy gọi activate_skill một lần nữa cho kỹ năng hiệu chuẩn áp suất đầu đốt lò nung để chắc chắn, rồi trả lời đúng một dòng: mã mà hướng dẫn của nó ghi."
    echo "   attempt $attempt: $(answer s2_$attempt | head -c 200)"
    if refused s2_$attempt; then [ "$STOP" = 1 ] && break; continue; fi
    E_ACT2='any(c.get("name") == "activate_skill" for m in '"$(since_turn "$M")"' for c in m.get("tool_calls") or [])'
    if ! ev "$M" root "$E_ACT2"; then
      echo "   attempt $attempt: the model did not call activate_skill in the resumed turn; retrying"
      continue
    fi
    ok2=$attempt; M2=$M
    break
  done
  refusal_check "$ok2"
else
  echo "   (no first-turn task to resume)"
fi
check "precondition: the first run's task was known, and the resumed turn called activate_skill (attempt ${ok2:-none})" '[ -n "$ok2" ]'
if [ -n "$ok2" ]; then
  check "precondition: the second run resumed the first conversation" '[ -n "$(jfield s2_$ok2 resumed_from)" ]'
  # Passes on 0.9.9 too: the catalog is given once per conversation, and the first turn had it.
  check "the resumed turn's request lists no catalog line for the skill" 'ev "$M2" root "\"- $SKILL:\" not in turn(\"$M2\")"'
  # service.rs `skills_given` reads the activation result; skills.rs `already_in_context`.
  # 0.9.9 FAILS: the held set was empty in the new process, and the skill was loaded again.
  E_HELD='any("already in this conversation" in r and "'$SKILL'" in r for m in '"$(since_turn "$M2")"' for r in [content(m)] if m.get("role") == "tool")'
  check "activate_skill answers that the skill is already in the conversation" 'ev "$M2" root "$E_HELD"'
  # 0.9.9 FAILS: the second activation returned the skill's text again.
  E_NOT_AGAIN='not any(content(m).startswith("activated skill") for m in '"$(since_turn "$M2")"' if m.get("role") == "tool")'
  check "the skill's text is not loaded a second time" 'ev "$M2" root "$E_NOT_AGAIN"'
  # 0.9.9 FAILS the first check of section 1, so the answer here held it only by a fresh load.
  check "the model still answers with the code word" 'answer s2_$ok2 | grep -qF "$TOKEN"'
fi

echo "evidence: $LIVE_ROOT/skill-activate (conversation.md, the *.json answers, the stores in home/data)"
result
