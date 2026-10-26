"""Read the memory stores of a live test's home (docs/architecture/MEMORY_STORE.en.md).

A scope directory - DATA/harness, or DATA/sessions/<task>/harness - holds
memory.sqlite3; it is opened read-only, so reading never imports or migrates.
A harness_state.json beside it is waiting to be imported on ha's next open:
`count` and `rows` include its entries, as the store will. An entry is named by
its key: the `key` column since schema 3 (0.8.2), the `name` column before it.

usage:
  memory_store.py entries DATA            scope kind name vN | content (global and every conversation)
  memory_store.py rows DIR [KIND...]      name|path|pinned|content|project|version|via of each entry
  memory_store.py count DIR [KIND...]     how many entries (memory and prompt by default)
  memory_store.py events DIR              operation|cause|actor type|name|vN|content of each event, oldest first
  memory_store.py document DIR            the whole state as JSON, harness_state.json's shape
  memory_store.py row DIR KIND NAME       id|tree|meta|temporal_start|temporal_end|version|content of one row
  memory_store.py named DIR KIND TREE NAME   the key of the entry with that name in that tree (schema 3)
"""

import glob
import json
import os
import sqlite3
import sys

sys.stdout.reconfigure(encoding="utf-8")


def connect(directory):
    path = os.path.join(directory, "memory.sqlite3")
    if not os.path.exists(path):
        return None
    return sqlite3.connect(f"file:{path}?mode=ro", uri=True)


def key(conn, table):
    """The column a table keeps an entry's key in."""
    columns = [column[1] for column in conn.execute(f"pragma table_info({table})")]
    return "key" if "key" in columns else "name"


def stored(directory):
    """(kind, name) -> entry, the store's and a pending harness_state.json's."""
    entries = {}
    conn = connect(directory)
    if conn is not None:
        column = key(conn, "memory")
        for kind, name, doc in conn.execute(f"select kind, {column}, doc from memory order by kind, {column}"):
            entries[(kind, name)] = json.loads(doc)
        conn.close()
    pending = os.path.join(directory, "harness_state.json")
    if os.path.exists(pending):
        state = json.load(open(pending, encoding="utf-8"))
        for kind, records in (state.get("entries") or {}).items():
            for name, entry in (records or {}).items():
                entries[(kind, name)] = entry
    return entries


def line(entry):
    return str(entry.get("content")).replace("\n", " ")


def main(command, target, *kinds):
    if command == "entries":
        scopes = [("global", os.path.join(target, "harness"))]
        scopes += [("local", d) for d in sorted(glob.glob(os.path.join(target, "sessions", "*", "harness")))]
        for scope, directory in scopes:
            for (kind, name), entry in stored(directory).items():
                print(scope, kind, name, "v%s" % entry.get("version"), "|", line(entry)[:80])
    elif command == "rows":
        wanted = kinds or ("memory", "prompt")
        for (kind, name), entry in stored(target).items():
            if kind not in wanted:
                continue
            project = entry.get("project")
            project = project.get("name", "-") if isinstance(project, dict) else "-"
            print("|".join([
                name,
                str(entry.get("path", "general")),
                str((entry.get("metadata") or {}).get("pinned", False)),
                line(entry),
                project,
                "v%s" % entry.get("version"),
                str((entry.get("origin") or {}).get("via", "-")),
            ]))
    elif command == "count":
        wanted = kinds or ("memory", "prompt")
        print(sum(1 for (kind, _) in stored(target) if kind in wanted))
    elif command == "events":
        conn = connect(target)
        if conn is None:
            return
        for operation, cause, actor, name, version, doc in conn.execute(
            f"select operation, cause, actor, {key(conn, 'memory_event')}, version, doc"
            " from memory_event order by at, rowid"
        ):
            print("|".join([operation, str(cause), str(json.loads(actor).get("type")), name,
                            "v%s" % version, line(json.loads(doc))]))
        conn.close()
    elif command == "row":
        kind, name = kinds
        conn = connect(target)
        if conn is None:
            return
        row = conn.execute(
            "select id, tree, meta, temporal_start, temporal_end, version, content from memory"
            f" where kind = ? and {key(conn, 'memory')} = ?",
            (kind, name),
        ).fetchone()
        conn.close()
        if row:
            print("|".join("" if value is None else str(value).replace("\n", " ") for value in row))
    elif command == "document":
        conn = connect(target)
        document = {"schema": 1, "entries": {}, "refinements": []}
        if conn is not None:
            for kind, name, doc in conn.execute(f"select kind, {key(conn, 'memory')}, doc from memory"):
                document["entries"].setdefault(kind, {})[name] = json.loads(doc)
            document["refinements"] = [json.loads(e) for (e,) in conn.execute("select event from refinement order by seq")]
            conn.close()
        print(json.dumps(document, ensure_ascii=False))
    elif command == "named":
        kind, tree, name = kinds
        conn = connect(target)
        if conn is None or key(conn, "memory") != "key":
            return
        row = conn.execute(
            "select key from memory where kind = ? and tree = ? and name = ?", (kind, tree, name)
        ).fetchone()
        conn.close()
        if row:
            print(row[0])
    else:
        raise SystemExit(__doc__)


if __name__ == "__main__":
    if len(sys.argv) < 3:
        raise SystemExit(__doc__)
    main(*sys.argv[1:])
