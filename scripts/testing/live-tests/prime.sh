#!/usr/bin/env bash
# What 0.5.8 ported from prime-agent 8d58e5b, checked with a real model: the
# python_repl name, the restore notice, background bash() collected on its
# notice, the Bash card, the prompt stash, Ctrl-C stopping a summary, children
# in the session's cost, /new --name, per-model prompt additions, the
# compaction threshold, ha exec waiting for its children, ACP's max_tokens
# with a models.json maxTokens, extra certificate authorities, and an MCP
# server's registered OAuth client and callback port. Every check prints PASS
# or FAIL; the evidence stays in $LIVE_ROOT/prime.
#
#   bash scripts/testing/live-tests/prime.sh
set -u
source "$(dirname "$0")/env.sh"
fresh prime
export HA_AUTO_REFINE=off HA_PYTHON=${HA_PYTHON:-$PY}
# A Windows path where there is one: Python cannot open /f/... paths.
HERE=$(cd "$(dirname "$0")" && (pwd -W 2>/dev/null || pwd))
project "$RX/p"
screen() { "$PY" "$HERE/screen.py" "$1"; }
seconds() { awk '/^## [0-9]/{s=""} /^_\([0-9]+s\)_$/{gsub(/[_()s]/,""); s=$0} END{print s}' "$LOG"; }
MODEL_ID=${MODEL#*/}
PROVIDER=${MODEL%%/*}
# A price, so the session's cost is a figure.
cat > "$HA_HOME/models.json" <<EOF
{"providers": {"$PROVIDER": {"modelOverrides": {"$MODEL_ID": {"cost": {"input": 0.3, "output": 1.2}}}}}}
EOF

echo "################ 1. the REPL tool is python_repl"
agent "$RX/p" r1
say r1 "Dùng tool chạy Python của bạn tính 6*7 và in RESULT=<kết quả>. Cho biết chính xác tên tool bạn đã gọi." | tail -4
check "the cell ran" "last | grep -q 'RESULT=42'"
check "the tool is called python_repl" "last | grep -q 'python_repl'"

echo "################ 2. a revived kernel says cwd and env were reset"
say r1 "Trong python_repl: tạo thư mục sub, os.chdir('sub'), rồi in os.getcwd()." | tail -2
"$HA" shutdown --force >/dev/null 2>&1
(cd "$RX/p" && "$PROBE" "$RX/resume.bin" "$HA" "wait:10000" -- chat --resume latest --model "$MODEL" --approval full-auto >/dev/null)
say r1 "Trong python_repl chạy print('CWD=' + os.getcwd()). Harness có thông báo gì về thư mục làm việc và biến môi trường khi kernel khởi động lại không? Trích nguyên văn câu đó." | tail -4
check "the notice says the working directory was reset" "last | grep -qi 'working directory and environment variables were reset'"

echo "################ 3. background bash() is not awaited; its notice brings the result"
say r1 "Trong python_repl chạy lệnh nền h = bash('sleep 25 && echo XONG-NEN') rồi kết thúc lượt ngay, không chờ nó." | tail -2
took=$(seconds)
check "the turn did not wait for the command (${took}s)" "[ \"${took:-99}\" -lt 25 ]"
sleep 45
say r1 "Lệnh nền vừa rồi in ra gì? Trả lời đúng chuỗi nó in." | tail -3
check "its output came back through the notice" "last | grep -q 'XONG-NEN'"

echo "################ 4. a cell of bash() calls shows as a Bash card"
(cd "$RX/p" && "$PROBE" "$RX/card.bin" "$HA" "wait:4000,key:Trong python_repl chạy đúng một cell gồm hai dòng: a = await bash('git status') và b = await bash('git log --oneline -1'); rồi trả lời OK.,enter,wait:60000" -- attach r1 >/dev/null)
screen "$RX/card.bin" > "$RX/card.txt"
# The cell the model ran, from the store: prime's rule makes it a Bash card when
# at least half of its lines call bash(), else a Python card (a model that adds
# print lines gets one).
CARD=$("$PY" - "$HA_HOME/data/projects" <<'EOF'
import glob, json, os, sqlite3, sys
for db in glob.glob(os.path.join(sys.argv[1], "*", "harness.sqlite3")):
    for (text,) in sqlite3.connect(f"file:{db}?mode=ro", uri=True).execute("select request_json from frozen_requests order by rowid"):
        for message in json.loads(text).get("messages", []):
            for call in message.get("tool_calls") or []:
                code = json.loads(call.get("arguments") or "{}").get("code", "")
                if "git log --oneline -1" in code:
                    lines = [line for line in code.splitlines() if line.strip()]
                    print("Bash" if 2 * sum("bash(" in line for line in lines) >= len(lines) else "Python")
                    raise SystemExit
EOF
)
echo "   the cell makes a $CARD card"
check "the card follows the rule: $CARD" "grep -q '$CARD.*git status' '$RX/card.txt'"

echo "################ 5. a stashed draft returns after another prompt is sent"
(cd "$RX/p" && "$PROBE" "$RX/stash.bin" "$HA" "wait:4000,key:ban nhap do dang,wait:300,ctrls,wait:500,key:Tra loi dung mot tu: OK,enter,wait:40000" -- attach r1 >/dev/null)
screen "$RX/stash.bin" > "$RX/stash.txt"
check "the draft is back in the editor" "grep -q 'ban nhap do dang' '$RX/stash.txt'"

echo "################ 6. Ctrl-C stops a running summary"
# The checkpoints the store holds: a finished /compact writes one.
checkpoints() { "$PY" - "$HA_HOME/data/projects" <<'EOF'
import glob, os, sqlite3, sys
print(sum(sqlite3.connect(f"file:{db}?mode=ro", uri=True).execute("select count(*) from context_checkpoints").fetchone()[0]
          for db in glob.glob(os.path.join(sys.argv[1], "*", "harness.sqlite3"))))
EOF
}
before=$(checkpoints)
"$HA" send r1 "/compact" >/dev/null 2>&1
sleep 3
"$HA" abort r1 > "$RX/abort.txt" 2>&1
sleep 30
cat "$RX/abort.txt"
check "the summary was cancelled, not finished" "[ \"\$(checkpoints)\" -eq $before ]"
started=$(date +%s)
say r1 "Trả lời đúng một từ: PONG" | tail -1
check "the agent is free again at once ($(( $(date +%s) - started ))s)" "[ $(( $(date +%s) - started )) -lt 60 ] && last | grep -q 'PONG'"

echo "################ 7. the session's cost includes its children"
say r1 "Dùng tool delegate (role explorer, wait=true) giao cho một agent con: 'Trả lời đúng một từ: PONG'. Báo lại câu trả lời của nó." | tail -2
say r1 "/session" > "$RX/session.txt"
cat "$RX/session.txt" | grep -i "cost" | head -3
check "/session names the children's spend" "grep -qi 'children \\\$' '$RX/session.txt'"

echo "################ 8. /new --name names the new conversation"
say r1 "/new --name du-an-thu-nghiem -- Trả lời đúng một từ: XIN-CHAO" | tail -2
sleep 30
"$HA" list --json > "$RX/list.json"
check "the conversation is named" "grep -q 'du-an-thu-nghiem' '$RX/list.json'"
"$HA" shutdown --force >/dev/null 2>&1

echo "################ 9. per-model prompt additions"
# The rule matches the family of the model the suite runs on (deepseek-*, gpt-*).
printf '[[rule]]\nmatch = ["%s-*"]\nfiles = ["dk.md"]\n' "${MODEL_ID%%-*}" > "$HA_HOME/model-prompts.toml"
echo 'Kết thúc MỌI câu trả lời bằng một dòng cuối chỉ gồm đúng chuỗi ZEBRA-7.' > "$HA_HOME/dk.md"
agent "$RX/p" r9
say r9 "Thủ đô của Pháp là gì?" | tail -3
check "the model followed its own addition" "last | grep -q 'ZEBRA-7'"
"$HA" shutdown --force >/dev/null 2>&1
rm -f "$HA_HOME/model-prompts.toml" "$HA_HOME/dk.md"

echo "################ 10. compaction.thresholdPercent compacts earlier"
"$PY" -c "
import sys
lines = ['row %05d ' % i + 'lorem ipsum dolor sit amet consectetur ' * 6 for i in range(6000)]
open(sys.argv[1], 'w', encoding='utf-8').write('\n'.join(lines))" "$RX/p/big.txt"
cat > "$HA_HOME/models.json" <<EOF
{"providers": {"$PROVIDER": {"modelOverrides": {"$MODEL_ID": {"contextWindow": 200000, "cost": {"input": 0.3, "output": 1.2}}}}}}
EOF
echo '{"compaction": {"thresholdPercent": 20}}' > "$HA_HOME/settings.json"
agent "$RX/p" r10
# Six turns of about 7.5k tokens each (an earlier turn replays a result up to
# 32,000 characters): past 20% of the 200k window (40k), far below the default
# 95%. The summary of the earlier turns is kept in the store.
for i in 0 1 2 3 4 5; do
  say r10 "Trong python_repl chạy đúng một cell: print(open('big.txt', encoding='utf-8').read()[$((i * 30000)):$(((i + 1) * 30000))]) rồi trả lời đúng một từ: XONG$i" | tail -1
done
say r10 "Trả lời đúng một từ: PONG" | tail -1
summaries() { "$PY" - "$HA_HOME/data/projects" <<'EOF'
import glob, os, sqlite3, sys
count = 0
for db in glob.glob(os.path.join(sys.argv[1], "*", "harness.sqlite3")):
    c = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
    count += c.execute("select count(*) from session_settings where setting_key='rolling_summary'").fetchone()[0]
print(count)
EOF
}
check "the session compacted at 20% of the window" "[ \"\$(summaries)\" -gt 0 ]"
check "it still answers after compacting" "last | grep -q 'PONG'"
"$HA" shutdown --force >/dev/null 2>&1
rm -f "$HA_HOME/settings.json"

echo "################ 11. ha exec delegates and waits for its children"
mkdir -p "$RX/p/a" "$RX/p/b"; for i in 1 2 3; do echo x > "$RX/p/a/$i.txt"; done; for i in 1 2; do echo x > "$RX/p/b/$i.txt"; done
(cd "$RX/p" && "$HA" exec "Dùng tool delegate với wait=false để khởi chạy hai explorer song song: một đếm số file .txt trong thư mục a, một đếm trong thư mục b; mỗi explorer chỉ trả lời bằng một con số. Không tự đếm, không chờ chúng trong lượt này: kết thúc lượt ngay. Khi nhận được kết quả của cả hai thì trả lời đúng dạng A=<số> B=<số>." --model "$MODEL" --approval full-auto --output-format text > "$RX/exec.txt" 2>&1)
tail -5 "$RX/exec.txt"
check "the run waited and reported both children" "grep -q 'A=3' '$RX/exec.txt' && grep -q 'B=2' '$RX/exec.txt'"

echo "################ 12. ACP ends a capped answer with max_tokens"
cat > "$HA_HOME/models.json" <<EOF
{"providers": {"$PROVIDER": {"modelOverrides": {"$MODEL_ID": {"maxTokens": 120}}}}}
EOF
"$PY" - "$HA" "$MODEL" "$RX/p" > "$RX/acp.txt" 2>&1 <<'EOF'
import json, subprocess, sys
ha, model, cwd = sys.argv[1:4]
p = subprocess.Popen([ha, "--mode", "acp", "--model", model], stdin=subprocess.PIPE, stdout=subprocess.PIPE, cwd=cwd, text=True, encoding="utf-8")
def send(obj):
    p.stdin.write(json.dumps(obj) + "\n"); p.stdin.flush()
def wait(rid):
    for line in p.stdout:
        msg = json.loads(line)
        if msg.get("id") == rid:
            return msg
send({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": 1}}); wait(1)
send({"jsonrpc": "2.0", "id": 2, "method": "session/new", "params": {"cwd": cwd, "mcpServers": []}})
session = wait(2)["result"]["sessionId"]
send({"jsonrpc": "2.0", "id": 3, "method": "session/prompt", "params": {"sessionId": session, "prompt": [{"type": "text", "text": "Viết một bài luận dài 2000 từ về lịch sử Việt Nam, không dùng tool."}]}})
print("STOP=" + json.dumps(wait(3).get("result", {}).get("stopReason")))
p.stdin.close(); p.kill()
EOF
cat "$RX/acp.txt"
# The ChatGPT Codex backend takes no output cap, so ha sends none to it.
if [ "$PROVIDER" = "openai-codex" ]; then
  echo "SKIP  the capped answer ended with max_tokens: $PROVIDER takes no output cap"
else
  check "the capped answer ended with max_tokens" "grep -q 'STOP=\"max_tokens\"' '$RX/acp.txt'"
fi
rm -f "$HA_HOME/models.json"

echo "################ 13. extra certificate authorities keep the model reachable"
CA=$("$PY" -c "import certifi; print(certifi.where())" 2>/dev/null || true)
if [ -n "$CA" ]; then
  (export SSL_CERT_FILE="$CA"; agent "$RX/p" r13; say r13 "Trả lời đúng một từ: PONG" | tail -1; "$HA" shutdown --force >/dev/null 2>&1)
  check "a turn runs with SSL_CERT_FILE set" "last | grep -q 'PONG'"
else
  echo "FAIL  certifi is missing from PY: no PEM bundle to test with"; fail=$((fail+1))
fi

echo "################ 14. an MCP server's registered client and callback port"
read -r MCP_PORT CB_PORT < <("$PY" -c "import socket
a=socket.socket(); a.bind(('127.0.0.1',0)); b=socket.socket(); b.bind(('127.0.0.1',0))
print(a.getsockname()[1], b.getsockname()[1])")
"$PY" - "$MCP_PORT" "$CB_PORT" > "$RX/oauth-server.log" 2>&1 <<'EOF' &
import http.server, json, socket, sys
port, callback = int(sys.argv[1]), int(sys.argv[2])
held = socket.socket(); held.bind(("127.0.0.1", callback)); held.listen()  # another program holds the port
base = f"http://127.0.0.1:{port}"
class H(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        if "oauth-protected-resource" in self.path:
            body = {"resource": base + "/mcp", "authorization_servers": [base]}
        elif "oauth-authorization-server" in self.path or "openid-configuration" in self.path:
            body = {"issuer": base, "authorization_endpoint": base + "/authorize", "token_endpoint": base + "/token",
                    "response_types_supported": ["code"], "code_challenge_methods_supported": ["S256"]}
        else:
            self.send_response(401); self.end_headers(); return
        data = json.dumps(body).encode()
        self.send_response(200); self.send_header("Content-Type", "application/json"); self.send_header("Content-Length", str(len(data))); self.end_headers(); self.wfile.write(data)
    def log_message(self, *a): pass
http.server.HTTPServer(("127.0.0.1", port), H).serve_forever()
EOF
SERVER=$!
sleep 2
cat >> "$HA_HOME/config.toml" <<EOF

[mcp_servers.thu-nghiem]
transport = "streamable_http"
url = "http://127.0.0.1:$MCP_PORT/mcp"
oauth_client_id = "registered-client"
oauth_callback_port = $CB_PORT
EOF
(cd "$RX/p" && timeout 60 "$HA" mcp login thu-nghiem > "$RX/mcp-login.txt" 2>&1 < /dev/null)
kill $SERVER 2>/dev/null
cat "$RX/mcp-login.txt" | tail -3
check "the login used the registered port and said it is taken" "grep -q \"$CB_PORT\" '$RX/mcp-login.txt' && grep -qi 'in use' '$RX/mcp-login.txt'"

result
