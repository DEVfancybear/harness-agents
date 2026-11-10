#!/usr/bin/env bash
# The person's review queue for memory, and what a child proposes (wave 2,
# docs/architecture/MEMORY_STORE.en.md §15), with a real model. Every check
# fails on 0.9.0, which queued nothing, had no `ha memory queue` or `/memory`,
# and whose note_finding took no `propose`.
#
#   1. a child that read a file only it saw proposes a memory entry with
#      note_finding's `propose`; the parent's review keeps it (in the store as
#      the review's, the proposal closed as kept) or leaves it to the person
#      (`ha memory queue` lists it, `ha doctor` counts it, `accept` puts it in
#      the store); the next conversation's request carries it;
#   2. in the app, with HA_MEMORY_REVIEW=all, the review that queued the
#      child's proposal says so once, `/memory queue` lists it,
#      `/memory queue reject` keeps it out, and the next review given the
#      same proposal does not queue it again;
#   3. `/refine` asked to keep a fetched page's rule as a constraint queues it
#      (0.9.0 refused it); accepting it makes it a constraint of the person's;
#   4. with HA_MEMORY_REVIEW=all a review's ordinary update waits and is not in
#      the store, and an accept after the entry changed is refused as stale;
#   5. an entry kept for app/billing/** with a citation, updated (or
#      superseded) by a review and accepted, is still kept for those files -
#      and, updated, still cites its code - and is in no request of a turn
#      that touches no billing file, yet loads with one that reads one. The
#      checks of 5 fail on the first wave-2 build, whose review dropped
#      `paths` and `citations`;
#   6. (after 0.9.3) a child's proposal with `scope: project`, the memory
#      tool's own scope, is taken, its evidence quoting two lines of a file
#      as the file has them (no `read_file` line numbers) is found, and a
#      quote no tool result holds is refused as a plain failure, never "side
#      effect may be uncertain". Each fails on 0.9.3 (a bug report's four
#      refused proposals and four refused quotes).
#
# Each check prints PASS or FAIL; the conversations, the exec runs and the
# diagnostics stay in $LIVE_ROOT/memory-queue as evidence. The model's steps
# are preconditions, judged before the checks, and retried when not met.
#
#   bash scripts/testing/live-tests/memory-queue.sh
set -u
source "$(dirname "$0")/env.sh"
fresh memory-queue
export HA_DIAG=debug HA_PYTHON=${HA_PYTHON:-$PY}
unset HA_AUTO_REFINE HA_MEMORY_REVIEW
D=$HA_HOME/data
G=$D/harness
DIAG=$D/diagnostics
# A review after every turn, the thinking level kept.
"$PY" - "$HA_HOME/settings.json" <<'EOF'
import json, os, sys
path = sys.argv[1]
settings = json.load(open(path, encoding="utf-8")) if os.path.exists(path) else {}
settings["autoRefine"] = {"turnInterval": 1}
json.dump(settings, open(path, "w", encoding="utf-8"))
EOF

