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

# --- 1. The review revises the proven skill with an edit; the revision is on trial.
held=0
for attempt in 1 2 3; do
  setup
  run "correct-$attempt" "$CORRECT"
  if skill_text | grep -q -- "--summary-only"; then held=1; break; fi
done
check "precondition: the review revised the skill with the correction (attempts: $attempt; ledger: $(ledger_actions))" '[ "$held" = 1 ]'
if [ "$held" = 1 ]; then
  check "the revision is an edit or a patch, not a whole rewrite of a proven skill (ledger: $(ledger_actions))" 'ledger_actions | grep -qE "(edit|patch) *$"'
  check "the revision is on trial, with the incumbent's three uses that worked kept ($(usage 'u.get("trial", {}).get("incumbent")'))" '[ "$(usage "u.get(\"trial\", {}).get(\"incumbent\", {}).get(\"worked\")")" = 3 ]'

  # --- 2. The failing revision does not stand.
  for use in 1 2 3 4; do
    run "use-$use" "$USE"
    echo "   use $use: trial_bad=$(usage 'u.get("trial_bad")') trial_good=$(usage 'u.get("trial_good")') trial=$(usage '"open" if u.get("trial") else "none"')"
    if [ "$(usage '"open" if u.get("trial") else "none"')" = none ]; then break; fi
  done
  # The failing revision does not stand, one of two ways: two failed uses
  # roll it back to the incumbent, or the review fixes it after a failed use,
  # the failing one is kept as superseded and the fix is judged against the
  # incumbent (and kept when its uses work).
  rolled=$(usage 'sum(1 for r in u.get("rejected", []) if r.get("kind") == "rolled_back")')
  superseded=$(usage 'sum(1 for r in u.get("rejected", []) if r.get("kind") == "superseded" and "--summary-only" in r.get("edits_summary", ""))')
  echo "   rejected: $(usage '[(r.get("kind"), r.get("reason")) for r in u.get("rejected", [])]')"
  check "the failing revision was rejected: rolled back ($rolled) or superseded by a fix ($superseded)" '[ "${rolled:-0}" -ge 1 ] || [ "${superseded:-0}" -ge 1 ]'
  check "the revision that broke the command is not the skill now (no --summary-only in SKILL.md)" '! skill_text | grep -q -- "--summary-only"'
  check "a failed use counted against the revision, not the incumbent (trial events: $(tr -d '\r' < "$D/skill-usage/revisions.jsonl" 2>/dev/null | py -c "import json,sys; print([json.loads(l)[\"event\"] for l in sys.stdin if l.strip()])"))" '[ "${rolled:-0}" -ge 1 ] && [ "$(skill_text)" = "$INCUMBENT" ] && [ "$(usage "u.get(\"worked\")")" = 3 ] || [ "${superseded:-0}" -ge 1 ]'
  check "ha learning report counts how the revision was settled" '"$HA" learning report 2>&1 | tee "$RX/report.txt" | grep -qiE "[1-9][0-9]* (rolled back|accepted)"'
fi

# --- 3. `ha learning prune` shortens a long proven skill, on trial as a prune.
# A home of its own, so sections 1 and 2 keep their evidence.
setup skill-prune
LONG=release-notes
mkdir -p "$HA_HOME/skills/$LONG"
{
  printf -- '---\nname: %s\ndescription: Write the release notes of this project from its git log, grouped by kind of change.\n---\n' "$LONG"
  for step in "Run git log from the last tag to HEAD with run_process." \
              "Read every commit subject in the output carefully." \
              "Group the commits by their prefix: feat, fix, docs, test, chore." \
              "Write one section per group, in that order." \
              "Write each commit as one bullet, its subject without the prefix."; do
    printf '%s\n' "- $step" "- Remember: $step" "- It is important that you always do this: $step" "- Check again that you did this: $step" "- Do not forget: $step"
  done
  printf '%s\n' "- Give the user the notes."
} > "$HA_HOME/skills/$LONG/SKILL.md"
echo '{"created_by": "ha", "created": "2026-10-01T00:00:00Z"}' > "$HA_HOME/skills/$LONG/.ha-learned.json"
cat > "$D/skill-usage/global.json" <<EOF
{"requests": 20, "skills": {"$LONG": {"offered": 4, "use": 4, "worked": 4, "last": "2026-11-12T00:00:00Z"}}}
EOF
before_lines=$(grep -cv '^\s*$' "$HA_HOME/skills/$LONG/SKILL.md")
before_chars=$(wc -c < "$HA_HOME/skills/$LONG/SKILL.md")
(cd "$P" && "$HA" learning prune --skill "$LONG" --model "$MODEL" "${TH[@]}" > "$RX/prune.txt" 2>&1)
after_lines=$(grep -cv '^\s*$' "$HA_HOME/skills/$LONG/SKILL.md")
after_chars=$(wc -c < "$HA_HOME/skills/$LONG/SKILL.md")
SKILL=$LONG
check "ha learning prune ran ($RX/prune.txt)" 'grep -qi "prune" "$RX/prune.txt" && ! grep -qi "unrecognized\|unknown command\|error:" "$RX/prune.txt"'
check "the pruned skill is shorter ($before_lines -> $after_lines non-blank lines, $before_chars -> $after_chars bytes)" '[ "$after_lines" -lt "$before_lines" ] && [ "$after_chars" -lt "$before_chars" ]'
check "the pruned skill keeps its steps (git log, the groups, one bullet per commit)" 'grep -qi "git log" "$HA_HOME/skills/$LONG/SKILL.md" && grep -qi "feat" "$HA_HOME/skills/$LONG/SKILL.md" && grep -qi "bullet" "$HA_HOME/skills/$LONG/SKILL.md"'
check "the prune is on trial with the objective prune ($(usage 'u.get("trial", {}).get("objective")'))" '[ "$(usage "u.get(\"trial\", {}).get(\"objective\")")" = prune ]'
check "the prune is an edit (ledger: $(ledger_actions))" 'ledger_actions | grep -qE "edit *$"'

echo "   ha: $HA ($(sha256sum "$HA" | cut -c1-16))"
result
