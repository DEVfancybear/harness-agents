#!/usr/bin/env bash
# What the automatic review is told of what went wrong before it plans again
# (after GUARD, arXiv 2611.23476, and SIM, arXiv 2611.18856;
# `docs/architecture/SELF_IMPROVEMENT_GATE.en.md`), with a real model through
# `ha exec` and the review after every turn:
#
#   1. G1, the ledger of refused edits: the review's memory edit about a
#      project that keeps its memory in markdown files is refused by the host;
#      the refusal is kept in the scope's `refused.json` (kind, target, reason,
#      a redacted summary, the quote) and in a `refine refusal_kept`
#      diagnostics record; the next review of the conversation plans with a
#      `<refused_before>` part holding it.
#   2. G1b and G2, a skill's revision memory and its standing: with a skill
#      that is mastered (worked three times, its cases kept, an accepted
#      revision in its history) and one whose revision on trial failed a use
#      while its incumbent was sound (regressed), the review plans with both
#      standings, the mastered skill's "worked before" lines and the revision
#      lines of both histories; and a revision the review makes of a proven
#      skill keeps the cause and the expectation it was made for in its trial.
#
# What the planner was given is not stored (the upkeep calls leave only their
# usage), so it is read from the `refine planner_given` diagnostics record the
# host writes for each plan: the names of the prompt's parts and counts, never
# the words. The state a check reads is the host's own files in $LIVE_ROOT/<test>.
# Where the model must act (propose an edit, revise a skill) the checks judge
# the feature only when the session's own record shows the model did (a
# "precondition:" check); a turn that did not is tried again, up to three
# times, and a precondition that never held is a FAIL by its name, never a
# pass. A comment above a check says what 0.9.14 does with it: "0.9.14 FAILS"
# is the feature, "passes on 0.9.14 too" a precondition or a control. The
# control is this script with HA=<0.9.14's ha.exe>.
#
#   bash scripts/testing/live-tests/guard.sh
set -u
source "$(dirname "$0")/env.sh"

winpath() { cygpath -m "$1" 2>/dev/null || printf '%s' "$1"; }
py() { "$PY" "$@" | tr -d '\r'; }

# One `ha exec` turn in $P: `run_exec <name> [flags] <prompt>`.
run_exec() {
  local name="$1"; shift
  printf '\n## %s (ha exec %s)\n\n> %s\n\n' "$(date +%H:%M:%S)" "$name" "${*: -1}" >> "$LOG"
  (cd "$P" && "$HA" exec --model "$MODEL" "${TH[@]}" --approval full-auto --output-format json "$@" >"$RX/$name.json" 2>"$RX/$name.err")
  answer "$name" >> "$LOG"
}
answer() { "$PY" -c "import json,sys; print(json.load(open(sys.argv[1],encoding='utf-8')).get('response',''))" "$(winpath "$RX/$1.json")" 2>/dev/null; }
jfield() { "$PY" -c "import json,sys; print(json.load(open(sys.argv[1],encoding='utf-8')).get(sys.argv[2]) or '')" "$(winpath "$RX/$1.json")" "$2" 2>/dev/null | tr -d '\r'; }
# 0 when the provider gave no answer to the turn: not the model skipping a step.
provider_refused() {
  [ -n "$(answer "$1")" ] && return 1
  echo "   the provider gave no answer for $1: $(head -c 300 "$RX/$1.err" | tr '\n' ' ')"
  return 0
}

# Review after every turn, the thinking level fresh() set kept, nothing else
# that writes memory or skills on its own.
setup() { # setup <name>
  fresh "$1"
  export HA_DAEMON=off HA_SELF_EVAL=off HA_SESSION_LOG=off HA_REPL=off HA_CODEMODE=off HA_ACTION_BANK=off HA_STEP_JUDGE=off
  unset HA_AUTO_REFINE
  D=$HA_HOME/data
  P=$RX/p
  project "$P"
  "$PY" - "$(winpath "$HA_HOME/settings.json")" <<'PYEOF'
import json, os, sys
path = sys.argv[1]
settings = json.load(open(path, encoding="utf-8")) if os.path.exists(path) else {}
settings["autoRefine"] = {"turnInterval": 1}
json.dump(settings, open(path, "w", encoding="utf-8"))
PYEOF
}

