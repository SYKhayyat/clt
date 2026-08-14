//! Loading, mutating and persisting the task list.
//!
//! Storage layout, and why:
//!
//! * Inside a repo, the list lives at `<worktree>/.clt/tasks.json`, excluded
//!   locally via `.git/info/exclude`. It sits next to the code it describes, so
//!   an agent working in the repo can find it without being told where it is —
//!   which is the whole point of the tool. It is *not* committed, so branch
//!   switching never touches it, merges never conflict on it, and your task
//!   list never lands in a PR diff.
//! * Outside a repo we fall back to one global list under the platform data
//!   dir, so `clt` still works in a scratch shell instead of erroring.
//!
//! Writes are atomic (temp file + rename) and serialized across processes (see
//! [`crate::lock`]). This is somebody's daily task list, concurrently written by
//! them and by their agent; losing it to a crash midway through
//! `serde_json::to_writer`, or to two writers interleaving, is not an acceptable
//! failure mode.

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::git::Repo;
use crate::lock::Lock;
use crate::task::{State, Task};

/// Bumped only for changes that need a migration. Readers must tolerate a
/// *lower* version by filling defaults, and refuse a higher one rather than
/// silently discarding fields they don't understand.
pub const FORMAT_VERSION: u32 = 1;

/// The task list's path relative to the repo root, for the git queries that
/// decide whether it is shared.
pub const REL_TASKS: &str = ".clt/tasks.json";

/// Dropped into `.clt/README.md` by `clt init`.
///
/// The directory holds a format this project deliberately documents as open —
/// hand-editable, agent-writable — so it should explain itself to whoever opens
/// it next, without them having to already know the tool exists.
pub const README: &str = r##"# .clt/

The task list for this repository, written by `clt`. Run `clt` anywhere in the
repo to see it, or `clt --help` for the rest.

## What is in here

| File | Purpose |
| --- | --- |
| `tasks.json` | The list itself. Documented format, safe to hand-edit. |
| `log.jsonl` | Append-only audit log: who changed what, and when. |
| `hooks/` | Executables run after a task changes. See below. |
| `.gitignore` | Present only when the list is shared; keeps local state local. |
| `lock` | Held briefly while a process writes. Safe to delete if stale. |

## tasks.json

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

`state` is `todo`, `doing` or `done`. `branch` absent means the task is
repo-wide and shows on every branch. `parent` absent means it is a root task.

Edit it by hand if you like. On load, clt repairs duplicate ids, parents that
do not exist, and parent cycles, reporting each repair on stderr. Ids are never
reused, so deleting a task does not free its number.

## Is this committed?

By default, no: `clt` adds `.clt/` to `.git/info/exclude`, which is per-clone
and untracked. That keeps the list out of pull requests and out of merge
conflicts, at the cost of it not surviving a clone.

Run `clt share` to commit it instead, and `clt unshare` to go back.

## Hooks

An executable named for the event, in `hooks/`, in any language:

    post-add  post-done  post-start  post-reopen  post-rm

The task arrives as JSON on stdin, and in `CLT_EVENT`, `CLT_TASK_ID`,
`CLT_TASK_TITLE`, `CLT_TASK_STATE`, `CLT_BRANCH`, `CLT_ACTOR` and
`CLT_TASK_JSON`. Exit status is reported but ignored — these run after the
change has already happened.

`post-add.sample` in `hooks/` is a working example. Rename it to `post-add`
(and `chmod +x` it on Unix) to enable it.
"##;

/// Written into `.clt/.gitignore` by `clt share`, so that opting in shares the
/// task list without also sharing per-clone noise.
pub const LOCAL_GITIGNORE: &str = "\
# Written by `clt share`. The task list (tasks.json) and hooks/ are shared with
# the repo; everything below is per-clone and must not be committed.
#
# The journal is local on purpose: it is an append-only record of activity in
# this checkout, and two clones appending to the same file would conflict on
# every pull.
log.jsonl
log.jsonl.*
lock
*.tmp
";

#[derive(Debug, Serialize, Deserialize)]
pub struct Data {
    pub version: u32,
    /// Monotonic id counter. Never reuses an id even after deletion: ids get
    /// typed by humans and quoted by agents, and silently recycling `3` onto a
    /// different task is a great way to close the wrong thing.
    pub next_id: u32,
    #[serde(default)]
    pub tasks: Vec<Task>,
    /// Last commit examined by `clt sync` for closing directives, so linkage
    /// scans only new history.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_commit: Option<String>,
}

impl Default for Data {
    fn default() -> Self {
        Self {
            version: FORMAT_VERSION,
            next_id: 1,
            tasks: Vec::new(),
            last_commit: None,
        }
    }
}

/// Which list we're operating on.
#[derive(Debug, Clone)]
pub enum Scope {
    Repo(Repo),
    Global,
}

impl Scope {
    pub fn repo(&self) -> Option<&Repo> {
        match self {
            Scope::Repo(r) => Some(r),
            Scope::Global => None,
        }
    }

    /// The branch a new task is scoped to, and the one the default view filters
    /// on. `None` outside a repo or on a detached HEAD.
    pub fn branch(&self) -> Option<&str> {
        self.repo().and_then(|r| r.branch.as_deref())
    }
}

/// Whether the caller intends to modify the list.
///
/// A writer takes the cross-process lock at load time and holds it until it has
/// saved, because a task edit is a read-modify-write and two of those
/// interleaved lose one of the changes. A reader takes nothing: the store is
/// replaced by an atomic rename, so a reader always sees some whole version of
/// the file, and making `clt ls` queue behind an agent's write would be a cost
/// with no benefit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Read,
    Write,
}

pub struct Store {
    pub scope: Scope,
    /// Directory holding `tasks.json`, the journal and the hooks.
    dir: PathBuf,
    data: Data,
    /// Warnings raised while loading (repaired cycles, dropped junk). Surfaced
    /// once, on stderr, rather than swallowed.
    pub warnings: Vec<String>,
    /// Set when this load performed a one-time migration, or repaired damage in
    /// the file, and the result isn't on disk yet.
    ///
    /// The caller must persist it even for a read-only command. Otherwise a
    /// plain `clt` re-runs the import on every single invocation and re-prints
    /// the same repair warning forever — and, worse, the broken file stays on
    /// disk, so every other reader of this deliberately-open format keeps
    /// seeing the damage clt has been quietly working around.
    pub migrated: bool,
    /// Held for the whole read-modify-write when opened for [`Access::Write`].
    lock: Option<Lock>,
}

