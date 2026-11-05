#!/usr/bin/env bash
# Memory on against memory off (0.9.0), with numbers: does what ha remembers
# make a task succeed, and what does it cost?
#
# 1. Teaching: in one `ha exec` conversation the user states four facts no
#    file holds - a deploy target, the CI suite's name, a rule (never edit
#    vendor/), a preference (a first line for new Python files) - each with a
#    random token, and the agent finds a fifth by tool (a test passes only
#    with an environment variable set to a value in a config file). The
#    reviews run after every run (autoRefine.turnInterval 1) and finish
#    before it exits; the precondition is that the stores (`ha memory
#    export`, the markdown store, .harness, the lessons) hold each fact by its
#    token, the teaching restated up to twice when they do not. Two more are
#    set up for the features of wave 2: the agent keeps the HTTP client's
#    retry limit with a citation of the line that sets it (app/net.py), which
#    the test then changes, so the kept fact is stale; and the markdown store
#    has a file kept for app/billing/** (a `paths:` frontmatter, not in the
#    index) naming a constant every billing module defines.
# 2. Evaluation: seven tasks whose success depends on a taught fact, judged
#    from a file the agent wrote - `retries` on noticing the kept fact went
#    stale (the citation), `ledger` on the billing file's rule (memory scoped
#    to paths) - and two that do not (their cost only), each
#    in a fresh `ha exec` conversation once per mode and repeat: memory ON,
#    memory OFF (HA_MEMORY=off), and with ABLATE=1 each feature of
#    HA_MEMORY_OFF off alone. Every run starts from a copy of the same
#    snapshot of the home's data and the project, with the background writes
#    off (HA_AUTO_REFINE, HA_SELF_EVAL, HA_SESSION_LOG), so no run learns from
#    another.
# 3. The report: $RX/memory-ab.json and $RX/memory-ab.md, a table per mode
#    (success, mean input / cached / output tokens, model calls, tool calls,
#    seconds, and how often a run reached earlier work by a tool: a memory
#    file, or another conversation's journal) with the deltas against OFF,
#    printed below.
#
# The checks judge the mechanics only: an ON request holds the digest and the
# markdown index (the precondition of the next), an OFF request holds no
# digest, markdown store, session brief or earlier state; an OFF run leaves
# every memory store file as it was; every run reports its usage; the report
# has every mode's numbers. The success and cost deltas are reported, not
# judged. All but the precondition fail on 0.8.9, which has no switch and no
# usage in its JSON.
#
#   REPEATS=2 (default) TASKS="deploy ci vendor owner region median explain retries ledger"
#   ABLATE=1 [ABLATE_FEATURES="digest markdown ..."]
#   bash scripts/testing/live-tests/memory-ab.sh
# The default run is 9 tasks x 2 modes x 2 repeats = 36 evaluation runs and
# 3-5 teaching runs.
set -u
source "$(dirname "$0")/env.sh"
fresh memory-ab
AB="$(cd "$(dirname "$0")" && (pwd -W 2>/dev/null || pwd))/memory_ab.py"
REPEATS=${REPEATS:-2}
TASKS=${TASKS:-deploy ci vendor owner region median explain retries ledger}
# One process per run: the data a run starts from is copied in place, and a
# background worker would hold it open.
export HA_DAEMON=off HA_PYTHON=${HA_PYTHON:-$PY}
D="$HA_HOME/data"
P="$RX/p"
started=$(date +%s)
runs=0

tok() { "$PY" -c "import secrets; print(secrets.token_hex(3))"; }
T1="stg-$(tok)" T2="ci-$(tok)" T3="textkit-$(tok)" T4="team-$(tok)" T5="r-$(tok)" T6="ldg-$(tok)"
printf 'deploy %s\nci %s\nvendor %s\nowner %s\nregion %s\nledger %s\n' "$T1" "$T2" "$T3" "$T4" "$T5" "$T6" > "$RX/tokens.txt"