run_exec() {
  local name="$1" dir="$2"; shift 2
  printf '\n## %s (ha exec %s)\n\n> %s\n\n' "$(date +%H:%M:%S)" "$name" "${*: -1}" >> "$LOG"
  (cd "$dir" && "$HA" exec --model "$MODEL" "${TH[@]}" --approval full-auto --output-format json "$@" >"$RX/$name.json" 2>"$RX/$name.err")
  "$PY" -c "import json,sys; print(json.load(open(sys.argv[1],encoding='utf-8')).get('response',''))" "$RX/$name.json" >> "$LOG" 2>/dev/null
  sed 's/^/    /' "$RX/$name.err" >> "$LOG"
}
task_of() { "$PY" -c "import json,sys; print(json.load(open(sys.argv[1],encoding='utf-8')).get('task_id',''))" "$RX/$1.json" 2>/dev/null; }
now_ms() { "$PY" -c "import time; print(int(time.time()*1000))"; }
# Diagnostic records of `kind` (and `event`) since a time, as `event key=value ... msg` lines.
records() {
  "$PY" - "$DIAG" "$1" "${2:-}" "${3:-0}" <<'EOF'
import glob, json, os, sys
sys.stdout.reconfigure(encoding="utf-8")
directory, kind, event, since = sys.argv[1], sys.argv[2], sys.argv[3], int(sys.argv[4])
rows = []
for path in glob.glob(os.path.join(directory, "*.jsonl")):
    for line in open(path, encoding="utf-8", errors="replace"):
        try:
            record = json.loads(line)
        except ValueError:
            continue
        if record.get("kind") == kind and (not event or record.get("event") == event) and record.get("ms", 0) > since:
            fields = " ".join(f"{k}={v}" for k, v in sorted(record.get("fields", {}).items()))
            rows.append((record.get("ms", 0), f"{record.get('event')} {fields} {record.get('msg', '')}".strip().replace("\n", " ")))
for _, row in sorted(rows):
    print(row)
EOF
}
# Wait for a review that ran after `since` (applied or nothing), at most ten minutes.
wait_review() {
  local since="$1"
  for _ in $(seq 1 120); do
    records review "" "$since" | grep -qE "^(applied|nothing|skipped|failed) " && return 0
    sleep 5
  done
  return 1
}
reviewed_since() { records review "" "$1" | grep -E "^(applied|nothing) " | head -1; }
# In the app the review's record comes before its child proposals are
# settled: wait until no proposal holding <text> is open (at most two minutes).
wait_settled() {
  for _ in $(seq 1 24); do
    [ -n "$(proposals "$1")" ] && ! proposals "$1" | grep -q " open " && return 0
    sleep 5
  done
  return 1
}
# Wait for a notice holding <text> after <since> (at most a minute).
wait_notice() {
  for _ in $(seq 1 12); do
    records notice shown "$2" | grep -q "$1" && return 0
    sleep 5
  done
  return 1
}
# The queue as JSON (every scope), and one field of the items holding a text.
queue_json() { "$HA" memory queue --json 2>/dev/null; }
# queued <text> [python condition on item i]: the ids of the waiting items whose edit holds <text>.
queued() {
  queue_json | "$PY" -c "
import json, sys
sys.stdout.reconfigure(encoding='utf-8')
text, extra = sys.argv[1], (sys.argv[2] if len(sys.argv) > 2 else 'True')
try:
    items = json.load(sys.stdin).get('items', [])
except ValueError:
    items = []
for i in items:
    if text in json.dumps(i['edit'], ensure_ascii=False) and eval(extra, {'i': i}):
        print(i['id'])" "$@"
}
# store_has <text> [python condition on entry e]: every scope's entries that hold now, by key.
store_has() {
  for scope in "$G" "$D"/sessions/*/harness; do
    [ -d "$scope" ] && "$PY" "$MEMORY_STORE" document "$scope" 2>/dev/null
  done | "$PY" -c "
import json, sys
sys.stdout.reconfigure(encoding='utf-8')
text, extra = sys.argv[1], (sys.argv[2] if len(sys.argv) > 2 else 'True')
decoder, data, found = json.JSONDecoder(), sys.stdin.read(), []
at = 0
while at < len(data):
    while at < len(data) and data[at].isspace():
        at += 1
    if at >= len(data):
        break
    doc, at = decoder.raw_decode(data, at)
    for key, e in (doc.get('entries', {}).get('memory', {}) or {}).items():
        if e.get('superseded_by') or e.get('expired'):
            continue
        if text in (e.get('title', '') + ' ' + e.get('content', '')) and eval(extra, {'e': e}):
            found.append(key)
print('\n'.join(found))" "$@"
}
global_has() { "$PY" "$MEMORY_STORE" document "$G" 2>/dev/null | "$PY" -c "
import json, sys
sys.stdout.reconfigure(encoding='utf-8')
doc = json.load(sys.stdin)
text, extra = sys.argv[1], (sys.argv[2] if len(sys.argv) > 2 else 'True')
for key, e in (doc.get('entries', {}).get('memory', {}) or {}).items():
    if not (e.get('superseded_by') or e.get('expired')) and text in (e.get('title', '') + ' ' + e.get('content', '')) and eval(extra, {'e': e}):
        print(key)" "$@"; }
# The child findings holding a text, as `seq closed queued` lines.
proposals() {
  cat "$D"/sessions/*/harness/child_findings.jsonl 2>/dev/null | "$PY" -c "
import json, sys
sys.stdout.reconfigure(encoding='utf-8')
for line in sys.stdin:
    try:
        f = json.loads(line)
    except ValueError:
        continue
    p = f.get('proposal')
    if sys.argv[1] in json.dumps(f, ensure_ascii=False) and p:
        print(f.get('seq'), p.get('closed') or 'open', p.get('queued') or '-', (p.get('note') or '').replace(chr(10), ' '))" "$1"
}
# Whether a request of conversation <task> (a whole request, not a delta) holds <text>.
carried() {
  "$PY" - "$D" "$1" "$2" <<'EOF'
import glob, os, sqlite3, sys
found = False
for db in glob.glob(os.path.join(sys.argv[1], "projects", "*", "harness.sqlite3")):
    c = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
    for (text,) in c.execute("select request_json from frozen_requests where task_id = ? order by rowid", (sys.argv[2],)):
        if '"delta_of"' not in text[:40] and sys.argv[3] in text:
            found = True
sys.exit(0 if found else 1)
EOF
}

