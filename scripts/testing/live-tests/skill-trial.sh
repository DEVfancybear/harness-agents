#!/usr/bin/env bash
# A learned skill's revision is a few edits on trial, rolled back when it does
# worse, and a proven skill is pruned under the same trial (after SkillOpt,
# arXiv 2605.23904, and EfficientSkill, arXiv 2611.19823;
# docs/architecture/COST_AND_SKILLS.en.md), with a real model through `ha exec`
# and its review after every turn:
#
#   1. A proven learned skill (three uses that worked) is corrected by the
#      user; the review revises it with an `edit` (a whole rewrite of a proven
#      skill is refused) and the revision goes on trial, the incumbent's
#      counters kept.
#   2. The revised command breaks in the next conversations; the review judges
#      the skill failed and the failure counts against the revision. It does
#      not stand: two failed uses roll it back by itself (SKILL.md is the
#      incumbent's text, its counters back, kept as rolled_back), or the
#      review's fix after a failed use replaces it (kept as superseded) and
#      is judged against the sound incumbent; `ha learning report` counts how
#      the trial settled.
#   3. `ha learning prune --skill <name>` prunes a long proven skill: it is
#      shorter, and on trial with the objective `prune`.
#
# Preconditions the model must meet (the review revised the skill; the skill
# was followed and judged) are retried and named when they never held.
# The control is this script with HA=<the previous release's ha.exe>: 0.9.11
# keeps the failing revision (it only holds it back from recall) and has no
# `ha learning prune`, so 2 and 3 fail there.
set -u
source "$(dirname "$0")/env.sh"

winpath() { cygpath -m "$1" 2>/dev/null || printf '%s' "$1"; }
py() { "$PY" "$@" | tr -d '\r'; }
SKILL=weekly-report