# The fixture: a small Python project with its tests, a vendored module, a
# config holding the region the tests want, and a markdown memory store.
project "$P"
"$PY" - "$P" "$T5" "r-$(tok)" "r-$(tok)" <<'EOF'
import os, sys
root, region, other1, other2 = sys.argv[1:]
files = {
    "app/__init__.py": "",
    "app/calc.py": 'def total(items):\n    """The sum of the amounts of (name, amount) pairs."""\n    return sum(amount for _, amount in items)\n',
    "app/text.py": "from vendor.textkit import slugify\n\n\ndef title_slug(title):\n    return slugify(title)\n",
    "vendor/__init__.py": "",
    "vendor/textkit.py": "def slugify(text):\n    return text.strip().lower()\n",
    "tests/__init__.py": "",
    "tests/test_calc.py": "import unittest\n\nfrom app.calc import total\n\n\nclass Total(unittest.TestCase):\n    def test_sums_the_amounts(self):\n        self.assertEqual(total([('a', 2), ('b', 3)]), 5)\n",
    "tests/test_env.py": (
        "import configparser\nimport os\nimport pathlib\nimport unittest\n\n\n"
        "def default_code():\n    config = configparser.ConfigParser()\n"
        "    config.read(pathlib.Path(__file__).parent.parent / 'config' / 'regions.ini')\n"
        "    for section in config.sections():\n"
        "        if config[section].getboolean('default', fallback=False):\n"
        "            return config[section]['code']\n\n\n"
        "class Region(unittest.TestCase):\n    def test_the_region_is_set(self):\n"
        "        self.assertEqual(os.environ.get('LEDGER_REGION'), default_code())\n"
    ),
    "config/regions.ini": f"[eu-west]\ncode = {other1}\n\n[ap-south]\ncode = {region}\ndefault = true\n\n[us-east]\ncode = {other2}\n",
    "app/net.py": "MAX_RETRIES = 3\n\n\ndef attempts():\n    \"\"\"How many times a failed request is sent again.\"\"\"\n    return MAX_RETRIES\n",
    "app/billing/__init__.py": "",
    "app/billing/invoice.py": "def total_cents(lines):\n    \"\"\"The sum of the (name, cents) lines of an invoice.\"\"\"\n    return sum(cents for _, cents in lines)\n",
    "run_tests.py": (
        "import argparse\nimport sys\nimport unittest\n\n"
        "parser = argparse.ArgumentParser()\nparser.add_argument('--suite', required=True)\n"
        "suite = parser.parse_args().suite\nprint(f'suite {suite}')\n"
        "result = unittest.TextTestRunner().run(unittest.defaultTestLoader.discover('tests'))\n"
        "sys.exit(0 if result.wasSuccessful() else 1)\n"
    ),
}
for path, text in files.items():
    full = os.path.join(root, path)
    os.makedirs(os.path.dirname(full), exist_ok=True)
    open(full, "w", encoding="utf-8", newline="\n").write(text)
EOF
(cd "$P" && "$HA" memory init > "$RX/init.out" 2>&1)
# A memory file kept for app/billing/**: the index does not route to it, so
# only memory scoped to paths brings it to a turn.
mkdir -p "$P/memory/projects"
printf -- '---\npaths: ["app/billing/**"]\n---\n# Billing\n\n- Every module under app/billing/ defines LEDGER = "%s" at module level: the ledger it posts to. [stated]\n' "$T6" > "$P/memory/projects/billing.md"
(cd "$P" && git add -A && git -c user.name=t -c user.email=t@t commit -qm fixture)
# A review after every run while teaching.
"$PY" - "$HA_HOME/settings.json" <<'EOF'
import json, os, sys
path = sys.argv[1]
settings = json.load(open(path, encoding="utf-8")) if os.path.exists(path) else {}
settings["autoRefine"] = {"enabled": True, "turnInterval": 1}
json.dump(settings, open(path, "w", encoding="utf-8"))
EOF

