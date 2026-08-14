# Contributing to clt

This document is meant to get you from a fresh clone to a merged change without
having to reverse-engineer anything. If something here turns out to be wrong or
missing, that is a bug in this file and worth fixing in the same PR.

## Contents

- [Getting set up](#getting-set-up)
- [The five-minute tour](#the-five-minute-tour)
- [Module map](#module-map)
- [Invariants](#invariants)
- [Adding a command, end to end](#adding-a-command-end-to-end)
- [Adding a field to a task](#adding-a-field-to-a-task)
- [Testing](#testing)
- [What CI enforces](#what-ci-enforces)
- [Style](#style)
- [Sending a change](#sending-a-change)

## Getting set up

You need Rust (see [Requirements](README.md#requirements)) and `git` on your
`PATH` — not just to work on this, but to run the tests, which shell out to it
constantly.

```sh
git clone https://github.com/SYKhayyat/clt
cd clt
cargo build
cargo test
```

That's the whole setup. There is no code generation step, no `.env`, no
database, and eight dependencies, all mainstream.

To drive your build by hand, use a throwaway repo rather than this one —
`clt` writes to whatever repo it finds itself in, and you do not want your
experiments in the checkout you are editing:

```sh
cargo build --release
mkdir /tmp/scratch && cd /tmp/scratch && git init
~/code/clt/target/release/clt add "does this work"
~/code/clt/target/release/clt
```

## The five-minute tour

`clt` is a CLI over a JSON file, with two things that make it more than that:
it asks git which branch you are on, and it can be driven by an agent over MCP
at the same time you are typing at it.

One invocation goes:

1. `main::run` parses argv (`cli.rs`) and decides, from the subcommand alone,
   whether this invocation reads or writes (`access_for`).
2. `Store::open` finds the repo by shelling out to git (`git.rs`), locates
   `.clt/tasks.json`, takes the cross-process write lock if this is a write
   (`lock.rs`), loads the file, and repairs anything a hand edit broke.
3. A `cmd_*` function in `main.rs` mutates the in-memory store.
4. `Ctx::commit` saves (atomic rename), appends to the journal (`journal.rs`),
   releases the lock, and then fires hooks (`hooks.rs`).
5. Output goes through `render.rs`, or straight to `serde_json` if `--json`.

`clt mcp` forks off at step 1 and runs its own loop in `mcp.rs`, opening and
closing a store per request so it never holds the lock between calls.

## Module map

| Module | Lines | Owns |
| --- | --- | --- |
| `main.rs` | ~1270 | Every `cmd_*` handler, and the dispatch that picks one |
| `store.rs` | ~1420 | Load, repair, mutate, save. Scope resolution, the tree, the global fallback |
| `mcp.rs` | ~550 | The JSON-RPC server and its tool definitions |
| `scan.rs` | ~450 | Finding `TODO(clt):` markers, and deciding what counts as a comment |
| `git.rs` | ~350 | Everything that shells out to `git` |
| `render.rs` | ~320 | Terminal output: colour, column widths, relative times |
| `task.rs` | ~280 | The on-disk model — `Task`, `State`, `Location`, `Origin` |
| `cli.rs` | ~280 | The clap definitions, and nothing else |
| `journal.rs` | ~240 | The append-only log behind `clt log`, and its rotation |
| `lock.rs` | ~220 | The cross-process write lock |
| `hooks.rs` | ~160 | Running executables out of `.clt/hooks/` |

Every one of these opens with a module doc explaining *why* it is the way it
is, not what it does. Read that before changing the module — most of the
non-obvious decisions have an argument attached, and a few have a bug behind
them.

## Invariants

These are the things that will look like details and are not. Each one has
tests behind it; if your change makes one of them fail, the invariant is the
thing to defend.

**Ids are never reused.** Deleting the highest task must not free its number.
People type ids and agents quote them, and recycling `3` onto a different task
is how you close the wrong thing. `next_id` only ever goes up.

**A subtree lives on one branch.** Subtasks inherit their parent's branch, and
re-scoping moves the whole subtree. A tree split across two branches renders
with holes in it from both sides, so re-scoping a subtask alone is refused.

**Closing cascades, reopening does not.** Reopening a parent to add one more
subtask must not undo the twelve already finished.

**A scan only closes tasks filed from the branch you are on.** Your working
tree holds one branch's files, so a marker on `feat/x` is simply absent while
you are on `main`. Reading that absence as "deleted" closes work nobody
finished, permanently — a later scan sees the task already done and leaves it
alone. This one was a real bug; `tests/scanning.rs` pins it.

**Writers hold the lock from load until save.** The read-modify-write cycle is
the whole critical section. Readers take nothing, because the store is replaced
by an atomic rename and a reader therefore sees the whole old file or the whole
new one. Never write `tasks.json` in place.

**Hooks run after the lock is released.** A hook can do anything, including
block on the network, and it must not do that inside the critical section.

**The on-disk format is open.** `.clt/tasks.json` is documented, hand-editable
and agent-writable. That means new fields must be `Option` or
`#[serde(default)]` so older files still load, and nothing may assume the file
was written by us. Bump `FORMAT_VERSION` only for a change that needs a
migration.

**Nothing diagnostic goes to stdout.** Warnings, repairs and errors go to
stderr; stdout carries the result and, under `--json`, nothing else. Under
`clt mcp`, stdout is the JSON-RPC transport — a stray `println!` there corrupts
the stream, which is why hook output is redirected.

**`--json` means JSON, from every command, including the ones that write and
the ones that change nothing.** A no-op returns an empty array, not a sentence.

## Adding a command, end to end

Say you want `clt archive <ids…>`. Six places, in order:

1. **`cli.rs`** — a variant on `Command`, an `Args` struct for its flags, and a
   doc comment on each (clap turns those into `--help`).
2. **`main.rs`: `access_for`** — does it write? The match is explicit rather
   than inferred, because guessing wrong in one direction costs a few
   milliseconds of queueing and in the other direction costs somebody a task.
3. **`main.rs`: the dispatch match** — one arm calling your handler.
4. **`main.rs`: `fn cmd_archive`** — take `&mut Ctx`, mutate `ctx.store`, and
   finish with `ctx.commit(entries)` if it wrote. Handle `ctx.json` *before*
   any human-readable output, and return early.
5. **`journal.rs`** — a `journal::Entry` per change, so `clt log` shows what
   happened and who did it.
6. **`README.md`** — the command reference table, and prose if it needs
   explaining.

Then tests: an integration suite that drives the real binary (see below), and
add the command to the sweep in `tests/json_output.rs`, which asserts that
every command emits JSON when asked.

If agents should be able to call it too, add a tool to `mcp.rs` and a case to
`tests/mcp.rs`.

## Adding a field to a task

1. `task.rs` — add it to `Task` with `#[serde(default)]` and, if optional,
   `skip_serializing_if = "Option::is_none"`. Never a bare required field: a
   file written by an older `clt` has to keep loading.
2. `README.md` — add it to the `--json` row example *and* the `Storage`
   example. This is not optional politeness; `tests/contract.rs` compares the
   emitted key set against the README's examples in both directions and will
   fail the build until you do.
3. `render.rs` if it should be visible in the list, and `mcp.rs` if an agent
   should be able to set it.

## Testing

108 tests, about two and a half minutes. Almost all of that is process spawning,
and that is the design rather than an accident.

**Unit tests** live at the bottom of the module they test, in `mod tests`. Use
them for pure logic: parsing a `Location`, bucketing a relative time, breaking
a parent cycle.

**Integration tests** live in `tests/` and drive the real binary against a real
`git init`ed repository, via the helpers in `tests/common/mod.rs`:

```rust
mod common;
use common::*;

#[test]
fn a_task_filed_on_one_branch_is_not_visible_from_another() {
    let dir = repo("scoping");                   // throwaway repo, named per test
    write(&dir, "code.txt", "hello\n");          // a branch needs a commit to fork from
    git_ok(&dir, &["add", "-A"]);
    git_ok(&dir, &["commit", "-qm", "init"]);

    clt_ok(&dir, &["add", "on main"]);           // runs the binary, insists on exit 0
    git_ok(&dir, &["switch", "-qc", "feat/x"]);

    let here = clt_json(&dir, &["ls"]);          // `ls` is scoped to the branch
    assert!(here.as_array().unwrap().is_empty(), "main's task leaked onto feat/x");

    cleanup(&dir);
}
```

The helpers worth knowing: `clt_ok` runs the binary and insists on exit 0,
`clt` returns the raw `Output` when you want to assert on a failure,
`clt_json` parses `--json` output, `all_tasks`/`titles` read the whole list
across branches, and `task_titled` finds one by title with an error message
that names what was actually there.

Most of what is worth testing here lives in the seam between `clt` and git —
branch scoping, `git ls-files`, `info/exclude`, whether a list survives a
clone — and none of it is reachable from a unit test. `tests/concurrency.rs`
goes further and spawns fifteen real processes at one store, because the bug it
pins (one process's complete JSON followed by another's tail) cannot be
reproduced in-process.

Two conventions worth keeping:

- **Name the test after the property, not the function.**
  `a_scan_does_not_close_a_marker_that_lives_on_another_branch` tells the next
  reader what broke; `test_scan_2` does not.
- **Say in a doc comment what bug the test pins.** Several tests here exist
  because something specific went wrong once, and the comment is the only place
  that survives.

Give each test its own repo — `repo("some-name")` names the directory after the
test and the pid — so tests can run in parallel without sharing a store.

## What CI enforces

Both jobs run on Linux and Windows ([`.github/workflows/ci.yml`](.github/workflows/ci.yml)):

```sh
cargo build --release --locked
cargo test --locked
cargo clippy --all-targets --locked -- -D warnings
```

...plus a step that drives the *release* binary end to end, because
`cargo test` builds the dev profile and the release profile is a different
compilation (`panic = "abort"`, LTO, `opt-level = "z"`, stripped) — and it is
the one people install.

Run the clippy line before you push; it is the one that fails most often. Note
that clippy is pinned to a specific toolchain there, on purpose: `-D warnings`
would otherwise turn a new Rust release into a build failure on a commit nobody
wrote.

There is deliberately **no `cargo fmt --check`**. This source is hand-wrapped so
the explanatory comments line up with what they explain, and rustfmt's default
chain width reflows a good deal of it into something harder to read. Match the
surrounding style rather than reaching for the formatter.

## Style

The prevailing convention here is that comments answer *why*, and they are
expected to earn their space:

- Module docs argue the design. `git.rs` explains why there is no `git2`
  dependency; `lock.rs` explains why `create_new` and not `flock`.
- Function docs explain the non-obvious choice, not the signature. If a
  function's behaviour is surprising, that surprise is what the comment is for.
- Constants that encode a judgement (`WAIT_LIMIT`, `STALE_AFTER`) say what the
  judgement was.
- A comment describing what the next line does is noise. Delete it.

Prose in comments, docs and commit messages is plain English. Keep it there.

## Sending a change

- One concern per commit. The history here reads as a sequence of arguments —
  "Rotate the audit log instead of letting it grow forever" — and it is more
  useful that way than as a pile of "fix stuff".
- Write the commit message body for someone who has the diff in front of them
  and wants to know why. What was wrong, what you did, what you decided not to
  do.
- Run `cargo test` and `cargo clippy --all-targets -- -D warnings` first.
- If you changed behaviour, a test that would have failed before your change is
  the thing that makes it reviewable.
- If you changed the on-disk format or the `--json` output, update `README.md`
  in the same commit — the build enforces it, and the docs are the contract
  other people's tools are written against.
