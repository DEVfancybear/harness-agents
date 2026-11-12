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
| `fix` a claim | the source is one of the page's sources and readable; the quote is in it word for word (whitespace and case aside, at least 8 characters); it has a replacement; the text it replaces occurs once in the page and is one sentence of at most 300 characters, or at most a quarter of the page's prose, and never over 2,000 characters (a fix corrects a claim, it does not rewrite the page; a page of one claim is fixed by replacing it); at least 80% of the word weight the fix brings in is the quote's (`FIX_SUPPORT`; a footnote marker is not a word); it may carry a `note`, a footnote definition line the host adds to the page, checked with the page's other footnotes ([below](#statements-with-evidence-somem)) |

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

## Statements with evidence (SoMem)

What ha took from one paper read on 2026-11-13, how it works, and what is not
taken.

- **arXiv 2611.11929**, *Can LLM Agents Organize Their Own Memory?* (Fu et al.),
  called SoMem here. It keeps memory as three layers. **Source** is the
  immutable record of the interactions. **Topic** is Markdown documents the
  agent writes and revises freely; a paragraph is the unit, has a stable
  identifier other documents can link to, and each statement carries a
  footnote with its **event time** (year, month or day, as precise as the
  source) and the **references** to the Source records that support it.
  **Core** is a small bounded document always in context. The runtime, not the
  model, validates every edit (the Source untouched, the structure, every
  reference and link resolving) and a rejected edit leaves memory as it was.
  After an accepted edit it derives three views without the model writing
  them: **Graph** (links and backlinks), **Timeline** (statements grouped by
  event time, each pointing to its paragraph and its Source) and **Recent**
  (the K newest units, in the initial context). The agent reads by searching
  (with path and date filters), opening documents and following links, and
  opens the Source behind a statement.
- Its ablation on LoCoMo: taking away Topic or Source hurts, Source most;
  taking away Core or any one derived view is within noise. So what matters is
  evidence that resolves. The views are ways to reach it.

ha already had the layers: the journal is the Source and is never edited
(`history_search`, `history_read`), a knowledge page is a Topic, the host
derives the graph on every scan and stores nothing for it, and each page
keeps its history. A page had no way to say which source a statement rests on:
its `sources` are page-level strings that nothing looked up when the agent
wrote them. There was no event time (only when the page was written), no
address for a paragraph (a link named a page and the fragment was dropped),
and no way to search by path or by time.

### The format

A page stays ordinary Markdown. Two things are added, both optional.

```text
Joined A in May.[^e1] Moved to B in August.[^e2] ^work-2026

[^e1]: 2026-05; src/hr.rs:12-14, event_01a0f16c-740a-7185-8a4a5bf83b00
[^e2]: 2026-08; https://example.com/news
```

- **A footnote.** A marker `[^id]` follows the statement. One line defines it:
  `[^id]: <event time>; <reference>, <reference>`. The id is 1 to 32 letters,
  digits, `_` or `-`. The event time is optional and is `YYYY`, `YYYY-MM` or
  `YYYY-MM-DD`, kept as written; a definition that is only a time is valid.
  References are separated by a comma and a space. A footnote may have no
  reference, and a statement may have no footnote.
- **A reference** is one of:
  - a workspace file: `path`, `path:line` or `path:first-last`, looked up in
    the workspace the agent runs in and then in the project's main checkout;
  - a source of this project's journal, as `history_search` returns it
    (`event_<uuid>`) and `history_read` takes it;
  - a conversation of this project (`task_<uuid>`);
  - an `http://` or `https://` URL. It is checked for its syntax only: offline
    it cannot be resolved, and a resolution that may be wrong is not invented.
- **An anchor** addresses a paragraph, a heading or a list item: its last line
  ends with a space and `^anchor` (3 to 32 letters, digits or `-`), unique in
  the page. It is the block id of Obsidian and the paper's own syntax. It is
  plain text, so no renderer or sanitizer can drop it, and the link scan leaves
  it alone. A link names it as `page.md#^anchor`; a fragment without the `^`
  is an ordinary heading link and is not checked.

The grammar is parsed, not judged: no word list decides what a statement is.
A statement is the sentence the marker follows, within its paragraph, list
item or heading: from the last `.`, `!` or `?` (or the CJK forms) followed by
a space, or from the previous marker, to the marker, at most 300 characters.
Code, comments and code spans are not read, as for links.

The global library, shared by every project, reads no path relative to one
workspace. In it a reference is a URL, a history source or conversation of the
project the agent is in, or an absolute path into that workspace. A history
source cited from another project cannot be opened there; `evidence` says so
for that reference and does not fail.

### What the host checks when a page is written

On every write by the agent, the review or the wiki pass:

| Check | Refused when |
|---|---|
| Pairing | a marker has no definition, a footnote is defined twice, or a definition is used by no marker |
| Time | the event time is not `YYYY`, `YYYY-MM` or `YYYY-MM-DD`, or is not a date that exists |
| References | a file does not exist, is a protected path (`.env`, `.git`, a key, a name with `secret` in it) or one Git ignores; a cited line is past the end of the file; a history source or conversation is not in this project's journal, or is no longer available; a URL has no host |
| Anchors | two paragraphs of a page have the same anchor |
| Links to a paragraph | a body link `page.md#^anchor` or a `related` entry names a page that has no such anchor (a link to a page that does not exist stays a dangling link) |

A refused write changes nothing, and the message names the footnote, the
reference and what would resolve: `[^e1]: reference src/hr.rs:99: src/hr.rs
has 5 line(s): lines 99-99 are out of range; cite lines within 1-5`. The secret
check covers the footnotes, since they are part of the content.

Only what the write brings in is checked. A footnote definition the page
already held is not looked up again, because the file it cites may have
changed since and an unrelated edit must not be refused for it. A fault the
page already had (an old footnote of free text, a link to a paragraph that was
rewritten) is not raised again. A page with no footnotes is checked by
nothing here and is written byte for byte as before; no page is migrated.
A `related` entry may name `page.md#^anchor` and is checked, but `related`
stays a list of pages: the paragraph is cited by a body link, which keeps its
fragment.

The wiki pass writes through the same check. A `fix` may carry a `note`, a
footnote definition line that the host adds to the page below the claim it
corrects (the model ends `new` with the marker); a `create` or a `merge` holds
its footnotes in its content. The words of a marker or a definition are not
counted as words the edit brings in. A store the host did not equip with the
workspace and the journal (a test, a tool that writes no footnotes) reads only
the syntax of a reference.

### Reading it

`knowledge` has three more actions, all read-only, so a child may use them.

- `evidence` with `path` and `note` (the id): for each reference of that
  footnote, what it holds now. A file gives the cited lines marked `>` with two
  lines around them (the first 40 lines when no line is cited); a journal
  source gives its text, cut at 4,000 characters, and `history_read` reads the
  rest; a URL is returned as it is. A reference that no longer resolves says
  why for that reference only.
- `timeline`: the dated statements of the library, the oldest first, each with
  its event time as written, its page and line, the anchor of its paragraph,
  the footnote and its references. A year sorts before its months and a month
  before its days. `path` is a folder prefix; `since` and `until` are the
  window `[since, until)` that `history_search` and memory search take, and a
  statement is in it when its span overlaps (a statement dated to 2026 is in a
  window inside 2026). `limit` and `offset` page through it.
- `recent`: the newest pages (`limit`, default 8) with title, summary, when and
  by whom. It is page-level. A paragraph-level answer would compare each page
  version with the one before it, and the page history keeps whole versions for
  30 days (and each page's ten newest), so it could be derived only for pages
  changed lately, and the rest would be a guess.

`search` takes the same `path`, `since` and `until`. A page matches a window by
a statement it holds whose event time is in it; a page with no dated statement
matches by its `updated_at`. Each hit says which: `time_match` is `event_time`
or `updated_at`. A page whose dated statements are all outside the window is
out, though it was written inside it.

A read of a page lists the `anchors` it defines and `cited_by`: each page that
links to one of its paragraphs, the anchor, and the start of the paragraph. A
graph edge carries the `anchors` its link cites. A link whose paragraph is
gone (the target was rewritten later) is not refused by a scan: the page lists
it as `dangling_anchors`, `/knowledge status` counts them beside the dangling
links, and the next write that brings it in is refused.

Nothing is stored for these views. A timeline written into the library as
files would be scanned as pages and kept in step by hand.

The catalog that goes into each turn adds a short part, `recently written (not
in the list above)`: the newest three pages by `updated_at` that the ranked
list did not show. Room for them is kept inside the block's size bound (3,000
characters per library), not added to it. SoMem puts Recent in the initial
context; its ablation found the effect small, so it is three lines.

`/knowledge timeline [FOLDER]` and `/knowledge recent [COUNT]` call the same
actions.

### What ha took, and what it did not

| SoMem | ha | Why |
|---|---|---|
| Source: immutable records | The journal, read by `history_search` and `history_read`. Unchanged | Already there, and never edited |
| Topic: Markdown the agent writes | Knowledge pages, unchanged. The format adds footnotes and anchors to the body | A page must stay a page: older libraries, the console and every renderer keep working |
| A paragraph is the unit, with a stable id | An anchor, ` ^id` at the end of a paragraph, heading or list item | The paper's own syntax. Plain text, so no sanitizer drops it |
| A footnote with event time and Source references | `[^id]: <time>; <ref>, <ref>`, optional on every statement | The paper found the references matter. A page that needs none stays valid |
| The runtime validates every edit | The store checks pairing, time, references, anchors and links on every write, including the wiki pass and the review. A refused write changes nothing | One check in the store is the same check for every writer |
| Graph: links and backlinks | Derived on each scan, as before. A backlink says which paragraph it cites; an edge carries its anchors | Page-level behaviour does not change |
| Timeline: statements by event time, as files | `timeline`, derived per call and stored nowhere | Files in the library would be scanned as pages |
| Recent: the K newest units | `recent` (page-level, K=8) and three lines in the turn's catalog | Small effect in the paper; small here |
| Core: a bounded document always in context | Not added | `constraints` and pinned entries are already in every turn ([MEMORY_STORE.en.md](MEMORY_STORE.en.md)) |
| Read: search with path and date filters, read, follow links, open the Source | `search` with `path`, `since`, `until`; `read` with `cited_by`; `evidence` | The same tools, as actions of the one `knowledge` tool |

**Not taken.**

- **Dense (embedding) search.** The paper lets the agent pick BM25 or dense
  search per query. ha removed semantic search in 0.8.8 on purpose: nothing
  leaves the machine, no model is downloaded, and a miss is answered by the
  model searching again with other words.
- **The timeline as files**, `timeline/YYYY/MM/DD.md`. They would be scanned
  as pages, count against the size guard, and have to be kept in step with the
  pages they come from.
- **A separate Core document.** The ablation found it within noise, and
  `constraints` and pinned entries already do its job.
- **A final pass that reorganizes the whole collection.** The wiki pass already
  tends the library in the background and refuses what it cannot check; a pass
  that rewrote every page would meet the size guard and the review of what it
  changed at once.
- **A footnote on every statement.** A page of distilled prose with no event
  time and no file behind it would be forced to invent a reference. A
  statement with no footnote is valid; one with a footnote that does not
  resolve is not.
- **Resolving a URL.** A web address is syntax only. Offline the host cannot
  tell, and fetching it from a write would send what the page says to a server.
- **Resolving a line against a past version of a file.** A reference is looked
  up in the file as it is. A page whose cited file changed later keeps its
  footnote; `evidence` says it no longer resolves, and the wiki pass is what
  checks a claim against a source that changed.
- **Event time as a real timestamp.** The paper keeps year, month or day as
  written. So does ha, and a coarse time overlaps any window that touches it.
- **A `related` entry that keeps its fragment.** `related` is a list of pages in
  the page's metadata and in the code that moves and merges pages. The
  paragraph is cited in the body.

### Tests

- Format, `notes.rs`: `a_time_keeps_the_precision_it_was_written_at_and_a_year_sorts_before_its_months`,
  `a_time_overlaps_a_window_by_its_whole_span`,
  `a_marker_takes_the_sentence_it_follows_and_its_definition_gives_time_and_references`,
  `markers_in_code_are_not_read_and_a_page_without_notes_has_none`,
  `faults_name_an_undefined_marker_an_unused_definition_a_bad_time_and_a_repeated_anchor`,
  `only_what_a_write_brings_in_is_checked`.
- Links, `links.rs`:
  `a_footnote_definition_is_not_a_link_and_a_fragment_that_names_a_paragraph_is_a_citation`.
- Store and views, `views.rs`:
  `a_footnote_that_cites_a_missing_file_a_line_out_of_range_or_an_unknown_source_is_refused_and_nothing_is_written`,
  `a_valid_footnote_is_written_and_evidence_returns_the_cited_lines_and_the_journal_source`,
  `a_page_without_footnotes_round_trips_byte_for_byte_and_old_free_text_footnotes_do_not_block_an_edit`,
  `a_link_or_related_entry_to_a_missing_anchor_is_refused_and_a_scan_reports_one_that_went_missing`,
  `a_backlink_names_the_paragraph_it_cites_and_the_graph_edge_carries_the_anchor`,
  `the_timeline_orders_statements_by_event_time_across_precisions_and_filters_by_window_and_path`,
  `a_search_keeps_pages_under_a_path_and_in_a_window_and_says_which_time_matched`,
  `the_catalog_adds_the_newest_pages_the_ranking_did_not_show_within_its_bound`,
  `recent_lists_the_newest_pages_first_and_names_who_wrote_the_one_the_store_wrote`,
  `a_link_to_a_paragraph_is_checked_against_the_page_it_names`; and, changed,
  `the_catalog_is_ranked_by_the_goal_and_says_what_it_leaves_out` in `store.rs`.
- The tool, `tools.rs`:
  `the_knowledge_tool_checks_a_footnote_and_a_read_only_agent_opens_evidence_timeline_and_recent`,
  `a_footnote_holding_a_secret_is_refused_before_it_is_looked_up`.
- The wiki pass, `wiki.rs`:
  `a_fix_may_cite_the_place_of_its_quote_and_a_note_that_does_not_resolve_changes_nothing`,
  `a_created_page_may_carry_footnotes_that_are_not_counted_as_new_words`.
- The mapping request, `lifecycle.rs`:
  `the_bootstrap_request_asks_for_sources_scope_certainty_and_boundaries`.
