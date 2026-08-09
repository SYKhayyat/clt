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

## Daily use

```sh
clt                          # this branch's tasks (the command you actually run)
clt add "fix the retry loop" # file one, scoped to the current branch
clt add "triage CI" --repo   # visible from every branch
clt start 3                  # mark in progress
clt done 3                   # done — cascades to everything nested under it
clt find refresh             # search every branch
clt ls --all                 # every branch, with a branch column
```

Quoting is optional — `clt add fix the retry loop` works.

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

Exposes `clt_list`, `clt_add`, `clt_close`, `clt_start`, `clt_search`. Changes
are attributed to `agent` unless `CLT_ACTOR` says otherwise.

### Or just the CLI

Every read command speaks `--json`:

```sh
clt ls --json
clt find auth --json
CLT_ACTOR=claude clt add "found a race" --file src/net.rs:12
```

`clt log` shows who did what:

```
2026-08-09 14:37  claude   add     #7    found a race in the retry loop
2026-08-09 14:37  you      done    #2    token refresh races on 401
```

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

The task arrives as JSON on stdin and in `CLT_TASK_ID`, `CLT_TASK_TITLE`,
`CLT_TASK_STATE`, `CLT_BRANCH`, `CLT_ACTOR`, `CLT_TASK_JSON`. Exit status is
reported but ignored — these are `post-` hooks and have no veto.

Under `clt mcp`, hook stdout is redirected to stderr so it can't corrupt the
JSON-RPC stream.

## Storage

`<repo>/.clt/tasks.json`, plus `.clt/log.jsonl` and `.clt/hooks/`.

The list is **not committed**. `clt` adds `.clt/` to `.git/info/exclude` (which
is per-clone and untracked) rather than editing your `.gitignore`. So branch
switching never touches your tasks, merges never conflict on them, and your
todo list never lands in a PR diff. If you'd rather commit it, delete that line
— `clt` only adds it when it's absent.

Outside a repo, `clt` falls back to one global list under your platform data
directory. `clt path` prints wherever it landed.

The format is documented and stable, and it's meant to be read and hand-edited.
Writes are atomic. On load, `clt` repairs what a careless edit can break —
duplicate ids, parents that don't exist, and parent cycles — reporting each
repair on stderr rather than failing or hanging.

## Status

0.1. Used daily by its author; the on-disk format is versioned and will be
migrated rather than broken.

## License

MIT