impl Store {
    pub fn open(cwd: &Path, access: Access) -> Result<Self> {
        let scope = match crate::git::discover(cwd)? {
            Some(repo) => Scope::Repo(repo),
            None => Scope::Global,
        };
        Self::open_in(scope, access)
    }

    pub fn open_in(scope: Scope, access: Access) -> Result<Self> {
        let dir = match &scope {
            Scope::Repo(repo) => repo.root.join(".clt"),
            Scope::Global => global_dir()?,
        };

        // Taken before the file is read, not before it is written: the value of
        // the lock is that nobody else loads the same starting state we did.
        let lock = match access {
            Access::Write => Some(Lock::acquire(&dir)?),
            Access::Read => None,
        };

        let path = dir.join("tasks.json");
        let mut warnings = Vec::new();
        let mut migrated = false;

        let mut data = if path.exists() {
            let raw = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            parse(&raw).with_context(|| {
                format!(
                    "{} is not readable as a clt task list.\n\
                     It has not been modified — move it aside to start fresh.",
                    path.display()
                )
            })?
        } else if let Some(legacy) = find_legacy(&scope) {
            // Pre-0.1 stored a bare `{"vec":[...]}` at the repo root. Import it
            // rather than leaving someone's tasks stranded in a file we no
            // longer read. The original is left untouched.
            let (imported, note) = import_legacy(&legacy)?;
            warnings.push(note);
            migrated = true;
            imported
        } else {
            Data::default()
        };

        // A repair is as much a reason to rewrite the file as a migration is:
        // the damage is still on disk until we do.
        let repaired = repair(&mut data, &mut warnings);

        Ok(Self {
            scope,
            dir,
            data,
            warnings,
            migrated: migrated || repaired,
            lock,
        })
    }

