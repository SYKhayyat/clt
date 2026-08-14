//! clt — branch-scoped tasks that live in your repo.

mod cli;
mod git;
mod hooks;
mod journal;
mod lock;
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
use store::{Access, Scope, Store};
use task::{Location, Origin, State, Task};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            // `{:#}` prints the whole anyhow chain on one line, so the cause is
            // visible without a --verbose flag nobody remembers.
            anstream::eprintln!("clt: {e:#}");
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
        // Every handler saves exactly once, so the write lock has done its job
        // by here. Releasing it now keeps hook scripts — which run next, and
        // which can do anything, including block on the network — out of the
        // critical section.
        self.store.release_lock();
        Ok(())
    }

    fn out_json(&self, value: &serde_json::Value) {
        anstream::println!(
            "{}",
            serde_json::to_string_pretty(value).unwrap_or_else(|_| "null".into())
        );
    }
}

/// Whether this invocation will modify the list, and so has to take the
/// cross-process write lock.
///
/// Enumerated explicitly rather than inferred, because getting it wrong in the
/// safe direction costs a few milliseconds of queueing and getting it wrong in
/// the other direction costs somebody a task. Dry runs mutate the in-memory
/// copy and deliberately never save, so they read.
fn access_for(args: &Cli) -> Access {
    match &args.command {
        None | Some(Command::List(_) | Command::Find(_) | Command::Log(_) | Command::Path) => {
            Access::Read
        }
        Some(Command::Scan(a)) if a.dry_run => Access::Read,
        Some(Command::Sync(a)) if a.dry_run => Access::Read,
        _ => Access::Write,
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
    let access = access_for(&args);
    let store = if args.global {
        Store::open_in(Scope::Global, access)?
    } else {
        Store::open(&cwd, access)?
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
        Some(Command::Scope(a)) => cmd_scope(&mut ctx, a),
        Some(Command::Scan(a)) => cmd_scan(&mut ctx, a),
        Some(Command::Sync(a)) => cmd_sync(&mut ctx, a),
        Some(Command::Log(a)) => cmd_log(&mut ctx, a),
        Some(Command::Path) => cmd_path(&ctx),
        Some(Command::Share) => cmd_share(&mut ctx),
        Some(Command::Unshare) => cmd_unshare(&mut ctx),
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
    if args.orphaned {
        return cmd_list_orphaned(ctx, &args);
    }

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

/// Tasks pinned to a branch git no longer has.
///
/// Kept off the default path deliberately: answering it costs a `for-each-ref`,
/// and bare `clt` is run dozens of times a day on a tool whose startup budget
/// is already two process spawns.
fn cmd_list_orphaned(ctx: &mut Ctx, args: &cli::ListArgs) -> Result<()> {
    let Some(repo) = ctx.store.scope.repo() else {
        bail!("--orphaned needs a git repository (there are no branches to compare against)");
    };
    let live = repo.branches();
    if live.is_empty() {
        // An unborn HEAD has no branches yet, and reporting every task as
        // orphaned would be technically true and actively misleading.
        bail!("this repository has no branches yet, so nothing can be orphaned");
    }

    let orphans: Vec<&Task> = ctx
        .store
        .orphaned(&live)
        .into_iter()
        .filter(|t| args.done || !t.is_done())
        .collect();

    if ctx.json {
        ctx.out_json(&serde_json::to_value(&orphans)?);
        return Ok(());
    }

    if orphans.is_empty() {
        anstream::println!("  Every task is on a branch that still exists.");
        return Ok(());
    }

    let ids: std::collections::HashSet<u32> = orphans.iter().map(|t| t.id).collect();
    let rows = ctx.store.tree(|t| ids.contains(&t.id));
    render::tasks(
        &ctx.store,
        &rows,
        &render::ListOpts {
            now: ctx.now,
            show_branch: true,
        },
    );
    anstream::println!();
    anstream::println!(
        "  {} task{} on branches that no longer exist.",
        orphans.len(),
        if orphans.len() == 1 { "" } else { "s" }
    );
    anstream::println!("  Rescue them with `clt scope <ID> --repo` or `--branch <NAME>`.");
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
        anstream::println!("  Nothing matching {:?}.", args.query.join(" "));
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

/// The hook fired when a task reaches `state`.
pub fn state_event(state: State) -> &'static str {
    match state {
        State::Done => "post-done",
        State::Doing => "post-start",
        State::Todo => "post-reopen",
    }
}

/// Moves tasks to `state`, returning what changed and the journal to match.
///
/// The single implementation of what a state transition *means*: the cascade
/// rule, clearing a stale `closed_by`, skipping tasks already in that state,
/// and the journal action it records.
///
/// Everything that closes a task routes through here — `clt done`, `clt edit
/// --state`, and the MCP tools. Each used to carry its own copy, and they had
/// drifted: `clt edit --state done` did not cascade, fired no hook, and
/// journalled as "edit". Three transcriptions of one rule is three chances to
/// get it wrong, so there is now only one.
///
/// Takes the store rather than a `Ctx` so the MCP server, which has no `Ctx`,
/// can call the same code instead of transcribing it a fourth time.
pub fn apply_state(
    store: &mut Store,
    actor: Option<&str>,
    now: DateTime<Utc>,
    ids: &[u32],
    state: State,
) -> (Vec<Task>, Vec<journal::Entry>) {
    let mut touched = Vec::new();
    let mut entries = Vec::new();

    for id in ids {
        // Closing a parent closes its subtree. The inverse (reopen, start)
        // deliberately does not cascade: reopening a parent to add one more
        // subtask should not undo the twelve you already finished.
        let targets = if state == State::Done {
            let mut all = vec![*id];
            all.extend(store.descendants(*id));
            all
        } else {
            vec![*id]
        };

        for target in targets {
            let Some(task) = store.get_mut(target) else {
                continue;
            };
            if task.state == state {
                continue;
            }
            task.state = state;
            task.updated = now;
            if state != State::Done {
                // A reopened task must stop naming the commit that closed it.
                task.closed_by = None;
            }
            let snapshot = task.clone();
            entries.push(
                journal::Entry::new(state.as_str())
                    .actor(actor.map(str::to_owned))
                    .id(target)
                    .detail(snapshot.title.clone())
                    .branch(snapshot.branch.as_deref()),
            );
            touched.push(snapshot);
        }
    }

    (touched, entries)
}

fn cmd_set_state(ctx: &mut Ctx, ids: Vec<u32>, state: State) -> Result<()> {
    // Validate everything before touching anything: a typo in the third id
    // shouldn't leave the first two half-applied.
    for id in &ids {
        ctx.store.require(*id)?;
    }

    let (touched, entries) = apply_state(
        &mut ctx.store,
        ctx.actor.clone().as_deref(),
        ctx.now,
        &ids,
        state,
    );

    if touched.is_empty() {
        // Nothing changed, but --json still owes the caller a document. Printing
        // "Already done." onto stdout here broke every pipeline that closed a
        // task twice, and did it with exit status 0 so nothing noticed.
        if ctx.json {
            ctx.out_json(&serde_json::Value::Array(Vec::new()));
        } else {
            anstream::println!("  Already {state}.");
        }
        return Ok(());
    }

    ctx.commit(entries)?;

    // Hooks fire for the ids you named, not for every task the cascade swept
    // up. Closing a parent with twenty children should not spawn twenty-one
    // processes.
    let event = state_event(state);
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
            anstream::println!("  deleted #{} {}", task.id, task.title);
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

    ctx.store.require(args.id)?;

    // Whether any plain field changed, as opposed to only the state. It decides
    // whether an "edit" belongs in the journal at all: `clt edit 3 --state done`
    // is a close, and recording it as an edit is what made the audit log lie
    // about how a task got closed.
    let edits = args.title.is_some() || args.note.is_some() || location.is_some();

    let now = ctx.now;
    if edits {
        let task = ctx.store.get_mut(args.id).expect("checked above");
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
        task.updated = now;
    }

    let mut entries = Vec::new();
    if edits {
        let task = ctx.store.require(args.id)?;
        entries.push(
            journal::Entry::new("edit")
                .actor(ctx.actor.clone())
                .id(args.id)
                .detail(task.title.clone())
                .branch(task.branch.as_deref()),
        );
    }

    // A state change goes through the same path as `clt done`/`start`/`reopen`,
    // so it cascades, clears `closed_by`, journals under its own verb and fires
    // the matching hook.
    let mut state_changed = Vec::new();
    if let Some(state) = args.state {
        let actor = ctx.actor.clone();
        let (touched, state_entries) =
            apply_state(&mut ctx.store, actor.as_deref(), ctx.now, &[args.id], state);
        entries.extend(state_entries);
        state_changed = touched;
    }

    ctx.commit(entries)?;

    if let Some(state) = args.state {
        for task in state_changed.iter().filter(|t| t.id == args.id) {
            hooks::fire(
                ctx.store.dir(),
                state_event(state),
                task,
                ctx.actor(),
                hooks::Output::Inherit,
            );
        }
    }

    let snapshot = ctx.store.require(args.id)?.clone();
    if ctx.json {
        ctx.out_json(&serde_json::to_value(&snapshot)?);
    } else {
        render::changed(&snapshot);
        // The cascade is invisible in a one-line summary, and silently closing
        // a dozen subtasks is exactly the kind of surprise worth a sentence.
        let swept = state_changed.len().saturating_sub(1);
        if swept > 0 {
            anstream::println!(
                "  {}",
                render::dimmed(&format!(
                    "  and {swept} nested task{}",
                    if swept == 1 { "" } else { "s" }
                ))
            );
        }
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
                anstream::println!("  #{} now under #{p}", args.id);
            }
            None => {
                anstream::println!("  #{} detached to the top level", args.id);
            }
        }
    }
    Ok(())
}

/// Moves tasks between a branch and the whole repo.
///
/// Scope was previously fixed at creation time, which made the branch lifecycle
/// a one-way door: merge a feature branch, delete it, and everything filed
/// there was stranded on a name git no longer knows. This is the way back —
/// promote the leftovers to repo-wide, or re-pin them to the branch the work
/// actually continued on.
fn cmd_scope(ctx: &mut Ctx, args: cli::ScopeArgs) -> Result<()> {
    let branch: Option<String> = if args.target.repo {
        None
    } else if let Some(name) = args.target.branch.clone() {
        Some(name)
    } else {
        // --here. Refuse rather than silently doing what --repo does: on a
        // detached HEAD there is no branch to mean, and quietly making a task
        // repo-wide is not what anyone typing --here asked for.
        Some(
            ctx.store
                .scope
                .branch()
                .map(str::to_owned)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "--here needs a branch, and HEAD is detached (try --branch <NAME> or --repo)"
                    )
                })?,
        )
    };

    // Validate every id before moving anything, so a typo in the third one
    // can't leave the first two re-scoped.
    for id in &args.ids {
        ctx.store.require(*id)?;
    }

    let mut changed = Vec::new();
    let mut entries = Vec::new();
    for id in &args.ids {
        for moved in ctx.store.set_scope(*id, branch.as_deref(), ctx.now)? {
            entries.push(
                journal::Entry::new("scope")
                    .actor(ctx.actor.clone())
                    .id(moved)
                    .detail(match branch.as_deref() {
                        Some(b) => format!("to `{b}`"),
                        None => "to the whole repo".into(),
                    })
                    .branch(branch.as_deref()),
            );
            changed.push(moved);
        }
    }

    ctx.commit(entries)?;

    let moved: Vec<&Task> = changed.iter().filter_map(|id| ctx.store.get(*id)).collect();

    if ctx.json {
        ctx.out_json(&serde_json::to_value(&moved)?);
        return Ok(());
    }

    if moved.is_empty() {
        anstream::println!("  Already scoped to {}.", describe_scope(branch.as_deref()));
        return Ok(());
    }
    for task in &moved {
        anstream::println!(
            "  #{} {} → {}",
            task.id,
            task.title,
            describe_scope(branch.as_deref())
        );
    }
    Ok(())
}