# exec_run <name> <prompt> [exec args...]: one `ha exec` in the project.
exec_run() {
  local name="$1" prompt="$2"; shift 2
  printf '\n## %s (ha exec %s)\n\n> %s\n\n' "$(date +%H:%M:%S)" "$name" "$prompt" >> "$LOG"
  (cd "$P" && "$HA" exec --model "$MODEL" "${TH[@]}" --approval full-auto --output-format json "$@" "$prompt" \
    > "$RX/$name.json" 2> "$RX/$name.err")
  runs=$((runs + 1))
  field "$RX/$name.json" response >> "$LOG"
  sed 's/^/    /' "$RX/$name.err" >> "$LOG"
}
field() { "$PY" -c "import json,sys; v=json.load(open(sys.argv[1],encoding='utf-8')).get(sys.argv[2]); print('' if v is None else v)" "$1" "$2" 2>/dev/null; }
# The review records of one conversation, as `event` lines.
reviews_of() {
  "$PY" - "$D/diagnostics" "$1" <<'EOF'
import glob, json, os, sys
for path in glob.glob(os.path.join(sys.argv[1], "*.jsonl")):
    for line in open(path, encoding="utf-8", errors="replace"):
        try:
            record = json.loads(line)
        except ValueError:
            continue
        if record.get("kind") == "review" and record.get("task") == sys.argv[2]:
            print(record.get("event"))
EOF
}
# Everything the stores hold, as text: the global memory, the markdown store,
# the project's .harness (session logs, knowledge, learned skills), the lessons.
stores_text() {
  {
    (cd "$P" && "$HA" memory export 2>/dev/null)
    cat "$P/MEMORY.md" 2>/dev/null
    find "$P/memory" "$P/.harness" -type f 2>/dev/null | while read -r file; do cat "$file"; done
    cat "$D/harness/lessons.json" 2>/dev/null
  } > "$RX/stores.txt"
}
missing_tokens() {
  stores_text
  local held
  held=$("$PY" "$AB" held "$RX/stores.txt" "$T1" "$T2" "$T3" "$T4" "$T5")
  for token in "$T1" "$T2" "$T3" "$T4" "$T5"; do
    printf '%s\n' "$held" | grep -qx -- "$token" || printf '%s\n' "$token"
  done
}

echo "################ 1. teaching: four facts the user states, one the agent finds by tool"
exec_run teach1 "Facts about this project that are written nowhere in the repo - keep them for future sessions: 1. Our staging deploy target is called $T1. 2. CI runs the test suite named $T2: python run_tests.py --suite $T2. 3. Never edit anything under vendor/ - it is synced from the upstream repository $T3; a change it needs goes in app/ instead. 4. My preference: every new Python file starts with the line '# owner: $T4'. Reply briefly."
TASK=$(field "$RX/teach1.json" task_id)
exec_run teach2 "Run this repo's tests (python -m unittest discover -s tests) and find out what it takes for all of them to pass, without changing any file. Tell me the exact command line that makes every test pass, and keep what you learned for future sessions." --resume "$(field "$RX/teach1.json" session_id)"
for attempt in 1 2; do
  missing=$(missing_tokens | tr '\n' ' ')
  [ -z "${missing// /}" ] && break
  echo "   attempt $attempt: the stores lack $missing; the teaching is restated"
  restated=""
  for token in $missing; do
    case "$token" in
      "$T1") restated="$restated The staging deploy target is $T1." ;;
      "$T2") restated="$restated CI runs python run_tests.py --suite $T2." ;;
      "$T3") restated="$restated Never edit vendor/: it is synced from the upstream repository $T3; changes go in app/." ;;
      "$T4") restated="$restated Every new Python file starts with the line '# owner: $T4'." ;;
      "$T5") restated="$restated The tests pass only with LEDGER_REGION set to the default region's code in config/regions.ini ($T5)." ;;
    esac
  done
  exec_run "teach-again$attempt" "Make sure each of these is kept in memory for future sessions:$restated" --resume "$(field "$RX/teach2.json" session_id)"
done
# The retry limit, kept with a citation of the line that sets it; the test
# then changes that line, so the kept fact is stale.
for attempt in 1 2 3; do
  exec_run "teach-cite$attempt" "Use the memory tool action write with exactly these arguments: scope 'global', path 'project/net', title 'HTTP retry limit', content 'The HTTP client in app/net.py retries a failed request at most 3 times.', citations [{\"path\": \"app/net.py\", \"quote\": \"MAX_RETRIES = 3\"}]. Do nothing else. Reply: saved."
  (cd "$P" && "$HA" memory export 2>/dev/null) | grep -q '"quote": *"MAX_RETRIES = 3"' && break
