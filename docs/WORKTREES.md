# Git worktrees

Nothing worktree-aware is implemented today. This doc records the verified
current behavior, the one real bug in it, and the format constraints that a
later `--worktrees` / `--rev` attempt must respect.

Verified against the tree on 2026-09-04. **Do not implement blind**, re-confirm
against current code first.

## Current behavior

### Per-worktree isolation already works, by accident

`detect_repo_root` (`src/cli/config.rs:120`) walks up looking for `.git`. In a
linked worktree `.git` is a *file*, not a directory, and `.exists()` is true for
both. So every worktree resolves to its own root and gets its own `.syntext/`.

That is the behavior we want: independent indexes, no cross-worktree staleness,
no shared lock. It is not the result of a decision, so it has no test pinning
it. Anything that changes `detect_repo_root` to require a directory would break
worktrees silently.

### Nested checkouts are indexed twice

`enumerate_files` (`src/index/walk.rs:31`) builds a `WalkBuilder` with
`hidden(false)`, `git_ignore(true)`, `follow_links(false)`. There is no
nested-checkout filter. A worktree, clone, or submodule checked out inside the
repo and not gitignored is walked in full and indexed a second time.

Not reachable in this repo today: `.claude/`, `.syntext/`, `_syntext-bench/` and
friends are gitignored. A `worktrees/` directory at the repo root would hit it.

### The freshness incoherence (this is the actual bug)

Duplicate results are the cosmetic half. The real problem is that the walk and
change detection disagree about what a nested checkout *is*.

Outer repo, one committed file, plus a nested `inner/` repo holding two files:

```
$ git status --porcelain=v1 -uall
?? inner/
```

One directory entry, even with `-uall`. Git does not descend into a directory
that holds its own `.git`.

`STATUS_ARGS` (`src/index/freshness.rs`) is the single change-detection spawn,
parsed by `src/index/porcelain.rs`. So the walk indexes N files that detection
can only ever report as 1 directory. That is exactly the failure mode `-uall`
exists to prevent, quoted in `CLAUDE.md`: notify a directory and miss every file
in it.

Consequence: the entry does resolve, and the damage is quieter than a stall.
`apply_changed_paths` notify-change()s the `inner/` directory; commit
classifies the unreadable directory as `Excluded` and anchors it for flush
(`note_commit_for_flush`), so after the next durable flush
`retain_unflushed` drops the entry and detection reports `Updated`. No
behind-forever loop, no catch-up child (that spawns only on
`BudgetExceeded`/`TooManyFiles`/`OverlayFull`). What remains: until that
flush, every fresh search process re-detects and re-excludes the entry in
memory, once per search, and the N files indexed inside `inner/` can never
be reported individually, so edits to them are invisible to incremental
updates until a full rebuild.

Skipping nested checkouts fixes the duplication and the incoherence together.
It is a correctness fix, not a de-duplication nicety.

## Proposed change: skip nested checkouts

A `WalkBuilder::filter_entry` predicate in `enumerate_files`:

- guard on `depth() > 0`, so the repo root is never filtered out by its own
  `.git`
- guard on `file_type().is_dir()`, so the cost is one `stat` per directory, not
  per file
- skip when `<dir>/.git` exists, as either a file or a directory. That one test
  covers nested clones, linked worktrees, and submodules

Symlinked directories are already skipped (`collect_symlink_entry`,
`src/index/walk.rs:157`), so the predicate only has to handle real directories.

**Surfacing it.** Add a `nested_checkouts: usize` counter to `WalkSkips`
(`src/index/walk.rs:22`, currently `too_large` only). It already flows to
`build.rs:49` and into `format_build_summary` (`src/index/helpers.rs:167`). That
summary is emitted through `log::debug!`, which means it prints under `st index`
(`src/cli/logger.rs:16`: `st index` without `--quiet` runs at `Debug`) and stays
silent at `Warn` on the internal rebuild path a search triggers. That is the
right split: tell the person who asked for an index, stay quiet inside a search.

**Escape hatch.** `--index-nested`, off by default, for people who do want
submodule contents in one index and accept the stale-detection tradeoff.

**Cost.** One `stat` per directory, on the build walk only. No new subprocess,
so no effect on search latency.

## Identity without a subprocess

Worktree name and branch are readable from disk:

- a linked worktree's `.git` file contains `gitdir: <repo>/.git/worktrees/<name>`
- `<gitdir>/HEAD` gives the branch

No `git` spawn is needed for either. This constraint is load-bearing: two tests
in `tests/integration/cli.rs` count git shim invocations and expect exactly 1 per
detection, so any new `git` call on the search path breaks them.

Surface this in `st status` only. Do **not** add it to search `--json`: that
output is rg-compatible and diffed by `oracle_cli`.

## Out of scope, with forward constraints

Neither of the following is being built. They are recorded so a later attempt
does not pick an output format that breaks agent callers.

### Searching other checked-out worktrees (`--worktrees`)

Structural blocker: `render_results` (`src/cli/search/output.rs:22`) takes
`files: HashMap<PathBuf, MatchedFile>`, and `get_file_size`
(`src/cli/render/json.rs:27`) re-looks-up by relative path in the snapshot. Two
worktrees with the same relative path collide in both. The cheapest correct
shape is per-worktree passes, not a merged result set.

Format constraint: emit a real, openable path (`../wt-foo/src/foo.rs`), never a
`[wt-foo] src/foo.rs` label. An agent reading a label will try to open
`src/foo.rs` in its own tree and silently edit the wrong file. Keep the worktree
name as a separate JSON field, not as a prefix on the path.

### Searching a ref that is not checked out (`--rev`)

This fights the architecture. `resolve_doc` (`src/search/resolver.rs:18`)
verifies every match by reading live disk bytes through
`open_beneath(root_fd, canonical_root, path)` (`:51`). While another branch is
checked out there is no file on disk for `main`'s version, so an indexed
non-checked-out ref can never verify and would return zero matches.

Making it work needs a content source that is not the working tree (`git
cat-file`, or storing content in the segment), which is a much larger change
than the query-side plumbing suggests.

Format constraint: render `main:src/foo.rs`, matching `git grep <rev>`,
precisely *because* that is not an openable path. The colon is the signal.

## Prior art

The split is clean between filesystem tools and index-backed engines.

- **rg, grep, IDE search**: no repo model at all. A worktree is a directory and
  duplicates are the caller's problem.
- **[Zoekt](https://github.com/sourcegraph/zoekt/blob/main/doc/design.md)**: a
  64-bit branch mask per document, deduped on (path, content), so a file
  unchanged across 10 branches is one document plus 10 bits. `BranchQuery` is a
  query atom that pre-filters before any trigram work.
- **[Sourcegraph](https://sourcegraph.com/docs/admin/search)**: indexes the
  default branch plus an explicit per-repo opt-in list (`search.index.branches`,
  64-branch cap, see
  [multi-branch indexing](https://docs.sourcebot.dev/docs/features/search/multi-branch-indexing)).
  Queries name the rev inline (`repo:X@develop`), and
  [unindexed revs still search](https://sourcegraph.com/docs/code-search/features),
  just slower.

Branch as a document field is the design that scales. It is also a much bigger
change than anything above, and it presumes a content source independent of the
working tree, so it belongs behind the `--rev` blocker, not before it.
