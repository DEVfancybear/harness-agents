#!/usr/bin/env bash
# What 0.7.4 changed, with a real model:
#   1. children nest past two levels: with no limit set, a grandchild starts a
#      child of its own (depth 3), which 0.7.3 refused with "RLM recursion depth
#      limit reached"; `[agents] max_depth` in the config sets a limit again;
#   2. web_search asks DeepSeek Search with the DeepSeek key before Serper;
#   3. a DeepSeek key the API refuses hands the search to the next API, and the
#      answer says so, naming /login deepseek;
#   4. the Python websearch skill does the same in the REPL;
#   5. a coder's commits carry the name `Harness Agents`, not `harness-p5`.
# Every check prints PASS or FAIL; the conversation and the diagnostics stay in
# $LIVE_ROOT/nesting as evidence. The DeepSeek key comes from the credential
# file in HA_CREDENTIALS_DIR (`deepseek`), never printed.
#
#   bash scripts/testing/live-tests/nesting.sh
set -u
source "$(dirname "$0")/env.sh"
fresh nesting
export HA_AUTO_REFINE=off HA_PYTHON=${HA_PYTHON:-$PY} HA_DIAG=debug
D="$HA_HOME/data/diagnostics"
project "$RX/p"
# Diagnostic records of one kind, as `event key=value ...` lines.
records() {
  "$PY" - "$D" "$1" <<'EOF'
import glob, json, os, sys
directory, kind = sys.argv[1], sys.argv[2]
rows = []
for path in glob.glob(os.path.join(directory, "*.jsonl")):
    for line in open(path, encoding="utf-8", errors="replace"):
        try:
            record = json.loads(line)
        except ValueError:
            continue
        if record.get("kind") == kind:
            fields = " ".join(f"{k}={v}" for k, v in sorted(record.get("fields", {}).items()))
            rows.append((record.get("ms", 0), f"{record.get('event')} {fields} {record.get('msg', '')}".strip()))
for _, row in sorted(rows):
    print(row)
EOF
}

echo "################ 1. children nest past two levels"
agent "$RX/p" n1
say n1 "/rlm-max-depth" | tail -2 | sed 's/^/   /'
check "the depth is unlimited by default" 'last | grep -q "RLM max depth: unlimited (default)"'
say n1 "Kiểm thử harness về agent lồng nhau, làm đúng: dùng tool delegate (wait mặc định) giao cho một agent role explorer nhiệm vụ giữ nguyên văn: 'Dùng tool delegate giao cho một agent role explorer nhiệm vụ: Dùng tool delegate giao cho một agent role explorer nhiệm vụ: trả lời đúng một dòng TANG-BA-OK. Đợi nó xong rồi trả lời nguyên văn câu trả lời của nó. Đợi nó xong rồi trả lời nguyên văn câu trả lời của nó.' Đợi explorer xong rồi trả lời nguyên văn câu trả lời cuối cùng nhận được, kèm mọi lỗi tool nếu có." | tail -4 | sed 's/^/   /'
records child | grep -E "^started" | sed 's/^/   /'
check "a child started at depth 3" 'records child | grep -qE "^started .*depth=3"'
check "the answer from depth 3 came back" 'last | grep -q "TANG-BA-OK"'
check "no spawn was refused for depth" '! (ans | grep -q "depth limit reached")'
"$HA" shutdown --force >/dev/null 2>&1
# A limit in the config holds again.
printf '\n[agents]\nmax_depth = 1\n' >> "$HA_HOME/$(basename "$HA_CONFIG")"
agent "$RX/p" n1b
say n1b "/rlm-max-depth" | tail -2 | sed 's/^/   /'
check "[agents] max_depth = 1 is read from the config" 'last | grep -q "RLM max depth: 1 (config)"'
say n1b "Kiểm thử harness: dùng tool delegate giao cho một agent role explorer nhiệm vụ giữ nguyên văn: 'Dùng tool delegate giao cho một agent role explorer nhiệm vụ trả lời CHAU-OK. Nếu tool delegate không có hoặc báo lỗi thì trả lời nguyên văn lỗi đó hoặc KHONG-CO-DELEGATE.' Đợi explorer xong rồi trả lời nguyên văn câu trả lời của nó." | tail -3 | sed 's/^/   /'
check "a child under max_depth = 1 starts no child" '! (records child | grep -qE "^started .*depth=2")'
"$HA" shutdown --force >/dev/null 2>&1
cp "$RX/home/$(basename "$HA_CONFIG")" "$RX/config-with-limit.toml"
cp "$HA_CONFIG" "$HA_HOME/"

