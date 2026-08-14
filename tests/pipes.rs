//! `clt ls | head` must not be a crash.
//!
//! Closing a pipe early is what `head`, `less` and every `grep -m1` do, and it
//! is the one io error a CLI meets constantly. Rust does not make it free:
//! `std::println!` panics when the write fails, and this crate builds with
//! `panic = "abort"`, so getting it wrong is an abort in the middle of somebody's
//! shell pipeline.
//!
//! clt gets it right by delegating: `anstream`'s macros swallow `BrokenPipe` and
//! panic on everything else. That is a property of a dependency's macro, not of
//! any line of code here, which is exactly why it is pinned from the outside —
//! nothing in this repo would fail to compile if `anstream` changed its mind.
//!
//! The reader is closed outright rather than after N lines: it is the same error
//! on the same write, and it does not depend on a pipe buffer size that differs
//! per platform.

mod common;

use common::*;
use std::path::Path;
use std::process::{Command, Stdio};

/// Runs clt with nobody on the other end of stdout, and reports how it took it.
fn into_a_closed_pipe(dir: &Path, args: &[&str]) -> (bool, String) {
    let mut child = Command::new(CLT)
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("running clt");

    // Drop the read end before the child gets anywhere. Every write it then
    // attempts fails, which is the condition under test.
    drop(child.stdout.take());

    let out = child.wait_with_output().expect("waiting for clt");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn seeded(name: &str) -> std::path::PathBuf {
    let dir = repo(name);
    write(&dir, "code.rs", "// TODO(clt): harvest me\n");
    git_ok(&dir, &["add", "-A"]);
    git_ok(&dir, &["commit", "-qm", "init"]);
    for i in 1..=40 {
        clt_ok(&dir, &["add", &format!("task number {i}")]);
    }
    dir
}

#[test]
fn a_closed_reader_is_not_a_crash() {
    let dir = seeded("pipe-closed");

    // Every command that writes a list to stdout, in both renderings: the human
    // one writes a line at a time, `--json` writes one document, and they fail
    // on different writes.
    for args in [
        vec!["ls"],
        vec!["ls", "--all"],
        vec!["ls", "--all", "--json"],
        vec!["find", "task"],
        vec!["find", "task", "--json"],
        vec!["log"],
        vec!["log", "--json"],
        vec!["path"],
        vec!["path", "--json"],
        vec!["scan", "-n"],
    ] {
        let (ok, stderr) = into_a_closed_pipe(&dir, &args);
        assert!(
            !stderr.contains("panicked"),
            "clt {args:?} panicked when stdout went away:\n{stderr}"
        );
        assert!(
            ok,
            "clt {args:?} failed when stdout went away; stderr was:\n{stderr}"
        );
    }

    cleanup(&dir);
}

#[test]
fn a_closed_reader_does_not_stop_the_write() {
    // The half that matters more than the exit code: a command that changes the
    // list must still change it, even if nobody is listening to the receipt.
    let dir = seeded("pipe-durable");

    let (ok, stderr) = into_a_closed_pipe(&dir, &["add", "filed into the void"]);
    assert!(ok, "clt add failed into a closed pipe:\n{stderr}");

    assert!(
        titles(&dir).iter().any(|t| t == "filed into the void"),
        "the task was lost because stdout was closed"
    );

    let (ok, stderr) = into_a_closed_pipe(&dir, &["done", "1"]);
    assert!(ok, "clt done failed into a closed pipe:\n{stderr}");
    let tasks = all_tasks(&dir);
    assert_eq!(task_titled(&tasks, "task number 1")["state"], "done");

    cleanup(&dir);
}
