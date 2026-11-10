# Memory store and memory-engine parity

English | [Tiếng Việt](MEMORY_STORE.vi.md)

This is a reference for how ha's memory store works: the SQLite stores, the
`memory` tool's writes, searches and filters, history, ranking, forgetting,
validity and trust, what reviews write, citations and scoped memory, the review
queue, the memory switch, and Deep Dream checked against the transcripts.
Where this document and the code differ, the code is the authority.

The design follows [timescale/memory-engine](https://github.com/timescale/memory-engine)
(read at commit `9c9bdc1`, 23 October 2026). The later parts of that plan - CLI
formats and packs, importers, and integrations with an MCP server - were not
built (§8).

## 1. Goal and decisions

memory-engine (ME) is a team memory server: Postgres with `ltree`, `pgvector` and
BM25. Agents reach it over MCP. ha keeps memory in one SQLite store per scope
(§3), knowledge as Markdown pages, and a history index in SQLite. The goal is
that every ME capability a single user can use also exists in ha, running
locally; §8 lists what was not built.

The maintainer decided:

| Topic | Decision |
|---|---|
| Sharing | **Out of scope for now.** ha stays local and single-user: no spaces, principals, groups, grants, API keys, login, invites or remote backend. To be revisited later. |
| Memory storage | **One SQLite store per scope** with ME's record model: tree, name, meta, temporal, version and an event log. It replaces `harness_state.json`. |
| Meaning | **No embeddings (0.8.8).** Search and the digest rank by words, then by how recently and how much an entry was used (§10). Nothing waits on a network call, and nothing leaves the machine. |
| Knowledge | **Stays Markdown files** that people can edit by hand. It has page history and revert, and shares one search with memory. |
| Filters | **Full parity:** tree exact/lquery/ltxtquery, `meta` containment, a JSONPath `metaPredicate`, five temporal modes and `grep`. |

What ha keeps, because ME has no equivalent:

- the digest injected into every turn, with pinned entries and constraints;
- the automatic review, Deep Dream and the skill curator;
- duplicate refusal on write;
- query variants (`queries`);
- the knowledge link graph.

ME deliberately has no extraction layer. In ha that layer stays, writing through
the store.

## 2. Feature map

| memory-engine | ha |
|---|---|
| `content`, version and CAS | `expected_version` or `version_hash` on an update (§4) |
| Hierarchical `tree`, address by `tree/name`, immutable id | `tree`, a `name` unique per tree, `memory_id` (§3.2, §4) |
| Free `meta` object with reserved keys | `meta`, the entry's `metadata` (§3.2) |
| `temporal` point or range | `temporal`, stored and filtered (§3.2, §6.3), and an entry's validity (§12) |
| `on_conflict` | `error`, `replace`, `ignore` (§4) |
| Append-only event log, 30-day retention | `memory_event` with actor, cause and `operation_id`; the 10 newest versions always kept (§3.5) |
| History by memory, tree, operation or time, with a cursor | `history` (§5) |
| Revert and undelete | `revert` for memory (§4) and for knowledge pages (§7) |
| `tree`, `count`, `mv`, `copy`, `delete_tree`, get and delete by path | `memory` tool actions (§5) |
| BM25 with the positive-match invariant | BM25 with English stemming (§6.1), indexed once per process (§11) |
| Semantic and hybrid search | None: removed in 0.8.8; ranking by words, recency and use (§10) |
| Filters `tree`, `meta`, `metaPredicate`, `temporal`, `grep` | All of them (§6.3) |
| `select` and `format` | On `search`, `read` and `history` (§6.4) |
| `memory.embeddingStatus` | `status` (§5) |
| One search over memory and knowledge | `sources: ["memory", "knowledge", "history"]` (§7) |
| The `me memory` CLI | `ha memory`: export, purge, queue, lint, init, migrate (§8) |
| Import and export formats, packs, importers, MCP server, harness integrations, web UI | Not built (§8) |

Out of scope: spaces, principals, groups, grants, API keys, service accounts,
invites, login, the CI import workflow, a remote backend, and the Granola and Slab
importers (one is macOS only, the other a niche service).

Not taken from ME: its batch create (only its importers use it), its stopword
list (ha does not judge text by word lists), and `strict` JSONPath mode (ME
runs lax).

## 3. Storage

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

The read-modify-write lock is `memory.lock` beside the database. The database
runs in WAL mode with a 5 s busy timeout. `PRAGMA user_version` carries the schema version; migrations are
ordered Rust functions, applied when the database is opened.

ha talks to it through `rusqlite` 0.39. That version links the same
`libsqlite3-sys` 0.37 (vendored SQLite 3.53.2) as `sqlx`, so there is one SQLite
library, called synchronously. The memory code is synchronous, so it does not
need a runtime bridge.

### 3.2 Schema

```sql
CREATE TABLE memory (
  id              TEXT PRIMARY KEY,      -- UUIDv7, immutable
  kind            TEXT NOT NULL,         -- prompt|memory|skill|subagent|factory
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
  actor        TEXT NOT NULL,      -- JSON: {"type": model|refine|kernel|migration|learning|sleep|cli, ...origin}
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
-- schema 2 (0.8.1): memory_vector, memory_embedding_queue (dropped by schema 4)
-- 0.8.2 (schema 3): the V1 `name` column renamed `key`, and `name` added
-- 0.8.8 (schema 4): memory_usage added, the two embedding tables dropped (§10)
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
  "project" scope. A record without a project is global.
- **meta.** The entry's existing `metadata` object, so `pinned` is `meta.pinned`.
  Reserved keys, as in ME: `$prev`, `$next`, `$thread`, `display_name`, `pinned`.
  `meta` is replaced whole on update, as in ME.
- **temporal.** A point (`start` only) or a half-open range `[start, end)`. The
  inputs are a string, a list of one or two strings, or `{start, end?}`.
- **doc.** Holds everything ha writes, unknown fields included, so the document
  view in §3.3 round-trips exactly.

### 3.3 The document view (compatibility layer)

Three functions read and write the whole state:

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
  - bumps the memory-change counter (`memory_changes`).
- `Writer { actor, cause }` is required. Every call site states who writes:
  - `model`: the `memory` tool;
  - `refine`: every refinement (`/refine`, the automatic review, Deep Dream,
    the curator), with the refinement's id, which `refinements.jsonl` explains,
    and cause `refine`, or `rollback` for a rollback;
  - `kernel`: `rlm.harness`;
  - `migration`: a legacy import;
  - `learning`: the learning loop, placing a lesson's rule;
  - `sleep`: the expiry pass (§12.5);
  - `cli`: `ha memory purge` (§11).

Filters, history, tree operations and search query the rows directly. The
document view serves the callers that read or write the whole state.

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

There is no downgrade command. `ha memory export` prints a store's state as
JSON in `harness_state.json`'s shape, and the `.imported` file is the state
from before the upgrade.

### 3.5 Event log retention

Events older than 30 days are dropped, as in ME. The exception: the 10 newest
versions of each memory are always kept, as many as a `history` page shows by
default. A
memory that has not changed for a month can still be reverted.

Pruning runs at most once an hour, inside a write. History and revert reach
exactly as far as the log does.

## 4. Writes and identity

The `memory` tool's `write` takes, besides a title and content:

- `name`, an explicit slug, unique in its tree;
- `tree`, either absolute (`/global/...`, `/projects/<this project>/...`,
  `/local/...`, which also chooses the scope) or relative to `scope` (`path`,
  also accepted);
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

`revert`, with `restore` as an alias:

- it re-applies a snapshot from the event log as a new forward version, with
  cause `revert`;
- reverting a deleted memory recreates it under its old id, continuing its version
  sequence;
- reverting into a key, or a `(tree, name)`, that another entry now holds is a
  conflict.

The digest leaves out entries whose temporal range
has ended (`temporal_end <= now`), which are history rather than current fact.
Search still finds them. Since 0.9.0 an entry a correction or the expiry pass
closed is left out of a search too, unless it names a time or `include_ended`
(§12).

## 5. Organizing and history

The `memory` tool's actions for organizing and history. Children get only the
read-only ones.

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

## 6. Search

### 6.1 Lexical

BM25 runs in `memory/search.rs` over the entries a search considers, after the
filters. A global and a conversation's store rank as one corpus, with one set of
statistics. There is no FTS5 index: since 0.8.9 each store's words are indexed
once per process (§11).

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

### 6.2 Meaning (removed in 0.8.8)

0.8.1 to 0.8.7 also searched by meaning, fusing embeddings with BM25 by weighted
RRF. 0.8.8 removed it (§10): a search's modes are `fulltext` and `filter`, and
`semantic`, `semantic_threshold`, `weights` and `has_embedding` are gone from
the tool. `limit` (10, at most 200) and `candidate_limit` (30, at least
`limit`, at most 1000) remain.

### 6.3 Filters

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

### 6.4 Presentation

| Parameter | Behaviour |
|---|---|
| `select` | Fields to return: `id`, `memory_id`, `name`, `tree`, `kind`, `title`, `meta`, `meta.<key>` (under `meta`, left out when missing, as ME projects it), `temporal`, `version`, `version_hash`, `score`, and the content slices `content:N`, `content:M:N`, `content:M:` (characters, with `content_length` added). On `search`, `read` and `history`. |
| `format` | `json` (the tool default, compact), `compact`, or `yaml`; on `search`, `read` and `history`. |

`format: yaml` comes from a small emitter in `memory/search.rs`, which quotes any
string YAML could read as something else.

## 7. Knowledge and history

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

The `knowledge` tool has:

- `history` (a page, a folder or a time feed, with `since`, `until`,
  `operation` and a per-scope `cursor`); with `version`, that version whole, to
  read before reverting;
- `revert` (`path`, `version` = an event id).

Reverting a deleted page recreates it, subject to the same backlink rule a write
has. Retention is the same as memory: 30 days, with the 10 newest versions of
each page always kept.

### One search over memory, knowledge and history

`memory` search accepts `sources: ["memory", "knowledge", "history"]`, any of
them; the default is memory only. Knowledge hits come from the knowledge
engine (BM25); history hits are the turns of this project's conversations as
the journal keeps them, found by `history_search`'s own search with scope
`project` (the index caught up first, at most three seconds, the rest behind
it; each query variant searched and the variants fused). Each source's hits
are fused by RRF into one list, by rank, since their scores are not
comparable. Each hit says `source`, plus `path` and the line range for a page,
and `source_id`, `kind`, `conversation`, `started` and the head of the text
for a turn (`history_read` reads its `source_id` whole). A turn has no tree,
meta or temporal value: those filters leave every turn out, and `grep` reads a
turn's head. History needs the turn's project store: a search where there is
none (a kernel's request, a store already closed) says history was not
searched. The default stays memory alone; the article this follows ("The
components of agent memory") found a wiki and search together beat either,
not that every search should read the journal.

The filters reach the pages as ME's would read a record with no meta and no
temporal value: a `meta`, `meta_predicate` or `temporal` filter leaves every
page out; a `tree` filter reads a page at `/knowledge/<scope>/<its folders>`;
`grep` reads its whole text; `select` picks its fields. The result says so.

### History tools

- **`history_search`** takes:
  - `since` and `until`: a source id is the journal event's `event_<UUIDv7>`,
    whose head is its time, so a window of time is a range of ids and needs no
    new column;
  - `kind`, one journal event type or a list of them;
  - `order`, `newest` (the default) or `oldest` first among equal scores.

- **`history_read`** returns `previous_source_id` and `next_source_id` within the
  same session, by `sequence`, so a reader can walk the thread without searching
  again.

## 8. The `ha memory` command

`ha memory` works on the stores and on a project's markdown memory store
(`crates/harness-cli/src/commands/memory_cli.rs`):

| Command | Does |
|---|---|
| `ha memory export [--conversation ID]` | Prints a store's whole state as JSON, its entries and refinements, in `harness_state.json`'s shape: the global store, or a conversation's |
| `ha memory purge <id or key> [--yes]`, `ha memory purge --secrets [--yes]` | Forgets an entry, or every entry and version that holds a secret value, with every copy beside the store; it lists first and changes nothing without `--yes`; `--scope`, `--conversation`, `--cwd` (§11) |
| `ha memory queue [list\|show\|accept\|reject] [id...] [--json]` | The changes that wait for the person's review (§15.3) |
| `ha memory lint` | Checks a markdown memory store; exits 1 when a check fails ([MARKDOWN_MEMORY.en.md](MARKDOWN_MEMORY.en.md)) |
| `ha memory init [--hook] [--examples]` | Writes an empty markdown memory store |
| `ha memory migrate` | Turns a flat memory file into the markdown store through an `ha exec` run, then lints the store |

A model reads and writes the stores through the `memory` tool; the CLI has no
`create`, `get`, `search`, `update` or `delete`.

**Not built.** The rest of the plan modelled on ME:

- `ha memory` actions mirroring `me memory` (`create`, `get`, `search`, `edit`, ...), and import and export in JSON, YAML, Markdown and NDJSON;
- memory packs (`validate`, `install`, `list`);
- importers for docs, git history, and Claude Code, Codex and OpenCode sessions, and the `record` kind they would write;
- an MCP server offering the memory tools to other harnesses, harness integrations with capture hooks, and a web console for memory (`ha memory serve`).

## 9. Compatibility

- The `memory` tool, the kernel's `rlm.harness`, `/refine`, `/learn`, Deep Dream
  and the digest read and write through the document view (§3.3), so they keep
  the JSON shape they had before the stores.
- Children stay read-only: every `memory` action is classified as read or write,
  and children get the read ones.
- The bug report includes a JSON export of both stores instead of the raw files,
  with event counts.
- Nothing leaves the machine.

## 10. Ranking by words, recency and use

0.8.8 removed semantic search everywhere: memory search, the digest's
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

## 11. An index for ranking, and forgetting

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

## 12. Validity and trust

Since 0.9.0 an entry says when it holds and where its words came from.

Memory does not help by itself: most memory setups measured in 2026 did no
better than memory off. What the current systems have and ha lacked: a
contradiction resolved by closing the old fact's validity rather than
deleting it (Mem0's ADD/UPDATE/DELETE/NOOP, Zep/Graphiti's `invalid_at`),
questions about time, trust by source against memory poisoning (MINJA), and
expiry of memory nobody uses (Copilot drops memory unused for 28 days).

### 12.1 The fields

All in `memory/validity.rs`, the one place a later field joins them:

| Field | On | Says |
|---|---|---|
| `origin.kind` | every entry the memory tool or a review writes | where its words came from: `user`, `tool`, `web`, `file`, `agent`, `child`, `review`. The host decides it (§12.4). An entry without one reads as `agent`. |
| `supersedes` | an entry that corrected others | the memory ids of the entries it closed |
| `superseded_by` | an entry a later one replaced | that one's memory id |
| `expired` | an entry the expiry pass closed | why: `unused` |

An entry's **validity** is its `temporal` value: a period `[start, end)`, or a
point it holds from on. Without one it holds from its creation on. Closing an
entry gives its period an end, now, from the start it had (its creation when
it had none). The digest leaves out a closed entry, as it does an ended
period, even while its period has not ended: a future-dated entry superseded
today ends a millisecond after its start.

### 12.2 Contradictions are resolved at write, by closing validity

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
new entry keyed by its title, next to the old one's path and project, kept for
the same files (`paths`, §14.4) but citing none of its code, and closes the
old one (`validity::supersede_edit`). The refinement records it as
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

### 12.3 Questions about time

Memory search takes:

| Argument | Reads |
|---|---|
| `as_of` | the entries valid at that time: their validity covers it, and they had been written by then. A corrected fact's old entry, asked about a time before the correction. |
| `since`, `until` | the entries valid at some time between |
| `include_ended` | the closed entries too |

A search that names a time (`as_of`, `since`/`until`, or the temporal filter
of §6.3) reads closed entries, and ranks the results by how well their
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

### 12.4 Trust by source

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
- Such an entry ranks below an otherwise equal one (§12.3).
- An entry whose kind is `web` - now, or when it was kept - cannot be written
  into `constraints/*` or `user/preferences` (and below): a create, an update,
  a move or a review's edit is refused with the reason, before the call runs.
  The model is told to keep it as a fact elsewhere, or to ask the user to
  state the rule.

### 12.5 Expiry by use

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

## 13. What a review writes, and Deep Dream in batches

0.9.0 closed the gaps two audits found in how refinements write the store
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
its `temporal`, `supersedes`, `superseded_by`, `expired`, `citations` and
`paths` (§14).

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

## 14. Citations and scoped memory

Since 0.9.2 an entry can cite the code it rests on, and can be kept for some
files only.

A fact about code goes stale when the code changes, and nothing in an entry
said so: the digest kept presenting it as fact. And a rule that matters only
for some files (how billing rounds, what a migration must not touch) was
either in every turn's digest, costing a place among the six, or out of it
when it mattered. Two features of current agents answer this: GitHub Copilot
Memory keeps citations to code with each memory and checks them against the
current branch when the memory is retrieved; Claude Code loads a
`.claude/rules/*.md` whose `paths:` match a file it reads, and Amp and Devin
scope knowledge to paths the same way. Both are measurable and switchable
(§14.5).

### 14.1 The fields

All in `memory/citations.rs` (`memory_citations`), as §12.1's are in
`memory/validity.rs`:

| Field | On | Says |
|---|---|---|
| `citations` | an entry the `memory` tool wrote with them | the code it rests on: `[{path, quote, lines, project}]` - a workspace-relative file, a short span of its own words, the lines `[start, end]` the host found the span at, and the key of the project it was written in |
| `paths` | an entry kept for some files | gitignore-style globs, relative to the workspace root (`src/billing/**`) |

Two records sit beside the entries, never in them: the store meta key
`citations` holds the last check of each entry's citations, by memory id
(§14.3); a conversation's store meta key `scoped_loaded` holds what the
conversation loaded (§14.4).

### 14.2 Citations, checked at write

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
`/refine`, `/learn` or Deep Dream, and an accepted queue item (§15.3), cannot
give `citations` or `paths`: the entry keeps its own. A citation may sit on an entry
of any scope; read in another project's workspace it is not checked (§14.3),
never called broken: those are other files.

**The host does not attach a citation itself.** When provenance says the
entry's words came from a file read (`origin.kind = file`, §12.4) and the
write gives none, the result's `cite` says how to add one. The host knows
which file was read, not which span of it the fact rests on: a guessed span
would later mark a sound entry as stale, or keep a stale one looking sound.

### 14.3 Citations, checked on read

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

### 14.4 Memory kept for some files

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

### 14.5 Measured, and switched off

`HA_MEMORY_OFF` (§16) takes two more features ([operator guide](../guides/OPERATOR_GUIDE.en.md)
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

## 15. The review queue and child proposals

Until 0.9.0 nothing waited for a person: a review, `/refine`, `/learn` and
Deep Dream applied what passed the checks at once, and refused the rest -
a fetched page's words made a rule among them, which the person could not
take even when it was right. The queue (`memory/review_queue.rs`, since 0.9.2) holds the
changes the person should have a say in, beside the store they would change.

### 15.1 What waits

A memory or prompt change a refinement planned is checked as it would be
applied - the evidence quote, the secrets and steering text, the size, the
duplicates, a closed entry - and then, instead of being applied, waits when:

| `HA_MEMORY_REVIEW` | What waits |
|---|---|
| `policy` (the default) | a change that writes, changes or removes a rule every later turn follows - an entry in `constraints/*` or `user/preferences`, now or before the change - from a fetched page's words (0.9.0 refused it), or from evidence that is not the user's own words (it quotes a file, a tool, a child's finding, or nothing: the review inferred it). Deep Dream's merges rest on the entries, not on a quote: only a page's words in them wait. And a delegated child's proposal its parent's review did not keep (§15.4). |
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

### 15.2 The queue

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

**A queue that cannot be read is unreadable, not empty**, as a memory store
is since 0.8.7: a `memory_queue.json` that cannot be read or parsed is never written
over, since a save of the empty queue would drop every change waiting there
and every rejection. Its changes are not listed; `ha memory queue` and
`/memory queue` name the file and why (`unreadable: the memory queue ...
cannot be read (...)`, `unreadable` in `--json`, and `list` exits 1), and
`ha doctor` fails on it. A change that would wait in it is not applied
either - nothing goes past the person because their queue cannot take it:
the refinement records it as not applied (`the memory queue could not keep
it, so it is not applied: ...`) and the diagnostics as `memory_queue
not_queued`. A reject or an accept in it is refused.

### 15.3 Accept and reject

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
  The accept holds the item's queue (`memory_queue.lock`) from reading the
  item until it leaves: a reject meanwhile is not overridden, and a queue it
  cannot have applies nothing.
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

### 15.4 What a child proposes

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

### 15.5 Where it shows

- After a review that queued changes, the session says so once, with where to
  review them (`memory: 1 proposed change waits for your review (2 in all):
  /memory queue ...`); the diagnostics record `memory_queue queued`. `ha exec`
  prints it with the review's other lines.
- `ha doctor` and `/doctor` warn while changes wait, and how many a child
  proposed; `/context` has a `Memory queue:` line while changes wait or the
  mode is not the default; the bug report's Memory section counts them and
  copies the queues (their outline without `--include-content`).

## 16. Memory on and off

Since 0.9.0 one switch turns memory off, whole or one feature at a time, so a
run with memory on can be measured against one with memory off
(`memory/switch.rs`). A feature that is off is neither read into a request nor
written after it: a run with memory off leaves every memory store as it was.
The conversation's own task state and the project's AGENTS.md are the task and
its instructions, not memory, and stay.

- **`HA_MEMORY=off`** (or `0`, `false`, `no`) turns every feature off.
  `HA_MEMORY=on` (or `1`, `true`, `yes`) turns memory on over the setting.
- **`HA_MEMORY_OFF=<feature>,...`** turns off the named features alone, for an
  ablation. Names are separated by commas, semicolons or spaces; case does not
  matter, and `-` reads as `_`.

| Feature | What it turns off |
|---|---|
| `digest` | the memory digest in every turn, the `memory` tool and the kernel's `rlm.harness`, a child's digest and the findings it hands back (`note_finding`), and the expiry of unused entries (§12.5); the store is not opened |
| `markdown` | a markdown memory store's rules and index in every turn |
| `session_log` | writing session logs, the log a new conversation is told of, and `/recall` |
| `lessons` | the learning loop's results in a turn: learned rules and their notes in tool descriptions, the learned skill recalled for a request, the learned skills' note, and the count of their use |
| `reviews` | every background pass: the automatic review (after a turn, of a quiet session, after a compaction), consolidation (Deep Dream), curation, evolution, the self-evaluation, `/refine`, `/learn`, and what a review tells the next turn; nothing is queued either (§15.1) |
| `earlier_state` | what a new conversation is told of earlier ones: the task state an earlier conversation left, and the previous conversation's last turns |
| `knowledge` | the knowledge library: its catalog in a turn and the `knowledge` tool |
| `citations` | checking, showing and writing citations (§14.5) |
| `scoped` | memory kept for some files: loading, listing, searching, reading and writing it, and a markdown store's files with `paths:` (§14.5) |

A name that is no feature is ignored, and said: `/context` and `ha doctor` name
it with the list of features. A value of `HA_MEMORY` that is neither on nor off
is ignored the same way.

- **The `memory` key of `settings.json`** says the same: `{"memory":
  {"enabled": false}}` or `{"memory": {"off": ["digest", "knowledge"]}}`. The
  environment wins: `HA_MEMORY` over `enabled`, and `HA_MEMORY_OFF`, even
  empty, over `off`. The same key's `review` field sets which changes wait for
  the person (`policy`, `all`, `off`), and `HA_MEMORY_REVIEW` wins over it
  (§15.1).
- **Where it shows.**
  - `/context` has a `Memory:` line: `on`, `off (HA_MEMORY): ...`, or `on
    except ... (HA_MEMORY_OFF)`, with `settings.json memory` in the brackets
    when the setting turned the features off. A `Memory queue:` line follows
    while changes wait or the review mode is not the default (§15.5).
  - `ha doctor` warns with what is off, and says nothing while all of it is on.
  - Each run of `ha exec`, and the first turn of each app conversation, write a
    `memory.switch` diagnostics record (`off`, `all_off`, `source`, and
    `unknown` for names that are no feature); the app writes it again when the
    switch changes. `ha exec`'s JSON has `memory.off`.
- **Measured.** The live suite `scripts/testing/live-tests/memory-ab.sh` teaches
  a project facts, then runs the same tasks from the same snapshot with memory
  on, off and (with `ABLATE=1`) each feature off alone, and writes a table of
  success, tokens, calls and time against memory off
  ([live tests](../../scripts/testing/live-tests/README.md)).

## 17. Deep Dream against the transcripts, verification, demotes

After Anthropic's "The components of agent memory" (2026-11-06): a note an
agent writes from what one moment seemed to show can be wrong from the start,
or drift, and a later session reads and trusts it. Dreaming tests the notes
against the original transcripts, keeps the injected layer small by moving
detail down and leaving links behind, follows a goal the user sets, and
spends its effort on what is likely to change. The code is
`memory/dream.rs`, called from `memory/refine.rs`.

### 17.1 The turns behind an entry

For each entry of a Deep Dream batch whose `origin` names a conversation, or
whose last edit (`edited_by`) quoted one - a review's edit or a correction,
each with its quote; a Deep Dream merge names only the conversation the pass
ran in, which said nothing of the entry, and is not read - the pass reads that conversation from its project store's
journal, read-only, as the memory tool's provenance does
(`memory_provenance::of_task`: what the user typed in each turn, and each tool
call's result with the kind of its tool; the agent's own words are not
read). The store is the entry's project's, then the project the pass runs in,
then the others (at most 64 looked through). Each turn is redacted first, as
a session log's transcript is (`learned::redact` after
`secret_values::redact`): a tool result that read a `.env` reaches neither the
curator nor a quote. Of those turns it keeps the ones that hold the source's
quote, then the rest by the weight of the entry's words each holds (each word
by its IDF over the conversation's turns), but only a turn holding at least a
third of the weight of all the entry's words (`dream::BEARING_MIN_SHARE`; a
word no turn holds weighs as one a single turn holds) - a turn sharing a word
or two of a longer entry is about something else. Each is cut to 1,500
characters around what matched, at most four per entry.

The budget is `dream::EVIDENCE_TOKENS` (6,000 tokens of excerpts per batch,
about four bytes a token) and `ENTRY_EVIDENCE_TOKENS` (1,200 per entry). The
entries likely to drift (§17.5) get theirs first; once the budget is spent no
journal is opened for the rest, which are named as having no turns. The
curator reads them in a
`<transcript_evidence>` block, each labelled by who said it (`[the user
typed]`, `[a tool result, file]`), framed as records, never instructions; a
`</` inside an excerpt cannot close the block. The entries with no turns are
named, and the curator corrects none of them. A `dream evidence` debug record
counts the entries, the excerpts and the entries without.

### 17.2 `correct`, and how an entry is known

A curator edit `{"action": "correct", "kind", "id", "title", "content",
"evidence", "changed"}` changes what an entry claims. The host checks it
before the apply:

- `evidence` must be one span of that entry's excerpt, word for word
  (`dream::spans`: whitespace and case aside, 12 characters or more, and no
  pieces joined by an ellipsis as a review's quote may be: "runs on port …
  5432" is not found in "runs on port 6543, not 5432"). Otherwise the edit is
  refused (`a correction's evidence must be a span
  of its entry's transcript excerpt ...`), recorded like any refusal. An
  entry without an excerpt cannot be corrected. An edit whose `evidence`
  holds a secret (`learned::secret_hits`) is refused whatever its action, as
  the quote would be kept in the entry's record and the refinement's.
- The correction's words may be the excerpt's: the merge check (more than
  half of the content's words in none of the batch's entries) also counts the
  excerpt's words for it. It may only say what the transcript shows.
- `changed: false` (the fact was never so) makes it an update: the old
  version stays in the store's history, as every update's does. `changed:
  true` (it held, then changed) makes it a supersede (§12.2): a new entry,
  the old one closed for `as_of`, the new one kept as the old one was
  (pinned, its meta) but for how it was known. ha's model is that a wrong
  note was never true, so `as_of` must not read it as a past fact, while a
  changed fact was true then. The curator says which; both need the quote.
- What it rests on is recorded: the entry's `edited_by` (or the new entry's
  `origin`) carries `correction: true`, the conversation of the excerpt, the
  quote and its kind (`user`, `tool`, ...), which is also the edit's
  `origin_kind`. The refinement's row says `Corrected global memory <id>`, and
  `/refine --rollback` undoes it.
- A correction of a rule every turn follows (`constraints/*`,
  `user/preferences`) waits for the person unless it quotes the user's own
  words, as a review's edit does (§15.1).

An entry's meta says how it is known: `verified` is `confirmed` (checked;
`verified_by` says how, at most 160 characters: `test passed: cargo test
billing`, `user confirmed`) or `unverified` (a theory nobody tested). The
review's prompt asks for it on a fact or rule, and for a trap to be kept as a
warning that names what sets it off, without telling the model what to
record: the article measured that fixed rules of what to record hurt strong
models. The host:

- writes `unverified` when the review gave its edit `confidence: low` and no
  `verified`;
- keeps an entry's `verified` through an update that says nothing of it,
  except a correction, whose claim the old check never covered, and a
  `confirmed` whose content the update rewrites (whitespace aside; a demote,
  whose detail moves unchanged, keeps it): the check was of the old words;
- adds an edit's metadata to the entry's when it holds only `verified`,
  `verified_by` or `scope` (a pinned entry stays pinned);
- drops any other value of `verified`, and the `verified_by` of an entry not
  confirmed;
- keeps a Deep Dream `verified: confirmed` only when its `evidence` is one
  span of the entry's excerpt; otherwise it is dropped and the record says so
  (`verified_dropped`).

The digest marks an entry `unverified: check it before relying on it` when its
meta says `unverified`, or says nothing and the review that wrote it last was
unsure (`confidence: low` in `edited_by`, else `origin`). A web page's or a
child's entry keeps its own mark (§12.4).

### 17.3 `demote`: detail down, a pointer left behind

While the knowledge library is on (`HA_MEMORY_OFF` without `knowledge`), the
curator is offered `{"action": "demote", "kind", "id", "content", "page":
{"path", "title", "summary"}}` for an entry of 600 characters or more
(`DEMOTE_MIN_CHARS`), or any entry while the memory is over its 30-entry
target, as an alternative to deleting or cutting it. The host:

- refuses it for a pinned entry or a constraint (read whole in every turn),
  for a global entry kept for one project (`project` set) in the global pass,
  whose detail the global library would show every project, for a statement
  over 100 characters (`DEMOTE_SUMMARY_CHARS`), for a path
  over 48 characters or one a page holds already, and while changes wait for
  the person (`HA_MEMORY_REVIEW=all`): a demote is made whole or not at all;
- writes the page through the knowledge store's own write (`Store::
  write_update`, origin `dream`): the global library for the global memory,
  the project's for a conversation's store. Its content is the entry's
  content exactly, never words of the curator's, so nothing invented or
  injected rides along; an empty entry has none to move. Its title, summary,
  content and sources pass the checks a memory entry does: no secret
  (`learned::secret_hits`), nothing that steers the agent
  (`learned::steering_hits`). The page as written is read back and checked
  to be the entry's content, and removed if not. Its sources are `memory
  <kind>:<id>` and the conversations the entry came from. Each page is noted
  in the refinement's pending record (`pending_refinements`) as it lands, so
  a process that dies before the save leaves no page behind: the next
  refinement in the scope deletes it;
- rewrites the entry to the statement and a pointer, `<statement> (details:
  global knowledge concepts/x.md)`, which fits the 180 characters the digest
  shows of an entry. The statement passes the merge check: it is the entry's
  point in its own words.

The refinement records the entry's update and the page's create. A rollback
deletes the page only with its entry: once the entry is put back and saved,
and while the page is as written; an entry changed since keeps its pointer,
and its page stays. When the save that keeps the pointer fails, the page is
removed.

### 17.4 `dream.md`: memory in service of a goal

The automatic passes read the user's instructions for what the memory is
for: `dream.md` in the global harness directory for the global memory, and
`<project>/.harness/dream.md` (the main checkout's) for a conversation's
store. They reach the curator as `<user_curate_instructions>`, framed as the
user's own file (as the project's AGENTS.md is), guiding what to keep, merge,
correct and demote within the curator's rules. At most 4,000 characters
(`DREAM_INSTRUCTIONS_CHARS`) are read: past it the file is cut on a character
boundary, the prompt says so, and a `dream instructions_cut` warning is
recorded; each read is a `dream instructions` record. What the user types
after `/refine --curate` joins the global file and wins where they differ.

### 17.5 What is likely to drift goes first

The batches of a pass run in the order of how likely their entries are to
have drifted (`dream::drift_scores`), and a batch shows its entries in that
order: a citation of code that no longer holds (its last check, §14.3), then
a state (a `status` in the meta, or a period of validity), then how long
since the entry was written or read against how often it was read or found,
so a rarely used old entry comes before an often used one. An entry a pass
showed the curator the turns of is marked checked (`dream_checked.json`
beside the store, `kind:id` to the time) and waits behind the others, its
place coming back as the check ages (half of it after 7 days; a check older
than 90 days is forgotten), so the excerpts' budget reaches every entry in
turn rather than the same few. Only the fields ha keeps decide; no word list
does. A pass that fails part way has checked those first.

### 17.6 Patterns across sessions

The learning loop's daily meditation ([LEARNING_LOOP.en.md](LEARNING_LOOP.en.md)
§5) also reads the project's newest session logs, while session logs are on:
at most 6, 4,000 characters each and 16,000 in all. It may report `signals`
found in them: a correction or a wrong assumption one log shows again, or
several show. Each signal's quote must be in the log it names, word for word;
it is that log's conversation's signal, tagged with the log (`log` on the
signal), and a lesson that already has a signal from that conversation gets
none from its log. There is no new writer: the gate (3 signals across 2
conversations) promotes as for any signal. Each signal is a `learning
log_signal` record, or a `log_signal_refused` one with why.

### 17.7 Switched off

All of it is Deep Dream's or the meditation's, under `reviews`
(`HA_MEMORY_OFF=reviews` runs none of it). With `knowledge` off no demote is
offered or made; with `session_log` off the meditation reads no log; with
`digest` off no digest shows the mark.
