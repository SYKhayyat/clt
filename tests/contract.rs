//! The documented shape of `--json`, checked against what actually comes out.
//!
//! `--json` is sold in the README as the surface an agent codes against, and the
//! `Storage` section is sold as a format you may hand-edit. Both are promises
//! about field names, and both drift the same way: a field gets added to
//! `Task`, `serde` starts emitting it, every test still passes, and the
//! documentation quietly describes a subset of reality. That is how `origin` and
//! `actor` — one of them on every row, and the one that makes `clt scan`
//! idempotent — went undocumented for six commits.
//!
//! So this compares the emitted key set to the README itself rather than to a
//! list kept here, which would drift in its own turn. Adding a field to `Task`
//! now fails this test until the README's examples show it.

mod common;

use common::*;
use std::collections::BTreeSet;

const README: &str = include_str!("../README.md");

/// A repo holding at least one task per optional field, so the union of emitted
/// keys is the whole model and not just the fields a plain `clt add` sets.
fn exercised(name: &str) -> std::path::PathBuf {
    let dir = repo(name);
    write(&dir, "src.rs", "// TODO(clt): harvested, so origin is a scan\n");
    git_ok(&dir, &["add", "-A"]);
    git_ok(&dir, &["commit", "-qm", "init"]);

    // Plain: only the required fields.
    clt_ok(&dir, &["add", "a plain task"]);
    // note, location, parent, actor.
    clt_ok(&dir, &[
        "add",
        "a detailed task",
        "--note",
        "the long version",
        "--file",
        "src.rs:1",
        "--parent",
        "1",
        "--actor",
        "claude",
    ]);
    // No branch at all: repo-wide.
    clt_ok(&dir, &["add", "a repo-wide task", "--repo"]);
    // origin = scan, actor = scan.
    clt_ok(&dir, &["scan"]);
    // closed_by: a commit that closes a task by reference.
    clt_ok(&dir, &["add", "closed by a commit"]);
    write(&dir, "more.rs", "fixed\n");
    git_ok(&dir, &["add", "-A"]);
    git_ok(&dir, &["commit", "-qm", "do the thing\n\ncloses clt#5"]);
    clt_ok(&dir, &["sync"]);

    dir
}

fn emitted_keys(dir: &std::path::Path) -> BTreeSet<String> {
    all_tasks(dir)
        .iter()
        .filter_map(|t| t.as_object())
        .flat_map(|o| o.keys().cloned())
        .collect()
}

#[test]
fn every_emitted_field_appears_in_the_readme() {
    let dir = exercised("contract-fields");
    let keys = emitted_keys(&dir);

    // The fixture is worth nothing if it didn't actually produce the optional
    // fields, so assert the hard-won ones are present before checking the docs.
    for expected in ["origin", "actor", "note", "closed_by", "location", "parent"] {
        assert!(
            keys.contains(expected),
            "the fixture never produced {expected:?}, so this test proves nothing; got {keys:?}"
        );
    }

    let undocumented: Vec<&String> = keys
        .iter()
        .filter(|k| !README.contains(&format!("\"{k}\"")))
        .collect();
    assert!(
        undocumented.is_empty(),
        "--json emits {undocumented:?}, which no example in README.md shows. \
         An agent coding against the documented shape would never know these exist."
    );

    cleanup(&dir);
}

#[test]
fn the_readme_does_not_document_fields_that_no_longer_exist() {
    // The other direction: a field removed from `Task` leaves an example
    // promising something nothing emits, which is worse than an omission
    // because it reads as authoritative.
    let dir = exercised("contract-stale");
    let keys = emitted_keys(&dir);

    // Only the keys the README shows inside a task object — `version`,
    // `next_id` and `tasks` belong to the file, not to a task.
    let file_level = ["version", "next_id", "tasks", "file", "line", "kind", "key"];
    let stale: Vec<&str> = [
        "id", "title", "note", "state", "parent", "branch", "location", "actor", "origin",
        "closed_by", "created", "updated", "depth", "context",
    ]
    .into_iter()
    .filter(|k| README.contains(&format!("\"{k}\"")))
    .filter(|k| !keys.contains(*k) && !file_level.contains(k))
    .collect();

    assert!(
        stale.is_empty(),
        "README.md documents {stale:?}, which nothing emits any more"
    );

    cleanup(&dir);
}

#[test]
fn origin_says_where_a_task_came_from() {
    // `origin` is the field the docs most need: it is what makes a rescan close
    // a harvested task instead of filing a second one, and `origin.key` is
    // derived from the marker text. A hand-edit that drops it silently changes
    // what the next `clt scan` does.
    let dir = exercised("contract-origin");
    let tasks = all_tasks(&dir);

    assert_eq!(
        task_titled(&tasks, "a plain task")["origin"],
        serde_json::json!({ "kind": "manual" }),
        "a typed task is manual"
    );

    let harvested = task_titled(&tasks, "harvested, so origin is a scan");
    assert_eq!(harvested["origin"]["kind"], "scan");
    assert!(
        harvested["origin"]["key"].is_string(),
        "a scanned task carries the marker key that identifies it: {harvested}"
    );
    assert_eq!(harvested["actor"], "scan");

    cleanup(&dir);
}

#[test]
fn path_json_does_not_mix_separators() {
    // Git answers in forward slashes on every platform; everything joined onto
    // its answer uses the platform separator. One string carrying both is
    // cosmetic when you open the file and ugly when you print it — and this
    // command exists to be printed and parsed.
    let dir = exercised("contract-path");
    let path = clt_json(&dir, &["path"]);

    for field in ["dir", "tasks"] {
        let value = path[field].as_str().unwrap_or_else(|| {
            panic!("clt path --json has no string {field:?}: {path}")
        });
        assert!(
            !(value.contains('/') && value.contains('\\')),
            "clt path --json {field:?} mixes separators: {value:?}"
        );
    }

    // And the plain rendering, which is the same path through `Display`.
    let printed = clt_ok(&dir, &["path"]);
    let printed = printed.trim();
    assert!(
        !(printed.contains('/') && printed.contains('\\')),
        "clt path mixes separators: {printed:?}"
    );

    cleanup(&dir);
}