    /// Releases the write lock early, once the last save is done.
    ///
    /// Called instead of waiting for the `Store` to drop because hooks run
    /// after the save, and a `post-add` that posts to Slack would otherwise
    /// hold every other `clt` in the repo hostage for the length of an HTTP
    /// round trip.
    pub fn release_lock(&mut self) {
        self.lock = None;
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn path(&self) -> PathBuf {
        self.dir.join("tasks.json")
    }

    /// Whether the list travels with the repo.
    ///
    /// Answered by asking git, not by a flag of our own — see
    /// [`Repo::is_tracked`]. Always false outside a repo, where there is
    /// nothing for the list to travel with.
    pub fn is_shared(&self) -> bool {
        self.scope
            .repo()
            .is_some_and(|r| r.is_tracked(REL_TASKS))
    }

    /// Writes the ignore file that keeps per-clone state out of a shared list.
    ///
    /// `tasks.json` and any hooks are the shared artifact. The journal is not:
    /// it is an append-only record of local activity, and two clones appending
    /// to it produces a merge conflict on every single pull, which would make
    /// sharing miserable enough that nobody would use it.
    pub fn write_local_gitignore(&self) -> Result<()> {
        let path = self.dir.join(".gitignore");
        std::fs::write(&path, LOCAL_GITIGNORE)
            .with_context(|| format!("writing {}", path.display()))
    }

    pub fn tasks(&self) -> &[Task] {
        &self.data.tasks
    }

    pub fn last_commit(&self) -> Option<&str> {
        self.data.last_commit.as_deref()
    }

    pub fn set_last_commit(&mut self, sha: Option<String>) {
        self.data.last_commit = sha;
    }

    pub fn get(&self, id: u32) -> Option<&Task> {
        self.data.tasks.iter().find(|t| t.id == id)
    }

    pub fn get_mut(&mut self, id: u32) -> Option<&mut Task> {
        self.data.tasks.iter_mut().find(|t| t.id == id)
    }

    /// Like [`Self::get`], but with an error message that helps.
    pub fn require(&self, id: u32) -> Result<&Task> {
        self.get(id)
            .ok_or_else(|| anyhow::anyhow!("no task #{id} (try `clt ls --all`)"))
    }

    pub fn insert(&mut self, mut task: Task) -> u32 {
        if task.id == 0 {
            task.id = self.data.next_id;
        }
        self.data.next_id = self.data.next_id.max(task.id + 1);
        let id = task.id;
        self.data.tasks.push(task);
        id
    }

    /// Allocates an id without inserting, for callers that build the task
    /// themselves.
    pub fn reserve_id(&mut self) -> u32 {
        let id = self.data.next_id;
        self.data.next_id += 1;
        id
    }

    /// Every descendant of `id`, breadth-first, excluding `id` itself.
    ///
    /// Carries a visited set even though [`repair`] guarantees an acyclic tree
    /// at load time — this also runs after in-memory reparents, and an infinite
    /// loop in a task list is a worse bug than a redundant `HashSet`.
    pub fn descendants(&self, id: u32) -> Vec<u32> {
        let mut by_parent: HashMap<u32, Vec<u32>> = HashMap::new();
        for t in &self.data.tasks {
            if let Some(p) = t.parent {
                by_parent.entry(p).or_default().push(t.id);
            }
        }

        let mut out = Vec::new();
        let mut seen = HashSet::from([id]);
        let mut queue = std::collections::VecDeque::from([id]);
        while let Some(cur) = queue.pop_front() {
            for &child in by_parent.get(&cur).map(Vec::as_slice).unwrap_or(&[]) {
                if seen.insert(child) {
                    out.push(child);
                    queue.push_back(child);
                }
            }
        }
        out
    }

    /// Ancestors of `id`, nearest first.
    pub fn ancestors(&self, id: u32) -> Vec<u32> {
        let parent_of: HashMap<u32, Option<u32>> =
            self.data.tasks.iter().map(|t| (t.id, t.parent)).collect();

        let mut out = Vec::new();
        let mut seen = HashSet::from([id]);
        let mut cur = id;
        while let Some(Some(p)) = parent_of.get(&cur).copied() {
            if !seen.insert(p) {
                break; // defensive; repair() should have made this impossible
            }
            out.push(p);
            cur = p;
        }
        out
    }

    /// Reparents `id` under `parent`, refusing anything that would corrupt the
    /// tree.
    pub fn reparent(&mut self, id: u32, parent: Option<u32>, now: DateTime<Utc>) -> Result<()> {
        self.require(id)?;

        if let Some(p) = parent {
            if p == id {
                bail!("a task cannot be its own parent");
            }
            self.require(p)?;
            // The cycle guard: `p` must not already live under `id`.
            if self.descendants(id).contains(&p) {
                bail!(
                    "#{p} is already inside #{id}'s subtree — that would make a cycle\n\
                     (detach it first: `clt move {p} --root`)"
                );
            }
            // Branch coherence: a subtree must be visible or invisible as a
            // unit, otherwise a tree is half-rendered in any given view.
            let parent_branch = self.require(p)?.branch.clone();
            let child_branch = self.require(id)?.branch.clone();
            if parent_branch != child_branch {
                bail!(
                    "#{id} is scoped to {} but #{p} is scoped to {} — \
                     subtasks must share their parent's branch",
                    describe_branch(child_branch.as_deref()),
                    describe_branch(parent_branch.as_deref()),
                );
            }
        }

        let task = self.get_mut(id).expect("checked above");
        task.parent = parent;
        task.updated = now;
        Ok(())
    }

    /// Moves `id` and everything beneath it to `branch` (`None` for repo-wide).
    ///
    /// Returns the ids that actually changed, so callers can journal and report
    /// them without re-deriving the subtree.
    ///
    /// The whole subtree moves together, and re-scoping anything other than a
    /// root is refused. Both fall out of the same invariant [`Self::reparent`]
    /// enforces: a subtask lives on its parent's branch. Moving one task out
    /// from under its parent would leave a tree that renders half-visible on
    /// either branch, which is worse than the inconvenience of being told to
    /// re-scope the parent instead.
    pub fn set_scope(
        &mut self,
        id: u32,
        branch: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<Vec<u32>> {
        let task = self.require(id)?;
        if let Some(parent) = task.parent {
            bail!(
                "#{id} is nested under #{parent}, and a subtask shares its parent's scope\n\
                 (re-scope #{parent}, or detach it first: `clt move {id} --root`)"
            );
        }

        let mut targets = vec![id];
        targets.extend(self.descendants(id));

        let mut changed = Vec::new();
        for target in targets {
            let Some(task) = self.get_mut(target) else {
                continue;
            };
            if task.branch.as_deref() == branch {
                continue;
            }
            task.branch = branch.map(str::to_owned);
            task.updated = now;
            changed.push(target);
        }
        Ok(changed)
    }

    /// Tasks pinned to a branch that no longer exists.
    ///
    /// This is what happens to every task on a feature branch you merged and
    /// deleted: it is scoped to a name git has forgotten, so it matches no view
    /// except `--all` and quietly stops existing as far as you are concerned.
    /// Repo-wide tasks are never orphaned — they belong to no branch by design.
    pub fn orphaned(&self, live: &HashSet<String>) -> Vec<&Task> {
        self.data
            .tasks
            .iter()
            .filter(|t| t.branch.as_deref().is_some_and(|b| !live.contains(b)))
            .collect()
    }

    /// Removes `id` and its entire subtree. Returns the ids removed.
    pub fn remove_subtree(&mut self, id: u32) -> Vec<u32> {
        let mut doomed: HashSet<u32> = self.descendants(id).into_iter().collect();
        doomed.insert(id);
        self.data.tasks.retain(|t| !doomed.contains(&t.id));
        let mut ids: Vec<u32> = doomed.into_iter().collect();
        ids.sort_unstable();
        ids
    }

    /// True when `task` should appear in a view scoped to `branch`.
    ///
    /// Repo-wide tasks (`branch: None`) are always visible; that's what makes
    /// them repo-wide. On a detached HEAD only repo-wide tasks show, since
    /// there is no branch to match against.
    pub fn in_scope(task: &Task, branch: Option<&str>) -> bool {
        match (task.branch.as_deref(), branch) {
            (None, _) => true,
            (Some(_), None) => false,
            (Some(tb), Some(cb)) => tb == cb,
        }
    }

    /// Flattens the task list into display order: depth-first, parents before
    /// children, in-progress work first.
    ///
    /// `keep` selects the tasks the caller actually asked for. Ancestors of a
    /// kept task are pulled in even when they don't match, so a matched subtask
    /// is never rendered floating with no context — they come back marked
    /// `context: true` so the renderer can dim them.
    pub fn tree(&self, keep: impl Fn(&Task) -> bool) -> Vec<Row<'_>> {
        let matched: HashSet<u32> = self
            .data
            .tasks
            .iter()
            .filter(|t| keep(t))
            .map(|t| t.id)
            .collect();

        // Built once and walked directly, rather than calling `ancestors()` per
        // match: that rebuilds this same map on every call, so a list where most
        // tasks match cost O(n²) — the dominant term in rendering a large list,
        // far more so than the per-row progress lookup.
        let parent_of: HashMap<u32, Option<u32>> =
            self.data.tasks.iter().map(|t| (t.id, t.parent)).collect();

        let mut visible = matched.clone();
        for &id in &matched {
            let mut cur = id;
            // `visible` doubles as the visited set: reaching something already
            // marked means the rest of this chain is already accounted for,
            // which also makes a cycle terminate.
            while let Some(Some(parent)) = parent_of.get(&cur).copied() {
                if !visible.insert(parent) {
                    break;
                }
                cur = parent;
            }
        }

        let mut by_parent: HashMap<Option<u32>, Vec<&Task>> = HashMap::new();
        for t in self.data.tasks.iter().filter(|t| visible.contains(&t.id)) {
            // A task whose parent got filtered out is rendered as a root, so
            // nothing is orphaned off-screen.
            let parent = t.parent.filter(|p| visible.contains(p));
            by_parent.entry(parent).or_default().push(t);
        }
        for group in by_parent.values_mut() {
            group.sort_by_key(|t| (t.state.rank(), t.id));
        }

        // Iterative pre-order DFS. Explicit stack rather than recursion: depth
        // here is attacker-controlled in the mundane sense that it comes from a
        // file an agent writes, and "your task list crashed the stack" is not a
        // bug report worth receiving.
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let mut stack: Vec<(&Task, usize)> = by_parent
            .get(&None)
            .map(|roots| roots.iter().rev().map(|t| (*t, 0usize)).collect())
            .unwrap_or_default();

        while let Some((task, depth)) = stack.pop() {
            if !seen.insert(task.id) {
                continue;
            }
            out.push(Row {
                task,
                depth,
                context: !matched.contains(&task.id),
            });
            if let Some(kids) = by_parent.get(&Some(task.id)) {
                // Reversed so siblings pop off in sorted order.
                for kid in kids.iter().rev() {
                    stack.push((*kid, depth + 1));
                }
            }
        }
        out
    }

    /// Done/total counts across a task's descendants, for the `2/5` rollup.
    ///
    /// Convenient for a single lookup. Rendering a list wants
    /// [`Self::progress_all`] instead — this rebuilds the parent index on every
    /// call, so asking it once per row is quadratic in the size of the list.
    pub fn progress(&self, id: u32) -> Option<(usize, usize)> {
        let kids = self.descendants(id);
        if kids.is_empty() {
            return None;
        }
        let done = kids
            .iter()
            .filter(|&&k| self.get(k).is_some_and(Task::is_done))
            .count();
        Some((done, kids.len()))
    }

    /// The same rollup for every task at once, in one pass.
    ///
    /// Tasks with no subtasks are absent from the map rather than present as
    /// zero, matching [`Self::progress`] returning `None` for a leaf.
    pub fn progress_all(&self) -> HashMap<u32, (usize, usize)> {
        let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
        for t in &self.data.tasks {
            if let Some(p) = t.parent {
                children.entry(p).or_default().push(t.id);
            }
        }

        // Pre-order across the whole forest. Every task is a starting point if
        // nothing has reached it yet, so a node orphaned by a hand-edit still
        // gets counted instead of silently reading 0/0.
        let mut order: Vec<u32> = Vec::with_capacity(self.data.tasks.len());
        let mut seen: HashSet<u32> = HashSet::new();
        for root in self.data.tasks.iter().filter(|t| t.parent.is_none()) {
            walk(root.id, &children, &mut seen, &mut order);
        }
        for t in &self.data.tasks {
            walk(t.id, &children, &mut seen, &mut order);
        }

        let done_flag: HashMap<u32, usize> = self
            .data
            .tasks
            .iter()
            .map(|t| (t.id, usize::from(t.is_done())))
            .collect();

        // Reversed pre-order visits every child before its parent, so each
        // subtotal is ready by the time the parent needs it.
        let mut subtree: HashMap<u32, (usize, usize)> = HashMap::new();
        for id in order.iter().rev() {
            let mut done = done_flag.get(id).copied().unwrap_or(0);
            let mut total = 1;
            for kid in children.get(id).map(Vec::as_slice).unwrap_or(&[]) {
                let (kd, kt) = subtree.get(kid).copied().unwrap_or((0, 0));
                done += kd;
                total += kt;
            }
            subtree.insert(*id, (done, total));
        }

        // Report descendants only, so a parent's own state never counts towards
        // its own progress.
        subtree
            .into_iter()
            .filter_map(|(id, (done, total))| {
                let self_done = done_flag.get(&id).copied().unwrap_or(0);
                (total > 1).then_some((id, (done - self_done, total - 1)))
            })
            .collect()
    }

    /// Finds an existing task harvested from the same source marker.
    pub fn find_by_scan_key(&self, key: &str) -> Option<&Task> {
        self.data.tasks.iter().find(|t| t.scan_key() == Some(key))
    }

    pub fn save(&self) -> Result<()> {
        // Only claim the exclude entry when writing the list for the first
        // time. Re-adding it on every save would silently undo `clt share`, and
        // would re-exclude a list that arrived with a clone.
        //
        // Keyed on the task file rather than on the directory: taking the write
        // lock creates the directory, so by the time we get here `.clt/` always
        // exists and testing for it would mean the entry was never written at
        // all.
        let first_time = !self.path().exists();

        std::fs::create_dir_all(&self.dir)
            .with_context(|| format!("creating {}", self.dir.display()))?;

        // A store opened read-only still saves in one case: a legacy import has
        // to land on disk or every subsequent run repeats it. Take the lock for
        // that write alone, so it can't interleave with a real writer.
        let _borrowed = match self.lock {
            Some(_) => None,
            None => Some(Lock::acquire(&self.dir)?),
        };

        if first_time
            && let Scope::Repo(repo) = &self.scope
        {
            repo.ensure_excluded();
        }

        let path = self.path();

        // Write-then-rename. A half-written temp file is garbage we can throw
        // away; a half-written tasks.json is your week.
        //
        // The temp name carries the pid. A shared `tasks.json.tmp` was the
        // original bug here and it was worse than a lost update: two writers
        // truncating and filling the same temp file produced a rename of one
        // process's complete JSON followed by the other's tail, and the list
        // came back "not readable as a clt task list". Writers are serialized
        // by the lock above, so this is belt to that suspenders — but the
        // migration path can save without holding it, and a temp file is
        // cheaper to make unique than to reason about.
        let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
        let bytes = serde_json::to_vec_pretty(&self.data).context("serializing task list")?;
        std::fs::write(&tmp, &bytes).with_context(|| format!("writing {}", tmp.display()))?;

        if let Err(e) = replace(&tmp, &path) {
            // Don't leave our debris behind for a failure the user now has to
            // read about.
            let _ = std::fs::remove_file(&tmp);
            return Err(e).with_context(|| format!("replacing {}", path.display()));
        }
        Ok(())
    }
}

/// `rename`, retried briefly.
///
/// The write lock keeps other *writers* out, but readers take no lock by
/// design, and on Windows replacing a file that another process has open can
/// fail with a sharing violation — from a concurrent `clt ls`, or from a virus
/// scanner that opened the file the instant we wrote it. Both clear in
/// milliseconds.
///
/// This is exactly the case the tool is built for, so failing the user's
/// `clt add` because someone else was reading at that moment is not acceptable.
/// A handful of retries covers it; a persistent failure (no permission, disk
/// full) still surfaces, because the last error is what gets returned.
fn replace(tmp: &Path, path: &Path) -> std::io::Result<()> {
    const ATTEMPTS: u32 = 12;
    let mut delay = std::time::Duration::from_millis(2);

    for attempt in 1..=ATTEMPTS {
        match std::fs::rename(tmp, path) {
            Ok(()) => return Ok(()),
            Err(e) if attempt == ATTEMPTS => return Err(e),
            Err(e) if crate::lock::is_transient(&e) => {
                std::thread::sleep(delay);
                // Backs off to ~50ms by the final attempt, roughly a quarter of
                // a second in total, which no interactive user will notice and
                // every sharing violation outlives.
                delay = (delay * 2).min(std::time::Duration::from_millis(50));
            }
            Err(e) => return Err(e),
        }
    }
    unreachable!("the loop returns on its final attempt")
}

/// Appends `start` and everything under it to `order`, pre-order.
///
/// Iterative rather than recursive, and guarded by `seen`, for the same reason
/// [`Store::tree`] is: the depth comes from a file an agent writes, and a task
/// list that overflows the stack is not a bug report worth receiving.
fn walk(
    start: u32,
    children: &HashMap<u32, Vec<u32>>,
    seen: &mut HashSet<u32>,
    order: &mut Vec<u32>,
) {
    let mut stack = vec![start];
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        order.push(id);
        for kid in children.get(&id).map(Vec::as_slice).unwrap_or(&[]) {
            stack.push(*kid);
        }
    }
}

