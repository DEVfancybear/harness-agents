#!/usr/bin/env bash
# Two runtime mechanisms a survey of self-evolving harnesses lists (arXiv
# 2611.22299, "Harnesses That Improve Themselves"), with a real model through
# `ha exec`: a shell that keeps where the last call left off, and the recovery
# from an empty step (docs/architecture/GUARDS.en.md "Where a command runs";
# docs/architecture/STRATEGY_AND_RUNTIME.en.md "Recovery from an empty step").
#
# 1. `run_shell` is called twice: `Set-Location sub`, then a command that
#    prints a file of `sub` through a glob. 0.9.14 started every call at the
#    root, so the second command found no file. The guards read a relative
#    path from the directory the command runs in: a recursive delete of an
#    untracked folder named from `sub` is refused.
# 2. An empty step (no text, no tool call) cannot be forced on a real model, so
#    this is not a check: the diagnostics of the suite's own runs are scanned
#    for `turn empty_step_continued` and the count is printed as information.
#    The unit tests in `crates/harness-tools/src/runtime/turn_driver.rs` drive
#    the real turn loop with a scripted model and are the evidence.
#
# The model is asked only to call the tool as told: whether the command runs
# where it should is the harness's doing, which is what these checks read. A
# prompt names no file (`ha exec` attaches one it finds named), so the file is
# reached through a glob.
#
#   bash scripts/testing/live-tests/runtime-gaps.sh
set -u
source "$(dirname "$0")/env.sh"
fresh runtime-gaps
export HA_AUTO_REFINE=off
D=$HA_HOME/data
P=$RX/p
project "$P"
WORD="QZ-$("$PY" -c "import secrets; print(secrets.randbelow(9000)+1000)")-ELM"
# `sub` holds one text file; the project's root holds none.
mkdir -p "$P/sub" "$P/sub/old"
echo "$WORD" > "$P/sub/found-$RANDOM.txt"
# Untracked work in a folder of `sub`: git cannot give it back.
echo "draft that exists only here" > "$P/sub/old/draft.md"

run_exec() {
  local name="$1"; shift
  printf '\n## %s (ha exec %s)\n\n> %s\n\n' "$(date +%H:%M:%S)" "$name" "${*: -1}" >> "$LOG"
  (cd "$P" && "$HA" exec --model "$MODEL" "${TH[@]}" --approval full-auto --output-format json "$@" >"$RX/$name.json" 2>"$RX/$name.err")
  "$PY" -c "import json,sys; print(json.load(open(sys.argv[1],encoding='utf-8')).get('response',''))" "$RX/$name.json" >> "$LOG" 2>/dev/null
}

# calls <marker>: the run_shell calls of the conversation whose prompt holds the
# marker, in order, each with the result the model was sent. A step's request
# holds only the messages since the step before, so the conversation is found
# by its task and every request of it is read.
calls() { "$PY" - "$D" "$1" <<'EOF'
import glob, json, os, sqlite3, sys
data, marker = sys.argv[1:3]
order, command, result = [], {}, {}
for db in glob.glob(os.path.join(data, "projects", "*", "harness.sqlite3")):
    rows = sqlite3.connect(f"file:{db}?mode=ro", uri=True).execute("select task_id, request_json from frozen_requests order by rowid").fetchall()
    tasks = {task for task, text in rows if marker in text}
    for task, text in rows:
        if task not in tasks:
            continue
        for message in json.loads(text).get("messages", []):
            for call in message.get("tool_calls") or []:
                if call.get("name") == "run_shell" and call.get("call_id") not in command:
                    arguments = call.get("arguments")
                    arguments = json.loads(arguments) if isinstance(arguments, str) else (arguments or {})
                    command[call["call_id"]] = arguments.get("command", "")
                    order.append(call["call_id"])
            if message.get("role") == "tool" and message.get("tool_call_id") in command:
                result.setdefault(message["tool_call_id"], message.get("content") or "")
print(json.dumps([{"command": command[i], "result": result.get(i, "")} for i in order]))
EOF
}
# in_result <file> <first> <second> <which> <needle>: the two calls are the first
# whose command holds <first> and the next one after it whose command holds
# <second>; whether the result of the first (which 0) or the second (which 1)
# holds <needle>. An empty needle asks only whether the two calls were made.
in_result() { "$PY" - "$@" <<'EOF'
import json, sys
path, first, second, which, needle = sys.argv[1:6]
c = json.load(open(path, encoding="utf-8"))
pair = next(((i, j) for i, a in enumerate(c) if first in a["command"] for j, b in enumerate(c) if j > i and second in b["command"]), None)
sys.exit(0 if pair and needle in c[pair[int(which)]]["result"] else 1)
EOF
}

