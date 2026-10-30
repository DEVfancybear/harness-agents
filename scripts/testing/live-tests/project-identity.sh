#!/usr/bin/env bash
# Which project a directory is (0.8.8, docs/architecture/PROJECT_IDENTITY.en.md),
# with a real model through `ha exec`. Four model turns. Every check after a
# precondition fails on 0.8.7, which keyed a project by the directory ha ran in.
#
# A project memory entry saved from a repository's root reaches the turn when
# ha runs in a subdirectory and in a linked worktree, all three in one store;
# a store a subdirectory kept under its own key (the previous layout) moves to
# the repository's key on the next launch from there, its conversation with it.
#
#   bash scripts/testing/live-tests/project-identity.sh
# Sections 3 and 4 (0.8.9) fail on 0.8.8: a moved repository found its store
# again by nothing, and there was no `ha project rekey`.
set -u
source "$(dirname "$0")/env.sh"
fresh project-identity
export HA_AUTO_REFINE=off HA_SELF_EVAL=off HA_SESSION_LOG=off
D=$HA_HOME/data
P=$RX/p
SUB=$P/src/app
W=$RX/wt
project "$P"
mkdir -p "$SUB"
git -C "$P" worktree add -q --detach "$W" HEAD >"$RX/worktree.log" 2>&1