fn describe_branch(branch: Option<&str>) -> String {
    match branch {
        Some(b) => format!("`{b}`"),
        None => "the whole repo".to_string(),
    }
}

/// A task plus where it sits in the rendered tree.
pub struct Row<'a> {
    pub task: &'a Task,
    pub depth: usize,
    /// True when this row is only present to give a matched descendant its
    /// context, and didn't match the filter itself.
    pub context: bool,
}

fn parse(raw: &str) -> Result<Data> {
    let data: Data = serde_json::from_str(raw)?;
    if data.version > FORMAT_VERSION {
        bail!(
            "task list is format version {} but this clt understands up to {}. \
             Upgrade clt rather than letting an old build rewrite the file.",
            data.version,
            FORMAT_VERSION
        );
    }
    Ok(data)
}

/// Repairs anything a hand-edit (by a human or an agent) could have broken.
///
/// The file is advertised as agent-writable, so treating it as trusted input
/// would be naive. Everything here fixes rather than rejects: refusing to start
/// because a subtask points at a deleted parent would be a terrible trade.
///
/// Returns whether anything was actually changed, so the caller knows the copy
/// on disk no longer matches the one in memory.
fn repair(data: &mut Data, warnings: &mut Vec<String>) -> bool {
    let before = warnings.len();
    // Duplicate ids: keep the first, renumber the rest onto fresh ids.
    let mut seen_ids = HashSet::new();
    let mut renumbered = Vec::new();
    let mut next = data.next_id.max(1);
    for task in &mut data.tasks {
        next = next.max(task.id.saturating_add(1));
        if !seen_ids.insert(task.id) {
            let old = task.id;
            task.id = next;
            next += 1;
            seen_ids.insert(task.id);
            renumbered.push((old, task.id));
        }
    }
    data.next_id = next;
    for (old, new) in renumbered {
        warnings.push(format!("duplicate id #{old} renumbered to #{new}"));
    }

    // Parents that don't exist.
    let ids: HashSet<u32> = data.tasks.iter().map(|t| t.id).collect();
    for task in &mut data.tasks {
        if let Some(p) = task.parent
            && !ids.contains(&p)
        {
            warnings.push(format!("#{} pointed at missing parent #{p}; detached", task.id));
            task.parent = None;
        }
        if task.parent == Some(task.id) {
            warnings.push(format!("#{} was its own parent; detached", task.id));
            task.parent = None;
        }
    }

    // Cycles. Without this a hand-edited `3 → 5 → 3` turns every tree walk into
    // an infinite loop, and `clt` — the thing you run twenty times a day —
    // hangs or blows the stack.
    for cycle in find_cycles(&data.tasks) {
        // Break at the lowest id on the cycle: deterministic, and it keeps the
        // largest possible intact subtree.
        let cut = cycle.iter().copied().min().expect("a cycle is non-empty");
        for task in &mut data.tasks {
            if task.id == cut {
                task.parent = None;
            }
        }
        warnings.push(format!(
            "parent cycle {} — detached #{cut} to break it",
            cycle
                .iter()
                .map(|id| format!("#{id}"))
                .collect::<Vec<_>>()
                .join(" → ")
        ));
    }

    if data.version < FORMAT_VERSION {
        data.version = FORMAT_VERSION;
    }

    // Every repair above records a warning, so "did we change anything" and
    // "did we have anything to say" are the same question.
    warnings.len() > before
}

