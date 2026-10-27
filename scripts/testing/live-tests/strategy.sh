#!/usr/bin/env bash
# What 0.9.0 took from arXiv 2608.19072 (strategy revision) and 2605.22166
# (LIFE-HARNESS), with a real model:
#   1. trajectory regulation: the same error from read_file three times, each
#      time with another path, is named under the third call, and the third
#      call made again unchanged is answered without running;
#   2. a goal whose strategy cannot pass the project's check stalls: every
#      refused goal_complete is an evaluation, three without progress are a
#      plateau, a strategist names other strategies, and the agent records a
#      switch with task_state and passes;
#   3. /strategy shows the strategy, the evaluations and the decisions;
#   4. the one learned skill that fits a request is put in the turn, with what
#      it got wrong before, and none for a request no skill fits;
#   5. the harness evolves by itself: failures of read_file called with `file`
#      become a rule in shadow, the rule is promoted on what it would have
#      done, and once active it acts on the next such call.
# Every check prints PASS or FAIL; the conversation stays in $LIVE_ROOT/strategy.
#
#   bash scripts/testing/live-tests/strategy.sh
set -u
source "$(dirname "$0")/env.sh"
fresh strategy
export HA_AUTO_REFINE=off HA_PYTHON=${HA_PYTHON:-$PY}
D="$HA_HOME/data"
# Records of one kind and event, since a time: `records <kind> <event> <since_ms>`.
records() {
  cat "$D"/diagnostics/*.jsonl 2>/dev/null | "$PY" -c "
import json, sys
kind, event, since = sys.argv[1], sys.argv[2], int(sys.argv[3])
for line in sys.stdin:
    try:
        r = json.loads(line)
    except ValueError:
        continue
    if r.get('kind') == kind and r.get('event') == event and r.get('ms', 0) >= since:
        print(json.dumps(r, ensure_ascii=False))
" "$1" "$2" "$3"
}
now_ms() { "$PY" -c "import time; print(int(time.time()*1000))"; }
# Trust a project in this home's config, so its [verify] checks load.
trust() {
  "$PY" - "$HA_HOME/config.toml" "$1" <<'PYEOF'
import re, sys, tomllib
path, project = sys.argv[1], sys.argv[2]
text = open(path, encoding="utf-8").read()
quoted = '"' + project.replace("\\", "\\\\") + '"'
if re.search(r"(?m)^\[trust\]", text):
    if re.search(r"(?ms)^\[trust\][^\[]*?^projects\s*=\s*\[", text):
        text = re.sub(r"(?ms)(^\[trust\][^\[]*?^projects\s*=\s*\[)", lambda m: m.group(1) + quoted + ", ", text, count=1)
    else:
        text = re.sub(r"(?m)^\[trust\]\s*$", "[trust]\nprojects = [" + quoted + "]", text, count=1)
else:
    text += "\n[trust]\nprojects = [" + quoted + "]\n"
tomllib.loads(text)
open(path, "w", encoding="utf-8").write(text)
PYEOF
}

echo "################ 1. a failure repeated is named; an unchanged retry does not run"
project "$RX/p1"
agent "$RX/p1" s1
since=$(now_ms)
say s1 "Kiểm thử harness, làm đúng thứ tự, mỗi bước là MỘT lời gọi tool read_file riêng (không gộp, không dùng paths): (1) path missing/a.rs (2) path missing/deep/a.rs (3) path lib/a.rs (4) lại path lib/a.rs đúng y như bước 3. Không gọi tool nào khác. Rồi in nguyên văn kết quả tool của bước 3 và bước 4, kể cả mọi dòng [harness: ...]." | tail -8 | sed 's/^/   /'
failed=$(records call settled "$since" | grep -c '"tool": "read_file".*"ok": false')
echo "   read_file failures recorded: $failed"
check "four read_file calls failed (the precondition)" '[ "$failed" -ge 4 ]'
check "the third failure was named under its call" 'records regulation repeated_failure "$since" | grep -q read_file'
check "the unchanged retry was not run" 'records regulation repeat_refused "$since" | grep -q read_file'
check "every settled call kept its argument hash and shape" 'records call settled "$since" | grep "read_file" | grep -q "\"args_shape\": \"path:string\""'
"$HA" shutdown --force >/dev/null 2>&1

echo "################ 2. a strategy that cannot pass stalls, and the agent switches"
P2="$RX/p2"
project "$P2"
mkdir -p "$P2/.harness"
cat > "$P2/.harness/config.toml" <<'EOF'
schema_version = 2
[verify]
judge = false
[[verify.checks]]
name = "result"
command = "git grep --untracked -q XONG-42 -- result.txt"
hint = "result.txt must hold XONG-42"
EOF
trust "$(cd "$P2" && (pwd -W 2>/dev/null || pwd))"
agent "$P2" s2
since=$(now_ms)
"$HA" send s2 "/goal Kiểm thử harness về chiến lược. Mục tiêu: check [verify] của dự án phải qua. Chiến lược ban đầu bắt buộc - ghi nó vào task_state với tên 'ghi draft': mỗi lượt ghi chuỗi XONG-42 vào file draft.txt bằng write_file rồi gọi goal_complete. Giữ đúng chiến lược này, không sửa file nào khác, cho tới khi harness báo nó không còn tiến triển (PLATEAU). Khi đó đọc các phương án mà harness nêu, ghi quyết định bằng task_state decide (kind switch, có evidence), rồi làm theo phương án bạn chọn." >/dev/null 2>&1
for _ in $(seq 1 120); do
  "$HA" send s2 "/goal status" --wait 2>&1 | grep -qi "status: *complete" && break
  [ -f "$P2/result.txt" ] && [ "$(records strategy decided "$since" | grep -c switch)" -ge 1 ] && sleep 30 && break
  sleep 10
done
"$HA" send s2 "/goal status" --wait 2>&1 | head -4 | sed 's/^/   /'
evaluated=$(records strategy evaluated "$since" | grep -vc '"source": "passed"')
echo "   evaluations before passing: $evaluated"
check "three evaluations or more under the first strategy (the precondition)" '[ "$evaluated" -ge 3 ]'
check "the third was a plateau" 'records strategy evaluated "$since" | grep -q "\"plateau\": true"'
check "a strategist named other strategies" 'records strategy proposed "$since" | grep -q proposed'
check "the agent recorded a switch" 'records strategy decided "$since" | grep -q "\"kind\": \"switch\""'
check "the switched strategy passed the check" 'grep -q XONG-42 "$P2/result.txt" 2>/dev/null'

echo "################ 3. /strategy shows what happened"
"$HA" send s2 "/strategy" --wait > "$RX/strategy.txt" 2>&1
sed 's/^/   /' "$RX/strategy.txt" | head -24
check "it names the strategy and the evaluations" 'grep -q "Strategy:" "$RX/strategy.txt" && grep -q "Evaluations" "$RX/strategy.txt"'
check "it lists the switch" 'grep -q "switch " "$RX/strategy.txt"'
"$HA" send s2 "/goal clear" --wait >/dev/null 2>&1
"$HA" shutdown --force >/dev/null 2>&1

echo "################ 4. the learned skill that fits is in the turn"
mkdir -p "$HA_HOME/skills/release-notes"
cat > "$HA_HOME/skills/release-notes/SKILL.md" <<'EOF'
---
name: release-notes
description: Use when the user asks for release notes or a changelog entry.
---
# Release notes

1. Add the entry at the top of CHANGELOG.md, under a heading that starts with RN-7.
2. One bullet per change, in the user's words.
EOF
echo '{"created_by": "ha", "created": "2026-10-01T00:00:00Z"}' > "$HA_HOME/skills/release-notes/.ha-learned.json"
mkdir -p "$D/skill-usage"
printf '{"skills": {"release-notes": {"uses": 1, "failed": 1, "failures": [{"at": "2026-10-20", "assumption": "CHANGELOG.md already exists", "evidence": "no such file"}]}}}\n' > "$D/skill-usage/global.json"
project "$RX/p4"
agent "$RX/p4" s4
since=$(now_ms)
say s4 "Viết release notes cho thay đổi: thêm nút xuất CSV. Ghi vào changelog của dự án." | tail -3 | sed 's/^/   /'
check "the fitting skill was recalled for the turn" 'records skill recalled "$since" | grep -q release-notes'
check "with what it got wrong before" 'records skill recalled "$since" | grep -q "\"failures\": 1"'
check "the agent followed it" 'grep -q "RN-7" "$RX/p4/CHANGELOG.md" 2>/dev/null'
since=$(now_ms)
say s4 "Chỉ trả lời: 2 cộng 2 bằng mấy?" | tail -1 | sed 's/^/   /'
check "a request no skill fits recalls none" '[ -z "$(records skill recalled "$since")" ]'
"$HA" shutdown --force >/dev/null 2>&1

echo "################ 5. the harness evolves a rule from failures, by itself"
unset HA_AUTO_REFINE
mkdir -p "$D/diagnostics"
seed="$D/diagnostics/20261001-seed-1.jsonl"
base=$(( $(now_ms) - 3600000 ))
: > "$seed"
for i in 1 2 3; do
  printf '{"kind":"call","event":"settled","ms":%s,"call":"seed-f%s","level":"info","fields":{"tool":"read_file","ok":false,"args_hash":"h%s","args_shape":"file:string","error_class":"invalid_call:seedclass","error_code":"invalid_call"}}\n' $((base+i)) $i $i >> "$seed"
  printf '{"kind":"tool","event":"failed","ms":%s,"call":"seed-f%s","level":"warn","msg":"invalid arguments for read_file: unknown field `file`, expected `path` (the file to read)","fields":{"tool":"read_file"}}\n' $((base+i)) $i >> "$seed"
  printf '{"kind":"call","event":"settled","ms":%s,"call":"seed-ok%s","level":"info","fields":{"tool":"read_file","ok":true,"args_hash":"k%s","args_shape":"path:string"}}\n' $((base+10+i)) $i $i >> "$seed"
done
project "$RX/p5"
agent "$RX/p5" s5
say s5 "Chỉ trả lời: OK" | tail -1 | sed 's/^/   /'
E="$HA_HOME/data/harness-evolution.json"
for _ in $(seq 1 30); do [ -f "$E" ] && break; sleep 5; done
rule=$("$PY" -c "import json,sys; d=json.load(open(sys.argv[1],encoding='utf-8')); r=[x for x in d.get('rules',[]) if x['tool']=='read_file']; print(r[0]['id'] if r else '')" "$E" 2>/dev/null)
echo "   rule: ${rule:-none}; $(cat "$E" 2>/dev/null | head -c 600)"
check "the run kept a rule for read_file, in shadow" '[ -n "$rule" ] && grep -q "\"state\": \"shadow\"" "$E"'
# What the rule would have done, recorded as a turn records it; and a day gone.
for i in 1 2 3; do
  printf '{"kind":"rule","event":"shadowed","ms":%s,"call":"seed-f%s","level":"info","fields":{"rule":"%s","tool":"read_file"}}\n' $((base+20+i)) $i "$rule" >> "$seed"
done
"$PY" -c "import json,sys; p=sys.argv[1]; d=json.load(open(p,encoding='utf-8')); d['ran']='2026-01-01T00:00:00Z'; json.dump(d,open(p,'w',encoding='utf-8'),indent=2)" "$E"
say s5 "Chỉ trả lời: OK" | tail -1 | sed 's/^/   /'
for _ in $(seq 1 30); do grep -q '"state": "active"' "$E" && break; sleep 5; done
check "the next run promoted it on what it would have done" 'grep -q "\"state\": \"active\"" "$E"'
since=$(now_ms)
"$HA" shutdown --force >/dev/null 2>&1
agent "$RX/p5" s6
say s6 "Kiểm thử harness: gọi tool read_file đúng một lần với tham số file (KHÔNG phải path) là README.md, tức {\"file\": \"README.md\"}. Không gọi tool nào khác. In nguyên văn kết quả tool." | tail -4 | sed 's/^/   /'
check "the active rule acted on the call" 'records rule acted "$since" | grep -q "$rule"'
"$HA" shutdown --force >/dev/null 2>&1

result
