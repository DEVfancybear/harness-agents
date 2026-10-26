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
#      each copy, where every copy was refused as ambiguous;
#   8. search_text on a path that is not there says it is missing and where a
#      file of that name is, where it said "path must be a directory".
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
echo "################ 8. a search in a path that is not there"
say s1 "Gọi tool search_text đúng một lần với query 'prepare_upload' và path 'src/net/upload.rs' (không có tham số nào khác), kể cả khi bạn nghĩ đường dẫn sai. Không gọi tool nào khác. In nguyên văn toàn bộ kết quả tool, kể cả lỗi." | tail -6 | sed 's/^/   /'
# What the tool answered, from its failure record rather than the model's copy.
refusal=$(grep -h '"tool":"search_text"' "$HA_HOME"/data/diagnostics/*.jsonl 2>/dev/null | grep '"event":"failed"' | tail -1)
echo "   refusal: $(echo "$refusal" | grep -o '"msg":"[^"]*"')"
check "search_text was refused for the path (the precondition)" '[ -n "$refusal" ]'
check "it said the path is not there, not that it must be a directory" 'echo "$refusal" | grep -q "path not found: src/net/upload.rs" && ! (echo "$refusal" | grep -q "must be a directory")'
check "it named where a file of that name is" 'echo "$refusal" | grep -q "the workspace has: src/upload.rs"'
"$HA" shutdown --force >/dev/null 2>&1

result