done
(cd "$P" && "$HA" memory export 2>/dev/null) > "$RX/cited.json"
check "precondition: the retry limit is kept with its citation" 'grep -q "\"quote\": *\"MAX_RETRIES = 3\"" "$RX/cited.json"'
sed -i 's/^MAX_RETRIES = 3$/MAX_RETRIES = 7/' "$P/app/net.py"
(cd "$P" && git add -A && git -c user.name=t -c user.email=t@t commit -qm "retry more")
missing=$(missing_tokens | tr '\n' ' ')
reviews=$(reviews_of "$TASK" | sort | uniq -c | tr '\n' ',' )
echo "   the teaching conversation's reviews: ${reviews:-none}"
echo "   tokens the stores lack: ${missing:-none}"
check "precondition: a review of the teaching conversation finished before its run exited" '[ -n "$reviews" ]'
check "precondition: the stores hold every taught fact by its token" '[ -z "${missing// /}" ]'
"$PY" - "$RX" "$runs" <<'EOF'
import glob, json, os, sys
rx = sys.argv[1]
teaching = []
for path in sorted(glob.glob(os.path.join(rx, "teach*.json"))):
    try:
        result = json.load(open(path, encoding="utf-8"))
    except ValueError:
        continue
    teaching.append({"run": os.path.basename(path), "usage": result.get("usage"),
                     "upkeep_usage": result.get("upkeep_usage"), "children_usage": result.get("children_usage")})
json.dump(teaching, open(os.path.join(rx, "teaching.json"), "w", encoding="utf-8"), indent=2)
for run in teaching:
    print("  ", run["run"], "usage", run["usage"], "upkeep", run["upkeep_usage"])
EOF

# The snapshot every evaluation run starts from.
rm -rf "$RX/snap" && mkdir -p "$RX/snap"
cp -r "$D" "$RX/snap/data" && cp -r "$P" "$RX/snap/p"
restore() { rm -rf "$D" "$P" && cp -r "$RX/snap/data" "$D" && cp -r "$RX/snap/p" "$P"; }

prompt_of() {
  case "$1" in
    deploy) echo "Write the name of our staging deploy target into the file deploy/TARGET - the name alone. If you do not know it, write UNKNOWN." ;;
    ci) echo "Create scripts/ci.sh, a bash script that runs our CI test suite the way CI runs it. If you do not know how CI runs it, make the script just echo UNKNOWN." ;;
    vendor) echo "slugify in vendor/textkit.py keeps dashes; they should become spaces. Fix it so that app.text.title_slug('Hello-World Now') returns 'hello world now'." ;;
    owner) echo "Add a new module app/stats.py with a function mean(xs) that returns the arithmetic mean of a non-empty list." ;;
    region) echo "Write into notes/test-command.txt the exact command line that makes every test of this repo pass - one line, nothing else." ;;
    median) echo "Add a function median(xs) to app/calc.py that returns the median of a non-empty list of numbers." ;;
    explain) echo "In one sentence: what does total() in app/calc.py do?" ;;
    retries) echo "Write into notes/retries.txt how many times our HTTP client retries a failed request - the number alone." ;;
    ledger) echo "Add a module app/billing/refunds.py with a function refund_cents(amount_cents, percent) that returns the refunded amount in integer cents, rounded down." ;;
  esac
}
kind_of() { case "$1" in median|explain) echo cost ;; *) echo fact ;; esac; }
token_of() { case "$1" in deploy) echo "$T1" ;; ci) echo "$T2" ;; vendor) echo "$T3" ;; owner) echo "$T4" ;; region) echo "$T5" ;; retries) echo "MAX_RETRIES = 3" ;; ledger) echo "$T6" ;; esac; }
# judge <task> <run json>: whether the run did the task (from what it wrote).
judge() {
  case "$1" in
    deploy) [ "$(tr -d ' \r\n' < "$P/deploy/TARGET" 2>/dev/null)" = "$T1" ] ;;
    ci) grep -q -- "$T2" "$P/scripts/ci.sh" 2>/dev/null ;;
    vendor) [ -z "$(git -C "$P" status --porcelain -- vendor)" ] \
      && (cd "$P" && "$PY" -c "from app.text import title_slug; assert title_slug('Hello-World Now') == 'hello world now'") 2>/dev/null ;;
    owner) head -1 "$P/app/stats.py" 2>/dev/null | grep -q -- "$T4" \
      && (cd "$P" && "$PY" -c "from app.stats import mean; assert mean([1, 2, 3]) == 2") 2>/dev/null ;;
    region) grep -q -- "$T5" "$P/notes/test-command.txt" 2>/dev/null && grep -q LEDGER_REGION "$P/notes/test-command.txt" ;;
    median) (cd "$P" && "$PY" -c "from app.calc import median; assert median([3, 1, 2]) == 2 and median([1, 2, 3, 4]) == 2.5") 2>/dev/null ;;
    explain) [ -n "$(field "$2" response)" ] ;;
    retries) [ "$(tr -d ' \r\n' < "$P/notes/retries.txt" 2>/dev/null)" = 7 ] ;;
    ledger) (cd "$P" && "$PY" -c "from app.billing.refunds import LEDGER, refund_cents; assert LEDGER == '$T6' and refund_cents(1000, 15) == 150") 2>/dev/null ;;
  esac
}

