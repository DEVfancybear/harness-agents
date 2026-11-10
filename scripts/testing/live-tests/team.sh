#!/usr/bin/env bash
# Children that work side by side, with a real model. After two papers:
#   s3 (arXiv 2611.08615): agents that all work at once repeat less of each
#     other's work once each knows who else is there and says what it takes;
#   Zero-shot coordination (arXiv 2611.06409): agents that cannot agree
#     beforehand each take their own convention for the natural one.
# 0.9.4 gave a child `agent_message` to a sibling "by name" and never told it
# a sibling's name, its brief, or how to work beside it.
#   1. two explorers started together each learn of the other: in its task
#      (`Teammates:`) or, for the one briefed first, by a `[teammate-joined`
#      message; both are told how to work beside a teammate, with the
#      coordination-friendly definition; the root is told to put what children
#      must agree on in their briefs;
#   2. each, asked to, tells the other what it found by its name, and the
#      message is in the other's conversation;
#   3. a verifier started beside them is on nobody's team.
# Every check prints PASS or FAIL; the conversation and the store stay in
# $LIVE_ROOT/team as evidence.
#
#   bash scripts/testing/live-tests/team.sh
set -u
source "$(dirname "$0")/env.sh"
fresh team
export HA_AUTO_REFINE=off HA_SELF_EVAL=off HA_SESSION_LOG=off HA_PYTHON=${HA_PYTHON:-$PY}
D="$HA_HOME/data"

# What the children of this home's conversations were sent, as one JSON object
# a child: {"task", "role", "name", "text"}; `text` is every message of every
# request of the child, joined. A child's name is read from the notices that
# name it; a request is a delta, so a child's text is all of them together.
children() {
  "$PY" - "$D" <<'PYEOF'
import glob, json, os, sqlite3, sys
sys.stdout.reconfigure(encoding="utf-8")
for db in glob.glob(os.path.join(sys.argv[1], "projects", "*", "harness.sqlite3")):
    c = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
    roles = dict(c.execute("select task_id, setting_value from session_settings where setting_key='delegated_child'"))
    texts = {}
    for (task, text) in c.execute("select task_id, request_json from frozen_requests order by rowid"):
        if task not in roles:
            continue
        parts = texts.setdefault(task, [])
        for m in json.loads(text).get("messages", []):
            parts.append(m.get("content") or "")
            for call in m.get("tool_calls") or []:
                parts.append(f"{call.get('name')} {call.get('arguments')}")
    for task, parts in texts.items():
        print(json.dumps({"task": task, "role": roles[task], "name": f"{roles[task]}-{task[-8:]}", "text": "\n".join(parts)}, ensure_ascii=False))
PYEOF
}
# The root's requests of this home, joined.
root_text() {
  "$PY" - "$D" <<'PYEOF'
import glob, json, os, sqlite3, sys
sys.stdout.reconfigure(encoding="utf-8")
for db in glob.glob(os.path.join(sys.argv[1], "projects", "*", "harness.sqlite3")):
    c = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
    kids = {task for (task,) in c.execute("select task_id from session_settings where setting_key='delegated_child'")}
    for (task, text) in c.execute("select task_id, request_json from frozen_requests order by rowid"):
        if task not in kids:
            for m in json.loads(text).get("messages", []):
                print(m.get("content") or "")
PYEOF
}
# judge <file of children()> <python expression>: `explorers` and `verifiers`
# are the children of the file, and the helpers below read one.
judge() {
  "$PY" - "$1" "$2" <<'PYEOF'
import json, sys
kids = [json.loads(line) for line in open(sys.argv[1], encoding="utf-8") if line.strip()]
explorers = [k for k in kids if k["role"] == "explorer"]
verifiers = [k for k in kids if k["role"] == "verifier"]
HEAD = "Teammates: your parent's other children"
def brief(k):
    """The brief a child was given, without what the host put after it."""
    return k["text"].split("Brief:", 1)[-1].split("\n\n", 1)[0]
def listed(a, b):
    return HEAD in a["text"] and f"- {b['name']} (" in a["text"].split(HEAD, 1)[1].split("\n\n", 1)[0]
def joined(a, b):
    return f"[teammate-joined: {b['name']}]" in a["text"]
def knows(a, b):
    return listed(a, b) or joined(a, b)
def sent(a, b):
    """Whether a called agent_message for the sibling b by its name."""
    return any(line.startswith("agent_message ") and b["name"] in line and "sibling" in line for line in a["text"].split("\n"))
def got(b, a, code):
    """Whether a's sibling message reached b, with `code` after it."""
    mark = f"[agent-message from sibling:{a['name']}]"
    return mark in b["text"] and code in b["text"].split(mark, 1)[1]
def pair():
    north = [e for e in explorers if "north/notes.txt" in brief(e)]
    south = [e for e in explorers if "south/notes.txt" in brief(e)]
    return (north[0], south[0]) if len(north) == 1 and len(south) == 1 else None
sys.exit(0 if eval(sys.argv[2]) else 1)
PYEOF
}

