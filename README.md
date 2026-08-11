# clt

**The task list that lives in your repo and that your coding agent can write to.**

Tasks are scoped to the git branch you filed them on. Switch branches, and your
list switches with it. Your agent files tasks into the same list you do, over
MCP or the CLI, and every change is attributed.

```
~/code/engage (feat/auth-refresh)
$ clt
  #1  ○ triage the flaky CI queue                                    2d
  #2  ○ token refresh races on 401    src/auth.rs:88     [claude]    1h
  #3  ● add auth  1/3                                                20m
  #4    ✓ hash passwords                                             18m
  #5    ○ rotate signing keys                                        20m
  4 open · 1 in progress

$ git switch main
$ clt
  #1  ○ triage the flaky CI queue                                    2d
```

## Why this instead of Taskwarrior / todo.txt / dstask

Those are all better than this at being a general-purpose todo list, and if
that's what you want you should use one of them. `clt` does three things they
structurally can't, because they're global and repo-blind:

- **Branch scoping.** A task filed on `feat/auth-refresh` isn't clutter on
  `main`. Your list is always the work in front of you.
- **Agents are first-class.** `clt mcp` serves the list to Claude Code as
  native tools. Your agent files the bug it noticed but didn't fix, and it's
  there when you sit down. Everything it does is tagged and journalled.
- **The code is the source of truth.** `clt scan` harvests `TODO(clt):`
  markers into real tasks bound to `file:line`, and closes them when the
  comment is deleted.

## Install

```sh
cargo install --path .
```

There is no setup step. The first task you file creates `.clt/` and excludes it
from git. `clt init` is optional and only adds the extras — a sample hook and a
README describing the directory — for when you want them.

## Daily use

```sh
clt                          # this branch's tasks (the command you actually run)
clt add "fix the retry loop" # file one, scoped to the current branch
clt add "triage CI" --repo   # visible from every branch
clt start 3                  # mark in progress
clt done 3                   # done — cascades to everything nested under it
clt reopen 3                 # back to todo — does not cascade
clt edit 3 --title "…"       # sharpen a task instead of refiling it
clt find refresh             # search every branch
clt ls --all                 # every branch, with a branch column
clt ls --orphaned            # tasks stranded on branches that are gone
clt scope 3 --repo           # move a task's scope after the fact
clt share                    # commit the list with the repo (see Storage)
```

Quoting is optional — `clt add fix the retry loop` works. The verbs you type
most have short aliases: `a`, `ls`, `f`, `d`, `mv`.

### What a list shows you

Done tasks disappear from the list the day after you finish them, so the
default view is what's left rather than a growing monument to what isn't.
`--done` brings the old ones back.

```sh
clt ls --done                # including everything finished before today
clt ls --state doing         # one state only (todo | doing | done)
clt ls --branch main         # another branch without switching to it
clt ls --all                 # every branch at once, with a branch column
clt find auth --here         # find spans all branches; --here narrows it
```

Filtering never leaves a subtask floating: when a match is nested, its
ancestors come along to hold it in place, dimmed, and are marked
`"context": true` in `--json`.

### Nesting

Arbitrary depth. Subtasks inherit their parent's branch, so a tree is never
half-visible.

```sh
clt add "hash passwords" --parent 3
clt move 7 --under 3         # re-nest
clt move 7 --root            # detach
clt done 3                   # closes 3 and everything beneath it
clt rm 3 -r                  # -r is required when a task has subtasks
```

Closing cascades; reopening does not. Reopening a parent to add one more
subtask shouldn't undo the twelve you already finished.

### Branch or repo-wide, after the fact

Scope isn't fixed at creation. Branches get merged and deleted, and the tasks
filed on them shouldn't go with them.

```sh
clt ls --orphaned            # tasks pinned to branches git no longer has
clt scope 3 --repo           # promote to visible-everywhere
clt scope 3 --branch main    # or re-pin to where the work continued
clt scope 3 --here           # the branch you're on now
```

Re-scoping moves the whole subtree, and re-scoping a subtask on its own is
refused — a tree split across two branches renders with holes in it from both
sides.

### Harvesting TODOs

```rust
// TODO(clt): retry on 429 too
```