fn describe_scope(branch: Option<&str>) -> String {
    match branch {
        Some(b) => format!("`{b}`"),
        None => "the whole repo".into(),
    }
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
    //
    // Only tasks scanned from *this* branch are eligible. A working tree holds
    // one branch's files, so a marker on `feat/x` is simply absent while you
    // are on `main` — and the sweep used to read that absence as "deleted" and
    // close it. Permanently, since a later scan finds the task already done and
    // leaves it alone. That turned the two headline features into a pair that
    // quietly ate each other's work.
    //
    // The test is an exact branch match rather than `in_scope`, which would also
    // admit repo-wide tasks. The two mistakes are not symmetric: closing a task
    // the user still has to do is invisible and unrecoverable, while leaving one
    // open until they scan from the right branch corrects itself the moment they
    // do.
    let live: std::collections::HashSet<&str> = hits.iter().map(|h| h.key.as_str()).collect();
    let stale: Vec<u32> = ctx
        .store
        .tasks()
        .iter()
        .filter(|t| !t.is_done())
        .filter(|t| t.branch.as_deref() == branch.as_deref())
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
        anstream::println!("  Markers and tasks already agree.");
        return;
    }
    for t in added {
        let loc = t.location.as_ref().map(ToString::to_string).unwrap_or_default();
        anstream::println!("  + #{}  {}  {}", t.id, t.title, loc);
    }
    for t in closed {
        anstream::println!("  ✓ #{}  {}  (marker gone)", t.id, t.title);
    }
    for t in moved {
        let loc = t.location.as_ref().map(ToString::to_string).unwrap_or_default();
        anstream::println!("  → #{}  {}  now at {}", t.id, t.title, loc);
    }
    if dry {
        anstream::println!("  (dry run — nothing written)");
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
            if armed
                && !digits.is_empty()
                && let Ok(id) = digits.parse()
            {
                ids.push(id);
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
            anstream::println!("  Nothing to close in {} commit(s).", commits.len());
        } else {
            for (sha, t) in &closed {
                anstream::println!("  ✓ #{}  {}  ({sha})", t.id, t.title);
            }
            anstream::println!("  (dry run — nothing written)");
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
        anstream::println!("  Nothing to close in {} commit(s).", commits.len());
    } else {
        for (sha, t) in &closed {
            anstream::println!("  ✓ #{}  {}  ({sha})", t.id, t.title);
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
        anstream::println!("  No history yet.");
        return Ok(());
    }

    for e in &entries {
        let who = e.actor.clone().unwrap_or_else(|| "you".into());
        let id = e.id.map(|i| format!("#{i}")).unwrap_or_default();
        anstream::println!(
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
            // Whether the list travels with the repo (`clt share`).
            "shared": ctx.store.is_shared(),
        }));
    } else {
        anstream::println!("{}", ctx.store.path().display());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// share / unshare
// ---------------------------------------------------------------------------

/// Makes the task list committable, so it survives a clone.
///
/// The default is local-only, which is right for a personal list but means the
/// list dies with your working copy: clone the repo elsewhere and your tasks
/// are gone, and a teammate never sees them at all. This is the opt-in.
///
/// It deliberately stops short of touching your index. Staging files is a
/// decision about a commit you are in the middle of composing, and clt has no
/// business making it — so we print the command instead of running it.
fn cmd_share(ctx: &mut Ctx) -> Result<()> {
    let Some(repo) = ctx.store.scope.repo().cloned() else {
        bail!("there is no repo here for the task list to travel with");
    };

    // Persist first: sharing a list that does not exist on disk yet would
    // produce advice to `git add` a file that isn't there.
    ctx.store.save()?;
    ctx.store.release_lock();

    let already = ctx.store.is_shared();
    let unexcluded = repo.unexclude()?;
    ctx.store.write_local_gitignore()?;

    // `info/exclude` is ours to edit; a committed .gitignore is not. If the
    // project ignores .clt/ there, nothing we do here will make git see the
    // file, and saying so beats appearing to succeed.
    let blocked = repo
        .ignore_source(store::REL_TASKS)
        .filter(|src| !src.ends_with("info/exclude"));

    if ctx.json {
        ctx.out_json(&serde_json::json!({
            "shared": already,
            "unexcluded": unexcluded,
            "blocked_by": blocked,
            "path": ctx.store.path(),
        }));
        return Ok(());
    }

    if already {
        anstream::println!("  The task list is already tracked by git.");
        return Ok(());
    }

    if let Some(src) = blocked {
        render::warn(&format!(
            "`{src}` still ignores the task list, and that file belongs to the project, \
             not to clt. Remove its .clt/ entry, then run `clt share` again."
        ));
        return Ok(());
    }

    anstream::println!("  The task list is no longer excluded. To share it:");
    anstream::println!();
    anstream::println!("    git add .clt && git commit -m \"track the clt task list\"");
    anstream::println!();
    for line in [
        "tasks.json and hooks/ travel with the repo; the journal stays local,",
        "per .clt/.gitignore. Expect the occasional merge conflict in tasks.json —",
        "the format is line-oriented and meant to be resolved by hand.",
    ] {
        anstream::println!("  {line}");
    }
    Ok(())
}

/// Puts the task list back to local-only.
fn cmd_unshare(ctx: &mut Ctx) -> Result<()> {
    let Some(repo) = ctx.store.scope.repo().cloned() else {
        bail!("there is no repo here — the global task list is always local");
    };

    let tracked = ctx.store.is_shared();
    repo.ensure_excluded();

    if ctx.json {
        ctx.out_json(&serde_json::json!({
            "shared": false,
            "was_tracked": tracked,
            "path": ctx.store.path(),
        }));
        return Ok(());
    }

    anstream::println!("  New commits will not carry the task list.");
    if tracked {
        // Excluding a file git already tracks does nothing at all — the entry
        // only applies to untracked paths. Without this the command would look
        // like it worked and the list would keep getting committed.
        anstream::println!();
        anstream::println!("  It is still tracked, though. To stop committing it:");
        anstream::println!();
        anstream::println!("    git rm -r --cached .clt");
        anstream::println!();
        anstream::println!("  That leaves your tasks on disk and drops them from the index.");
    }
    Ok(())
}

fn cmd_init(ctx: &Ctx) -> Result<()> {
    let dir = ctx.store.dir();
    let hooks_dir = dir.join("hooks");
    std::fs::create_dir_all(&hooks_dir)
        .with_context(|| format!("creating {}", hooks_dir.display()))?;

    // Neither file is overwritten. `clt init` is safe to re-run, and someone
    // who has annotated the README should keep their notes.
    let sample = hooks_dir.join("post-add.sample");
    if !sample.exists() {
        std::fs::write(&sample, hooks::SAMPLE)
            .with_context(|| format!("writing {}", sample.display()))?;
    }

    // The directory documents itself, so whoever opens it next does not have to
    // already know what clt is to work out what these files are.
    let readme = dir.join("README.md");
    if !readme.exists() {
        std::fs::write(&readme, store::README)
            .with_context(|| format!("writing {}", readme.display()))?;
    }

    ctx.store.save()?;

    if ctx.json {
        ctx.out_json(&serde_json::json!({
            "tasks": ctx.store.path(),
            "dir": dir,
            "hooks": hooks_dir,
            "sample_hook": sample,
            "readme": readme,
            "branch": ctx.store.scope.branch(),
            "shared": ctx.store.is_shared(),
        }));
        return Ok(());
    }

    anstream::println!("  Task list ready at {}", ctx.store.path().display());
    if let Some(branch) = ctx.store.scope.branch() {
        anstream::println!("  Tasks you add now are scoped to `{branch}`.");
    }
    anstream::println!("  Sample hook: {}", sample.display());
    anstream::println!("  What's in .clt/: {}", readme.display());
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
