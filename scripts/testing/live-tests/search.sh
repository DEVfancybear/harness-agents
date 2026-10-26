#!/usr/bin/env bash
# What 0.7.5 changed, after fsearch's tools for coding agents, with a real model:
#   1. find_files finds a file through typos (`confg lodaer`, `mian`), best
#      match first, which 0.7.4 had no tool for;
#   2. find_files filters by git status and names it on each hit;
#   3. search_text's any_of finds every spelling in one call;
#   4. search_text puts the line that defines the name first;
#   5. search_text, finding nothing for an identifier, tries its other
#      spellings and says so;
#   6. (0.7.8) find_files naming several files, no path holding them all,
#      lists the files matching the most words instead of nothing;
#   7. (0.7.9) edit_file with one edit per copy of the same text changes
#      each copy, where every copy was refused as ambiguous.
# And what 0.8.2 took from agentic-file-search:
#   8. search_text with several words no line holds ranks the files by BM25:
#      the file holding the rare word leads, where a word on every line of
#      the first files filled the 512-match cut and the rare one was lost;
#   9. list_files with preview shows a code file's outline (the lines that
#      declare something, with their numbers) in the same call;
#  10. find_files matching only some words puts the file matching the rare
#      word first, ahead of newer, shorter paths matching a common word;
#  11. read_file names the other workspace files the text it shows refers
#      to (the "backtrack" phase);
#  12. asked what a folder of documents says, without being asked to cite,
#      the agent follows the reference to the file that holds the answer and
#      cites it as path:line (the system prompt's three phases and citations).
# Every check prints PASS or FAIL; the conversation stays in $LIVE_ROOT/search
# as evidence.
#
#   bash scripts/testing/live-tests/search.sh
set -u
source "$(dirname "$0")/env.sh"
fresh search
export HA_AUTO_REFINE=off HA_PYTHON=${HA_PYTHON:-$PY}
P="$RX/p"
mkdir -p "$P/src/config" "$P/tests" "$P/web"
cat > "$P/src/main.rs" <<'EOF'
fn main() {
    let file = std::path::Path::new("a.txt");
    upload::prepare_upload(file);
}
EOF
printf 'pub fn load() {}\n' > "$P/src/config/loader.rs"
printf 'mod loader;\n' > "$P/src/config/mod.rs"
printf '#[test]\nfn t() {}\n' > "$P/tests/main_test.rs"
cat > "$P/src/a_use.rs" <<'EOF'
pub fn run(file: &std::path::Path) {
    crate::upload::prepare_upload(file);
}
EOF
cat > "$P/src/upload.rs" <<'EOF'
// prepare_upload reads the file before it is sent.
pub fn prepare_upload(file: &std::path::Path) -> usize {
    file.as_os_str().len()
}
EOF
printf 'export const prepareUpload = (file) => file.length;\n' > "$P/web/upload.ts"
# 8: six files with "value" on every line, then the one file with "checksum".
mkdir -p "$P/rank"
for name in a b c d e f; do
  for i in $(seq 1 300); do printf 'let value = value + %s;\n' "$i"; done > "$P/rank/$name.rs"
done
printf 'fn verify(checksum: u32) -> bool {\n    checksum > 0\n}\n' > "$P/rank/zz_ledger.rs"
# 10: the rare word's file is the oldest and has the longest path.
mkdir -p "$P/f/zz/deep/more" "$P/f/src" "$P/f/web" "$P/f/lib" "$P/f/tests"
printf 'x\n' > "$P/f/zz/deep/more/ledger.rs"
touch -d '2 hours ago' "$P/f/zz/deep/more/ledger.rs"
for path in src/handler.rs web/handler.ts lib/handler.py tests/handler_test.rs; do printf 'x\n' > "$P/f/$path"; done
# 11, 12: the terms send the reader to an annex whose name says nothing of fees;
# a superseded draft holds another figure.
mkdir -p "$P/docs"
printf '# Service terms\n\n1. Scope: hosting and support.\n2. Fees: the monthly service fee is set out in annex_q.md.\n' > "$P/docs/01_terms.md"
printf '# Annex Q\n\nMonthly service fee: 4,250 EUR, invoiced on the first day of each month.\n' > "$P/docs/annex_q.md"
printf '# Drafting history\n\nAn early draft proposed a monthly service fee of 3,900 EUR; it was withdrawn.\n' > "$P/docs/02_history.md"
(cd "$P" && git init -q && git add -A && git -c user.name=t -c user.email=t@t commit -qm init)
printf 'mod loader; // changed\n' > "$P/src/config/mod.rs"
printf 'if a {\n    retry();\n}\nif b {\n    retry();\n}\n' > "$P/dup.txt"

agent "$P" s1
echo "################ 1. find_files through typos"
say s1 "Kiểm thử harness: gọi tool find_files đúng một lần với query 'confg lodaer'. In nguyên văn toàn bộ kết quả tool, không gọi tool nào khác." | tail -4 | sed 's/^/   /'
check "a find_files result came back" 'last | grep -q "find_files"'
check "'confg lodaer' found src/config/loader.rs" 'last | grep -q "src/config/loader.rs"'
say s1 "Gọi tool find_files đúng một lần với query 'mian'. In nguyên văn toàn bộ kết quả tool." | tail -4 | sed 's/^/   /'
check "'mian' put src/main.rs first" 'last | grep -E "src/main.rs|tests/main_test.rs" | head -1 | grep -q "src/main.rs"'

