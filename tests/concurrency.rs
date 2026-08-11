//! Concurrent writers, run as real processes.
//!
//! This is the one property that cannot be tested in-process: the failure mode
//! is two `clt` invocations interleaving their read-modify-write, and a unit
//! test with two `Store` values in one process shares no filesystem lock
//! contention worth speaking of.
//!
//! The bug this pins down: with a shared temp filename and no lock, fifteen
//! concurrent `clt add` calls reliably produced a `tasks.json` consisting of one
//! process's complete JSON followed by another's tail, at which point the list
//! was unreadable and clt's own advice was to move it aside and start over.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const CLT: &str = env!("CARGO_BIN_EXE_clt");

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("clt-it-{}-{name}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();

    // A real repo, so the store lands in <root>/.clt and branch scoping is live.
    for args in [
        vec!["init", "--quiet"],
        vec!["config", "user.email", "test@example.com"],
        vec!["config", "user.name", "test"],
    ] {
        let ok = Command::new("git")
            .args(&args)
            .current_dir(&dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("git must be on PATH for the integration tests")
            .success();
        assert!(ok, "git {args:?} failed");
    }
    dir
}

fn clt(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(CLT)
        .args(args)
        .current_dir(dir)
        .output()
        .expect("running clt")
}

/// How many tasks the store actually contains, via clt's own reader.
fn count(dir: &Path) -> usize {
    let out = clt(dir, &["ls", "--all", "--done", "--json"]);
    assert!(
        out.status.success(),
        "the task list must still be readable\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let parsed: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("--json must emit parseable JSON");
    parsed.as_array().expect("a JSON array").len()
}

#[test]
fn concurrent_adds_neither_corrupt_the_store_nor_vanish() {
    let dir = scratch("concurrent-add");
    const WRITERS: usize = 16;

    let mut children: Vec<std::process::Child> = (0..WRITERS)
        .map(|i| {
            Command::new(CLT)
                .args(["add", &format!("task number {i}")])
                .current_dir(&dir)
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .expect("spawning clt")
        })
        .collect();

    let mut failures = Vec::new();
    for (i, child) in children.iter_mut().enumerate() {
        let out = child.wait_with_output_ref();
        if !out.0 {
            failures.push(format!("writer {i}: {}", out.1));
        }
    }

    assert!(
        failures.is_empty(),
        "every writer must succeed; got:\n{}",
        failures.join("\n")
    );
    assert_eq!(
        count(&dir),
        WRITERS,
        "every concurrent add must survive — a lost update here is a task the \
         user filed and will never see again"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_concurrent_reader_never_observes_a_torn_file() {
    // Readers deliberately take no lock, which is only safe because the store is
    // swapped in by an atomic rename. If that ever regresses to an in-place
    // write, this catches it.
    let dir = scratch("concurrent-read");
    for i in 0..40 {
        clt(&dir, &["add", &format!("seed {i}")]);
    }

    let mut writers: Vec<std::process::Child> = (0..8)
        .map(|i| {
            Command::new(CLT)
                .args(["add", &format!("late {i}")])
                .current_dir(&dir)
                .stdout(Stdio::null())
                // Captured, not discarded: a writer that fails here has to be
                // able to say why, or this test can only ever report a number
                // that came out wrong.
                .stderr(Stdio::piped())
                .spawn()
                .expect("spawning clt")
        })
        .collect();

    // Hammer reads while those land. Every one must parse.
    for _ in 0..25 {
        let out = clt(&dir, &["ls", "--all", "--done", "--json"]);
        assert!(
            out.status.success(),
            "a read raced a write and failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice::<serde_json::Value>(&out.stdout)
            .expect("a reader must never see a partially written store");
    }

    let mut failures = Vec::new();
    for (i, w) in writers.iter_mut().enumerate() {
        let (ok, err) = w.wait_with_output_ref();
        if !ok {
            failures.push(format!("writer {i}: {err}"));
        }
    }
    assert!(failures.is_empty(), "writers failed:\n{}", failures.join("\n"));
    assert_eq!(count(&dir), 48);

    std::fs::remove_dir_all(&dir).ok();
}

/// `wait_with_output` consumes the child, which we cannot do through `&mut`.
trait WaitRef {
    fn wait_with_output_ref(&mut self) -> (bool, String);
}

impl WaitRef for std::process::Child {
    fn wait_with_output_ref(&mut self) -> (bool, String) {
        use std::io::Read;
        let mut err = String::new();
        if let Some(mut pipe) = self.stderr.take() {
            let _ = pipe.read_to_string(&mut err);
        }
        let status = self.wait().expect("waiting on clt");
        (status.success(), err)
    }
}
