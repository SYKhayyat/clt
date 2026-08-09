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
//! Writes are atomic (temp file + rename). This is somebody's daily task list;
//! losing it to a crash midway through `serde_json::to_writer` is not an
//! acceptable failure mode.

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::git::Repo;
use crate::task::{State, Task};

/// Bumped only for changes that need a migration. Readers must tolerate a
/// *lower* version by filling defaults, and refuse a higher one rather than
/// silently discarding fields they don't understand.
pub const FORMAT_VERSION: u32 = 1;

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

pub struct Store {
    pub scope: Scope,
    /// Directory holding `tasks.json`, the journal and the hooks.
    dir: PathBuf,
    data: Data,
    /// Warnings raised while loading (repaired cycles, dropped junk). Surfaced
    /// once, on stderr, rather than swallowed.
    pub warnings: Vec<String>,
    /// Set when this load performed a one-time migration whose result isn't on
    /// disk yet. The caller must persist it even for a read-only command —
    /// otherwise a plain `clt` re-runs the import on every single invocation.
    pub migrated: bool,
}

impl Store {
    pub fn open(cwd: &Path) -> Result<Self> {
        let scope = match crate::git::discover(cwd)? {
            Some(repo) => Scope::Repo(repo),
            None => Scope::Global,
        };
        Self::open_in(scope)
    }

    pub fn open_in(scope: Scope) -> Result<Self> {
        let dir = match &scope {
            Scope::Repo(repo) => repo.root.join(".clt"),
            Scope::Global => global_dir()?,
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

        repair(&mut data, &mut warnings);

        Ok(Self {
            scope,
            dir,
            data,
            warnings,
            migrated,
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn path(&self) -> PathBuf {
        self.dir.join("tasks.json")
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

        let mut visible = matched.clone();
        for &id in &matched {
            visible.extend(self.ancestors(id));
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

    /// Finds an existing task harvested from the same source marker.
    pub fn find_by_scan_key(&self, key: &str) -> Option<&Task> {
        self.data.tasks.iter().find(|t| t.scan_key() == Some(key))
    }

    pub fn save(&self) -> Result<()> {
        // Only claim the exclude entry when we create the storage directory for
        // the first time. Re-adding it on every save would silently undo the
        // choice of anyone who deleted the line in order to commit their task
        // list, which is a documented escape hatch.
        let first_time = !self.dir.exists();

        std::fs::create_dir_all(&self.dir)
            .with_context(|| format!("creating {}", self.dir.display()))?;

        if first_time
            && let Scope::Repo(repo) = &self.scope
        {
            repo.ensure_excluded();
        }

        let path = self.path();
        // Write-then-rename. A half-written temp file is garbage we can throw
        // away; a half-written tasks.json is your week.
        let tmp = path.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(&self.data).context("serializing task list")?;
        std::fs::write(&tmp, &bytes).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &path).with_context(|| {
            format!(
                "replacing {} (temp file left at {})",
                path.display(),
                tmp.display()
            )
        })?;
        Ok(())
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
fn repair(data: &mut Data, warnings: &mut Vec<String>) {
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
    let parent_of: HashMap<u32, Option<u32>> =
        data.tasks.iter().map(|t| (t.id, t.parent)).collect();
    let mut cyclic = Vec::new();
    for task in &data.tasks {
        let mut seen = HashSet::from([task.id]);
        let mut cur = task.id;
        while let Some(Some(p)) = parent_of.get(&cur).copied() {
            if !seen.insert(p) {
                cyclic.push(task.id);
                break;
            }
            cur = p;
        }
    }
    if !cyclic.is_empty() {
        // Break the cycle at its lowest id: deterministic, and it keeps the
        // largest possible intact subtree.
        let cut = cyclic.iter().copied().min().expect("non-empty");
        for task in &mut data.tasks {
            if task.id == cut {
                task.parent = None;
            }
        }
        warnings.push(format!(
            "parent cycle involving {} — detached #{cut} to break it",
            cyclic
                .iter()
                .map(|id| format!("#{id}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    if data.version < FORMAT_VERSION {
        data.version = FORMAT_VERSION;
    }
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

    fn store_with(tasks: Vec<Task>) -> Store {
        let mut data = Data::default();
        data.next_id = tasks.iter().map(|t| t.id + 1).max().unwrap_or(1);
        data.tasks = tasks;
        Store {
            scope: Scope::Global,
            dir: PathBuf::from("."),
            data,
            warnings: Vec::new(),
            migrated: false,
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
        let mut data = Data::default();
        data.tasks = vec![task(1, Some(3)), task(2, Some(1)), task(3, Some(2))];
        let mut warnings = Vec::new();
        repair(&mut data, &mut warnings);

        assert_eq!(data.tasks.iter().filter(|t| t.parent.is_none()).count(), 1);
        assert!(warnings.iter().any(|w| w.contains("cycle")), "{warnings:?}");

        // And the tree walk now terminates.
        let store = store_with(data.tasks);
        assert_eq!(store.tree(|_| true).len(), 3);
    }

    #[test]
    fn load_detaches_tasks_pointing_at_missing_parents() {
        let mut data = Data::default();
        data.tasks = vec![task(1, Some(99))];
        let mut warnings = Vec::new();
        repair(&mut data, &mut warnings);
        assert_eq!(data.tasks[0].parent, None);
        assert!(warnings.iter().any(|w| w.contains("missing parent")));
    }

    #[test]
    fn load_renumbers_duplicate_ids() {
        let mut data = Data::default();
        data.tasks = vec![task(1, None), task(1, None)];
        let mut warnings = Vec::new();
        repair(&mut data, &mut warnings);
        let ids: HashSet<u32> = data.tasks.iter().map(|t| t.id).collect();
        assert_eq!(ids.len(), 2, "duplicate ids must be resolved");
        assert!(warnings.iter().any(|w| w.contains("duplicate")));
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