/// Every parent cycle in the list, each as the ids that form the loop.
///
/// The distinction that matters here is between a task that *is* on a cycle and
/// one that merely *points into* one. The original implementation collected
/// both and then detached the lowest id it had found, which is very often a
/// task hanging off the loop rather than in it — so the cycle survived, its
/// members stayed unreachable from any root, and they vanished from `ls`,
/// `find` and `--json` alike while a warning announced the repair. Tasks
/// silently absent from every view is the worst outcome available, given the
/// whole point of the repair is that a hand-edited file should not cost you
/// data.
///
/// Every task has at most one parent, so cycles are disjoint and detaching a
/// single member breaks one outright — no need to re-run to a fixed point.
fn find_cycles(tasks: &[Task]) -> Vec<Vec<u32>> {
    let parent_of: HashMap<u32, Option<u32>> = tasks.iter().map(|t| (t.id, t.parent)).collect();

    let mut cycles = Vec::new();
    // Ids whose ancestry has already been walked. Without this the scan is
    // quadratic on a long chain, and re-reports the same cycle once per task
    // that leads into it.
    let mut settled: HashSet<u32> = HashSet::new();

    for task in tasks {
        if settled.contains(&task.id) {
            continue;
        }
        // Where each id sits in the current walk, so that revisiting one tells
        // us not just *that* there is a loop but where it starts.
        let mut position: HashMap<u32, usize> = HashMap::new();
        let mut path: Vec<u32> = Vec::new();
        let mut cur = task.id;

        loop {
            if settled.contains(&cur) {
                break; // joins ancestry we have already accounted for
            }
            if let Some(&start) = position.get(&cur) {
                // Everything from the first sighting onwards is the loop; what
                // came before it is the tail leading in, and is not cyclic.
                cycles.push(path[start..].to_vec());
                break;
            }
            position.insert(cur, path.len());
            path.push(cur);
            match parent_of.get(&cur).copied().flatten() {
                Some(parent) => cur = parent,
                None => break, // reached a root
            }
        }
        settled.extend(path);
    }
    cycles
}

