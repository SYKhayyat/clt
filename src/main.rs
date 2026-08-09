//! clt — branch-scoped tasks that live in your repo.

mod cli;
mod git;
mod hooks;
mod journal;
mod mcp;
mod render;
mod scan;
mod store;
mod task;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Local, TimeZone, Utc};
use clap::Parser;
use std::process::ExitCode;

use cli::{Cli, Command};
use store::{Scope, Store};
use task::{Location, Origin, State, Task};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            // `{:#}` prints the whole anyhow chain on one line, so the cause is
            // visible without a --verbose flag nobody remembers.
            let _ = anstream::eprintln!("clt: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// Everything a handler needs, so signatures stay short.
struct Ctx {
    store: Store,
    actor: Option<String>,
    json: bool,
    now: DateTime<Utc>,
}

impl Ctx {
    fn actor(&self) -> Option<&str> {
        self.actor.as_deref()
    }

    /// Persist, then fire hooks and journal. Order matters: nothing observes a
    /// change until it's durable on disk.
    fn commit(&mut self, entries: Vec<journal::Entry>) -> Result<()> {
        self.store.save()?;
        journal::append(self.store.dir(), &entries);
        Ok(())
    }

    fn out_json(&self, value: &serde_json::Value) {
        let _ = anstream::println!(
            "{}",
            serde_json::to_string_pretty(value).unwrap_or_else(|_| "null".into())
        );
    }
}

fn run() -> Result<()> {
    let args = Cli::parse();

    // The MCP server manages its own storage lifecycle and must never write to
    // stdout, so it forks off before any of the normal setup.
    if matches!(args.command, Some(Command::Mcp)) {
        return mcp::serve(args.global);
    }

    let cwd = std::env::current_dir().context("reading the current directory")?;
    let store = if args.global {
        Store::open_in(Scope::Global)?
    } else {
        Store::open(&cwd)?
    };

    // Repairs and imports are reported once, on stderr, so they never pollute
    // --json on stdout.
    for warning in &store.warnings {
        render::note(warning);
    }

    // A migration must land on disk even when the command itself only reads,
    // or a bare `clt` re-imports the legacy file on every invocation.
    if store.migrated {
        store.save()?;
    }

    let mut ctx = Ctx {
        store,
        actor: args.actor.clone(),
        json: args.json,
        now: Utc::now(),
    };

    match args.command {
        None => cmd_list(&mut ctx, args.list),
        Some(Command::List(a)) => cmd_list(&mut ctx, a),
        Some(Command::Add(a)) => cmd_add(&mut ctx, a),
        Some(Command::Find(a)) => cmd_find(&mut ctx, a),
        Some(Command::Done(a)) => cmd_set_state(&mut ctx, a.ids, State::Done),
        Some(Command::Start(a)) => cmd_set_state(&mut ctx, a.ids, State::Doing),
        Some(Command::Reopen(a)) => cmd_set_state(&mut ctx, a.ids, State::Todo),
        Some(Command::Rm(a)) => cmd_rm(&mut ctx, a),
        Some(Command::Edit(a)) => cmd_edit(&mut ctx, a),
        Some(Command::Move(a)) => cmd_move(&mut ctx, a),
        Some(Command::Scan(a)) => cmd_scan(&mut ctx, a),
        Some(Command::Sync(a)) => cmd_sync(&mut ctx, a),
        Some(Command::Log(a)) => cmd_log(&mut ctx, a),
        Some(Command::Path) => cmd_path(&ctx),
        Some(Command::Init) => cmd_init(&ctx),
        Some(Command::Mcp) => unreachable!("handled above"),
    }
}

// ---------------------------------------------------------------------------
// list / find
// ---------------------------------------------------------------------------

/// Midnight local time, as UTC. Used to decide which finished tasks still
/// deserve screen space.
fn start_of_today(now: DateTime<Utc>) -> DateTime<Utc> {
    let local = now.with_timezone(&Local).date_naive();
    Local
        .from_local_datetime(&local.and_hms_opt(0, 0, 0).expect("midnight exists"))
        .earliest()
        .map(|dt| dt.with_timezone(&Utc))
        // A DST transition can make local midnight ambiguous or nonexistent.
        // Falling back an hour is wrong by an hour, twice a year, in one
        // column of a task list. Panicking would be worse.
        .unwrap_or(now - chrono::Duration::hours(24))
}

fn cmd_list(ctx: &mut Ctx, args: cli::ListArgs) -> Result<()> {
    let branch = args
        .branch
        .clone()
        .or_else(|| ctx.store.scope.branch().map(str::to_owned));
    let today = start_of_today(ctx.now);

    let in_scope = |t: &Task| args.all || Store::in_scope(t, branch.as_deref());

    // Done tasks stay visible for the rest of the day you closed them — you get
    // to see what you finished — then drop out so the list stays short.
    let state_ok = |t: &Task| match args.state {
        Some(s) => t.state == s,
        None if args.done => true,
        None => !t.is_done() || t.updated >= today,
    };

    let rows = ctx.store.tree(|t| in_scope(t) && state_ok(t));

    if ctx.json {
        let payload: Vec<serde_json::Value> = rows
            .iter()
            .map(|r| {
                let mut v = serde_json::to_value(r.task).unwrap_or(serde_json::Value::Null);
                if let Some(obj) = v.as_object_mut() {
                    obj.insert("depth".into(), r.depth.into());
                    obj.insert("context".into(), r.context.into());
                }
                v
            })
            .collect();
        ctx.out_json(&serde_json::Value::Array(payload));
        return Ok(());
    }

    if rows.is_empty() {
        render::empty(branch.as_deref(), args.state.is_some());
        return Ok(());
    }

    render::tasks(
        &ctx.store,
        &rows,
        &render::ListOpts {
            now: ctx.now,
            show_branch: args.all,
        },
    );

    let shown: Vec<&Task> = rows.iter().map(|r| r.task).collect();
    let open = shown.iter().filter(|t| t.state == State::Todo).count();
    let doing = shown.iter().filter(|t| t.state == State::Doing).count();
    let hidden_done = ctx
        .store
        .tasks()
        .iter()
        .filter(|t| in_scope(t) && t.is_done() && !state_ok(t))
        .count();
    render::summary(open, doing, hidden_done);
    Ok(())
}

fn cmd_find(ctx: &mut Ctx, args: cli::FindArgs) -> Result<()> {
    let needle = args.query.join(" ").to_lowercase();
    let branch = ctx.store.scope.branch().map(str::to_owned);

    // Search spans every branch by default: needing to search is precisely the
    // situation where you've forgotten which branch you filed it on.
    let hit = |t: &Task| {
        let haystacks = [
            Some(t.title.to_lowercase()),
            t.note.as_ref().map(|n| n.to_lowercase()),
            t.location.as_ref().map(|l| l.to_string().to_lowercase()),
            t.branch.as_ref().map(|b| b.to_lowercase()),
        ];
        haystacks
            .iter()
            .flatten()
            .any(|h| h.contains(needle.as_str()))
    };

    let rows = ctx
        .store
        .tree(|t| hit(t) && (!args.here || Store::in_scope(t, branch.as_deref())));

    if ctx.json {
        let payload: Vec<&Task> = rows.iter().filter(|r| !r.context).map(|r| r.task).collect();
        ctx.out_json(&serde_json::to_value(payload)?);
        return Ok(());
    }

    if rows.is_empty() {
        let _ = anstream::println!("  Nothing matching {:?}.", args.query.join(" "));
        return Ok(());
    }

    render::tasks(
        &ctx.store,
        &rows,
        &render::ListOpts {
            now: ctx.now,
            show_branch: true,
        },
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// mutations
// ---------------------------------------------------------------------------

fn cmd_add(ctx: &mut Ctx, args: cli::AddArgs) -> Result<()> {
    let title = args.title.join(" ").trim().to_string();
    if title.is_empty() {
        bail!("a task needs a title");
    }

    // A subtask lives on its parent's branch, full stop. Letting the two differ
    // would make a tree half-visible in any given view.
    let branch = match args.parent {
        Some(pid) => {
            let parent = ctx.store.require(pid)?;
            if args.repo && parent.branch.is_some() {
                bail!(
                    "--repo conflicts with --parent {pid}, which is scoped to `{}`\n\
                     (subtasks inherit their parent's branch)",
                    parent.branch.as_deref().unwrap_or("?")
                );
            }
            parent.branch.clone()
        }
        None if args.repo => None,
        None => ctx.store.scope.branch().map(str::to_owned),
    };

    let location = args
        .file
        .as_deref()
        .map(str::parse::<Location>)
        .transpose()
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let id = ctx.store.reserve_id();
    let mut t = Task::new(id, title, ctx.now);
    t.note = args.note;
    t.parent = args.parent;
    t.branch = branch;
    t.location = location;
    t.actor = ctx.actor.clone();
    t.state = if args.start { State::Doing } else { State::Todo };
    ctx.store.insert(t.clone());

    let entry = journal::Entry::new("add")
        .actor(ctx.actor.clone())
        .id(id)
        .detail(t.title.clone())
        .branch(t.branch.as_deref());
    ctx.commit(vec![entry])?;

    hooks::fire(ctx.store.dir(), "post-add", &t, ctx.actor(), hooks::Output::Inherit);

    if ctx.json {
        ctx.out_json(&serde_json::to_value(&t)?);
    } else {
        render::changed(&t);
    }
    Ok(())
}

fn cmd_set_state(ctx: &mut Ctx, ids: Vec<u32>, state: State) -> Result<()> {
    // Validate everything before touching anything: a typo in the third id
    // shouldn't leave the first two half-applied.
    for id in &ids {
        ctx.store.require(*id)?;
    }

    let mut touched = Vec::new();
    let mut entries = Vec::new();

    for id in &ids {
        // Closing a parent closes its subtree. The inverse (reopen, start)
        // deliberately does not cascade: reopening a parent to add one more
        // subtask should not undo the twelve you already finished.
        let targets = if state == State::Done {
            let mut all = vec![*id];
            all.extend(ctx.store.descendants(*id));
            all
        } else {
            vec![*id]
        };

        for target in targets {
            let now = ctx.now;
            let Some(task) = ctx.store.get_mut(target) else {
                continue;
            };
            if task.state == state {
                continue;
            }
            task.state = state;
            task.updated = now;
            if state != State::Done {
                task.closed_by = None;
            }
            let snapshot = task.clone();
            entries.push(
                journal::Entry::new(state.as_str())
                    .actor(ctx.actor.clone())
                    .id(target)
                    .detail(snapshot.title.clone())
                    .branch(snapshot.branch.as_deref()),
            );
            touched.push(snapshot);
        }
    }

    if touched.is_empty() {
        let _ = anstream::println!("  Already {state}.");
        return Ok(());
    }

    ctx.commit(entries)?;

    // Hooks fire for the ids you named, not for every task the cascade swept
    // up. Closing a parent with twenty children should not spawn twenty-one
    // processes.
    let event = match state {
        State::Done => "post-done",
        State::Doing => "post-start",
        State::Todo => "post-reopen",
    };
    for task in touched.iter().filter(|t| ids.contains(&t.id)) {
        hooks::fire(ctx.store.dir(), event, task, ctx.actor(), hooks::Output::Inherit);
    }

    if ctx.json {
        ctx.out_json(&serde_json::to_value(&touched)?);
    } else {
        for task in &touched {
            render::changed(task);
        }
    }
    Ok(())
}

fn cmd_rm(ctx: &mut Ctx, args: cli::RmArgs) -> Result<()> {
    for id in &args.ids {
        ctx.store.require(*id)?;
        let kids = ctx.store.descendants(*id).len();
        if kids > 0 && !args.recursive {
            bail!(
                "#{id} has {kids} subtask{} — pass -r to delete them too, \
                 or `clt move` them out first",
                if kids == 1 { "" } else { "s" }
            );
        }
    }

    let mut removed = Vec::new();
    let mut entries = Vec::new();
    for id in &args.ids {
        let Some(task) = ctx.store.get(*id).cloned() else {
            continue; // already swept up by an earlier subtree removal
        };
        for gone in ctx.store.remove_subtree(*id) {
            entries.push(
                journal::Entry::new("rm")
                    .actor(ctx.actor.clone())
                    .id(gone)
                    .branch(task.branch.as_deref()),
            );
        }
        removed.push(task);
    }

    ctx.commit(entries)?;
    for task in &removed {
        hooks::fire(ctx.store.dir(), "post-rm", task, ctx.actor(), hooks::Output::Inherit);
    }

    if ctx.json {
        ctx.out_json(&serde_json::to_value(&removed)?);
    } else {
        for task in &removed {
            let _ = anstream::println!("  deleted #{} {}", task.id, task.title);
        }
    }
    Ok(())
}

fn cmd_edit(ctx: &mut Ctx, args: cli::EditArgs) -> Result<()> {
    if args.title.is_none()
        && args.note.is_none()
        && args.file.is_none()
        && args.state.is_none()
    {
        bail!("nothing to change (try --title, --note, --file or --state)");
    }

    let location = args
        .file
        .as_deref()
        .map(str::parse::<Location>)
        .transpose()
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let now = ctx.now;
    let task = ctx
        .store
        .get_mut(args.id)
        .ok_or_else(|| anyhow::anyhow!("no task #{}", args.id))?;

    if let Some(title) = args.title {
        task.title = title;
        // Editing a scanned task's title severs it from its source marker;
        // otherwise the next scan would "helpfully" revert your edit.
        if matches!(task.origin, Origin::Scan { .. }) {
            task.origin = Origin::Manual;
        }
    }
    if let Some(note) = args.note {
        task.note = (!note.trim().is_empty()).then_some(note);
    }
    if let Some(loc) = location {
        task.location = Some(loc);
    }
    if let Some(state) = args.state {
        task.state = state;
    }
    task.updated = now;
    let snapshot = task.clone();

    ctx.commit(vec![
        journal::Entry::new("edit")
            .actor(ctx.actor.clone())
            .id(args.id)
            .detail(snapshot.title.clone())
            .branch(snapshot.branch.as_deref()),
    ])?;

    if ctx.json {
        ctx.out_json(&serde_json::to_value(&snapshot)?);
    } else {
        render::changed(&snapshot);
    }
    Ok(())
}

fn cmd_move(ctx: &mut Ctx, args: cli::MoveArgs) -> Result<()> {
    if args.under.is_none() && !args.root {
        bail!("say where: --under <ID> to nest, or --root to detach");
    }
    let parent = if args.root { None } else { args.under };
    ctx.store.reparent(args.id, parent, ctx.now)?;

    let snapshot = ctx.store.require(args.id)?.clone();
    ctx.commit(vec![
        journal::Entry::new("move")
            .actor(ctx.actor.clone())
            .id(args.id)
            .detail(match parent {
                Some(p) => format!("under #{p}"),
                None => "to top level".into(),
            })
            .branch(snapshot.branch.as_deref()),
    ])?;

    if ctx.json {
        ctx.out_json(&serde_json::to_value(&snapshot)?);
    } else {
        match parent {
            Some(p) => {
                let _ = anstream::println!("  #{} now under #{p}", args.id);
            }
            None => {
                let _ = anstream::println!("  #{} detached to the top level", args.id);
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// scan / sync
// ---------------------------------------------------------------------------

fn cmd_scan(ctx: &mut Ctx, args: cli::ScanArgs) -> Result<()> {
    let Some(repo) = ctx.store.scope.repo().cloned() else {
        bail!("clt scan needs a git repository (it reads the working tree via git ls-files)");
    };

    let ignores = scan::load_ignores(&repo.root);
    let hits: Vec<scan::Hit> = scan::scan(&repo)?
        .into_iter()
        .filter(|h| !scan::is_own_storage(&h.file))
        .filter(|h| !scan::is_ignored(&h.file, &ignores))
        .collect();

    let branch = ctx.store.scope.branch().map(str::to_owned);
    let mut added = Vec::new();
    let mut moved = Vec::new();
    let mut entries = Vec::new();

    for hit in &hits {
        let location = Location::new(&hit.file, Some(hit.line));
        match ctx.store.find_by_scan_key(&hit.key).map(|t| t.id) {
            Some(id) => {
                let now = ctx.now;
                let task = ctx.store.get_mut(id).expect("just found");
                if task.location.as_ref() != Some(&location) {
                    task.location = Some(location);
                    task.updated = now;
                    moved.push(task.clone());
                }
            }
            None => {
                let id = ctx.store.reserve_id();
                let mut t = Task::new(id, hit.title.clone(), ctx.now);
                t.location = Some(location);
                t.branch = branch.clone();
                t.origin = Origin::Scan {
                    key: hit.key.clone(),
                };
                t.actor = Some("scan".into());
                ctx.store.insert(t.clone());
                entries.push(
                    journal::Entry::new("scan")
                        .actor(ctx.actor.clone())
                        .id(id)
                        .detail(format!("+ {}", t.title))
                        .branch(t.branch.as_deref()),
                );
                added.push(t);
            }
        }
    }

    // Markers that vanished: the comment was deleted, so the work is done.
    let live: std::collections::HashSet<&str> = hits.iter().map(|h| h.key.as_str()).collect();
    let stale: Vec<u32> = ctx
        .store
        .tasks()
        .iter()
        .filter(|t| !t.is_done())
        .filter(|t| t.scan_key().is_some_and(|k| !live.contains(k)))
        .map(|t| t.id)
        .collect();

    let mut closed = Vec::new();
    for id in stale {
        let now = ctx.now;
        let task = ctx.store.get_mut(id).expect("just listed");
        task.state = State::Done;
        task.updated = now;
        let snapshot = task.clone();
        entries.push(
            journal::Entry::new("scan")
                .actor(ctx.actor.clone())
                .id(id)
                .detail(format!("closed (marker gone) {}", snapshot.title))
                .branch(snapshot.branch.as_deref()),
        );
        closed.push(snapshot);
    }

    if args.dry_run {
        if ctx.json {
            ctx.out_json(&serde_json::json!({
                "dry_run": true,
                "added": added,
                "closed": closed,
                "moved": moved,
            }));
        } else {
            report_scan(&added, &closed, &moved, true);
        }
        return Ok(());
    }

    ctx.commit(entries)?;
    for task in &added {
        hooks::fire(ctx.store.dir(), "post-add", task, Some("scan"), hooks::Output::Inherit);
    }

    if ctx.json {
        ctx.out_json(&serde_json::json!({
            "dry_run": false,
            "added": added,
            "closed": closed,
            "moved": moved,
        }));
    } else {
        report_scan(&added, &closed, &moved, false);
    }
    Ok(())
}

fn report_scan(added: &[Task], closed: &[Task], moved: &[Task], dry: bool) {
    if added.is_empty() && closed.is_empty() && moved.is_empty() {
        let _ = anstream::println!("  Markers and tasks already agree.");
        return;
    }
    for t in added {
        let loc = t.location.as_ref().map(ToString::to_string).unwrap_or_default();
        let _ = anstream::println!("  + #{}  {}  {}", t.id, t.title, loc);
    }
    for t in closed {
        let _ = anstream::println!("  ✓ #{}  {}  (marker gone)", t.id, t.title);
    }
    for t in moved {
        let loc = t.location.as_ref().map(ToString::to_string).unwrap_or_default();
        let _ = anstream::println!("  → #{}  {}  now at {}", t.id, t.title, loc);
    }
    if dry {
        let _ = anstream::println!("  (dry run — nothing written)");
    }
}

/// Closing keywords recognised in commit messages, matching the vocabulary
/// GitHub trained everyone on.
const CLOSING_WORDS: &[&str] = &[
    "close", "closes", "closed", "fix", "fixes", "fixed", "resolve", "resolves", "resolved",
];

/// Extracts ids from `closes clt#3, clt#4` style references.
fn closing_refs(message: &str) -> Vec<u32> {
    let tokens: Vec<String> = message
        .split_whitespace()
        .map(|t| t.to_lowercase())
        .collect();
    let mut ids = Vec::new();
    let mut armed = false;

    for token in &tokens {
        let bare = token.trim_matches(|c: char| !c.is_alphanumeric() && c != '#');
        if CLOSING_WORDS.contains(&bare) {
            armed = true;
            continue;
        }
        if let Some(rest) = bare.strip_prefix("clt#") {
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            if armed && !digits.is_empty() {
                if let Ok(id) = digits.parse() {
                    ids.push(id);
                }
            }
            // Stay armed: "closes clt#3 clt#4" should close both.
            continue;
        }
        // Any other word breaks the association, so "closes the bug in clt#3"
        // doesn't silently close #3.
        armed = false;
    }
    ids
}

fn cmd_sync(ctx: &mut Ctx, args: cli::SyncArgs) -> Result<()> {
    let Some(repo) = ctx.store.scope.repo().cloned() else {
        bail!("clt sync needs a git repository");
    };

    let since = if args.rescan {
        None
    } else {
        ctx.store.last_commit().map(str::to_owned)
    };
    let commits = repo.commits_since(since.as_deref())?;

    let mut closed = Vec::new();
    let mut entries = Vec::new();

    for commit in &commits {
        for id in closing_refs(&commit.message) {
            let now = ctx.now;
            let Some(task) = ctx.store.get_mut(id) else {
                continue; // reference to a task that no longer exists
            };
            if task.is_done() {
                continue;
            }
            task.state = State::Done;
            task.closed_by = Some(commit.sha.clone());
            task.updated = now;
            let snapshot = task.clone();
            entries.push(
                journal::Entry::new("sync")
                    .actor(ctx.actor.clone())
                    .id(id)
                    .detail(format!("closed by {}", commit.short()))
                    .branch(snapshot.branch.as_deref()),
            );
            closed.push((commit.short().to_string(), snapshot));
        }
    }

    if args.dry_run {
        if ctx.json {
            let payload: Vec<&Task> = closed.iter().map(|(_, t)| t).collect();
            ctx.out_json(&serde_json::json!({ "dry_run": true, "closed": payload }));
        } else if closed.is_empty() {
            let _ = anstream::println!("  Nothing to close in {} commit(s).", commits.len());
        } else {
            for (sha, t) in &closed {
                let _ = anstream::println!("  ✓ #{}  {}  ({sha})", t.id, t.title);
            }
            let _ = anstream::println!("  (dry run — nothing written)");
        }
        return Ok(());
    }

    // Advance the watermark even when nothing closed, so the next sync doesn't
    // re-walk history we've already read.
    ctx.store.set_last_commit(repo.head()?);
    ctx.commit(entries)?;

    for (_, task) in &closed {
        hooks::fire(ctx.store.dir(), "post-done", task, ctx.actor(), hooks::Output::Inherit);
    }

    if ctx.json {
        let payload: Vec<&Task> = closed.iter().map(|(_, t)| t).collect();
        ctx.out_json(&serde_json::json!({ "dry_run": false, "closed": payload }));
    } else if closed.is_empty() {
        let _ = anstream::println!("  Nothing to close in {} commit(s).", commits.len());
    } else {
        for (sha, t) in &closed {
            let _ = anstream::println!("  ✓ #{}  {}  ({sha})", t.id, t.title);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// log / path / init
// ---------------------------------------------------------------------------

fn cmd_log(ctx: &mut Ctx, args: cli::LogArgs) -> Result<()> {
    let entries = journal::tail(ctx.store.dir(), args.limit);

    if ctx.json {
        ctx.out_json(&serde_json::to_value(&entries)?);
        return Ok(());
    }

    if entries.is_empty() {
        let _ = anstream::println!("  No history yet.");
        return Ok(());
    }

    for e in &entries {
        let who = e.actor.clone().unwrap_or_else(|| "you".into());
        let id = e.id.map(|i| format!("#{i}")).unwrap_or_default();
        let _ = anstream::println!(
            "  {}  {:<8} {:<7} {:<5} {}",
            render::stamp(e.ts),
            who,
            e.action,
            id,
            e.detail.clone().unwrap_or_default()
        );
    }
    Ok(())
}

fn cmd_path(ctx: &Ctx) -> Result<()> {
    if ctx.json {
        ctx.out_json(&serde_json::json!({
            "tasks": ctx.store.path(),
            "dir": ctx.store.dir(),
            "branch": ctx.store.scope.branch(),
            "scoped": ctx.store.scope.repo().is_some(),
        }));
    } else {
        let _ = anstream::println!("{}", ctx.store.path().display());
    }
    Ok(())
}

fn cmd_init(ctx: &Ctx) -> Result<()> {
    let dir = ctx.store.dir();
    let hooks_dir = dir.join("hooks");
    std::fs::create_dir_all(&hooks_dir)
        .with_context(|| format!("creating {}", hooks_dir.display()))?;

    let sample = hooks_dir.join("post-add.sample");
    if !sample.exists() {
        std::fs::write(&sample, hooks::SAMPLE)?;
    }

    ctx.store.save()?;

    let _ = anstream::println!("  Task list ready at {}", ctx.store.path().display());
    if let Some(branch) = ctx.store.scope.branch() {
        let _ = anstream::println!("  Tasks you add now are scoped to `{branch}`.");
    }
    let _ = anstream::println!("  Sample hook: {}", sample.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closing_refs_reads_the_github_vocabulary() {
        assert_eq!(closing_refs("fix the race\n\ncloses clt#3"), vec![3]);
        assert_eq!(closing_refs("resolves clt#12"), vec![12]);
        assert_eq!(closing_refs("Fixed clt#7."), vec![7]);
    }

    #[test]
    fn closing_refs_takes_several_at_once() {
        assert_eq!(closing_refs("closes clt#3 clt#4"), vec![3, 4]);
        assert_eq!(closing_refs("closes clt#3, clt#4"), vec![3, 4]);
    }

    #[test]
    fn bare_references_do_not_close_anything() {
        // Mentioning a task is not the same as finishing it.
        assert!(closing_refs("see clt#3 for context").is_empty());
        assert!(closing_refs("clt#3").is_empty());
    }

    #[test]
    fn an_intervening_word_breaks_the_association() {
        assert!(closing_refs("closes the bug described in clt#3").is_empty());
    }

    #[test]
    fn other_projects_hash_refs_are_ignored() {
        assert!(closing_refs("closes #3").is_empty());
        assert!(closing_refs("closes GH-3").is_empty());
    }
}