INCUMBENT="---
name: $SKILL
description: Make this project's weekly sales report: run its report script and give the user the total it prints.
---
1. Run the report script with run_process: executable \`python\`, args [\"report.py\"], in the project root.
2. Give the user the line that starts with TOTAL, as printed."

setup() {  # setup [name]: a fresh home (default skill-trial)
  fresh "${1:-skill-trial}"
  export HA_DAEMON=off HA_SELF_EVAL=off HA_SESSION_LOG=off HA_REPL=off HA_CODEMODE=off HA_ACTION_BANK=off HA_STEP_JUDGE=off
  unset HA_AUTO_REFINE
  D=$HA_HOME/data
  P=$RX/p
  project "$P"
  cat > "$P/report.py" <<'EOF'
import argparse, sys
parser = argparse.ArgumentParser()
parser.parse_args()  # takes no option: any flag is an error
sales = [120, 340, 95, 410]
print("week 46")
print(f"TOTAL: {sum(sales)}")
EOF
  (cd "$P" && git add -A && git -c user.name=t -c user.email=t@t commit -qm report)
  "$PY" - "$HA_HOME/settings.json" <<'EOF'
import json, os, sys
path = sys.argv[1]
settings = json.load(open(path, encoding="utf-8")) if os.path.exists(path) else {}
settings["autoRefine"] = {"turnInterval": 1}
json.dump(settings, open(path, "w", encoding="utf-8"))
EOF
  # The user's global library, a skill as ha writes it, proven by three uses
  # that worked.
  mkdir -p "$HA_HOME/skills/$SKILL" "$D/skill-usage"
  printf '%s\n' "$INCUMBENT" > "$HA_HOME/skills/$SKILL/SKILL.md"
  echo '{"created_by": "ha", "created": "2026-10-01T00:00:00Z"}' > "$HA_HOME/skills/$SKILL/.ha-learned.json"
  cat > "$D/skill-usage/global.json" <<EOF
{"requests": 12, "skills": {"$SKILL": {"offered": 3, "use": 3, "worked": 3, "last": "2026-11-10T00:00:00Z",
  "cases": [{"at": "2026-11-10T00:00:00Z", "evidence": "TOTAL: 965"}]}}}
EOF
}

run() {
  local name="$1"; shift
  printf '\n## %s (ha exec %s)\n\n> %s\n\n' "$(date +%H:%M:%S)" "$name" "${*: -1}" >> "$LOG"
  (cd "$P" && "$HA" exec --model "$MODEL" "${TH[@]}" --approval full-auto --output-format json "$@" >"$RX/$name.json" 2>"$RX/$name.err")
  py -c "import json,sys; print(json.load(open(sys.argv[1],encoding='utf-8')).get('response',''))" "$(winpath "$RX/$name.json")" >> "$LOG" 2>/dev/null
}
# A field of the skill's usage record: `usage <python expression over u>`.
usage() {
  py -c "
import json, sys
try:
    u = json.load(open(sys.argv[1], encoding='utf-8'))['skills'].get(sys.argv[2], {})
except Exception:
    u = {}
v = eval(sys.argv[3], {'u': u})
print('' if v is None else v)" "$(winpath "$D/skill-usage/global.json")" "$SKILL" "$1"
}
skill_text() { tr -d '\r' < "$HA_HOME/skills/$SKILL/SKILL.md"; }
ledger_actions() { py -c "
import json, sys
for line in open(sys.argv[1], encoding='utf-8'):
    try: print(json.loads(line).get('action', ''))
    except ValueError: pass" "$(winpath "$HA_HOME/skills/$SKILL/.ledger.jsonl")" 2>/dev/null | tr '\n' ' '; }

CORRECT="A correction for the learned skill $SKILL, for next time: the report script must now be run with the --summary-only flag, as python report.py --summary-only. Do not run anything now; only say you have noted it."
USE="Make this week's sales report with the learned skill $SKILL, following its steps as written, and tell me the total."

# A use whose run_process call passed --summary-only and was refused (exit 2):
# the revision, followed as written, broke the command.
broke() {
  py - "$(winpath "$D")" <<'PY'
import glob, json, os, sqlite3, sys
calls, broken = {}, 0
for db in glob.glob(os.path.join(sys.argv[1], "projects", "*", "harness.sqlite3")):
    connection = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
    for (request,) in connection.execute("select request_json from frozen_requests"):
        for message in json.loads(request).get("messages") or []:
            for call in message.get("tool_calls") or []:
                if call.get("name") == "run_process" and "--summary-only" in str(call.get("arguments")):
                    calls[call.get("call_id")] = True
            if message.get("role") == "tool" and calls.get(message.get("tool_call_id"))                     and "exit=Some(2)" in str(message.get("content")):
                broken += 1
print(broken)
PY
}

# --- 1, 2. The review revises the proven skill with an edit; the revision is on
# trial; followed as written it breaks the command, and it does not stand. The
# review may instead write a revision that checks the flag first: it breaks
# nothing, and the round is tried again (the precondition names it).
held=0; revised=0
for round in 1 2 3; do
  setup "skill-trial-r$round"
  run "correct" "$CORRECT"
  skill_text | grep -q -- "--summary-only" || continue
  revised=1
  ledger1=$(ledger_actions)
  incumbent_worked=$(usage 'u.get("trial", {}).get("incumbent", {}).get("worked")')
  for use in 1 2 3 4 5 6; do
    run "use-$use" "$USE"
    echo "   round $round use $use: trial_bad=$(usage 'u.get("trial_bad")') trial_good=$(usage 'u.get("trial_good")') trial=$(usage '"open" if u.get("trial") else "none"') broke=$(broke)"
    if [ "$(usage '"open" if u.get("trial") else "none"')" = none ]; then break; fi
  done
  if [ "$(broke)" -ge 1 ] && [ "$(usage '"open" if u.get("trial") else "none"')" = none ]; then held=1; break; fi
done
check "precondition: the review revised the skill with the correction (rounds: $round)" '[ "$revised" = 1 ]'
check "precondition: a use followed the revision as written and its command broke, and the trial settled (rounds: $round)" '[ "$held" = 1 ]'
if [ "$held" = 1 ]; then
  check "the revision was an edit or a patch, not a whole rewrite of a proven skill (ledger: $ledger1)" 'printf "%s" "$ledger1" | grep -qE "(edit|patch) *$"'
  check "the revision went on trial with the incumbent's three uses that worked kept ($incumbent_worked)" '[ "$incumbent_worked" = 3 ]'
  rolled=$(usage 'sum(1 for r in u.get("rejected", []) if r.get("kind") == "rolled_back")')
  superseded=$(usage 'sum(1 for r in u.get("rejected", []) if r.get("kind") == "superseded" and "--summary-only" in r.get("edits_summary", ""))')
  echo "   rejected: $(usage '[(r.get("kind"), r.get("reason")) for r in u.get("rejected", [])]')"
  check "the failing revision was rejected: rolled back ($rolled) or superseded by a fix ($superseded)" '[ "${rolled:-0}" -ge 1 ] || [ "${superseded:-0}" -ge 1 ]'
  # Since 0.9.15 a skill whose revision regressed is narrowed with a condition
  # (GUARD's paired regret, docs/architecture/SELF_IMPROVEMENT_GATE.en.md): the
  # fix may keep the flag behind a check and run the script without it when
  # the script does not take it. Either way the broken call is not the skill's
  # only way any more: the flag is gone, or the skill also runs report.py
  # without it and that revision was accepted on good uses.
  check "the revision that broke the command is not the skill now (no --summary-only, or a fallback without it that was accepted)" '! skill_text | grep -qF "\"report.py\", \"--summary-only\"" || { skill_text | grep -qF "[\"report.py\"]" && tr -d "" < "$D/skill-usage/revisions.jsonl" | tail -1 | grep -q "\"accepted\""; }'
  check "a failed use counted against the revision, not the incumbent (trial events: $(tr -d '\r' < "$D/skill-usage/revisions.jsonl" 2>/dev/null | py -c "import json,sys; print([json.loads(l)[\"event\"] for l in sys.stdin if l.strip()])"))" '[ "${rolled:-0}" -ge 1 ] && [ "$(skill_text)" = "$INCUMBENT" ] && [ "$(usage "u.get(\"worked\")")" = 3 ] || [ "${superseded:-0}" -ge 1 ]'
  check "ha learning report counts how the revision was settled" '"$HA" learning report 2>&1 | tee "$RX/report.txt" | grep -qiE "[1-9][0-9]* (rolled back|accepted)"'
fi

# --- 3. `ha learning prune` shortens a long proven skill, on trial as a prune.
# A home of its own each attempt, so sections 1 and 2 keep their evidence. The
# model's edits must name text the skill holds; an edit the promoter refuses
# lands nothing, and the attempt is made again (the precondition names it).
LONG=release-notes
landed=0
for attempt in 1 2 3; do
  setup "skill-prune-$attempt"
  mkdir -p "$HA_HOME/skills/$LONG"
  {
    printf -- '---
name: %s
description: Write the release notes of this project from its git log, grouped by kind of change.
---
' "$LONG"
    for step in "Run git log from the last tag to HEAD with run_process."                 "Read every commit subject in the output carefully."                 "Group the commits by their prefix: feat, fix, docs, test, chore."                 "Write one section per group, in that order."                 "Write each commit as one bullet, its subject without the prefix."; do
      printf '%s
' "- $step" "- Remember: $step" "- It is important that you always do this: $step" "- Check again that you did this: $step" "- Do not forget: $step"
    done
    printf '%s
' "- Give the user the notes."
  } > "$HA_HOME/skills/$LONG/SKILL.md"
  echo '{"created_by": "ha", "created": "2026-10-01T00:00:00Z"}' > "$HA_HOME/skills/$LONG/.ha-learned.json"
  cat > "$D/skill-usage/global.json" <<EOF
{"requests": 20, "skills": {"$LONG": {"offered": 4, "use": 4, "worked": 4, "last": "2026-11-12T00:00:00Z"}}}
EOF
  before_lines=$(grep -cv '^\s*$' "$HA_HOME/skills/$LONG/SKILL.md")
  before_chars=$(wc -c < "$HA_HOME/skills/$LONG/SKILL.md")
  (cd "$P" && "$HA" learning prune --skill "$LONG" --model "$MODEL" "${TH[@]}" > "$RX/prune.txt" 2>&1)
  SKILL=$LONG
  echo "   prune attempt $attempt: $(tr -d '
' < "$RX/prune.txt" | tail -2 | tr '
' ' ')"
  if ledger_actions | grep -qE "edit *$"; then landed=1; break; fi
done
after_lines=$(grep -cv '^\s*$' "$HA_HOME/skills/$LONG/SKILL.md")
after_chars=$(wc -c < "$HA_HOME/skills/$LONG/SKILL.md")
check "ha learning prune ran ($RX/prune.txt)" 'grep -qi "prune" "$RX/prune.txt" && ! grep -qi "unrecognized\|unknown command" "$RX/prune.txt"'
check "precondition: the model's prune landed as an edit (attempts: $attempt)" '[ "$landed" = 1 ]'
if [ "$landed" = 1 ]; then
  check "the pruned skill is shorter ($before_lines -> $after_lines non-blank lines, $before_chars -> $after_chars bytes)" '[ "$after_lines" -lt "$before_lines" ] && [ "$after_chars" -lt "$before_chars" ]'
  check "the pruned skill keeps its steps (git log, the groups, one bullet per commit)" 'grep -qi "git log" "$HA_HOME/skills/$LONG/SKILL.md" && grep -qi "feat" "$HA_HOME/skills/$LONG/SKILL.md" && grep -qi "bullet" "$HA_HOME/skills/$LONG/SKILL.md"'
  check "the prune is on trial with the objective prune ($(usage 'u.get("trial", {}).get("objective")'))" '[ "$(usage "u.get(\"trial\", {}).get(\"objective\")")" = prune ]'
fi

echo "   ha: $HA ($(sha256sum "$HA" | cut -c1-16))"
result
