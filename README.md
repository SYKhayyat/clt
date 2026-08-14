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
  1 in progress · 3 open

$ git switch main
$ clt
  #1  ○ triage the flaky CI queue                                    2d
```

## Contents

- [Why this instead of Taskwarrior / todo.txt / dstask](#why-this-instead-of-taskwarrior--todotxt--dstask)
- [Requirements](#requirements)
- [Install](#install)
- [Your first five minutes](#your-first-five-minutes)
- [Daily use](#daily-use)
- [Agents](#agents)
- [Hooks](#hooks)
- [Storage](#storage)
- [Command reference](#command-reference)
- [Troubleshooting](#troubleshooting)
- [Contributing](#contributing)

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

## Requirements

- **`git` on your `PATH`**, for anything repo-scoped. `clt` shells out to it
  rather than linking a git library — see [`src/git.rs`](src/git.rs) for why.
  Without git, or outside a repo, `clt` falls back to a single
  [global list](#outside-a-repo-the-global-list) instead of failing.
- **A recent Rust stable** to build it. The crate is 2024 edition and uses
  let-chains, so 1.88 at the oldest; it is developed on 1.97 and CI builds it
  on current stable. Nothing is needed at runtime.
- Linux, macOS and Windows. CI runs the suite on Linux and Windows; macOS is
  expected to work and is not tested.

## Install

```sh
cargo install --git https://github.com/SYKhayyat/clt
```

Or from a clone, which is what you want if you plan to change anything:

```sh
git clone https://github.com/SYKhayyat/clt
cd clt
cargo install --path .
```

Both put the binary in `~/.cargo/bin`. If `clt --version` comes back "command
not found", that directory isn't on your `PATH` yet.

There is no setup step. The first task you file creates `.clt/` and excludes it
from git. `clt init` is optional and only adds the extras — a sample hook and a
README describing the directory — for when you want them.

## Your first five minutes

From inside any git repo:

```sh
$ cd ~/code/engage
$ clt add "fix the retry loop"
  ○ #1 fix the retry loop
```

That created `.clt/`, added it to `.git/info/exclude` so it never lands in a
commit, and filed the task against the branch you're on. No init, no config
file, nothing added to your `.gitignore`.

```sh
$ clt add "hash passwords" --parent 1     # nest it
$ clt start 1                             # mark the parent in progress
$ clt
  #1  ● fix the retry loop  0/1  now
  #2    ○ hash passwords         now
  1 in progress · 1 open
```

The footer counts `todo` as open and reports `doing` separately, so those two
numbers are the same task list counted in two buckets, not a total and a
subset.

Now switch branches and look again:

```sh
$ git switch main
$ clt
  Nothing on main.

  clt add "the thing"   file a task on this branch
  clt ls --all              see every branch
```

The tasks aren't gone — they belong to the other branch. `clt ls --all` shows
everything with a branch column, and `clt add "triage CI" --repo` files a task
that's visible from every branch.

Finally, let the code file its own:

```sh
$ echo '// TODO(clt): retry on 429 too' >> src/http.rs
$ clt scan
  + #3  retry on 429 too  src/http.rs:142
```

Delete that comment and the next `clt scan` closes #3.

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

Reading a row: `#id`, a state glyph (`○` todo, `●` doing, `✓` done), the title,
`done/total` when a task has subtasks, then the optional columns — branch,
`file:line`, `[actor]` — and how long ago it last changed. Columns you aren't
using don't appear at all.

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

`clt sync` reads only history it hasn't seen before, remembering where it got
to in `last_commit`. `--rescan` ignores that mark and re-reads the most recent
50 commits, which is what you want after a rebase rewrote the shas it recorded.
The same 50-commit ceiling applies to the very first sync in a repo, so a
`closes clt#3` buried deeper than that in existing history is not picked up —
close it by hand.

## Agents

### MCP (recommended)

```sh
claude mcp add clt -- clt mcp
```

For any other MCP client, the server is `clt mcp` over stdio with no arguments
and no configuration:

```json
{
  "mcpServers": {
    "clt": {
      "command": "clt",
      "args": ["mcp"],
      "env": { "CLT_ACTOR": "claude" }
    }
  }
}
```

It serves the list for whatever directory the client launched it in, so an
agent working in your repo gets your repo's tasks. Add `"args": ["mcp",
"--global"]` for the global list instead.

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
  "id": 3, "title": "hash passwords", "note": "argon2, not bcrypt",
  "state": "doing", "branch": "feat/auth", "parent": 2,
  "location": { "file": "src/auth.rs", "line": 88 },
  "actor": "claude", "origin": { "kind": "manual" },
  "created": "2026-08-09T14:37:02Z", "updated": "2026-08-09T14:37:02Z",
  "depth": 1, "context": false
}
```

