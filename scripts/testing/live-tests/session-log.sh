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

# Sections 7-10 fail on 0.8.6: it wrote a log only on a quiet timer inside the
# running process (lost on /new, an exit or a kill), skipped a one-request task
# as too few turns, and left .harness/sessions out of .gitignore. The quiet
# timer is pushed out of reach here, so only the new triggers can write.

echo "################ 7. /new leaves a log for the conversation it leaves, at once"
N=$RX/n
project "$N"
HA_IDLE_REVIEW_SECONDS=3600 agent "$N" l7
say l7 "Ghi nhớ: cổng mặc định của dịch vụ này là 8081. Trả lời ngắn." | tail -1 | sed 's/^/   /'
say l7 "Việc còn lại: viết healthcheck cho cổng đó. Chưa làm, chỉ ghi nhận. Trả lời ngắn." | tail -1 | sed 's/^/   /'
check "no log before the conversation is left (the quiet timer is out of reach)" '[ -z "$(ls "$N"/.harness/sessions/*.md 2>/dev/null)" ]'
"$HA" send l7 "/new -- Trả lời đúng một từ: xong" --wait > "$RX/new.out" 2>&1
wait_for "$N/.harness/sessions/*.md" 120; sleep 2
NLOG=$(ls "$N"/.harness/sessions/*.md 2>/dev/null | head -1)
check "/new wrote the left conversation's log" '[ -n "$NLOG" ] && grep -q "8081" "$NLOG"'
NCONV=$( [ -n "$NLOG" ] && sed -n 's/^\*\*Conversation:\*\* //p' "$NLOG" | head -1)
check "nothing is owed for it any more" '[ -n "$NCONV" ] && [ -z "$(ls "$D"/projects/*/session-log/owed/"$NCONV" 2>/dev/null)" ]'
check ".harness/.gitignore lists sessions/ and the lock" 'grep -qx "sessions/" "$N/.harness/.gitignore" && grep -qx "\*.lock" "$N/.harness/.gitignore"'
check "the host's files are not the project's changes" '! (cd "$N" && git status --porcelain --untracked-files=all) | grep -qE "\.harness/(sessions|session-log\.lock)"'
# A coder started before the log is written finds the checkout it started
# from only when the .gitignore the host created is hidden too (0.8.7's first
# candidate listed it, and integrate_children refused the coder's work).
check "the .gitignore the host created is not a change either" '[ -z "$(cd "$N" && git ls-files --others --exclude-standard -- .harness)" ]'
"$HA" shutdown --force >/dev/null 2>&1

echo "################ 8. a one-request task with several tool calls gets a log"
T=$RX/t
project "$T"
agent "$T" l8
say l8 "Tạo năm file n1.txt đến n5.txt, mỗi file chứa đúng tên của nó, mỗi file bằng một lần gọi tool riêng; rồi đọc lại từng file bằng một lần gọi tool riêng. Trả lời ngắn." | tail -1 | sed 's/^/   /'
MADE=$(ls "$T"/n[1-5].txt 2>/dev/null | wc -l)
check "precondition: the one request ran its tool calls (5 files made)" '[ "$MADE" -eq 5 ]'
wait_for "$T/.harness/sessions/*.md" 120
check "the one-request task got a log" '[ "$MADE" -lt 5 ] || [ -n "$(ls "$T"/.harness/sessions/*.md 2>/dev/null)" ]'
"$HA" shutdown --force >/dev/null 2>&1

