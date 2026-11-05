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

0.8.2 closes what a second review against ME found missing in P1 to P4 (§13):
a name unique per tree and kept through updates, patch updates, the immutable
`memory_id` as an address, paged history with every field, English stemming,
lquery's `%` as PostgreSQL has it, PostgreSQL 17's JSONPath methods, filters
on knowledge pages, old page versions read whole, and `status`.

## 1. Goal and decisions

memory-engine (ME) is a team memory server: Postgres with `ltree`, `pgvector` and
BM25. Agents reach it over MCP. ha keeps memory in `harness_state.json` files,
keeps knowledge as Markdown pages and keeps a history index in SQLite. The goal is
that every ME capability a single user can use also exists in ha, running locally.

The maintainer decided:

| Topic | Decision |
|---|---|
| Sharing | **Out of scope for now.** ha stays local and single-user: no spaces, principals, groups, grants, API keys, login, invites or remote backend. To be revisited later. |
| Memory storage | **One SQLite store per scope** with ME's record model: tree, name, meta, temporal, version and an event log. It replaces `harness_state.json`. |
| Meaning | **No embeddings (0.8.8).** Search and the digest rank by words, then by how recently and how much an entry was used (§14). Nothing waits on a network call, and nothing leaves the machine. |
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

Status before this work: ✅ present, 🟡 partial, ❌ missing. P1 to P7 are the
phases in §12. Since 0.8.2 every row up to P4 is present; §13 lists what the
review after 0.8.1 found and what 0.8.2 changed.

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
| Semantic search on memory with a background embedding queue | ❌ | P3 · removed in 0.8.8 (§14) |
| Weighted RRF hybrid; `weights`, `candidateLimit`, `semanticThreshold` per call | 🟡 | P3 · only `candidate_limit` since 0.8.8 |
| Filters `tree`, `meta`, `metaPredicate`, `temporal`, `grep`; filter-only browse with `order_by` | ✅ (since 0.8.1) | P3 |
| `select` projection and `format` yaml/json/compact | 🟡 | P3 |
| Knowledge page history, revert and undelete | ❌ | P4 |
| One search over memory and knowledge | ❌ | P4 |
| History filters (`since`, `until`, `kind`); thread links on `history_read` | ✅ (since 0.8.1; `until` 0.8.2) | P4 |
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
| global | `<data>/harness/` | `/global/...`; a project-qualified entry goes under `/projects/<project_key>/...`, the key of the project's repository ([project identity](PROJECT_IDENTITY.en.md)) |
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
  key             TEXT NOT NULL,         -- ha's entry id (slug), unique per kind
  name            TEXT,                  -- ME's name, unique in its tree (schema 3)
  tree            TEXT NOT NULL,         -- '/global/user/preferences'
  title           TEXT NOT NULL,
  content         TEXT NOT NULL,
  meta            TEXT NOT NULL DEFAULT '{}',   -- JSON object (the entry's `metadata`)
  temporal_start  TEXT,                  -- RFC 3339 UTC, NULL = no temporal
  temporal_end    TEXT,                  -- exclusive; = start for a point
  doc             TEXT NOT NULL,         -- the whole entry as ha writes it (lossless)
  version         INTEGER NOT NULL,
  version_hash    TEXT NOT NULL,         -- 32 hex of SHA-256 over the canonical payload
  content_version INTEGER NOT NULL,      -- grows when the title or content does
  created_at      TEXT NOT NULL,
  updated_at      TEXT NOT NULL,
  UNIQUE (kind, key)
);
CREATE INDEX memory_tree ON memory(tree);
CREATE INDEX memory_path ON memory(kind, tree, name);
CREATE INDEX memory_temporal ON memory(temporal_start, temporal_end);

