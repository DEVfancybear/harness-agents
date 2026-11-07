#!/usr/bin/env bash
# The knowledge library as a wiki, and history in one search, with a real
# model (docs/architecture/KNOWLEDGE_WIKI.en.md, MEMORY_STORE.en.md §7): a
# seeded library with a link to a missing page, two pages on one subject, a
# page whose claim its workspace source contradicts and more than twelve
# pages; `/knowledge status` and a read naming the dangling link; the wiki
# pass (made due now with HA_WIKI_DUE_NOW) repointing the link, merging the
# pair with the link that pointed at the absorbed page rewritten and the
# absorbed page kept in the history, writing an index page of links and
# headings only, fixing the claim from its source; status then showing zero
# dangling links and the size ratio; an answer that needs two pages the index
# lists; and a memory search with sources history finding a codename said only
# in an earlier conversation. Every check prints PASS or FAIL; the
# conversation and the state stay in $LIVE_ROOT/wiki as evidence.
#
#   bash scripts/testing/live-tests/wiki.sh
#
# WIKI_ONLY=pass|answer|history runs one part (the pass seeds the library the
# answer needs, so `answer` runs the pass too). WIKI_WAIT is how many ten
# seconds the pass is waited for (60); a control run on a build without the
# pass sets it low.
set -u
source "$(dirname "$0")/env.sh"
fresh wiki
D=$HA_HOME/data
P="$RX/p-wiki"
project "$P"
K="$P/.harness/knowledge"
ONLY=${WIKI_ONLY:-}
# The pass is due at the first turn's end, without its day's wait.
export HA_WIKI_DUE_NOW=1
# No automatic review in these few turns: the pass is what changes the pages.
echo '{"autoRefine": {"turnInterval": 1000}}' > "$HA_HOME/settings.json"
if [ -n "${THINKING:-}" ]; then
  printf '{"autoRefine": {"turnInterval": 1000}, "defaultThinkingLevel": "%s"}\n' "$THINKING" > "$HA_HOME/settings.json"
fi