/// Platform data directory for the out-of-repo fallback list.
///
/// Hand-rolled rather than pulling in `dirs`: it's ten lines and two env vars,
/// and a dependency that exists to read `$XDG_DATA_HOME` is not a good trade.
fn global_dir() -> Result<PathBuf> {
    #[cfg(windows)]
    {
        if let Ok(base) = std::env::var("LOCALAPPDATA")
            && !base.is_empty()
        {
            return Ok(PathBuf::from(base).join("clt"));
        }
        if let Ok(profile) = std::env::var("USERPROFILE")
            && !profile.is_empty()
        {
            return Ok(PathBuf::from(profile)
                .join("AppData")
                .join("Local")
                .join("clt"));
        }
        bail!("cannot locate an application data directory (LOCALAPPDATA and USERPROFILE are unset)")
    }
    #[cfg(not(windows))]
    {
        if let Ok(xdg) = std::env::var("XDG_DATA_HOME")
            && !xdg.is_empty()
        {
            return Ok(PathBuf::from(xdg).join("clt"));
        }
        if let Ok(home) = std::env::var("HOME")
            && !home.is_empty()
        {
            return Ok(PathBuf::from(home).join(".local").join("share").join("clt"));
        }
        bail!("cannot locate a data directory (XDG_DATA_HOME and HOME are unset)")
    }
}

// ---------------------------------------------------------------------------
// Legacy import
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct LegacyList {
    vec: Vec<LegacyTask>,
}

#[derive(Deserialize)]
struct LegacyTask {
    id: i32,
    name: String,
    description: String,
    state: String,
}

fn find_legacy(scope: &Scope) -> Option<PathBuf> {
    let root = scope.repo()?.root.clone();
    let path = root.join("tasks.json");
    path.exists().then_some(path)
}

