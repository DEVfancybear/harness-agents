#!/usr/bin/env bash
# Statements with evidence in the knowledge library, with a real model. After
# SoMem (arXiv 2611.11929, `docs/architecture/KNOWLEDGE_WIKI.en.md`, "Statements
# with evidence"): 0.9.7 kept a page's sources as strings nothing looked up, no
# time for a statement, no address for a paragraph, and no search by path or
# time. A page may now carry a footnote after a statement
# (`[^e1]: 2026-05; src/limits.py:1-2`) and a paragraph anchor (` ^id`), and:
#   S1. a write whose footnote cites a file that is not there is refused and
#       writes nothing; one that resolves is written, and `knowledge evidence`
#       returns what the reference holds, the cited lines marked `>N|`;
#   S2. a write whose link names a paragraph the page lacks (`page.md#^nope`)
#       is refused, naming it; a read of a cited page lists its `anchors` and
#       `cited_by`, and of a citing page its `dangling_anchors`;
#   S3. `timeline` lists the dated statements, oldest first, a year before its
#       months; `recent` the newest pages (both also as `/knowledge timeline`
#       and `/knowledge recent`); and the turn's knowledge catalog adds the
#       newest three pages the ranked list left out;
#   S4. `knowledge search` with `since` returns hits carrying `time_match`
#       (`event_time` for a page with a dated statement in the window,
#       `updated_at` for one with none);
#   S5. the `/knowledge bootstrap` request asks for the footnote form
#       `[^id]: path:line`, which the store checks.
# The library is seeded on disk in the form the store writes (a page header
# line, `<!-- ha-knowledge: {...} -->`), so timeline, recent, read and the
# catalog do not depend on the model writing anything. Where the model must
# write (S1, S2's refusal) or search (S4), the turn asks for one exact call, and
# the checks judge the feature only when the session's own record shows the
# model made that call (a "precondition:" check); a turn whose precondition did
# not hold is tried again, up to three times. A precondition that never held is
# a FAIL by its name, never a pass. What the model was sent and what the tool
# answered is read from the frozen requests (`ev`); the markers are the host's
# own strings, quoted in a comment from the Rust source that writes them (the
# prompts never contain them). A comment above a check says what 0.9.7 does
# with it: "0.9.7 FAILS" is the feature, "passes on 0.9.7 too" a precondition or
# a control.
# Every check prints PASS or FAIL; the conversation and the library stay in
# $LIVE_ROOT/evidence as evidence.
#
#   bash scripts/testing/live-tests/evidence.sh
set -u
source "$(dirname "$0")/env.sh"
fresh evidence
export HA_AUTO_REFINE=off HA_SELF_EVAL=off HA_SESSION_LOG=off HA_PYTHON=${HA_PYTHON:-$PY}
D=$HA_HOME/data
P=$RX/p
K=$P/.harness/knowledge
NL=$'\n'
project "$P"
# A file for a footnote to cite, three lines long.
mkdir -p "$P/src"
printf 'MAX = 10\nMIN = 1\nSTEP = 2\n' > "$P/src/limits.py"
(cd "$P" && git add -A && git -c user.name=t -c user.email=t@t commit -qm limits)
"$HA" knowledge on --cwd "$P" >/dev/null 2>&1