project "$RX/p"
mkdir -p "$RX/p/north" "$RX/p/south"
printf 'Ghi chú vùng bắc.\nMã vùng: KESTREL-4471\n' > "$RX/p/north/notes.txt"
printf 'Ghi chú vùng nam.\nMã vùng: PLOVER-9026\n' > "$RX/p/south/notes.txt"
(cd "$RX/p" && git add -A && git -c user.name=t -c user.email=t@t commit -qm notes)

echo "################ 1-2. two explorers at once know of each other and say what they found"
# Each child first waits, so both are in their runs together whatever the
# order they were started in; the brief names no sibling.
ran=""
for attempt in 1 2 3; do
  agent "$RX/p" "t$attempt"
  say "t$attempt" "Kiểm thử harness (lần $attempt). Trong MỘT bước, gọi tool delegate hai lần với wait=false, role explorer, brief đúng nguyên văn như sau, không thêm bớt. Brief thứ nhất: 'Lần $attempt. Trước hết gọi run_process đúng một lần với executable python, args -c và import time; time.sleep(20) (bắt buộc chạy). Sau đó đọc north/notes.txt và tìm mã vùng ở dòng cuối. Rồi gửi cho đồng đội của bạn (agent con khác của cha bạn, tên nó có trong nhiệm vụ này hoặc trong thông báo teammate-joined) đúng một agent_message với receiver_role sibling và receiver_name là tên đó, nội dung là mã vùng bạn tìm được. Cuối cùng trả lời mã vùng.' Brief thứ hai: y hệt brief thứ nhất nhưng thay north/notes.txt bằng south/notes.txt. Giao xong cả hai thì đợi cả hai báo kết quả rồi trả lời hai mã vùng." | tail -2 | sed 's/^/   /'
  # The children's reports wake the session; give them time to end.
  for _ in $(seq 1 30); do
    children > "$RX/kids-$attempt.jsonl"
    judge "$RX/kids-$attempt.jsonl" 'len([k for k in explorers if "Lần '"$attempt"'." in k["text"] and "[agent-message from sibling:" in k["text"]]) >= 2' && break
    sleep 10
  done
  sleep 15
  "$HA" shutdown --force >/dev/null 2>&1
  children > "$RX/kids-$attempt.jsonl"
  grep "Lần $attempt\\." "$RX/kids-$attempt.jsonl" > "$RX/team-$attempt.jsonl"
  count=$(grep -c . "$RX/team-$attempt.jsonl")
  echo "   attempt $attempt: explorers with this brief: $count"
  if [ "$count" -eq 2 ]; then ran=$attempt; break; fi
