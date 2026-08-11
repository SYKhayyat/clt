//! State transitions, whichever command performs them.
//!
//! There are two doors to the same room — `clt done 3` and `clt edit 3 --state
//! done` — and they used to have different rules. The second skipped the
//! subtree cascade, fired no hook, left a stale `closed_by` in place, and wrote
//! the journal entry as "edit", so the audit log could not tell you a task had
//! been closed at all.

mod common;

use common::*;

use std::path::Path;

fn tree(name: &str) -> std::path::PathBuf {
    let dir = repo(name);
    clt_ok(&dir, &["add", "parent"]);
    clt_ok(&dir, &["add", "child", "--parent", "1"]);
    clt_ok(&dir, &["add", "grandchild", "--parent", "2"]);
    dir
}

fn states(dir: &Path) -> Vec<String> {
    let mut tasks = all_tasks(dir);
    tasks.sort_by_key(|t| t["id"].as_u64().unwrap_or(0));
    tasks
        .iter()
        .map(|t| t["state"].as_str().unwrap_or_default().to_string())
        .collect()
}

fn journal_actions(dir: &Path) -> Vec<String> {
    clt_json(dir, &["log"])
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["action"].as_str().unwrap_or_default().to_string())
        .collect()
}

#[test]
fn edit_state_done_cascades_exactly_like_done() {
    let via_edit = tree("state-edit");
    clt_ok(&via_edit, &["edit", "1", "--state", "done"]);

    let via_done = tree("state-done");
    clt_ok(&via_done, &["done", "1"]);

    assert_eq!(states(&via_edit), vec!["done", "done", "done"]);
    assert_eq!(
        states(&via_edit),
        states(&via_done),
        "both doors must leave the tree in the same state"
    );

    cleanup(&via_edit);
    cleanup(&via_done);
}

#[test]
fn edit_state_journals_under_the_transition_not_as_an_edit() {
    let dir = tree("state-journal");
    clt_ok(&dir, &["edit", "1", "--state", "done"]);

    let actions = journal_actions(&dir);
    assert_eq!(
        actions.iter().filter(|a| *a == "done").count(),
        3,
        "the close and its cascade must be recorded as closes; got {actions:?}"
    );
    assert!(
        !actions.contains(&"edit".to_string()),
        "a pure state change is not an edit; got {actions:?}"
    );
    cleanup(&dir);
}

#[test]
fn editing_a_field_and_the_state_records_both() {
    let dir = tree("state-both");
    clt_ok(&dir, &["edit", "3", "--title", "renamed", "--state", "doing"]);

    let actions = journal_actions(&dir);
    assert!(actions.contains(&"edit".to_string()), "{actions:?}");
    assert!(actions.contains(&"doing".to_string()), "{actions:?}");

    let tasks = all_tasks(&dir);
    let t = task_titled(&tasks, "renamed");
    assert_eq!(t["state"], "doing");
    cleanup(&dir);
}

#[test]
fn reopening_through_edit_clears_the_commit_that_closed_it() {
    // `closed_by` is set by commit linkage. Leaving it behind on a reopened task
    // claims a commit closed something that is currently open.
    let dir = repo("state-closed-by");
    write(&dir, "code.txt", "hello\n");
    git_ok(&dir, &["add", "-A"]);
    git_ok(&dir, &["commit", "-qm", "init"]);
    clt_ok(&dir, &["add", "fix the race"]);
    git_ok(&dir, &["commit", "-qm", "fix it\n\ncloses clt#1", "--allow-empty"]);
    clt_ok(&dir, &["sync"]);

    let tasks = all_tasks(&dir);
    assert!(
        task_titled(&tasks, "fix the race")["closed_by"].is_string(),
        "fixture: sync should have recorded the closing commit"
    );

    clt_ok(&dir, &["edit", "1", "--state", "todo"]);
    let tasks = all_tasks(&dir);
    let t = task_titled(&tasks, "fix the race");
    assert_eq!(t["state"], "todo");
    assert!(
        t.get("closed_by").is_none(),
        "an open task must not still name the commit that closed it"
    );
    cleanup(&dir);
}

#[test]
fn reopening_a_parent_does_not_reopen_its_subtasks() {
    // Closing cascades; reopening deliberately does not. Reopening a parent to
    // add one more subtask should not undo the ones already finished.
    let dir = tree("state-reopen");
    clt_ok(&dir, &["done", "1"]);
    assert_eq!(states(&dir), vec!["done", "done", "done"]);

    clt_ok(&dir, &["edit", "1", "--state", "todo"]);
    assert_eq!(
        states(&dir),
        vec!["todo", "done", "done"],
        "only the named task reopens"
    );
    cleanup(&dir);
}