echo "################ 9. a chat killed right after a turn gets its log on the next start"
K=$RX/k
project "$K"
touch "$RX/k.stamp"; sleep 1
HA_IDLE_REVIEW_SECONDS=3600 agent "$K" l9
say l9 "Ghi nhớ: tên mã của bản phát hành tới là Orchid. Trả lời ngắn." | tail -1 | sed 's/^/   /'
say l9 "Việc còn lại: viết release notes cho Orchid. Chưa làm, chỉ ghi nhận. Trả lời ngắn." | tail -1 | sed 's/^/   /'
"$HA" shutdown --force >/dev/null 2>&1
OWEDF=$(find "$D"/projects/*/session-log/owed -type f -newer "$RX/k.stamp" 2>/dev/null | head -1); OWED=$(basename "${OWEDF:-}" 2>/dev/null)
check "precondition: killed with the log owed and none written" '[ -n "$OWED" ] && [ -z "$(ls "$K"/.harness/sessions/*.md 2>/dev/null)" ]'
HA_IDLE_REVIEW_SECONDS=3600 agent "$K" l9b
say l9b "Trả lời đúng một từ: chào" > /dev/null
wait_for "$K/.harness/sessions/*.md" 120; sleep 2
KLOG=$(ls "$K"/.harness/sessions/*.md 2>/dev/null | head -1)
check "the next start wrote the owed log" '[ -n "$KLOG" ] && grep -qi "orchid" "$KLOG" && grep -q "Conversation:\*\* $OWED" "$KLOG"'
check "and it is no longer owed" '[ -n "$OWEDF" ] && [ ! -e "$OWEDF" ]'
"$HA" shutdown --force >/dev/null 2>&1

# Section 10 fails on 0.8.7: a new conversation was offered the task state of
# a conversation still open in another agent, an abandoned task state was
# offered to every new conversation however many had passed it by, and no
# request carried one precedence rule for where things stand. Only the first
# turn needs the model (to keep a task state); the rest is the host's choice.
echo "################ 10. only a task that was left is offered, three times at most"
W=$RX/w
project "$W"
# The earlier task state's own words, in 0.8.7 and since.
OFFER="left its task unfinished"
# The precedence rule's own words.
RULE="When records of the work disagree"
offered() { printf "%s" "$1" | grep -q "$OFFER"; }
zstates() { grep -l "Zephyr" "$D"/sessions/*/harness/task_state.json 2>/dev/null | wc -l; }
HA_IDLE_REVIEW_SECONDS=3600 agent "$W" w1
say w1 "Dùng tool task_state để lưu một task, không làm gì khác: objective 'Viết parser cho định dạng Zephyr', checklist ba mục đều pending: lexer, grammar, tests. Chưa làm mục nào. Trả lời ngắn." | tail -1 | sed 's/^/   /'
ZSTATE=$(grep -l "Zephyr" "$D"/sessions/*/harness/task_state.json 2>/dev/null | head -1)
check "precondition: the first conversation kept a task state with open items" '[ -n "$ZSTATE" ] && grep -q "\"pending\"" "$ZSTATE"'
# A second conversation while the first is still open (its agent runs).
HA_IDLE_REVIEW_SECONDS=3600 agent "$W" w2
say w2 "Trả lời đúng một từ: táo" > /dev/null
RW2=$(request "Trả lời đúng một từ: táo")
check "the second conversation's request was found" '[ -n "$RW2" ]'
check "it is not offered the open conversation's task state" '[ -n "$RW2" ] && ! offered "$RW2"'
check "the request carries the precedence rule once" '[ "$(printf "%s" "$RW2" | grep -o "$RULE" | wc -l)" -eq 1 ]'
"$HA" shutdown --force >/dev/null 2>&1
# The first conversation is left now: it is offered to new conversations,
# three of them, and then no more.
for n in 1 2 3; do
  HA_IDLE_REVIEW_SECONDS=3600 agent "$W" "w3$n"
  say "w3$n" "Trả lời đúng một từ: lê$n" > /dev/null
  RW3=$(request "Trả lời đúng một từ: lê$n")
  check "precondition: new conversation $n is offered the left task" '[ -n "$RW3" ] && offered "$RW3"'
  "$HA" shutdown --force >/dev/null 2>&1
done
check "precondition: no later conversation adopted the task" '[ "$(zstates)" -eq 1 ]'
HA_IDLE_REVIEW_SECONDS=3600 agent "$W" w4
say w4 "Trả lời đúng một từ: mận" > /dev/null
RW4=$(request "Trả lời đúng một từ: mận")
check "the fourth new conversation's request was found" '[ -n "$RW4" ]'
check "an abandoned task is not offered a fourth time" '[ -n "$RW4" ] && ! offered "$RW4"'
check "the earlier conversations were shown it three times, as counted" '[ -n "$ZSTATE" ] && [ "$(tr -cd , < "$(dirname "$ZSTATE")/task_state.offered.json" 2>/dev/null | wc -c)" -eq 2 ]'
"$HA" shutdown --force >/dev/null 2>&1

result