MODES="on off"
if [ "${ABLATE:-}" = 1 ]; then
  for feature in ${ABLATE_FEATURES:-digest markdown session_log lessons reviews earlier_state knowledge citations scoped}; do
    MODES="$MODES ablate:$feature"
  done
fi
mkdir -p "$RX/runs"
: > "$RX/runs.jsonl"
stores_text
cp "$RX/stores.txt" "$RX/stores-taught.txt"

echo "################ 2. evaluation: $(echo $TASKS | wc -w) tasks x $(echo $MODES | wc -w) modes x $REPEATS repeats, each from the snapshot"
for repeat in $(seq 1 "$REPEATS"); do
  for task in $TASKS; do
    for mode in $MODES; do
      name="$mode-$task-$repeat"; name="${name//:/-}"
      restore
      "$PY" "$AB" hashes "$D" "$P" > "$RX/runs/$name.before.json"
      case "$mode" in
        on) memory=() ;;
        off) memory=(HA_MEMORY=off) ;;
        ablate:*) memory=(HA_MEMORY_OFF="${mode#ablate:}") ;;
      esac
      began=$(date +%s)
      printf '\n## %s (%s)\n\n> %s\n\n' "$(date +%H:%M:%S)" "$name" "$(prompt_of "$task")" >> "$LOG"
      (cd "$P" && env HA_AUTO_REFINE=off HA_SELF_EVAL=off HA_SESSION_LOG=off "${memory[@]}" \
        "$HA" exec --model "$MODEL" "${TH[@]}" --approval full-auto --output-format json "$(prompt_of "$task")" \
        > "$RX/runs/$name.json" 2> "$RX/runs/$name.err")
      runs=$((runs + 1))
      seconds=$(( $(date +%s) - began ))
      field "$RX/runs/$name.json" response >> "$LOG"
      if judge "$task" "$RX/runs/$name.json"; then ok=true; else ok=false; fi
      task_id=$(field "$RX/runs/$name.json" task_id)
      "$PY" "$AB" request "$D" "${task_id:-none}" > "$RX/runs/$name.request.json"
      "$PY" "$AB" hashes "$D" "$P" > "$RX/runs/$name.after.json"
      "$PY" "$AB" changed "$RX/runs/$name.before.json" "$RX/runs/$name.after.json" "$task_id" > "$RX/runs/$name.changed"
      changes=$(grep -c . "$RX/runs/$name.changed")
      held=true
      if [ "$(kind_of "$task")" = fact ] && ! grep -qF -- "$(token_of "$task")" "$RX/stores-taught.txt"; then held=false; fi
      reads=$("$PY" -c "import json,sys; d=json.load(open(sys.argv[1],encoding='utf-8')); print(json.dumps(d.get('memory_file_calls',[])))" "$RX/runs/$name.request.json")
      history=$("$PY" -c "import json,sys; d=json.load(open(sys.argv[1],encoding='utf-8')); print(json.dumps(d.get('history_project_calls',[])))" "$RX/runs/$name.request.json")
      "$PY" "$AB" add "$RX/runs.jsonl" "mode=\"$mode\"" "task=\"$task\"" "kind=\"$(kind_of "$task")\"" "repeat=$repeat" \
        "ok=$ok" "held=$held" "seconds=$seconds" "json=\"$(cd "$RX/runs" && (pwd -W 2>/dev/null || pwd))/$name.json\"" \
        "task_id=\"$task_id\"" "store_changes=$changes" "memory_file_calls=$reads" "history_project_calls=$history"
      echo "   $name: $([ "$ok" = true ] && echo pass || echo fail) in ${seconds}s, $changes store file(s) changed"
    done
  done
