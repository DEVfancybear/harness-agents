# Project identity: which project a directory is

English | [Tiếng Việt](PROJECT_IDENTITY.vi.md)

ha keeps a project's data under a key: its store (`<data>/projects/<key>/`,
the journal of every conversation, artifacts, leases, deferred reviews and
session-log marks), the global memory entries kept for it
(`/projects/<key>/...`) and its lessons. Since 0.8.8 the key follows the Git
repository, so ha started in a subdirectory of a repository, or in one of its
linked worktrees, opens the same project as ha started at its root.

Code: `project_root` and `resolve` in
`crates/harness-cli/src/host/application/bootstrap.rs` (the key),
`crates/harness-cli/src/host/configuration/adoption.rs` (older stores) and
`crates/harness-cli/src/host/configuration/project.rs` (the project id).

## 1. The key

The key is `project-` and the first 16 hex digits of the SHA-256 of the key
root's path. The key root is found from the directory ha runs in (`--cwd`, or
the directory it was started in), read from the repository's files without
running Git:

| The directory is | Its key root |
|---|---|
| In a main checkout (the nearest `.git` is a directory) | That checkout's top level |
| In a linked worktree (a `.git` file whose git directory's `commondir` is a main checkout's `.git`) | The main checkout's top level |
| In a submodule, or a worktree of a bare clone (a delegated coder's) | Its own top level |
| Not in a repository | The directory itself |

A key root that would be the user's home or a directory above it (a home kept
in Git), a drive's root, or anything under an ha data directory is refused:
the directory itself is the key root then.

A launch from a repository's root hashes the same path it always did, so its
key, and every store and memory entry kept under it, is unchanged.

Only the key follows the repository. The directory ha runs in is still where
the tools work, where the `AGENTS.md` walk ends, where `.harness/` session logs
and trust are looked for, and what the header's `Project:` line shows. The
header's `Store:` line names the store the key chose.

## 2. One project id for the repository

Inside a store, each directory ha has run in is registered under a project id
(`project_registrations`). A directory seen for the first time takes the id of
the store's registered directory with the same key root, the root's own first,
and is registered under it. When the store refuses that because the two do not
share a Git common directory it can see (no `git` on the path, say), the
directory gets an id of its own, as every directory did before 0.8.8.

## 3. Data kept under an older key

Before 0.8.8 a launch from a subdirectory or a linked worktree kept its data
under the key of that directory. The first launch from that directory that
resolves the repository's key adopts it:

1. When `<data>/projects/<old key>/harness.sqlite3` exists and the new key has
   no store, and no ha process holds the old store's `writer.lock` and no
   background worker serves it, the directory is renamed to
   `<data>/projects/<new key>/`, under `<data>/adoption.lock`. A directory
   already at the new key without a store is set aside as
   `<new key>.before-adoption-<time>`, never deleted. A store in use is left
   for a later launch, and the launch says so.
2. The global memory entries kept for the old key are given the new key and the
   repository's name, through the memory store's own save under its lock: each
   is one event of the `migration` writer with cause `adopt`, and keeps its id.
   The lessons about the old key (and a learned skill's project) get the new
   key; the old store's worker journal is removed (it would restart agents on
   the old path) and its agent names move with it.
3. `<data>/projects/<new key>/project.json` records the key root and, under
   `adopted`, the old keys it took.

Two stores that both hold data are never merged. When the new key already has a
store, the old one is left where it is: its key is recorded under `apart` in
`project.json`, the memory and lessons still move (they are not in either
store), a `project/store_kept_apart` diagnostic is written, and every launch
from that directory shows one line naming the other store (`Note:` in the
header, `ha: note:` on stderr for `ha exec`). `ha doctor` and `/doctor` list
every store whose registered directories key to this project under another
key, under State. Its conversations stay readable with
`ha sessions list --data-dir <store>`.

Every step looks again before it acts and is a no-op once done, so it is safe
to run twice or from several launches at once. A launch from a directory whose
key did not change does nothing beyond two file checks.

## 4. What it changes

- Conversations started in a subdirectory or a linked worktree are in the
  repository's store: `/resume` and `history_search` see them from any
  directory of the repository.
- A new conversation hears only of its own checkout's earlier ones. Each
  conversation keeps the checkout it was started in (the nearest directory
  with a `.git` entry), and the previous conversation's turns and the task it
  left are taken from that checkout's: a linked worktree on another branch is
  not shown the main checkout's last task, nor the main checkout the
  worktree's. A conversation from before 0.8.8 is the key root's.
- One background worker serves the repository.
- `ha maintenance --cwd <dir>` and `ha sessions list` from a subdirectory open
  the repository's store.
- `ha learning` keys a project lesson brought in by the repository too, from
  the current directory spelled as a launch from it resolves it.

## 5. A repository that moves (0.8.9)

The key is the hash of a path, so a repository moved or renamed gets a new
key. Before 0.8.9 its first launch at the new place started an empty store,
and its memory stayed under the old key.

**Its identity is recorded.** A launch whose key root is a repository writes
its identity into `<data>/projects/<key>/project.json`, under `identity`:

- `roots`: its root commits (the commits without a parent), from one
  `git rev-list --max-parents=0 HEAD`, run once and kept: a later launch reads
  them from the manifest and starts no process;
- `origin`: the URL of its `origin` remote, read from the repository's
  `config` file on each launch (no process), trimmed of a trailing `/` and
  `.git`.

A repository without a commit records nothing until it has one. A directory
outside a repository has no identity.

**A moved store is found again.** When a launch's key has no store yet, the
stores under `<data>/projects/` whose manifest has the same root commits, at
least one, and the same remote (or none for both), and whose recorded root no
longer exists while the directory it was in still does, are this repository
before it moved:

- exactly one: it is adopted as a store under an older key is (§3): moved to
  the new key under `<data>/adoption.lock` when nothing holds it, its global
  memory and lessons given the new key and the repository's new name, the old
  key recorded under `adopted`, and the diagnostics record
  `project/store_followed`;
- several: none moves, the launch says so in a notice, and
  `project/store_follow_ambiguous` names them; `ha project rekey` moves the
  one to keep.

A fork has the same root commits under another remote, and never matches. A
second clone that still exists is never taken: its root is there. A clone out
of reach - on a drive that is not mounted, a share that is not connected - has
lost the directory it was in too, and is not taken either. Two stores that
both hold data are never merged.

What ha cannot see stays a limit: a data directory shared between machines (a
synced home) sees another machine's clone as gone when its path is not on this
one, and one fresh clone of the same repository and remote here could take its
store. Keep the data directory per machine, or move a store with
`ha project rekey` by hand.

**By hand.** `ha project rekey <old path or key> [--cwd DIR]` moves the store
kept under another key to the key of the current directory (or `--cwd`), with
the same move and retag. The old store is named by its key (`project-` and 16
hex digits) or by the directory the project was at: the key that directory
had, the key it had before 0.8.8, or the store whose manifest or registered
directories name it (separators and, on Windows, case do not matter). It is
refused when the current key has a store of its own, or something holds the
old one.