fn import_legacy(path: &Path) -> Result<(Data, String)> {
    let raw = std::fs::read_to_string(path)?;
    let legacy: LegacyList = serde_json::from_str(&raw)
        .with_context(|| format!("{} exists but isn't a pre-0.1 clt list", path.display()))?;

    let now = Utc::now();
    let mut data = Data::default();
    for old in legacy.vec {
        let id = u32::try_from(old.id).unwrap_or(data.next_id);
        let mut task = Task::new(id, old.name, now);
        task.state = old.state.parse().unwrap_or(State::Todo);
        task.note = (!old.description.trim().is_empty()).then_some(old.description);
        // Legacy tasks predate branch scoping, so they become repo-wide rather
        // than getting silently pinned to whatever branch you happen to be on.
        task.branch = None;
        data.next_id = data.next_id.max(id + 1);
        data.tasks.push(task);
    }

    let count = data.tasks.len();
    Ok((
        data,
        format!(
            "imported {count} task{} from {} (the original is untouched; delete it when happy)",
            if count == 1 { "" } else { "s" },
            path.display()
        ),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A store's worth of tasks, with `next_id` past the highest one so an
    /// insert can't collide with a fixture.
    fn data_with(tasks: Vec<Task>) -> Data {
        Data {
            next_id: tasks.iter().map(|t| t.id + 1).max().unwrap_or(1),
            tasks,
            ..Data::default()
        }
    }

    fn store_with(tasks: Vec<Task>) -> Store {
        let data = data_with(tasks);
        Store {
            scope: Scope::Global,
            dir: PathBuf::from("."),
            data,
            warnings: Vec::new(),
            migrated: false,
            // Pure in-memory fixtures never save, so they need no lock — and
            // taking one would have these tests contend with each other.
            lock: None,
        }
    }

    fn task(id: u32, parent: Option<u32>) -> Task {
        let mut t = Task::new(id, format!("task {id}"), Utc::now());
        t.parent = parent;
        t
    }

    #[test]
    fn ids_are_never_reused() {
        let mut store = store_with(vec![task(1, None), task(2, None)]);
        store.remove_subtree(2);
        let mut fresh = Task::new(0, "new", Utc::now());
        fresh.id = 0;
        let id = store.insert(fresh);
        assert_eq!(id, 3, "deleting the highest id must not free it for reuse");
    }

    #[test]
    fn removing_a_parent_takes_the_whole_subtree() {
        let mut store = store_with(vec![
            task(1, None),
            task(2, Some(1)),
            task(3, Some(2)),
            task(4, None),
        ]);
        let removed = store.remove_subtree(1);
        assert_eq!(removed, vec![1, 2, 3]);
        assert_eq!(store.tasks().len(), 1);
        assert_eq!(store.tasks()[0].id, 4);
    }

    #[test]
    fn reparent_refuses_to_build_a_cycle() {
        let mut store = store_with(vec![task(1, None), task(2, Some(1)), task(3, Some(2))]);
        // Putting 1 under its own grandchild would close the loop.
        let err = store.reparent(1, Some(3), Utc::now()).unwrap_err();
        assert!(err.to_string().contains("cycle"), "got: {err}");
    }

    #[test]
    fn reparent_refuses_to_be_its_own_parent() {
        let mut store = store_with(vec![task(1, None)]);
        assert!(store.reparent(1, Some(1), Utc::now()).is_err());
    }

    #[test]
    fn reparent_refuses_to_cross_branches() {
        let mut a = task(1, None);
        a.branch = Some("main".into());
        let mut b = task(2, None);
        b.branch = Some("feat/x".into());
        let mut store = store_with(vec![a, b]);

        let err = store.reparent(2, Some(1), Utc::now()).unwrap_err();
        assert!(err.to_string().contains("branch"), "got: {err}");
    }

    #[test]
    fn load_breaks_parent_cycles_instead_of_hanging() {
        // Exactly what a careless hand-edit produces.
        let mut data = data_with(vec![task(1, Some(3)), task(2, Some(1)), task(3, Some(2))]);
        let mut warnings = Vec::new();
        repair(&mut data, &mut warnings);

        assert_eq!(data.tasks.iter().filter(|t| t.parent.is_none()).count(), 1);
        assert!(warnings.iter().any(|w| w.contains("cycle")), "{warnings:?}");

        // And the tree walk now terminates.
        let store = store_with(data.tasks);
        assert_eq!(store.tree(|_| true).len(), 3);
    }

    /// Ids still reachable by walking down from the roots — i.e. everything the
    /// renderer, `find` and `--json` can actually see.
    fn visible(store: &Store) -> HashSet<u32> {
        store.tree(|_| true).iter().map(|r| r.task.id).collect()
    }

    #[test]
    fn a_cycle_is_broken_even_when_another_task_points_into_it() {
        // The shape the old repair could not handle: 2 ↔ 3 is the cycle, and 1
        // merely hangs off it. Collecting "everything that reaches a cycle" put
        // #1 in the set, and detaching the lowest id detached #1 — which was
        // never the problem. The loop survived, and #2 and #3 disappeared from
        // every view while the warning claimed a fix.
        let mut data = data_with(vec![task(1, Some(2)), task(2, Some(3)), task(3, Some(2))]);
        let mut warnings = Vec::new();
        repair(&mut data, &mut warnings);

        assert!(
            find_cycles(&data.tasks).is_empty(),
            "the cycle must actually be gone, not merely reported"
        );
        assert!(warnings.iter().any(|w| w.contains("cycle")), "{warnings:?}");

        let store = store_with(data.tasks);
        assert_eq!(
            visible(&store),
            HashSet::from([1, 2, 3]),
            "no task may be left unreachable from any root"
        );
    }

    #[test]
    fn several_independent_cycles_are_all_broken() {
        // Each task has one parent, so cycles are disjoint — but there can be
        // more than one, and stopping after the first would leave the rest.
        let mut data = data_with(vec![
            task(1, Some(2)),
            task(2, Some(1)),
            task(3, Some(4)),
            task(4, Some(5)),
            task(5, Some(3)),
        ]);
        let mut warnings = Vec::new();
        repair(&mut data, &mut warnings);

        assert!(find_cycles(&data.tasks).is_empty());
        assert_eq!(
            warnings.iter().filter(|w| w.contains("cycle")).count(),
            2,
            "each cycle deserves its own report"
        );
        let store = store_with(data.tasks);
        assert_eq!(visible(&store).len(), 5);
    }

    #[test]
    fn a_task_that_is_its_own_parent_is_a_cycle_of_one() {
        let mut data = data_with(vec![task(1, Some(1)), task(2, Some(1))]);
        let mut warnings = Vec::new();
        repair(&mut data, &mut warnings);

        assert_eq!(data.tasks[0].parent, None);
        let store = store_with(data.tasks);
        assert_eq!(visible(&store).len(), 2);
    }

    #[test]
    fn find_cycles_reports_only_the_loop_not_the_tail_leading_into_it() {
        // 1 → 2 → 3 → 4 → 3. The cycle is {3,4}; 1 and 2 are just a tail.
        let tasks = vec![
            task(1, Some(2)),
            task(2, Some(3)),
            task(3, Some(4)),
            task(4, Some(3)),
        ];
        let cycles = find_cycles(&tasks);
        assert_eq!(cycles.len(), 1);
        let members: HashSet<u32> = cycles[0].iter().copied().collect();
        assert_eq!(members, HashSet::from([3, 4]), "got {:?}", cycles[0]);
    }

    #[test]
    fn find_cycles_leaves_an_ordinary_tree_alone() {
        let tasks = vec![task(1, None), task(2, Some(1)), task(3, Some(2))];
        assert!(find_cycles(&tasks).is_empty());
    }

    #[test]
    fn load_detaches_tasks_pointing_at_missing_parents() {
        let mut data = data_with(vec![task(1, Some(99))]);
        let mut warnings = Vec::new();
        repair(&mut data, &mut warnings);
        assert_eq!(data.tasks[0].parent, None);
        assert!(warnings.iter().any(|w| w.contains("missing parent")));
    }

    #[test]
    fn load_renumbers_duplicate_ids() {
        let mut data = data_with(vec![task(1, None), task(1, None)]);
        let mut warnings = Vec::new();
        repair(&mut data, &mut warnings);
        let ids: HashSet<u32> = data.tasks.iter().map(|t| t.id).collect();
        assert_eq!(ids.len(), 2, "duplicate ids must be resolved");
        assert!(warnings.iter().any(|w| w.contains("duplicate")));
    }

    #[test]
    fn re_scoping_takes_the_whole_subtree() {
        // Half a tree on one branch and half on another renders as a tree with
        // holes in it from both sides, so the subtree is the unit of movement.
        let mut a = task(1, None);
        a.branch = Some("feat/x".into());
        let mut b = task(2, Some(1));
        b.branch = Some("feat/x".into());
        let mut c = task(3, Some(2));
        c.branch = Some("feat/x".into());
        let mut store = store_with(vec![a, b, c]);

        let moved = store.set_scope(1, None, Utc::now()).unwrap();
        assert_eq!(moved, vec![1, 2, 3]);
        assert!(store.tasks().iter().all(|t| t.branch.is_none()));
    }

    #[test]
    fn re_scoping_a_subtask_is_refused() {
        let mut parent = task(1, None);
        parent.branch = Some("feat/x".into());
        let mut child = task(2, Some(1));
        child.branch = Some("feat/x".into());
        let mut store = store_with(vec![parent, child]);

        let err = store.set_scope(2, None, Utc::now()).unwrap_err();
        assert!(err.to_string().contains("nested under #1"), "got: {err}");
        // And nothing moved.
        assert_eq!(store.get(2).unwrap().branch.as_deref(), Some("feat/x"));
    }

    #[test]
    fn re_scoping_reports_only_what_actually_changed() {
        let mut t = task(1, None);
        t.branch = Some("main".into());
        let mut store = store_with(vec![t]);
        assert!(
            store.set_scope(1, Some("main"), Utc::now()).unwrap().is_empty(),
            "moving a task where it already is is not a change"
        );
    }

    #[test]
    fn orphans_are_tasks_whose_branch_git_has_forgotten() {
        let mut live_branch = task(1, None);
        live_branch.branch = Some("main".into());
        let mut dead_branch = task(2, None);
        dead_branch.branch = Some("feat/merged-and-deleted".into());
        let repo_wide = task(3, None);

        let store = store_with(vec![live_branch, dead_branch, repo_wide]);
        let live = HashSet::from(["main".to_string()]);

        let orphans: Vec<u32> = store.orphaned(&live).iter().map(|t| t.id).collect();
        assert_eq!(
            orphans,
            vec![2],
            "only the task on a branch that no longer exists — a repo-wide task \
             belongs to no branch and can never be orphaned"
        );
    }

    #[test]
    fn repo_wide_tasks_are_visible_from_every_branch() {
        let repo_wide = task(1, None);
        assert!(Store::in_scope(&repo_wide, Some("main")));
        assert!(Store::in_scope(&repo_wide, Some("feat/x")));
        assert!(Store::in_scope(&repo_wide, None));
    }

    #[test]
    fn branch_tasks_hide_on_other_branches_and_on_detached_head() {
        let mut scoped = task(1, None);
        scoped.branch = Some("feat/x".into());
        assert!(Store::in_scope(&scoped, Some("feat/x")));
        assert!(!Store::in_scope(&scoped, Some("main")));
        assert!(!Store::in_scope(&scoped, None));
    }

    #[test]
    fn tree_pulls_in_ancestors_of_matches_as_context() {
        let store = store_with(vec![task(1, None), task(2, Some(1)), task(3, Some(2))]);
        // Match only the deepest task; its ancestors must come along.
        let rows = store.tree(|t| t.id == 3);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows.iter().map(|r| r.task.id).collect::<Vec<_>>(), vec![1, 2, 3]);
        assert_eq!(rows.iter().map(|r| r.depth).collect::<Vec<_>>(), vec![0, 1, 2]);
        assert!(rows[0].context, "unmatched ancestor should be marked context");
        assert!(!rows[2].context, "the match itself is not context");
    }

    #[test]
    fn tree_reroots_children_whose_parent_was_filtered_out() {
        let store = store_with(vec![task(1, None), task(2, Some(1))]);
        // Keep only the child, and suppress the ancestor pull-in by matching a
        // task with no parent in the visible set.
        let rows = store.tree(|t| t.id == 2);
        // Parent comes back as context, so nothing floats.
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].depth, 0);
    }

    #[test]
    fn progress_counts_the_whole_subtree() {
        let mut kids = vec![task(1, None), task(2, Some(1)), task(3, Some(2))];
        kids[1].state = State::Done;
        let store = store_with(kids);
        assert_eq!(store.progress(1), Some((1, 2)));
        assert_eq!(store.progress(3), None, "a leaf has no progress");
    }

    #[test]
    fn progress_all_agrees_with_progress_task_for_task() {
        // The batch version exists only to be faster, so the one thing that
        // must never differ is the answer.
        let mut tasks = vec![
            task(1, None),
            task(2, Some(1)),
            task(3, Some(1)),
            task(4, Some(2)),
            task(5, Some(4)),
            task(6, None), // a leaf root
            task(7, None),
        ];
        tasks[1].state = State::Done; // #2
        tasks[4].state = State::Done; // #5
        tasks[6].state = State::Done; // #7
        let store = store_with(tasks);

        let batch = store.progress_all();
        for t in store.tasks() {
            assert_eq!(
                batch.get(&t.id).copied(),
                store.progress(t.id),
                "disagreement on #{}",
                t.id
            );
        }
        // And spot-check the shape, so a mutual failure can't pass.
        assert_eq!(batch.get(&1).copied(), Some((2, 4)));
        assert_eq!(batch.get(&6), None, "a leaf is absent, not (0, 0)");
    }

    #[test]
    fn progress_all_counts_a_parent_that_is_itself_done() {
        // A parent's own state must not leak into its own rollup.
        let mut tasks = vec![task(1, None), task(2, Some(1))];
        tasks[0].state = State::Done;
        let store = store_with(tasks);
        assert_eq!(store.progress_all().get(&1).copied(), Some((0, 1)));
    }

    #[test]
    fn format_version_from_the_future_is_refused_not_silently_rewritten() {
        let raw = r#"{"version":999,"next_id":1,"tasks":[]}"#;
        let err = parse(raw).unwrap_err();
        assert!(err.to_string().contains("Upgrade clt"), "got: {err}");
    }

    #[test]
    fn legacy_lists_import_as_repo_wide_tasks() {
        let dir = std::env::temp_dir().join(format!("clt-legacy-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tasks.json");
        std::fs::write(
            &path,
            r#"{"vec":[{"id":1,"name":"Fix empty list","description":"Fix the code","state":"InProgress"}]}"#,
        )
        .unwrap();

        let (data, note) = import_legacy(&path).unwrap();
        assert_eq!(data.tasks.len(), 1);
        assert_eq!(data.tasks[0].title, "Fix empty list");
        assert_eq!(data.tasks[0].state, State::Doing);
        assert_eq!(data.tasks[0].note.as_deref(), Some("Fix the code"));
        assert_eq!(data.tasks[0].branch, None, "legacy tasks predate branches");
        assert!(note.contains("imported 1 task"));

        std::fs::remove_dir_all(&dir).ok();
    }
}
