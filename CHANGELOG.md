# Changelog

Notable changes, newest first. The format is [Keep a Changelog][kac]; the version
number is [Semantic Versioning][semver], with the usual `0.x` caveat — the number
that actually protects your data is `"version": 1` inside `tasks.json`, which is
versioned separately and changes on its own schedule.

[kac]: https://keepachangelog.com/en/1.1.0/
[semver]: https://semver.org/spec/v2.0.0.html

## [0.1.0] — 2026-11-08

First release. Everything below is new; the four defects under *Fixed* were found
and closed before it shipped, not after.

### Added

- **A task list scoped to the branch you are standing on.** Tasks live in
  `.comtai/tasks.json` and belong to the branch they were filed on, so switching
  branches switches the list. `--repo` files one that every branch sees,
  `comtai ls --all` shows them all with a branch column, `comtai ls --orphaned`
  finds the ones stranded on branches git no longer has, and `comtai scope` moves
  one afterwards.
- **Sixteen commands** — bare list, `add`, `start`, `done`, `reopen`, `edit`,
  `rm`, `move`, `scope`, `find`, `scan`, `sync`, `log`, `path`, `share`/`unshare`,
  `init`, `mcp` — with `--json`, `--actor` and `--global` on every one, and the
  aliases `a`, `ls`, `f`, `d`, `mv`, `remove`.
- **Markers that file themselves.** `comtai scan` harvests `TODO(comtai):` and
  `FIXME(comtai):` out of tracked files, and closes a task when the marker goes
  away — but only on the branch that owns it, so a marker missing because you are
  on `main` is not read as a deletion. `-n` shows what a scan would do.
- **Closes from commit messages.** `comtai sync` closes tasks named by new commits
  (`comtai#3`), tracking a watermark rather than re-reading history each time.
  `--rescan` re-reads the most recent 50 commits, `-n` is a dry run.
- **An MCP server your agent can write to.** Seven tools over stdio —
  `comtai_list`, `comtai_add`, `comtai_close`, `comtai_start`, `comtai_reopen`,
  `comtai_edit`, `comtai_search` — with every change attributed to the actor that
  made it. Writes take the lock, reads do not.
- **Hooks.** Executables in `.comtai/hooks/` named `post-add`, `post-done`,
  `post-start`, `post-reopen`, `post-rm`, receiving the task as JSON on stdin and
  as `COMTAI_*` environment variables. No veto: these run after the write is on
  disk and after the lock is released, so a slow hook cannot lock anyone out.
- **An audit log.** Every change appends to `.comtai/log.jsonl`, readable with
  `comtai log`, rotated at 1 MB rather than growing without limit.
- **Concurrency that holds.** One lock file guards every write, expiring after 60
  seconds and carrying the writer's pid, so a killed process does not wedge the
  list.
- **Storage that behaves in a repo.** The first write appends the exclude line to
  `.git/info/exclude` so `.comtai/` never shows up as untracked; `comtai share`
  commits the list with the repo when you want it in the history instead.
  Worktrees and a detached HEAD are handled rather than guessed at, and outside
  any repository there is a global list.
- **A documented JSON contract**, field by field, so an agent can depend on it.
- **Docs**: README, `docs/ONBOARDING.md` for adopting it in an existing project,
  `docs/TROUBLESHOOTING.md` for the failures that actually come up, and
  `CONTRIBUTING.md`.

### Fixed

Found before this release, recorded because each one loses something silently and
the reasoning is worth keeping:

- **Journal rotation and append were not one operation** ([#1][i1]). The size
  check, the rename and the write happened in separate steps, so two processes
  crossing the 1 MB mark at once could each rotate, and the second rename
  discarded a generation. They now run under a lock of their own.
- **`sync` advanced its mark when it could not read history** ([#2][i2]). After a
  rebase or force-push the stored mark names a commit git no longer has: nothing
  closed *and* the mark moved to HEAD, so every commit in between was never read.
  It now refuses, leaves the mark alone, and names `--rescan`.
- **`scan` read files it then discarded** ([#3][i3]). Files excluded by
  `.comtaiignore` were opened before being filtered out of the results. The
  decision now comes before the read: 1.2–2.0 s became 70–110 ms on a 40 MB
  vendored tree.
- **MCP read tools took the write lock** ([#4][i4]). A pending repair made
  `comtai_list` save, which borrowed the write lock and rewrote `tasks.json` to
  answer a read. The repair is still applied to what the read returns and reported
  on stderr; persisting it is left to the next write.

### Notes

- Shipped as `clt` until this release. Nothing was published under that name and
  no `.clt/` store was ever written, so there is nothing to migrate.
- The first `sync` in a repository reads at most 50 commits back. Older history
  needs `--rescan` or a mark that is already current.
- Task ids are never reused, but `tasks.json` is meant to be hand-editable, so it
  is validated on every read and repaired (`renumbered`, reported) rather than
  trusted.

[i1]: https://github.com/SYKhayyat/comtai/issues/1
[i2]: https://github.com/SYKhayyat/comtai/issues/2
[i3]: https://github.com/SYKhayyat/comtai/issues/3
[i4]: https://github.com/SYKhayyat/comtai/issues/4