```sh
clt scan          # + #4  retry on 429 too    src/http.rs:142
clt scan -n       # dry run
```

Only `TODO(clt):` and `FIXME(clt):` are harvested — a bare `TODO` is left
alone, because scraping every TODO in a real codebase produces hundreds of
tasks nobody asked for. Delete the comment and the next scan closes the task.
Move it to another file and the task follows it. File enumeration goes through
`git ls-files`, so `.gitignore` is respected exactly.

Markers are identified by their *text*, so two files carrying byte-identical
marker text collapse into one task. Write markers that say something specific.

A marker only counts when it's in a comment, so test fixtures and docs that
quote the syntax don't file tasks against you. Raw string literals defeat that
check by design, and the escape hatch is a repo-root `.cltignore`:

```
docs/            # a directory
tests/fixtures.rs
src/generated_*  # trailing * wildcard
```

Exact path, directory prefix, or a trailing `*` — not gitignore semantics.
Anything needing real globbing belongs in `.gitignore`, which the scanner
already honours. Unlike `.clt/`, this file is meant to be committed: what
counts as un-harvestable is a decision the whole team shares.

A scan only closes tasks filed from the branch you're on. Your working tree
holds one branch's files, so a marker living on `feat/x` is simply absent while
you're on `main` — that's not the same as deleted, and closing it would be
unrecoverable.

### Closing from commits

```sh
git commit -m "fix the race

closes clt#3"
clt sync
```

Recognises `close(s|d)`, `fix(es|ed)`, `resolve(s|d)`. A bare `clt#3` mention
doesn't close anything.

## Agents

### MCP (recommended)

```sh
claude mcp add clt -- clt mcp
```

Exposes `clt_list`, `clt_add`, `clt_edit`, `clt_close`, `clt_start`,
`clt_reopen` and `clt_search`. Changes are attributed to `agent` unless
`CLT_ACTOR` says otherwise.

`clt_edit` matters more than it looks: without it an agent that files a vague
task can only close it and file a replacement, which throws away its history.

### Or just the CLI

Every command speaks `--json`, including the ones that write — so an agent can
add a task and read back what it created without a second call.

```sh
clt ls --json
clt find auth --json
CLT_ACTOR=claude clt add "found a race" --file src/net.rs:12
```

