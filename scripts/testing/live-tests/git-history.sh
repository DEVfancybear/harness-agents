#!/usr/bin/env bash
# The repository's past, searched and read (docs/architecture/MEMORY_STORE.en.md,
# "The repository's own history"; after RepoMem, arXiv 2510.01003), with a real
# model through `ha exec`: `git_search` finds a commit by what its message was
# about and `git_show` reads what it changed.
#
# A project of twelve commits holds one that "fixes the rounding of invoice
# totals" in billing/calc.py. The line it added carries a random marker, and a
# later commit rewrites that line, so the marker is in the fix's diff and
# nowhere else: neither the file at HEAD nor any other commit shows it. The
# model is asked, without naming the file (`ha exec` attaches a file the prompt
# names), which earlier commit fixed that rounding and what line it added.
#
# 0.9.14 has `git_log` (the latest commits, no search) and `git_diff` (what is
# not committed): it has no `git_search` and no `git_show`, so the model cannot
# call them, and no result it is sent holds the marker.
#
#   bash scripts/testing/live-tests/git-history.sh
set -u
source "$(dirname "$0")/env.sh"
fresh git-history
export HA_AUTO_REFINE=off
D=$HA_HOME/data
P=$RX/p
project "$P"
MARKER="QZ-$("$PY" -c "import secrets; print(secrets.randbelow(9000)+1000)")-RND"

# commit <file> <content> <message>: one commit that writes the file.
commit() {
  mkdir -p "$P/$(dirname "$1")"
  printf '%s\n' "$2" > "$P/$1"
  (cd "$P" && git add -A && git -c user.name=t -c user.email=t@t commit -qm "$3")
}
commit billing/models.py "class Invoice: pass" "add the invoice model"
commit README.md "# shop" "write usage notes in the readme"
commit crm/export.py "def export(): pass" "add a customer export"
commit reports/gen.py "def generate(): pass" "speed up report generation"
commit billing/calc.py "def total(items): return sum(items)" "add the billing calculator"
commit requirements.txt "requests==2.0" "update dependency pins"
commit billing/calc.py "def total(items): return quantize(sum(items), \"$MARKER\")" "fix rounding of invoice totals in billing/calc.py"
commit auth/throttle.py "def throttle(): pass" "add login throttling"
# The line the fix added is rewritten: its marker is in no file at HEAD.
commit billing/calc.py "def total(items): return sum_items(items)" "tidy imports"
commit docs/export.md "export format" "document the export format"
commit VERSION "1.2.3" "bump the build number"
FIX=$(cd "$P" && git log --format=%H --grep="fix rounding of invoice totals" -n1)
FIX7=${FIX:0:7}
[ "$(cd "$P" && git rev-list --count HEAD)" = 12 ] || echo "WARN  the project has not twelve commits"
echo "INFO  the fix commit is $FIX7, its marker $MARKER"

run_exec() {
  local name="$1"; shift
  printf '\n## %s (ha exec %s)\n\n> %s\n\n' "$(date +%H:%M:%S)" "$name" "${*: -1}" >> "$LOG"
  (cd "$P" && "$HA" exec --model "$MODEL" "${TH[@]}" --approval full-auto --output-format json "$@" >"$RX/$name.json" 2>"$RX/$name.err")
  "$PY" -c "import json,sys; print(json.load(open(sys.argv[1],encoding='utf-8')).get('response',''))" "$RX/$name.json" >> "$LOG" 2>/dev/null
}

# calls <marker>: the git_search and git_show calls of the conversation whose
# prompt holds the marker, in order, each with its arguments and the result the
# model was sent. A step's request holds only the messages since the step
# before, so the conversation is found by its task and every request of it is
# read.
calls() { "$PY" - "$D" "$1" <<'EOF'
import glob, json, os, sqlite3, sys
data, marker = sys.argv[1:3]
order, info, result = [], {}, {}
for db in glob.glob(os.path.join(data, "projects", "*", "harness.sqlite3")):
    rows = sqlite3.connect(f"file:{db}?mode=ro", uri=True).execute("select task_id, request_json from frozen_requests order by rowid").fetchall()
    tasks = {task for task, text in rows if marker in text}
    for task, text in rows:
        if task not in tasks:
            continue
        for message in json.loads(text).get("messages", []):
            for call in message.get("tool_calls") or []:
                if call.get("name") in ("git_search", "git_show") and call.get("call_id") not in info:
                    arguments = call.get("arguments")
                    arguments = json.loads(arguments) if isinstance(arguments, str) else (arguments or {})
                    info[call["call_id"]] = {"name": call["name"], "arguments": arguments}
                    order.append(call["call_id"])
            if message.get("role") == "tool" and message.get("tool_call_id") in info:
                result.setdefault(message["tool_call_id"], message.get("content") or "")
print(json.dumps([dict(info[i], result=result.get(i, "")) for i in order]))
EOF
}

