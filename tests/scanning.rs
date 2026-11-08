//! `comtai scan` across branches.
//!
//! Harvesting markers and scoping tasks to branches are the two features this
//! tool is sold on, and they meet here. A working tree contains one branch's
//! files, so "the marker is gone" and "the marker is on a branch I am not
//! standing on" look identical from inside a scan — and conflating them closes
//! work nobody finished.

mod common;

use common::*;

fn seeded(name: &str) -> std::path::PathBuf {
    let dir = repo(name);
    write(&dir, "code.txt", "hello\n");
    git_ok(&dir, &["add", "-A"]);
    git_ok(&dir, &["commit", "-qm", "init"]);
    dir
}

fn state_of(dir: &std::path::Path, title: &str) -> String {
    let tasks = all_tasks(dir);
    task_titled(&tasks, title)["state"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

#[test]
fn scanning_on_one_branch_does_not_close_another_branchs_tasks() {
    let dir = seeded("scan-cross-branch");

    git_ok(&dir, &["switch", "-qc", "feat/x"]);
    write(&dir, "feature.rs", "// TODO(comtai): finish the feature\n");
    git_ok(&dir, &["add", "-A"]);
    git_ok(&dir, &["commit", "-qm", "feature marker"]);
    comtai_ok(&dir, &["scan"]);
    assert_eq!(state_of(&dir, "finish the feature"), "todo");

    // Back to main, where feature.rs does not exist at all.
    git_ok(&dir, &["switch", "-q", "main"]);
    assert!(!dir.join("feature.rs").exists(), "fixture assumption");
    comtai_ok(&dir, &["scan"]);

    assert_eq!(
        state_of(&dir, "finish the feature"),
        "todo",
        "a marker that lives on another branch has not been deleted, and closing \
         it here is unrecoverable — a later scan sees a done task and leaves it"
    );
    cleanup(&dir);
}

#[test]
fn deleting_a_marker_on_its_own_branch_still_closes_the_task() {
    // The guard above must not cost the feature its actual job.
    let dir = seeded("scan-close");
    write(&dir, "code.rs", "// TODO(comtai): retry on 429 too\n");
    comtai_ok(&dir, &["scan"]);
    assert_eq!(state_of(&dir, "retry on 429 too"), "todo");

    write(&dir, "code.rs", "// nothing to see here\n");
    comtai_ok(&dir, &["scan"]);
    assert_eq!(
        state_of(&dir, "retry on 429 too"),
        "done",
        "the marker was deleted on the branch that owns the task"
    );
    cleanup(&dir);
}

#[test]
fn a_marker_that_moves_between_files_keeps_one_task() {
    let dir = seeded("scan-move");
    write(&dir, "old.rs", "// TODO(comtai): a specific thing\n");
    comtai_ok(&dir, &["scan"]);

    std::fs::remove_file(dir.join("old.rs")).unwrap();
    write(&dir, "new.rs", "\n\n// TODO(comtai): a specific thing\n");
    comtai_ok(&dir, &["scan"]);

    let tasks = all_tasks(&dir);
    assert_eq!(tasks.len(), 1, "the task follows the comment, it does not fork");
    let t = task_titled(&tasks, "a specific thing");
    assert_eq!(t["state"], "todo");
    assert_eq!(t["location"]["file"], "new.rs");
    assert_eq!(t["location"]["line"], 3);
    cleanup(&dir);
}

#[test]
fn switching_branches_does_not_churn_scanned_tasks() {
    // The regression this pins: `comtai scan` on every branch switch used to flip
    // tasks closed and leave them closed. Walk a realistic loop and assert the
    // list is stable.
    let dir = seeded("scan-churn");
    write(&dir, "shared.rs", "// TODO(comtai): shared work\n");
    git_ok(&dir, &["add", "-A"]);
    git_ok(&dir, &["commit", "-qm", "shared marker"]);
    comtai_ok(&dir, &["scan"]);

    git_ok(&dir, &["switch", "-qc", "feat/y"]);
    write(&dir, "only-here.rs", "// TODO(comtai): branch work\n");
    git_ok(&dir, &["add", "-A"]);
    git_ok(&dir, &["commit", "-qm", "branch marker"]);
    comtai_ok(&dir, &["scan"]);

    for _ in 0..3 {
        git_ok(&dir, &["switch", "-q", "main"]);
        comtai_ok(&dir, &["scan"]);
        git_ok(&dir, &["switch", "-q", "feat/y"]);
        comtai_ok(&dir, &["scan"]);
    }

    assert_eq!(state_of(&dir, "shared work"), "todo");
    assert_eq!(state_of(&dir, "branch work"), "todo");
    assert_eq!(all_tasks(&dir).len(), 2, "and no duplicates accumulated");
    cleanup(&dir);
}

/// A marker in a file the scan is told not to look at is never harvested (#3).
///
/// `.comtaiignore` and comtai's own storage are excluded file by file. That exclusion
/// used to be applied to the finished hits, so every file in `git ls-files` was
/// read and most of it thrown away; it is applied before the read now. The hits
/// have to be identical either way, which is what this pins.
#[test]
fn a_marker_in_an_ignored_file_is_not_harvested() {
    let dir = seeded("scan-ignored");
    write(&dir, ".comtaiignore", "vendor/\n");
    write(
        &dir,
        "vendor/lib.js",
        "// TODO(comtai): a vendored marker nobody wants harvested\n",
    );
    write(&dir, "src/real.rs", "// TODO(comtai): a marker worth having\n");
    git_ok(&dir, &["add", "-A"]);
    git_ok(&dir, &["commit", "-qm", "markers"]);

    comtai_ok(&dir, &["scan"]);

    let titles = titles(&dir);
    assert!(
        titles.iter().any(|t| t.contains("a marker worth having")),
        "the real marker must be harvested; the list holds {titles:?}"
    );
    assert!(
        !titles.iter().any(|t| t.contains("vendored marker")),
        "an ignored file must not contribute a task; the list holds {titles:?}"
    );

    cleanup(&dir);
}