`depth` is nesting depth, so a flat array still renders as a tree. `context` is
true for an ancestor pulled in only to hold a matched subtask in place.

`origin` is on every row and says how the task got here — `{"kind":"manual"}`
for one somebody filed, `{"kind":"scan","key":"…"}` for one harvested from a
marker. `actor` is who filed it. Everything optional — `note`, `parent`,
`branch`, `location`, `actor`, `closed_by` — is left out rather than sent as
null, so read the row defensively; only `id`, `title`, `state`, `origin`,
`created`, `updated`, `depth` and `context` are always there.

State changes return the array of tasks they changed, which is empty when the
call was a no-op — closing an already-closed task is a successful request that
changed nothing, not an error. Scanning and syncing return
`{"dry_run", "added", "closed", "moved"}`.

### Exit codes and where output goes

| Code | Means |
| --- | --- |
| `0` | It worked. The result, if any, is on **stdout** |
| `1` | It failed. The reason is on **stderr**, as `clt: …`. Stdout is empty |
| `2` | You typed something clap couldn't parse. Usage is on stderr |

Nothing diagnostic is ever written to stdout, so `--json` output can be piped
straight into a parser. The reverse also holds: warnings — a repaired duplicate
id, a hook that exited non-zero — go to stderr and leave the exit code at 0,
because they describe something `clt` already handled.

### Environment