echo "################ 2. find_files by git status"
say s1 "Gọi tool find_files đúng một lần với query 'git:modified'. In nguyên văn toàn bộ kết quả tool." | tail -4 | sed 's/^/   /'
check "git:modified listed the changed file with its status" 'last | grep -qE "src/config/mod.rs.*\[modified\]"'
check "git:modified left the clean files out" '! (last | grep -q "src/config/loader.rs")'

echo "################ 3. any_of finds every spelling in one call"
say s1 "Gọi tool search_text đúng một lần với query 'prepare_upload' và any_of ['prepareUpload']. In nguyên văn toàn bộ kết quả tool." | tail -6 | sed 's/^/   /'
check "one search found both spellings" 'last | grep -q "web/upload.ts" && last | grep -q "src/upload.rs"'

echo "################ 4. the definition comes first"
say s1 "Gọi tool search_text đúng một lần với query 'prepare_upload' (không có tham số nào khác). In nguyên văn toàn bộ kết quả tool, giữ đúng thứ tự." | tail -6 | sed 's/^/   /'
check "the defining line led the result" 'last | grep -E "^[^ ]*(src/[a-z_]+\.rs):[0-9]+" | head -1 | grep -q "src/upload.rs:2"'

echo "################ 5. another spelling when nothing matched"
say s1 "Gọi tool search_text đúng một lần với query 'PrepareUpload' (không có tham số nào khác). In nguyên văn toàn bộ kết quả tool." | tail -6 | sed 's/^/   /'
check "the search said it found another spelling" 'last | grep -q "0 matches for .PrepareUpload.; found as"'
check "the other spellings were found" 'last | grep -q "src/upload.rs" && last | grep -q "web/upload.ts"'
echo "################ 6. several files named in one find_files query"
say s1 "Gọi tool find_files đúng một lần với query 'upload loader nginx' (không có tham số nào khác). In nguyên văn toàn bộ kết quả tool." | tail -8 | sed 's/^/   /'
check "each named file was found though no path holds every word" 'last | grep -q "src/upload.rs" && last | grep -q "src/config/loader.rs"'
check "it named the word no file name holds" 'last | grep -q "no files match .nginx."'
echo "################ 7. one edit per copy of the same text"
say s1 "Gọi tool edit_file đúng một lần trên dup.txt với tham số edits gồm đúng hai phần tử theo thứ tự: phần tử 1 có old_string là '    retry();' và new_string là '    retry(1);'; phần tử 2 có old_string là '    retry();' và new_string là '    retry(2);'. Không gọi tool nào khác. In nguyên văn kết quả tool." | tail -6 | sed 's/^/   /'
cat "$P/dup.txt" | sed 's/^/   dup.txt: /'
check "each edit took the next copy, in the file's order" '[ "$(tr -d "\r" < "$P/dup.txt")" = "$(printf "if a {\n    retry(1);\n}\nif b {\n    retry(2);\n}")" ]'
echo "################ 8. several words ranked by how rare they are"
say s1 "Gọi tool search_text đúng một lần với query 'checksum value' và path 'rank' (không có tham số nào khác). Không gọi tool nào khác. In nguyên văn 6 dòng đầu tiên của kết quả tool, giữ đúng thứ tự." | tail -8 | sed 's/^/   /'
check "the result says no line holds both words" 'last | grep -q "0 files match all of .checksum value."'
check "the file holding the rare word led the result" 'last | grep -oE "rank/[a-z_]+\.rs:[0-9]+" | head -1 | grep -q "rank/zz_ledger.rs"'
echo "################ 9. list_files previews a folder's code"
say s1 "Gọi tool list_files đúng một lần với path 'src' và preview 5. Không gọi tool nào khác. In nguyên văn toàn bộ kết quả tool." | tail -12 | sed 's/^/   /'
check "the preview showed src/upload.rs's outline" 'last | grep -q "src/upload.rs" && last | grep -q "outline"'
check "the outline named the declaration with its line number" 'last | grep -qE "2: pub fn prepare_upload"'
echo "################ 10. find_files: the rare word first"
say s1 "Gọi tool find_files đúng một lần với query 'handler ledger nginx' và path 'f' (không có tham số nào khác). Không gọi tool nào khác. In nguyên văn toàn bộ kết quả tool, giữ đúng thứ tự." | tail -8 | sed 's/^/   /'
check "every file matching a word was listed" '[ "$(last | grep -oE "f/(src|web|lib|tests|zz/deep/more)/[a-z_]+\.(rs|ts|py)" | sort -u | wc -l)" -eq 5 ]'
check "the file matching the rare word came first" 'last | grep -oE "f/(src|web|lib|tests|zz/deep/more)/[a-z_]+\.(rs|ts|py)" | head -1 | grep -q "f/zz/deep/more/ledger.rs"'
echo "################ 11. read_file names the files its text refers to"
say s1 "Gọi tool read_file đúng một lần với path 'docs/01_terms.md'. Không gọi tool nào khác. In nguyên văn toàn bộ kết quả tool, kể cả mọi ghi chú ở cuối." | tail -8 | sed 's/^/   /'
check "the read named docs/annex_q.md as a file its text refers to" 'last | grep -q "names other workspace files: docs/annex_q.md"'
echo "################ 12. a question about a folder: the reference followed, the answer cited"
agent "$P" s2
say s2 "Theo các tài liệu trong thư mục docs, phí dịch vụ hằng tháng hiện là bao nhiêu?" | tail -10 | sed 's/^/   /'
check "the answer is the annex's figure" 'last | grep -qE "4[.,]?250"'
check "the answer cites the annex as path:line, unasked" 'last | grep -qE "annex_q\.md:[0-9]+"'
"$HA" shutdown --force >/dev/null 2>&1

result