# One `ha exec` turn in $WD: `run_exec <name> <prompt>`.
WD=$P
run_exec() {
  local name="$1"; shift
  printf '\n## %s (ha exec %s)\n\n> %s\n\n' "$(date +%H:%M:%S)" "$name" "${*: -1}" >> "$LOG"
  (cd "${WD:-$P}" && "$HA" exec --model "$MODEL" "${TH[@]}" --approval full-auto --output-format json "$@" >"$RX/$name.json" 2>"$RX/$name.err")
  "$PY" -c "import json,sys; print(json.load(open(sys.argv[1],encoding='utf-8')).get('response',''))" "$RX/$name.json" >> "$LOG" 2>/dev/null
  sed 's/^/    /' "$RX/$name.err" >> "$LOG"
}
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
# requests: `ev <marker> root|child <python expression>`. It looks at the first
# conversation whose messages hold <marker> and judges the expression, true or
# false. In the expression: `msgs` (every message of that conversation, once: a
# request is stored as a delta of the one before), `text` (all of them and the
# tool calls, joined), `results` (the tool results), `calls` ([(tool,
# arguments)]), `n` (how many conversations), `content(m)`, `re`, `json`, `os`.
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
msgs = [m for t in picked for m in tasks[t]]
def arguments(call):
    try:
        return json.loads(call.get("arguments") or "{}")
    except ValueError:
        return {}
calls = [(c.get("name"), arguments(c)) for m in msgs for c in m.get("tool_calls") or []]
results = [content(m) for m in msgs if m.get("role") == "tool"]
text = "\n".join([content(m) for m in msgs] + [f"{c.get('name')} {c.get('arguments')}" for m in msgs for c in m.get("tool_calls") or []])
n = len(picked)
# The knowledge catalog of the turn: the ranked list, then (when the newest
# pages are not in it) the part under store.rs RECENT_HEADER.
HEAD = "recently written (not in the list above):"
seg = text.split("project knowledge:", 1)[1] if "project knowledge:" in text else ""
listed, _, recent = seg.partition(HEAD)
def paths(part):
    return re.findall(r'"path":"([^"]+)"', part)
newest = ["zz/fresh-1.md", "zz/fresh-2.md", "zz/fresh-3.md"]
sys.exit(0 if eval("(" + expr.strip() + ")") else 1)
PYEOF
ev() { "$PY" "$RX/ev.py" "$D" "$@"; }
# `ha knowledge <args>` for the project: no agent, no model.
kn() { (cd "$P" && "$HA" knowledge "$@" --cwd "$P" 2>/dev/null); }
# kjson <python expression> <ha knowledge args...>: the expression over `d` (the
# payload the command printed) and `r` (its results) is true.
kjson() {
  local expr="$1"; shift
  kn "$@" | "$PY" -c "
import json, sys
d = json.loads(sys.stdin.read().strip().splitlines()[-1])
r = d.get('results', [])
sys.exit(0 if eval(sys.argv[1]) else 1)" "$expr"
}
# page <path> <updated_at> <body>: a page as a person would have saved it, with
# the time it was written, as the store reads it (its header line, a title).
page() {
  "$PY" - "$K" "$1" "$2" "$3" <<'PYEOF'
import json, os, sys
root, path, updated, body = sys.argv[1:5]
title = path[:-3]
file = os.path.join(root, *path.split("/"))
os.makedirs(os.path.dirname(file), exist_ok=True)
meta = {"title": title, "summary": "About " + title, "updated_at": updated}
with open(file, "w", encoding="utf-8", newline="\n") as handle:
    handle.write("<!-- ha-knowledge: " + json.dumps(meta) + " -->\n# " + title + "\n\n" + body + "\n")
PYEOF
}