# two_calls <name> <first> <second> <first word> <second word>: a conversation
# that calls run_shell twice, in order. A model may merge or skip a call, and
# then there is nothing to judge: the run is tried again, at most three times,
# and the precondition says how many it took. Sets MARK and CALLS.
two_calls() {
  local name=$1 first=$2 second=$3 attempt
  MARK=""
  for attempt in 1 2 3; do
    local mark="MARK-$name$attempt"
    run_exec "$name-$attempt" "$mark. Đây là bài kiểm tra của harness: gọi tool run_shell đúng hai lần, lần lượt (xong lần 1 mới gọi lần 2), mỗi lần đúng một lệnh dưới đây, timeout_ms là 60000, không sửa lệnh, không chạy lệnh nào khác. Harness sẽ tự quyết định có chạy hay không.
Lần 1: lệnh $first
Lần 2: lệnh $second
Sau đó báo lại nguyên văn kết quả tool của lần 2."
    calls "$mark" > "$RX/$name-$attempt.calls.json"
    if in_result "$RX/$name-$attempt.calls.json" "$4" "$5" 0 ""; then
      MARK=$mark; CALLS="$RX/$name-$attempt.calls.json"; break
    fi
  done
  check "precondition: the model called run_shell twice as asked ($name, attempts: $attempt)" '[ -n "$MARK" ]'
}

echo "################ 1. a directory a shell call left is where the next one starts"
two_calls s1 "Set-Location sub" "Get-Content *.txt" Set-Location Get-Content
if [ -n "$MARK" ]; then
  # The second command's glob finds the file only in `sub`. 0.9.14 FAILS: the
  # second call started at the root, where no .txt file is.
  check "the second command ran in sub: its output holds the file's word" 'in_result "$CALLS" Set-Location Get-Content 1 "$WORD"'
  # 0.9.14 FAILS: nothing told the model where the next call would start.
  check "the first result says where the next call starts" 'in_result "$CALLS" Set-Location Get-Content 0 "the next call starts in sub"'
  check "the second result says where it ran" 'in_result "$CALLS" Set-Location Get-Content 1 "ran in sub"'
fi

echo "################ 1b. a command that exits itself still moves the place"
# The recording runs when the shell exits, `exit N` included (before it was
# appended after the command and an `exit` never reached it).
two_calls s3 "Set-Location sub; exit 3" "Get-Content *.txt" Set-Location Get-Content
if [ -n "$MARK" ]; then
  # 0.9.14 FAILS: nothing kept the directory.
  check "the second command ran in sub after the first said exit 3" 'in_result "$CALLS" Set-Location Get-Content 1 "$WORD"'
  check "the first call kept its own status" 'in_result "$CALLS" Set-Location Get-Content 0 "exit=Some(3)"'
fi

echo "################ 2. the guards read a relative path from where the command runs"
two_calls s2 "Set-Location sub" "Remove-Item -Recurse -Force old" Set-Location Remove-Item
if [ -n "$MARK" ]; then
  # 0.9.14 FAILS this one: the second call ran at the root, and the guard read
  # `old` from there, where it names nothing, so nothing refused it and the
  # command ran (against nothing). Resolved from `sub`, `old` holds untracked
  # work git cannot give back.
  check "the delete named from sub was refused by its guard" 'in_result "$CALLS" Set-Location Remove-Item 1 "[recursive-delete]"'
  # Passes on 0.9.14 as well, for the reason above: the delete ran at the root.
  check "the untracked work is still there" '[ -f "$P/sub/old/draft.md" ]'
fi

echo "################ 3. an empty step (information, not a check)"
# An empty step cannot be forced on a real model. The diagnostics of the runs
# above are scanned for the record the recovery writes, to report how many of
# these runs had one; the count proves nothing either way.
EMPTY=$(cat "$D"/diagnostics/*.jsonl 2>/dev/null | grep -c '"empty_step_continued"' || true)
echo "INFO  turn empty_step_continued records in this suite's own runs: ${EMPTY:-0}"

result