done
T="$RX/team-${ran:-1}.jsonl"
check "precondition: the root started exactly two explorers with the briefs (attempt ${ran:-none})" '[ -n "$ran" ]'
check "the briefs name no sibling (the precondition)" '[ -n "$ran" ] && judge "$T" "pair() and all(e[\"name\"] not in brief(o) for e in explorers for o in explorers)"'
check "each explorer was told of the other, in its task or as it joined" '[ -n "$ran" ] && judge "$T" "pair() and knows(*pair()) and knows(*pair()[::-1])"'
check "none was told of the other twice" '[ -n "$ran" ] && judge "$T" "pair() and not (listed(*pair()) and joined(*pair())) and not (listed(*pair()[::-1]) and joined(*pair()[::-1]))"'
check "a teammate is named with what it was asked" '[ -n "$ran" ] && judge "$T" "any(\"(explorer): Lần\" in e[\"text\"] for e in explorers)"'
check "each was told how to work beside a teammate (s3)" '[ -n "$ran" ] && judge "$T" "all(\"No message is required and working alone is fine\" in e[\"text\"] and \"do not count a claim a teammate repeats as a second check of it\" in e[\"text\"] for e in explorers)"'
check "and not to build on a choice a teammate would have to guess (the CFD)" '[ -n "$ran" ] && judge "$T" "all(\"do not build on a choice they would have to guess\" in e[\"text\"] for e in explorers)"'
root_text > "$RX/root.txt"
check "the root is told to put what children must agree on in their briefs" 'grep -q "put what they must agree on in every one.s brief" "$RX/root.txt"'
# What each did with the name it was given.
check "each sent the other a sibling message by its name" '[ -n "$ran" ] && judge "$T" "pair() and sent(*pair()) and sent(*pair()[::-1])"'
check "the north code reached the south explorer" '[ -n "$ran" ] && judge "$T" "pair() and got(pair()[1], pair()[0], \"KESTREL-4471\")"'
check "the south code reached the north explorer" '[ -n "$ran" ] && judge "$T" "pair() and got(pair()[0], pair()[1], \"PLOVER-9026\")"'
check "no sibling name was guessed wrong" '[ -n "$ran" ] && ! grep -q "no sibling named" "$T"'

echo "################ 3. a verifier is on nobody's team"
agent "$RX/p" tv
say tv "Kiểm thử harness. Trong MỘT bước, gọi tool delegate hai lần với wait=false: (1) role explorer, brief đúng nguyên văn: 'Việc VERIF-A. Gọi run_process đúng một lần với executable python, args -c và import time; time.sleep(25) (bắt buộc chạy), rồi đọc north/notes.txt và trả lời mã vùng.' (2) role verifier, brief đúng nguyên văn: 'Việc VERIF-B. Gọi run_process đúng một lần với executable python, args -c và import time; time.sleep(25) (bắt buộc chạy), rồi kiểm tra rằng south/notes.txt có dòng Mã vùng: PLOVER-9026.' Giao xong thì đợi cả hai báo kết quả rồi trả lời XONG." | tail -1 | sed 's/^/   /'
for _ in $(seq 1 24); do
  children > "$RX/kids-v.jsonl"
  [ "$(grep -c "VERDICT" "$RX/kids-v.jsonl")" -ge 1 ] && break
  sleep 10
done
sleep 10
"$HA" shutdown --force >/dev/null 2>&1
children | grep "VERIF-" > "$RX/kids-v.jsonl"
check "precondition: one explorer and one verifier ran with the briefs" 'judge "$RX/kids-v.jsonl" "len(explorers) == 1 and len(verifiers) == 1"'
check "the verifier was told of no teammate and given no team protocol" 'judge "$RX/kids-v.jsonl" "len(verifiers) == 1 and \"Teammates: your parent\" not in verifiers[0][\"text\"] and \"[teammate-joined:\" not in verifiers[0][\"text\"] and \"working alone is fine\" not in verifiers[0][\"text\"]"'
check "the explorer beside it was not told of the verifier" 'judge "$RX/kids-v.jsonl" "len(explorers) == 1 and \"Teammates: your parent\" not in explorers[0][\"text\"] and \"[teammate-joined:\" not in explorers[0][\"text\"]"'

result
