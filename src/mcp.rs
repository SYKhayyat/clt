//! MCP server over stdio.
//!
//! This is the half of the product that isn't a CLI. An agent shelling out to
//! `clt add` works, but it has to *know* to do that; an MCP server puts the
//! task list in its tool list, where it belongs.
//!
//! Hand-rolled JSON-RPC rather than an SDK. The stdio transport is
//! newline-delimited JSON and the surface we need is four methods, which is
//! genuinely less code than wiring up an async runtime — and it keeps `clt`
//! free of tokio, which matters when the same binary has to start in a
//! millisecond for interactive use.
//!
//! **Invariant: stdout carries protocol frames and nothing else.** One stray
//! `println!` corrupts the stream and the agent loses the connection with no
//! useful diagnostic. Everything human-readable goes to stderr.

use anyhow::{Context, Result};
use chrono::Utc;
use serde_json::{Value, json};
use std::io::{BufRead, Write};

use crate::store::{Scope, Store};
use crate::task::{Location, State, Task};
use crate::{hooks, journal};

/// Protocol version we implement. We echo the client's requested version when
/// it sends one, since MCP clients are more forgiving of agreement than of a
/// server insisting on its own revision.
const DEFAULT_PROTOCOL: &str = "2025-06-18";

pub fn serve(global: bool) -> Result<()> {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();

    for line in stdin.lock().lines() {
        let line = line.context("reading from stdin")?;
        if line.trim().is_empty() {
            continue;
        }

        let request: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                write_frame(
                    &mut stdout,
                    &json!({
                        "jsonrpc": "2.0",
                        "id": Value::Null,
                        "error": { "code": -32700, "message": format!("parse error: {e}") }
                    }),
                )?;
                continue;
            }
        };

        let id = request.get("id").cloned();
        let method = request.get("method").and_then(Value::as_str).unwrap_or("");
        let params = request.get("params").cloned().unwrap_or(json!({}));

        // Notifications have no id and must not be answered at all — replying
        // to one is a protocol violation that some clients treat as fatal.
        let Some(id) = id else {
            continue;
        };

        let response = match dispatch(method, &params, global) {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err(RpcError { code, message }) => {
                json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
            }
        };
        write_frame(&mut stdout, &response)?;
    }
    Ok(())
}

fn write_frame(out: &mut impl Write, value: &Value) -> Result<()> {
    let line = serde_json::to_string(value)?;
    out.write_all(line.as_bytes())?;
    out.write_all(b"\n")?;
    // Agents block waiting on this. An unflushed response is a hang.
    out.flush()?;
    Ok(())
}

struct RpcError {
    code: i32,
    message: String,
}

impl RpcError {
    fn method_not_found(method: &str) -> Self {
        Self {
            code: -32601,
            message: format!("unknown method: {method}"),
        }
    }

    fn invalid(message: impl Into<String>) -> Self {
        Self {
            code: -32602,
            message: message.into(),
        }
    }
}

fn dispatch(method: &str, params: &Value, global: bool) -> Result<Value, RpcError> {
    match method {
        "initialize" => {
            let protocol = params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or(DEFAULT_PROTOCOL)
                .to_string();
            Ok(json!({
                "protocolVersion": protocol,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "clt", "version": env!("CARGO_PKG_VERSION") }
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tool_definitions() })),
        "tools/call" => {
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| RpcError::invalid("missing tool name"))?;
            let args = params.get("arguments").cloned().unwrap_or(json!({}));

            // Tool failures are reported *inside* a successful result with
            // isError set, not as JSON-RPC errors. That's what lets the model
            // read the message and correct itself instead of the client
            // treating it as a transport fault.
            Ok(match call_tool(name, &args, global) {
                Ok(text) => json!({
                    "content": [{ "type": "text", "text": text }],
                    "isError": false
                }),
                Err(e) => json!({
                    "content": [{ "type": "text", "text": format!("{e:#}") }],
                    "isError": true
                }),
            })
        }
        other => Err(RpcError::method_not_found(other)),
    }
}

fn tool_definitions() -> Value {
    json!([
        {
            "name": "clt_list",
            "description": "List tasks for the current git branch. Repo-wide tasks always \
                            appear. Returns JSON including each task's id, state, parent \
                            (for nesting) and source location.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "all": { "type": "boolean", "description": "Every branch, not just the current one" },
                    "include_done": { "type": "boolean", "description": "Include completed tasks" }
                }
            }
        },
        {
            "name": "clt_add",
            "description": "File a task on the current branch. Use this when you find work \
                            you are not doing right now — a bug you noticed in passing, a \
                            follow-up, something the user should decide. Set `file` to the \
                            source location it concerns.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "title": { "type": "string", "description": "One line, specific" },
                    "note": { "type": "string", "description": "Detail, context, reproduction" },
                    "file": { "type": "string", "description": "Location as path or path:line" },
                    "parent": { "type": "integer", "description": "Id of a task to nest this under" },
                    "repo": { "type": "boolean", "description": "Visible on every branch, not just this one" }
                },
                "required": ["title"]
            }
        },
        {
            "name": "clt_close",
            "description": "Mark tasks done. Closing a task also closes everything nested \
                            under it.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "ids": { "type": "array", "items": { "type": "integer" } }
                },
                "required": ["ids"]
            }
        },
        {
            "name": "clt_start",
            "description": "Mark a task as in progress.",
            "inputSchema": {
                "type": "object",
                "properties": { "id": { "type": "integer" } },
                "required": ["id"]
            }
        },
        {
            "name": "clt_search",
            "description": "Search every branch for tasks matching text in their title, \
                            note or file path.",
            "inputSchema": {
                "type": "object",
                "properties": { "query": { "type": "string" } },
                "required": ["query"]
            }
        }
    ])
}