echo "################ 1. a child proposes, the review keeps it or the person accepts it"
P1=$RX/p1
project "$P1"
mkdir -p "$P1/config"
printf '[release]\nrelease_channel = teal-osprey-417\n' > "$P1/config/deploy.ini"
(cd "$P1" && git add -A && git -c user.name=t -c user.email=t@t commit -qm config)
held1=""
for attempt in 1 2 3; do
  since1=$(now_ms)
  run_exec "q1-$attempt" "$P1" "Dùng tool delegate (wait mặc định, đợi kết quả) giao cho đúng một agent role explorer, brief đúng nguyên văn: 'Đọc file config/deploy.ini (lần $attempt). Kênh phát hành (release_channel) của project là một sự thật lâu dài cần nhớ: gọi tool note_finding với fact nêu kênh đó, evidence là dòng chứa nó chép đúng như read_file trả về, và propose là {\"title\": \"Release channel\", \"path\": \"project/release\", \"scope\": \"global\"}. Trả lời agent cha đúng một dòng: ĐÃ-GHI, không nêu giá trị nào.' Bản thân bạn không đọc file đó và không gọi tool memory. Đợi explorer xong rồi trả lời đúng một dòng."
  proposed=$(proposals teal-osprey-417 | tail -1)
  review=$(reviewed_since "$since1")
  echo "   attempt $attempt: proposal: ${proposed:-none}; review: ${review:0:160}"
  if [ -n "$proposed" ] && [ -n "$review" ] && ! grep -q teal-osprey-417 "$RX/q1-$attempt.json"; then held1=$attempt; break; fi
done
check "precondition: the child proposed the entry with note_finding, the review ran, the answer does not hold the value (attempt ${held1:-none})" '[ -n "$held1" ]'
closed1=$(proposals teal-osprey-417 | tail -1 | awk '{print $2}')
echo "   the proposal ended: $(proposals teal-osprey-417 | tail -1)"
check "the proposal was settled by the review: kept, or left to the person (${closed1:-none})" 'echo "$closed1" | grep -qxE "kept|queued"'
if [ "$closed1" = queued ]; then
  item1=$(queued teal-osprey-417 | head -1)
  "$HA" memory queue > "$RX/q1-list.txt" 2>&1
  sed 's/^/   /' "$RX/q1-list.txt" | head -6
  check "ha memory queue lists what waits, with who proposed it" '[ -n "$item1" ] && grep -q "$item1" "$RX/q1-list.txt" && grep -qE "delegated child|the review" "$RX/q1-list.txt"'
  (cd "$P1" && "$HA" doctor) > "$RX/q1-doctor.txt" 2>&1
  check "ha doctor counts the change that waits" 'grep -q "proposed memory change(s) wait for your review" "$RX/q1-doctor.txt"'
  check "nothing of it is in the store before it is accepted" '[ -z "$(store_has teal-osprey-417)" ]'
  "$HA" memory queue show "$item1" > "$RX/q1-show.txt" 2>&1
  check "show gives its evidence and what it adds" 'grep -q "evidence: \"release_channel = teal-osprey-417" "$RX/q1-show.txt" && grep -q "^  + " "$RX/q1-show.txt"'
  "$HA" memory queue accept "$item1" > "$RX/q1-accept.txt" 2>&1; accepted=$?
  sed 's/^/   /' "$RX/q1-accept.txt"
  check "accept puts it in the store, as the person's" '[ "$accepted" -eq 0 ] && [ -n "$(store_has teal-osprey-417 "e.get(\"origin\",{}).get(\"kind\")==\"user\" or (e.get(\"edited_by\") or {}).get(\"accepted\")")" ]'
  check "the queue no longer holds it" '[ -z "$(queued teal-osprey-417)" ]'
else
  # Judged only on a proposal the review kept: 0.9.0's review keeps the
  # finding too, with no proposal to close.
  check "the review kept it as its own entry, the proposal closed as kept" '[ "$closed1" = kept ] && [ -n "$(store_has teal-osprey-417 "(e.get(\"origin\") or {}).get(\"via\")==\"refine\" or (e.get(\"edited_by\") or {}).get(\"via\")==\"refine\"")" ]'
  check "nothing of it waits" '[ "$closed1" = kept ] && [ -z "$(queued teal-osprey-417)" ]'
fi
if [ -n "$held1" ] && [ -n "$(global_has teal-osprey-417)" ]; then
  run_exec q1-next "$P1" "Không dùng tool nào, không đọc file. Trả lời đúng một dòng: kênh phát hành (release_channel) của project này là gì, theo những gì bạn đã có sẵn; nếu không có thì trả lời KHÔNG-BIẾT."
  T1N=$(task_of q1-next)
  check "the next conversation's request carries it" '[ -n "$T1N" ] && carried "$T1N" teal-osprey-417'
