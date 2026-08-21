# Onboarding

The README's [first five minutes](../README.md#your-first-five-minutes) gets
`clt` working. This page is the rest of the first week: adopting it in a project
you already have, wiring your agent to it, the three workflows it exists for,
and the handful of decisions worth making deliberately rather than discovering.

If you are here to change the code rather than use it, go to
[CONTRIBUTING.md](../CONTRIBUTING.md) instead. It covers the module map, the
invariants, and how to add a command end to end.

---

## Contents

- [1. Install and verify](#1-install-and-verify)
- [2. Adopt it in a repo you already have](#2-adopt-it-in-a-repo-you-already-have)
- [3. The mental model](#3-the-mental-model)
- [4. Wire up your agent](#4-wire-up-your-agent)
- [5. The three workflows](#5-the-three-workflows)
- [6. Decisions worth making on purpose](#6-decisions-worth-making-on-purpose)
- [7. A week's worth of habits](#7-a-weeks-worth-of-habits)
- [8. What clt is not](#8-what-clt-is-not)

---

## 1. Install and verify

```sh
cargo install --git https://github.com/SYKhayyat/clt
clt --version
```

You need a recent Rust stable to build it — 1.88 at the oldest, since the crate
is 2024 edition and uses let-chains — and **nothing at runtime** except `git` on
your `PATH` for anything repo-scoped.

If `clt --version` says "command not found", `~/.cargo/bin` is not on your
`PATH`. See
[TROUBLESHOOTING](TROUBLESHOOTING.md#clt-command-not-found-after-cargo-install).

There is no setup step and no config file. The first task you file creates
`.clt/`.

## 2. Adopt it in a repo you already have

Pick a real project — this works much better with real work in it than with a
scratch directory.

```sh
cd ~/code/your-project
clt add "the thing you are actually about to do"
```

That one command:

- created `.clt/` in the repository root;
- added `.clt/` to `.git/info/exclude`, **not** to your `.gitignore`;
- filed the task against the branch you are currently on.

Nothing was committed, nothing you track was modified, and nobody else on the
project is affected. That is deliberate: adopting `clt` should not be a decision
you have to get your team to agree to first.

Now check where things landed:

```sh
clt path --json
```

You will see the store path, the current branch, and whether the list is shared.
Those three facts answer most confusion later, so it is worth knowing the
command now rather than when something looks wrong.

### Optional extras

```sh
clt init
```

Only adds a documented sample hook and a `.clt/README.md` explaining the
directory to whoever opens it next. Skip it until you want a hook.

## 3. The mental model

Three ideas. Everything else follows from them.

### A task belongs to a branch

Not to you, not to the repository — to the branch you filed it on. Switch
branches and your list switches with it, because the list is meant to be *the
work in front of you*, not an inbox.

Two escape hatches, both after the fact:

```sh
clt add "triage the CI queue" --repo    # visible from every branch
clt scope 3 --repo                      # promote an existing one
clt scope 3 --branch main               # or re-pin it where the work continued
```

Scope is not fixed at creation, because branches get merged and deleted and the
tasks filed on them should not go with them. `clt ls --orphaned` finds the ones
stranded on branches git no longer has.

### The code can file its own tasks

```rust
// TODO(clt): retry on 429 too
```

```sh
clt scan
```

Only `TODO(clt):` and `FIXME(clt):` are harvested. A bare `TODO` is left alone
on purpose — scraping every TODO in a real codebase produces hundreds of tasks
nobody asked for, and you would stop reading the list within a day.

Delete the comment and the next scan closes the task. Move it and the task
follows.

### Something other than you writes to the list

This is the assumption the whole design is built on. Your agent filing a task
while you are typing one is the **normal** case, not an exotic one — which is
why writers take a lock across the whole read-modify-write cycle and the store
is replaced by atomic rename.

It is also why every change is attributed, and why `clt log` exists.

## 4. Wire up your agent

### Claude Code

```sh
claude mcp add clt -- clt mcp
```

### Any other MCP client

The server is `clt mcp` over stdio, with no arguments and no configuration:

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

Set `CLT_ACTOR`. Without it everything the agent does is attributed to the
generic `agent`, and the value of attribution is being able to tell *which*
thing wrote a task six weeks later.

The server serves the list for **whatever directory the client launched it
in**, so an agent working in your repo gets your repo's tasks. That is also the
first thing to check when it appears to see the wrong list.

Tools exposed: `clt_list`, `clt_add`, `clt_edit`, `clt_close`, `clt_start`,
`clt_reopen`, `clt_search`.

`clt_edit` matters more than it looks. Without it, an agent that files a vague
task can only close it and file a replacement, which throws away the task's
history — including when it was first noticed.

### Or skip MCP entirely

Every command speaks `--json`, including the ones that write, so an agent can
add a task and read back what it created without a second call.

```sh
CLT_ACTOR=claude clt add "found a race" --file src/net.rs:12 --json
```

Two things to tell whatever consumes that output:

- **Optional fields are absent, not null.** Only `id`, `title`, `state`,
  `origin`, `created`, `updated`, `depth` and `context` are always there.
- **An empty result is not an error.** State changes return the array of tasks
  they changed; closing an already-closed task is a successful request that
  changed nothing.

### Tell your agent the conventions

Worth putting in your `CLAUDE.md` or equivalent, because an agent will not
infer them:

> File anything you notice but do not fix, with `--file path:line` when you know
> the location. Use `clt edit` to sharpen an existing task rather than closing
> and refiling it. Do not touch the `origin` field.

## 5. The three workflows

### Work in front of you

```sh
clt                          # the command you actually run
clt add "fix the retry loop"
clt start 3
clt done 3                   # cascades to everything nested under it
```

Nesting is arbitrary depth, and subtasks inherit their parent's branch so a tree
is never half-visible. Closing cascades down; **reopening does not** — reopening
a parent to add one more subtask should not undo the twelve you already
finished.

### Harvested from the source

```sh
clt scan -n                  # dry run first, always, the first few times
clt scan
```

Markers are identified by their **text**, so two byte-identical markers collapse
into one task. Write markers that say something specific.

A scan only closes tasks filed from the branch you are on, because your working
tree holds one branch's files — a marker living on `feat/x` is absent while you
are on `main`, and that is not the same as deleted.

If a marker in a string literal files a task at you, that is what `.cltignore`
is for:

```
docs/
tests/fixtures.rs
src/generated_*
```

Exact path, directory prefix, or a trailing `*`. Not gitignore semantics —
`.gitignore` is already honoured by the scanner, since enumeration goes through
`git ls-files`.

Unlike `.clt/`, **`.cltignore` is meant to be committed**: what counts as
un-harvestable is a decision the whole team shares.

### Closed by commits

```sh
git commit -m "fix the race

closes clt#3"
clt sync
```

Recognises `close(s|d)`, `fix(es|ed)`, `resolve(s|d)`. A bare `clt#3` mention
closes nothing.

Two things to know before you rely on it:

- `clt sync` reads only history it has not seen, remembering where it got to. If
  a rebase rewrote the shas it recorded, `clt sync --rescan` re-reads the most
  recent 50 commits.
- That 50-commit ceiling also applies to the **first** sync in a repository. A
  `closes clt#3` buried deeper than that in existing history is not picked up.
  Close it by hand.

Consider a `post-commit` git hook that runs `clt sync` so you never have to
remember.

## 6. Decisions worth making on purpose

### Local or shared?

Default is **local only**, and for a solo user that is almost always right:
branch switching never touches your tasks, merges never conflict on them, and
your todo list never appears in a PR diff.

```sh
clt share      # lift the exclusion; it prints the `git add` for you to run
clt unshare    # back to local-only
```

`clt share` writes a `.clt/.gitignore` so `tasks.json` and your hooks travel
while the journal — an append-only record of activity in *this* checkout —
stays local.

Share when the list is a team artefact. Do not share it because you want it on
another machine; a shared list conflicts in `tasks.json` when two people file at
once, and while that resolves like any other merge, it is a cost with no benefit
if nobody else is reading it.

Whether the list is shared is read back from git rather than stored as a setting
of ours, so `git rm --cached` is a perfectly good way to unshare and
`clt path --json` will agree with you.

### Does `.cltignore` go in?

Yes, as soon as your first false positive appears. It is a shared decision and
it belongs in the commit.

### Do you want hooks?

Git's model: an executable in `.clt/hooks/`, in any language.

```
.clt/hooks/post-add  post-done  post-start  post-reopen  post-rm
```

The task arrives as JSON on stdin and in `CLT_EVENT`, `CLT_TASK_ID`,
`CLT_TASK_TITLE`, `CLT_TASK_STATE`, `CLT_BRANCH`, `CLT_ACTOR`, `CLT_TASK_JSON`.

These are `post-` hooks with **no veto**: exit status is reported and ignored.
They run after the write is on disk and after the lock is released, so a hook
that blocks on the network does not keep anyone else out of the list.

`clt scan` and `clt sync` fire them too, so a hook sees harvested and
commit-closed tasks exactly like typed ones, with `CLT_ACTOR=scan` for the
former.

### Global list, or per-repo?

Outside a git repo — or with `--global` anywhere — you get one list under your
platform's data directory. Nothing there is branch-scoped, since there is no
branch.

| Platform | Location |
| --- | --- |
| Linux, BSD | `$XDG_DATA_HOME/clt/`, else `~/.local/share/clt/` |
| macOS | `~/.local/share/clt/` |
| Windows | `%LOCALAPPDATA%\clt\` |

Most people end up using the global list for things that are not about any one
repository, and never think about it again. `clt path` always tells you which
one you are looking at.

## 7. A week's worth of habits

**Day one.** `clt add` instead of a mental note. That is the whole adoption
step.

**Day two.** Put one `TODO(clt):` marker on something you notice and do not want
to fix now, and run `clt scan`. The point is not the task — it is that the
comment and the task are now the same object, and deleting the comment closes
it.

**Day three.** Wire the agent. Then read `clt log` at the end of the day and see
what it filed.

**Day four.** Use `--file path:line` on something. A task that knows where it
lives is worth several that describe where they live.

**Day five.** `clt ls --orphaned`. If you have merged anything this week, there
will be something in there. Decide whether to `scope --repo` it or close it.

Two more, whenever they come up:

- `clt edit` rather than close-and-refile. Sharpening a task keeps its history.
- `clt find <word>` spans every branch by default. `--here` narrows it. When you
  cannot find a task, `find` is the right first move, not `ls --all`.

## 8. What clt is not

It is not a general-purpose todo list, and Taskwarrior, todo.txt and dstask are
all better at being one. If you want a global inbox, use one of those.

It is not an issue tracker. There are no assignees, no labels, no milestones, no
comments thread. Tasks have a title, an optional note, a state, a scope, an
optional `file:line`, and an actor.

It is not a sync service. The list is a file in your repository. Getting it onto
another machine means committing it (`clt share`) or copying it.

What it does that those structurally cannot, because they are global and
repo-blind, is exactly three things: **scope tasks to a branch**, **let an agent
write to the same list you do, attributed**, and **bind tasks to the code that
caused them**.

---

## Where to go next

- [../README.md](../README.md) — the full command reference and the storage
  format.
- [TROUBLESHOOTING.md](TROUBLESHOOTING.md) — symptom-first. Worth skimming the
  headings now.
- [../CONTRIBUTING.md](../CONTRIBUTING.md) — if you are going to change the
  code: module map, invariants, testing, and what CI enforces.
