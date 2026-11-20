#!/usr/bin/env bash
# The Anthropic models ha lacked or had wrong in 0.9.14 - claude-opus-5-5,
# claude-sonnet-5-5, claude-haiku-5-5, claude-fable-5-2 and the Mythos models
# claude-mythos-5-2, claude-mythos-5-1 and claude-mythos-5 - as
# `ha exec --model anthropic/<id>` takes them, with prime-agent's upstream
# catalog cached the way a day-old install has it (it lists Sonnet 5.5 with a
# $0.20 cache read and no way to turn its thinking off, and lacks the others
# but Opus 5.5):
#   1. each model is taken and its request reaches Anthropic's API - with no
#      Anthropic credential, under a key the API refuses (401), never an
#      unknown model or another provider; at `--thinking off` too;
#   2. with ANTHROPIC_LIVE=1 and an Anthropic key in HA_CREDENTIALS_DIR or
#      ANTHROPIC_API_KEY, each model answers a one-line task at `--thinking off`
#      and at `low`, and the run's cost is priced (its parts add up to a total
#      above 0). Without them these checks are skipped and said to be. The
#      Mythos models answer only an organization Anthropic has verified, so
#      they are left out of 2 unless MYTHOS_LIVE=1 too.
# Every check prints PASS or FAIL; the evidence stays in $LIVE_ROOT/claude-models.
#
#   bash scripts/testing/live-tests/claude-models.sh
set -u
source "$(dirname "$0")/env.sh"
fresh claude-models
export HA_AUTO_REFINE=off
P="$RX/p"
project "$P"
MODELS=(claude-opus-5-5 claude-sonnet-5-5 claude-haiku-5-5 claude-fable-5-2 claude-mythos-5-2 claude-mythos-5-1 claude-mythos-5)

# The upstream catalog, cached under the home's data directory as the daily
# refresh leaves it (fresh, so this run does not download it again).
mkdir -p "$HA_HOME/data/cache"
if curl -sSf -o "$HA_HOME/data/cache/provider-model-catalog.v1.json" \
  https://raw.githubusercontent.com/PrimeIntellect-ai/prime-agent-catalog/main/models/catalog.v1.json; then
  upstream=yes
else
  upstream=no
fi
check "the upstream catalog is cached (the precondition)" '[ "$upstream" = yes ]'

echo "== each model taken under a key Anthropic refuses"
# A credential directory of its own with nothing in it, so the refused key is
# the one this run sets and the user's own stay unread.
mkdir -p "$RX/no-credentials"
for id in "${MODELS[@]}"; do
  for level in off high; do
    (cd "$P" && HA_CREDENTIALS_DIR="$RX/no-credentials" ANTHROPIC_API_KEY="sk-ant-live-test-refused" \
      "$HA" exec --model "anthropic/$id" --thinking "$level" --approval full-auto \
      "Answer with the single word: ready." >"$RX/$id-$level.out" 2>"$RX/$id-$level.err")
    echo "   $id $level: $(tr -d '\r' < "$RX/$id-$level.err" | grep -i -m1 'error\|401\|refused' | cut -c1-160)"
    check "anthropic/$id at $level reaches Anthropic and is refused for the key" \
      'grep -qi "401\|authentication_error\|invalid x-api-key" "$RX/$id-$level.out" "$RX/$id-$level.err"'
    # 0.9.14 sent a model the catalog lacked to the configured provider, which
    # said it was not set up ("provider setup is incomplete: log in to ...").
    check "anthropic/$id at $level is neither unknown nor sent to another provider" \
      '! grep -qi "unknown model\|not in the catalog\|no such model\|model_not_found\|provider setup is incomplete" "$RX/$id-$level.out" "$RX/$id-$level.err"'
  done
done

echo "== each model answering (ANTHROPIC_LIVE=1)"
if [ "${ANTHROPIC_LIVE:-0}" = 1 ]; then
  for id in "${MODELS[@]}"; do
    case "$id" in
      claude-mythos-*) if [ "${MYTHOS_LIVE:-0}" != 1 ]; then echo "   $id skipped: set MYTHOS_LIVE=1 for a verified organization's key"; continue; fi ;;
    esac
    for level in off low; do
      (cd "$P" && "$HA" exec --json --model "anthropic/$id" --thinking "$level" --approval full-auto \
        "Answer with the single word: ready." >"$RX/live-$id-$level.json" 2>"$RX/live-$id-$level.err")
      priced=$("$PY" - "$RX/live-$id-$level.json" <<'PY'
import json, sys
try:
    run = json.load(open(sys.argv[1], encoding="utf-8"))
    cost = run["cost_usd"]
    parts = [cost[k] for k in ("uncached_input", "cache_read", "cache_write", "output")]
    ok = cost["total"] > 0 and abs(sum(parts) - cost["total"]) < 1e-5 and "ready" in json.dumps(run).lower()
except Exception:
    ok = False
print("yes" if ok else "no")
PY
)
      check "anthropic/$id at $level answers and its cost is priced" '[ "$(printf %s "$priced" | tr -d "\r")" = yes ]'
    done
  done
else
  echo "   skipped: set ANTHROPIC_LIVE=1 with an Anthropic key to run them"
fi

result
