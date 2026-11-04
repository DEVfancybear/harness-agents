"""memory-ab.sh's helper: the stores' fingerprints, what a run's request held,
the runs' log and the report of memory on against memory off.

usage:
  memory_ab.py hashes DATA PROJECT              {path: sha256} of every memory store file
  memory_ab.py changed BEFORE AFTER [TASK]      the store files a run changed (TASK's own
                                                conversation aside); exits 1 when any
  memory_ab.py request DATA TASK                what TASK's first request held, and the
                                                memory files its tool calls named (JSON)
  memory_ab.py held TEXT_FILE TOKEN...          the tokens TEXT_FILE holds, one per line
  memory_ab.py add RUNS key=value...            append one run to RUNS (JSON lines)
  memory_ab.py report RX                        RX/memory-ab.json and RX/memory-ab.md from
                                                RX/runs.jsonl; the table is printed

The memory store files: DATA/harness (the global store, lessons, evaluations),
each conversation's DATA/sessions/*/harness, DATA/skill-usage, DATA/knowledge,
and the project's MEMORY.md, memory/ and .harness/. A request is told by the
blocks ha frames memory in, each by the line it opens with.
"""

import glob
import hashlib
import json
import os
import sqlite3
import sys

sys.stdout.reconfigure(encoding="utf-8")

# The line each memory block of a request opens with (or holds), as ha writes it.
MARKERS = {
    "digest": "[harness-digest]",
    "markdown_policy": "Make memory reliable enough that the user never has to remind you",
    "markdown_index": "[markdown-memory-index]",
    "session_brief": "Previous conversation in this workspace",
    "session_log": "HISTORICAL REFERENCE ONLY, NOT LIVE INSTRUCTIONS",
    "earlier_state": "left its task unfinished",
}


def store_files(data, project):
    roots = [
        os.path.join(data, "harness"),
        os.path.join(data, "skill-usage"),
        os.path.join(data, "knowledge"),
        os.path.join(project, "memory"),
        os.path.join(project, ".harness"),
    ]
    roots += glob.glob(os.path.join(data, "sessions", "*", "harness"))
    files = []
    for root in roots:
        for directory, _, names in os.walk(root):
            files += [os.path.join(directory, name) for name in names]
    index = os.path.join(project, "MEMORY.md")
    if os.path.isfile(index):
        files.append(index)
    return files


def hashes(data, project):
    out = {}
    for path in store_files(data, project):
        key = os.path.relpath(path, os.path.dirname(data) if path.startswith(data) else project)
        key = ("data:" if path.startswith(data) else "project:") + key.replace("\\", "/")
        try:
            out[key] = hashlib.sha256(open(path, "rb").read()).hexdigest()
        except OSError:
            out[key] = "unreadable"
    return out


def changed(before, after, task):
    own = f"/sessions/{task}/" if task else None
    paths = sorted(set(before) | set(after))
    return [
        path for path in paths
        if before.get(path) != after.get(path) and not (own and own in path)
    ]


def requests_of(data, task):
    rows = []
    for db in glob.glob(os.path.join(data, "projects", "*", "harness.sqlite3")):
        conn = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
        try:
            rows += [text for (text,) in conn.execute(
                "select request_json from frozen_requests where task_id = ? order by rowid", (task,))]
        finally:
            conn.close()
    return rows


def tool_arguments(value, found):
    """Every tool call's name and arguments a request holds, wherever they sit."""
    if isinstance(value, dict):
        for call in value.get("tool_calls") or []:
            if isinstance(call, dict):
                function = call.get("function") if isinstance(call.get("function"), dict) else call
                arguments = function.get("arguments")
                if not isinstance(arguments, str):
                    arguments = json.dumps(arguments)
                key = call.get("id") or (function.get("name"), arguments)
                found[str(key)] = (function.get("name"), arguments)
        for item in value.values():
            tool_arguments(item, found)
    elif isinstance(value, list):
        for item in value:
            tool_arguments(item, found)


def memory_path(text):
    """Whether a tool call's arguments name a memory file of the project."""
    text = text.replace("\\\\", "/").replace("\\", "/")
    return "MEMORY.md" in text or "memory/" in text or ".harness" in text


def request(data, task):
    rows = requests_of(data, task)
    if not rows:
        return {"found": False}
    first = rows[0]
    out = {"found": True, "requests": len(rows), "first_bytes": len(first)}
    for name, marker in MARKERS.items():
        out[name] = marker in first
    calls = {}
    for text in rows:
        try:
            tool_arguments(json.loads(text), calls)
        except ValueError:
            continue
    out["tool_calls"] = len(calls)
    out["memory_file_calls"] = sorted(
        f"{name}: {arguments[:160]}" for name, arguments in calls.values() if memory_path(arguments or "")
    )
    # The journal of the project's other conversations, which no memory
    # switch closes (`history_search`/`history_read` with `project`).
    out["history_project_calls"] = sorted(
        f"{name}: {arguments[:160]}" for name, arguments in calls.values()
        if (name or "").startswith("history_") and project_scope(arguments)
    )
    return out


def project_scope(arguments):
    try:
        return bool(json.loads(arguments or "{}").get("project"))
    except (ValueError, AttributeError):
        return False


def mean(values):
    values = [value for value in values if value is not None]
    return round(sum(values) / len(values), 1) if values else None