run_exec() {
  local name="$1" dir="$2"; shift 2
  printf '\n## %s (ha exec %s in %s)\n\n> %s\n\n' "$(date +%H:%M:%S)" "$name" "$dir" "${*: -1}" >> "$LOG"
  (cd "$dir" && "$HA" exec --model "$MODEL" "${TH[@]}" --approval full-auto --output-format json "$@" >"$RX/$name.json" 2>"$RX/$name.err")
  "$PY" -c "import json,sys; print(json.load(open(sys.argv[1],encoding='utf-8')).get('response',''))" "$RX/$name.json" >> "$LOG" 2>/dev/null
}
# The first whole request with this text in it, from any store (a later step is a delta).
request() { "$PY" - "$D" "$1" <<'EOF'
import glob, os, sqlite3, sys
sys.stdout.reconfigure(encoding="utf-8")
for db in sorted(glob.glob(os.path.join(sys.argv[1], "projects", "*", "harness.sqlite3"))):
    for (text,) in sqlite3.connect(f"file:{db}?mode=ro", uri=True).execute("select request_json from frozen_requests order by rowid"):
        if sys.argv[2] in text and '"delta_of"' not in text[:40]:
            print(text)
            raise SystemExit
EOF
}
# The project stores under the data directory, one name per line.
stores() { for s in "$D"/projects/*/harness.sqlite3; do [ -f "$s" ] && basename "$(dirname "$s")"; done; }
# The global memory entry whose content holds $1, kept for a project: its project key.
project_key_of_entry() { "$HA" memory export | "$PY" -c "
import json, sys
code = sys.argv[1]
state = json.load(sys.stdin)
state = state.get('document', state)
for entry in state.get('entries', {}).get('memory', {}).values():
    project = entry.get('project')
    key = project.get('key') if isinstance(project, dict) else project
    if code in json.dumps(entry, ensure_ascii=False) and key:
        print(key)
        break
" "$1"; }

CODE="ZEBRA-$("$PY" -c 'import secrets; print(secrets.token_hex(4).upper())')"

echo "################ 1. a project entry saved at the root reaches a subdirectory and a linked worktree"
run_exec root "$P" "Harness test: call the memory tool exactly once with action write, scope project, title 'Build flag', content: This project's build flag is $CODE. Do nothing else, then answer DONE."
KEY=$(project_key_of_entry "$CODE")
if [ -z "$KEY" ]; then
  echo "PRECONDITION FAILED: the model kept no project entry with $CODE (see $RX/root.json); rerun"
  exit 2
fi
echo "precondition: the entry is kept for project $KEY"
run_exec sub "$SUB" "Answer exactly one line: OK-SUB. Do not call any tool."
run_exec tree "$W" "Answer exactly one line: OK-TREE. Do not call any tool."
RS=$(request "OK-SUB")
RT=$(request "OK-TREE")
if [ -z "$RS" ] || [ -z "$RT" ]; then
  echo "PRECONDITION FAILED: a turn left no request (see $RX/sub.err, $RX/tree.err); rerun"
  exit 2
fi
check "the subdirectory's turn carries the root's project entry" 'printf "%s" "$RS" | grep -q "$CODE"'
check "the linked worktree's turn carries the root's project entry" 'printf "%s" "$RT" | grep -q "$CODE"'
check "the root, the subdirectory and the worktree share one store" '[ "$(stores | wc -l)" -eq 1 ]'
check "and it is the store of the entry's project" '[ "$(stores)" = "$KEY" ]'

echo "################ 2. a store a subdirectory kept under its own key is adopted"
Q=$RX/q
X=$Q/app
mkdir -p "$X"
before=$(stores | sort)
# Not yet in a repository: the launch keys the store by the directory itself,
# the key a subdirectory had before 0.8.8.
run_exec old "$X" "Answer exactly one line: OK-OLD. Do not call any tool."
OLD=$(comm -13 <(printf '%s\n' "$before") <(stores | sort) | head -1)
if [ -z "$OLD" ] || [ -z "$(request OK-OLD)" ]; then
  echo "PRECONDITION FAILED: the turn in $X left no store of its own (see $RX/old.err); rerun"
  exit 2
fi
echo "precondition: $X kept its conversation in $OLD"
# Now the directory is a subdirectory of a repository.
project "$Q"
(cd "$X" && "$HA" sessions list --json >"$RX/adopt-sub.json" 2>"$RX/adopt-sub.err")
(cd "$Q" && "$HA" sessions list --json >"$RX/adopt-root.json" 2>"$RX/adopt-root.err")
NEW=$(comm -13 <(printf '%s\n' "$before") <(stores | sort) | head -1)
count() { "$PY" -c "import json,sys; print(len(json.load(open(sys.argv[1],encoding='utf-8'))['sessions']))" "$1" 2>/dev/null || echo 0; }
check "the old key's store is gone" '[ ! -e "$D/projects/$OLD" ]'
check "the store moved to the repository's key" '[ -n "$NEW" ] && [ "$NEW" != "$OLD" ] && [ -f "$D/projects/$NEW/harness.sqlite3" ]'
check "project.json records the key it adopted" 'grep -q "$OLD" "$D/projects/$NEW/project.json"'
check "the subdirectory still lists its conversation" '[ "$(count "$RX/adopt-sub.json")" -ge 1 ]'
check "the repository's root lists it too" '[ "$(count "$RX/adopt-root.json")" -ge 1 ]'
check "the adoption is in the diagnostics" 'grep -rh "\"store_adopted\"" "$D/diagnostics" | grep -q .'
# A second launch changes nothing.
(cd "$X" && "$HA" sessions list --json >"$RX/adopt-again.json" 2>"$RX/adopt-again.err")
check "a second launch finds the same store" '[ "$(count "$RX/adopt-again.json")" -ge 1 ] && [ "$(comm -13 <(printf "%s\n" "$before") <(stores | sort))" = "$NEW" ]'

echo "################ 3. a repository moved and renamed keeps its store and its project memory"
# 0.8.9: project.json records the repository's root commit and origin remote;
# a launch whose key has no store adopts the store whose identity matches and
# whose directory is gone. A clone that still exists keeps its own store.
M=$RX/m
project "$M"
git -C "$M" remote add origin https://example.invalid/team/moved-repo.git
MC=$RX/m-clone
git clone -q "$M" "$MC" 2>>"$RX/worktree.log"
git -C "$MC" remote set-url origin https://example.invalid/team/moved-repo.git
CODE3="OTTER-$("$PY" -c 'import secrets; print(secrets.token_hex(4).upper())')"
run_exec mroot "$M" "Harness test: call the memory tool exactly once with action write, scope project, title 'Release flag', content: This project's release flag is $CODE3. Do nothing else, then answer DONE."
MKEY=$(project_key_of_entry "$CODE3")
if [ -z "$MKEY" ] || [ ! -f "$D/projects/$MKEY/harness.sqlite3" ]; then
  echo "PRECONDITION FAILED: the model kept no project entry with $CODE3 in a store of its own (see $RX/mroot.json); rerun"
  exit 2
fi
echo "precondition: the entry is kept for project $MKEY"
before=$(stores | sort)
run_exec mclone "$MC" "Answer exactly one line: OK-CLONE. Do not call any tool."
CKEY=$(comm -13 <(printf '%s\n' "$before") <(stores | sort) | head -1)
if [ -z "$CKEY" ] || [ -z "$(request OK-CLONE)" ]; then
  echo "PRECONDITION FAILED: the clone's turn left no store of its own (see $RX/mclone.err); rerun"
  exit 2
fi
echo "precondition: the clone keeps its conversation in $CKEY"
before=$(stores | sort)
mkdir -p "$RX/moved"
mv "$M" "$RX/moved/renamed-repo"
MM=$RX/moved/renamed-repo
run_exec mmoved "$MM" "Answer exactly one line: OK-MOVED. Do not call any tool."
RM=$(request "OK-MOVED")
if [ -z "$RM" ]; then
  echo "PRECONDITION FAILED: the turn in the moved repository left no request (see $RX/mmoved.err); rerun"
  exit 2
fi
NEWM=$(stores | sort | comm -13 <(printf '%s\n' "$before") - | head -1)
check "the moved repository's turn carries its project entry" 'printf "%s" "$RM" | grep -q "$CODE3"'
check "its old key's store is gone and no store was added" '[ ! -e "$D/projects/$MKEY" ] && [ "$(stores | sort)" != "$before" ] && [ "$(stores | wc -l)" -eq "$(printf "%s\n" "$before" | wc -l)" ]'
check "the store that moved is the one the turn used" '[ -n "$NEWM" ] && grep -q "$MKEY" "$D/projects/$NEWM/project.json"'
check "project.json records the repository's identity" 'grep -q "moved-repo" "$D/projects/$NEWM/project.json" && grep -q "\"roots\"" "$D/projects/$NEWM/project.json"'
check "the move is in the diagnostics" 'grep -rh "\"store_followed\"" "$D/diagnostics" | grep -q .'
check "the clone that still exists keeps its own store" '[ -f "$D/projects/$CKEY/harness.sqlite3" ] && [ "$CKEY" != "$NEWM" ]'
(cd "$MC" && "$HA" sessions list --json >"$RX/clone-again.json" 2>"$RX/clone-again.err")
check "and a launch from the clone still opens it" '[ -f "$D/projects/$CKEY/harness.sqlite3" ] && [ "$(stores | wc -l)" -eq "$(printf "%s\n" "$before" | wc -l)" ]'

echo "################ 4. ha project rekey moves a store by hand, never over one that holds data"
Y=$RX/y
mkdir -p "$Y"
before=$(stores | sort)
# Not a repository: nothing identifies it once it moves.
run_exec yold "$Y" "Answer exactly one line: OK-YOLD. Do not call any tool."
YKEY=$(comm -13 <(printf '%s\n' "$before") <(stores | sort) | head -1)
if [ -z "$YKEY" ] || [ -z "$(request OK-YOLD)" ]; then
  echo "PRECONDITION FAILED: the turn in $Y left no store of its own (see $RX/yold.err); rerun"
  exit 2
fi
mv "$Y" "$RX/y-moved"
Z=$RX/y-moved
(cd "$Z" && "$HA" project rekey "$Y" >"$RX/rekey.out" 2>&1); rekey=$?
sed 's/^/   /' "$RX/rekey.out"
ZKEY=$(stores | sort | comm -13 <(printf '%s\n' "$before") - | head -1)
check "rekey moved the store to the directory's key" '[ "$rekey" -eq 0 ] && [ ! -e "$D/projects/$YKEY" ] && [ -n "$ZKEY" ] && [ "$ZKEY" != "$YKEY" ] && [ -f "$D/projects/$ZKEY/harness.sqlite3" ]'
(cd "$Z" && "$HA" sessions list --json >"$RX/rekey-list.json" 2>"$RX/rekey-list.err")
check "its conversation is listed from the new place" '[ "$(count "$RX/rekey-list.json")" -ge 1 ]'
check "project.json records the key it took" 'grep -q "$YKEY" "$D/projects/$ZKEY/project.json"'
# A store that holds data is never merged into.
W=$RX/w
mkdir -p "$W"
before=$(stores | sort)
run_exec wown "$W" "Answer exactly one line: OK-WOWN. Do not call any tool."
WKEY=$(comm -13 <(printf '%s\n' "$before") <(stores | sort) | head -1)
if [ -z "$WKEY" ]; then
  echo "PRECONDITION FAILED: the turn in $W left no store of its own (see $RX/wown.err); rerun"
  exit 2
fi
(cd "$Z" && "$HA" project rekey "$WKEY" >"$RX/rekey-refused.out" 2>&1); refused=$?
sed 's/^/   /' "$RX/rekey-refused.out"
check "a rekey into a store that holds data is refused, both kept" '[ "$refused" -ne 0 ] && grep -q "never merged" "$RX/rekey-refused.out" && [ -f "$D/projects/$WKEY/harness.sqlite3" ] && [ -f "$D/projects/$ZKEY/harness.sqlite3" ]'

result
