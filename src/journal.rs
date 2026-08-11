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

/// The single previous generation, kept so rotation trims the log without
/// throwing history away outright.
pub const ROTATED: &str = "log.jsonl.1";

/// Rotate once the live log passes this.
///
/// Roughly ten thousand entries — years of use for one person, months for a
/// repo with an enthusiastic agent in it. Two generations bounds the directory
/// at about 2 MB, which is small enough not to care about and large enough that
/// nobody loses history they were actually going to read.
const MAX_BYTES: u64 = 1024 * 1024;

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
    rotate_if_large(dir);
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

/// Moves the live log aside once it gets big, replacing the older generation.
///
/// Best-effort on purpose: a rotation that fails means the log stays large,
/// which is a great deal better than an audit entry going unrecorded because
/// housekeeping got in the way.
fn rotate_if_large(dir: &Path) {
    let path = dir.join(FILE);
    let Ok(meta) = std::fs::metadata(&path) else {
        return; // no log yet
    };
    if meta.len() < MAX_BYTES {
        return;
    }
    // `rename` replaces the previous generation on every platform we target.
    let _ = std::fs::rename(&path, dir.join(ROTATED));
}

/// Reads the last `limit` entries, oldest first.
///
/// Skips unparseable lines instead of failing: a truncated final line from an
/// interrupted write shouldn't make `clt log` useless.
pub fn tail(dir: &Path, limit: usize) -> Vec<Entry> {
    let mut entries = read_entries(&dir.join(FILE));

    // Reach back into the previous generation only when the live log cannot
    // satisfy the request on its own, so the common `clt log` stays a single
    // read of a file that was just capped in size.
    if entries.len() < limit {
        let mut older = read_entries(&dir.join(ROTATED));
        if !older.is_empty() {
            let want = limit - entries.len();
            if older.len() > want {
                older.drain(..older.len() - want);
            }
            older.append(&mut entries);
            entries = older;
        }
    }

    if entries.len() > limit {
        entries.drain(..entries.len() - limit);
    }
    entries
}

fn read_entries(path: &Path) -> Vec<Entry> {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    raw.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("clt-journal-{}-{name}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn entry(n: u32) -> Entry {
        Entry::new("add").id(n).detail(format!("task {n}"))
    }

    #[test]
    fn a_large_log_rotates_on_the_next_write() {
        let dir = scratch("rotate");
        // Stand in for a log that has been accumulating for a year.
        let filler = std::iter::repeat_n(
            r#"{"ts":"2026-01-01T00:00:00Z","action":"add","id":1}"#,
            (MAX_BYTES / 50) as usize + 10,
        )
        .collect::<Vec<_>>()
        .join("\n");
        std::fs::write(dir.join(FILE), filler).unwrap();

        append(&dir, &[entry(9999)]);

        assert!(dir.join(ROTATED).exists(), "the old log must be kept aside");
        assert!(
            std::fs::metadata(dir.join(FILE)).unwrap().len() < MAX_BYTES,
            "the live log must start fresh"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn tail_reads_across_a_rotation() {
        // The entries either side of a rotation are one continuous history, and
        // `clt log -n 20` must not go blank the moment the log rolls over.
        let dir = scratch("tail-across");
        let older: Vec<String> = (1..=5)
            .map(|n| serde_json::to_string(&entry(n)).unwrap())
            .collect();
        let newer: Vec<String> = (6..=8)
            .map(|n| serde_json::to_string(&entry(n)).unwrap())
            .collect();
        std::fs::write(dir.join(ROTATED), older.join("\n")).unwrap();
        std::fs::write(dir.join(FILE), newer.join("\n")).unwrap();

        let ids: Vec<u32> = tail(&dir, 6).iter().filter_map(|e| e.id).collect();
        assert_eq!(
            ids,
            vec![3, 4, 5, 6, 7, 8],
            "oldest first, spanning the rotation boundary"
        );

        // And a request the live log can satisfy alone still works.
        let ids: Vec<u32> = tail(&dir, 2).iter().filter_map(|e| e.id).collect();
        assert_eq!(ids, vec![7, 8]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn tail_survives_a_truncated_final_line() {
        let dir = scratch("tail-torn");
        let good = serde_json::to_string(&entry(1)).unwrap();
        std::fs::write(dir.join(FILE), format!("{good}\n{{\"ts\":\"2026")).unwrap();
        assert_eq!(tail(&dir, 10).len(), 1, "a half-written line is skipped");
        std::fs::remove_dir_all(&dir).ok();
    }
}
