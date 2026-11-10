# The knowledge library as a wiki

A knowledge library (`.harness/knowledge` of the project, `<data-dir>/knowledge`
for global scope) is written one conversation at a time. Each conversation sees
only what it saw: a page links to a page that was never written, two
conversations write the same subject under two names, and no page says where
the others are. Anthropic's "The components of agent memory" (6 Nov 2026)
describes what closes these gaps: a second, background pass over the whole
wiki that repointed nine links to a missing page at an existing page on the
same document, added an index, merged duplicates and checked claims against
the sources. It also names the failure to avoid: a wiki that grew larger than
the documents it summarized, and so abstracted nothing (Borges' map as large
as the empire). ha's wiki pass does this (`knowledge/wiki.rs`,
`knowledge/wiki_store.rs`).

## Dangling links

A body link to a page that does not exist used to be dropped from the graph
without a trace. Since this release the store keeps it as the page's
**dangling link**:

- a `knowledge` read lists the page's `dangling` links, and a list or a write
  result carries them;
- `/knowledge status` counts them per library and names the first five as
  `page -> target`;
- `ha doctor` (State) warns of a library with dangling links and names them;
  a library with none passes.

A dangling link is not an edge: backlinks, the graph and the delete rule still
see only links to pages that exist. It resolves by itself once the target is
written.

## The pass

The pass is a background job of the Sleep queue (`Kind::Wiki`,
[SLEEP.en.md](SLEEP.en.md)), one job per library, so one pass runs per library
at a time across processes; it fails, backs off and gives up like the others.

**When it is due** (`wiki::due`, checked at a turn's end): the library is on,
has two pages or more, changed since the last pass (a fingerprint of its pages'
paths and hashes, kept in `.ha-wiki.json`), has work for a pass, and either a
review wrote a page since the last pass (trigger `review`) or a day passed
(trigger `due`). A library the pass has not seen yet starts its day's wait
when it is first checked, so a library kept before the pass existed is not
sent to the model at the first turn after an upgrade. `HA_WIKI_DUE_NOW=1`
drops the wait, for tests. A library that changed but has no work is not
checked again until it changes. The pass runs
only while the automatic review runs (`autoRefine`, `/refinements`, the
`reviews` feature) and knowledge is on (`/knowledge off`, `HA_MEMORY_OFF`
`knowledge`).

**What it reads**: every page's path, title and summary, and in full (cut at
6,000 characters each, 100,000 in all) the pages each kind of work needs:

| Work | Found by |
|---|---|
| Dangling links | the store, with the existing pages whose words come closest to each missing page's name (a store search on its file name) |
| Two pages on one subject | the store's same-subject check, pairwise: most of one page's title and summary words, weighted by rarity, are the other's (`SAME_SUBJECT_SHARE`, 0.7), at most six pairs |
| An index | more than `INDEX_MIN_PAGES` (12) pages and no page that links to 60% of the others |
| Claims to check | pages whose source is a workspace file (a path in the workspace, `:line` allowed) or a conversation of the project (a conversation id in the source), not checked since the page or the source last changed; at most four, the newest first, each source cut at 12,000 characters. A protected path (`.env`, `.git`, `.harness`, a key, a name with `secret`, `credential` or `password` in it, as the file tools refuse them) and a file Git ignores are never sources: their text would go to the model. The global library, shared by every project, reads no path relative to this workspace: only an absolute path into it, or a conversation |

The catalog (at most 30,000 characters, then how many pages it left out) and the dangling links (at most 8,000) count against the same budget. A check is marked done only when its page and its source were in the prompt; one the budget left out waits for a later pass. Everything it reads is data, not instructions: the curator's prompt says so,
and the host checks each edit it proposes before applying it.

**What it may do** - one JSON list of at most twelve edits, each applied
through the store with the hash of the page the pass read (a page the user
changed meanwhile is refused as stale), recorded in the page history as the
wiki's (`cause` and `actor` `wiki`, the log's line `by wiki`):

| Edit | Checked |
|---|---|
| `repoint` a dangling link to an existing page | the link is a dangling link of the page; the target exists and is not the page itself |
| `unlink` a dangling link, its label kept | the link is a dangling link of the page |
| `create` the missing page | its path is a missing page a link points to; it names the pages it is written from, and those hold at least 80% of its word weight (`CREATE_SUPPORT`); its sources are theirs and it relates to them |
| `merge` two pages | the pair is one the same-subject check found; the merged text keeps at least 80% of each page's word weight (`MERGE_KEEP`); the host joins their sources and related pages, points every page that linked to the absorbed one at the kept one, and deletes the absorbed one (its history keeps it, so `revert` brings it back) |
| `index` | asked for; nothing that looks like a secret; headings of at most 120 characters and `- [Title](path)` lines only; a link to a page that is not there is left out, pages it left out are listed at its end, and an index that leaves out more than a fifth of the pages is refused; written at `topics.md` (a page of the user's own there is never touched) |
| `fix` a claim | the source is one of the page's sources and readable; the quote is in it word for word (whitespace and case aside, at least 8 characters); it has a replacement; the text it replaces occurs once in the page and is at most a quarter of the page's prose and 2,000 characters (a fix corrects a claim, it does not rewrite the page); at least 80% of the word weight the fix brings in is the quote's (`FIX_SUPPORT`) |

Word weights are token statistics: `harness::query_terms` over folded text, a
word weighed by how few pages hold it (a smoothed IDF). No word list judges
the text; where the numbers cannot decide, the model does.

## The size guard

The pages that have a readable source are kept under `MAX_SIZE_RATIO` (1.0)
times the characters of those sources, each source counted once: a library
larger than what it summarizes abstracts nothing. An edit that would push the
ratio past it (and above where it stands) is refused. Pages whose sources the
host cannot read (URLs, documents elsewhere) and the index are not counted.
`/knowledge status` and `ha doctor` show the ratio: `size: N characters of P
page(s) over S of their readable sources (ratio R, kept under 1.00)`.

## What shows it

- The session gets a notice when a pass starts and one with what it did
  (`made 2 repoint, 1 merge; 0 dangling link(s) left`).
- Diagnostics: `wiki.applied` and `wiki.refused` for every edit (scope, edit,
  page, and the refusal's reason), `wiki.pass` for each pass (applied,
  refused, dangling links before and after, the size ratio); the `sleep`
  records of its job.
- The bug report's *Knowledge wiki* section counts the passes and each kind
  of edit applied and refused, with the last refusals; its *Sleep* section
  lists a wiki job that waits or gave up.

## What it leaves out

- It never edits a page that links only to pages that exist, a page of the
  user's at `topics.md`, or a page's style: only the links, the duplicates, the
  index and what a source contradicts.
- It does not fetch a URL to check a page: only workspace files and the
  project's conversations are sources it can read.
- It is not a curator of memory entries: Deep Dream is
  ([MEMORY_STORE.en.md](MEMORY_STORE.en.md), section 13).
