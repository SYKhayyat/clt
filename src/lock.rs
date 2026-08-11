//! A cross-process lock over the task list.
//!
//! The headline feature of this tool is that something other than you writes to
//! your list. That makes `clt add` from your shell racing `clt_add` from an
//! agent the *normal* case rather than an exotic one, and a task list is a
//! read-modify-write cycle: load the file, change one field, write it back. Two
//! of those interleaved silently lose whichever landed first.
//!
//! So writers hold this lock from load until save. Readers do not need it: the
//! store is replaced by an atomic rename, so a reader sees either the whole old
//! file or the whole new one, never a splice of the two.
//!
//! Implemented with `create_new`, which is an atomic "create only if absent" on
//! every platform we target, rather than `flock`/`LockFileEx` behind a `cfg`.
//! The cost of that choice is that a process killed mid-write leaves the file
//! behind, so we expire stale locks by age — see [`STALE_AFTER`].

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// How long to wait for a competing writer before giving up.
///
/// Generous, because the thing we are usually queued behind is another `clt`
/// run whose critical section is a few milliseconds. Anything approaching this
/// bound means a real problem, and the error says so rather than hanging
/// forever.
const WAIT_LIMIT: Duration = Duration::from_secs(10);

/// A lock older than this is assumed to belong to a process that died holding
/// it. No legitimate critical section here is longer than a file write; a
/// minute is far past that but still short enough that a crash doesn't wedge
/// the tool until someone reads the source.
const STALE_AFTER: Duration = Duration::from_secs(60);

const NAME: &str = "lock";

/// Held for as long as the caller may write. Releases on drop.
#[derive(Debug)]
pub struct Lock {
    path: PathBuf,
}

impl Lock {
    /// Blocks until the lock is ours, [`WAIT_LIMIT`] elapses, or the directory
    /// turns out not to be writable.
    pub fn acquire(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("creating {}", dir.display()))?;
        let path = dir.join(NAME);

        let deadline = SystemTime::now() + WAIT_LIMIT;
        // Poll rather than back off exponentially: the expected wait is one
        // file write, and a 200ms sleep would make the common case feel broken.
        let poll = Duration::from_millis(5);

        loop {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut file) => {
                    // Content is purely diagnostic — the lock is the file's
                    // existence, not anything inside it. Best-effort, since
                    // failing here would mean releasing a lock we just took.
                    use std::io::Write;
                    let _ = writeln!(file, "{}", std::process::id());
                    return Ok(Self { path });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if expire_if_stale(&path, STALE_AFTER) {
                        continue;
                    }
                    if SystemTime::now() >= deadline {
                        bail!(
                            "timed out waiting for another clt process to finish writing\n\
                             (lock: {}) — if no other clt is running, delete that file",
                            path.display()
                        );
                    }
                    std::thread::sleep(poll);
                }
                // Windows reports a file that has been deleted but still has an
                // open handle as ACCESS_DENIED rather than ALREADY_EXISTS, so
                // arriving in the instant another process releases the lock
                // lands here rather than above. Treating it as fatal made two
                // back-to-back `clt add`s fail outright, which is precisely the
                // contention this type exists to absorb.
                Err(e) if is_transient(&e) && SystemTime::now() < deadline => {
                    std::thread::sleep(poll);
                }
                Err(e) => {
                    return Err(e).with_context(|| format!("creating {}", path.display()));
                }
            }
        }
    }
}

/// Whether a filesystem error is the kind that clears if you wait a moment.
///
/// Shared with the store's atomic replace, because both hit the same Windows
/// behaviour: an operation on a file another process has open — or has just
/// deleted, leaving it pending-delete — fails with a sharing violation that
/// std surfaces as `PermissionDenied`. Retrying is correct; a genuine
/// permission problem persists and still surfaces once the attempts run out.
pub fn is_transient(e: &std::io::Error) -> bool {
    use std::io::ErrorKind;
    matches!(
        e.kind(),
        ErrorKind::PermissionDenied | ErrorKind::Interrupted | ErrorKind::WouldBlock
    )
}

/// Removes the lock if it has gone unclaimed for `threshold`. Returns true if
/// it did, meaning the caller should retry.
///
/// Age is read from the filesystem rather than by probing the recorded pid:
/// pids are recycled, and "is pid 4821 alive" is three different syscalls
/// across the platforms this runs on.
///
/// `threshold` is a parameter rather than a straight read of [`STALE_AFTER`] so
/// the expiry path is testable without backdating a file's mtime, which std
/// cannot portably do.
fn expire_if_stale(path: &Path, threshold: Duration) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        // Vanished underneath us, which means the holder just released it.
        return true;
    };
    let Ok(modified) = meta.modified() else {
        return false;
    };
    let Ok(age) = SystemTime::now().duration_since(modified) else {
        // Timestamp in the future: clock skew, not staleness.
        return false;
    };
    if age < threshold {
        return false;
    }
    crate::render::warn(&format!(
        "clearing a stale lock left by a process that died ({})",
        path.display()
    ));
    std::fs::remove_file(path).is_ok()
}

impl Drop for Lock {
    fn drop(&mut self) {
        // If this fails the lock expires by age instead. Warning here would fire
        // during unwinding and bury whatever actually went wrong.
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("clt-lock-{}-{name}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_second_acquire_fails_while_the_first_is_held() {
        let dir = scratch("held");
        let _first = Lock::acquire(&dir).unwrap();
        // Not waiting out WAIT_LIMIT in a unit test; assert the file is the
        // gate, which is what the second acquire would spin on.
        assert!(dir.join(NAME).exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn dropping_the_lock_lets_the_next_writer_in() {
        let dir = scratch("released");
        {
            let _lock = Lock::acquire(&dir).unwrap();
        }
        assert!(!dir.join(NAME).exists(), "drop must release");
        let _second = Lock::acquire(&dir).unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_lock_left_by_a_dead_process_is_reclaimed() {
        // The scenario: someone killed clt mid-write. Nothing will ever remove
        // this file, so acquire() has to be able to take it over or the tool is
        // wedged permanently.
        let dir = scratch("stale");
        let path = dir.join(NAME);
        std::fs::write(&path, "99999\n").unwrap();

        assert!(
            expire_if_stale(&path, Duration::ZERO),
            "a lock past its threshold must be reclaimed"
        );
        assert!(!path.exists(), "reclaiming means removing it");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_lock_that_is_merely_busy_is_left_alone() {
        // The inverse, and the one that matters: a freshly-taken lock belongs to
        // a live process. Expiring it would put us straight back to two writers
        // in the same critical section.
        let dir = scratch("busy");
        let _held = Lock::acquire(&dir).unwrap();
        assert!(
            !expire_if_stale(&dir.join(NAME), STALE_AFTER),
            "a lock held right now must never be stolen"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
