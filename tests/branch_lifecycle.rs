//! What happens to tasks when branches come and go.
//!
//! Branch scoping is the feature the whole tool is built around, and branches
//! are not permanent: they get merged, deleted, and renamed. A task filed on a
//! branch that no longer exists matches no view except `--all`, which means it
//! has effectively disappeared while still occupying the file. These tests
//! cover finding those tasks and getting them back.

mod common;

use common::*;

/// A repo on `main` with one commit, so branches can actually be created.
fn seeded(name: &str) -> std::path::PathBuf {
    let dir = repo(name);
    write(&dir, "code.txt", "hello\n");
    git_ok(&dir, &["add", "-A"]);
    git_ok(&dir, &["commit", "-qm", "init"]);
    dir
}

#[test]
fn tasks_survive_the_branch_they_were_filed_on() {
    let dir = seeded("orphan-rescue");

    git_ok(&dir, &["switch", "-qc", "feat/auth"]);
    clt_ok(&dir, &["add", "wire up the refresh"]);
    clt_ok(&dir, &["add", "hash passwords", "--parent", "1"]);

    // The lifecycle nobody handles: merged, then deleted.
    git_ok(&dir, &["switch", "-q", "main"]);
    git_ok(&dir, &["merge", "-q", "feat/auth"]);
    git_ok(&dir, &["branch", "-qD", "feat/auth"]);

    let orphans = clt_json(&dir, &["ls", "--orphaned"]);
    let ids: Vec<u64> = orphans
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["id"].as_u64().unwrap())
        .collect();
    assert_eq!(ids, vec![1, 2], "both tasks are stranded on a dead branch");

    clt_ok(&dir, &["scope", "1", "--repo"]);

    let after = clt_json(&dir, &["ls", "--orphaned"]);
    assert!(
        after.as_array().unwrap().is_empty(),
        "rescuing the root must rescue the subtree too"
    );

    // And they are visible again from a plain `clt`.
    let visible = clt_json(&dir, &["ls"]);
    assert_eq!(visible.as_array().unwrap().len(), 2);
    cleanup(&dir);
}

#[test]
fn re_scoping_moves_the_whole_subtree_and_refuses_to_split_one() {
    let dir = seeded("scope-subtree");
    clt_ok(&dir, &["add", "parent"]);
    clt_ok(&dir, &["add", "child", "--parent", "1"]);

    // A subtask cannot be moved out from under its parent: that would leave the
    // tree visible from one branch and half-visible from another.
    let out = clt(&dir, &["scope", "2", "--repo"]);
    assert!(!out.status.success(), "splitting a tree must be refused");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("nested under #1"),
        "the error must say which parent to re-scope instead"
    );

    clt_ok(&dir, &["scope", "1", "--repo"]);
    let tasks = all_tasks(&dir);
    for title in ["parent", "child"] {
        assert!(
            task_titled(&tasks, title).get("branch").is_none(),
            "{title} should be repo-wide after re-scoping the root"
        );
    }
    cleanup(&dir);
}

#[test]
fn a_repo_wide_task_can_be_pinned_back_to_a_branch() {
    // The inverse direction, so scope is a door that swings both ways.
    let dir = seeded("scope-pin");
    clt_ok(&dir, &["add", "everywhere", "--repo"]);
    clt_ok(&dir, &["scope", "1", "--here"]);

    let tasks = all_tasks(&dir);
    assert_eq!(task_titled(&tasks, "everywhere")["branch"], "main");

    // Now it is genuinely branch-scoped: invisible from another branch.
    git_ok(&dir, &["switch", "-qc", "other"]);
    assert!(
        clt_json(&dir, &["ls"]).as_array().unwrap().is_empty(),
        "a task pinned to main must not show on `other`"
    );
    cleanup(&dir);
}

#[test]
fn orphaned_ignores_repo_wide_tasks() {
    // Repo-wide tasks belong to no branch, so no branch can strand them.
    let dir = seeded("orphan-repo-wide");
    clt_ok(&dir, &["add", "belongs to everything", "--repo"]);
    git_ok(&dir, &["switch", "-qc", "temp"]);
    clt_ok(&dir, &["add", "belongs to temp"]);
    git_ok(&dir, &["switch", "-q", "main"]);
    git_ok(&dir, &["branch", "-qD", "temp"]);

    let orphans = clt_json(&dir, &["ls", "--orphaned"]);
    let titles: Vec<&str> = orphans
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["title"].as_str())
        .collect();
    assert_eq!(titles, vec!["belongs to temp"]);
    cleanup(&dir);
}
