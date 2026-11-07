#!/usr/bin/env bash
# Deep Dream that tests its notes against the transcripts, follows the
# user's dream.md, moves long detail down to knowledge and says what is
# unverified (docs/architecture/MEMORY_STORE.en.md §20, after Anthropic's
# "The components of agent memory", 2026-11-06), with a real model:
#
#  1. a conversation (`ha exec`) where the user states a fact;
#  2. the global memory seeded with that fact recorded WRONG, as a review
#     would have written it (origin: that conversation and a quote of it),
#     entries under `scratch`, a long procedure holding a token found nowhere
#     else, and a pinned theory marked unverified; a `dream.md` beside the
#     global memory asks for the scratch notes to go and for long procedures
#     to move down to knowledge;
#  3. a turn in the app: its request shows the theory as unverified, and the
#     automatic Deep Dream that follows corrects the wrong entry with a quote
#     of the conversation, deletes the scratch notes, and demotes the
#     procedure to a knowledge page that holds it word for word, leaving a
#     short entry with a pointer;
#  4. the next turn's request carries the short entry and not the detail, and
#     the agent answers a question about the detail by reading the page.
#
# On 0.9.3 (HA=<0.9.3 ha.exe>, the control) the entry stays wrong, dream.md
# is not read, nothing is demoted and nothing is marked unverified. Every
# check prints PASS or FAIL; the conversation, the stores and the
# diagnostics stay in $LIVE_ROOT/dream as evidence.
#
#   bash scripts/testing/live-tests/dream.sh
set -u
source "$(dirname "$0")/env.sh"
fresh dream
export HA_PYTHON=${HA_PYTHON:-$PY}
export HA_AUTO_REFINE=off HA_DIAG=debug
D=$HA_HOME/data
G=$D/harness
P="$RX/p-dream"
project "$P"
TOKEN="ZEBRA-4417"

