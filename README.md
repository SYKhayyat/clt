# comtai

**The task list that lives in your repo and that your coding agent can write to.**

Tasks are scoped to the git branch you filed them on. Switch branches, and your
list switches with it. Your agent files tasks into the same list you do, over
MCP or the CLI, and every change is attributed.

```
~/code/engage (feat/auth-refresh)
$ comtai
  #1  ○ triage the flaky CI queue                                    2d
  #2  ○ token refresh races on 401    src/auth.rs:88     [claude]    1h
  #3  ● add auth  1/3                                                20m
  #4    ✓ hash passwords                                             18m
  #5    ○ rotate signing keys                                        20m
  1 in progress · 3 open

$ git switch main
$ comtai
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

Longer form: **[docs/ONBOARDING.md](docs/ONBOARDING.md)** for adopting `comtai` in a
project you already have and wiring an agent to it, and
**[docs/TROUBLESHOOTING.md](docs/TROUBLESHOOTING.md)** when something is not
behaving.

## Why this instead of Taskwarrior / todo.txt / dstask

Those are all better than this at being a general-purpose todo list, and if
that's what you want you should use one of them. `comtai` does three things they
structurally can't, because they're global and repo-blind:

- **Branch scoping.** A task filed on `feat/auth-refresh` isn't clutter on
  `main`. Your list is always the work in front of you.
- **Agents are first-class.** `comtai mcp` serves the list to Claude Code as
  native tools. Your agent files the bug it noticed but didn't fix, and it's
  there when you sit down. Everything it does is tagged and journalled.
- **The code is the source of truth.** `comtai scan` harvests `TODO(comtai):`
  markers into real tasks bound to `file:line`, and closes them when the
  comment is deleted.

## Requirements

- **`git` on your `PATH`**, for anything repo-scoped. `comtai` shells out to it
  rather than linking a git library — see [`src/git.rs`](src/git.rs) for why.
  Without git, or outside a repo, `comtai` falls back to a single
  [global list](#outside-a-repo-the-global-list) instead of failing.
- **A recent Rust stable** to build it. The crate is 2024 edition and uses
  let-chains, so 1.88 at the oldest; it is developed on 1.97 and CI builds it
  on current stable. Nothing is needed at runtime.
- Linux, macOS and Windows. CI runs the suite on Linux and Windows; macOS is
  expected to work and is not tested.

## Install

```sh
cargo install --git https://github.com/SYKhayyat/comtai
```

Or from a clone, which is what you want if you plan to change anything:

```sh
git clone https://github.com/SYKhayyat/comtai
cd comtai
cargo install --path .
```

Both put the binary in `~/.cargo/bin`. If `comtai --version` comes back "command
not found", that directory isn't on your `PATH` yet.

Both of those follow `main`. To pin a release instead, add `--tag v0.1.0`; what
changed between versions is in [CHANGELOG.md](CHANGELOG.md).

There is no setup step. The first task you file creates `.comtai/` and excludes it
from git. `comtai init` is optional and only adds the extras — a sample hook and a
README describing the directory — for when you want them.

## Your first five minutes

From inside any git repo:

```sh
$ cd ~/code/engage
$ comtai add "fix the retry loop"
  ○ #1 fix the retry loop
```

That created `.comtai/`, added it to `.git/info/exclude` so it never lands in a
commit, and filed the task against the branch you're on. No init, no config
file, nothing added to your `.gitignore`.

```sh
$ comtai add "hash passwords" --parent 1 # nest it
$ comtai start 1                         # mark the parent in progress
$ comtai
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
$ comtai
  Nothing on main.

  comtai add "the thing"   file a task on this branch
  comtai ls --all              see every branch
```

The tasks aren't gone — they belong to the other branch. `comtai ls --all` shows
everything with a branch column, and `comtai add "triage CI" --repo` files a task
that's visible from every branch.

Finally, let the code file its own:

```sh
$ echo '// TODO(comtai): retry on 429 too' >> src/http.rs
$ comtai scan
  + #3  retry on 429 too  src/http.rs:142
