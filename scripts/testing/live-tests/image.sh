#!/usr/bin/env bash
# An image a message names, budgeted by its pixels (bug report 20261119-115208):
# a pasted 153 KiB PNG raised the request's estimate by 58k tokens, its base64
# counted as text, and an image a little over a megabyte - the app attaches up
# to 8 MiB - made the request "mandatory context requires ... tokens" more than
# the window. Here `ha exec` names a 1.7 MB PNG (its top a red band, the rest
# noise that does not compress):
#   1. the run answers what the image shows, so the image reached the model -
#      named mid-sentence, and at a sentence's end (its full stop once stayed on
#      the name, which then read the PNG to the 256 KiB text ceiling and sent it
#      cut off: "could not parse image");
#   2. no request was refused as too large for the window;
#   3. the request's estimate stays near the tokens the provider counted.
# Every check prints PASS or FAIL; the evidence stays in $LIVE_ROOT/image.
#
#   bash scripts/testing/live-tests/image.sh
set -u
source "$(dirname "$0")/env.sh"
fresh image
export HA_AUTO_REFINE=off
P="$RX/p"
project "$P"
"$PY" - "$P/shot.png" <<'EOF'
import os, random, struct, sys, zlib
width, height, band = 1024, 768, 200
random.seed(7)
rows = []
for y in range(height):
    if y < band:
        row = bytes([255, 0, 0]) * width
    else:
        row = bytes(random.getrandbits(8) for _ in range(width * 3))
    rows.append(b"\x00" + row)
def chunk(kind, data):
    return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data) & 0xffffffff)
png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)) \
    + chunk(b"IDAT", zlib.compress(b"".join(rows), 9)) + chunk(b"IEND", b"")
open(sys.argv[1], "wb").write(png)
print(f"   shot.png: {len(png)} bytes, {width}x{height}")
EOF
size=$(wc -c < "$P/shot.png")
check "the image is over a megabyte (the precondition)" '[ "$size" -gt 1048576 ]'

echo "== ha exec naming the image mid-sentence"
(cd "$P" && "$HA" exec --model "$MODEL" "${TH[@]}" --approval full-auto "Look at shot.png and tell me the colour of the band across its top, in one English word." >"$RX/exec.out" 2>"$RX/exec.err")
tail -3 "$RX/exec.out" | sed 's/^/   /'
check "the run answers what the image shows" 'grep -qi red "$RX/exec.out"'
check "no request was refused as too large for the window" '! grep -qi "mandatory context\|context_overflow\|mandatory_context_overflow" "$RX/exec.out" "$RX/exec.err" "$HA_HOME"/data/diagnostics/*.jsonl'

echo "== ha exec naming the image at a sentence's end"
(cd "$P" && "$HA" exec --model "$MODEL" "${TH[@]}" --approval full-auto "Look at shot.png. What colour is the band across its top? Answer with one English word." >"$RX/exec2.out" 2>"$RX/exec2.err")
tail -3 "$RX/exec2.out" | sed 's/^/   /'
grep -i "image attached" "$RX/exec2.err" | sed 's/^/   /'
check "the image named at a sentence's end is attached whole and read" 'grep -qi red "$RX/exec2.out" && ! grep -qi "could not parse image" "$RX/exec2.err"'

# The first model call carries the image: its estimate against the input the
# provider counted. Text alone is estimated about a tenth over.
read -r estimated input <<< "$("$PY" - "$HA_HOME/data/diagnostics" <<'EOF'
import glob, json, os, sys
calls = []
for path in glob.glob(os.path.join(sys.argv[1], "*-exec-*.jsonl")):
    for line in open(path, encoding="utf-8"):
        r = json.loads(line)
        f = r.get("fields", {})
        if r.get("kind") == "provider" and r.get("event") == "call" and f.get("estimated") and f.get("input"):
            calls.append((r["ms"], f["estimated"], f["input"]))
calls.sort()
print(*(calls[0][1:] if calls else (0, 0)))
EOF
)"
estimated=$(printf '%s' "${estimated:-0}" | tr -d '\r'); input=$(printf '%s' "${input:-0}" | tr -d '\r')
echo "   first call: estimated ${estimated:-0}, input ${input:-0}"
check "the provider counted the call (the precondition)" '[ "${input:-0}" -gt 0 ]'
check "the estimate stays near what the provider counted" '[ "${estimated:-0}" -le $(( ${input:-0} * 3 / 2 + 5000 )) ]'

result