A row is the stored task (see [Storage](#storage)) plus two fields that only
exist in a rendered list:

```json
{
  "id": 3, "title": "hash passwords", "state": "doing", "branch": "feat/auth",
  "parent": 2, "location": { "file": "src/auth.rs", "line": 88 },
  "created": "2026-08-09T14:37:02Z", "updated": "2026-08-09T14:37:02Z",
  "depth": 1, "context": false
}
```

`depth` is nesting depth, so a flat array still renders as a tree. `context` is
true for an ancestor pulled in only to hold a matched subtask in place.

`clt log` shows who did what:

```
2026-08-09 14:37  claude   add     #7    found a race in the retry loop
2026-08-09 14:37  you      done    #2    token refresh races on 401
```

Attribution comes from `--actor`/`-A`, or `CLT_ACTOR` in the environment, which
is the one to set for an agent. `--global` works on the global list from inside
a repo; both are accepted by every command.

## Hooks

Git's model: an executable in `.clt/hooks/`, in whatever language you like.
`clt init` writes a documented sample.

```sh
.clt/hooks/post-add
.clt/hooks/post-done
.clt/hooks/post-start
.clt/hooks/post-reopen
.clt/hooks/post-rm
```

The task arrives as JSON on stdin and in `CLT_EVENT`, `CLT_TASK_ID`,
`CLT_TASK_TITLE`, `CLT_TASK_STATE`, `CLT_BRANCH`, `CLT_ACTOR` and
`CLT_TASK_JSON`. Exit status is reported but ignored — these are `post-` hooks
and have no veto.

`clt scan` and `clt sync` fire them too, so a hook sees tasks harvested from
the source and closed by a commit exactly like ones you typed. Scanned tasks
arrive with `CLT_ACTOR=scan`.

Under `clt mcp`, hook stdout is redirected to stderr so it can't corrupt the
JSON-RPC stream.

## Storage

`<repo>/.clt/tasks.json`, plus `.clt/log.jsonl` and `.clt/hooks/`. `clt init`
writes a `.clt/README.md` describing the directory to whoever opens it next.

```json
{
  "version": 1,
  "next_id": 4,
  "tasks": [
    {
      "id": 3,
      "title": "token refresh races on 401",
      "state": "todo",
      "branch": "feat/auth",
      "parent": 1,
      "location": { "file": "src/auth.rs", "line": 88 },
      "created": "2026-08-09T14:37:02Z",
      "updated": "2026-08-09T14:37:02Z"
    }
  ]
}
```

`state` is `todo`, `doing` or `done`. An absent `branch` means repo-wide; an
absent `parent` means a root task. Ids are never reused, so deleting a task
doesn't free its number. Nesting is stored as a parent pointer rather than
nested children, which keeps every task addressable by a flat id and makes a
re-nest a one-field write.

By default the list is **local only**. `clt` adds `.clt/` to
`.git/info/exclude` (per-clone and untracked) rather than editing your
`.gitignore`. So branch switching never touches your tasks, merges never
conflict on them, and your todo list never lands in a PR diff.

The cost is that a local list doesn't travel: clone the repo somewhere else and
your tasks stay behind.

### Sharing the list

```sh
clt share      # stop excluding it, then `git add .clt` and commit
clt unshare    # back to local-only
```

`clt share` lifts the exclusion and writes a `.clt/.gitignore` so that
`tasks.json` and your hooks travel while the journal — an append-only record of
activity in *this* checkout — stays local. It stops short of touching your
index and prints the `git add` for you to run.

Shared lists do conflict in `tasks.json` when two people file tasks at once.
The format is line-oriented and meant to be resolved by hand, and duplicate ids
are repaired on load, so the worst case is a merge you resolve like any other.

Whether the list is shared is read back from git rather than stored as a
setting of ours, so `git rm --cached` is a perfectly good way to unshare and
`clt path --json` will agree with you.

Outside a repo, `clt` falls back to one global list under your platform data
directory. `clt path` prints wherever it landed, and `--global` reaches that
list from inside a repo.

### Hand-editing and concurrency

The format is meant to be read and hand-edited. On load, `clt` repairs what a
careless edit can break — duplicate ids, parents that don't exist, and parent
cycles — reporting each repair on stderr rather than failing or hanging.

Concurrent writes are safe: a writer takes a lock across the whole
read-modify-write and the file is replaced by an atomic rename, so you and your
agent editing the list at the same moment cannot lose a task or leave a
half-written store behind. A stale `.clt/lock` is safe to delete.

## Command reference

Every command takes `--json`, `--actor <who>` and `--global`.

| Command | What it does |
| --- | --- |
| `clt` / `clt ls` | This branch's tasks. `--all`, `--branch`, `--state`, `--done`, `--orphaned` |
| `clt add <title>` | File one. `--note`, `--file path:line`, `--parent <id>`, `--repo`, `--start` |
| `clt start` / `done` / `reopen <ids…>` | Change state. `done` cascades down; `reopen` doesn't |
| `clt edit <id>` | `--title`, `--note`, `--file`, `--state` — only what you pass changes |
| `clt rm <ids…>` | Delete. `-r` required when the task has subtasks |
| `clt move <id>` | `--under <id>` to re-nest, `--root` to detach |
| `clt scope <ids…>` | `--repo`, `--branch <name>` or `--here` |
| `clt find <query>` | Search titles, notes and paths across branches. `--here` to narrow |
| `clt scan` | Harvest `TODO(clt):` markers. `-n` for a dry run |
| `clt sync` | Close tasks named by new commits. `--rescan` re-reads all history, `-n` dry run |
| `clt log` | Who changed what. `-n <count>` |
| `clt path` | Where the list is stored; `--json` adds branch and shared status |
| `clt share` / `unshare` | Commit the list with the repo, or stop |
| `clt init` | Create `.clt/` with a sample hook and a README |
| `clt mcp` | Serve the list to an agent over stdio |

## Status

0.1. Used daily by its author; the on-disk format is versioned and will be
migrated rather than broken.

## License

MIT — see [LICENSE](LICENSE).