# judge <calls.json> <what> <fix hash> <marker>: exit 0 when
#   searched    the model called git_search;
#   first       any git_search result of the conversation lists the fix
#               commit as its first hit (a line `1. <short hash> ...`): the
#               model may search more than once, and the best evidence counts;
#   which       always exit 0, printing which search calls listed it first and
#               with what query (information);
#   show_called a git_show named the fix commit (a hash prefix of 7 or more
#               characters, or the full one), whatever it returned;
#   shown       a git_show named the fix commit (a hash prefix of 7 or more
#               characters, or the full one) and its result holds the marker;
#   shown_more  that result also holds the commit's whole message and the
#               changed file with its line counts.
judge() { "$PY" - "$@" <<'EOF'
import json, re, sys
path, what, fix, marker = sys.argv[1:5]
calls = json.load(open(path, encoding="utf-8"))
searches = [c for c in calls if c["name"] == "git_search"]
shows = [c for c in calls if c["name"] == "git_show"
         and len(str(c["arguments"].get("commit", ""))) >= 7 and fix.startswith(str(c["arguments"]["commit"]))]
if what == "searched":
    ok = bool(searches)
elif what == "first":
    ok = any(re.search(r"(?m)^1[.] " + re.escape(fix[:7]), s["result"]) for s in searches)
elif what == "which":
    for number, s in enumerate(searches, 1):
        top = re.search(r"(?m)^1[.] " + re.escape(fix[:7]), s["result"]) is not None
        print(f"INFO  git_search call {number} (query {s['arguments'].get('query')!r}): the fix commit is " + ("hit 1" if top else "not hit 1"))
    ok = True
elif what == "show_called":
    ok = bool(shows)
elif what == "shown":
    ok = any(marker in c["result"] for c in shows)
elif what == "shown_more":
    ok = any("fix rounding of invoice totals" in c["result"] and re.search(r"\+1 -1\s+billing/calc\.py", c["result"]) for c in shows)
else:
    ok = False
sys.exit(0 if ok else 1)
EOF
}

# The model may answer from `git log` in the shell, or skip a step: the checks
# are about what the tools return, so a run that never called git_search, or
# never called git_show on the commit it found, is tried again, at most three
# times. Each precondition says how many attempts it took, and the checks that
# need the call are judged only when it was made; a precondition that never
# held fails by its name.
SEARCH_AT=""; SHOW_AT=""
for attempt in 1 2 3; do
  mark="MARK-gh$attempt"
  run_exec "ask-$attempt" "$mark. Trong lịch sử git của dự án này, commit nào trước đây đã sửa việc làm tròn tổng hóa đơn (invoice totals)? Hãy dùng các tool lịch sử git: git_search để tìm commit theo nội dung của nó, rồi git_show để đọc commit tìm được. Trả lời: 7 ký tự đầu của hash commit đó, và nguyên văn dòng mã mà commit đó đã thêm."
  calls "$mark" > "$RX/ask-$attempt.calls.json"
  if [ -z "$SEARCH_AT" ] && judge "$RX/ask-$attempt.calls.json" searched "$FIX" "$MARKER"; then SEARCH_AT=$attempt; fi
  if [ -z "$SHOW_AT" ] && judge "$RX/ask-$attempt.calls.json" show_called "$FIX" "$MARKER"; then SHOW_AT=$attempt; fi
  [ -n "$SEARCH_AT" ] && [ -n "$SHOW_AT" ] && break
done
answer_of() { "$PY" -c "import json,sys; print(json.load(open(sys.argv[1],encoding='utf-8')).get('response',''))" "$RX/ask-$1.json"; }
# Passes on 0.9.14 too: the model (the tool is its to call) called git_search.
check "precondition: the model called git_search (attempt ${SEARCH_AT:-none} of $attempt)" '[ -n "$SEARCH_AT" ]'
if [ -n "$SEARCH_AT" ]; then
  CALLS="$RX/ask-$SEARCH_AT.calls.json"
  echo "################ the commit is found by what its message was about"
  # 0.9.14 FAILS: it has no git_search, so no search result is ever sent.
  judge "$CALLS" which "$FIX" "$MARKER"
  check "a git_search result lists the commit that fixed the rounding as hit 1" 'judge "$CALLS" first "$FIX" "$MARKER"'
fi
# Passes on 0.9.14 too: the model called git_show on the commit it found.
check "precondition: the model called git_show on the found commit (attempt ${SHOW_AT:-none} of $attempt)" '[ -n "$SHOW_AT" ]'
if [ -n "$SHOW_AT" ]; then
  CALLS="$RX/ask-$SHOW_AT.calls.json"
  ANSWER=$(answer_of "$SHOW_AT")
  echo "################ the commit is read"
  # 0.9.14 FAILS: it has no git_show, and git_diff shows only uncommitted
  # changes, so no result the model is sent holds the marker, which no file at
  # HEAD holds either.
  check "git_show on that commit returned the line it added (the marker)" 'judge "$CALLS" shown "$FIX" "$MARKER"'
  # 0.9.14 FAILS for the same reason.
  check "git_show's result holds the whole message and the changed file with its counts" 'judge "$CALLS" shown_more "$FIX" "$MARKER"'
  # 0.9.14 FAILS: the answer could not name the commit or its line.
  check "the answer names the commit's short hash" 'printf "%s" "$ANSWER" | grep -q "$FIX7"'
  check "the answer quotes the line the commit added" 'printf "%s" "$ANSWER" | grep -q "$MARKER"'
fi

result