echo "################ 1. a library seeded on disk: dated statements, paragraphs that are cited, and sixty-eight pages"
# Four dated statements in two pages (a year, a day, a month, a day), one
# footnote with no time, and a paragraph (^orion-1) that another page links.
page people/ann.md 2026-06-01T00:00:00Z $'Ann was born in the harbor town.[^a] Ann moved to the harbor office in May.[^b] Ann left on the third.[^c]\n\n[^a]: 2026\n[^b]: 2026-05\n[^c]: 2026-05-03'
page people/bob.md 2026-06-02T00:00:00Z $'Bob arrived at the harbor at the end of April.[^d] Bob has a note with no date.[^e]\n\n[^d]: 2026-04-30\n[^e]: src/limits.py'
page project.md 2026-06-03T00:00:00Z $'Orion is a project at B. ^orion-1\n\nOther text.'
page work.md 2026-06-04T00:00:00Z $'Moved to B for [Orion](project.md#^orion-1) and [the page](project.md).'
# A link to a paragraph the target has since lost.
page old.md 2026-06-05T00:00:00Z $'From [Orion](project.md#^gone) once.'
# The three newest pages, and sixty old ones that fill the catalog's list.
page zz/fresh-1.md 2026-11-10T00:00:00Z 'The harbor office opened a new pier.'
page zz/fresh-2.md 2026-11-11T00:00:00Z 'Pier maintenance is scheduled for spring.'
page zz/fresh-3.md 2026-11-12T00:00:00Z 'Crane inspection notes are kept in the yard office.'
for number in $(seq -w 1 60); do page "notes/n-$number.md" 2026-01-01T00:00:00Z "Filler note $number about nothing in particular."; done
pages=$(find "$K" -name '*.md' -not -name index.md -not -name log.md | wc -l)
echo "   pages seeded: $pages"
# Passes on 0.9.7 too.
check "precondition: the library holds the seeded pages" '[ "$pages" -eq 68 ]'

echo "################ 2. S3: timeline and recent, derived from the pages, no model"
# views.rs timeline: dated statements oldest first, a year before its months.
# 0.9.7 FAILS: `ha knowledge timeline` answered with the usage line.
check "S3: timeline lists the dated statements oldest first: 2026, 2026-04-30, 2026-05, 2026-05-03" 'kjson "[(x[\"time\"], x[\"path\"]) for x in r] == [(\"2026\", \"people/ann.md\"), (\"2026-04-30\", \"people/bob.md\"), (\"2026-05\", \"people/ann.md\"), (\"2026-05-03\", \"people/ann.md\")]" timeline'
check "S3: a timeline statement is the sentence its marker follows, and says its footnote" 'kjson "r[0][\"statement\"] == \"Ann was born in the harbor town.\" and r[0][\"note\"] == \"a\"" timeline'
check "S3: timeline takes a folder or a page prefix and keeps what is under it" 'kjson "len(r) == 1 and r[0][\"time\"] == \"2026-04-30\"" timeline people/bob'
# views.rs recent: the newest pages first, from the time each was written.
check "S3: recent lists the newest pages first and takes a count" 'kjson "[x[\"path\"] for x in r] == [\"zz/fresh-3.md\", \"zz/fresh-2.md\"]" recent 2'
echo "################ 3. S2: a read lists the paragraphs that are cited, and the links to paragraphs that are gone"
# store.rs PageRead `anchors`, `cited_by`, `dangling_anchors`. 0.9.7 FAILS: none of the three existed.
check "S2: a read of a cited page lists its anchors and who cites each paragraph, with its start" 'kjson "r[0][\"anchors\"] == [\"orion-1\"] and any(c[\"page\"] == \"work.md\" and c[\"anchor\"] == \"orion-1\" and c[\"paragraph\"].startswith(\"Orion is a project at B.\") for c in r[0][\"cited_by\"])" read project.md'
check "S2: a read of a page whose link names a paragraph that is gone lists it as a dangling anchor" 'kjson "r[0][\"dangling_anchors\"] == [\"project.md#^gone\"]" read old.md'
check "S2: and the page that links an anchor that exists lists none" 'kjson "not r[0].get(\"dangling_anchors\")" read work.md'

echo "################ 4. S3: the same through the slash commands of a chat"
agent "$P" e1
"$HA" send e1 "/knowledge timeline people/" --wait > "$RX/slash-timeline.txt" 2>&1
"$HA" send e1 "/knowledge recent 2" --wait > "$RX/slash-recent.txt" 2>&1
"$HA" shutdown --force >/dev/null 2>&1
# tools.rs `command`: timeline [FOLDER], recent [COUNT]. 0.9.7 FAILS: both were the usage line.
check "S3: /knowledge timeline FOLDER lists the dated statements under it" 'grep -q "Ann was born in the harbor town" "$RX/slash-timeline.txt" && grep -q "Bob arrived at the harbor" "$RX/slash-timeline.txt" && ! grep -q "usage: /knowledge" "$RX/slash-timeline.txt"'
check "S3: /knowledge recent COUNT lists the newest pages" 'grep -q "zz/fresh-3.md" "$RX/slash-recent.txt" && ! grep -q "zz/fresh-1.md" "$RX/slash-recent.txt"'

