//! The MCP server, driven over its real stdio transport.
//!
//! This is half the product — the half an agent actually talks to — and it had
//! no test coverage at all. The invariant worth guarding above everything else
//! is that stdout carries protocol frames and nothing else: one stray line from
//! a hook or a `println!` desynchronises the stream and the agent loses the
//! connection with no useful diagnostic.

mod common;

use common::*;

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

/// Sends each request and returns one parsed response per line of stdout.
fn converse(dir: &std::path::Path, requests: &[serde_json::Value]) -> Vec<serde_json::Value> {
    let mut child = Command::new(CLT)
        .arg("mcp")
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawning clt mcp");

    {
        let mut stdin = child.stdin.take().expect("stdin");
        for req in requests {
            writeln!(stdin, "{req}").expect("writing a request");
        }
        // Closing stdin is what ends the server's read loop.
    }

    let stdout = child.stdout.take().expect("stdout");
    let frames: Vec<serde_json::Value> = BufReader::new(stdout)
        .lines()
        .map(|l| l.expect("reading a frame"))
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            serde_json::from_str(&l)
                .unwrap_or_else(|e| panic!("stdout carried a non-frame line ({e}): {l}"))
        })
        .collect();

    child.wait().expect("waiting for clt mcp");
    frames
}

fn call(id: u64, tool: &str, arguments: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": { "name": tool, "arguments": arguments }
    })
}

/// The text a tool call returned, insisting it did not report an error.
fn ok_text(frame: &serde_json::Value) -> String {
    let result = &frame["result"];
    assert_eq!(
        result["isError"], false,
        "tool reported an error: {}",
        result["content"][0]["text"]
    );
    result["content"][0]["text"].as_str().unwrap_or_default().to_string()
}

fn initialize() -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0", "id": 0, "method": "initialize",
        "params": { "protocolVersion": "2025-06-18" }
    })
}

#[test]
fn the_advertised_tools_cover_creating_and_correcting_a_task() {
    let dir = repo("mcp-tools");
    let frames = converse(
        &dir,
        &[serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}})],
    );

    let names: Vec<&str> = frames[0]["result"]["tools"]
        .as_array()
        .expect("a tool list")
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();

    for expected in [
        "clt_list",
        "clt_add",
        "clt_close",
        "clt_start",
        "clt_reopen",
        "clt_edit",
        "clt_search",
    ] {
        assert!(names.contains(&expected), "missing {expected}; have {names:?}");
    }
    cleanup(&dir);
}

#[test]
fn an_agent_can_file_correct_close_and_reopen_a_task() {
    // Before, an agent could create a task but never fix one: a mistake could
    // only be closed and re-filed, which loses its history.
    let dir = repo("mcp-lifecycle");
    let frames = converse(
        &dir,
        &[
            initialize(),
            call(1, "clt_add", serde_json::json!({"title": "vaguely worded thing"})),
            call(
                2,
                "clt_edit",
                serde_json::json!({
                    "id": 1,
                    "title": "token refresh races on 401",
                    "note": "two requests refresh at once",
                    "file": "src/auth.rs:88"
                }),
            ),
            call(3, "clt_close", serde_json::json!({"ids": [1]})),
            call(4, "clt_reopen", serde_json::json!({"ids": [1]})),
        ],
    );
    assert_eq!(frames.len(), 5, "one frame per request: {frames:?}");
    for frame in &frames[1..] {
        ok_text(frame);
    }

    let tasks = all_tasks(&dir);
    let t = task_titled(&tasks, "token refresh races on 401");
    assert_eq!(t["state"], "todo", "reopened");
    assert_eq!(t["note"], "two requests refresh at once");
    assert_eq!(t["location"]["line"], 88);
    assert_eq!(t["actor"], "agent", "MCP changes are attributed to the agent");
    cleanup(&dir);
}

#[test]
fn closing_through_mcp_cascades_exactly_like_the_cli() {
    // The MCP server used to carry its own copy of the transition rules, and
    // copies drift.
    let dir = repo("mcp-cascade");
    clt_ok(&dir, &["add", "parent"]);
    clt_ok(&dir, &["add", "child", "--parent", "1"]);

    let frames = converse(&dir, &[call(1, "clt_close", serde_json::json!({"ids": [1]}))]);
    ok_text(&frames[0]);

    let tasks = all_tasks(&dir);
    assert_eq!(task_titled(&tasks, "parent")["state"], "done");
    assert_eq!(
        task_titled(&tasks, "child")["state"],
        "done",
        "closing a parent over MCP must close its subtree, as it does on the CLI"
    );
    cleanup(&dir);
}

#[test]
fn a_hook_that_prints_cannot_corrupt_the_frame_stream() {
    // The invariant the whole transport rests on. A `post-add` hook that echoes
    // — most of them do — must not put a byte on stdout.
    let dir = repo("mcp-hook-noise");
    let hooks = dir.join(".clt/hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    std::fs::write(
        hooks.join("post-add"),
        "#!/bin/sh\necho 'this must never reach stdout'\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(hooks.join("post-add"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
    }

    // converse() panics on any stdout line that is not a frame, so reaching the
    // assertions at all is most of the point.
    let frames = converse(
        &dir,
        &[
            initialize(),
            call(1, "clt_add", serde_json::json!({"title": "noisy hook"})),
        ],
    );
    assert_eq!(frames.len(), 2);
    ok_text(&frames[1]);
    cleanup(&dir);
}

#[test]
fn tool_failures_come_back_as_results_not_transport_errors() {
    // A model has to be able to read the message and correct itself. A JSON-RPC
    // error would instead look like the connection is broken.
    let dir = repo("mcp-errors");
    let frames = converse(
        &dir,
        &[
            call(1, "clt_edit", serde_json::json!({"id": 999, "title": "nope"})),
            call(2, "clt_edit", serde_json::json!({"id": 1})),
            call(3, "clt_add", serde_json::json!({"title": "   "})),
            call(4, "clt_nonexistent", serde_json::json!({})),
        ],
    );

    for frame in &frames {
        assert!(
            frame.get("error").is_none(),
            "a tool failure must not be a transport error: {frame}"
        );
        assert_eq!(
            frame["result"]["isError"], true,
            "expected an in-band error: {frame}"
        );
        assert!(
            !frame["result"]["content"][0]["text"]
                .as_str()
                .unwrap_or_default()
                .is_empty(),
            "an error must say something useful: {frame}"
        );
    }
    cleanup(&dir);
}

#[test]
fn notifications_are_not_answered() {
    // Replying to a request with no id is a protocol violation some clients
    // treat as fatal, and `notifications/initialized` arrives in every session.
    let dir = repo("mcp-notify");
    let frames = converse(
        &dir,
        &[
            initialize(),
            serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            call(1, "clt_list", serde_json::json!({})),
        ],
    );
    assert_eq!(
        frames.len(),
        2,
        "exactly two answerable requests were sent: {frames:?}"
    );
    cleanup(&dir);
}
