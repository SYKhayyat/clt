# Troubleshooting

Symptom first. Find the line that sounds like what you are seeing.

Two commands answer most questions before you read any further:

```sh
clt path --json      # which list am I looking at, which branch, is it shared
clt ls --all         # every task on every branch, with a branch column
```

Between them they resolve the majority of "my tasks are gone" reports, which
are the majority of all reports.

---

## Contents

- [Installing and running](#installing-and-running)
- [Missing tasks](#missing-tasks)
- [Git situations](#git-situations)
- [Scanning TODO markers](#scanning-todo-markers)
- [Closing from commits](#closing-from-commits)
- [Agents and MCP](#agents-and-mcp)
- [Hooks](#hooks)
- [Storage, locks and corruption](#storage-locks-and-corruption)
- [Sharing](#sharing)
- [Output, colour and pipes](#output-colour-and-pipes)
- [Platform notes](#platform-notes)
- [Building from source](#building-from-source)

---

## Installing and running

### `clt: command not found` after `cargo install`

The binary went to `~/.cargo/bin` (`%USERPROFILE%\.cargo\bin` on Windows) and
that directory is not on your `PATH`.

```sh
ls ~/.cargo/bin/clt          # confirm it is actually there
export PATH="$HOME/.cargo/bin:$PATH"
```

Make it permanent in your shell profile. On Windows, the `rustup` installer
normally does this; a shell opened before the install will not have picked it
up, so try a fresh terminal first.

### `cargo install` fails on the edition or on let-chains

```
error: edition 2024 is unstable
error[E0658]: let chains are unstable
```

Your toolchain is too old. The crate is 2024 edition and uses let-chains, so
**1.88 at the oldest**; it is developed on 1.97 and CI builds it on current
stable.

```sh
rustup update stable
rustc --version
```

### It runs but every command is slow

`clt` shells out to `git` for anything repo-scoped. If `git` itself is slow in
that repository — a very large working tree, a network-backed filesystem, a
slow credential helper on `git status`-adjacent calls — `clt` inherits it. Time
`git rev-parse --show-toplevel` in the same directory to confirm where the cost
is.

---

## Missing tasks

### "My tasks vanished"

Almost always a branch switch. Tasks are scoped to the branch they were filed
on, and that is the entire point of the tool.

```sh
clt ls --all         # every branch, with a branch column
clt find <word>      # search spans all branches by default
```

### The branch they were on is deleted

They are orphaned, not lost.

```sh
clt ls --orphaned            # tasks pinned to branches git no longer has
clt scope 3 --repo           # rescue: make it visible everywhere
clt scope 3 --branch main    # or re-pin it to where the work continued
```

Note that "the branch no longer exists" is judged against **local heads only**.
A branch that exists on the remote but not in your clone counts as gone.

### `clt ls` is empty but `clt ls --done` is not

Done tasks drop out of the default view the day after you finish them, so the
list stays a picture of what is left rather than a monument to what is not.

```sh
clt ls --done
```

### I am in the right repo and the right branch and it is still empty

Check which list you are actually reading:

```sh
clt path --json
```

If it reports the global list, `clt` did not find a git repository here — see
[Git situations](#git-situations). If it reports a path you do not recognise,
you may be inside a linked worktree or a submodule, which have their own roots.

### A subtask is showing but its parent is greyed out

Working as intended. When a filter matches a nested task, its ancestors are
pulled in dimmed to hold it in place, and marked `"context": true` in `--json`.
A tree rendered with holes in it is worse than a tree with dim rows.

---

## Git situations

### `clt` says there is no repo here

`clt` asks `git rev-parse`. Anything git does not consider a repository, `clt`
does not either. It then falls back to the global list rather than failing.

```sh
git rev-parse --show-toplevel     # if this fails, so will clt
clt path                          # tells you which list you got
```

Note that `git` missing from `PATH` entirely produces the same fallback, on
purpose. `clt` does not link a git library; if `git` cannot be run at all, it
degrades to the global list instead of refusing to work.

### I am on a detached HEAD and my tasks are behaving strangely

On a detached HEAD there is no branch, so `clt` has nothing to scope to. A task
filed there is stored with **no branch**, which means repo-wide — visible from
every branch.

This is not a bug, but it is easy to do by accident during a rebase or a
bisect. If you filed tasks while detached and wanted them branch-scoped:

```sh
clt ls --all                 # they will show with no branch
clt scope <id> --here        # after checking out the branch you meant
```

`clt` deliberately uses `git symbolic-ref` rather than `rev-parse --abbrev-ref
HEAD`, because the latter returns the literal string `HEAD` when detached — and
git allows a branch genuinely named `HEAD`.

### A freshly `git init`ed repo with no commits

Works. An unborn HEAD is handled specifically: the repository lookup and the
branch lookup are separate git calls precisely so that a repo with no commits
does not look like no repo at all.

`clt sync` will find no history to read, which is correct.

### Linked worktrees (`git worktree add`)

Supported, and the behaviour is deliberate:

- The **task list** lives in the worktree you are in, not the primary one. Tasks
  follow the checkout.
- The **`.clt/` exclusion** is written to the *common* git dir, so it is shared
  by every worktree rather than needing to be re-established in each.

If you expected worktrees to share one list, they do not. Use `clt add --repo`
or `clt share` depending on which kind of sharing you meant.

### Submodules

A submodule is its own repository as far as `git rev-parse` is concerned, so it
gets its own `.clt/`. Running `clt` from inside a submodule shows the
submodule's tasks, not the superproject's.

### Running from a subdirectory

Fine. Git reports `--git-common-dir` relative to the current directory, and
`clt` resolves it against the current directory rather than the worktree root,
because those differ whenever you are in a subdirectory. If you see a path that
looks wrong in `clt path`, that is where to look.

---

## Scanning TODO markers

### `clt scan` is not picking up my marker

All five of these must be true. Check them in order:

1. **The marker is `TODO(clt):` or `FIXME(clt):`.** A bare `TODO` is ignored on
   purpose — harvesting every TODO in a real codebase produces hundreds of
   tasks nobody asked for.
2. **It is inside a comment.** A marker in a string literal is not harvested, so
   documentation and test fixtures that quote the syntax do not file tasks
   against you.
3. **The file is listed by `git ls-files`.** Enumeration goes through git, so
   `.gitignore` is respected exactly — and an untracked new file is invisible
   until you `git add` it.
4. **The path is not in `.cltignore`.** Exact path, `dir/` prefix, or a trailing
   `*` wildcard.
5. **You are on the branch whose working tree contains it.**

```sh
clt scan -n          # dry run: shows what a scan would do, writes nothing
git ls-files | grep <yourfile>
cat .cltignore
```

### `clt scan` filed a duplicate

Markers are identified by their **text**. Two markers with byte-identical text
collapse into one task, by design. Write markers that say something specific
enough to be distinguishable.

### A raw string literal filed a task at me

Known limitation and the reason `.cltignore` exists. Quote-counting cannot see
inside a raw string literal, so a marker written in one looks like a comment.
Add the file to `.cltignore`:

```
src/generated_*
tests/fixtures.rs
docs/
```

Exact path, directory prefix, or a trailing `*`. **Not** gitignore semantics —
anything needing real globbing belongs in `.gitignore`, which the scanner
already honours.

This repository is its own pathological case: `README.md` and `src/scan.rs` are
both in its `.cltignore`.

Unlike `.clt/`, `.cltignore` is meant to be committed. What counts as
un-harvestable is a decision the whole team shares.

### `clt scan` did not close a task after I deleted the comment

A scan only closes tasks **filed from the branch you are on**. Your working
tree holds one branch's files, so a marker living on `feat/x` is simply absent
while you are on `main` — which is not the same as deleted. Closing it would be
unrecoverable, so it is not done.

Switch to the branch the task was filed on and scan there.

### A scan closed a task and refiled it as a new one

Something changed the `origin.key` of the stored task, or the marker text
changed. The key is a hash of the marker's text and it is how a rescan
recognises a marker it already harvested. Edit the marker's wording, and the
next scan sees an unfamiliar marker: it files a second task and closes the
first, exactly as if you had deleted the comment.

Do not hand-edit `origin` in `tasks.json`. It is the one field to leave alone.

---

## Closing from commits

### `clt sync` did not close the task my commit named

Check the wording. Recognised verbs are `close`/`closes`/`closed`,
`fix`/`fixes`/`fixed`, `resolve`/`resolves`/`resolved`, followed by `clt#<id>`.
A bare `clt#3` mention closes nothing — it is a reference, not an instruction.

```
fix the race

closes clt#3
```

### `clt sync` says there is nothing new

It reads only history it has not seen before, remembering how far it got in
`last_commit` in `tasks.json`. If you already synced past that commit, there is
nothing to do.

```sh
clt sync -n          # dry run
clt sync --rescan    # ignore the mark and re-read the most recent 50 commits
```

### After a rebase, sync stopped working

A rebase rewrites shas, so the `last_commit` mark points at a commit that no
longer exists on the branch. `clt sync --rescan` ignores the mark and re-reads
the last 50 commits. That is what `--rescan` is for.

### An old `closes clt#3` deep in history was never picked up

The 50-commit ceiling applies to the very first sync in a repository too. A
closing directive buried deeper than that in existing history is not picked up.
Close it by hand: `clt done 3`.

---

## Agents and MCP

### Claude Code does not see the `clt` tools

```sh
claude mcp add clt -- clt mcp
claude mcp list                 # confirm it is registered
```

Then check the basics:

- **`clt` is on the `PATH` the client uses.** A GUI-launched client often has a
  different `PATH` than your shell. Use an absolute path in the config if in
  doubt.
- **`clt mcp` takes no arguments and no configuration.** If you added flags,
  remove them, apart from `--global`.
- **The server serves the directory the client launched it in.** An agent
  working in your repo gets your repo's tasks; an agent launched from your home
  directory gets the global list.

### The agent sees the wrong list

Same cause as above: the working directory the client launched the server in.
Ask the agent to call `clt_list` and compare against `clt path --json` run by
hand in the repository.

Add `"args": ["mcp", "--global"]` if you deliberately want the global list.

### Everything the agent files is attributed to `agent`

That is the default. Set `CLT_ACTOR` in the MCP server's `env` block:

```json
{ "mcpServers": { "clt": {
    "command": "clt", "args": ["mcp"],
    "env": { "CLT_ACTOR": "claude" } } } }
```

`clt log` shows the attribution. On the CLI, `--actor`/`-A` does the same
per-invocation.

### The JSON-RPC stream is corrupted / the client reports a parse error

Under `clt mcp`, hook stdout is redirected to stderr specifically so a hook
cannot corrupt the stream. If you are seeing corruption anyway, something else
is writing to stdout — check for a wrapper script, a shell profile that prints
a banner, or a `command` that is not the `clt` binary itself.

### The agent closed a task and reported success but nothing changed

Correct behaviour. Closing an already-closed task is a successful request that
changed nothing, not an error. State-changing calls return **the array of tasks
they changed**, which is empty in that case.

### My `--json` parser is throwing on missing fields

Read rows defensively. Everything optional — `note`, `parent`, `branch`,
`location`, `actor`, `closed_by` — is **left out rather than sent as null**.

Only these are always present: `id`, `title`, `state`, `origin`, `created`,
`updated`, `depth`, `context`.

---

## Hooks

### My hook is not firing

1. It must be in `.clt/hooks/` with the exact name: `post-add`, `post-done`,
   `post-start`, `post-reopen`, `post-rm`.
2. It must be **executable**. On Unix, `chmod +x`. On Windows, it needs to be
   something the OS can execute directly.
3. `clt init` writes a documented sample — compare against it.

### The hook runs but I cannot see its output

Under `clt mcp`, hook stdout is redirected to stderr. From the CLI it goes where
you would expect.

### The hook failed and `clt` reported success anyway

By design. These are `post-` hooks and have **no veto**: exit status is
reported but ignored, and a non-zero exit is a warning on stderr that leaves
the exit code at 0. The write already happened.

### A slow hook is blocking other writers

It should not be. Hooks run **after** the write is on disk and **after** the
lock is released, precisely so a hook that blocks on the network does not keep
anyone else out of the list.

If you are seeing lock contention that correlates with a hook, check whether
your hook is itself calling `clt`.

### Do hooks fire for scanned and synced tasks?

Yes. `clt scan` and `clt sync` fire them too, so a hook sees tasks harvested
from source and closed by a commit exactly like ones you typed. Scanned tasks
arrive with `CLT_ACTOR=scan`.

---

## Storage, locks and corruption

### "The lock is held" and nothing is running

A stale `.clt/lock`, left behind by a process killed mid-write. The lock is
implemented with an atomic create-if-absent rather than `flock`, so a dead
process does not release it.

Locks are expired by age after **60 seconds**, so waiting is usually enough. If
you do not want to wait, deleting it is safe:

```sh
rm .clt/lock
```

Safe because the store itself is replaced by an **atomic rename** — there is
never a half-written `tasks.json` behind it. Readers see either the whole old
file or the whole new one.

### A write timed out waiting for the lock

The wait limit is 10 seconds. A normal critical section is a few milliseconds,
so anything approaching that bound means a real problem — usually a stale lock
younger than the 60-second expiry, or a genuinely wedged writer.

### A hand edit broke `tasks.json`

`clt` repairs on load what a careless edit can break: **duplicate ids, parents
that do not exist, and parent cycles**. Each repair is reported on stderr, and
the exit code stays 0 because the situation was handled.

If the JSON itself is malformed, `clt` refuses rather than guessing. Nothing is
overwritten — the previous content is still whatever your editor last saved.

### Can I edit the file by hand?

Yes, that is intended. Two rules:

- **Leave `origin` alone.** See [scanning](#a-scan-closed-a-task-and-refiled-it-as-a-new-one).
- **Do not reuse ids.** Ids are never reused, so deleting a task does not free
  its number. `next_id` is the counter.

### The journal is getting large

`.clt/log.jsonl` is append-only and is rotated once it gets large, so it records
history without growing without bound. Deleting it loses `clt log` history and
nothing else.

### Are concurrent writes actually safe?

Yes, and it is tested — the suite races fifteen writers against one store. A
writer holds the lock across the whole read-modify-write cycle; readers take no
lock because the atomic rename makes a torn read impossible.

---

## Sharing

### My tasks did not travel to another clone

By default the list is **local only**. `clt` adds `.clt/` to
`.git/info/exclude`, which is per-clone and untracked — so branch switching
never touches your tasks, merges never conflict on them, and your todo list
never lands in a PR diff. The cost is exactly this: it does not travel.

```sh
clt share            # lift the exclusion, then run the git add it prints
```

### `clt share` did not commit anything

It stops short of touching your index on purpose. It lifts the exclusion,
writes a `.clt/.gitignore` so `tasks.json` and hooks travel while the journal
stays local, and **prints the `git add` for you to run**.

### `tasks.json` conflicted on merge

Expected for a shared list when two people file tasks at once. The format is
line-oriented and meant to be resolved by hand, and duplicate ids are repaired
on load — so the worst case is a merge you resolve like any other.

### Is the list shared? `clt` and I disagree

It is read back from git rather than stored as a setting, so there is nothing to
get out of sync. `git rm --cached` is a perfectly good way to unshare, and
`clt path --json` will agree with you.

### `.clt/` showed up in my `.gitignore`

It should not have. `clt` writes to `.git/info/exclude` deliberately, never to
your `.gitignore`. If `.clt/` is in your `.gitignore`, something else put it
there.

---

## Output, colour and pipes

### Colour codes in a file or a pipe

`clt` disables colour when stdout is not a terminal. If you are seeing escape
codes anyway, `CLICOLOR_FORCE` is set somewhere. To force it off:

```sh
NO_COLOR=1 clt ls
```

### `--json` output has diagnostics mixed into it

It should not — nothing diagnostic is ever written to stdout, so `--json` can be
piped straight into a parser. Warnings (a repaired duplicate id, a hook that
exited non-zero) go to **stderr** and leave the exit code at 0.

If you are capturing both streams together, separate them:

```sh
clt ls --json 2>/dev/null | jq .
```

### Exit codes

| Code | Means |
| --- | --- |
| `0` | It worked. The result, if any, is on **stdout** |
| `1` | It failed. The reason is on **stderr**, as `clt: ...`. Stdout is empty |
| `2` | Unparseable arguments. Usage is on stderr |

### `clt ls \| head` kills the process oddly

A closed pipe surfaces as `EPIPE` on Unix and as a Windows pipe error on
Windows, and the release profile is built with `panic = "abort"`. Both paths
are handled and tested (`tests/pipes.rs`), but if you find one that is not,
that is a bug worth reporting with the exact command.

---

## Platform notes

### Windows: paths look like `C:/code/app\.clt\tasks.json`

Fixed. Git answers in forward slashes on every platform, including Windows, and
the mixed separator is normalised at the boundary. If you still see one, it is
a printing site that bypassed the normaliser — report it.

Both separators work when *opening* a file, so this was always cosmetic.

### Windows: the test suite takes minutes

Expected, and it is not a typo in the README. `cargo test` runs 108 tests in
roughly 15 seconds on Linux and around 2.5 minutes on Windows. Almost all the
runtime is spawning real `git` and `clt` processes against real repositories —
which is deliberate — and Windows charges roughly ten times as much for process
creation.

### macOS

Expected to work and **not tested in CI**, which runs Linux and Windows. If
something is broken there, a report with the exact command is genuinely useful.

---

## Building from source

### `cargo clippy` fails but `cargo build` is fine

CI gates on `cargo clippy --all-targets --locked -- -D warnings`, pinned to
1.97.1. `--all-targets` lints the tests too, and the tests are about a third of
the code here.

The clippy job is pinned deliberately while the build job tracks stable:
`-D warnings` on a moving toolchain turns every future lint into a build failure
on a commit nobody wrote, which teaches everyone to ignore a red badge.

### `cargo fmt --check` — where is it?

Deliberately absent. The source is hand-wrapped so the explanatory comments and
the code they explain line up, and rustfmt's default chain width reflows a good
deal of it into something harder to read. Adding the gate would mean either a
reformat commit touching every file or a `rustfmt.toml` reverse-engineered to
bless the current layout, and neither catches a bug.

Do not add one without discussing it.

### CI passes but my local test run fails

Try what CI does:

```sh
cargo build --release --locked
cargo test --locked
```

`--locked` throughout: a run that quietly resolves a different dependency tree
than `Cargo.lock` is testing something nobody committed.

Test failures here also come from the environment more often than from the
code — the suite creates branches, shells out to `git`, and races writers. Check
that `git` is configured with a `user.email` and `user.name`, which the suite's
temporary repositories need.

### The release build behaves differently from `cargo test`

They are different compilations. `cargo test` builds the dev profile; the
release profile adds `panic = "abort"`, LTO, `opt-level = "z"` and stripped
symbols, and it is the one people install. CI drives the release binary end to
end for exactly this reason.

---

## Reporting something not on this page

Include:

```sh
clt --version
git --version
clt path --json
```

plus the exact command, what you expected, and what happened. If it involves
scanning, `clt scan -n` output is worth more than a description of it.
