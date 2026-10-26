# Memory store and memory-engine parity

English | [Tiếng Việt](MEMORY_STORE.vi.md)

**Status:** design, 26 October 2026. This is the plan for giving ha's memory the
features of [timescale/memory-engine](https://github.com/timescale/memory-engine)
(read at commit `9c9bdc1`, 23 October 2026). The work ships in seven phases. Each
section says which phase delivers it. Until a phase ships, the code is the
authority, not this document.

0.8.1 ships P1 to P4. P5 (CLI, formats, packs), P6 (importers) and P7 (MCP
server, integrations, console) are on hold until the maintainer resumes them.
P1 adds only the one CLI command its tests need: `ha memory export`.

## 1. Goal and decisions

memory-engine (ME) is a team memory server: Postgres with `ltree`, `pgvector` and
BM25. Agents reach it over MCP. ha keeps memory in `harness_state.json` files,
keeps knowledge as Markdown pages and keeps a history index in SQLite. The goal is
that every ME capability a single user can use also exists in ha, running locally.

The maintainer decided:

| Topic | Decision |
|---|---|
| Sharing | **Out of scope for now.** ha stays local and single-user: no spaces, principals, groups, grants, API keys, login, invites or remote backend. To be revisited later. |
| Memory storage | **One SQLite store per scope** with ME's record model: tree, name, meta, temporal, version, an event log, FTS5 and vectors. It replaces `harness_state.json`. |
| Knowledge | **Stays Markdown files** that people can edit by hand. It gains page history and revert, and shares one search with memory. |
| Filters | **Full parity:** tree exact/lquery/ltxtquery, `meta` containment, a JSONPath `metaPredicate`, five temporal modes and `grep`. |

What ha keeps, because ME has no equivalent:

- the digest injected into every turn, with pinned entries and constraints;
- the automatic review, Deep Dream and the skill curator;
- duplicate refusal on write;
- query variants (`queries`);
- the knowledge link graph.

ME deliberately has no extraction layer. In ha that layer stays, writing through
the new store.

## 2. Feature map

Status today: ✅ present, 🟡 partial, ❌ missing. P1 to P7 are the phases in §12.

| ME feature | Today | Phase |
|---|---|---|
| `content`, version and CAS | ✅ | P1 (kept) |
| Hierarchical `tree`, address by `tree/name`, immutable id | 🟡 | P1 |
| Free `meta` object, reserved `$prev`/`$next`/`$thread`, `display_name` | ❌ | P1 |
| `temporal` point or range | ❌ | P1 (stored) · P3 (queried) |
| `on_conflict` error / replace / ignore | 🟡 | P1 |
| Append-only event log: snapshot, actor, cause, `operation_id` | 🟡 | P1 |
| History by memory, path, subtree, operation or time feed, with cursor | 🟡 | P2 |
| Revert and undelete as a new forward version | ✅ memory · ❌ knowledge | P1 · P4 |
| Time-based retention (30 days) | ❌ | P1 |
| `tree` (counts per node), `count`, `mv`, `copy`, `delete_tree`, get/delete by path | ❌ | P2 |
| BM25 with the positive-match invariant | 🟡 | P3 |
| Semantic search on memory with a background embedding queue | ❌ | P3 |
| Weighted RRF hybrid; `weights`, `candidateLimit`, `semanticThreshold` per call | 🟡 | P3 |
| Filters `tree`, `meta`, `metaPredicate`, `temporal`, `grep`; filter-only browse with `order_by` | ❌ | P3 |
| `select` projection and `format` yaml/json/compact | 🟡 | P3 |
| Knowledge page history, revert and undelete | ❌ | P4 |
| One search over memory and knowledge | ❌ | P4 |
| History filters (`since`, `until`, `kind`); thread links on `history_read` | ❌ | P4 |
| CLI for memory, including `edit` | ❌ | P5 |
| Import and export in JSON, YAML, Markdown and NDJSON | ❌ | P5 |
| Memory packs (validate, install, list) | ❌ | P5 |
| Import docs (`--git-aware`, `--prune`) | 🟡 | P6 |
| Import git history | ❌ | P6 |
| Import Claude Code, Codex and OpenCode sessions | ❌ | P6 |
| MCP server offering memory tools to other harnesses | ❌ | P7 |
| Install harness integrations, with capture hooks | ❌ | P7 |
| Web UI for memory | 🟡 knowledge only | P7 |
| `doctor` / `status` for memory | 🟡 | P7 |

Out of scope: spaces, principals, groups, grants, API keys, service accounts,
invites, login, the CI import workflow, a remote backend, and the Granola and Slab
importers (one is macOS only, the other a niche service).

## 3. Storage (P1)

### 3.1 Files

A scope directory holds `memory.sqlite3` in place of `harness_state.json`:

| Scope | Directory | Tree root of its records |
|---|---|---|
| global | `<data>/harness/` | `/global/...`; a project-qualified entry goes under `/projects/<project_key>/...` |
| local (one conversation) | `<data>/sessions/<task>/harness/` | `/local/...` |

There is one database per scope directory, not one for all of ha:

- A turn reads exactly two stores, global and its own conversation's. The local
  memory of other conversations is never in scope.
- The two lock independently. `knowledge.sh` checks that a held global lock fails
  only the global part of a review.
- Tests and tools that point at a bare directory keep working.

The read-modify-write lock becomes `memory.lock` beside the database (today
`harness_state.json.lock`). The database runs in WAL mode with a 5 s busy
timeout. `PRAGMA user_version` carries the schema version; migrations are
ordered Rust functions, applied when the database is opened.

ha talks to it through `rusqlite` 0.39. That version links the same
`libsqlite3-sys` 0.37 (vendored SQLite 3.53.2) as `sqlx`, so there is one SQLite
library, called synchronously. The memory code is synchronous, so it does not
need a runtime bridge.

### 3.2 Schema

```sql
CREATE TABLE memory (
  id              TEXT PRIMARY KEY,      -- UUIDv7, immutable
  kind            TEXT NOT NULL,         -- prompt|memory|skill|subagent|factory|record
  name            TEXT NOT NULL,         -- ha's entry id (slug), unique per kind
  tree            TEXT NOT NULL,         -- '/global/user/preferences'
  title           TEXT NOT NULL,
  content         TEXT NOT NULL,
  meta            TEXT NOT NULL DEFAULT '{}',   -- JSON object (the entry's `metadata`)
  temporal_start  TEXT,                  -- RFC 3339 UTC, NULL = no temporal
  temporal_end    TEXT,                  -- exclusive; = start for a point
  doc             TEXT NOT NULL,         -- the whole entry as ha writes it (lossless)
  version         INTEGER NOT NULL,
  version_hash    TEXT NOT NULL,         -- 32 hex of SHA-256 over the canonical payload
  content_version INTEGER NOT NULL,      -- embedding guard (P3)
  created_at      TEXT NOT NULL,
  updated_at      TEXT NOT NULL,
  UNIQUE (kind, name)
);
CREATE INDEX memory_tree ON memory(tree);
CREATE INDEX memory_temporal ON memory(temporal_start, temporal_end);

CREATE TABLE memory_event (
  event_id     TEXT PRIMARY KEY,   -- UUIDv7: a chronological total order
  at           TEXT NOT NULL,
  memory_id    TEXT NOT NULL,      -- outlives the memory
  kind TEXT NOT NULL, name TEXT NOT NULL, tree TEXT NOT NULL,
  operation    TEXT NOT NULL,      -- insert|update|delete
  operation_id TEXT NOT NULL,      -- one per write transaction
  cause        TEXT,               -- create|update|delete|move|copy|revert|import|migrate|review|dream|curate|rollback|...
  actor        TEXT NOT NULL,      -- JSON: {"type": model|refine|kernel|migration|cli|mcp|import, ...origin}
  version      INTEGER NOT NULL,
  doc          TEXT NOT NULL       -- snapshot of the resulting state (the removed state for a delete)
);
CREATE INDEX memory_event_memory ON memory_event(memory_id, at, event_id);
CREATE INDEX memory_event_operation ON memory_event(operation_id);
CREATE INDEX memory_event_tree ON memory_event(tree);
CREATE INDEX memory_event_path ON memory_event(tree, name);
CREATE INDEX memory_event_at ON memory_event(at DESC);

CREATE TABLE refinement (seq INTEGER PRIMARY KEY, event TEXT NOT NULL);  -- the `refinements` array
CREATE TABLE store_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);     -- `schema` and other top-level fields
-- P3 (schema 2): memory_vector, memory_embedding_queue (§6)
```

How ha's entries map onto ME's record:

- **name.** ha's entry id. The current rule, the title slug with aliases kept when
  a title changes, still applies; ME's `name` regex is accepted too. Uniqueness is
  `(kind, name)` within a scope. ME's `(tree, name)` uniqueness is weaker than this.
- **tree.** The scope root, then the entry's `path` turned into labels:
  - each segment is accent-folded (NFKD without marks);
  - characters outside `[A-Za-z0-9_-]` become `_`;
  - empty segments are dropped;
  - an empty path becomes `general`.

  The original `path` is kept in `doc`. A tree under `/projects/<key>` is the
  "project" scope of today. A record without a project is global.
- **meta.** The entry's existing `metadata` object, so `pinned` is `meta.pinned`.
  Reserved keys, as in ME: `$prev`, `$next`, `$thread`, `display_name`, `pinned`.
  `meta` is replaced whole on update, as in ME.
- **temporal.** A point (`start` only) or a half-open range `[start, end)`. The
  inputs are a string, a list of one or two strings, or `{start, end?}`.
- **doc.** Holds everything ha writes, unknown fields included, so the document
  view in §3.3 round-trips exactly.

### 3.3 The document view (compatibility layer)

Three functions are how the code reads and writes the state today:

- `harness::load`, used by the digest;
- `refine::load_raw` and `refine::save_raw`, used by the `memory` tool, the review,
  Deep Dream, rollback and `/learn`;
- `harness_kernel::Store`, used by `rlm.harness`.

They keep their JSON document shape:

- `load_raw(dir)` builds `{"schema", "entries": {kind: {name: doc}}, "refinements"}`
  from the rows.
- `save_raw(dir, &state, &Writer)` diffs the new document against the rows. In
  one transaction it then:
  - inserts, updates and deletes the rows that changed;
  - writes one event per changed row, all sharing one `operation_id`;
  - replaces `refinement` when it changed;
  - bumps the memory-change counter (`memory_changes`) as today.
- `Writer { actor, cause }` is new and required. Every call site states who
  writes:
  - `model`: the `memory` tool;
  - `refine`: every refinement (`/refine`, the automatic review, Deep Dream,
    the curator), with the refinement's id, which `refinements.jsonl` explains,
    and cause `refine`, or `rollback` for a rollback;
  - `kernel`: `rlm.harness`;
  - `migration`: a legacy import;
  - `cli` and `mcp` once those land.

New features (filters, history, tree operations, search) query the rows directly.
The document view only serves existing callers, so their behaviour does not
change in P1.

### 3.4 Migration and legacy files

When a store opens and its directory holds a `harness_state.json`, the file's
entries are imported:

- each entry is upserted by `(kind, name)`;
- a differing payload replaces the stored one;
- refinements are appended unless their `id` is already present.

The file is then renamed `harness_state.json.imported`. In the same pass,
`memory_history.jsonl` becomes `migrate` events and is renamed too. Its records
are earlier states, so each becomes a snapshot at its own `version`, and
`history`/`revert` keep reaching versions written before the upgrade.

So the first open of an upgraded ha migrates the user's memory. The same rule
gives tests and users a way to drop entries in: write a `harness_state.json` and
it is imported on the next open.

Going back to an older ha: `ha memory export --legacy` (P5) writes a
`harness_state.json` that 0.8.x reads. The `.imported` file is the state from
before the upgrade.

### 3.5 Event log retention

Events older than 30 days are dropped, as in ME. The exception: the 10 newest
versions of each memory are always kept, which is what `history` shows today. A
memory that has not changed for a month can still be reverted.

Pruning runs at most once an hour, inside a write. History and revert reach
exactly as far as the log does.

## 4. Writes and identity (P1)

The `memory` tool's `write` gains:

- `name`, an explicit slug;
- `tree`, either absolute (`/global/...`, `/projects/<this project>/...`,
  `/local/...`, which also chooses the scope) or relative to `scope` (today's
  `path`, still accepted);
- `meta`, an object;
- `temporal`, `{start, end?}`;
- `on_conflict`, `error` | `replace` | `ignore`, keyed on `(kind, name)`.

`replace` is content-aware: an identical payload is a no-op and does not bump the
version. ha's duplicate refusal (0.6 similarity, 85 % containment) stays ahead of
`on_conflict` for new entries, and `distinct` still overrides it.

Updates keep `expected_version`, and also accept ME's `version_hash`.

`restore` becomes `revert`, and `restore` stays as an alias:

- it re-applies a snapshot from the event log as a new forward version, with
  cause `revert`;
- reverting a deleted memory recreates it under its old id, continuing its version
  sequence;
- reverting into a `(kind, name)` slot that is now taken is a conflict.

The digest is unchanged in P1. It also leaves out entries whose temporal range
has ended (`temporal_end <= now`), which are history rather than current fact.
Search still finds them.

## 5. Organizing and history (P2)

New `memory` tool actions. Children get only the read-only ones.

| Action | ME equivalent | Notes |
|---|---|---|
| `get` with `path` (`tree/name`) | `get_by_path` | An ambiguous name across kinds needs `kind` |
| `tree` (`tree`, `levels`) | `me_memory_tree` | Nodes with counts, global and local together |
| `count` (`tree` filter, `max_count`) | `me_memory_count` | |
| `move` (`source`, `destination`, `dry_run`) | `me_memory_mv` | Prefix replace. Moving across scope roots moves rows between stores, under one `operation_id` |
| `copy` (`source`, `destination`, `dry_run`) | `me_memory_copy` | Copies get new ids. A clashing name gets a `-2`, `-3` suffix |
| `delete` with `path` | `delete_by_path` | |
| `delete_tree` (`tree`, `dry_run`) | `me_memory_delete_tree` | |
| `history` | `me_memory_history` | Scope by `id`, `path`, `tree`, `operation_id` or `since`; also `until`, `operation`, `order`, `cursor` (keyset on `event_id`), `limit` (max 1000) |

A refinement records the `operation_id` of its write, so `/refine --rollback` and
`history operation_id=…` agree on what one review changed.

## 6. Search (P3)

### 6.1 Lexical

BM25 runs in `memory/search.rs` over the entries a search considers, after the
filters. A global and a conversation's store rank as one corpus, with one set of
statistics. The stores are small, so this beats keeping an FTS5 index in step;
an index can come later if they grow.

Words are ha's own tokens:

- lowercase and NFC;
- CJK runs as bigrams;
- a query without accents reads the entries without theirs, so `may in` finds
  `máy in`; one with accents reads them as written.

A query word matches the words it begins, so "deploy" still finds "deploys" as
the old substring match did.

The fields weigh title 3, content 1, tree and name 1, meta values 0.5. Only
entries holding at least one query word are returned: ME's positive-match
invariant. The query's variants (`queries`) are fused by RRF within the arm.

### 6.2 Semantic

Memory uses the embedding endpoint configured for the global knowledge library
(`/knowledge semantic --global`). Nothing is embedded until the user configures
it.

| Table | Contents |
|---|---|
| `memory_vector` | `memory_id`, `content_version`, `namespace` (endpoint and model), vector |
| `memory_embedding_queue` | One row per `(memory_id, content_version)` |

An entry needs a vector when it has none for its current content version in the
current namespace. The queue table records only failed attempts. Embedding runs
in a background thread after each write through the `memory` tool, and for up
to 20 s before a semantic search.

The write-back is guarded by `content_version`, so a stale vector never lands.
Failures retry three times, and a 429 defers the row without spending an attempt.

Scoring is exact cosine over every vector: ha's stores are small enough that ME's
HNSW recall caveats do not arise. The result is cosine similarity in `[-1, 1]`.
`semantic_threshold` is in `[0, 1]`; a value outside it is refused, not clamped.

### 6.3 Hybrid

Weighted Reciprocal Rank Fusion, as in ME:

```text
score = w_fulltext / (60 + rank_fulltext) + w_semantic / (60 + rank_semantic)
```

| Parameter | Default and limits |
|---|---|
| `weights` | 1 / 1, each in [0, 1] |
| `candidate_limit` per arm | 30, at least `limit`, at most 1000 |
| `limit` | 10, at most 200 |

A missing arm adds 0, and ties break by id.

Query variants (`queries`) are fused inside each arm first, by plain RRF as today.

`harness_types::search::hybrid_fuse` is replaced by this fusion, so knowledge and
history search also reward cross-arm agreement. Today they average normalized
ranks, and a one-arm top hit beats an item that both arms found.

Each score keeps its mode's meaning, as in ME:

| Mode | Score |
|---|---|
| fulltext | BM25 > 0 |
| semantic | cosine |
| hybrid | RRF |
| filter-only | -1 |

### 6.4 Filters

They compose with any mode and with each other, all ANDed. They are evaluated in
Rust over the candidate rows; SQL narrows candidates first with a tree prefix and
the temporal index.

| Filter | Semantics |
|---|---|
| `tree` | **Exact path:** the node and its descendants. **lquery,** when the pattern holds `* ! { } \| @ %`: `*`, `*{n}`, `*{n,m}`, `!label`, `a\|b`, `label@` (case-insensitive), `label%` (word match), `label*` (prefix). **ltxtquery,** when it holds `&`: Boolean `& \| !` over labels, with the same modifiers. Follows PostgreSQL's ltree documentation. |
| `meta` | JSON containment, PostgreSQL `@>` semantics: an object matches a sub-object, an array contains the given elements. |
| `meta_predicate` | A Boolean SQL/JSON path predicate. See below. |
| `temporal` | `before`, `after` (strict, exclude rows without temporal), `contains`, `overlaps {start,end}`, `within {start,end}`. Multiple modes are ANDed. Bounds are `[start, end)`, and a point is `[t, t]`. |
| `grep` | Case-insensitive regex (`regex` crate) on content. Like ME, it must come with a search, `tree`, `meta` or `temporal`; `meta_predicate` alone does not count. |

`meta_predicate` covers PostgreSQL's SQL/JSON path dialect for predicates:

- `$`, `@`, `.key`, `."quoted key"`, `[*]`, `[n]`, `[n to m]`, `last`, `.**`;
- filters `? ( … )`;
- `== != < <= > >=`, `&& || !`, `exists( … )`, `like_regex "…" flag "i"`,
  `starts with`, `is unknown`;
- arithmetic `+ - * / %` and unary minus;
- the methods `.size() .type() .double() .ceiling() .floor() .abs() .keyvalue()`;
- lax mode, as ME runs it, where structural errors are false.

It is a hand-written parser and evaluator in `memory/jsonpath.rs`, tested against
a table of PostgreSQL results.

With no search text, filters browse: newest first, or by `order_by` `asc`/`desc`.
Every response carries `total`, counted with `max_count` semantics up to 10 000.

### 6.5 Presentation

| Parameter | Behaviour |
|---|---|
| `select` | Fields to return: `id`, `name`, `tree`, `kind`, `title`, `meta`, `meta.<key>`, `temporal`, `version`, `version_hash`, `score`, and the content slices `content:N`, `content:M:N`, `content:M:` (characters, with `content_length` added). |
| `format` | `json` (the tool default, compact), `compact`, or `yaml`. |

`format: yaml` comes from a small emitter in `memory/search.rs`, which quotes any
string YAML could read as something else. Reading YAML (packs, import) needs a
parser: a pinned serde YAML crate is chosen when P5 resumes.

`semantic` names what is meant. Without it, `query` is also searched by meaning
whenever embeddings are configured, as knowledge search already does. A
`semantic` given without embeddings is searched by its words, and the result
says why.

## 7. Knowledge and history (P4)

### Knowledge page history

Each library gets `.ha-history.sqlite3`. Its `page_event` table records every
write and delete:

- `event_id`, `at`, `path`;
- `operation`, `operation_id`, `cause`, `actor`;
- the page's `hash`, `title`, `summary`, `sources`, `related` and full content.

The event is written right after the page is committed, under the library's
lock. A history that cannot be written is reported in the diagnostics and does
not undo the write. A page's first recorded change also keeps the text it
replaced (cause `baseline`), so a page written before the history began can
still be reverted to.

The `knowledge` tool gains:

- `history` (a page, a folder or a time feed);
- `revert` (`path`, `version` = an event id).

Reverting a deleted page recreates it, subject to the same backlink rule a write
has. Retention is the same as memory: 30 days, with the 10 newest versions of
each page always kept.

### One search over both

`memory` search accepts `sources: ["memory", "knowledge"]`; the default is memory
only. Knowledge hits come from the knowledge engine (BM25, or hybrid when
configured) and are fused by RRF into one list. Each hit says `source`, plus
`path` and the line range for a page. The CLI does the same with `--knowledge`.

### History tools

- **`history_search`** gains:
  - `since` and `until`: a source id is the journal event's `evt_<UUIDv7>`,
    whose head is its time, so a window of time is a range of ids and needs no
    new column;
  - `kind`, one journal event type or a list of them;
  - `order`, `newest` (the default) or `oldest` first among equal scores.

  A source found by meaning keeps to the same window and kinds.
- **`history_read`** returns `previous_source_id` and `next_source_id` within the
  same session, by `sequence`, so a reader can walk the thread without searching
  again.

## 8. CLI, formats and packs (P5)

### `ha memory`

`ha memory <action>`, mirroring `me memory`:

- `create`, `get`, `search`, `update`, `delete`, `deltree`;
- `history`, `revert`, `edit`;
- `count`, `tree`, `copy`, `move`;
- `import`, `export`.

It also has `ha memory pack validate|install|list`.

| Option | Meaning |
|---|---|
| `--cwd DIR` | The project, as in `ha knowledge` |
| `--global` | Global entries only |
| `--conversation ID` | A conversation's local store |
| `--json`, `--yaml` | Output format |
| `--select` | Fields to show |

Every write the CLI makes is recorded with actor `cli`.

`edit` opens `$VISUAL` / `$EDITOR` (Notepad on Windows when neither is set) on a
Markdown file: YAML front matter for tree, name, meta and temporal, then the
content. Saving writes the file back with `expected_version`.

### Formats

The same as ME's `formats.md`:

- a JSON array or a single object;
- NDJSON (`.ndjson`, `.jsonl`; import only);
- a YAML array or a single object;
- Markdown, one memory per file, with YAML front matter.

Format comes from the extension or is sniffed. `temporal` accepts all three input
shapes. A record without a tree goes to `/global/imported`.

Import uses `on_conflict: ignore`, so a re-import is a no-op. Export takes the
§6.4 filters and writes to a file or a directory (Markdown, one file per memory),
up to 200 records per call, or more with `--all` on the CLI. `--legacy` writes a
0.8-compatible `harness_state.json`.

### Packs

The same format as ME:

- a YAML array of records, with the comment header `# Pack:`, `# Version:`,
  `# Description:`, `# ID prefix:`;
- deterministic UUIDv7 ids;
- `meta.pack {name, version}`;
- a tree under `/global/pack/<name>/...`.

`install` deletes the records of an older version of the pack and then inserts the
new ones. `validate` runs offline.

## 9. Importers (P6)

| Command | Writes | Shape |
|---|---|---|
| `ha memory import docs [dir]` | Knowledge pages in the project library, under `docs/<relative path>`, `origin: import:docs`, `sources: [repo path]` | ME's options: `--git-aware`, `--include`, `--exclude`, `--temporal-key`, `--prune` (deletes only pages this importer wrote), `--dry-run`. Front matter is document data. Over-size pages are truncated and flagged. |
| `ha memory import git [repo]` | `record` rows at `/projects/<key>/git_history/<sha>` | Subject, body and up to 50 changed files with counts; temporal = commit date; `$prev` along first parents; ME's metadata keys; incremental from the newest imported commit; `--branch`, `--since`, `--until`, `--max-count`, `--full`, `--no-merges`, `--no-file-list`. |
| `ha memory import claude\|codex\|opencode` | `record` rows at `/projects/<key>/agent_sessions/<session>/msg_<id>` | ME's flags (`--project`, `--since`, `--until`, `--full-transcript`, `--include-temp-cwd`, `--include-trivial`, `--include-sidechains`, `--dry-run`); `$prev` and `$thread`; ME's `source_*` metadata; `on_conflict: replace` stamped with `importer_version`. |

`record` is a new kind. It is searchable and filterable, and it is **never in the
digest**: thousands of commits must not crowd out the user's preferences.

Docs go into knowledge rather than into memory, because a document is what a
knowledge page already is. The shared search of §7 reaches them either way.

## 10. Integration (P7)

- **`ha mcp serve memory`.** A stdio MCP server (rmcp, already a dependency with
  `server`) that exposes ME's tool set as `ha_memory_*`:
  - `context`, `create`, `update`, `delete`, `delete_by_path`, `delete_tree`;
  - `search`, `get`, `get_by_path`, `count`, `tree`, `copy`, `mv`;
  - `import`, `export`, `history`, `revert`;
  - knowledge search.

  It works on the project of its working directory. `--read-only` exposes only the
  read tools.
- **`ha integrate install|uninstall claude|codex|opencode`.**
  - Registers that MCP server with the harness.
  - With `--capture`, also adds the harness's stop/session-end hook that runs the
    matching `ha memory import <harness>` for that one session. The hook runs
    asynchronously and always exits with success.
  - ha records every file it writes in `<data>/integrations.json`.
  - Uninstall removes only recorded files that have not been changed since.
  - Codex hooks still need the user's approval in Codex.
- **`ha memory serve`.** The knowledge console, extended with memory: tree
  browsing, search with filters, an entry's history with revert, and
  previous/next along `$prev`.
- **`ha doctor`.** Gains a memory section: the stores and their schema versions,
  entry and event counts, legacy files still waiting, embedding queue status, and
  installed integrations.

## 11. Compatibility rules

- The `memory` tool, the kernel's `rlm.harness`, `/refine`, `/learn`, Deep Dream
  and the digest behave as they do today unless this document says otherwise.
  Their unit tests keep passing through the document view.
- Children stay read-only. Every new action is classified as read or write.
- The bug report includes a JSON export of both stores instead of the raw files,
  with event counts.
- Nothing ever leaves the machine. Embeddings call only the endpoint the user
  configured.

## 12. Phases

Each phase passes the gate (`fmt`, `clippy`, `test`) and the live suites it
touches before the next one starts (see `AGENTS.md`). A new live suite,
`memory-store.sh`, collects the parity checks. Each check must fail on 0.8.0 and
pass on the new build, and that control run is reported. The version and release
stay with the maintainer.

| Phase | Delivers | Live suites |
|---|---|---|
| **P1 Core** | §3, §4: SQLite stores, document view, migration, event log with actor/cause/operation, retention, `name`/`tree`/`meta`/`temporal`/`on_conflict`, revert on the log, digest temporal rule | memory, kernel, knowledge, compaction, session-continuity, children, memory-store |
| **P2 Organize** | §5: get/delete by path, tree, count, move, copy, delete_tree, history feed | memory, children, memory-store |
| **P3 Search** | §6: FTS5 BM25, memory vectors and queue, weighted RRF (also for knowledge and history), every filter, JSONPath, select/format, YAML crate | memory, knowledge, memory-store |
| **P4 Knowledge and history** | §7: page history and revert, unified search, history filters and thread links | knowledge, session-continuity, memory-store |
| **P5 CLI** | §8: `ha memory`, four formats, `--legacy`, packs | memory-store |
| **P6 Importers** | §9: docs, git, Claude, Codex and OpenCode sessions, `record` kind | memory-store |
| **P7 Integration** | §10: MCP server, harness integrations and capture, web console, doctor | memory-store, plus a run with a real MCP client |

Before P1 the live tests read and seed `harness_state.json` directly
(`kernel.sh`, `knowledge.sh`, `memory.sh`, `session-continuity-evidence.py`).
P1 moves their seeding to the legacy-import rule of §3.4, and their reads to
`ha memory export --json` (a minimal export lands in P1 for this) or to read-only
`sqlite3` queries.
