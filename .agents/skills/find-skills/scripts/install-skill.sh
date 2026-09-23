#!/usr/bin/env bash

# Install one discovered skill into this project's .agents/skills directory.
# Usage: bash .agents/skills/find-skills/scripts/install-skill.sh owner/repo@skill-name

set -euo pipefail

if [[ $# -ne 1 || "$1" != *@* ]]; then
  echo "Usage: $0 <owner/repo@skill-name>" >&2
  exit 1
fi

source_repo="${1%@*}"
skill_name="${1##*@}"
if [[ ! "$source_repo" =~ ^[^/]+/[^/]+$ || ! "$skill_name" =~ ^[a-z0-9][a-z0-9-]*$ ]]; then
  echo "Error: expected owner/repo@skill-name" >&2
  exit 1
fi

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
project_root="$(cd -- "$script_dir/../../../.." && pwd -P)"
skill_target="$project_root/.agents/skills/$skill_name"
if [[ ! -d "$project_root/.agents/skills" ]]; then
  echo "Error: project skill directory is missing" >&2
  exit 1
fi
if [[ -e "$skill_target" ]]; then
  echo "Error: skill already exists in this project: $skill_name" >&2
  exit 1
fi

cd "$project_root"
npx skills add "$source_repo" --skill "$skill_name" -a codex --copy -y

if [[ ! -f "$skill_target/SKILL.md" ]]; then
  echo "Error: installation did not create $skill_target/SKILL.md" >&2
  exit 1
fi

echo "Installed $skill_name to $skill_target"