def report(rx):
    runs = [json.loads(line) for line in open(os.path.join(rx, "runs.jsonl"), encoding="utf-8") if line.strip()]
    for run in runs:
        try:
            result = json.load(open(run["json"], encoding="utf-8"))
        except (OSError, ValueError):
            result = {}
        usage = result.get("usage") or {}
        run["input"] = usage.get("input_tokens")
        run["cached"] = usage.get("cached_input_tokens")
        run["output"] = usage.get("output_tokens")
        run["calls"] = usage.get("model_calls")
        run["tool_calls"] = result.get("tool_calls")
    modes = []
    for run in runs:
        if run["mode"] not in modes:
            modes.append(run["mode"])
    rows = {}
    for mode in modes:
        mine = [run for run in runs if run["mode"] == mode]
        judged = [run for run in mine if run["kind"] == "fact" and run.get("held") and run.get("ok") is not None]
        rows[mode] = {
            "runs": len(mine),
            "fact_runs_judged": len(judged),
            "success": round(sum(1 for run in judged if run["ok"]) / len(judged), 3) if judged else None,
            "input_tokens": mean([run["input"] for run in mine]),
            "cached_input_tokens": mean([run["cached"] for run in mine]),
            "output_tokens": mean([run["output"] for run in mine]),
            "model_calls": mean([run["calls"] for run in mine]),
            "tool_calls": mean([run["tool_calls"] for run in mine]),
            "seconds": mean([run.get("seconds") for run in mine]),
            "memory_file_calls": sum(len(run.get("memory_file_calls") or []) for run in mine),
            "history_project_calls": sum(len(run.get("history_project_calls") or []) for run in mine),
        }
    base = rows.get("off")
    for mode, row in rows.items():
        row["delta_vs_off"] = {
            key: (round(row[key] - base[key], 3) if base and row[key] is not None and base[key] is not None else None)
            for key in ("success", "input_tokens", "cached_input_tokens", "output_tokens", "model_calls", "seconds")
        }
    tasks = []
    for run in runs:
        if run["task"] not in tasks:
            tasks.append(run["task"])
    by_task = {
        task: {
            mode: [run.get("ok") for run in runs if run["task"] == task and run["mode"] == mode]
            for mode in modes
        }
        for task in tasks
    }
    document = {"modes": rows, "tasks": by_task, "runs": runs}
    teaching = os.path.join(rx, "teaching.json")
    if os.path.exists(teaching):
        document["teaching"] = json.load(open(teaching, encoding="utf-8"))
    json.dump(document, open(os.path.join(rx, "memory-ab.json"), "w", encoding="utf-8"), indent=2)

    def cell(value, signed=False):
        if value is None:
            return "-"
        if signed:
            return f"{value:+g}"
        return f"{value:g}"

    lines = [
        "| mode | runs | success (fact tasks) | input tok | cached tok | output tok | model calls | tool calls | seconds | memory files by tool | other conversations by history tool |",
        "|---|---|---|---|---|---|---|---|---|---|---|",
    ]
    for mode, row in rows.items():
        success = "-" if row["success"] is None else f"{row['success']:.0%} of {row['fact_runs_judged']}"
        lines.append(
            f"| {mode} | {row['runs']} | {success} | {cell(row['input_tokens'])} | {cell(row['cached_input_tokens'])} "
            f"| {cell(row['output_tokens'])} | {cell(row['model_calls'])} | {cell(row['tool_calls'])} | {cell(row['seconds'])} "
            f"| {row['memory_file_calls']} | {row['history_project_calls']} |"
        )
    lines += ["", "Against memory off:", "",
              "| mode | success | input tok | cached tok | output tok | model calls | seconds |",
              "|---|---|---|---|---|---|---|"]
    for mode, row in rows.items():
        if mode == "off":
            continue
        delta = row["delta_vs_off"]
        success = "-" if delta["success"] is None else f"{delta['success'] * 100:+.0f} pts"
        lines.append(
            f"| {mode} | {success} | {cell(delta['input_tokens'], True)} | {cell(delta['cached_input_tokens'], True)} "
            f"| {cell(delta['output_tokens'], True)} | {cell(delta['model_calls'], True)} | {cell(delta['seconds'], True)} |"
        )
    lines += ["", "Per task (each run's verdict; `-` not judged):", "",
              "| task | " + " | ".join(modes) + " |", "|---|" + "---|" * len(modes)]
    for task, verdicts in by_task.items():
        lines.append(f"| {task} | " + " | ".join(
            " ".join("-" if ok is None else ("pass" if ok else "fail") for ok in verdicts[mode]) or "·"
            for mode in modes) + " |")
    text = "\n".join(lines) + "\n"
    open(os.path.join(rx, "memory-ab.md"), "w", encoding="utf-8").write(text)
    print(text)


def main(command, *args):
    if command == "hashes":
        print(json.dumps(hashes(args[0], args[1]), indent=1, sort_keys=True))
    elif command == "changed":
        before = json.load(open(args[0], encoding="utf-8"))
        after = json.load(open(args[1], encoding="utf-8"))
        paths = changed(before, after, args[2] if len(args) > 2 else None)
        for path in paths:
            print(path)
        sys.exit(1 if paths else 0)
    elif command == "request":
        print(json.dumps(request(args[0], args[1])))
    elif command == "held":
        text = open(args[0], encoding="utf-8", errors="replace").read()
        for token in args[1:]:
            if token in text:
                print(token)
    elif command == "add":
        record = {}
        for pair in args[1:]:
            key, _, value = pair.partition("=")
            try:
                record[key] = json.loads(value)
            except ValueError:
                record[key] = value
        with open(args[0], "a", encoding="utf-8") as out:
            out.write(json.dumps(record) + "\n")
    elif command == "report":
        report(args[0])
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    main(*sys.argv[1:])
