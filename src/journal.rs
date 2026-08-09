//! Append-only audit log at `.clt/log.jsonl`.
//!
//! This exists because the headline feature is that something *other than you*
//! writes to your task list. The moment an agent can close tasks unsupervised,
//! "what changed, when, and who did it" stops being a nice-to-have.
//!
//! JSONL rather than JSON: appending a line is one syscall and can't corrupt
//! the records already on disk, which matters more here than being able to
//! parse the whole file with one `serde_json::from_str`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::Path;

pub const FILE: &str = "log.jsonl";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub ts: DateTime<Utc>,
    /// `None` means you, at a terminal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    /// `add`, `done`, `start`, `reopen`, `rm`, `edit`, `move`, `scan`, `sync`.
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
}

impl Entry {
    pub fn new(action: impl Into<String>) -> Self {
        Self {
            ts: Utc::now(),
            actor: None,
            action: action.into(),
            id: None,
            detail: None,
            branch: None,
        }
    }

    pub fn actor(mut self, actor: Option<String>) -> Self {
        self.actor = actor;
        self
    }

    pub fn id(mut self, id: u32) -> Self {
        self.id = Some(id);
        self
    }

    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub fn branch(mut self, branch: Option<&str>) -> Self {
        self.branch = branch.map(str::to_owned);
        self
    }
}

/// Appends entries to the journal.
///
/// Deliberately does not return `Result`: a failed write here must never abort
/// the command that succeeded. But it isn't silent either — an audit log that
/// quietly stops recording is worse than none, so failures go to stderr.
pub fn append(dir: &Path, entries: &[Entry]) {
    if entries.is_empty() {
        return;
    }
    if let Err(e) = try_append(dir, entries) {
        crate::render::warn(&format!("could not write {}: {e}", dir.join(FILE).display()));
    }
}

fn try_append(dir: &Path, entries: &[Entry]) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(FILE))?;

    let mut buf = String::new();
    for entry in entries {
        match serde_json::to_string(entry) {
            Ok(line) => {
                buf.push_str(&line);
                buf.push('\n');
            }
            Err(e) => return Err(std::io::Error::other(e)),
        }
    }
    // One write for the batch: on every platform we target, a single small
    // append to a file opened O_APPEND won't interleave with a concurrent clt.
    file.write_all(buf.as_bytes())
}

/// Reads the last `limit` entries, oldest first.
///
/// Skips unparseable lines instead of failing: a truncated final line from an
/// interrupted write shouldn't make `clt log` useless.
pub fn tail(dir: &Path, limit: usize) -> Vec<Entry> {
    let Ok(raw) = std::fs::read_to_string(dir.join(FILE)) else {
        return Vec::new();
    };
    let mut entries: Vec<Entry> = raw
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    if entries.len() > limit {
        entries.drain(..entries.len() - limit);
    }
    entries
}