else
  echo "   no proposal settled, or the entry is not global (kept in the conversation's store): the next conversation is not told of it"
  check "the proposal's entry is in the store" '[ -n "$held1" ] && [ -n "$(store_has teal-osprey-417)" ]'
fi

echo "################ 2. in the app with HA_MEMORY_REVIEW=all: said once, listed, rejected, not queued again"
P2=$RX/p2
project "$P2"
mkdir -p "$P2/ops"
printf '[deploy]\ndeploy_window = quartz-lynx-608\n' > "$P2/ops/deploy.ini"
(cd "$P2" && git add -A && git -c user.name=t -c user.email=t@t commit -qm ops)
"$HA" shutdown --force >/dev/null 2>&1
export HA_MEMORY_REVIEW=all
agent "$P2" q2
BRIEF2="Đọc file ops/deploy.ini (lần LAN). Khung giờ deploy (deploy_window) của project là một sự thật lâu dài cần nhớ: gọi tool note_finding với fact nêu khung giờ đó, evidence là dòng chứa nó chép đúng như read_file trả về, và propose là {\"title\": \"Deploy window\", \"path\": \"project/deploy\", \"scope\": \"global\"}. Trả lời agent cha đúng một dòng: ĐÃ-GHI, không nêu giá trị nào."
held2=""
for attempt in 1 2; do
  since2=$(now_ms)
  say q2 "Dùng tool delegate (wait mặc định, đợi kết quả) giao cho đúng một agent role explorer, brief đúng nguyên văn: '${BRIEF2//LAN/$attempt}' Bản thân bạn không đọc file đó và không gọi tool memory. Đợi explorer xong rồi trả lời đúng một dòng." | tail -2 | sed 's/^/   /'
  wait_review "$since2"
  wait_settled quartz-lynx-608
  item2=$(queued quartz-lynx-608 | head -1)
  echo "   attempt $attempt: proposal: $(proposals quartz-lynx-608 | tail -1); waiting: ${item2:-none}"
  if [ -n "$(proposals quartz-lynx-608)" ] && [ -n "$(reviewed_since "$since2")" ]; then held2=$attempt; break; fi
done
check "precondition: the child proposed the entry and the review ran (attempt ${held2:-none})" '[ -n "$held2" ]'
check "with all, what the review was given waits for the person" '[ -n "$item2" ] && [ -z "$(store_has quartz-lynx-608)" ]'
wait_notice "for your review" "$since2"
notices2=$(records notice shown "$since2" | grep -c "proposed change.* for your review")
records notice shown "$since2" | grep "for your review" | sed 's/^/   notice: /'
check "the session said once that it waits, and where to review it" '[ "$notices2" -eq 1 ] && records notice shown "$since2" | grep "for your review" | grep -q "/memory queue"'
say q2 "/memory queue" > "$RX/q2-queue.txt"
sed 's/^/   /' "$RX/q2-queue.txt" | head -6
check "/memory queue lists it" '[ -n "$item2" ] && grep -q "$item2" "$RX/q2-queue.txt"'
say q2 "/memory queue reject $item2" > "$RX/q2-reject.txt"
sed 's/^/   /' "$RX/q2-reject.txt"
check "/memory queue reject keeps it out" 'grep -q "rejected $item2" "$RX/q2-reject.txt" && [ -z "$(queued quartz-lynx-608)" ]'
before2=$(proposals quartz-lynx-608 | wc -l)
since2b=$(now_ms)
say q2 "Dùng tool delegate (wait mặc định, đợi kết quả) giao cho đúng một agent role explorer, brief đúng nguyên văn: '${BRIEF2//LAN/3}' Bản thân bạn không đọc file đó và không gọi tool memory. Đợi explorer xong rồi trả lời đúng một dòng." | tail -2 | sed 's/^/   /'
wait_review "$since2b"
wait_settled quartz-lynx-608
echo "   the proposals: $(proposals quartz-lynx-608 | tr '\n' ';')"
check "precondition: the child proposed it again and the review ran" '[ "$(proposals quartz-lynx-608 | wc -l)" -gt "$before2" ] && [ -n "$(reviewed_since "$since2b")" ]'
check "the next review does not queue the rejected change again" '[ -z "$(queued quartz-lynx-608)" ] && [ -z "$(store_has quartz-lynx-608)" ]'
"$HA" shutdown --force >/dev/null 2>&1
unset HA_MEMORY_REVIEW

