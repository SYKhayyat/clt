//! Shared scaffolding for the integration tests.
//!
//! These tests drive the real binary in a real repository, because most of what
//! is worth testing here lives in the seam between clt and git — branch
//! scoping, `git ls-files`, `info/exclude`, whether a list survives a clone.
//! None of that is reachable from a unit test.

#![allow(dead_code)] // each test binary uses a different subset

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

pub const CLT: &str = env!("CARGO_BIN_EXE_clt");

/// A throwaway git repo. Named per test so parallel tests never share one.
pub fn repo(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("clt-it-{}-{name}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    init_repo(&dir);
    dir
}

pub fn init_repo(dir: &Path) {
    for args in [
        vec!["init", "--quiet", "--initial-branch=main"],
        vec!["config", "user.email", "test@example.com"],
        vec!["config", "user.name", "test"],
    ] {
        let ok = git(dir, &args).status.success();
        assert!(ok, "git {args:?} failed in {}", dir.display());
    }
}

pub fn git(dir: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git must be on PATH for the integration tests")
}

/// Runs git and insists it worked, so a broken fixture fails loudly at the line
/// that broke it rather than as a confusing assertion three steps later.
pub fn git_ok(dir: &Path, args: &[&str]) {
    let out = git(dir, args);
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

pub fn clt(dir: &Path, args: &[&str]) -> Output {
    Command::new(CLT)
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .expect("running clt")
}

/// Runs clt and returns stdout, insisting on a zero exit.
pub fn clt_ok(dir: &Path, args: &[&str]) -> String {
    let out = clt(dir, args);
    assert!(
        out.status.success(),
        "clt {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Runs clt with `--json` and parses the result.
pub fn clt_json(dir: &Path, args: &[&str]) -> serde_json::Value {
    let mut argv = args.to_vec();
    argv.push("--json");
    let stdout = clt_ok(dir, &argv);
    serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!("clt {argv:?} did not emit JSON ({e}); stdout was:\n{stdout}")
    })
}

/// Every task in the list, across branches and states.
pub fn all_tasks(dir: &Path) -> Vec<serde_json::Value> {
    clt_json(dir, &["ls", "--all", "--done"])
        .as_array()
        .expect("ls --json returns an array")
        .clone()
}

pub fn titles(dir: &Path) -> Vec<String> {
    all_tasks(dir)
        .iter()
        .map(|t| t["title"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// The task with this title, or a panic naming what was actually there.
pub fn task_titled<'a>(tasks: &'a [serde_json::Value], title: &str) -> &'a serde_json::Value {
    tasks
        .iter()
        .find(|t| t["title"] == title)
        .unwrap_or_else(|| {
            let have: Vec<&str> = tasks.iter().filter_map(|t| t["title"].as_str()).collect();
            panic!("no task titled {title:?}; the list holds {have:?}")
        })
}

pub fn write(dir: &Path, rel: &str, contents: &str) {
    let path = dir.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, contents).unwrap();
}

pub fn cleanup(dir: &Path) {
    std::fs::remove_dir_all(dir).ok();
}
