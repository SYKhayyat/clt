//! `clt share` / `clt unshare` — whether the task list travels.
//!
//! The default is a local-only list, which means it dies with the working copy:
//! clone the repo somewhere else and the tasks are gone. These tests pin down
//! the opt-in, and the one assertion that actually matters is that a clone can
//! see the list.

mod common;

use common::*;

#[test]
fn a_shared_list_survives_a_clone() {
    let origin = repo("share-clone");
    write(&origin, "code.txt", "hello\n");
    git_ok(&origin, &["add", "-A"]);
    git_ok(&origin, &["commit", "-qm", "init"]);

    clt_ok(&origin, &["add", "scoped to main"]);
    clt_ok(&origin, &["add", "visible everywhere", "--repo"]);

    assert_eq!(
        clt_json(&origin, &["path"])["shared"],
        false,
        "local-only is the default"
    );

    clt_ok(&origin, &["share"]);
    assert!(
        origin.join(".clt/.gitignore").exists(),
        "share must keep per-clone state out of the shared list"
    );

    git_ok(&origin, &["add", ".clt"]);
    git_ok(&origin, &["commit", "-qm", "track the task list"]);
    assert_eq!(clt_json(&origin, &["path"])["shared"], true);

    // The whole point of the feature.
    let clone = origin.with_extension("clone");
    std::fs::remove_dir_all(&clone).ok();
    git_ok(
        origin.parent().unwrap(),
        &[
            "clone",
            "--quiet",
            origin.to_str().unwrap(),
            clone.to_str().unwrap(),
        ],
    );

    let mut travelled = titles(&clone);
    travelled.sort();
    assert_eq!(
        travelled,
        vec!["scoped to main", "visible everywhere"],
        "a shared list must arrive with the clone"
    );

    cleanup(&origin);
    cleanup(&clone);
}

#[test]
fn the_journal_stays_local_when_the_list_is_shared() {
    // Sharing the append-only journal would conflict on every pull, so it is
    // deliberately excluded. Losing that would make sharing unusable rather
    // than merely imperfect.
    let dir = repo("share-journal");
    clt_ok(&dir, &["add", "something"]);
    clt_ok(&dir, &["share"]);
    git_ok(&dir, &["add", ".clt"]);

    let staged = String::from_utf8_lossy(&git(&dir, &["diff", "--cached", "--name-only"]).stdout)
        .into_owned();
    assert!(
        staged.contains(".clt/tasks.json"),
        "the list itself must be staged; got:\n{staged}"
    );
    assert!(
        !staged.contains("log.jsonl"),
        "the journal must not be; got:\n{staged}"
    );
    cleanup(&dir);
}

#[test]
fn adding_a_task_in_a_clone_does_not_silently_re_exclude_the_list() {
    // save() adds the exclude entry when it creates .clt/ for the first time.
    // In a clone that directory arrives with the checkout, so the entry must
    // never be written — otherwise the first `clt add` after cloning would
    // quietly unshare the list the project had decided to share.
    let origin = repo("share-reexclude");
    clt_ok(&origin, &["add", "from origin"]);
    clt_ok(&origin, &["share"]);
    git_ok(&origin, &["add", ".clt"]);
    git_ok(&origin, &["commit", "-qm", "track"]);

    let clone = origin.with_extension("clone");
    std::fs::remove_dir_all(&clone).ok();
    git_ok(
        origin.parent().unwrap(),
        &[
            "clone",
            "--quiet",
            origin.to_str().unwrap(),
            clone.to_str().unwrap(),
        ],
    );

    clt_ok(&clone, &["add", "filed in the clone"]);

    let exclude = std::fs::read_to_string(clone.join(".git/info/exclude")).unwrap_or_default();
    assert!(
        !exclude.contains(".clt"),
        "a write in a clone must leave the list shared; exclude was:\n{exclude}"
    );
    assert_eq!(clt_json(&clone, &["path"])["shared"], true);

    cleanup(&origin);
    cleanup(&clone);
}

#[test]
fn unshare_puts_the_exclude_back_and_share_can_undo_it_again() {
    let dir = repo("share-roundtrip");
    clt_ok(&dir, &["add", "a task"]);

    let exclude = dir.join(".git/info/exclude");
    assert!(
        std::fs::read_to_string(&exclude).unwrap_or_default().contains(".clt/"),
        "the default is local-only"
    );

    clt_ok(&dir, &["share"]);
    assert!(
        !std::fs::read_to_string(&exclude).unwrap_or_default().contains(".clt/"),
        "share must lift the exclusion"
    );

    clt_ok(&dir, &["unshare"]);
    assert!(
        std::fs::read_to_string(&exclude).unwrap_or_default().contains(".clt/"),
        "unshare must restore it"
    );

    // And round-tripping must not have eaten anyone else's patterns.
    clt_ok(&dir, &["share"]);
    cleanup(&dir);
}

#[test]
fn unexclude_leaves_other_peoples_patterns_alone() {
    let dir = repo("share-preserve");
    let exclude = dir.join(".git/info/exclude");
    std::fs::create_dir_all(exclude.parent().unwrap()).unwrap();
    std::fs::write(&exclude, "# mine\nscratch/\n*.local\n").unwrap();

    clt_ok(&dir, &["add", "a task"]); // writes clt's own entry
    clt_ok(&dir, &["share"]); // and takes it back out

    let after = std::fs::read_to_string(&exclude).unwrap();
    assert!(after.contains("scratch/"), "lost a pattern:\n{after}");
    assert!(after.contains("*.local"), "lost a pattern:\n{after}");
    assert!(after.contains("# mine"), "lost a comment:\n{after}");
    assert!(!after.contains(".clt"), "kept our own:\n{after}");
    cleanup(&dir);
}

#[test]
fn share_reports_a_project_gitignore_it_cannot_override() {
    // clt only writes info/exclude. A committed .gitignore listing .clt/ is the
    // project's decision, and share has to say so rather than appearing to work.
    let dir = repo("share-blocked");
    write(&dir, ".gitignore", "/target\n.clt/\n");
    git_ok(&dir, &["add", "-A"]);
    git_ok(&dir, &["commit", "-qm", "ignore clt"]);
    clt_ok(&dir, &["add", "a task"]);

    let reported = clt_json(&dir, &["share"]);
    assert_eq!(
        reported["blocked_by"], ".gitignore",
        "share must name the file that is still ignoring the list"
    );
    assert_eq!(reported["shared"], false);
    cleanup(&dir);
}