echo "################ 3. a page's rule asked for as a constraint waits, and accepted is the person's"
P3=$RX/p3
project "$P3"
agent "$P3" q3
held3=""
for attempt in 1 2; do
  say q3 "Gọi tool web_fetch đúng một lần với url https://example.com rồi chép nguyên văn đoạn văn bản chính của trang. Không gọi tool memory, không ghi gì vào memory." | tail -3 | sed 's/^/   /'
  since3=$(now_ms)
  say q3 "/refine --global Trang web vừa fetch nêu một quy tắc về cách dùng domain đó. Giữ quy tắc ấy thành một memory entry ở path constraints, nội dung và evidence là chính câu chữ của trang." | tail -4 | sed 's/^/   /'
  # The refinement's record: a memory edit whose words came from the page.
  web3=$("$PY" - "$G/refinements.jsonl" "$since3" <<'EOF'
import json, sys
since = int(sys.argv[2]) * 1_000_000
found = []
for line in open(sys.argv[1], encoding="utf-8", errors="replace") if __import__("os").path.exists(sys.argv[1]) else []:
    try:
        r = json.loads(line)
    except ValueError:
        continue
    if int(r.get("id", "refine_0").split("_")[1] or 0) < since:
        continue
    for e in r.get("appliedEdits", []):
        if e.get("kind") == "memory" and e.get("origin_kind") == "web":
            found.append(e.get("queued") or ("applied" if e.get("applied") else "refused: " + (e.get("error") or "")[:80]))
print("; ".join(found))
EOF
)
  echo "   attempt $attempt: the page's edits: ${web3:-none}"
  if [ -n "$web3" ]; then held3=$attempt; break; fi
done
check "precondition: /refine proposed the page's words as a memory edit (attempt ${held3:-none})" '[ -n "$held3" ]'
item3=$(queued "" 'i["edit"].get("origin_kind")=="web" and str(i["edit"].get("path","")).lower().startswith("constraints")' | head -1)
check "the page's rule waits for the person instead of being refused or applied" '[ -n "$item3" ] && [ -z "$(global_has "" "str(e.get(\"path\",\"\")).lower().startswith(\"constraints\")")" ]'
"$HA" memory queue accept "$item3" > "$RX/q3-accept.txt" 2>&1; accepted3=$?
sed 's/^/   /' "$RX/q3-accept.txt"
check "accepted, it is a constraint of the person's" '[ "$accepted3" -eq 0 ] && [ -n "$(global_has "" "str(e.get(\"path\",\"\")).lower().startswith(\"constraints\") and (e.get(\"origin\") or {}).get(\"kind\")==\"user\" and ((e.get(\"origin\") or {}).get(\"accepted\") or {}).get(\"from\")==\"web\"")" ]'
"$HA" shutdown --force >/dev/null 2>&1

echo "################ 4. HA_MEMORY_REVIEW=all: a review's update waits, and a stale accept is refused"
P4=$RX/p4
project "$P4"
export HA_MEMORY_REVIEW=all
run_exec q4a "$P4" "Dùng tool memory action write, scope global, tiêu đề 'Service port', nội dung 'The service of this project listens on port 18417.' Không làm gì khác, trả lời ĐÃ-LƯU."
E4=$(global_has 18417 | head -1)
echo "   the entry: ${E4:-none}"
held4=""
for attempt in 1 2; do
  run_exec "q4b-$attempt" "$P4" --resume "$(task_of q4a)" "Cổng dịch vụ của project đã đổi: từ hôm nay service lắng nghe ở port 19417, không còn 18417 nữa. Đừng tự cập nhật memory và đừng gọi tool memory: review sau lượt này sẽ lo việc đó. Trả lời đúng một dòng: ĐÃ-BIẾT."
  item4=$(queued 19417 "i['edit'].get('id')=='$E4' and i['edit'].get('action') in ('update','supersede')" | head -1)
  echo "   attempt $attempt: the update that waits: ${item4:-none} ($(grep -o 'memory: [0-9]* proposed change[^:]*' "$RX/q4b-$attempt.err" | head -1))"
  if [ -n "$E4" ] && [ -n "$item4" ]; then held4=$attempt; break; fi
done
check "precondition: the memory tool kept the entry, the review proposed its update (attempt ${held4:-none})" '[ -n "$held4" ]'
check "the review's update waits and is not in the store" '[ -n "$item4" ] && [ -z "$(store_has 19417)" ] && [ -n "$(global_has 18417)" ]'
check "the run said a change waits for review" 'grep -q "proposed change.* for your review" "$RX/q4b-${held4:-1}.err"'
run_exec q4c "$P4" "Dùng tool memory action write với id '$E4' (scope global), nội dung mới 'The service of this project listens on port 17417.' Không làm gì khác, trả lời ĐÃ-SỬA."
check "precondition: the entry changed since the update was proposed" '[ -n "$(global_has 17417)" ]'
"$HA" memory queue accept "$item4" > "$RX/q4-accept.txt" 2>&1; stale4=$?
sed 's/^/   /' "$RX/q4-accept.txt"
check "an accept after the entry changed is refused as stale" '[ -n "$item4" ] && [ "$stale4" -ne 0 ] && grep -q "refused $item4 as stale" "$RX/q4-accept.txt"'
check "the entry keeps the later version, and the stale change left the queue" '[ -n "$item4" ] && [ -n "$(global_has 17417)" ] && [ -z "$(global_has 19417)" ] && ! queue_json | grep -q "\"$item4\""'
unset HA_MEMORY_REVIEW

