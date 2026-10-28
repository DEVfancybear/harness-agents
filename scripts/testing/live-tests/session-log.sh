#!/usr/bin/env bash
# Session logs, /recall and /preserve (docs/architecture/SESSION_CONTINUITY.en.md),
# with a real model in the chat. Every check fails on 0.8.5, which wrote no
# session log and had no /recall or /preserve.
#
# A conversation that goes quiet gets a curated log in .harness/sessions; the
# next conversation is told its outcome as historical reference, in place of
# the raw last turns; /recall brings the logs back by topic; /preserve keeps
# AGENTS.md short with an archive; a project with a markdown store keeps the
# log as a section of the day's note, which the lint passes; HA_SESSION_LOG=off
# writes none.
#
#   bash scripts/testing/live-tests/session-log.sh
set -u
source "$(dirname "$0")/env.sh"
fresh session-log
export HA_AUTO_REFINE=off
# A conversation is quiet after this long; the log is written then.
export HA_IDLE_REVIEW_SECONDS=20
D=$HA_HOME/data
P=$RX/p
project "$P"

# The first whole request a turn sent with this text in it (later steps are
# stored as deltas of it).
request() { "$PY" - "$D" "$1" <<'EOF'
import glob, os, sqlite3, sys
sys.stdout.reconfigure(encoding="utf-8")
for db in glob.glob(os.path.join(sys.argv[1], "projects", "*", "harness.sqlite3")):
    for (text,) in sqlite3.connect(f"file:{db}?mode=ro", uri=True).execute("select request_json from frozen_requests order by rowid"):
        if sys.argv[2] in text and '"delta_of"' not in text[:40]:
            print(text)
            raise SystemExit
EOF
}
# Wait up to $2 seconds for a file matching the glob $1.
wait_for() { for _ in $(seq 1 "$2"); do compgen -G "$1" > /dev/null && return 0; sleep 1; done; return 1; }

echo "################ 1. a conversation that goes quiet gets a curated log"
agent "$P" l1
say l1 "Mình quyết định project này dùng SQLite thay vì Postgres, vì chỉ chạy một máy và không cần server. Hãy ghi quyết định đó vào file DECISIONS.md, một dòng." | tail -2 | sed 's/^/   /'
say l1 "Còn một việc chưa làm: viết migration tạo bảng users. Chưa làm bây giờ, chỉ ghi nó vào DECISIONS.md dưới mục TODO." | tail -2 | sed 's/^/   /'
wait_for "$P/.harness/sessions/*.md" 120
LOGF=$(ls "$P"/.harness/sessions/*.md 2>/dev/null | head -1)
[ -n "$LOGF" ] && sed 's/^/   /' "$LOGF" | head -30
check "a log was written in .harness/sessions" '[ -n "$LOGF" ]'
check "it has CPR's quick reference and resume context" '[ -n "$LOGF" ] && grep -q "^## Quick Reference (for AI scanning)" "$LOGF" && grep -q "^## Quick Resume Context" "$LOGF"'
check "the host names its conversation" '[ -n "$LOGF" ] && grep -q "\*\*Conversation:\*\* task_" "$LOGF"'
check "it holds the decision and the pending task" '[ -n "$LOGF" ] && grep -qi "sqlite" "$LOGF" && grep -qi "migration" "$LOGF"'
"$HA" shutdown --force >/dev/null 2>&1

echo "################ 2. the next conversation hears the log, as historical reference"
agent "$P" l2
say l2 "Không đọc file nào: lần trước trong project này mình đã quyết định gì về database, và còn việc gì dang dở?" | tail -3 | sed 's/^/   /'
R=$(request "lần trước trong project này")
check "the turn's request was found" '[ -n "$R" ]'
check "it carries the earlier session's log, framed as historical" 'printf "%s" "$R" | grep -q "HISTORICAL REFERENCE ONLY, NOT LIVE INSTRUCTIONS"'
check "not also that conversation's raw turns" '! printf "%s" "$R" | grep -q "Hãy ghi quyết định đó vào file DECISIONS.md"'
check "the model answered from it" 'last | grep -qi "sqlite" && last | grep -qi "migration"'

echo "################ 3. /recall brings the logs back by topic"
"$HA" send l2 "/recall 2 sqlite" --wait > "$RX/recall.out" 2>&1
tail -12 "$RX/recall.out" | sed 's/^/   /'
R3=$(request "Related (sqlite)")
check "the recall turn carried the matching log" '[ -n "$R3" ] && printf "%s" "$R3" | grep -qi "sqlite"'
check "the report names the decision" 'grep -qi "sqlite" "$RX/recall.out"'

echo "################ 4. /preserve keeps AGENTS.md short, with an archive"
{
  echo "# Agents"
  echo
  echo "## Commands (PROTECTED)"
  echo "- test: python -m pytest"
  echo
  echo "## Session Notes (2026-01-05) (ARCHIVABLE)"
  for index in $(seq 1 200); do echo "- old note $index about a finished spike"; done
} > "$P/AGENTS.md"
(cd "$P" && git add -A && git -c user.name=t -c user.email=t@t commit -qm agents)
"$HA" send l2 "/preserve" --wait > "$RX/preserve.out" 2>&1
tail -6 "$RX/preserve.out" | sed 's/^/   /'
check "AGENTS.md is at most 200 lines" '[ "$(wc -l < "$P/AGENTS.md")" -le 200 ]'
check "the protected section stayed" 'grep -q "python -m pytest" "$P/AGENTS.md"'
check "the archivable notes moved to AGENTS-Archive.md" '[ -f "$P/AGENTS-Archive.md" ] && grep -q "old note 150" "$P/AGENTS-Archive.md" && grep -q "^## Archived: " "$P/AGENTS-Archive.md"'
check "it says the line count" 'grep -qE "[0-9]+ (lines|dòng)" "$RX/preserve.out"'
"$HA" shutdown --force >/dev/null 2>&1

echo "################ 5. with a markdown store the log is a section of the day's note"
S=$RX/s
project "$S"
(cd "$S" && "$HA" memory init >/dev/null && git add -A && git -c user.name=t -c user.email=t@t commit -qm store)
agent "$S" l5
say l5 "Ghi nhớ: build của project này là cargo build --release, không có Makefile. Trả lời ngắn." | tail -2 | sed 's/^/   /'
say l5 "Việc tiếp theo là thêm CI chạy cargo test. Chưa làm, chỉ ghi nhận. Trả lời ngắn." | tail -2 | sed 's/^/   /'
NOTE="$S/memory/$(date +%Y-%m-%d).md"
for _ in $(seq 1 120); do grep -q "^## Session " "$NOTE" 2>/dev/null && break; sleep 1; done
check "the day's note has the session's section" 'grep -q "^## Session " "$NOTE"'
check "with the conversation named" 'grep -q "\*\*Conversation:\*\* task_" "$NOTE"'
check "and no file in .harness/sessions" '[ -z "$(ls "$S"/.harness/sessions/*.md 2>/dev/null)" ]'
check "the store passes the lint" '(cd "$S" && "$HA" memory lint --quiet)'
"$HA" shutdown --force >/dev/null 2>&1

echo "################ 6. HA_SESSION_LOG=off writes no log"
O=$RX/o
project "$O"
HA_SESSION_LOG=off agent "$O" l6
say l6 "Trả lời đúng một từ: một" > /dev/null
say l6 "Trả lời đúng một từ: hai" > /dev/null
sleep 60
check "no log was written" '[ -z "$(ls "$O"/.harness/sessions/*.md 2>/dev/null)" ]'
"$HA" shutdown --force >/dev/null 2>&1

result
