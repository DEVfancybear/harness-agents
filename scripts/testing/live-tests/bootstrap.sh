#!/usr/bin/env bash
# `/knowledge bootstrap`, with a real model: the agent maps a project it has
# never seen into its knowledge library before any task needs it. After
# Recursa (arXiv 2611.04035), whose bootstrapping maps a domain's materials
# with parallel agents, every entry with its source, its scope and how sure it
# is, and records the boundaries of what was read. 0.9.4 had no such command:
# `/knowledge bootstrap` answered with the usage line.
#   1. the command starts a turn with the mapping request, and the turn writes
#      pages: one that routes, pages for the parts, one of boundaries;
#   2. a fact only one file holds is in a page with that file as its source;
#   3. no project file changed;
#   4. a new conversation answers from the library, naming the source.
# Every check prints PASS or FAIL; the conversation and the pages stay in
# $LIVE_ROOT/bootstrap as evidence.
#
#   bash scripts/testing/live-tests/bootstrap.sh
set -u
source "$(dirname "$0")/env.sh"
fresh bootstrap
export HA_AUTO_REFINE=off HA_SELF_EVAL=off HA_SESSION_LOG=off HA_PYTHON=${HA_PYTHON:-$PY}
D="$HA_HOME/data"
P="$RX/p"
project "$P"
mkdir -p "$P/ingest" "$P/billing" "$P/docs"
cat > "$P/README.md" <<'EOF'
# Parcel
Parcel has two parts: `ingest/` reads shipment files, `billing/` prices them.
Design notes are in `docs/`.
EOF
cat > "$P/ingest/reader.py" <<'EOF'
"""Reads shipment files dropped by carriers."""

# Carriers upload at most this many rows a file; larger files are split upstream.
MAX_ROWS = 48213


def read(path):
    with open(path, encoding="utf-8") as handle:
        return [line.rstrip("\n").split(";") for line in handle][:MAX_ROWS]
EOF
cat > "$P/billing/rates.py" <<'EOF'
"""Prices a shipment."""

# The surcharge code finance agreed for remote islands.
ISLAND_SURCHARGE_CODE = "PETREL-6093"


def price(weight_kg, remote=False):
    base = 4 + 2 * weight_kg
    return base * (1.5 if remote else 1.0)
EOF
cat > "$P/docs/design.md" <<'EOF'
# Design
Files arrive in `ingest/`, are priced by `billing/`, and nothing is stored: there is no database.
Tests are run with `python -m pytest`; there are none yet for `billing/`.
EOF
(cd "$P" && git add -A && git -c user.name=t -c user.email=t@t commit -qm parcel)
before=$(cd "$P" && git status --porcelain -- . ':!.harness' | wc -l)

# Whether a request this home sent holds $1.
sent() {
  "$PY" - "$D" "$1" <<'PYEOF'
import glob, os, sqlite3, sys
for db in glob.glob(os.path.join(sys.argv[1], "projects", "*", "harness.sqlite3")):
    c = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
    for (text,) in c.execute("select request_json from frozen_requests"):
        if sys.argv[2] in text:
            sys.exit(0)
sys.exit(1)
PYEOF
}
K="$P/.harness/knowledge"
pages() { find "$K" -name '*.md' -not -name 'index.md' -not -name 'log.md' 2>/dev/null; }

echo "################ 1. /knowledge bootstrap maps the project into the library"
agent "$P" b1
"$HA" send b1 "/knowledge bootstrap" --wait > "$RX/bootstrap.txt" 2>&1
# The mapping is a turn, with explorers of its own: wait for its pages.
for _ in $(seq 1 60); do
  [ "$(pages | wc -l)" -ge 3 ] && "$HA" send b1 "/knowledge status" --wait 2>/dev/null | grep -q "page" && break
  sleep 10
done
say b1 "Việc lập bản đồ đã xong chưa? Nếu còn agent con đang chạy thì đợi chúng xong và ghi nốt các trang. Rồi chỉ trả lời: XONG." | tail -1 | sed 's/^/   /'
"$HA" shutdown --force >/dev/null 2>&1
tail -6 "$RX/bootstrap.txt" | cut -c1-200 | sed 's/^/   /'
count=$(pages | wc -l)
echo "   pages written: $count"
pages | sed "s|$K/||; s/^/   - /"
check "the command started a turn with the mapping request" 'sent "Map this project into its knowledge library now"'
check "it did not answer with the usage line" '! grep -q "usage: /knowledge" "$RX/bootstrap.txt"'
check "three pages or more were written" '[ "$count" -ge 3 ]'
check "one page is of the boundaries: what was not read, what is not there" 'pages | xargs grep -liE "boundar|not read|not checked|unmapped|absent|ranh giới|chưa đọc" 2>/dev/null | grep -q .'

echo "################ 2. a fact one file holds is in a page, with the file as its source"
check "the row limit only ingest/reader.py holds is in a page" 'pages | xargs grep -l "48213" 2>/dev/null | grep -q .'
check "with that file named beside it as path:line" 'pages | xargs grep -hE "ingest/reader\.py:[0-9]+" 2>/dev/null | grep -q .'
check "the entries say how sure they are" 'pages | xargs grep -liE "read in the file|inferred|not checked|đã đọc|suy ra|chưa kiểm" 2>/dev/null | grep -q .'

echo "################ 3. the project itself was not changed"
after=$(cd "$P" && git status --porcelain -- . ':!.harness' | wc -l)
check "no project file was edited, added or removed" '[ "$before" -eq 0 ] && [ "$after" -eq 0 ]'
check "the files hold what they held" '(cd "$P" && git diff --quiet HEAD -- README.md ingest billing docs)'

echo "################ 4. a new conversation answers from the library"
agent "$P" b2
say b2 "Không đọc file mã nguồn nào (không dùng read_file, search_text, find_files hay shell); chỉ dùng tool knowledge. Mã phụ phí cho đảo xa trong project này là gì, và nó được ghi ở file nào, dòng nào?" > "$RX/answer.txt"
tail -3 "$RX/answer.txt" | sed 's/^/   /'
"$HA" shutdown --force >/dev/null 2>&1
check "precondition: the surcharge code is in a page" 'pages | xargs grep -l "PETREL-6093" 2>/dev/null | grep -q .'
check "the answer gives the code" 'grep -q "PETREL-6093" "$RX/answer.txt"'
check "and names the file it comes from" 'grep -q "billing/rates.py" "$RX/answer.txt"'

result