echo "################ 5. a review's accepted update keeps the entry's files and its citation"
P5=$RX/p5
project "$P5"
mkdir -p "$P5/app/billing"
printf '# Billing amounts.\n\nROUNDING_MODE = HALF_EVEN\n\n\ndef rounded(cents):\n    return cents\n' > "$P5/app/billing/rounding.py"
(cd "$P5" && git add -A && git -c user.name=t -c user.email=t@t commit -qm billing)
export HA_MEMORY_REVIEW=all
# requested <task> <text>: whether any request of the conversation, a delta included, holds <text>.
requested() {
  "$PY" - "$D" "$1" "$2" <<'EOF'
import glob, os, sqlite3, sys
found = False
for db in glob.glob(os.path.join(sys.argv[1], "projects", "*", "harness.sqlite3")):
    c = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
    for (text,) in c.execute("select request_json from frozen_requests where task_id = ? order by rowid", (sys.argv[2],)):
        if sys.argv[3] in text:
            found = True
sys.exit(0 if found else 1)
EOF
}
# calls5 <marker>: the tool calls of the conversation whose input holds <marker>, as `name arguments` lines.
calls5() {
  "$PY" - "$D" "$1" <<'EOF'
import glob, json, os, sqlite3, sys
sys.stdout.reconfigure(encoding="utf-8")
for db in glob.glob(os.path.join(sys.argv[1], "projects", "*", "harness.sqlite3")):
    rows = sqlite3.connect(f"file:{db}?mode=ro", uri=True).execute(
        "select session_id, event_json from events order by session_id, sequence").fetchall()
    sessions = {session for session, text in rows
                if json.loads(text)["event_type"] == "input.admitted" and sys.argv[2] in text}
    for session, text in rows:
        event = json.loads(text)
        if session in sessions and event["event_type"] == "model.response":
            for call in event["payload"].get("tool_calls") or []:
                print(call.get("name"), call.get("arguments"))
EOF
}
# The entry as the memory tool wrote it: kept for app/billing/**, citing the
# line it rests on. Then the user changes the fact; the review
# (HA_MEMORY_REVIEW=all) proposes the entry's update, which waits. A turn
# whose own model wrote memory left the review nothing to propose: the entry
# is purged and the attempt starts again from its write.
E5=""; T5=""; held5=""; item5=""; wrote5=""
for attempt in 1 2 3; do
  if [ -n "$E5" ]; then
    (cd "$P5" && "$HA" memory purge "$E5" --scope project --yes) > "$RX/q5-purge-$attempt.txt" 2>&1
    E5=""
  fi
  run_exec "q5a-$attempt" "$P5" "Dùng tool memory action write với đúng các tham số: scope 'project', path 'billing', title 'Billing rounding', content 'Billing amounts in this project round half-even (rm-5521).', paths ['app/billing/**'], citations [{\"path\": \"app/billing/rounding.py\", \"quote\": \"ROUNDING_MODE = HALF_EVEN\"}]. Không làm gì khác, trả lời ĐÃ-LƯU."
  E5=$(global_has rm-5521 "e.get('paths')==['app/billing/**'] and any(c.get('path')=='app/billing/rounding.py' for c in e.get('citations') or [])" | head -1)
  T5=$(task_of "q5a-$attempt")
  echo "   attempt $attempt: the entry: ${E5:-none}"
  [ -n "$E5" ] || continue
  run_exec "q5b-$attempt" "$P5" --resume "$T5" "Quy tắc làm tròn của billing đã đổi: từ hôm nay số tiền billing của project làm tròn half-up (rm-5522), không còn half-even nữa. Đừng tự cập nhật memory và đừng gọi tool memory: review sau lượt này sẽ lo việc đó. Trả lời đúng một dòng: ĐÃ-BIẾT. MARKER_Q5_FACT_$attempt"
  wrote5=$(calls5 "MARKER_Q5_FACT_$attempt" | grep '^memory' | grep -c '"action":"write"')
  item5=$(queued rm-5522 "i['edit'].get('id')=='$E5' and i['edit'].get('action') in ('update','supersede')" | head -1)
  echo "   attempt $attempt: the turn's own memory writes: $wrote5; the change of $E5 that waits: ${item5:-none}"
  if [ "$wrote5" -eq 0 ] && [ -n "$item5" ]; then held5=$attempt; break; fi
  # Another proposal of this attempt is not this check's: rejected, it leaves the queue.
  for other in $(queued rm-5522); do "$HA" memory queue reject "$other" > /dev/null 2>&1; done