# The first request that holds a phrase of its prompt: the turn's first
# model call, before any tool result joined it.
request() { "$PY" - "$D" "$1" <<'EOF'
import glob, os, sqlite3, sys
sys.stdout.reconfigure(encoding="utf-8")
for db in glob.glob(os.path.join(sys.argv[1], "projects", "*", "harness.sqlite3")):
    for (text,) in sqlite3.connect(f"file:{db}?mode=ro", uri=True).execute("select request_json from frozen_requests order by rowid"):
        if sys.argv[2] in text:
            print(text)
            sys.exit(0)
EOF
}
# One global entry's field: `entry <id> <python expression over e>`.
entry() { "$PY" "$MEMORY_STORE" document "$G" | "$PY" -c "
import json, sys
e = json.load(sys.stdin)['entries']['memory'].get(sys.argv[1])
print('' if e is None else eval(sys.argv[2], {'e': e}))" "$1" "$2" | tr -d '\r'; }
# Diagnostics records of one kind and event, one JSON line each.
diag() { "$PY" - "$D" "$1" "$2" <<'EOF'
import glob, json, os, sys
for path in glob.glob(os.path.join(sys.argv[1], "diagnostics", "*.jsonl")):
    for line in open(path, encoding="utf-8", errors="replace"):
        try:
            record = json.loads(line)
        except ValueError:
            continue
        if record.get("kind") == sys.argv[2] and record.get("event") == sys.argv[3]:
            print(json.dumps(record.get("fields", {}), ensure_ascii=False))
EOF
}

echo "################ 1. the user states a fact"
(cd "$P" && "$HA" exec --model "$MODEL" "${TH[@]}" --approval full-auto --output-format json \
  "For this test only: our staging database listens on port 6543, not on 5432. Do not save anything to memory or knowledge and call no tool; just answer with the one word OK." \
  >"$RX/d1.json" 2>"$RX/d1.err")
TASK=$("$PY" -c "import json,sys; print(json.load(open(sys.argv[1],encoding='utf-8'))['task_id'])" "$RX/d1.json" | tr -d '\r')
echo "   conversation: $TASK"
check "precondition: the conversation is in the journal" '[ -n "$TASK" ] && "$PY" -c "
import glob, os, sqlite3, sys
for db in glob.glob(os.path.join(sys.argv[1], \"projects\", \"*\", \"harness.sqlite3\")):
    c = sqlite3.connect(f\"file:{db}?mode=ro\", uri=True)
    if c.execute(\"select count(*) from events e join sessions s on s.session_id = e.session_id where s.task_id = ? and e.event_json like ?\", (sys.argv[2], \"%listens on port 6543%\")).fetchone()[0]:
        sys.exit(0)
sys.exit(1)" "$D" "$TASK"'
check "precondition: the agent kept no memory of it" '! "$PY" "$MEMORY_STORE" rows "$G" 2>/dev/null | grep -q 6543'

echo "################ 2. the global memory, with the fact recorded wrong"
"$PY" - "$G" "$TASK" "$TOKEN" <<'EOF'
import json, os, sys
directory, task, token = sys.argv[1], sys.argv[2], sys.argv[3]
os.makedirs(directory, exist_ok=True)
old = "2026-09-01T00:00:00Z"
def entry(key, title, content, path, **extra):
    e = {"id": key, "kind": "memory", "title": title, "content": content, "path": path,
         "scope": "global", "version": 1, "source": "agent", "updated_at": old}
    e.update(extra)
    return e
memory = {}
# Wrong from the start, as a review would have written it from the turn.
memory["db_port"] = entry("db_port", "Staging database port", "The staging database listens on port 5432.", "project",
    source="refine", origin={"via": "refine", "refinement": "refine_seeded", "conversation": task,
    "evidence": "our staging database listens on port", "kind": "user", "confidence": "high"})
for key, fact in [("font", "The blog's font is Inter."), ("color", "The blog's main colour is navy blue."),
                  ("host", "The blog is hosted on Netlify."), ("cms", "Blog posts are written in Markdown under content/."),
                  ("seo", "Every blog post needs a meta description."), ("rss", "The blog has an RSS feed at /rss.xml."),
                  ("images", "Blog images are WebP."), ("analytics", "The blog uses Plausible for analytics."),
                  ("comments", "Blog comments go through Giscus."), ("domain", "The blog lives at blog.example.org.")]:
    memory[key] = entry(key, key.capitalize(), fact, "blog")
for number, fact in enumerate(["The office printer is on the third floor.", "Lunch orders close at 11:00.",
                               "The coffee machine takes tokens."], start=1):
    memory[f"scratch_{number}"] = entry(f"scratch_{number}", f"Note {number}", fact, "scratch")
steps = [f"Step {n}: run release check {n} and write its result to the release log." for n in range(1, 13)]
steps[8] = f"Step 9: sign the release zip with the key named {token} and write its result to the release log."
memory["release"] = entry("release", "Release procedure", " ".join(steps), "ops")
memory["theory"] = entry("theory", "Cache theory", "The slow page loads may come from the image cache.", "perf",
    metadata={"pinned": True, "verified": "unverified"})
path = os.path.join(directory, "harness_state.json")
json.dump({"schema": 1, "entries": {"memory": memory}, "refinements": []}, open(path, "w", encoding="utf-8"))
EOF
cat > "$G/dream.md" <<'EOF'
What this memory is for: the projects I work on, not office life.
- Entries under the path `scratch` are notes I no longer need: delete them.
- A long step-by-step procedure belongs in the knowledge library: move its detail down (demote) rather than keeping it whole in memory.
EOF
echo 20 > "$G/memory_changes"

echo "################ 3. a turn; the automatic Deep Dream after it"
unset HA_AUTO_REFINE
agent "$P" d2
say d2 "Answer with the one word OK." >/dev/null
R=$(request "Answer with the one word OK.")
check "the request shows the theory as unverified" 'printf "%s" "$R" | grep -q "Cache theory.*unverified: check it before relying on it"'
for _ in $(seq 1 60); do [ -f "$G/memory_consolidated_changes" ] && break; sleep 10; done
"$PY" "$MEMORY_STORE" rows "$G" | cut -d'|' -f1-4 | sed 's/^/   /'
check "precondition: the consolidation ran" '[ -f "$G/memory_consolidated_changes" ]'
check "the curator was given the conversation's turns" '[ -n "$(diag dream evidence | grep -v "\"excerpts\": 0")" ]'
check "dream.md was read" 'diag dream instructions | grep -q "dream.md"'
# The live entry a correction wrote, as JSON: the entry updated in place,
# or the one that superseded it.
corrected() { "$PY" "$MEMORY_STORE" document "$G" | "$PY" -c "
import json, sys
for e in json.load(sys.stdin)['entries']['memory'].values():
    by = e.get('edited_by') or {}
    if '6543' in e.get('content', '') and not e.get('superseded_by') and (by.get('correction') or (e.get('origin') or {}).get('correction')):
        print(json.dumps(e)); break" | tr -d '\r'; }
FIX=$(corrected)
echo "   db_port: $(entry db_port "e['content']")"
echo "   corrected: $FIX"
field() { printf '%s' "$FIX" | "$PY" -c "import json, sys; e = json.load(sys.stdin); by = e.get('edited_by') or e.get('origin'); print(by.get(sys.argv[1], ''))" "$1" | tr -d '\r'; }
check "the wrong entry is corrected: it says 6543" '[ -n "$FIX" ]'
check "the correction names the conversation" '[ -n "$FIX" ] && [ "$(field conversation)" = "$TASK" ]'
check "the correction quotes it word for word" '[ -n "$FIX" ] && "$PY" -c "import sys; q = \" \".join(sys.argv[1].lower().split()); sys.exit(0 if len(q) >= 12 and q in sys.argv[2].lower() else 1)" "$(field evidence)" "$SAID"'
check "dream.md was honoured: the scratch notes are gone" '! "$PY" "$MEMORY_STORE" rows "$G" | grep -q "^scratch_"'
page=$(entry release "e['content']" | sed -n 's/.*(details: global knowledge \(.*\.md\)).*/\1/p')
echo "   release: $(entry release "e['content']")"
check "the procedure was demoted: a short entry with a pointer" '[ -n "$page" ] && [ "$(entry release "len(e[\"content\"])")" -le 180 ] && ! entry release "e[\"content\"]" | grep -q "$TOKEN"'
check "the page holds the procedure word for word" '[ -n "$page" ] && grep -q "Step 9: sign the release zip with the key named $TOKEN" "$D/knowledge/$page"'
check "the pinned theory is still pinned and unverified" '[ "$(entry theory "e.get(\"metadata\", {}).get(\"pinned\")")" = True ] && [ "$(entry theory "e[\"metadata\"].get(\"verified\")")" = unverified ]'
"$HA" shutdown --force >/dev/null 2>&1

echo "################ 4. the next turn: the short entry, and the detail read from the page"
export HA_AUTO_REFINE=off
agent "$P" d3
say d3 "In our release procedure, which key signs the release zip in step 9? Look it up and answer with the key's name only." | tail -3
R=$(request "which key signs the release zip in step 9")
check "the request carries the short entry with its pointer" 'printf "%s" "$R" | grep -q "(details: global knowledge"'
check "the request does not carry the detail" '! printf "%s" "$R" | grep -q "$TOKEN"'
check "the agent answers from the page" 'last | grep -q "$TOKEN"'
"$HA" shutdown --force >/dev/null 2>&1

result