done

echo "################ 3. the report"
"$PY" "$AB" report "$RX" | tee "$RX/report.out"
# Every run's request and store changes, for the checks.
"$PY" - "$RX" <<'EOF' > "$RX/mechanics.json"
import glob, json, os, sys
rx = sys.argv[1]
out = {}
for line in open(os.path.join(rx, "runs.jsonl"), encoding="utf-8"):
    run = json.loads(line)
    name = os.path.basename(run["json"])[:-len(".json")]
    request = json.load(open(os.path.join(rx, "runs", name + ".request.json"), encoding="utf-8"))
    try:
        usage = json.load(open(run["json"], encoding="utf-8")).get("usage")
    except (OSError, ValueError):
        usage = None
    out[name] = {"mode": run["mode"], "request": request, "store_changes": run["store_changes"], "usage": usage}
print(json.dumps(out))
EOF
mechanics() { "$PY" -c "import json,sys; d=json.load(open(sys.argv[1],encoding='utf-8')); sys.exit(0 if ($1) else 1)" "$RX/mechanics.json"; }
check "an ON request holds the memory digest and the markdown index" \
  'mechanics "any(r[\"mode\"]==\"on\" and r[\"request\"].get(\"digest\") and r[\"request\"].get(\"markdown_index\") for r in d.values())"'
check "an OFF request holds no digest, markdown store, session brief or earlier state" \
  'mechanics "[r for r in d.values() if r[\"mode\"]==\"off\"] and all(r[\"request\"].get(\"found\") and not any(r[\"request\"].get(k) for k in (\"digest\",\"markdown_policy\",\"markdown_index\",\"session_brief\",\"session_log\",\"earlier_state\")) for r in d.values() if r[\"mode\"]==\"off\")"'
check "an OFF run leaves every memory store file as it was" \
  'mechanics "[r for r in d.values() if r[\"mode\"]==\"off\"] and all(r[\"store_changes\"]==0 for r in d.values() if r[\"mode\"]==\"off\")"'
check "every run reports its usage, its cache fields included, and it is not zero" \
  'mechanics "all(isinstance(r[\"usage\"], dict) and r[\"usage\"].get(\"input_tokens\",0)>0 and r[\"usage\"].get(\"model_calls\",0)>=1 and \"cached_input_tokens\" in r[\"usage\"] and \"cache_write_tokens\" in r[\"usage\"] for r in d.values())"'
if [ "${ABLATE:-}" = 1 ]; then
  check "an ablation leaves out its own feature: no digest with digest off, no markdown with markdown off" \
    'mechanics "all(not r[\"request\"].get(\"digest\") for r in d.values() if r[\"mode\"]==\"ablate:digest\") and all(not r[\"request\"].get(\"markdown_index\") and r[\"request\"].get(\"digest\") for r in d.values() if r[\"mode\"]==\"ablate:markdown\")"'
fi
check "the report has every mode's success and token means" \
  '"$PY" -c "import json,sys; d=json.load(open(sys.argv[1],encoding=\"utf-8\")); sys.exit(0 if all(d[\"modes\"].get(m,{}).get(\"input_tokens\") is not None for m in (\"on\",\"off\")) and \"delta_vs_off\" in d[\"modes\"][\"on\"] else 1)" "$RX/memory-ab.json"'
took=$(( $(date +%s) - started ))
echo "################ memory-ab took $((took / 60)) min $((took % 60)) s, $runs model runs (ha exec)"
result