```

Delete that comment and the next `comtai scan` closes #3.

## Daily use

```sh
comtai                          # this branch's tasks (the command you actually run)
comtai add "fix the retry loop" # file one, scoped to the current branch
comtai add "triage CI" --repo   # visible from every branch
comtai start 3                  # mark in progress
comtai done 3                   # done — cascades to everything nested under it
comtai reopen 3                 # back to todo — does not cascade
comtai edit 3 --title "…"       # sharpen a task instead of refiling it
comtai find refresh             # search every branch
comtai ls --all                 # every branch, with a branch column
comtai ls --orphaned            # tasks stranded on branches that are gone
comtai scope 3 --repo           # move a task's scope after the fact
comtai share                    # commit the list with the repo (see Storage)
```

Quoting is optional — `comtai add fix the retry loop` works. The verbs you type
most have short aliases: `a`, `ls`, `f`, `d`, `mv`.

### What a list shows you

Done tasks disappear from the list the day after you finish them, so the
default view is what's left rather than a growing monument to what isn't.
`--done` brings the old ones back.

```sh
comtai ls --done        # including everything finished before today
comtai ls --state doing # one state only (todo | doing | done)
comtai ls --branch main # another branch without switching to it
comtai ls --all         # every branch at once, with a branch column
comtai find auth --here # find spans all branches; --here narrows it
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
comtai add "hash passwords" --parent 3
comtai move 7 --under 3 # re-nest
comtai move 7 --root    # detach
comtai done 3           # closes 3 and everything beneath it
comtai rm 3 -r          # -r is required when a task has subtasks
```

Closing cascades; reopening does not. Reopening a parent to add one more
subtask shouldn't undo the twelve you already finished.

### Branch or repo-wide, after the fact

Scope isn't fixed at creation. Branches get merged and deleted, and the tasks
filed on them shouldn't go with them.

```sh
comtai ls --orphaned         # tasks pinned to branches git no longer has
comtai scope 3 --repo        # promote to visible-everywhere
comtai scope 3 --branch main # or re-pin to where the work continued
comtai scope 3 --here        # the branch you're on now
```

Re-scoping moves the whole subtree, and re-scoping a subtask on its own is
refused — a tree split across two branches renders with holes in it from both
sides.

### Harvesting TODOs

```rust
// TODO(comtai): retry on 429 too
```

```sh
comtai scan    # + #4  retry on 429 too    src/http.rs:142
comtai scan -n # dry run
```

Only `TODO(comtai):` and `FIXME(comtai):` are harvested — a bare `TODO` is left
alone, because scraping every TODO in a real codebase produces hundreds of
tasks nobody asked for. Delete the comment and the next scan closes the task.
Move it to another file and the task follows it. File enumeration goes through
`git ls-files`, so `.gitignore` is respected exactly.

Markers are identified by their *text*, so two files carrying byte-identical
marker text collapse into one task. Write markers that say something specific.

A marker only counts when it's in a comment, so test fixtures and docs that
quote the syntax don't file tasks against you. Raw string literals defeat that
check by design, and the escape hatch is a repo-root `.comtaiignore`:

```
docs/            # a directory
tests/fixtures.rs
src/generated_*  # trailing * wildcard
```

Exact path, directory prefix, or a trailing `*` — not gitignore semantics.
Anything needing real globbing belongs in `.gitignore`, which the scanner
already honours. Unlike `.comtai/`, this file is meant to be committed: what
counts as un-harvestable is a decision the whole team shares.

A scan only closes tasks filed from the branch you're on. Your working tree
holds one branch's files, so a marker living on `feat/x` is simply absent while
you're on `main` — that's not the same as deleted, and closing it would be
unrecoverable.

### Closing from commits

```sh
git commit -m "fix the race

closes comtai#3"
comtai sync
```

Recognises `close(s|d)`, `fix(es|ed)`, `resolve(s|d)`. A bare `comtai#3` mention
doesn't close anything.

