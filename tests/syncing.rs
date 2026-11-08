//! `clt sync` and the mark it leaves behind for the next run.
//!
//! `tasks.json` records the last commit the sync looked at, and the next sync
//! walks from there. That mark is the only thing standing between routine use
//! and re-reading the repository every time, which makes what happens when it
//! stops naming a real commit worth its own test: a rebase or a force-push
//! rewrites shas, and the mark is left pointing at one that no longer exists.

mod common;
use common::*;

use std::path::Path;

/// The commit `clt sync` says it got to.
fn watermark(dir: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(dir.join(".clt/tasks.json")).unwrap();
    let data: serde_json::Value = serde_json::from_str(&raw).unwrap();
    data["last_commit"].as_str().map(str::to_owned)
}

/// Hands the store a mark, the way a rebase leaves one: a sha this repository
/// no longer has. `tasks.json` is documented as hand-editable, and this is the
/// state a colleague's rebase puts a checkout in after the old objects are gone.
fn put_watermark(dir: &Path, sha: &str) {
    let path = dir.join(".clt/tasks.json");
    let mut data: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    data["last_commit"] = serde_json::Value::String(sha.to_string());
    std::fs::write(&path, serde_json::to_string_pretty(&data).unwrap()).unwrap();
}

/// A mark that names no commit must not be read as "nothing new" (#2).
///
/// `git log <gone>..HEAD` fails. That failure used to be folded into an empty
/// commit list, and the two consequences together are the bug: nothing closed,
/// *and* the mark advanced to HEAD, so every commit between the old mark and
/// HEAD became permanently unread — silently, on the one command whose entire
/// job is to notice commits.
#[test]
fn a_mark_that_names_no_commit_closes_nothing_and_keeps_the_mark() {
    let dir = repo("sync-lost-mark");
    write(&dir, "code.txt", "one\n");
    git_ok(&dir, &["add", "-A"]);
    git_ok(&dir, &["commit", "-qm", "init"]);

    // A task the first sync closes, so the store records a mark at all.
    clt_ok(&dir, &["add", "the first thing"]);
    write(&dir, "code.txt", "two\n");
    git_ok(&dir, &["add", "-A"]);
    git_ok(
        &dir,
        &["commit", "-qm", "feat: land the first thing\n\ncloses clt#1"],
    );
    clt_ok(&dir, &["sync"]);

    let mark = watermark(&dir);
    assert!(mark.is_some(), "the first sync must record where it got to");
    assert_eq!(
        task_titled(&all_tasks(&dir), "the first thing")["state"],
        "done",
        "fixture: the first sync should have closed the task it found"
    );

    // A second task, and history that names it after the mark went stale.
    clt_ok(&dir, &["add", "the second thing"]);
    write(&dir, "code.txt", "three\n");
    git_ok(&dir, &["add", "-A"]);
    git_ok(
        &dir,
        &["commit", "-qm", "feat: land the second thing\n\ncloses clt#2"],
    );
    let gone = "0123456789abcdef0123456789abcdef01234567";
    put_watermark(&dir, gone);

    let out = clt(&dir, &["sync"]);
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        !out.status.success(),
        "a mark git cannot resolve must not report a successful sync; stdout was {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        stderr.contains("--rescan"),
        "the error must name the way forward, got: {stderr}"
    );
    assert_eq!(
        watermark(&dir).as_deref(),
        Some(gone),
        "the mark must not move when the walk never happened"
    );
    assert_eq!(
        task_titled(&all_tasks(&dir), "the second thing")["state"],
        "todo",
        "nothing may be closed by a walk that did not happen"
    );

    // And `--rescan` is the repair the message promised: the mark is ignored,
    // the last 50 commits are re-read, and the close lands.
    clt_ok(&dir, &["sync", "--rescan"]);
    assert_eq!(
        task_titled(&all_tasks(&dir), "the second thing")["state"],
        "done",
        "a rescan must find the close the failed walk could not"
    );

    cleanup(&dir);
}