fn open(global: bool) -> Result<Store> {
    let store = if global {
        Store::open_in(Scope::Global)?
    } else {
        let cwd = std::env::current_dir().context("reading the current directory")?;
        Store::open(&cwd)?
    };
    if store.migrated {
        store.save()?;
    }
    Ok(store)
}

/// Who to attribute MCP-driven changes to.
///
/// Defaults to "agent" rather than to you: everything arriving over this
/// transport is, by construction, not a human at a terminal, and mislabelling
/// it would defeat the point of the journal.
fn actor() -> Option<String> {
    Some(
        std::env::var("CLT_ACTOR")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "agent".into()),
    )
}

fn call_tool(name: &str, args: &Value, global: bool) -> Result<String> {
    // Storage is reopened per call rather than held across the session, so a
    // task you add from your own terminal is visible to the agent immediately.
    let mut store = open(global)?;
    let now = Utc::now();
    let who = actor();

    match name {
        "clt_list" => {
            let all = args.get("all").and_then(Value::as_bool).unwrap_or(false);
            let include_done = args
                .get("include_done")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let branch = store.scope.branch().map(str::to_owned);

            let rows = store.tree(|t| {
                (all || Store::in_scope(t, branch.as_deref())) && (include_done || !t.is_done())
            });
            let payload: Vec<Value> = rows
                .iter()
                .map(|r| {
                    let mut v = serde_json::to_value(r.task).unwrap_or(Value::Null);
                    if let Some(o) = v.as_object_mut() {
                        o.insert("depth".into(), r.depth.into());
                    }
                    v
                })
                .collect();
            Ok(serde_json::to_string_pretty(&json!({
                "branch": branch,
                "tasks": payload
            }))?)
        }

        "clt_add" => {
            let title = args
                .get("title")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .context("title is required and must not be empty")?
                .to_string();

            let parent = args
                .get("parent")
                .and_then(Value::as_u64)
                .map(|v| v as u32);
            let repo_wide = args.get("repo").and_then(Value::as_bool).unwrap_or(false);

            let branch = match parent {
                Some(pid) => store.require(pid)?.branch.clone(),
                None if repo_wide => None,
                None => store.scope.branch().map(str::to_owned),
            };

            let location = args
                .get("file")
                .and_then(Value::as_str)
                .map(str::parse::<Location>)
                .transpose()
                .map_err(|e| anyhow::anyhow!("{e}"))?;

            let id = store.reserve_id();
            let mut t = Task::new(id, title, now);
            t.note = args
                .get("note")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .filter(|s| !s.trim().is_empty());
            t.parent = parent;
            t.branch = branch;
            t.location = location;
            t.actor = who.clone();
            store.insert(t.clone());
            store.save()?;

            journal::append(
                store.dir(),
                &[journal::Entry::new("add")
                    .actor(who.clone())
                    .id(id)
                    .detail(t.title.clone())
                    .branch(t.branch.as_deref())],
            );
            // Divert: our stdout is the JSON-RPC frame stream.
            hooks::fire(store.dir(), "post-add", &t, who.as_deref(), hooks::Output::Divert);

            Ok(serde_json::to_string_pretty(&t)?)
        }

        "clt_close" | "clt_start" => {
            let state = if name == "clt_close" {
                State::Done
            } else {
                State::Doing
            };
            let ids: Vec<u32> = match name {
                "clt_start" => vec![
                    args.get("id")
                        .and_then(Value::as_u64)
                        .context("id is required")? as u32,
                ],
                _ => args
                    .get("ids")
                    .and_then(Value::as_array)
                    .context("ids is required")?
                    .iter()
                    .filter_map(Value::as_u64)
                    .map(|v| v as u32)
                    .collect(),
            };

            for id in &ids {
                store.require(*id)?;
            }

            let mut touched = Vec::new();
            let mut entries = Vec::new();
            for id in &ids {
                let targets = if state == State::Done {
                    let mut all = vec![*id];
                    all.extend(store.descendants(*id));
                    all
                } else {
                    vec![*id]
                };
                for target in targets {
                    let Some(task) = store.get_mut(target) else {
                        continue;
                    };
                    if task.state == state {
                        continue;
                    }
                    task.state = state;
                    task.updated = now;
                    let snapshot = task.clone();
                    entries.push(
                        journal::Entry::new(state.as_str())
                            .actor(who.clone())
                            .id(target)
                            .detail(snapshot.title.clone())
                            .branch(snapshot.branch.as_deref()),
                    );
                    touched.push(snapshot);
                }
            }

            store.save()?;
            journal::append(store.dir(), &entries);
            for task in touched.iter().filter(|t| ids.contains(&t.id)) {
                let event = if state == State::Done {
                    "post-done"
                } else {
                    "post-start"
                };
                hooks::fire(store.dir(), event, task, who.as_deref(), hooks::Output::Divert);
            }

            Ok(serde_json::to_string_pretty(&touched)?)
        }

        "clt_search" => {
            let needle = args
                .get("query")
                .and_then(Value::as_str)
                .context("query is required")?
                .to_lowercase();

            let hits: Vec<&Task> = store
                .tasks()
                .iter()
                .filter(|t| {
                    t.title.to_lowercase().contains(&needle)
                        || t.note.as_ref().is_some_and(|n| n.to_lowercase().contains(&needle))
                        || t.location
                            .as_ref()
                            .is_some_and(|l| l.to_string().to_lowercase().contains(&needle))
                })
                .collect();

            Ok(serde_json::to_string_pretty(&hits)?)
        }

        other => anyhow::bail!("unknown tool: {other}"),
    }
}