echo "################ 5. S3: the turn's catalog adds the newest pages the ranked list left out"
run_exec c1 "MARK-S3C. Reply with the single word OK-CATALOG and call no tools."
r=$(refusal c1)
check "precondition: the provider answered the catalog turn${r:+ - it refused: ${r:0:200}}" '[ -z "$r" ]'
# store.rs `Store::catalog`: the catalog is the ranked list (ranked by the
# request's words, so which pages it holds is the ranking's), and then the
# newest three pages (views::newest) that the list did not hold, under
# RECENT_HEADER, inside the same bound. A newest page the list holds is not
# repeated. The first run's list held two of the three, and the part held the
# third, as designed. So: when the list left at least one of the three out the
# header must be there, with exactly the ones left out, and every one of the
# three must be in the catalog once.
E_LEFTOUT='seg != "" and not all(p in paths(listed) for p in newest)'
E_HEADER='HEAD in seg'
E_RECENT='HEAD in seg and paths(recent) and all(p in newest and p not in paths(listed) for p in paths(recent)) and all(p in paths(listed) + paths(recent) for p in newest) and len(paths(listed) + paths(recent)) == len(set(paths(listed) + paths(recent)))'
# Passes on 0.9.7 too: the turn's request holds the catalog, and the ranked list left a newest page out
# (were all three in it, nothing would be owed and the header rightly absent).
check "precondition: the turn's catalog is in the request and its ranked list left at least one of the three newest pages out" 'ev MARK-S3C root "$E_LEFTOUT"'
# 0.9.7 FAILS: the catalog was the ranked list alone.
check "S3: the catalog has the part 'recently written (not in the list above):'" 'ev MARK-S3C root "$E_HEADER"'
check "S3: that part holds exactly the newest pages the list left out, and each of the three newest is in the catalog once" 'ev MARK-S3C root "$E_RECENT"'

echo "################ 6. S5: /knowledge bootstrap asks for footnotes the store checks"
P5=$RX/p5
project "$P5"
"$HA" knowledge on --cwd "$P5" >/dev/null 2>&1
agent "$P5" b1
"$HA" send b1 "/knowledge bootstrap" >/dev/null 2>&1
# The request is the first thing the turn sends; once it is stored the turn is stopped.
for _ in $(seq 1 60); do
  ev "Map this project into its knowledge library now" root 'n >= 1' && break
  sleep 5
done
"$HA" abort b1 >/dev/null 2>&1
"$HA" shutdown --force >/dev/null 2>&1
check "precondition: the command started a turn with the mapping request" 'ev "Map this project into its knowledge library now" root "n >= 1"'
# lifecycle.rs knowledge_bootstrap_request. 0.9.7 FAILS: it asked for "its source as path:line".
E_BOOT='"`[^id]: path:line`" in text and "refuses the page otherwise" in text'
check "S5: the request asks for the source as a footnote [^id]: path:line that the store checks" 'ev "Map this project into its knowledge library now" root "$E_BOOT"'
check "S5: and no longer asks for a source that nothing looks up" 'ev "Map this project into its knowledge library now" root "n >= 1 and \"its source as path:line\" not in text"'

