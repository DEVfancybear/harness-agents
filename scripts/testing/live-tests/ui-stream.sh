#!/usr/bin/env bash
# What 0.9.3 changed, with a real model: GPT-6.1 in the catalog when the
# downloaded catalog lacks it, `/fast` on the GPT-6 models of the ChatGPT
# backend (and `/tier auto` refused there, which the backend answers with a
# 400), and the fullscreen TUI keeping up with a long answer: frames close
# together while it streams, the live block growing in many small steps, and a
# typed request wrapped between words. Every check prints PASS or FAIL; the
# recordings, frame logs and conversation stay in $LIVE_ROOT/ui-stream.
#
#   MODEL=openai-codex/gpt-6-luna THINKING=max bash scripts/testing/live-tests/ui-stream.sh
#
# MODEL must be on openai-codex: the fast-mode checks switch the session to
# openai-codex/gpt-6.1-terra and gpt-6.1-sol.
set -u
source "$(dirname "$0")/env.sh"
fresh ui-stream
export HA_AUTO_REFINE=off
HERE=$(cd "$(dirname "$0")" && (pwd -W 2>/dev/null || pwd))
project "$RX/p"
case "$MODEL" in
  openai-codex/*) ;;
  *) echo "PRECONDITION: MODEL must be an openai-codex model, not $MODEL"; exit 2 ;;
esac

echo "################ 1. gpt-6.1-terra with a downloaded catalog that lacks it"
# The catalog the upstream publishes, cached as a launch would have: 0.9.2
# replaced its own snapshot with it whole, and lost gpt-6.1-terra.
mkdir -p "$RX/home/data/cache"
curl -fsSL -o "$RX/home/data/cache/provider-model-catalog.v1.json" \
  "https://raw.githubusercontent.com/PrimeIntellect-ai/prime-agent-catalog/main/models/catalog.v1.json"
if grep -q '"gpt-6.1-terra"' "$RX/home/data/cache/provider-model-catalog.v1.json"; then
  echo "PRECONDITION: the upstream catalog now has gpt-6.1-terra; this check needs one that lacks it"
  exit 2
fi
check "the cached download is the upstream catalog (it names gpt-6.1-sol)" \
  'grep -q "\"gpt-6.1-sol\"" "$RX/home/data/cache/provider-model-catalog.v1.json"'
agent "$RX/p" s1
"$HA" send s1 "/model openai-codex/gpt-6.1-terra" --wait > "$RX/model-terra.txt" 2>&1
sed 's/^/   /' "$RX/model-terra.txt" | head -4
check "/model takes openai-codex/gpt-6.1-terra" '! grep -qiE "not found|unknown|no model" "$RX/model-terra.txt"'
say s1 "Reply with exactly one line: TERRA-OK" | tail -1
check "gpt-6.1-terra answered" 'last | grep -q TERRA-OK'
check "the session runs on gpt-6.1-terra" '"$HA" list --json | grep -q "gpt-6.1-terra"'

echo "################ 2. /fast on gpt-6.1-terra and gpt-6.1-sol, /tier auto refused"
"$HA" send s1 "/fast" --wait > "$RX/fast-terra.txt" 2>&1
sed 's/^/   /' "$RX/fast-terra.txt" | head -3
check "/fast turns fast mode on for gpt-6.1-terra" 'grep -q "Fast mode: on" "$RX/fast-terra.txt"'
say s1 "Reply with exactly one line: FAST-TERRA-OK" | tail -1
check "a turn on the priority tier is answered (the backend refuses a tier it does not take)" \
  'last | grep -q FAST-TERRA-OK'
"$HA" send s1 "/model openai-codex/gpt-6.1-sol" --wait >/dev/null 2>&1
"$HA" send s1 "/tier" --wait > "$RX/tier-sol.txt" 2>&1
sed 's/^/   /' "$RX/tier-sol.txt" | head -3
check "gpt-6.1-sol keeps the priority tier" 'grep -q "Service tier: priority" "$RX/tier-sol.txt"'
check "/tier offers no auto or flex on the ChatGPT backend" \
  'grep -q "available: default, priority)" "$RX/tier-sol.txt"'
say s1 "Reply with exactly one line: FAST-SOL-OK" | tail -1
check "gpt-6.1-sol answered in fast mode" 'last | grep -q FAST-SOL-OK'
"$HA" send s1 "/tier auto" --wait > "$RX/tier-auto.txt" 2>&1
sed 's/^/   /' "$RX/tier-auto.txt" | head -3
say s1 "Reply with exactly one line: AFTER-AUTO-OK" | tail -1
check "after /tier auto the next turn is still answered" 'last | grep -q AFTER-AUTO-OK'
"$HA" stop s1 >/dev/null 2>&1 || true

echo "################ 3. a long answer streams in small, close steps"
# Settings as a fresh home has them: on 0.9.2 the `/tier auto` above was kept
# as the default tier, and every later turn on the ChatGPT backend failed.
if [ -n "${THINKING:-}" ]; then
  printf '{"defaultThinkingLevel": "%s"}\n' "$THINKING" > "$RX/home/settings.json"
else
  rm -f "$RX/home/settings.json"
fi
PROMPT="Without using any tools write a long guide of about 1200 words on terminal rendering with several headings and bullet lists and one python code block. Then end with one line made of the word STREAM and the word FINISHED joined by a dash."
export HA_FRAME_LOG
HA_FRAME_LOG="$RX/frames.log"
(cd "$RX/p" && "$PROBE" "$RX/stream.bin" "$HA" "wait:7000,key:$PROMPT,enter,wait:3000,until:STREAM-FINISHED:240000,wait:3000" -- chat --model "$MODEL" "${TH[@]}" --approval full-auto >/dev/null)
unset HA_FRAME_LOG
check "the answer reached its end" 'grep -q "found STREAM-FINISHED" "$RX/stream.bin.marks"'
# Frames while the answer streamed: from the first that committed answer text
# to the scrollback to the last. Gaps between frames, and how many different
# sizes the live block was drawn at.
"$PY" - "$RX/frames.log" > "$RX/stream-stats.txt" <<'EOF'
import sys
rows = [line.split() for line in open(sys.argv[1], encoding="utf-8") if line.strip()]

streams = [i for i, r in enumerate(rows) if "Stream" in r[7]]
if len(streams) < 2:
    print("0 0 0 0")
    sys.exit()
window = rows[streams[0]:streams[-1] + 1]
gaps = sorted(int(b[0]) - int(a[0]) for a, b in zip(window, window[1:]))
p90 = gaps[int(len(gaps) * 0.9)] if gaps else 0
p50 = gaps[len(gaps) // 2] if gaps else 0
sizes = {r[8] for r in window if len(r) > 8 and r[8].startswith("live=")}
print(len(window), p50, p90, len(sizes))
EOF
read -r frames p50 p90 sizes < "$RX/stream-stats.txt"
echo "   while streaming: $frames frames, gap p50 ${p50} ms, p90 ${p90} ms, $sizes live-block sizes"
# 0.9.2: a frame every 62 ms at the median (the loop waited 50 ms for a key
# before it took the worker's frames, which came at most every 33 ms).
check "frames come every 33 ms or less at the median and 60 ms at p90 while the answer streams" \
  '[ "$frames" -gt 50 ] && [ "$p50" -le 33 ] && [ "$p90" -le 60 ]'
check "the live block grew in many small steps (over 150 sizes)" '[ "$sizes" -gt 150 ]'

echo "################ 4. the typed request wraps between words"
"$PY" "$HERE/screen.py" "$RX/stream.bin" --upto "$(awk '$2 ~ /^until:/ { print $1; exit }' "$RX/stream.bin.marks")" > "$RX/request.txt"
"$PY" - "$RX/request.txt" "$PROMPT" > "$RX/request-check.txt" <<'EOF'
import sys
rows = open(sys.argv[1], encoding="utf-8").read().splitlines()
prompt = sys.argv[2]
# The request's card: the rows after the first one holding its first words,
# each starting at the rail.
body = []
for row in rows:
    if "▎" not in row:
        if body:
            break
        continue
    text = row.split("▎", 1)[1].strip()
    if not body and not text.startswith(prompt[:20]):
        continue
    if text:
        body.append(text)
print("ok" if " ".join(body) == prompt else "split: " + " | ".join(body))
EOF
sed 's/^/   /' "$RX/request-check.txt"
check "every word of the request is whole on screen" 'grep -qx ok "$RX/request-check.txt"'

result