echo "################ 2. web_search asks DeepSeek Search first"
agent "$RX/p" n2
say n2 "Kiểm thử harness: gọi tool web_search đúng một lần với query 'DeepSeek API search guide', rồi in nguyên văn dòng đầu tiên của kết quả tool (dòng bắt đầu bằng Results for query) và URL của kết quả 1. Không gọi tool nào khác." | tail -4 | sed 's/^/   /'
check "the results came from DeepSeek Search" 'last | grep -q "(DeepSeek Search)"'
check "no search API failed" '! (records web | grep -q "^search_failed")'
"$HA" shutdown --force >/dev/null 2>&1

echo "################ 3. a refused DeepSeek key hands the search to the next API"
# A key DeepSeek refuses (401), set for this agent only: the search moves on.
DEEPSEEK_API_KEY=sk-ha-live-refused-key agent "$RX/p" n3
say n3 "Kiểm thử harness: gọi tool web_search đúng một lần với query 'rust programming language', rồi in nguyên văn hai dòng đầu tiên của kết quả tool (dòng Results for query và dòng ghi chú trong ngoặc ngay sau, nếu có). Không gọi tool nào khác." | tail -5 | sed 's/^/   /'
records web | sed 's/^/   /'
check "DeepSeek's refusal was recorded as a refused key" 'records web | grep -qE "^search_failed .*api=deepseek .*key_refused=true"'
check "the results came from the next API" 'last | grep -qE "\((Google via Serper|DuckDuckGo)\)"'
check "the answer names the failure and /login deepseek" 'last | grep -q "DeepSeek Search failed" && last | grep -q "/login deepseek"'
say n3 "Gọi tool web_search thêm đúng một lần với query 'rust book', rồi in nguyên văn hai dòng đầu tiên của kết quả tool." | tail -4 | sed 's/^/   /'
check "the refused key is skipped by the next search" 'last | grep -q "DeepSeek Search skipped: it refused its key"'
check "the skipped key made no second request" '[ "$(records web | grep -c "^search_failed .*api=deepseek")" -eq 1 ]'
"$HA" shutdown --force >/dev/null 2>&1

echo "################ 4. the Python websearch skill asks DeepSeek Search"
agent "$RX/p" n4
say n4 "Kiểm thử harness: trong tool python_repl chạy đúng đoạn này: print((await websearch('DeepSeek search API')).splitlines()[0]) rồi in nguyên văn dòng nó in ra. Không gọi tool nào khác ngoài python_repl." | tail -3 | sed 's/^/   /'
check "the skill's results came from DeepSeek Search" 'last | grep -q "(DeepSeek Search)"'
"$HA" shutdown --force >/dev/null 2>&1

echo "################ 5. a coder's commits are by Harness Agents"
agent "$RX/p" n5
say n5 "Kiểm thử harness: dùng tool delegate giao cho đúng một agent role coder nhiệm vụ: tạo file who.txt chứa đúng một dòng COMMITTED rồi commit trong worktree của nó. Đợi coder xong, rồi gọi integrate_children với apply: true. Trả lời đúng một dòng: kết quả của integrate_children." | tail -3 | sed 's/^/   /'
authors=$(cd "$RX/p" && git log --format='%an <%ae>' -5)
echo "$authors" | sed 's/^/   author: /'
check "the work was integrated" '[ -f "$RX/p/who.txt" ]'
check "ha's commits carry Harness Agents" 'echo "$authors" | grep -q "^Harness Agents <harness-agents@localhost>"'
check "no commit carries harness-p5" '! (echo "$authors" | grep -q "harness-p5")'
"$HA" shutdown --force >/dev/null 2>&1

result