echo "################ 7. S1: a write whose footnote cites a file that is not there is refused; one that resolves is kept and opened by evidence"
# notes.rs "was not written, its footnotes do not hold: ..." and evidence.rs
# "<path>: no such file in the workspace". The prompts below do not hold those
# words, so they are in the record only if the host said them.
E_S1A_CALLED='any(name == "knowledge" and args.get("action") == "write" and "src/does-not-exist.rs:3" in str(args.get("content")) and "[^e1]" in str(args.get("content")) for name, args in calls)'
E_S1A_REFUSED='"was not written, its footnotes do not hold" in text and "no such file" in text and "[^e1]" in text'
asked1=""; REFUSED=""; STOP=0
for a in 1 2 3; do
  run_exec s1a$a "MARK-S1A-$a. Call the knowledge tool exactly once with action write, scope project, path ops/refused.md, title Refused, summary Where the cap came from, and this content (three lines: a sentence, an empty line, a footnote line):${NL}The cap was 10 in May.[^e1]${NL}${NL}[^e1]: 2026-05; src/does-not-exist.rs:3${NL}Make no other call, do not correct or retry the write, and answer with the tool's reply word for word."
  if refused s1a$a; then [ "$STOP" = 1 ] && break; continue; fi
  if ev "MARK-S1A-$a" root "$E_S1A_CALLED"; then asked1=$a; break; fi
  echo "   attempt $a: the model did not make the write as asked"
done
refusal_check "$asked1"
check "precondition: knowledge write was called with the footnote that cites src/does-not-exist.rs:3 (attempt ${asked1:-none})" '[ -n "$asked1" ]'
# 0.9.7 FAILS: the page was written, its footnote never looked up.
check "S1: the write was refused: 'was not written, its footnotes do not hold' and 'no such file'" '[ -n "$asked1" ] && ev "MARK-S1A-$asked1" root "$E_S1A_REFUSED"'
check "S1: a refused write creates no page" '[ -n "$asked1" ] && [ ! -e "$K/ops/refused.md" ]'

E_S1B_CALLED='any(name == "knowledge" and args.get("action") == "write" and "src/limits.py:1-2" in str(args.get("content")) for name, args in calls) and any(name == "knowledge" and args.get("action") == "evidence" and args.get("note") in ("e1", "[^e1]") and args.get("path") == "ops/limits.md" for name, args in calls)'
# evidence.rs `Sources::open`: the cited lines marked `>`, a little around them.
E_S1B_LINES='any(">1|" in r and ">2|" in r and "MAX = 10" in r and "2026-05" in r for r in results)'
asked2=""; REFUSED=""; STOP=0
for a in 1 2 3; do
  rm -f "$K/ops/limits.md"
  run_exec s1b$a "MARK-S1B-$a. Make exactly two knowledge tool calls, in this order. First: action write, scope project, path ops/limits.md, title Limits, summary How the limits changed, and this content (three lines: a sentence, an empty line, a footnote line):${NL}The cap was 10 in May.[^e1]${NL}${NL}[^e1]: 2026-05; src/limits.py:1-2${NL}Second: action evidence, scope project, path ops/limits.md, note e1. Then answer with the evidence call's reply word for word."
  if refused s1b$a; then [ "$STOP" = 1 ] && break; continue; fi
  if ev "MARK-S1B-$a" root "$E_S1B_CALLED"; then asked2=$a; break; fi
  echo "   attempt $a: the model did not make both calls as asked"
done
refusal_check "$asked2"
check "precondition: knowledge write (footnote src/limits.py:1-2) and knowledge evidence (note e1) were called (attempt ${asked2:-none})" '[ -n "$asked2" ]'
# Passes on 0.9.7 too: a footnote of free text was part of the page, written unchecked.
check "S1: a footnote that resolves is written with the page" '[ -n "$asked2" ] && grep -q "^\[\^e1\]: 2026-05; src/limits.py:1-2" "$K/ops/limits.md"'
# 0.9.7 FAILS: there was no evidence action.
check "S1: evidence returns the cited lines marked >N| and the event time" '[ -n "$asked2" ] && ev "MARK-S1B-$asked2" root "$E_S1B_LINES"'

