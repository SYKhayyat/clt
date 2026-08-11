//! `--json` must mean JSON, from every command, on every path.
//!
//! The README sells `--json` as the integration surface — "that, not a plugin
//! API, is how other tools and agents are meant to build on this". A command
//! that drops a line of prose onto stdout breaks the consumer *and* exits 0, so
//! nothing upstream notices until the parse fails somewhere far away.
//!
//! This sweeps the whole command surface rather than testing the one path that
//! was known to be broken, because the next leak will be somewhere else.

mod common;

use common::*;

/// Runs a command with `--json` and insists stdout is a JSON document.
fn expect_json(dir: &std::path::Path, args: &[&str]) -> serde_json::Value {
    let mut argv = args.to_vec();
    argv.push("--json");
    let out = clt(dir, &argv);
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        out.status.success(),
        "clt {argv:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!("clt {argv:?} put non-JSON on stdout ({e}):\n{stdout}")
    })
}

fn seeded(name: &str) -> std::path::PathBuf {
    let dir = repo(name);
    write(&dir, "code.txt", "hello\n");
    git_ok(&dir, &["add", "-A"]);
    git_ok(&dir, &["commit", "-qm", "init"]);
    dir
}

#[test]
fn every_command_emits_json_when_asked() {
    let dir = seeded("json-sweep");

    expect_json(&dir, &["init"]);
    expect_json(&dir, &["add", "first task"]);
    expect_json(&dir, &["add", "second task"]);
    expect_json(&dir, &["add", "a subtask", "--parent", "1"]);
    expect_json(&dir, &["ls"]);
    expect_json(&dir, &["ls", "--all"]);
    expect_json(&dir, &["ls", "--done"]);
    expect_json(&dir, &["ls", "--orphaned"]);
    expect_json(&dir, &["find", "task"]);
    expect_json(&dir, &["find", "nothing matches this"]);
    expect_json(&dir, &["start", "2"]);
    expect_json(&dir, &["edit", "2", "--note", "some detail"]);
    expect_json(&dir, &["move", "3", "--root"]);
    expect_json(&dir, &["scope", "3", "--repo"]);
    expect_json(&dir, &["scan"]);
    expect_json(&dir, &["scan", "-n"]);
    expect_json(&dir, &["sync"]);
    expect_json(&dir, &["sync", "-n"]);
    expect_json(&dir, &["log"]);
    expect_json(&dir, &["path"]);
    expect_json(&dir, &["share"]);
    expect_json(&dir, &["unshare"]);
    expect_json(&dir, &["done", "1"]);
    expect_json(&dir, &["reopen", "1"]);
    expect_json(&dir, &["rm", "1"]);

    cleanup(&dir);
}

#[test]
fn a_no_op_state_change_still_returns_json() {
    // The original leak: closing an already-closed task printed "Already done."
    // onto stdout and exited 0.
    let dir = seeded("json-noop");
    clt_ok(&dir, &["add", "a task"]);
    clt_ok(&dir, &["done", "1"]);

    let repeated = expect_json(&dir, &["done", "1"]);
    assert_eq!(
        repeated,
        serde_json::json!([]),
        "closing an already-closed task changed nothing, so the answer is an \
         empty list of changes — not a sentence"
    );

    // Same shape as the call that did change something, so a consumer can treat
    // both identically.
    clt_ok(&dir, &["add", "another"]);
    assert!(expect_json(&dir, &["done", "2"]).is_array());

    // And the same on the other two transitions.
    assert!(expect_json(&dir, &["start", "2"]).is_array());
    assert!(expect_json(&dir, &["start", "2"]).is_array());
    assert!(expect_json(&dir, &["reopen", "1"]).is_array());
    assert!(expect_json(&dir, &["reopen", "1"]).is_array());
    cleanup(&dir);
}

#[test]
fn a_no_op_re_scope_still_returns_json() {
    let dir = seeded("json-noop-scope");
    clt_ok(&dir, &["add", "a task", "--repo"]);
    assert_eq!(expect_json(&dir, &["scope", "1", "--repo"]), serde_json::json!([]));
    cleanup(&dir);
}

#[test]
fn errors_stay_on_stderr_and_set_a_failing_exit_code() {
    // The other half of the contract: a consumer must be able to tell failure
    // from an empty result, and must never find a diagnostic in the document.
    let dir = seeded("json-errors");
    for args in [
        vec!["done", "999", "--json"],
        vec!["edit", "999", "--title", "x", "--json"],
        vec!["scope", "999", "--repo", "--json"],
        vec!["move", "999", "--root", "--json"],
    ] {
        let out = clt(&dir, &args);
        assert!(!out.status.success(), "clt {args:?} should have failed");
        assert!(
            out.stdout.is_empty(),
            "clt {args:?} wrote to stdout on failure: {}",
            String::from_utf8_lossy(&out.stdout)
        );
        assert!(
            !String::from_utf8_lossy(&out.stderr).is_empty(),
            "clt {args:?} failed without saying why"
        );
    }
    cleanup(&dir);
}
