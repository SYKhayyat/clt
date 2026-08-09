//! Hook scripts in `.clt/hooks/`.
//!
//! This is the extensibility story, and it is deliberately git's: an executable
//! with the right name, in the right directory, in whatever language you like.
//! No plugin API to design, version, or keep ABI-stable; no scripting runtime
//! embedded in the binary. If you want `clt` to post to Slack when an agent
//! files a task, that's four lines of shell and zero lines of Rust in here.
//!
//! Hooks receive the task as JSON on stdin and as `CLT_*` environment
//! variables, inherit stdout/stderr so their output is visible, and cannot fail
//! the command that triggered them — every hook we fire is a `post-` hook,
//! reporting something that already happened.

use crate::task::Task;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// Where a hook's stdout is allowed to go.
///
/// This exists because hooks run user-authored scripts that print whatever they
/// like, and in MCP mode our stdout is a JSON-RPC frame stream. A single line
/// of hook chatter on that stream desynchronises the transport and the agent
/// silently loses the connection — so the caller has to state which regime it
/// is in, rather than relying on anyone remembering the invariant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Output {
    /// Terminal use: hook output goes straight to the user's screen.
    Inherit,
    /// Protocol use: hook stdout is captured and replayed on *our* stderr, so
    /// it stays visible for debugging without touching the frame stream.
    Divert,
}

/// Fires `.clt/hooks/<event>` if it exists, with `task` as its payload.
pub fn fire(dir: &Path, event: &str, task: &Task, actor: Option<&str>, out: Output) {
    let hooks = dir.join("hooks");
    let Some((program, args)) = resolve(&hooks, event) else {
        return;
    };

    let payload = serde_json::to_string(task).unwrap_or_else(|_| "{}".into());

    let mut cmd = Command::new(program);
    cmd.args(args)
        .current_dir(dir.parent().unwrap_or(dir))
        .env("CLT_EVENT", event)
        .env("CLT_TASK_ID", task.id.to_string())
        .env("CLT_TASK_TITLE", &task.title)
        .env("CLT_TASK_STATE", task.state.as_str())
        .env("CLT_TASK_JSON", &payload)
        .env("CLT_BRANCH", task.branch.as_deref().unwrap_or(""))
        .env("CLT_ACTOR", actor.unwrap_or(""))
        .stdin(Stdio::piped())
        .stdout(match out {
            Output::Inherit => Stdio::inherit(),
            Output::Divert => Stdio::piped(),
        })
        // stderr is never a protocol channel, so it always passes through.
        .stderr(Stdio::inherit());

    let Ok(mut child) = cmd.spawn() else {
        crate::render::warn(&format!("hook `{event}` could not be started"));
        return;
    };

    if let Some(mut stdin) = child.stdin.take() {
        // A hook that ignores stdin (most of them) closes the pipe early; that
        // is a BrokenPipe, not a problem worth reporting.
        let _ = stdin.write_all(payload.as_bytes());
    }

    let status = match out {
        Output::Inherit => child.wait(),
        Output::Divert => child.wait_with_output().map(|o| {
            if !o.stdout.is_empty() {
                let _ = std::io::stderr().write_all(&o.stdout);
            }
            o.status
        }),
    };

    match status {
        Ok(status) if !status.success() => {
            // Reported, not fatal. A post- hook has no veto: the task is
            // already added, closed, or deleted by the time we get here.
            crate::render::warn(&format!(
                "hook `{event}` exited with {}",
                status.code().map_or_else(|| "a signal".into(), |c| c.to_string())
            ));
        }
        Err(e) => crate::render::warn(&format!("hook `{event}` failed: {e}")),
        _ => {}
    }
}

/// Finds a runnable hook for `event`, returning the program and leading args.
///
/// On Unix this is "a file with the execute bit". Windows has no execute bit
/// and `CreateProcess` won't run an extensionless script, so we look for the
/// usual suffixes and route `.ps1` and `.sh` through their interpreters —
/// otherwise a `post-add` written as shell would silently never fire, which is
/// the worst possible behaviour for a hook.
fn resolve(hooks: &Path, event: &str) -> Option<(String, Vec<String>)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let direct = hooks.join(event);
        if let Ok(meta) = std::fs::metadata(&direct)
            && meta.is_file()
            && meta.permissions().mode() & 0o111 != 0
        {
            return Some((direct.to_string_lossy().into_owned(), Vec::new()));
        }
        None
    }
    #[cfg(windows)]
    {
        for ext in ["", ".exe", ".cmd", ".bat", ".ps1", ".sh"] {
            let path = hooks.join(format!("{event}{ext}"));
            if !path.is_file() {
                continue;
            }
            let p = path.to_string_lossy().into_owned();
            return Some(match ext {
                ".ps1" => (
                    "powershell".into(),
                    vec![
                        "-NoProfile".into(),
                        "-ExecutionPolicy".into(),
                        "Bypass".into(),
                        "-File".into(),
                        p,
                    ],
                ),
                // Extensionless hooks are almost always shell scripts written
                // on another machine or by an agent; git ships bash on Windows,
                // so give them a fair chance rather than ignoring them.
                "" | ".sh" => ("sh".into(), vec![p]),
                _ => (p, Vec::new()),
            });
        }
        None
    }
}

/// Example hooks written on `clt init`, so the directory documents itself.
pub const SAMPLE: &str = r#"#!/bin/sh
# clt hook — rename to `post-add` (and chmod +x) to enable.
#
# Fired after a task is created. The task is on stdin as JSON, and also in:
#   CLT_EVENT CLT_TASK_ID CLT_TASK_TITLE CLT_TASK_STATE CLT_BRANCH CLT_ACTOR
#   CLT_TASK_JSON
#
# Exit status is reported but ignored — this hook cannot veto the change.
#
# Events: post-add  post-done  post-reopen  post-rm
#
# Example: shout when your agent files something, but stay quiet for yourself.
if [ -n "$CLT_ACTOR" ]; then
  printf '\n  %s filed #%s: %s\n\n' "$CLT_ACTOR" "$CLT_TASK_ID" "$CLT_TASK_TITLE"
fi
"#;