echo "################ 8. S2: a write whose link names a paragraph the page lacks is refused"
# views.rs check_citations: "<page> was not written, a link names a paragraph that is
# not there: project.md has no paragraph anchored ^nope (it has ^orion-1)". A link is
# relative to the page's folder, so the page is written at the library's root, where
# project.md is (a page ops/links.md linking project.md names ops/project.md, a page
# that is not there: a dangling link, which the store allows by design - the first run).
E_S2_CALLED='any(name == "knowledge" and args.get("action") == "write" and "project.md#^nope" in str(args.get("content")) for name, args in calls)'
E_S2_REFUSED='"was not written, a link names a paragraph that is not there" in text and "^nope" in text and "^orion-1" in text'
asked3=""; REFUSED=""; STOP=0
for a in 1 2 3; do
  run_exec s2a$a "MARK-S2A-$a. Call the knowledge tool exactly once with action write, scope project, path links.md, title Links, summary A page that links a paragraph, and this one-line content: Moved to B for [Orion](project.md#^nope).${NL}Make no other call, do not correct or retry the write, and answer with the tool's reply word for word."
  if refused s2a$a; then [ "$STOP" = 1 ] && break; continue; fi
  if ev "MARK-S2A-$a" root "$E_S2_CALLED"; then asked3=$a; break; fi
  echo "   attempt $a: the model did not make the write as asked"
done
refusal_check "$asked3"
check "precondition: knowledge write was called with a link to project.md#^nope (attempt ${asked3:-none})" '[ -n "$asked3" ]'
# 0.9.7 FAILS: a fragment was dropped, the page written.
check "S2: the write was refused, naming ^nope and the anchor the page does have" '[ -n "$asked3" ] && ev "MARK-S2A-$asked3" root "$E_S2_REFUSED"'
check "S2: a refused write creates no page" '[ -n "$asked3" ] && [ ! -e "$K/links.md" ]'

echo "################ 9. S4: a search with since returns hits that say which time matched"
# store.rs SearchHit.time_match, views.rs apply_filter: ann.md holds statements
# dated in the window (event_time), zz/fresh-1.md has none and was written in
# it (updated_at); bob.md's only dated statement is before it, so it is out.
E_S4_CALLED='any(name == "knowledge" and args.get("action") == "search" and str(args.get("since")).startswith("2026-05") for name, args in calls)'
E_S4_HITS='any("keyword_rrf" in r and "people/ann.md" in r and "zz/fresh-1.md" in r and "\"time_match\":\"event_time\"" in r.replace(" ", "") and "\"time_match\":\"updated_at\"" in r.replace(" ", "") for r in results)'
E_S4_OUT='any("keyword_rrf" in r and "people/bob.md" not in r for r in results)'
asked4=""; REFUSED=""; STOP=0
for a in 1 2 3; do
  run_exec s4$a "MARK-S4-$a. Call the knowledge tool exactly once with action search, query harbor, since 2026-05-01. Make no other call. Then answer with the single word DONE-S4."
  if refused s4$a; then [ "$STOP" = 1 ] && break; continue; fi
  if ev "MARK-S4-$a" root "$E_S4_CALLED"; then asked4=$a; break; fi
  echo "   attempt $a: the model did not make the search as asked"
done
refusal_check "$asked4"
check "precondition: knowledge search was called with since 2026-05-01 (attempt ${asked4:-none})" '[ -n "$asked4" ]'
# 0.9.7 FAILS: search took no since, and no hit carried time_match.
check "S4: the hits carry time_match: event_time for the page with a dated statement, updated_at for the page with none" '[ -n "$asked4" ] && ev "MARK-S4-$asked4" root "$E_S4_HITS"'
check "S4: a page whose dated statements are all before the window is left out" '[ -n "$asked4" ] && ev "MARK-S4-$asked4" root "$E_S4_OUT"'

echo "evidence: $RX (conversation.md, the *.json answers, the library in $K)"
result