CREATE TABLE memory_event (
  event_id     TEXT PRIMARY KEY,   -- UUIDv7: a chronological total order
  at           TEXT NOT NULL,
  memory_id    TEXT NOT NULL,      -- outlives the memory
  kind TEXT NOT NULL, key TEXT NOT NULL, name TEXT, tree TEXT NOT NULL,
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
-- P3 (schema 2): memory_vector, memory_embedding_queue (dropped by schema 4)
-- 0.8.2 (schema 3): the V1 `name` column renamed `key`, and `name` added (§13)
-- 0.8.8 (schema 4): memory_usage added, the two embedding tables dropped (§14)
CREATE TABLE memory_usage (
  memory_id     TEXT PRIMARY KEY,   -- removed with its memory
  reads         INTEGER NOT NULL DEFAULT 0,  last_read_at  TEXT,
  hits          INTEGER NOT NULL DEFAULT 0,  last_hit_at   TEXT,
  shown         INTEGER NOT NULL DEFAULT 0,  last_shown_at TEXT
);
```

How ha's entries map onto ME's record:

- **key.** ha's entry id, the document's key, unique per kind within a store: the
  name when it is free, else the name with a `-2`, `-3` suffix. It follows a
  new name while the two are the same, keeping the old key as an alias.
- **name.** ME's: unique within its tree, as ME has it since 0.8.2, so the same
  name can sit in two trees. It is the one a write gave (ME's regex), else the
  title's slug. A name made from the title follows a new title; a name given, or
  one apart from the title, stays. An entry written before 0.8.2 is named by its
  key. `<tree>/<name>` addresses it.
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

A read opens a store the same way (`Store::existing`, behind `load_document`):
a scope with neither a store nor a legacy file is left as it is, but a store
is migrated to this schema, and a legacy file alone gets a store with the
file imported. Such a read writes. A read that must change nothing - the bug
report's snapshot, `purge`'s dry run, the store meta `ha doctor` reads - opens
the database read-only instead.

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

- `name`, an explicit slug, unique in its tree;
- `tree`, either absolute (`/global/...`, `/projects/<this project>/...`,
  `/local/...`, which also chooses the scope) or relative to `scope` (today's
  `path`, still accepted);
- `meta`, an object;
- `temporal`, `{start, end?}`;
- `on_conflict`, `error` | `replace` | `ignore`, keyed on `(kind, tree, name)`.

`replace` is content-aware: an identical payload is a no-op and does not bump the
version. Otherwise it takes the entry as the write says it, `meta` and
`temporal` included, as ME does. ha's duplicate refusal (0.6 similarity, 85 %
containment) stays ahead of `on_conflict` for new entries, and `distinct` still
overrides it.

Updates keep `expected_version`, and also accept ME's `version_hash`. An update
is a patch (0.8.2): it changes what it gives - title, content, name, tree or
path, meta, temporal, pinned - and keeps the rest, so `content` may be left out.
An update that changes nothing keeps its version. An absolute `tree` in another
scope moves the entry to that scope's store under its immutable id. A write's
result carries the entry's `name`, `tree`, `version_hash` and `memory_id`.

An entry is addressed by its key, by `<tree>/<name>`, or by its `memory_id`, the
UUID the store gave it, which never changes; list and search show it.

`restore` becomes `revert`, and `restore` stays as an alias:

- it re-applies a snapshot from the event log as a new forward version, with
  cause `revert`;
- reverting a deleted memory recreates it under its old id, continuing its version
  sequence;
- reverting into a key, or a `(tree, name)`, that another entry now holds is a
  conflict.

The digest is unchanged in P1. It also leaves out entries whose temporal range
has ended (`temporal_end <= now`), which are history rather than current fact.
Search still finds them. Since 0.9.0 an entry a correction or the expiry pass
closed is left out of a search too, unless it names a time or `include_ended`
(§16).

## 5. Organizing and history (P2)

New `memory` tool actions. Children get only the read-only ones.

| Action | ME equivalent | Notes |
|---|---|---|
| `get` with `path` (`tree/name`) | `get_by_path` | An ambiguous name across kinds needs `kind` |
| `tree` (`tree`, `levels`) | `me_memory_tree` | Nodes with counts, global and local together |
| `count` (`tree` filter, `max_count`) | `me_memory_count` | A tree, an lquery or an ltxtquery, as search takes it |
| `move` (`source`, `destination`, `dry_run`) | `me_memory_mv` | Prefix replace. Moving across scope roots moves rows between stores, under one `operation_id` |
| `copy` (`source`, `destination`, `dry_run`) | `me_memory_copy` | Copies get new ids and keep their names; a name the destination tree holds is a conflict, and nothing is copied |
| `delete` with `path` | `delete_by_path` | |
| `delete_tree` (`tree`, `dry_run`) | `me_memory_delete_tree` | |
| `history` | `me_memory_history` | With `id` (a key, a path or a `memory_id`): the entry's versions, each with its tree, name, meta, temporal, cause and `operation_id`, a page of `limit` (10, at most 100) from `offset`. Without: the feed, scoped by `tree`, `operation_id`, `memory_id` or `since`; also `until`, `operation`, `order`, `cursor` (keyset on `event_id`), `limit` (max 1000) |
| `status` | `memory.embeddingStatus` | Entries per scope, and `search: words`; a note when an old `.ha-embedding.json` is left (0.8.8) |

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
the old substring match did, and since 0.8.2 the words of its English stem
(Snowball, as PostgreSQL's `english` configuration that ME uses), so
"deploying" finds "deployed". There is no stopword list: ha judges words by
their statistics, not by lists.

The fields weigh title 3, content 1, tree and name 1, meta values 0.5. Only
entries holding at least one query word are returned: ME's positive-match
invariant. The query's variants (`queries`) are fused by RRF within the arm.

Search matches words, not meaning. An empty result says which queries were
searched and to search again with other words - a synonym, the other language
the project uses, a shorter or broader term, a name or path from the code -
before saying it is not remembered. It also lists, as `branches`, the trees the
searched entries sit in (at most 12, the fullest first): their names are the
store's own words, so the model sees which language the store is written in
and can browse a branch with `tree` and no query. A question asked in English
of a store kept in Vietnamese was retried with English synonyms only until the
branches were shown. The model picks the words; ha keeps no synonym table.

### 6.2 Semantic (removed in 0.8.8)

What follows is how 0.8.1 to 0.8.7 searched by meaning. 0.8.8 removed it: the
tables are dropped (§14), and `semantic`, `semantic_threshold`, `weights` and
`has_embedding` are gone from the tool.

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

### 6.3 Hybrid (removed in 0.8.8)

Until 0.8.7, weighted Reciprocal Rank Fusion, as in ME:

```text
score = w_fulltext / (60 + rank_fulltext) + w_semantic / (60 + rank_semantic)
```

| Parameter | Default and limits |
|---|---|
| `weights` | 1 / 1, each in [0, 1] |
| `candidate_limit` per arm | 30, at least `limit`, at most 1000 |
| `limit` | 10, at most 200 |

A missing arm adds 0, and ties break by id. A weight of 0 turns its arm off:
`weights {semantic: 0}` ranks by words alone, with BM25 scores, and says when
no word matched.

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

Since 0.8.8 a search's modes are `fulltext` (BM25, its order also weighing
recency and use, §14) and `filter`.

### 6.4 Filters

They compose with any mode and with each other, all ANDed. They are evaluated in
Rust over the candidate rows; SQL narrows candidates first with a tree prefix and
the temporal index.

| Filter | Semantics |
|---|---|
| `tree` | **Exact path:** the node and its descendants. **lquery,** when the pattern holds `* ! { } \| @ %`: `*`, `*{n}`, `*{n,m}`, `!label`, `a\|b`, `label@` (case-insensitive), `label%` (each word of the pattern matches some underscore-separated word of the label, in any order, as PostgreSQL's `compare_subnode`), `label*` (prefix). **ltxtquery,** when it holds `&`: Boolean `& \| !` over labels, with the same modifiers. Labels are joined by `/`, or by `.` as ME's ltree joins them; a pattern without a leading `/` also matches a tree read below its scope's root. |
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
- `.**`, and `.**{n}`, `.**{n to m}` with `last`;
- string escapes, `\uXXXX` included;
- the methods `.size() .type() .double() .ceiling() .floor() .abs() .keyvalue()`,
  and PostgreSQL 17's `.datetime() .date() .timestamp() .timestamp_tz()
  .string() .number() .decimal() .integer() .bigint() .boolean()` without
  arguments. A date or time becomes UTC RFC 3339, so two compare in time order.
  Any other method is refused when the predicate is parsed: one that passed and
  matched nothing would read as no entry having the facet;
- lax mode, as ME runs it, where structural errors are false.

It is a hand-written parser and evaluator in `memory/filters.rs`.

With no search text, filters browse: newest first, or by `order_by` `asc`/`desc`.
Every response carries `total`, counted with `max_count` semantics up to 10 000.

### 6.5 Presentation

| Parameter | Behaviour |
|---|---|
| `select` | Fields to return: `id`, `memory_id`, `name`, `tree`, `kind`, `title`, `meta`, `meta.<key>` (under `meta`, left out when missing, as ME projects it), `temporal`, `version`, `version_hash`, `score`, and the content slices `content:N`, `content:M:N`, `content:M:` (characters, with `content_length` added). On `search`, `read` and `history`. |
| `format` | `json` (the tool default, compact), `compact`, or `yaml`; on `search`, `read` and `history`. |

`format: yaml` comes from a small emitter in `memory/search.rs`, which quotes any
string YAML could read as something else. Reading YAML (packs, import) needs a
parser: a pinned serde YAML crate is chosen when P5 resumes.

Until 0.8.7 `semantic` named what was meant, searched by meaning; 0.8.8
removed it with the embeddings.

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

- `history` (a page, a folder or a time feed, with `since`, `until`,
  `operation` and a per-scope `cursor`); with `version`, that version whole, to
  read before reverting;
- `revert` (`path`, `version` = an event id).

Reverting a deleted page recreates it, subject to the same backlink rule a write
has. Retention is the same as memory: 30 days, with the 10 newest versions of
each page always kept.

### One search over both

`memory` search accepts `sources: ["memory", "knowledge"]`; the default is memory
only. Knowledge hits come from the knowledge engine (BM25) and are fused by
RRF into one list. Each hit says `source`, plus
`path` and the line range for a page. The CLI does the same with `--knowledge`.

The filters reach the pages as ME's would read a record with no meta and no
temporal value: a `meta`, `meta_predicate` or `temporal` filter leaves every
page out; a `tree` filter reads a page at `/knowledge/<scope>/<its folders>`;
`grep` reads its whole text; `select` picks its fields. The result says so.

### History tools

- **`history_search`** gains:
  - `since` and `until`: a source id is the journal event's `event_<UUIDv7>`,
    whose head is its time, so a window of time is a range of ids and needs no
    new column;
  - `kind`, one journal event type or a list of them;
  - `order`, `newest` (the default) or `oldest` first among equal scores.

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
  entry and event counts, legacy files still waiting, and installed
  integrations.

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
| **P3 Search** | §6: FTS5 BM25, memory vectors and queue (removed in 0.8.8), weighted RRF (also for knowledge and history), every filter, JSONPath, select/format, YAML crate | memory, knowledge, memory-store |
| **P4 Knowledge and history** | §7: page history and revert, unified search, history filters and thread links | knowledge, session-continuity, memory-store |
| **P5 CLI** | §8: `ha memory`, four formats, `--legacy`, packs | memory-store |
| **P6 Importers** | §9: docs, git, Claude, Codex and OpenCode sessions, `record` kind | memory-store |
| **P7 Integration** | §10: MCP server, harness integrations and capture, web console, doctor | memory-store, plus a run with a real MCP client |

Before P1 the live tests read and seed `harness_state.json` directly
(`kernel.sh`, `knowledge.sh`, `memory.sh`, `session-continuity-evidence.py`).
P1 moves their seeding to the legacy-import rule of §3.4, and their reads to
`ha memory export --json` (a minimal export lands in P1 for this) or to read-only
`sqlite3` queries.

## 13. The review after 0.8.1 (0.8.2)

After 0.8.1 the maintainer asked for a second, independent review of P1 to P4
against ME's source, with P5 to P7 left out. What it found, and what 0.8.2 does:

| Found | 0.8.2 |
|---|---|
| An update renamed an entry that had a name of its own: the name followed the title on every update, so `<tree>/<name>` broke | A name follows the title only while it says the title; a given name stays (§3.2) |
| Names were unique per store, so two trees could not share one, and a copy renamed its entries | Names are unique per tree (schema 3: `key` and `name`); a copy keeps names, a clash is a conflict (§5) |
| `content` was required on every update | An update is a patch; one that changes nothing keeps its version (§4) |
| The `memory_id` a feed showed could not address anything | `memory_id` reads, writes, pages history, reverts and deletes; list and search show it (§4) |
| History of an entry showed ten versions, title and content only | Every field of each version, a page at a time (§5) |
| An entry could not move to another scope alone | An absolute tree in another scope moves it, under its id (§4) |
| `replace` kept the old meta and period when the write left them out | `replace` takes the entry whole, as ME (§4) |
| A new title kept the old vector | The title is embedded with the content, and bumps `content_version` |
| No English stemming | Snowball English, as PostgreSQL's `english` configuration (§6.1) |
| Pure keyword search was not possible once embeddings were on | A weight of 0 turns an arm off (§6.3) |
| `%` matched words by position; trees took only `/` | `%` as PostgreSQL's `compare_subnode`; `.` joins labels; relative trees (§6.4) |
| An unknown JSONPath method matched nothing, silently; no `.datetime()`, `.**{n}`, `\u` | Unknown methods are refused; PostgreSQL 17's conversion methods, levels and escapes (§6.4) |
| Combined search ignored the filters on knowledge pages | The filters reach the pages (§7) |
| No way to read an old page version, no `until` or cursor | `history` with `version`, `until`, `operation`, `cursor` (§7) |
| No embedding status, no `has_embedding` | `status`, and `has_embedding` in `select` (§5, §6.5) |
| `select` and `format` only on search; `meta.<key>` flat | Also on read and history; `meta.<key>` under `meta` (§6.5) |

Not taken: ME's batch create (only its importers use it; P5), ME's stopword
list (ha does not judge text by word lists), and `strict` JSONPath mode (ME runs
lax).

The rows about vectors and `has_embedding` describe 0.8.2; 0.8.8 removed both
(§14).

## 14. Words, recency and use (0.8.8)

0.8.8 removes semantic search everywhere: memory search, the digest's
by-meaning arm, the readout's meaning recall, knowledge search, history search,
the embedding cache and `/knowledge semantic`. It was off unless the user
configured an endpoint, and when on it could stall a turn on the provider and
retire entries for good during an outage. What ranks instead:

**Usage.** Schema 4 adds `memory_usage`: per memory id, `reads` and
`last_read_at` (the memory tool's `read`), `hits` and `last_hit_at` (the
results a memory `search` returns, in either mode), and `shown` and
`last_shown_at` (the entries a turn's digest showed, pinned ones included). Each
batch is one upsert in its own transaction, only for ids the store holds; the
row goes with its entry. A use never touches the entry's document or the event
log, so it is never a version and never pushes a revertable one out. The digest
records what it showed off the turn's path, in a background thread.

**Ranking.** The digest and memory search rank the same way:

1. Constraints first (the digest), pinned entries outside the ranking.
2. Entries the words match (BM25 above zero), best first. The best of them -
   twice what is shown: 12 in the digest, twice the `limit` in search - are
   fused by weighted RRF with two lighter arms (weight 0.3 each, the words 1):
   - **recency**: the later of the entry's `updated_at` and `last_read_at`;
   - **use**: `reads`, halving in weight every 30 days after the last read.

   An entry with no time or no reads is left out of that arm. The rest of the
   matched entries follow in their words' order, so an entry the words barely
   reached never passes the best of them.
3. Entries nothing matched come after every matched one, the most recently
   written or read first; with no words at all, every entry is ordered so. They
   used to be alphabetical.

`hits` and `shown` are counted but never ranked on: they record what was put in
front of the model, not what it used, and ranking by them would feed itself.
Ties go to the earlier place (path, title and key in the digest), so the same
state always ranks the same. A search hit's `score` stays its BM25 score.

**Upgrade.** A store opens at schema 4: `memory_vector` and
`memory_embedding_queue` are dropped, and nothing else changes. A knowledge
library's `.ha-embedding.json` and `.ha-embedding.sqlite`, and
`<data>/history-embedding/`, are left on disk and never read. `memory status`
and `/knowledge status` say when a `.ha-embedding.json` is still there, and the
diagnostics record `knowledge.embedding_config_ignored` once.

## 15. An index for ranking, and forgetting (0.8.9)

**The words are indexed once.** Until 0.8.8 the digest and memory search
tokenized, stemmed and folded every entry on every call, then sorted every
match: about 0.4 s a turn on 10,000 entries and 2 s on 50,000. Now:

- Each store's entries are indexed once per process (`memory/search.rs`,
  `Corpus`): every word of the four fields, as written and folded, points at
  the entries that hold it (an inverted index), with each word's English stem.
- An entry is kept in its store's index under its key and its `version_hash`,
  which changes whenever its tree, name, title, content, meta or temporal value
  does, so any write, revert, move, import or purge is seen on the next read;
  only the entries that changed are indexed again. Entries not read from a
  store (the session logs `rank_documents` ranks) are indexed for the call.
- A term's entries are the postings of the words it begins and of its stem's
  words, so only the entries that hold a query word are scored. The
  statistics are still those of exactly the entries a call considers, so the
  scores are the same as before, to the bit; a test checks the index against
  the old pass over random stores.
- The digest takes the few it shows, and a search with one query its
  candidates, without sorting the rest.
- A store's index whose slots pass twice the most entries one call read from
  it (plus 1,024) is dropped and built again; at most 32 stores are kept.

Measured with `ranking_cost_over_large_stores` (`cargo test --release -p
harness-cli --bin ha -- --ignored ranking_cost --nocapture`) on one machine;
"now" is the second and third of three calls on one loaded state:

| Entries | Digest before | Digest now | Search before | Search now |
|---|---|---|---|---|
| 10,000 | 380 ms | 20 ms (225 ms on the first call) | 330 ms | 6 ms |
| 50,000 | 2.1 s | 125 ms (1.1 s on the first call) | 2.0 s | 45 ms |

**Purge.** Nothing could be forgotten: a deleted entry's versions stayed in
the event log for 30 days and its ten newest for good, and a secret that
slipped past the write filter stayed with them. `ha memory purge` removes an
entry and every trace of its content:

- its row, every event of it (all versions), its `memory_usage` row;
- in `refinements.jsonl`, each refinement's edit of it is cut to its action,
  kind and outcome, with no title, content, `before` or `after`;
- each refinement that changed it - its summary in the store (`refinement`),
  its record in `refinements.jsonl`, its copy in the imported legacy state -
  loses its words (trigger, evidence, outcome; summary, rationale, expected
  outcome) for a note that it changed a purged memory, and the key its changes
  name becomes `[purged]`: a review's summary says what it recorded;
- its record in `harness_state.json.imported` and its lines in
  `memory_history.jsonl.imported`;
- one event remains: operation `purge`, cause `purge`, actor `cli`, with the
  memory's kind and id, the store's root as its tree, no name, the memory id in
  place of the key (a key may be made from a title that held the text), and an
  empty document. History and revert never offer it as a version.

The deleted rows are overwritten (`secure_delete`), and the database is
rebuilt (`VACUUM`) and its log emptied (`wal_checkpoint(TRUNCATE)`), so the
text is in none of the store's files. While another process reads the store,
the rebuild or the checkpoint may not finish: the purge stands, and the
command says the log keeps the text until that process closes the store. The
conversation's own journal, which recorded the turn that wrote the entry, is
not memory and is not changed.

`--secrets` finds what holds a secret value, by the shapes the write path
refuses (`harness_tools::secret_values`), in the global store and every
conversation's: a live entry that holds one is purged whole; a live entry that
holds none now loses only the versions that held one, and keeps its current
state; a deleted entry whose log holds one is purged whole. It also redacts the
secret values left in the refinement table, `refinements.jsonl` and the
imported legacy files. The listing names each secret's kind, never its value.

A scope bounds what a purge touches: with `--scope`, `--secrets` purges the
scope's entries and their own copies, and leaves the secret values of other
entries' reviews and of the imported files alone; only `--secrets` with no
scope redacts every one. `--conversation` names a conversation's store, so it
goes with `--scope local` or no scope.

The dry run reads the store read-only: no migration, no import, no file
changed. It lists, beside each entry, how many refinement summaries,
`refinements.jsonl` records and imported legacy lines or entries `--yes`
rewrites, and names a legacy file still waiting to be imported, which it does
not read.

| Command | Does |
|---|---|
| `ha memory purge <id or key>` | Lists the entries that memory id or key names, live or deleted, and changes nothing |
| `... --yes` | Purges them |
| `--scope global`, `project`, `local` | Only the global entries, this project's (`--cwd`), or a conversation's (`--conversation`) |
| `ha memory purge --secrets [--yes]` | The same for everything that holds a secret value |

## 16. When an entry holds, and where its words came from (0.9.0)

Memory does not help by itself: most memory setups measured in 2026 did no
better than memory off. What the current systems have and ha lacked: a
contradiction resolved by closing the old fact's validity rather than
deleting it (Mem0's ADD/UPDATE/DELETE/NOOP, Zep/Graphiti's `invalid_at`),
questions about time, trust by source against memory poisoning (MINJA), and
expiry of memory nobody uses (Copilot drops memory unused for 28 days).

### 16.1 The fields

All in `memory/validity.rs`, the one place a later field joins them:

| Field | On | Says |
|---|---|---|
| `origin.kind` | every entry the memory tool or a review writes | where its words came from: `user`, `tool`, `web`, `file`, `agent`, `child`, `review`. The host decides it (§16.4). An entry without one reads as `agent`. |
| `supersedes` | an entry that corrected others | the memory ids of the entries it closed |
| `superseded_by` | an entry a later one replaced | that one's memory id |
| `expired` | an entry the expiry pass closed | why: `unused` |

An entry's **validity** is its `temporal` value: a period `[start, end)`, or a
point it holds from on. Without one it holds from its creation on. Closing an
entry gives its period an end, now, from the start it had (its creation when
it had none). The digest leaves out a closed entry, as it does an ended
period, even while its period has not ended: a future-dated entry superseded
today ends a millisecond after its start.

### 16.2 Contradictions are resolved at write, by closing validity

The `memory` tool's `write` takes `supersedes: [id, ...]` (keys, `<tree>/<name>`
addresses or memory ids):

- On a create, the new entry records `supersedes` (their memory ids) and holds
  from now unless its `temporal` says from when. Each named entry is closed:
  `temporal.end` = now, `superseded_by` = the new entry's memory id, as its next
  version. Nothing is deleted. The new entry's memory id is given before the
  save (`Writer::reviving`), so the closed entries can name it; the closing is
  one more save in the same operation, with cause `supersede`.
- On an update, the entry written supersedes them the same way, and its
  `supersedes` grows. An update may give `supersedes` alone.
- A named entry must be live, of the same kind, not the entry written, and not
  superseded already (that one is named, to supersede the one that replaced
  it). The duplicate refusal and the cross-language repeat refusal do not count
  the entries a write supersedes: it corrects them, it does not repeat them.
  Nor do they count a closed entry, or one whose period ended: they compare
  with what holds now, and a fact that holds again is written anew.
- The `related` note on a create, and both duplicate refusals, give the model
  its choices: supersede the old entry, update the existing one, or write
  nothing (delete the new entry).

A review's edits get the same: an edit `{"action": "supersede", "kind",
"id": <old>, "title", "content", "path"?, "metadata"?, "evidence"}` writes a
new entry keyed by its title, next to the old one's path and project, and
closes the old one (`validity::supersede_edit`). The refinement records it as
two edits: the old entry's (its `before`, and its closed `after`) and the new
entry's `create`, so a rollback deletes the new entry and puts the old one back.
It undoes both or neither: when one of them changed since, both are kept, so
the old fact and its correction are never live at once, and the result names
the one that changed.

`restore` (or `revert`) with an id alone, on an entry a correction or the
expiry pass closed, opens it again: `superseded_by` and `expired` go, and its
period is the one it had before it was closed.

`history` shows the chain: each version carries its `supersedes`,
`superseded_by`, `expired` and `from`.

### 16.3 Questions about time

Memory search takes:

| Argument | Reads |
|---|---|
| `as_of` | the entries valid at that time: their validity covers it, and they had been written by then. A corrected fact's old entry, asked about a time before the correction. |
| `since`, `until` | the entries valid at some time between |
| `include_ended` | the closed entries too |

A search that names a time (`as_of`, `since`/`until`, or the temporal filter
of §6.4) reads closed entries, and ranks the results by how well their
validity matches the time before the words: for a point, whether the entry
held then; for a window, the share of it the entry held (`validity::fit`).
The temporal filter's modes make the window: `contains` a point, `after` and
`before` its bounds, `overlaps` and `within` their periods.

A search that names no time leaves out the closed entries (superseded or
expired). An entry whose own period ended is read as before. Among entries
the words matched alike, one that holds now ranks above one that ended, and
one the user or the agent said above a page's or a child's
(`search::settled`); a better match keeps its place. When the words also match
closed entries, the result says how many (`closed.left_out`) and that `as_of`
or `include_ended` reads them.

No reranker by the model: a model call per search costs a request's time and
tokens, and the validity and the words already order what a question about
time needs.

### 16.4 Trust by source

`origin.kind` is the host's, never the model's. The memory tool reads the
conversation as the project store's journal holds it (`memory/provenance.rs`;
the newest 4,000 events): the words the user typed in each turn (a turn the
host started, such as a child's report, is a child's), and each tool call's
result, by its tool - a web tool's (`web_search`, `web_fetch`) is `web`, a
file read's (`read_file`, `search_text`, `list_files`, `glob`, `find_files`)
`file`, a delegated child's `child`, the memory's own tools none, any other
tool's `tool`. Each of the entry's words, folded and stemmed, weighs by its
IDF over those parts (a word in no part weighs as one in a single part). The
entry is of the kind whose parts hold most of the weight, when they hold at
least half of it; else it is `agent`, the agent's own words. A tie goes to
the more trusted kind, in the order user, file, tool, child, web. No word list
judges the text.

An update takes the kind its words have now; when this conversation shows
none, the entry keeps its kind, so a page's words stay a page's whoever
rewrites them later. A move keeps it. A review's edit takes the kind of the
turn its evidence quotes word for word (`provenance::quoted_kind`), else the
kind the quote's words point to, else `review`: the review inferred it.
The review's transcript is split into turns at blank lines, and keeps a blank
line inside a turn or a tool result as a line of one space
(`harness_runtime::transcript_body`): a fetched page that carries `[User]: ...`
after a blank line stays the page's. A Deep Dream edit takes the kind of the
entries it rewrites: an update the least trusted of its own entry's and of
the entries merged into it, so a page's words merged into a constraint are
refused. Any `origin_kind` a model writes into an edit is dropped.

What follows from it:

- The digest frames an entry from a web page (`, from a web page:
  unverified`) or a delegated child (`, from a delegated agent: unverified`).
  Search, read and history show `from` and `unverified`.
- Such an entry ranks below an otherwise equal one (§16.3).
- An entry whose kind is `web` - now, or when it was kept - cannot be written
  into `constraints/*` or `user/preferences` (and below): a create, an update,
  a move or a review's edit is refused with the reason, before the call runs.
  The model is told to keep it as a fact elsewhere, or to ask the user to
  state the rule.

### 16.5 Expiry by use

A pass closes the entries nobody used: no read and no search hit for
`HA_MEMORY_EXPIRE_DAYS` (default 28; 0 turns it off; at most 36,500, a
century; fractional days allowed, so a test can use seconds) since they were
last written, read or found. A
digest showing an entry is not a use. Each is closed, not deleted
(`temporal.end`, `expired: "unused"`), as one event of the `sleep` writer with
cause `expire`; `restore` with its id alone brings it back.

- Exempt: `constraints/*`, `user/preferences`, `source: learning` entries, and
  pinned ones.
- The clock starts with a store's first pass (`expiry_since` in the store's
  meta): before 0.8.8 no read was counted, and an upgrade must not close a
  whole store for a disuse nobody measured.
- The pass is a job of the Sleep queue (kind `expiry`), run at the end of a
  turn in the app and of `ha exec`, over the global store and the
  conversation's, at most daily (or once per period, when that is shorter).
  It is local and fast: no model call. A pass that fails, or panics, fails
  its job, which the queue retries.
- `ha doctor` says when the last pass ran and how many it closed (the
  global store's `expiry` meta).

## 17. What a review writes, and Deep Dream in batches (0.9.0)

0.9.0 closes the gaps two audits found in how refinements write the store
(`memory/refine.rs`, `memory/learned.rs`). Each is a rule of `apply`, the
one entry point every refinement - a review, `/refine`, `/learn`, Deep Dream,
the curator, a rollback - writes through.

**An update keeps who kept the entry.** An update keeps the entry's `source`
(`agent`, `learning`, `refine`, ...) and its `origin`, and records the
refinement that changed it in `edited_by`: `{via: "refine", refinement,
conversation, evidence, confidence}`. A create is the review's (`source:
refine`, `origin` the same record). Before, every update set `source:
refine`: a review that reworded a learned rule made it a review entry, Deep
Dream (which skips `source: learning`) could then merge it away, and the
learning loop retired its lesson as deleted. A rollback puts the snapshot
back whole, `source` and `origin` with it, as a new version. The model's
`memory` tool does the same for the learning loop's entries: an update, a
replace or a move of a `source: learning` entry keeps its `source` and
`origin` and records the edit's origin in `edited_by`; any other entry the
tool edits becomes the agent's, as before.

**A rollback undoes only what its refinement wrote.** Each restore carries the
version the refinement left (`after`, or nothing for an entry it deleted).
An entry is rolled back only while it is that version - the same `version`,
title and content; an entry the refinement deleted, only while no entry took
its id. A learned skill's change records the SHA-256 of the `SKILL.md` it
wrote (`undo.written`) and of each other file it wrote or removed
(`undo.written_files`), and is undone only while every one is still so; a
change recorded before 0.9.0 has no hash and is undone as before. A knowledge
page was already deleted only while it is as written. What changed since is
skipped, not failed: the result names it ("Skipped: update local memory
`deploy`: changed since refinement refine_... wrote it: the later version is
kept"), and a rollback that skipped everything is "Harness rollback
unchanged".

**A merge's delete stands or falls with its survivor.** A delete that merges an
entry into another names it in `merged_into`; the curator's prompt asks for it,
and the review's may give it. Such deletes are applied after every other edit
of the refinement, and a delete is skipped when its survivor's edit was not
applied (refused, or skipped because the entry changed meanwhile), when the
survivor is not there after the other edits (deleted in the same refinement),
or refused when it names itself. A delete without `merged_into` (stale,
temporary, planted) is judged alone, as before. Deep Dream's guard rails use
the survivor too: a pinned entry merged into an entry the pass updates keeps
that entry pinned, one merged into an entry kept unpinned is not deleted, a
constraint is merged only into a constraint, and nothing is merged into an
entry the pass deletes.

**A closed entry is kept as it was.** A review, `/refine`, `/learn` and Deep
Dream are not shown the entries a correction or the expiry pass closed, and
those entries count toward no consolidation (`CONSOLIDATE_MIN_ENTRIES`,
`LOCAL_CONSOLIDATE_ENTRIES`). An update, delete or supersede of one, or a
merge into one, is refused with the reason, recorded like any refusal: an
edit would reopen it, or delete what `as_of` and history read. A rollback
still puts back what its refinement closed. An update of a live entry keeps
its `temporal`, `supersedes`, `superseded_by` and `expired`.

**A review's memory edit quotes what it read.** A memory or prompt-note edit
of a review, `/refine` or `/learn` is applied only when its `evidence` is
found in what the refinement read: the conversation (which holds a child's
`[Child findings, from <child>]` block, quoted for a child's fact), and for
`/refine` what the user typed after the command - never the review gate's
instructions, nor those of a refinement the model asked for (`refine.run`),
which are a model's words. The quote is compared with
whitespace and case aside, without the quotation marks around it; pieces
joined by `...` or `…` must each be found, in order; and its pieces together
are a phrase of 12 characters or more, as a lesson's is, each of 4 or more:
letters scattered between ellipses are found in any text. Otherwise the edit is refused with the
reason, recorded with the refinement like any refusal. A global edit without
evidence is still no global edit, and is refused where it stays. The edit's
`confidence` (`high`, `medium` or `low`) is recorded in the entry's `origin`
(or `edited_by`); nothing reads it yet.

**Deep Dream's evidence is the entries it was given.** A merge quotes no
conversation, so its check is on the entries: an edit naming an entry outside
its batch is refused; a delete in a merge names a survivor of its batch; and
an update whose content has more than half of its words (the search's terms)
in none of the batch's entries is refused as adding what no entry says. A
title may be new. The pass creates no entry and supersedes none: a `create`
or a `supersede` the curator proposes is dropped, and the rationale says so.

**Deep Dream in batches.** All the global memory went into one prompt, with no
cap: about 10,000 entries overflowed the model's context, and the pass failed
every time. A pass now:

- orders the scope's entries so related ones are side by side - by kind,
  project and path, then by the rarest word each shares with another entry,
  which a near-duplicate shares with its twin;
- cuts the order into batches of at most `HA_CONSOLIDATE_BATCH_TOKENS` tokens
  of entries (32,000 by default; about four bytes a token, as the context
  compiler counts), each of two entries or more; a memory within the budget is
  one batch, as before;
- moves where the first batch ends, between a quarter and all of the budget,
  by the pass's moment, so the boundaries of one pass are not those of the
  next and two entries a boundary parted meet later;
- calls the curator once per batch, showing it only the batch's entries and
  where it stands (`batch 2 of 5`, the memory's size, the batch's part of the
  30-entry target), and applies the batch's edits at once: one pass deletes
  at most half of each batch;
- records each batch as a refinement `<id>_b<n>` whose `batch` names the pass
  (`{of, index, count}`), shown together under `<id>`, which `/refine
  --rollback <id>` undoes in one go; a pass of one batch is recorded as
  `<id>`, as before;
- fails when a batch's call or apply fails: the job's failure, which the queue
  retries ([SLEEP.en.md](SLEEP.en.md)); the batches before it stay applied,
  and the error names them.

The same holds for a conversation's store past 40 entries and for the
curator's memory part (`/refine --curate`), whose batches precede the skills'
refinement `<id>`.

## 18. Code an entry cites, and memory kept for some files (0.9.2)

A fact about code goes stale when the code changes, and nothing in an entry
said so: the digest kept presenting it as fact. And a rule that matters only
for some files (how billing rounds, what a migration must not touch) was
either in every turn's digest, costing a place among the six, or out of it
when it mattered. Two features of current agents answer this: GitHub Copilot
Memory keeps citations to code with each memory and checks them against the
current branch when the memory is retrieved; Claude Code loads a
`.claude/rules/*.md` whose `paths:` match a file it reads, and Amp and Devin
scope knowledge to paths the same way. Both are measurable and switchable
(§18.5).

### 18.1 The fields

All in `memory/citations.rs` (`memory_citations`), as §16.1's are in
`memory/validity.rs`:

| Field | On | Says |
|---|---|---|
| `citations` | an entry the `memory` tool wrote with them | the code it rests on: `[{path, quote, lines, project}]` - a workspace-relative file, a short span of its own words, the lines `[start, end]` the host found the span at, and the key of the project it was written in |
| `paths` | an entry kept for some files | gitignore-style globs, relative to the workspace root (`src/billing/**`) |

Two records sit beside the entries, never in them: the store meta key
`citations` holds the last check of each entry's citations, by memory id
(§18.3); a conversation's store meta key `scoped_loaded` holds what the
conversation loaded (§18.4).

### 18.2 Citations, checked at write

The `memory` tool's `write`, a create or an update, takes
`citations: [{path, quote, lines?}]`, at most 5:

- `path` is a file of the conversation's workspace, relative to its root; an
  absolute path inside it is made relative, one outside it, or with `..`, is
  refused. A file over 1 MiB is not read.
- `quote` is the code's own words the fact rests on: at least 8 characters
  besides whitespace, at most 400. It is compared with whitespace folded: a
  run of spaces, tabs and line breaks is one space, case is kept.
- `lines` (`[start, end]`, 1-based) is optional. Given, the quote must be
  found within them; left out, it is found anywhere, at its first place.

The host checks each citation before the write runs and keeps it with the
lines it found the quote at and the project's key. One that does not hold is
refused with the reason, for example `citation 1: src/billing/tax.py does not
hold that quote (compared with whitespace aside): copy a short span of the
file's own words`, or `... holds that quote at lines 6-6, not within 1-2:
give those lines, or leave lines out`.

An update's `citations` replaces the entry's whole; `[]` or `null` clears
them. A create that replaces the entry its name holds (`on_conflict:
replace`) keeps only the citations it gives. An update by a review,
`/refine`, `/learn` or Deep Dream, and an accepted queue item (§19.3), cannot
give `citations` or `paths`: the entry keeps its own. A citation may sit on an entry
of any scope; read in another project's workspace it is not checked (§18.3),
never called broken: those are other files.

**The host does not attach a citation itself.** When provenance says the
entry's words came from a file read (`origin.kind = file`, §16.4) and the
write gives none, the result's `cite` says how to add one. The host knows
which file was read, not which span of it the fact rests on: a guessed span
would later mark a sound entry as stale, or keep a stale one looking sound.

### 18.3 Citations, checked on read

Each read of an entry checks its citations against the files as they are:
the digest, for each entry it shows (pinned ones included); `search`, for each
hit, in both modes; `read`. A citation:

| State | When | The lines |
|---|---|---|
| `holds` | the quote is where it was found | as kept |
| `moved` | the quote is elsewhere in the file (lines inserted above it) | the place nearest to where it was last found: they follow the code |
| `broken` | the file is gone, is no longer a file, or no longer holds the quote | |
| `unchecked` | the file is over 1 MiB or cannot be read, the citation is another project's, or the read's budget is spent | |

The entry's verdict is `broken` when one citation is, `unchecked` when one is
and none is broken, else `holds`. A search hit and a read carry `citations`
(each one's `path`, `state`, `lines` and `reason`), `citations_state` (the
verdict) and, when broken, `citations_note`.

The digest shows `[cites src/billing/tax.py:6]` after an entry whose citations
hold, and after one whose citations are broken `[its cited code changed
since: src/billing/tax.py no longer holds the quoted code; check the code
before relying on it, then update or supersede the entry]`: it is not
presented as fact. It is marked rather than left out, so the model sees it
and can update or supersede it, and an ablation (`citations` off) measures
the mark. An unchecked entry is shown as before.

- **Cost.** Files are cached by path, modification time and size (256 of
  them, the least recently used going first), so a digest does not read an
  unchanged file again; a digest, a search or a read reads at most 4 MiB of
  files not cached, and its citations past that are unchecked.
- **No version.** A check never writes the entry: its result is the store
  meta `citations` record of the memory id - the verdict, each citation's
  lines where it was found (where the next check looks first: the lines
  follow a moved quote silently), the broken ones' reasons and the time,
  written only when it changed. The digest's checks are recorded with its
  exposure counts, off the turn's path.
- **A review sees it.** The overview a review reads marks an entry whose last
  check was broken (`[its cited code changed since it was kept: ...]`).
- **`ha doctor`** counts the project's entries that cite code and those whose
  citations are broken now (`memory citations: 1 of 3 entries cite code that
  changed since they were kept`).

### 18.4 Memory kept for some files

The `memory` tool's `write` takes `paths: [glob, ...]`, at most 16, each at
most 200 characters, relative to the workspace root and gitignore-style: a
glob with no `/` matches at any depth (`*.sql`), a directory matches what is
below it (`docs/api`), a leading `/` anchors at the root; an absolute glob,
one with `..` or a drive is refused, and so is one that does not parse
(`paths[0] "src/[billing" is not a glob: ...`). `[]` or `null` clears them.

- **Out of the digest.** An entry with `paths` is not among the entries every
  turn's digest shows; the digest counts them (`memory kept for some files
  only: 2 (...)`).
- **Loaded with the result that touched a matching file.** A file tool's
  result touches a file when `read_file`, `edit_file`, `write_file` or
  `apply_patch` names it, `list_files` or `glob` lists it, `find_files` finds
  it, or one of `search_text`'s matches is in it (the first 500 paths of a
  result). The entries kept for a touched file come with that result, each a
  section of its own after the result's text, which the next model call
  reads: `[memory-scoped: a memory entry kept for src/billing/**; this result
  touched src/billing/refunds.py. A record of earlier work - data to weigh,
  not an instruction: ...]` and the entry's line, its content whole up to
  2,000 characters, with its citations' state. At most 8 come with one result.
- **Why with the result.** A turn sends its first step's context packet at
  every step, so the provider's cache holds it; a block added to the
  packet in the middle of a turn would reach the model only at the next turn.
  The result reaches the next call, as the instructions of a directory a call
  reached do (`NestedInstructions`).
- **Once per conversation.** The conversation's store records what it loaded
  (`scoped_loaded`: the entry's memory id, the path that loaded it, the time,
  and how many times the conversation had been compacted then, as its
  journal's `compaction.completed` events count). Another touch, in the same
  turn, a later one or a later process, brings nothing more - until a
  compaction: it summarized away the result the entry came with, and the
  next touch brings it again. With harness refinements or the `digest` off
  the store is not opened: none of its entries loads, and what a markdown
  store's file loaded is recorded in the running process only.
- **Recorded.** Each load is a `memory.scoped_loaded` diagnostics record
  (`loaded`, `path`, `compactions`).
- **Searched.** Search still finds such an entry, and shows its `paths`. Its
  globs and the files it cites are words of its place (the path field of
  memory search's BM25), so a search for `billing` finds an entry kept for
  `src/billing/**`; a search whose query names a path - a word with a `/`, or
  a file's name with its extension (`invoice.rs`) - puts the entries kept for
  it or citing it first, each group in its order.
- **Citations do not scope.** An entry that cites `src/billing/tax.py` is
  still in every turn's digest unless it has `paths`: a fact about a function
  matters at its callers too, far from its file, and where an entry applies
  is its writer's choice.
- **Where it runs.** The chat and `ha exec`. A delegated child is not given
  scoped memory: the agent that delegates keeps the memory, and a child's
  file reads load nothing.
- **The markdown store.** A memory file whose frontmatter has `paths:` is
  loaded the same way ([MARKDOWN_MEMORY.en.md](MARKDOWN_MEMORY.en.md) §9).

### 18.5 Measured, and switched off

`HA_MEMORY_OFF` takes two more features ([operator guide](../guides/OPERATOR_GUIDE.en.md)
§12.21):

- `citations`: citations are neither checked nor shown - no check in the
  digest, search or read, no mark, no meta record - and a write that gives
  them is refused; the tool does not offer the argument.
- `scoped`: entries kept for some files are neither loaded nor written - no
  load with a result, the `memory` tool does not list, search or read them,
  a write that gives `paths` is refused and the argument is not offered - and
  a markdown store's files with `paths:` are not loaded.

`/context` and `ha doctor` name them when they are off. `memory-ab.sh` has a
task for each: `retries` (the agent kept the HTTP client's retry limit with a
citation, the test then changed the line: the task succeeds when the run
notices the kept fact went stale) and `ledger` (an entry kept for `app/billing/**`
names the ledger the billing modules post to, which a new module records; a file of the
repository would not do, since a run with memory off reads the markdown
store's files as any file).

## 19. The person's review queue, and what a child proposes (0.9.2)

Until 0.9.0 nothing waited for a person: a review, `/refine`, `/learn` and
Deep Dream applied what passed the checks at once, and refused the rest -
a fetched page's words made a rule among them, which the person could not
take even when it was right. The queue (`memory/review_queue.rs`) holds the
changes the person should have a say in, beside the store they would change.

### 19.1 What waits

A memory or prompt change a refinement planned is checked as it would be
applied - the evidence quote, the secrets and steering text, the size, the
duplicates, a closed entry - and then, instead of being applied, waits when:

| `HA_MEMORY_REVIEW` | What waits |
|---|---|
| `policy` (the default) | a change that writes, changes or removes a rule every later turn follows - an entry in `constraints/*` or `user/preferences`, now or before the change - from a fetched page's words (0.9.0 refused it), or from evidence that is not the user's own words (it quotes a file, a tool, a child's finding, or nothing: the review inferred it). Deep Dream's merges rest on the entries, not on a quote: only a page's words in them wait. And a delegated child's proposal its parent's review did not keep (§19.4). |
| `all` | every memory and prompt change of a review, `/refine`, `/learn` and Deep Dream, and every child's proposal the review did not keep |
| `off` | nothing: what 0.9.0 did (a page's words into a rule refused, a child's proposal the review did not keep dropped) |

`HA_MEMORY_REVIEW` wins over the `review` field of the `memory` key of
`settings.json` (`{"memory": {"review": "all"}}`), as the memory switch reads
its own. The default is `policy`: the person is asked about what steers every
later turn and rests on words they did not say, and about nothing else. The
`reviews` feature off (`HA_MEMORY_OFF=reviews`, `HA_MEMORY=off`) runs no
review, so nothing is queued either. memory-ab needs no feature of its own for
the queue: `HA_MEMORY_REVIEW=off` is the queue off, and what waits is not in a
request, as with memory off.

A merge's delete whose survivor's change waits, waits on it (`after`): it is
accepted only after the survivor's change, and leaves the queue when that is
rejected. A rollback and an accept never wait.

### 19.2 The queue

One file per scope, `memory_queue.json` in the scope's folder (the global
store's `harness/`, a conversation's `sessions/<task>/harness/`), written
whole under `memory_queue.lock`. An item holds:

| Field | Holds |
|---|---|
| `id` | `mq-` and eight hex digits |
| `at_ms` | when it was queued |
| `scope`, `conversation` | the store it would change, the conversation whose review proposed it |
| `edit` | the edit as `apply` takes it (`action` create, update, supersede or delete, `kind`, `id`, `title`, `content`, `path`, `evidence`, `reason`, and `origin_kind`, the host's) |
| `expected` | the entry as it was when proposed (`version`, `title`, `content`), or `null` for one that was not there |
| `by` | who proposed it: `{via: review, refine, learn, deep_dream or child, refinement, child, role}` |
| `why` | why it waits |
| `fingerprint` | what makes two proposals the same change |
| `after` | the item it waits on |

An item waits 30 days (`KEEP_DAYS`), then expires unread. The same change
proposed again while it waits is not queued twice. The refinement records a
change that waits as `queued` (the item's id) and `waits` (why), neither
applied nor failed; its outcome says how many wait (`Harness unchanged · 1
change waits for your review`), and the `review` diagnostics record counts
them (`queued`).

### 19.3 Accept and reject

`ha memory queue` and `/memory queue` (§12.22 of the operator guide) list the
items of every queue under the data directory, show one with its evidence and
a diff against the entry as it is now, and accept or reject them.

- **Accept** applies the change through `apply`, the write path a review's
  edit takes, with its checks: a duplicate, a secret or a closed entry is
  still refused, and the item keeps waiting. The entry must be as it was when
  the change was proposed - the same `version`, title and content, or still
  absent for a new entry - else the change is refused as stale (`stale: the
  entry changed since the change was proposed (version 1 then, 2 now)`) and
  leaves the queue. An accepted change is the person's: its `origin.kind` (or,
  for an update, the entry's) is `user`, and `origin.accepted` (or
  `edited_by.accepted`) records the item, the kind its words had (`from`), who
  proposed it and when. The page's-words refusal is not made: the person made
  it a rule. It is recorded as a refinement, which `/refine --rollback` undoes.
- **Reject** takes the item out and remembers the change for 90 days: its
  kind, whether it writes or deletes, the entry it changes, its fingerprint,
  its words and its quote. The same change proposed again - the same entry (or
  a new one again) with 60 % of its words the same or resting on the same quote
  (a child's proposal and the review's edit that kept it are worded apart), or a
  delete of the same entry - is refused as rejected
  (`the person rejected the same change in the memory queue`), not queued. The
  next reviews, `/refine` and `/learn` are told the ten newest rejections
  (`<rejected_by_user>`) and asked not to propose them again unless the
  conversation shows the user wants them now.

### 19.4 What a child proposes

0.8.9's `note_finding` let a child keep a fact with the words of its own tool
result that show it; the parent's review could turn it into memory. A
proposal builds on it rather than beside it: `note_finding` takes an optional
`propose` - `{title, path?, scope?: global (the default) or local, content?
(the fact, when left out)}` - so a proposal has the finding's checks (its
evidence must be in one of the child's tool results, at most 8 a run) and its
way to the review. A sibling tool would have needed the same checks and the
same file twice.

- The finding is kept with its proposal in `child_findings.jsonl`, and the
  review reads it in the `[Child findings, from <child>]` block, marked
  `[proposed as a global memory entry in <path>, title "...": ...]`.
- When an edit of the review rests on the finding - its evidence quotes the
  finding's line as the review read it - and that edit is applied, the
  proposal is closed as `kept`: the entry is the review's. When that edit
  waits for the person, the proposal is closed as `queued` with its item.
- Otherwise, once the review succeeded (or found nothing to refine), the
  proposal waits in the queue of its scope as the child's - `by.via` child,
  `origin_kind` child - checked as a review's edit is (a duplicate or a
  rejected change is refused: `refused`). With `HA_MEMORY_REVIEW=off` it is
  dropped (`dropped`).
- A proposal is never applied by the child's authority. The child's system
  text and the tool's description say so. How each ended is recorded on the
  finding (`proposal.closed`, `note`, `queued`) and in the diagnostics
  (`memory_queue child_proposal`).

### 19.5 Where it shows

- After a review that queued changes, the session says so once, with where to
  review them (`memory: 1 proposed change waits for your review (2 in all):
  /memory queue ...`); the diagnostics record `memory_queue queued`. `ha exec`
  prints it with the review's other lines.
- `ha doctor` and `/doctor` warn while changes wait, and how many a child
  proposed; `/context` has a `Memory queue:` line while changes wait or the
  mode is not the default; the bug report's Memory section counts them and
  copies the queues (their outline without `--include-content`).