`comtai sync` reads only history it hasn't seen before, remembering where it got
to in `last_commit`. If that mark points at a commit this repository no longer
has — what a rebase or a force-push leaves behind — the sync stops and says so
rather than closing nothing and moving the mark forward, which would skip the
commits in between for good. `--rescan` ignores the mark and re-reads the most
recent 50 commits, which is the repair in that situation.
The same 50-commit ceiling applies to the very first sync in a repo, so a
`closes comtai#3` buried deeper than that in existing history is not picked up —
close it by hand.

## Agents

### MCP (recommended)

```sh
claude mcp add comtai -- comtai mcp
```

For any other MCP client, the server is `comtai mcp` over stdio with no arguments
and no configuration:

```json
{
  "mcpServers": {
    "comtai": {
      "command": "comtai",
      "args": ["mcp"],
      "env": { "COMTAI_ACTOR": "claude" }
    }
  }
}
```

It serves the list for whatever directory the client launched it in, so an
agent working in your repo gets your repo's tasks. Add `"args": ["mcp",
"--global"]` for the global list instead.

Exposes `comtai_list`, `comtai_add`, `comtai_edit`, `comtai_close`, `comtai_start`,
`comtai_reopen` and `comtai_search`. Changes are attributed to `agent` unless
`COMTAI_ACTOR` says otherwise.

`comtai_list` and `comtai_search` are reads and never take the write lock. If the
file on disk needs a repair — a duplicate id, a parent that is not there — the
repair is applied to what the tool answers, reported on stderr, and written by
the next call that changes something. A read must not queue behind a writer, or
fail on a checkout nobody can write to, to tidy a file up for later.

`comtai_edit` matters more than it looks: without it an agent that files a vague
task can only close it and file a replacement, which throws away its history.

### Or just the CLI

Every command speaks `--json`, including the ones that write — so an agent can
add a task and read back what it created without a second call.

```sh
comtai ls --json
comtai find auth --json
COMTAI_ACTOR=claude comtai add "found a race" --file src/net.rs:12
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
| `1` | It failed. The reason is on **stderr**, as `comtai: …`. Stdout is empty |
| `2` | You typed something clap couldn't parse. Usage is on stderr |

Nothing diagnostic is ever written to stdout, so `--json` output can be piped
straight into a parser. The reverse also holds: warnings — a repaired duplicate
id, a hook that exited non-zero — go to stderr and leave the exit code at 0,
because they describe something `comtai` already handled.

### Environment

