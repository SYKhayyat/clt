//! Append-only audit log at `.comtai/log.jsonl`.
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

/// The lock the rotation and the append are done under.
///
/// Rotation is a `rename` of the live log onto the single previous generation,
/// and `append` used to do it holding nothing. Two runs that both find a full
/// log both rename; the second one replaces the generation the first had just
/// set aside, so a megabyte of history nobody has read yet is discarded with
/// nothing printed (#1). The MCP writers make that reachable in ordinary use —
/// they append *after* releasing the store's lock, deliberately, so hooks and
/// their network calls stay outside the critical section.
///
/// So the journal takes a lock of its own rather than borrowing the store's:
/// who calls `append` and what they hold is then not part of this module's
/// correctness. See `Lock::acquire_named` for why it has to be a second file.
const LOCK: &str = "journal.lock";

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
    // The size check, the rotation and the write are one critical section: the
    // window that loses a generation is between one writer's `metadata` and
    // another's `rename`, so guarding the rename alone would not close it.
    let _held = crate::lock::Lock::acquire_named(dir, LOCK).map_err(std::io::Error::other)?;
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
    // append to a file opened O_APPEND won't interleave with a concurrent comtai.
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
/// interrupted write shouldn't make `comtai log` useless.
pub fn tail(dir: &Path, limit: usize) -> Vec<Entry> {
    let mut entries = read_entries(&dir.join(FILE));

    // Reach back into the previous generation only when the live log cannot
    // satisfy the request on its own, so the common `comtai log` stays a single
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
        let dir = std::env::temp_dir().join(format!("comtai-journal-{}-{name}", std::process::id()));
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

    /// The rotation and the append are one critical section (#1).
    ///
    /// The test above races two writers and hopes to land in the window that
    /// loses a generation; the window is microseconds wide and most rounds miss
    /// it. This one does not hope. It takes the journal's lock from outside, so
    /// any append that writes while it is held is an append that is not
    /// honouring the discipline at all — which was the state of the code.
    #[test]
    fn an_append_waits_for_the_journal_lock() {
        let dir = scratch("append-waits");
        let held = crate::lock::Lock::acquire_named(&dir, LOCK).unwrap();

        let writer = std::thread::spawn({
            let dir = dir.clone();
            move || append(&dir, &[entry(1)])
        });

        std::thread::sleep(std::time::Duration::from_millis(150));
        assert!(
            !dir.join(FILE).exists(),
            "an append wrote while another writer held the journal lock"
        );

        drop(held);
        writer.join().expect("the writer thread panicked");
        assert_eq!(
            read_entries(&dir.join(FILE)).len(),
            1,
            "the append must land once the lock is free"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Two writers crossing a rotation at the same time.
    ///
    /// Rotation is a `rename` onto the one previous generation (#1), and
    /// `append` held nothing while it happened. Two writers that both saw a
    /// full log both rotated, and the second `rename` replaced the generation
    /// the first had set aside a moment earlier: a megabyte of history
    /// discarded before a reader could see it, with nothing in the output to
    /// say so.
    ///
    /// The assertion is conservation, not order. Every entry a writer handed
    /// over is on disk exactly once — in the live log or the generation beside
    /// it — and the generation that existed *before* the race is still there,
    /// because a duplicate rotation is not a second legitimate rotation.
    #[test]
    fn concurrent_appends_across_a_rotation_lose_nothing() {
        // Repeated, because the interleaving is the machine's to choose: one
        // round in three lands on the clobbering order, six do not — and six
        // rounds cost ~3s, which is already the priciest unit test in the file.
        for round in 0..6 {
            let dir = scratch(&format!("race-{round}"));

            // A log that has been accumulating for a year. Its lines are the
            // history a reader could have asked for, so none may vanish.
            let filler: String = std::iter::repeat_n(
                r#"{"ts":"2026-01-01T00:00:00Z","action":"add","id":1}"#,
                (MAX_BYTES / 50) as usize + 10,
            )
            .collect::<Vec<_>>()
            .join("\n");
            let filler_len = filler.lines().count();
            std::fs::write(dir.join(FILE), &filler).unwrap();

            let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
            let writers: Vec<_> = (0..2u32)
                .map(|i| {
                    let dir = dir.clone();
                    let barrier = barrier.clone();
                    std::thread::spawn(move || {
                        barrier.wait();
                        append(&dir, &[entry(9000 + i)]);
                    })
                })
                .collect();
            for writer in writers {
                writer.join().expect("a writer thread panicked");
            }

            let live = read_entries(&dir.join(FILE));
            let older = read_entries(&dir.join(ROTATED));

            let ids: Vec<u32> = live
                .iter()
                .chain(older.iter())
                .filter_map(|e| e.id)
                .filter(|id| (9000..9002).contains(id))
                .collect();
            assert_eq!(
                ids.len(),
                2,
                "round {round}: an appended entry was lost — found {ids:?}"
            );

            let left = live.len() + older.len();
            assert!(
                left >= filler_len,
                "round {round}: the previous generation was overwritten — {left} of {filler_len} lines left"
            );

            std::fs::remove_dir_all(&dir).ok();
        }
    }

    #[test]
    fn tail_reads_across_a_rotation() {
        // The entries either side of a rotation are one continuous history, and
        // `comtai log -n 20` must not go blank the moment the log rolls over.
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