done
check "precondition: the entry is kept for app/billing/** and cites app/billing/rounding.py" '[ -n "$E5" ]'
check "precondition: the review proposed an update of that entry, and it waits (attempt ${held5:-none})" '[ -n "$held5" ]'
action5=$(queue_json | "$PY" -c "import json,sys; print(next((i['edit'].get('action','') for i in json.load(sys.stdin).get('items',[]) if i['id']==sys.argv[1]), ''))" "$item5" 2>/dev/null)
"$HA" memory queue accept "$item5" > "$RX/q5-accept.txt" 2>&1; accepted5=$?
sed 's/^/   /' "$RX/q5-accept.txt"
check "precondition: the accept applied the review's ${action5:-change}" '[ -n "$held5" ] && [ "$accepted5" -eq 0 ] && [ -n "$(global_has rm-5522)" ]'
check "the entry the accept wrote is still kept for app/billing/** (${action5:-none})" '[ -n "$held5" ] && [ -n "$(global_has rm-5522 "e.get(\"paths\")==[\"app/billing/**\"]")" ]'
if [ "$action5" = supersede ]; then
  # A correction cites none of the old entry's code (MEMORY_STORE §12).
  check "superseded, the correction cites none of the old entry's code" '[ -n "$(global_has rm-5522 "not e.get(\"citations\")")" ]'
else
  check "updated, it still cites app/billing/rounding.py" '[ -n "$held5" ] && [ -n "$(global_has rm-5522 "any(c.get(\"path\")==\"app/billing/rounding.py\" for c in e.get(\"citations\") or [])")" ]'
fi
unset HA_MEMORY_REVIEW
# A turn that touches no billing file: the entry is in none of its requests.
held5c=""
for attempt in 1 2; do
  since5=$(now_ms)
  run_exec "q5c-$attempt" "$P5" "Đọc file README.md bằng read_file rồi trả lời đúng một từ: XONG. Không đọc, liệt kê hay tìm file nào khác, không gọi tool memory. MARKER_Q5_PLAIN_$attempt"
  T5C=$(task_of "q5c-$attempt")
  calls5 "MARKER_Q5_PLAIN_$attempt" | sed 's/^/   call: /'
  if [ -n "$T5C" ] && calls5 "MARKER_Q5_PLAIN_$attempt" | grep -q '^read_file.*README.md' && ! calls5 "MARKER_Q5_PLAIN_$attempt" | grep -q 'billing'; then held5c=$attempt; break; fi
done
check "precondition: that turn read README.md and touched no billing file (attempt ${held5c:-none})" '[ -n "$held5c" ]'
check "the updated entry is in no request of a turn that touches no billing file" '[ -n "$held5c" ] && requested "$T5C" "MARKER_Q5_PLAIN_$held5c" && ! requested "$T5C" rm-5522 && ! records memory scoped_loaded "$since5" | grep -q .'
# And one that reads a billing file loads it, with that result.
held5d=""
for attempt in 1 2; do
  since5d=$(now_ms)
  run_exec "q5d-$attempt" "$P5" "Đọc file app/billing/rounding.py bằng read_file rồi trả lời đúng một từ: XONG. Không gọi tool memory. MARKER_Q5_BILLING_$attempt"
  T5D=$(task_of "q5d-$attempt")
  if [ -n "$T5D" ] && calls5 "MARKER_Q5_BILLING_$attempt" | grep -q '^read_file.*app/billing/rounding.py'; then held5d=$attempt; break; fi
done
check "precondition: that turn read app/billing/rounding.py (attempt ${held5d:-none})" '[ -n "$held5d" ]'
check "a turn that reads a billing file loads the updated entry with that result" '[ -n "$held5d" ] && records memory scoped_loaded "$since5d" | grep -q "path=app/billing/rounding.py" && requested "$T5D" rm-5522'