| Variable | Effect |
| --- | --- |
| `CLT_ACTOR` | Who to attribute changes to. The one to set for an agent; equivalent to `--actor`/`-A` |
| `NO_COLOR` | Any value disables colour |
| `CLICOLOR_FORCE` | Keeps colour on when stdout is not a terminal |
| `XDG_DATA_HOME`, `HOME` | Where the [global list](#outside-a-repo-the-global-list) lives (Unix) |
| `LOCALAPPDATA`, `USERPROFILE` | The same, on Windows |

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

Hooks run after the write is on disk and after the lock is released, so a hook
that blocks on the network doesn't keep anyone else out of the list.

Under `clt mcp`, hook stdout is redirected to stderr so it can't corrupt the
JSON-RPC stream.

## Storage

`<repo>/.clt/tasks.json`, plus `.clt/log.jsonl` and `.clt/hooks/`. `clt init`
writes a `.clt/README.md` describing the directory to whoever opens it next.

```json
{
  "version": 1,
  "next_id": 5,
  "last_commit": "9f3c1ab4d0e2f6a8b1c3d5e7f9a0b2c4d6e8f012",
  "tasks": [
    {
      "id": 3,
      "title": "token refresh races on 401",
      "note": "only when the clock skews past the leeway",
      "state": "todo",
      "branch": "feat/auth",
      "parent": 1,
      "location": { "file": "src/auth.rs", "line": 88 },
      "actor": "claude",
      "origin": { "kind": "manual" },
      "created": "2026-08-09T14:37:02Z",
      "updated": "2026-08-09T14:37:02Z"
    },
    {
      "id": 4,
      "title": "retry on 429 too",
      "state": "done",
      "branch": "feat/auth",
      "location": { "file": "src/http.rs", "line": 142 },
      "actor": "scan",
      "origin": { "kind": "scan", "key": "d441342cf10dab0d" },
      "closed_by": "9f3c1ab4d0e2f6a8b1c3d5e7f9a0b2c4d6e8f012",
      "created": "2026-08-09T14:41:55Z",
      "updated": "2026-08-09T15:02:10Z"
    }
  ]
}
```

`state` is `todo`, `doing` or `done`. An absent `branch` means repo-wide; an
absent `parent` means a root task. Ids are never reused, so deleting a task
doesn't free its number. Nesting is stored as a parent pointer rather than
nested children, which keeps every task addressable by a flat id and makes a
re-nest a one-field write.

`note` is the long description, `actor` is who filed it, and `closed_by` is the
full sha of the commit that closed it, when `clt sync` did the closing. At the
file level, `last_commit` is how far `clt sync` has already read history.

**`origin` is the one to leave alone.** It records how a task got here.
`{"kind":"manual"}` means somebody filed it and no scan will ever touch it.
`{"kind":"scan","key":"…"}` means a `TODO(clt):` marker owns it, and the key is
a hash of that marker's text — it is how a rescan recognises the marker it
already harvested. Delete or edit the key by hand and the next `clt scan` sees
an unfamiliar marker: it files a second task for it, and closes this one as if
you'd deleted the comment.

`version` is the format version. It exists so a future change can migrate your
file rather than reject it; `clt` already imports the pre-0.1 format on sight.

`.clt/log.jsonl` is the append-only journal behind `clt log` — one JSON object
per line, rotated once it gets large, so it records history without growing
without bound.

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

### Outside a repo: the global list

Run `clt` somewhere that isn't a git repo — or with `--global` anywhere — and
you get one list under your platform's data directory:

| Platform | Location |
| --- | --- |
| Linux, BSD | `$XDG_DATA_HOME/clt/`, else `~/.local/share/clt/` |
| macOS | `~/.local/share/clt/` |
| Windows | `%LOCALAPPDATA%\clt\` |

Nothing there is branch-scoped, since there's no branch to scope to. `clt path`
prints wherever the list landed, whichever kind it is.

### Hand-editing and concurrency

The format is meant to be read and hand-edited. On load, `clt` repairs what a
careless edit can break — duplicate ids, parents that don't exist, and parent
cycles — reporting each repair on stderr rather than failing or hanging.

Concurrent writes are safe: a writer takes a lock across the whole
read-modify-write and the file is replaced by an atomic rename, so you and your
agent editing the list at the same moment cannot lose a task or leave a
half-written store behind. A stale `.clt/lock` is safe to delete.

## Command reference

Every command takes `--json`, `--actor <who>`/`-A` and `--global`.

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
| `clt sync` | Close tasks named by new commits. `--rescan` re-reads the last 50, `-n` dry run |
| `clt log` | Who changed what. `-n <count>` (default 20) |
| `clt path` | Where the list is stored; `--json` adds branch and shared status |
| `clt share` / `unshare` | Commit the list with the repo, or stop |
| `clt init` | Create `.clt/` with a sample hook and a README |
| `clt mcp` | Serve the list to an agent over MCP (stdio) |

Aliases: `a` for `add`, `ls` for `list`, `f` for `find`, `d` for `done`, `mv`
for `move`, `remove` for `rm`.

## Troubleshooting

**My tasks vanished.** Almost always a branch switch — they're scoped, and
`clt ls --all` will show them with a branch column. If the branch they were
filed on has been deleted, `clt ls --orphaned` finds them and
`clt scope <id> --repo` rescues them.

**`clt` says there's no repo here.** It asks `git rev-parse`, so anything git
doesn't consider a repository, `clt` doesn't either. Without a repo it uses the
[global list](#outside-a-repo-the-global-list) — `clt path` tells you which one
you're looking at.

**`clt scan` isn't picking up my marker.** It has to be `TODO(clt):` or
`FIXME(clt):` — a bare `TODO` is ignored on purpose — inside a comment, in a
file `git ls-files` lists, and not excluded by `.cltignore`. `clt scan -n`
shows what a scan would do without writing anything.

**`clt scan` filed a duplicate.** Two markers with byte-identical text are one
task by design. Make the text specific.

**It says the lock is held and nothing is running.** A stale `.clt/lock` from a
process that died mid-write. Deleting it is safe: the store itself is written
by atomic rename, so there is never a half-written `tasks.json` behind it.

**A hand edit broke the file.** `clt` repairs duplicate ids, missing parents and
parent cycles on load, and says on stderr what it changed. If the JSON itself is
malformed it will refuse rather than guess — the previous content is still
whatever your editor last saved.

## Contributing

Start with [CONTRIBUTING.md](CONTRIBUTING.md). It covers building and testing,
what each module owns, the invariants that must not break, and how to add a
command end to end.

```sh
cargo test                                    # 108 tests: ~15s on Linux, ~2.5 min on Windows
cargo clippy --all-targets -- -D warnings     # what CI gates on
```

That gap is not a typo. Almost all of the runtime is spawning real `git` and
`clt` processes against real repositories — which is deliberate, and which
Windows charges roughly ten times as much for. See
[CONTRIBUTING.md](CONTRIBUTING.md).

## Status

0.1, and young. Everything documented here is covered by the test suite and
driven end to end in CI on Linux and Windows, but it has not been through a
long stretch of real daily use yet — expect the rough edges that only turn up
that way. The on-disk format is versioned and will be migrated rather than
broken.

## License

MIT — see [LICENSE](LICENSE).
