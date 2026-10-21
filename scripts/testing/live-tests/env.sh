# Source me: what every live test shares. Each value can be set before sourcing.
#
#   HA          the ha binary under test        (default: target/release/ha.exe)
#   PROBE       the pseudo-console driver        (default: target/debug/examples/pty_probe.exe;
#               build it with `cargo build -p harness-cli --example pty_probe`)
#   PY          a Python 3.11+ with the kernel's packages, for the checks and the REPL
#   MODEL       the model the agents run on      (default: opencode-go/deepseek-v4.1-flash)
#   THINKING    the agents' thinking level (off .. max), when set
#   LIVE_ROOT   where each test makes its own home and projects (default: $TEMP/ha-live)
#   HA_CONFIG   the config.toml to copy into each test's home (default: the user's)
#   HA_CREDENTIALS_DIR  the private directory holding the model's credentials (required)
# A Windows path where there is one (Git Bash): ConPTY cannot start /f/... paths.
REPO=$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && (pwd -W 2>/dev/null || pwd))
export HA=${HA:-$REPO/target/release/ha.exe}
export PROBE=${PROBE:-$REPO/target/debug/examples/pty_probe.exe}
export PY=${PY:-python}
export MODEL=${MODEL:-opencode-go/deepseek-v4.1-flash}
export LIVE_ROOT=${LIVE_ROOT:-${TEMP:-/tmp}/ha-live}
export HA_CONFIG=${HA_CONFIG:-${APPDATA:-$HOME/.config}/HarnessAgents/config.toml}
: "${HA_CREDENTIALS_DIR:?set HA_CREDENTIALS_DIR to the directory with the model credentials}"
export HA_CREDENTIALS_DIR
# Git Bash turns a leading /goal into a Windows path.
export MSYS_NO_PATHCONV=1 PYTHONIOENCODING=utf-8
mkdir -p "$LIVE_ROOT"

# A fresh home for one test: `fresh <name>` sets RX and HA_HOME.
fresh() {
  export RX="$LIVE_ROOT/$1"
  rm -rf "$RX"
  mkdir -p "$RX/home"
  cp "$HA_CONFIG" "$RX/home/"
  # THINKING (off .. max) sets the agents' thinking level, as the model's own
  # default may be lower than a run wants.
  if [ -n "${THINKING:-}" ]; then
    printf '{"defaultThinkingLevel": "%s"}\n' "$THINKING" > "$RX/home/settings.json"
  fi
  export HA_HOME="$RX/home" LOG="$RX/conversation.md"
  : > "$LOG"
}

# say <agent> <message>: send, wait for the answer, log both.
say() {
  local agent="$1"; shift
  local started
  started=$(date +%s)
  printf '\n## %s\n\n> %s\n\n' "$(date +%H:%M:%S)" "$*" >> "$LOG"
  local answer
  answer=$("$HA" send "$agent" "$*" --wait 2>&1)
  printf '%s\n\n_(%ss)_\n' "$answer" "$(( $(date +%s) - started ))" >> "$LOG"
  printf '%s\n' "$answer"
}

# What the agents answered, without the prompts (which name the values checked).
# A prompt is the first `> ` line after its heading; a quote in an answer stays.
ans() { awk '/^## [0-9]/{p=1} p && /^> /{p=0; next} {print}' "$LOG"; }
# The answer to the latest message only.
last() { awk '/^## [0-9]/{block=""; p=1} p && /^> /{p=0; next} {block=block"\n"$0} END{print block}' "$LOG"; }

pass=0; fail=0
check() { if eval "$2"; then echo "PASS  $1"; pass=$((pass+1)); else echo "FAIL  $1"; fail=$((fail+1)); fi; }
result() { echo "################ RESULT: $pass passed, $fail failed"; [ "$fail" -eq 0 ]; }

# agent <dir> <name>: start an agent in <dir> through a pseudo-console and name it.
agent() {
  (cd "$1" && "$PROBE" "$RX/$2.bin" "$HA" "wait:9000" -- chat --model "$MODEL" --approval full-auto >/dev/null)
  local id
  id=$("$HA" list --json | "$PY" -c "import json,sys; d=json.load(sys.stdin); a=d if isinstance(d,list) else d.get('agents',d); print([x['id'] for x in a if not x.get('name')][0])")
  "$HA" rename "$id" "$2" >/dev/null
}

# A git repository with one commit: `project <dir>`.
project() {
  mkdir -p "$1"
  (cd "$1" && git init -q && printf '# %s\n' "$(basename "$1")" > README.md && git add -A && git -c user.name=t -c user.email=t@t commit -qm init)
}