# The `fields` of the diagnostics records of a kind and event at or after a
# time (ms), one JSON object a line: `diag_fields <kind> <event> <since_ms>`.
diag_fields() {
  cat "$D"/diagnostics/*.jsonl 2>/dev/null | "$PY" -c "
import json, sys
kind, event, since = sys.argv[1], sys.argv[2], int(sys.argv[3])
for line in sys.stdin:
    try:
        r = json.loads(line)
    except ValueError:
        continue
    if r.get('kind') == kind and r.get('event') == event and r.get('ms', 0) >= since:
        print(json.dumps(r.get('fields', {})))
" "$1" "$2" "$3" | tr -d '\r'
}
now_ms() { "$PY" -c "import time; print(int(time.time()*1000))" | tr -d '\r'; }
# given_has <python condition over f, one record's fields>: some record of
# $given (the planner_given lines) satisfies it.
given_has() {
  printf '%s' "$given" | "$PY" -c "
import json, sys
fields = [json.loads(line) for line in sys.stdin if line.strip()]
sys.exit(0 if any(($1) for f in fields) else 1)"
}

echo "################ 1. G1: the host's refusal of a memory edit is kept and told to the next review"
# refine.rs `apply` -> memory_refused::keep: a memory or prompt edit the host
# refused (not skipped, not waiting for the person) is kept in the scope's
# refused.json beside its state; `plan` gives the next planner a
# <refused_before> part. The refusal chosen is the host's own, deterministic
# once the review proposes the edit: the project keeps its memory in markdown
# files (`ha memory init`), so a memory edit about it is refused
# (MARKDOWN_STORE_REFUSAL). 0.9.14 keeps the reason in the refinement's record
# and the diagnostics only.
FACT1="Here is a fact about this repository for later, so remember it: its release branch is called stable-9, and the maintainers cut every release tag from that branch and never from main. Do not write any file now; only tell me that you have noted it."
FACT2="One more fact about this repository, to remember: the changelog is generated by the release script and nobody edits it by hand. Do not write any file now; only tell me that you have noted it."
# refused_store_edits: the memory edits of this home's reviews the host refused
# for the markdown store.
refused_store_edits() { "$PY" - "$D" <<'PYEOF' | tr -d '\r'
import glob, json, os, sys
count = 0
for path in glob.glob(os.path.join(sys.argv[1], "**", "refinements.jsonl"), recursive=True):
    for line in open(path, encoding="utf-8", errors="replace"):
        try:
            record = json.loads(line)
        except ValueError:
            continue
        for edit in record.get("appliedEdits") or []:
            if edit.get("applied") is False and str(edit.get("error", "")).startswith("this project keeps its memory in markdown files"):
                count += 1
print(count)
PYEOF
}
# ledger_ok: refused.json of some scope holds the refusal, with what the
# ledger keeps of it.
ledger_ok() { "$PY" - "$D" <<'PYEOF'
import glob, json, os, sys
for path in glob.glob(os.path.join(sys.argv[1], "**", "refused.json"), recursive=True):
    try:
        records = json.load(open(path, encoding="utf-8")).get("refused", [])
    except (OSError, ValueError):
        continue
    for r in records:
        if r.get("kind") == "memory" and r.get("reason", "").startswith("this project keeps its memory") and r.get("target") and r.get("summary") and r.get("at"):
            sys.exit(0)
sys.exit(1)
PYEOF
}
held1=""
for attempt in 1 2 3; do
  setup guard-ledger
  (cd "$P" && "$HA" memory init > "$RX/init.out" 2>&1)
  run_exec a1 "$FACT1"
  if provider_refused a1; then continue; fi
  refused_now=$(refused_store_edits)
  echo "   attempt $attempt: the host refused $refused_now memory edit(s) for the markdown store"
  if [ "${refused_now:-0}" -ge 1 ]; then held1=$attempt; break; fi
done
# Passes on 0.9.14 too: the review proposed a memory edit about the project and
# the host refused it (the model's to propose it).
check "precondition: the review's memory edit about the project was refused for the markdown store (attempt ${held1:-none})" '[ -n "$held1" ]'
if [ -n "$held1" ]; then
  # 0.9.14 FAILS: no ledger file, no record.
  check "G1: the refusal is in the scope's refused.json: a memory edit, the reason, its target, a summary and the time" 'ledger_ok'
  # 0.9.14 FAILS: no such record.
  check "G1: a refine refusal_kept diagnostics record shows it" '[ -n "$(diag_fields refine refusal_kept 0)" ]'
  task=$(jfield a1 task_id)
  planned=""
  for attempt in 1 2 3; do
    since=$(now_ms)
    run_exec a2 --resume "$task" "$FACT2"
    if provider_refused a2; then continue; fi
    given=$(diag_fields refine planner_given "$since")
    echo "   second review, attempt $attempt: planner_given=${given:-none}"
    if [ -n "$given" ]; then planned=$attempt; break; fi
  done
  check "precondition: the second turn's review reached the planner (attempt ${planned:-none})" '[ -n "$planned" ]'
  if [ -n "$planned" ]; then
    # 0.9.14 FAILS: no planner_given record, and no <refused_before> part.
    check "G1: the second review's planner was given a <refused_before> part holding the refusal" \
      'given_has "\"refused_before\" in f.get(\"parts\", \"\").split(\",\") and f.get(\"refused_before\", 0) >= 1"'
  fi
fi

echo "################ 2. G1b and G2: a skill's standing and its revision history reach the planner"
# learned.rs `LearnedSkill::standing` and `library_overview`: each learned skill
# is marked regressed (its revision on trial failed a judged use and the
# incumbent was sound), unsolved or mastered (worked twice or more, no failure
# since its last revision), a mastered skill's cases are shown as "worked
# before:", and each skill's history is shown as "revised <date> (<how it
# ended>): cause: ...; expected: ...; observed: n good, m bad". Seeded in the
# usage record, as skill-trial.sh seeds a proven skill; the review runs on a
# neutral conversation. 0.9.14 has no standing, no such lines and no
# planner_given record.
MASTERED=build-notes
REGRESSED=weekly-report
seed_skills() {
  mkdir -p "$HA_HOME/skills/$MASTERED" "$HA_HOME/skills/$REGRESSED" "$D/skill-usage"
  printf '%s\n' "---
name: $MASTERED
description: Write this project's build notes: list the targets of its Makefile.
---
1. Read the Makefile with read_file.
2. List each target on a line of its own." > "$HA_HOME/skills/$MASTERED/SKILL.md"
  printf '%s\n' "---
name: $REGRESSED
description: Make this project's weekly sales report: run its report script and give the user the total it prints.
---
1. Run the report script with run_process: executable python, args [\"report.py\", \"--summary-only\"].
2. Give the user the line that starts with TOTAL, as printed." > "$HA_HOME/skills/$REGRESSED/SKILL.md"
  for skill in "$MASTERED" "$REGRESSED"; do
    echo '{"created_by": "ha", "created": "2026-10-01T00:00:00Z"}' > "$HA_HOME/skills/$skill/.ha-learned.json"
  done
  cat > "$D/skill-usage/global.json" <<EOF
{"requests": 20, "skills": {
  "$MASTERED": {"offered": 4, "use": 4, "worked": 3, "trial_good": 3, "trial_bad": 0, "last": "2026-11-12T00:00:00Z",
    "cases": [{"at": "2026-11-10T00:00:00Z", "evidence": "the Makefile lists build, test and lint"},
              {"at": "2026-11-11T00:00:00Z", "evidence": "TARGETS: build test lint"}],
    "revisions": [{"at": "2026-11-05T00:00:00Z", "kind": "accepted", "ledger_id": "chg-1", "edits_summary": "insert_after \"1. Read the Makefile\"",
      "cause": "the skill listed targets without reading the Makefile first", "expect": "the targets listed are the ones in the file", "good": 2, "bad": 0}]},
  "$REGRESSED": {"offered": 5, "use": 5, "worked": 2, "failed": 1, "trial_good": 0, "trial_bad": 1, "last": "2026-11-12T00:00:00Z",
    "failures": [{"at": "2026-11-12T00:00:00Z", "assumption": "report.py takes --summary-only", "evidence": "exit=Some(2)"}],
    "trial": {"action": "edit", "objective": "repair", "ledger_id": "chg-2", "ledger_ids": ["chg-2"], "began": "2026-11-11T00:00:00Z",
      "incumbent": {"worked": 2, "failed": 0, "checked_pass": 0, "checked_fail": 0, "trial_good": 2, "trial_bad": 0},
      "edits_summary": "replace the report command with one that passes --summary-only",
      "cause": "the user wanted only the summary line", "expect": "the report script is run with the summary flag and prints less",
      "undo": {}}}}}
EOF
}
held2=""
for attempt in 1 2 3; do
  setup guard-standing
  seed_skills
  since=$(now_ms)
  run_exec s1 "Here is a fact about this repository for later: its tests are run with make test, never with a bare runner. Do not write any file now; only tell me that you have noted it."
  if provider_refused s1; then continue; fi
  given=$(diag_fields refine planner_given "$since")
  echo "   attempt $attempt: planner_given=${given:-none}"
  if [ -n "$given" ]; then held2=$attempt; break; fi
done
# Passes on 0.9.14 too only as far as the review runs; it writes no planner_given there.
check "precondition: the review reached the planner with the two skills in the library (attempt ${held2:-none})" '[ -n "$held2" ]'
if [ -n "$held2" ]; then
  # 0.9.14 FAILS: no standing is computed.
  check "G2: the planner was given $REGRESSED as regressed and $MASTERED as mastered" \
    'given_has "\"$REGRESSED=regressed\" in f.get(\"standings\", \"\") and \"$MASTERED=mastered\" in f.get(\"standings\", \"\")"'
  # 0.9.14 FAILS: a mastered skill's cases were not shown.
  check "G2: the mastered skill's cases were shown as worked-before lines" \
    'given_has "f.get(\"worked_before\", 0) >= 1"'
  # 0.9.14 FAILS: the library showed only the refused and rolled-back revisions;
  # an accepted one and the one on trial were not shown.
  check "G1b: the library showed the accepted revision and the one on trial as revised lines" \
    'given_has "f.get(\"revised\", 0) >= 2"'
fi

# The revision the review makes keeps the cause and the expectation it was made
# for. A proven skill the user corrects is revised with an edit (skill-trial.sh);
# the trial in its usage record holds the cause and the expectation the planner
# gave. A round in which the review did not revise the skill is tried again.
SKILL=weekly-report
INCUMBENT="---
name: $SKILL
description: Make this project's weekly sales report: run its report script and give the user the total it prints.
---
1. Run the report script with run_process: executable \`python\`, args [\"report.py\"], in the project root.
2. Give the user the line that starts with TOTAL, as printed."
CORRECT="A correction for the learned skill $SKILL, for next time: the report script must now be run with the --summary-only flag, as python report.py --summary-only. Do not run anything now; only say you have noted it."
trial_field() { py -c "
import json, sys
try:
    u = json.load(open(sys.argv[1], encoding='utf-8'))['skills'].get(sys.argv[2], {})
except Exception:
    u = {}
print(u.get('trial', {}).get(sys.argv[3], ''))" "$(winpath "$D/skill-usage/global.json")" "$SKILL" "$1"; }
revised=""
for attempt in 1 2 3; do
  setup guard-revision
  mkdir -p "$HA_HOME/skills/$SKILL" "$D/skill-usage"
  printf '%s\n' "$INCUMBENT" > "$HA_HOME/skills/$SKILL/SKILL.md"
  echo '{"created_by": "ha", "created": "2026-10-01T00:00:00Z"}' > "$HA_HOME/skills/$SKILL/.ha-learned.json"
  cat > "$D/skill-usage/global.json" <<EOF
{"requests": 12, "skills": {"$SKILL": {"offered": 3, "use": 3, "worked": 3, "last": "2026-11-10T00:00:00Z",
  "cases": [{"at": "2026-11-10T00:00:00Z", "evidence": "TOTAL: 965"}]}}}
EOF
  run_exec r1 "$CORRECT"
  if provider_refused r1; then continue; fi
  if tr -d '\r' < "$HA_HOME/skills/$SKILL/SKILL.md" | grep -q -- "--summary-only" && [ -n "$(trial_field ledger_id)" ]; then revised=$attempt; break; fi
  echo "   attempt $attempt: the review did not revise $SKILL"
done
check "precondition: the review revised the proven skill and the revision is on trial (attempt ${revised:-none})" '[ -n "$revised" ]'
if [ -n "$revised" ]; then
  echo "   cause: $(trial_field cause)"
  echo "   expect: $(trial_field expect)"
  # 0.9.14 FAILS: the planner was not asked for them and the trial keeps none.
  check "G1b: the trial keeps the cause the revision addressed and what it expects" '[ -n "$(trial_field cause)" ] && [ -n "$(trial_field expect)" ]'
fi

echo "evidence: $LIVE_ROOT/guard-ledger, $LIVE_ROOT/guard-standing, $LIVE_ROOT/guard-revision (conversation.md, the *.json answers, the state in home/data)"
echo "   ha: $HA ($(sha256sum "$HA" | cut -c1-16))"
result