echo "################ 6. a project proposal, a quote of two lines, and a refusal that is plain"
P6=$RX/p6
project "$P6"
mkdir -p "$P6/config"
printf '[owners]\nteam = "amber-kite-913"\npager = "slate-wren-274"\n' > "$P6/config/owners.toml"
(cd "$P6" && git add -A && git -c user.name=t -c user.email=t@t commit -qm owners)
# The findings of the child holding a text, as `scope evidence` lines.
findings6() {
  cat "$D"/sessions/*/harness/child_findings.jsonl 2>/dev/null | "$PY" -c "
import json, sys
sys.stdout.reconfigure(encoding='utf-8')
for line in sys.stdin:
    try:
        f = json.loads(line)
    except ValueError:
        continue
    if sys.argv[1] in f.get('fact', '') + f.get('evidence', ''):
        print((f.get('proposal') or {}).get('scope', '-'), json.dumps(f.get('evidence', ''), ensure_ascii=False))" "$1"
}
held6=""
for attempt in 1 2 3; do
  since6=$(now_ms)
  run_exec "q6-$attempt" "$P6" "Dùng tool delegate (wait mặc định, đợi kết quả) giao cho đúng một agent role explorer, brief đúng nguyên văn: 'Đọc file config/owners.toml bằng read_file (lần $attempt). Rồi gọi note_finding đúng hai lần, theo thứ tự. Lần 1 (một trích dẫn không có trong kết quả nào, để xem tool từ chối ra sao): fact là \"owners.toml names no escalation contact\", evidence là \"escalation = nobody-here-000\". Lần 2: fact nêu team và pager của project theo owners.toml; evidence là ĐÚNG HAI DÒNG team và pager, chép như trong file, KHÔNG kèm số dòng mà read_file đặt đầu mỗi dòng, hai dòng cách nhau bằng một xuống dòng; propose là {\"title\": \"Project owners\", \"path\": \"project/owners\", \"scope\": \"project\"}. Trả lời agent cha đúng một dòng: kết quả mỗi lần gọi (KEPT hoặc lỗi tool trả về), không nêu team hay pager.' Bản thân bạn không đọc file đó và không gọi tool memory. Đợi explorer xong rồi trả lời đúng một dòng."
  calls6=$(records call settled "$since6" | grep -c 'tool=note_finding')
  echo "   attempt $attempt: note_finding calls: $calls6; findings: $(findings6 amber-kite-913 | tr '\n' ' ')"
  if [ "$calls6" -ge 2 ]; then held6=$attempt; break; fi
done
check "precondition: the child called note_finding twice (attempt ${held6:-none})" '[ -n "$held6" ]'
records tool failed "$since6" | grep 'tool=note_finding' | sed 's/^/   refused: /'
check "the quote no tool result holds is refused" '[ -n "$held6" ] && records tool failed "$since6" | grep "tool=note_finding" | grep -q "no tool result of yours holds this evidence"'
check "the refusal is a plain failure, not an unknown outcome" '[ -n "$held6" ] && ! records tool failed "$since6" | grep "tool=note_finding" | grep -q "side effect may be uncertain" && ! records call settled "$since6" | grep "tool=note_finding" | grep -q "error_code=outcome_unknown"'
check "a proposal with scope project is taken, not refused by the schema" '[ -n "$held6" ] && ! records call settled "$since6" | grep "tool=note_finding" | grep -q "error_code=invalid_call" && findings6 amber-kite-913 | grep -q "^project "'
check "the quote of two lines without line numbers is found and kept" '[ -n "$held6" ] && findings6 amber-kite-913 | grep "amber-kite-913.*slate-wren-274" | grep -qvE "[0-9]+(#[0-9a-z]+)?: (team|pager)"'
# The review settles the proposal: kept as its own edit, or left to the
# person. Either way the entry is this project's (bug report 2026-11-10: a
# child's project fact kept untagged reached every project).
[ -n "$held6" ] && { wait_review "$since6"; wait_settled amber-kite-913; }
settled6=$(proposals amber-kite-913 | tail -1)
closed6=$(echo "$settled6" | awk '{print $2}')
kept6=$(echo "$settled6" | sed -n 's/.*`\([^`]*\)`.*/\1/p')
echo "   the project proposal ended: ${settled6:-none}"
check "precondition: the review settled the project proposal (${closed6:-none})" 'echo "$closed6" | grep -qxE "kept|queued"'
if [ "$closed6" = queued ]; then
  check "it waits tagged with its project" '[ -n "$(queued amber-kite-913 "(i[\"edit\"].get(\"project\") or {}).get(\"key\")")" ]'
  item6=$(queued amber-kite-913 | head -1)
  "$HA" memory queue accept "$item6" > "$RX/q6-accept.txt" 2>&1
  check "accepted, it is an entry kept for its project" '[ -n "$(global_has "" "(e.get(\"project\") or {}).get(\"name\")==\"p6\" and \"amber-kite-913\" in e.get(\"title\",\"\")+e.get(\"content\",\"\")")" ]'
else
  check "the review kept it as an entry for its project" '[ -n "$kept6" ] && [ -n "$(global_has "" "e.get(\"id\")==\"$kept6\" and (e.get(\"project\") or {}).get(\"name\")==\"p6\"")" ]'
fi

result
