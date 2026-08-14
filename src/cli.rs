//! Command-line surface.
//!
//! Two rules shape everything here. First, the verbs you use twenty times a day
//! (`clt`, `clt add`, `clt done`) are short and take no mandatory flags.
//! Second, every read command can emit `--json`, because that — not a plugin
//! API — is how other tools and agents are meant to build on this.

use clap::{Args, Parser, Subcommand};

use crate::task::State;

#[derive(Debug, Parser)]
#[command(
    name = "clt",
    version,
    about = "Branch-scoped tasks that live in your repo",
    long_about = "clt keeps a task list in .clt/tasks.json inside the current repo.\n\
                  Tasks are scoped to the branch you filed them on, so switching\n\
                  branches switches your list. Your coding agent can read and write\n\
                  the same list via `clt --json` or `clt mcp`.",
    // With no subcommand, `clt` lists. That's the command you actually run.
    args_conflicts_with_subcommands = true,
    subcommand_negates_reqs = true
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Flags for the implicit `ls` when no subcommand is given.
    #[command(flatten)]
    pub list: ListArgs,

    /// Who is making this change. Renders as a tag and lands in the journal.
    ///
    /// Set `CLT_ACTOR=claude` in an agent's environment and everything it files
    /// is attributed automatically.
    #[arg(long, short = 'A', global = true, env = "CLT_ACTOR")]
    pub actor: Option<String>,

    /// Emit machine-readable JSON instead of a table.
    #[arg(long, global = true)]
    pub json: bool,

    /// Operate on the global list even inside a repo.
    #[arg(long, global = true)]
    pub global: bool,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// File a new task on the current branch
    #[command(visible_alias = "a")]
    Add(AddArgs),

    /// List tasks (the default when you run bare `clt`)
    #[command(visible_alias = "ls")]
    List(ListArgs),

    /// Search every branch for a task
    #[command(visible_alias = "f")]
    Find(FindArgs),

    /// Mark tasks done, along with everything nested under them
    #[command(visible_alias = "d")]
    Done(IdsArgs),

    /// Mark tasks in progress
    Start(IdsArgs),

    /// Move tasks back to todo
    Reopen(IdsArgs),

    /// Delete tasks permanently
    #[command(visible_alias = "remove")]
    Rm(RmArgs),

    /// Change a task's title, note, location or state
    Edit(EditArgs),

    /// Re-nest a task under another one, or detach it to the top level
    #[command(name = "move", visible_alias = "mv")]
    Move(MoveArgs),

    /// Move tasks between a branch and the whole repo
    Scope(ScopeArgs),

    /// Harvest `TODO(clt):` markers from the source into tasks
    Scan(ScanArgs),

    /// Close tasks referenced by new commit messages ("closes clt#3")
    Sync(SyncArgs),

    /// Show the audit log of who changed what
    Log(LogArgs),

    /// Print where the task list is stored
    Path,

    /// Commit the task list with the repo, so it survives a clone
    Share,

    /// Go back to a local-only task list that is never committed
    Unshare,

    /// Create .clt/ with sample hooks and a README
    Init,

    /// Serve the task list to an agent over MCP (stdio)
    Mcp,
}

#[derive(Debug, Args)]
pub struct AddArgs {
    /// Task title. Quoting is optional — trailing words are joined.
    #[arg(required = true, num_args = 1..)]
    pub title: Vec<String>,

    /// Longer description
    #[arg(long, short)]
    pub note: Option<String>,

    /// Source location this is about, as `path` or `path:line`
    #[arg(long, short = 'f', value_name = "FILE[:LINE]")]
    pub file: Option<String>,

    /// Nest under an existing task
    #[arg(long, short, value_name = "ID")]
    pub parent: Option<u32>,

    /// Visible from every branch, not just this one
    #[arg(long)]
    pub repo: bool,

    /// Create it already in progress
    #[arg(long)]
    pub start: bool,
}

#[derive(Debug, Args, Clone, Default)]
pub struct ListArgs {
    /// Every branch, not just the current one
    #[arg(long, short)]
    pub all: bool,

    /// Include done tasks older than today
    #[arg(long)]
    pub done: bool,

    /// Only this state
    #[arg(long, short, value_name = "STATE", value_parser = parse_state)]
    pub state: Option<State>,

    /// A specific branch instead of the current one
    #[arg(long, short, value_name = "BRANCH", conflicts_with = "all")]
    pub branch: Option<String>,

    /// Only tasks pinned to a branch that no longer exists
    #[arg(long, conflicts_with_all = ["all", "branch"])]
    pub orphaned: bool,
}

#[derive(Debug, Args)]
pub struct FindArgs {
    /// Text to look for in titles, notes and file paths
    #[arg(required = true, num_args = 1..)]
    pub query: Vec<String>,

    /// Restrict to the current branch (search spans every branch by default)
    #[arg(long)]
    pub here: bool,
}

#[derive(Debug, Args)]
pub struct IdsArgs {
    /// One or more task ids
    #[arg(required = true, num_args = 1..)]
    pub ids: Vec<u32>,
}

#[derive(Debug, Args)]
pub struct RmArgs {
    #[arg(required = true, num_args = 1..)]
    pub ids: Vec<u32>,

    /// Also delete everything nested underneath. Required when a task has
    /// subtasks — deleting a subtree by accident is not recoverable.
    #[arg(long, short)]
    pub recursive: bool,
}

#[derive(Debug, Args)]
pub struct EditArgs {
    pub id: u32,

    #[arg(long, short)]
    pub title: Option<String>,

    #[arg(long, short)]
    pub note: Option<String>,

    #[arg(long, short = 'f', value_name = "FILE[:LINE]")]
    pub file: Option<String>,

    #[arg(long, short, value_parser = parse_state)]
    pub state: Option<State>,
}

#[derive(Debug, Args)]
pub struct MoveArgs {
    pub id: u32,

    /// New parent
    #[arg(long, short, value_name = "ID", conflicts_with = "root")]
    pub under: Option<u32>,

    /// Detach to the top level
    #[arg(long)]
    pub root: bool,
}

/// Where a task lives: one branch, or the whole repo.
///
/// Exactly one of these is required. Left to default, `clt scope 3` would be a
/// coin flip between two irreversible-looking outcomes, and the error clap
/// generates for a missing group is better than any guess.
#[derive(Debug, Args)]
#[group(required = true, multiple = false)]
pub struct ScopeTarget {
    /// Visible from every branch
    #[arg(long)]
    pub repo: bool,

    /// A named branch
    #[arg(long, value_name = "BRANCH")]
    pub branch: Option<String>,

    /// The branch you are on right now
    #[arg(long)]
    pub here: bool,
}

#[derive(Debug, Args)]
pub struct ScopeArgs {
    /// One or more task ids. Subtasks follow their parent, so name the root.
    #[arg(required = true, num_args = 1..)]
    pub ids: Vec<u32>,

    #[command(flatten)]
    pub target: ScopeTarget,
}

#[derive(Debug, Args)]
pub struct ScanArgs {
    /// Report what would change without writing anything
    #[arg(long, short = 'n')]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct SyncArgs {
    /// Re-read the last 50 commits instead of only what is new since the last sync
    #[arg(long)]
    pub rescan: bool,

    #[arg(long, short = 'n')]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct LogArgs {
    /// How many entries to show
    #[arg(long, short = 'n', default_value_t = 20)]
    pub limit: usize,
}

fn parse_state(s: &str) -> Result<State, String> {
    s.parse()
}