| Variable | Effect |
| --- | --- |
| `COMTAI_ACTOR` | Who to attribute changes to. The one to set for an agent; equivalent to `--actor`/`-A` |
| `NO_COLOR` | Any value disables colour |
| `CLICOLOR_FORCE` | Keeps colour on when stdout is not a terminal |
| `XDG_DATA_HOME`, `HOME` | Where the [global list](#outside-a-repo-the-global-list) lives (Unix) |
| `LOCALAPPDATA`, `USERPROFILE` | The same, on Windows |

`comtai log` shows who did what:

```
2026-08-09 14:37  claude   add     #7    found a race in the retry loop
2026-08-09 14:37  you      done    #2    token refresh races on 401
```

Attribution comes from `--actor`/`-A`, or `COMTAI_ACTOR` in the environment, which
is the one to set for an agent. `--global` works on the global list from inside
a repo; both are accepted by every command.

## Hooks

Git's model: an executable in `.comtai/hooks/`, in whatever language you like.
`comtai init` writes a documented sample.

```sh
.comtai/hooks/post-add
.comtai/hooks/post-done
.comtai/hooks/post-start
.comtai/hooks/post-reopen
.comtai/hooks/post-rm
```

The task arrives as JSON on stdin and in `COMTAI_EVENT`, `COMTAI_TASK_ID`,
`COMTAI_TASK_TITLE`, `COMTAI_TASK_STATE`, `COMTAI_BRANCH`, `COMTAI_ACTOR` and
`COMTAI_TASK_JSON`. Exit status is reported but ignored — these are `post-` hooks
and have no veto.

`comtai scan` and `comtai sync` fire them too, so a hook sees tasks harvested from
the source and closed by a commit exactly like ones you typed. Scanned tasks
arrive with `COMTAI_ACTOR=scan`.

Hooks run after the write is on disk and after the lock is released, so a hook
that blocks on the network doesn't keep anyone else out of the list.

Under `comtai mcp`, hook stdout is redirected to stderr so it can't corrupt the
JSON-RPC stream.

## Storage

`<repo>/.comtai/tasks.json`, plus `.comtai/log.jsonl` and `.comtai/hooks/`. `comtai init`
writes a `.comtai/README.md` describing the directory to whoever opens it next.

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
full sha of the commit that closed it, when `comtai sync` did the closing. At the
file level, `last_commit` is how far `comtai sync` has already read history.

**`origin` is the one to leave alone.** It records how a task got here.
`{"kind":"manual"}` means somebody filed it and no scan will ever touch it.
`{"kind":"scan","key":"…"}` means a `TODO(comtai):` marker owns it, and the key is
a hash of that marker's text — it is how a rescan recognises the marker it
already harvested. Delete or edit the key by hand and the next `comtai scan` sees
an unfamiliar marker: it files a second task for it, and closes this one as if
you'd deleted the comment.

`version` is the format version. It exists so a future change can migrate your
file rather than reject it; `comtai` already imports the pre-0.1 format on sight.

`.comtai/log.jsonl` is the append-only journal behind `comtai log` — one JSON object
per line, rotated once it gets large, so it records history without growing
without bound.

By default the list is **local only**. `comtai` adds `.comtai/` to
`.git/info/exclude` (per-clone and untracked) rather than editing your
`.gitignore`. So branch switching never touches your tasks, merges never
conflict on them, and your todo list never lands in a PR diff.

The cost is that a local list doesn't travel: clone the repo somewhere else and
your tasks stay behind.

### Sharing the list

```sh
comtai share   # stop excluding it, then `git add .comtai` and commit
comtai unshare # back to local-only
```

`comtai share` lifts the exclusion and writes a `.comtai/.gitignore` so that
`tasks.json` and your hooks travel while the journal — an append-only record of
activity in *this* checkout — stays local. It stops short of touching your
index and prints the `git add` for you to run.

Shared lists do conflict in `tasks.json` when two people file tasks at once.
The format is line-oriented and meant to be resolved by hand, and duplicate ids
are repaired on load, so the worst case is a merge you resolve like any other.

Whether the list is shared is read back from git rather than stored as a
setting of ours, so `git rm --cached` is a perfectly good way to unshare and
`comtai path --json` will agree with you.

### Outside a repo: the global list

Run `comtai` somewhere that isn't a git repo — or with `--global` anywhere — and
you get one list under your platform's data directory:

| Platform | Location |
| --- | --- |
| Linux, BSD | `$XDG_DATA_HOME/comtai/`, else `~/.local/share/comtai/` |
| macOS | `~/.local/share/comtai/` |
| Windows | `%LOCALAPPDATA%\comtai\` |

Nothing there is branch-scoped, since there's no branch to scope to. `comtai path`
prints wherever the list landed, whichever kind it is.

### Hand-editing and concurrency

The format is meant to be read and hand-edited. On load, `comtai` repairs what a
careless edit can break — duplicate ids, parents that don't exist, and parent
cycles — reporting each repair on stderr rather than failing or hanging.

Concurrent writes are safe: a writer takes a lock across the whole
read-modify-write and the file is replaced by an atomic rename, so you and your
agent editing the list at the same moment cannot lose a task or leave a
half-written store behind. A stale `.comtai/lock` is safe to delete.

## Command reference

Every command takes `--json`, `--actor <who>`/`-A` and `--global`.

| Command | What it does |
| --- | --- |
| `comtai` / `comtai ls` | This branch's tasks. `--all`, `--branch`, `--state`, `--done`, `--orphaned` |
| `comtai add <title>` | File one. `--note`, `--file path:line`, `--parent <id>`, `--repo`, `--start` |
| `comtai start` / `done` / `reopen <ids…>` | Change state. `done` cascades down; `reopen` doesn't |
| `comtai edit <id>` | `--title`, `--note`, `--file`, `--state` — only what you pass changes |
| `comtai rm <ids…>` | Delete. `-r` required when the task has subtasks |
| `comtai move <id>` | `--under <id>` to re-nest, `--root` to detach |
| `comtai scope <ids…>` | `--repo`, `--branch <name>` or `--here` |
| `comtai find <query>` | Search titles, notes and paths across branches. `--here` to narrow |
| `comtai scan` | Harvest `TODO(comtai):` markers. `-n` for a dry run |
| `comtai sync` | Close tasks named by new commits. `--rescan` re-reads the last 50, `-n` dry run |
| `comtai log` | Who changed what. `-n <count>` (default 20) |
| `comtai path` | Where the list is stored; `--json` adds branch and shared status |
| `comtai share` / `unshare` | Commit the list with the repo, or stop |
| `comtai init` | Create `.comtai/` with a sample hook and a README |
| `comtai mcp` | Serve the list to an agent over MCP (stdio) |

Aliases: `a` for `add`, `ls` for `list`, `f` for `find`, `d` for `done`, `mv`
for `move`, `remove` for `rm`.

## Troubleshooting

The five that come up most. Everything else — MCP wiring, hooks, worktrees,
detached HEAD, rebases, locks, platform differences — is in
[docs/TROUBLESHOOTING.md](docs/TROUBLESHOOTING.md).

Two commands answer most questions before you read anything:

```sh
comtai path --json # which list, which branch, shared or not
comtai ls --all    # every task on every branch
```

**My tasks vanished.** Almost always a branch switch — they're scoped, and
`comtai ls --all` will show them with a branch column. If the branch they were
filed on has been deleted, `comtai ls --orphaned` finds them and
`comtai scope <id> --repo` rescues them.

**`comtai` says there's no repo here.** It asks `git rev-parse`, so anything git
doesn't consider a repository, `comtai` doesn't either. Without a repo it uses the
[global list](#outside-a-repo-the-global-list) — `comtai path` tells you which one
you're looking at.

**`comtai scan` isn't picking up my marker.** It has to be `TODO(comtai):` or
`FIXME(comtai):` — a bare `TODO` is ignored on purpose — inside a comment, in a
file `git ls-files` lists, and not excluded by `.comtaiignore`. `comtai scan -n`
shows what a scan would do without writing anything.

**`comtai scan` is slow.** It reads the files `git ls-files` lists, except the ones
`.comtaiignore` excludes and comtai's own `.comtai/` — those are skipped before they are
opened rather than filtered out of the results, so a large vendored tree costs
nothing. If a scan is still slow, the files it is reading are the tracked ones.

**It says the lock is held and nothing is running.** A stale `.comtai/lock` from a
process that died mid-write. Locks expire after 60 seconds, and deleting it is
safe: the store itself is written by atomic rename, so there is never a
half-written `tasks.json` behind it.

**A hand edit broke the file.** `comtai` repairs duplicate ids, missing parents and
parent cycles on load, and says on stderr what it changed. If the JSON itself is
malformed it will refuse rather than guess — the previous content is still
whatever your editor last saved.

## Contributing

Start with [CONTRIBUTING.md](CONTRIBUTING.md). It covers building and testing,
what each module owns, the invariants that must not break, and how to add a
command end to end.

```sh
cargo test                                # 108 tests: ~15s on Linux, ~2.5 min on Windows
cargo clippy --all-targets -- -D warnings # what CI gates on
```

That gap is not a typo. Almost all of the runtime is spawning real `git` and
`comtai` processes against real repositories — which is deliberate, and which
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