# A page as the store writes it: its metadata line, title, summary, sources.
page() { # page <path> <title> <summary> <sources-json> <body>
  mkdir -p "$(dirname "$K/$1")"
  "$PY" - "$K/$1" "$2" "$3" "$4" "$5" <<'EOF'
import json, sys
path, title, summary, sources, body = sys.argv[1:6]
meta = {"title": title, "summary": summary, "sources": json.loads(sources), "related": []}
text = "<!-- ha-knowledge: " + json.dumps(meta, ensure_ascii=False) + " -->\n# " + title + "\n\n" + summary + "\n\n"
for source in meta["sources"]:
    text += "> Source: " + source + "\n"
if meta["sources"]:
    text += "\n"
text += body + "\n"
open(path, "w", encoding="utf-8", newline="\n").write(text)
EOF
}
status() { (cd "$P" && "$HA" knowledge status --cwd "$P" 2>&1); }
read_page() { (cd "$P" && "$HA" knowledge read "$1" --cwd "$P" 2>&1); }
# The diagnostics records of one kind and event.
records() { cat "$D"/diagnostics/*.jsonl 2>/dev/null | grep "\"kind\":\"$1\"" | grep "\"event\":\"$2\""; }
# The request a turn sent, found by a phrase of its prompt.
request() { "$PY" - "$D" "$1" <<'EOF'
import glob, os, sqlite3, sys
sys.stdout.reconfigure(encoding="utf-8")
found = None
for db in glob.glob(os.path.join(sys.argv[1], "projects", "*", "harness.sqlite3")):
    for (text,) in sqlite3.connect(f"file:{db}?mode=ro", uri=True).execute("select request_json from frozen_requests order by rowid"):
        if sys.argv[2] in text:
            found = text
print(found or "")
EOF
}
# The page history's events of a path as "operation actor".
page_history() { "$PY" - "$K/.ha-history.sqlite3" "$1" <<'EOF'
import os, sqlite3, sys
if not os.path.exists(sys.argv[1]):
    sys.exit(0)
for operation, actor in sqlite3.connect(f"file:{sys.argv[1]}?mode=ro", uri=True).execute(
        "select operation, actor from page_event where path = ? order by at", (sys.argv[2],)):
    print(operation, actor)
EOF
}

# Whether the project's journal holds text matching a regular expression,
# however deep its JSON escapes it (backslashes and quotes are left out).
journal_has() { "$PY" - "$D" "$1" <<'EOF'
import glob, os, re, sqlite3, sys
pattern = re.compile(sys.argv[2])
for db in glob.glob(os.path.join(sys.argv[1], "projects", "*", "harness.sqlite3")):
    for (text,) in sqlite3.connect(f"file:{db}?mode=ro", uri=True).execute("select event_json from events"):
        if pattern.search(text.replace("\\", "").replace('"', "")):
            sys.exit(0)
sys.exit(1)
EOF
}

seed() {
  mkdir -p "$P/docs" "$P/src"
  {
    printf '# Retries\n\nThe API client retries a failed call with exponential backoff: wait one second, then double the wait each time.\nThe wait is capped at thirty seconds.\n\n'
    for n in $(seq 1 30); do printf 'Note %s: a retry is logged with the attempt number, the wait chosen and the status the server answered, so an incident review can tell a slow server from a client that gave up.\n' "$n"; done
  } > "$P/docs/retries.md"
  {
    printf '"""Request limits."""\n\n# The most retries one request gets before it fails.\nMAX_RETRIES = 6\n\n'
    for n in $(seq 1 20); do printf 'LIMIT_%s = %s  # a per-endpoint limit kept for the load tests\n' "$n" "$n"; done
  } > "$P/src/limits.py"
  (cd "$P" && git add -A && git -c user.name=t -c user.email=t@t commit -qm sources)
  page concepts/retry-backoff.md "Retry backoff policy" "Exponential backoff for API client retries" '["docs/retries.md"]' \
    "The API client waits one second after a failed call, then doubles the wait each time."
  page concepts/backoff-for-retries.md "Backoff policy for API client retries" "Exponential backoff of retries" '["docs/retries.md"]' \
    "The wait between retries is capped at thirty seconds."
  page ops/deploy.md "Deploy checklist" "What to check before a deploy" '[]' \
    "Before a deploy, check the client's [retry backoff](../concepts/backoff.md) settings and run the smoke tests."
  page concepts/limits.md "Request limits" "How many times a request is retried" '["src/limits.py"]' \
    "A request is retried at most 4 times (MAX_RETRIES) before it fails."
  page services/billing.md "Billing service" "The service that charges customers" '[]' \
    "The billing service charges customers monthly. It is owned by team Saffron."
  page teams/saffron.md "Team Saffron" "A product team" '[]' \
    "Team Saffron pages its on-call engineer in the Slack channel #saffron-oncall-7Q."
  local topic
  for topic in "Cache keys|The response cache key layout|Cache keys join the tenant id and the request path." \
               "Feature flags|How flags are rolled out|Flags roll out to ten percent of tenants first." \
               "Log retention|How long logs are kept|Application logs are kept for fourteen days." \
               "Search index|How the search index is rebuilt|The search index is rebuilt every night at two." \
               "Image formats|Which image formats are served|Images are served as WebP with a PNG fallback." \
               "Release train|When releases ship|Releases ship every second Tuesday." \
               "Session tokens|How long sessions last|A session token expires after eight hours of idle time." \
               "Email sender|Which address sends mail|Transactional mail is sent from no-reply addresses."; do
    local title=${topic%%|*} rest=${topic#*|}
    local summary=${rest%%|*} body=${rest#*|}
    page "notes/$(printf '%s' "$title" | tr 'A-Z ' 'a-z-').md" "$title" "$summary" '[]' "$body"
  done
}

if [ -z "$ONLY" ] || [ "$ONLY" = pass ] || [ "$ONLY" = answer ]; then
echo "################ 1. the library names its dangling link"
seed
S=$(status)
echo "$S" | sed 's/^/   /'
check "status counts the dangling link" 'printf "%s" "$S" | grep -q "project knowledge: on, 14 page(s), .*1 dangling link(s) (ops/deploy.md -> concepts/backoff.md)"'
check "a read of the page names it" 'read_page ops/deploy.md | grep -q "\"dangling\":\[\"concepts/backoff.md\"\]"'
check "status shows the size against the readable sources" 'printf "%s" "$S" | grep -q "ratio [0-9.]*, kept under 1.00"'
check "ha doctor warns of the dangling link" '(cd "$P" && "$HA" doctor 2>&1) | grep -q "project knowledge: 1 dangling link(s)"'

echo "################ 2. the wiki pass tends the library"
agent "$P" w1
touch "$RX/pass.marker"
say w1 "Reply with exactly one word: OK" | tail -2
for _ in $(seq 1 "${WIKI_WAIT:-60}"); do records wiki pass | grep -q . && break; sleep 10; done
sleep 5 # the job is removed just after the pass's record
records wiki pass | tail -1 | cut -c1-400 | sed 's/^/   /'
records wiki applied | sed 's/^/   applied /' | cut -c1-300
records wiki refused | sed 's/^/   refused /' | cut -c1-400
check "the pass ran, as a job of the Sleep queue" 'records wiki pass | grep -q . && records sleep claimed | grep -q "\"job\":\"wiki\""'
check "its job is done and gone" '! grep -lq "\"kind\": \"wiki\"" "$D"/sleep/*.json 2>/dev/null'
S=$(status)
echo "$S" | sed 's/^/   /'
check "status shows zero dangling links" 'printf "%s" "$S" | grep -q "project knowledge: on, .* 0 dangling link(s)"'
check "status shows the ratio, under 1" 'printf "%s" "$S" | grep -q "ratio 0\.[0-9]*, kept under 1.00"'
check "the link in the deploy page points at an existing backoff page" 'grep -Eq "\]\(\.\./concepts/(retry-backoff|backoff-for-retries)\.md\)" "$K/ops/deploy.md"'
left=$(ls "$K/concepts" | grep -c "backoff")
check "the two backoff pages are merged into one" '[ "$left" -eq 1 ]'
kept=$(ls "$K/concepts" | grep "backoff" | head -1)
check "the merged page keeps both facts" 'grep -qi "one second" "$K/concepts/$kept" && grep -qi "thirty seconds" "$K/concepts/$kept"'
check "the deploy page links to the page that was kept" 'grep -q "concepts/$kept" "$K/ops/deploy.md"'
gone=$(ls "$K/concepts" | grep -q "^retry-backoff.md$" && echo concepts/backoff-for-retries.md || echo concepts/retry-backoff.md)
check "the absorbed page is in the history, deleted by the wiki" 'page_history "$gone" | grep -q "^delete wiki$"'
check "an index page lists the pages by topic" '[ -f "$K/topics.md" ] && [ "$(grep -c "^- \[" "$K/topics.md")" -ge 12 ] && grep -q "services/billing.md" "$K/topics.md" && grep -q "teams/saffron.md" "$K/topics.md"'
check "the index holds links and headings only" '"$PY" -c "
import sys
text = open(sys.argv[1], encoding=\"utf-8\").read().split(\"\n\")
body = [l.strip() for l in text[1:] if l.strip()]
title, summary = body[0], body[1]
rest = [l for l in body[2:] if not l.startswith(\"## Related\")]
sys.exit(0 if all(l.startswith(\"#\") or (l.startswith(\"- [\") and l.endswith(\")\")) for l in rest) else 1)
" "$K/topics.md"'
check "the limits page is fixed from its source" 'sed 1d "$K/concepts/limits.md" | grep -Ewq "6" && ! grep -q "at most 4 times" "$K/concepts/limits.md"'
check "every edit is recorded as the wiki's in the log" 'grep -q " by wiki" "$K/log.md"'
check "the bug report has the Knowledge wiki section" '(cd "$P" && "$HA" bug-report --since 1h --out "$RX/report.zip" >/dev/null 2>&1) && "$PY" -c "
import sys, zipfile
z = zipfile.ZipFile(sys.argv[1])
text = \"\".join(z.read(n).decode(\"utf-8\", \"replace\") for n in z.namelist() if n.endswith(\"summary.md\"))
sys.exit(0 if \"## Knowledge wiki\" in text else 1)
" "$RX/report.zip"'
fi

if [ -z "$ONLY" ] || [ "$ONLY" = answer ]; then
echo "################ 3. an answer that needs two pages the index lists"
if [ -f "$K/topics.md" ]; then
  say w1 "Use the project's knowledge library (start from its index of topics). In which Slack channel do I page the on-call engineer for the billing service? Answer with the channel name." | tail -3
  check "the answer joins the two pages" 'last | grep -q "#saffron-oncall-7Q"'
  R=$(request "In which Slack channel do I page")
  echo "   read the index: $(cat "$D"/diagnostics/*.jsonl | grep -c '"tool":"knowledge"') knowledge call(s)"
else
  echo "FAIL  the answer: no index was written (precondition)"; fail=$((fail+1))
fi
fi
"$HA" shutdown --force >/dev/null 2>&1

if [ -z "$ONLY" ] || [ "$ONLY" = history ]; then
echo "################ 4. a memory search with history finds what an earlier conversation said"
# No background pass in this part: nothing but the journal holds the codename.
export HA_AUTO_REFINE=off
CODE="BLUEHERON-$(( RANDOM % 900 + 100 ))"
agent "$P" h1
say h1 "For this conversation only: the codename of the Q3 storage migration is $CODE. Do not save it to memory, knowledge or any file; just acknowledge in one word." | tail -2
"$HA" shutdown --force >/dev/null 2>&1
check "precondition: the codename is in no memory store or page" '! grep -rlaq "$CODE" "$D/harness" "$K" 2>/dev/null && ! find "$D/sessions" -name "*.sqlite3" -exec grep -laq "$CODE" {} + 2>/dev/null'
agent "$P" h2
say h2 "In an earlier conversation of this project I told you the codename of the Q3 storage migration. Find it with the memory tool: one search with action search, sources [\"memory\", \"knowledge\", \"history\"] and query \"codename Q3 storage migration\". Then answer with the codename only." | tail -3
check "the memory search named history among its sources" 'journal_has "sources:\[memory, ?knowledge, ?history\]"'
check "a hit of that search is the earlier conversation's turn" 'journal_has "$CODE[^{]*source:history|source:history[^}]*$CODE"'
check "the answer is the codename" 'last | grep -q "$CODE"'
"$HA" shutdown --force >/dev/null 2>&1
fi

result
