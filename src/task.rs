//! The on-disk task model.
//!
//! Everything here is serialized straight into `.clt/tasks.json`, which is a
//! documented, agent-writable format rather than an internal detail. That means
//! two things for anyone editing this file: new fields must be `Option` or have
//! `#[serde(default)]` so older files still load, and nothing may assume the
//! data was written by us. A human or an agent may have hand-edited it.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// Where a task sits in its lifecycle.
///
/// Deliberately three states. Every extra state is a decision the user has to
/// make on every task forever, and "blocked" is usually better expressed as a
/// subtask that isn't done yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    #[default]
    Todo,
    Doing,
    Done,
}

impl State {
    /// The glyph shown in list output. Three columns wide once padded.
    pub fn glyph(self) -> &'static str {
        match self {
            State::Todo => "○",
            State::Doing => "●",
            State::Done => "✓",
        }
    }

    /// Sort weight for the default view: in-progress work floats to the top,
    /// finished work sinks.
    pub fn rank(self) -> u8 {
        match self {
            State::Doing => 0,
            State::Todo => 1,
            State::Done => 2,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            State::Todo => "todo",
            State::Doing => "doing",
            State::Done => "done",
        }
    }
}

impl fmt::Display for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for State {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Generous on input: these get typed by hand many times a day, and the
        // legacy names appear in files written by clt 0.0.x.
        match s.trim().to_ascii_lowercase().as_str() {
            "todo" | "t" | "open" | "notstarted" | "not-started" => Ok(State::Todo),
            "doing" | "d" | "wip" | "started" | "inprogress" | "in-progress" => Ok(State::Doing),
            "done" | "x" | "closed" | "finished" | "complete" => Ok(State::Done),
            other => Err(format!(
                "unknown state {other:?} (expected one of: todo, doing, done)"
            )),
        }
    }
}

/// A point in the source tree a task is about.
///
/// Stored structurally rather than as a `"file:line"` string so that consumers
/// of `--json` don't have to parse it back apart, and so a Windows drive-letter
/// path can never be mistaken for a line number.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Location {
    /// Always repo-relative and slash-separated, so the file reads identically
    /// on every platform that shares the repo.
    pub file: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
}

impl Location {
    pub fn new(file: impl Into<String>, line: Option<u32>) -> Self {
        Self {
            file: file.into().replace('\\', "/"),
            line,
        }
    }
}

impl fmt::Display for Location {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.line {
            Some(line) => write!(f, "{}:{}", self.file, line),
            None => f.write_str(&self.file),
        }
    }
}

impl FromStr for Location {
    type Err = String;

    /// Parses `src/auth.rs:88`, `src/auth.rs`, or `C:\src\auth.rs:88`.
    ///
    /// Splits on the *last* colon and only treats it as a line number if what
    /// follows is entirely digits, which keeps drive letters and colons in
    /// filenames intact.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if s.is_empty() {
            return Err("empty location".into());
        }
        if let Some((file, line)) = s.rsplit_once(':')
            && !line.is_empty()
            && line.chars().all(|c| c.is_ascii_digit())
            && !file.is_empty()
        {
            let line = line
                .parse()
                .map_err(|_| format!("line number out of range in {s:?}"))?;
            return Ok(Location::new(file, Some(line)));
        }
        Ok(Location::new(s, None))
    }
}

/// How a task came to exist.
///
/// The distinction matters for `clt scan`: scanned tasks are owned by the
/// source comment that produced them and get closed when it disappears, while
/// manual tasks are owned by you and are never touched by a scan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Origin {
    /// Created by a human or an agent through the CLI or MCP.
    Manual,
    /// Harvested from a `TODO(clt):` marker in the source.
    Scan {
        /// Identity of the marker, derived from its normalized text rather than
        /// its position, so a task survives the comment moving down the file.
        key: String,
    },
}

impl Default for Origin {
    fn default() -> Self {
        Origin::Manual
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: u32,
    pub title: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,

    #[serde(default)]
    pub state: State,

    /// Parent task, for arbitrary-depth nesting. `None` is a root task.
    ///
    /// Stored as a parent pointer rather than nested children so that every
    /// task is addressable by a flat, stable id and a reparent is a one-field
    /// write. The tree is rebuilt at render time; see [`crate::store::Store::tree`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<u32>,

    /// Branch this task belongs to. `None` means repo-wide: visible from every
    /// branch. Recorded as data rather than inferred from git so that deleting
    /// a branch orphans a task instead of vaporizing it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<Location>,

    /// Who filed it. `None` is you; `Some("claude")` renders as a dim tag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,

    #[serde(default)]
    pub origin: Origin,

    /// Commit that closed this task, when it was closed by commit linkage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_by: Option<String>,

    pub created: DateTime<Utc>,
    pub updated: DateTime<Utc>,
}

impl Task {
    pub fn new(id: u32, title: impl Into<String>, now: DateTime<Utc>) -> Self {
        Self {
            id,
            title: title.into(),
            note: None,
            state: State::Todo,
            parent: None,
            branch: None,
            location: None,
            actor: None,
            origin: Origin::Manual,
            closed_by: None,
            created: now,
            updated: now,
        }
    }

    pub fn is_done(&self) -> bool {
        self.state == State::Done
    }

    /// The scan marker key, if this task is owned by a source comment.
    pub fn scan_key(&self) -> Option<&str> {
        match &self.origin {
            Origin::Scan { key } => Some(key.as_str()),
            Origin::Manual => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn location_parses_line_suffix() {
        let loc: Location = "src/auth.rs:88".parse().unwrap();
        assert_eq!(loc.file, "src/auth.rs");
        assert_eq!(loc.line, Some(88));
        assert_eq!(loc.to_string(), "src/auth.rs:88");
    }

    #[test]
    fn location_without_line_round_trips() {
        let loc: Location = "src/auth.rs".parse().unwrap();
        assert_eq!(loc.line, None);
        assert_eq!(loc.to_string(), "src/auth.rs");
    }

    #[test]
    fn location_keeps_windows_drive_letters() {
        // The naive `split(':')` version of this turns C: into a filename and
        // `\src\auth.rs:88` into garbage.
        let loc: Location = r"C:\src\auth.rs:88".parse().unwrap();
        assert_eq!(loc.file, "C:/src/auth.rs");
        assert_eq!(loc.line, Some(88));
    }

    #[test]
    fn location_backslashes_are_normalized() {
        let loc: Location = r"src\http\client.rs".parse().unwrap();
        assert_eq!(loc.file, "src/http/client.rs");
    }

    #[test]
    fn state_accepts_legacy_names() {
        // Files written by the pre-0.1 version used these.
        assert_eq!("NotStarted".parse::<State>().unwrap(), State::Todo);
        assert_eq!("InProgress".parse::<State>().unwrap(), State::Doing);
        assert_eq!("Finished".parse::<State>().unwrap(), State::Done);
    }

    #[test]
    fn state_rejects_nonsense_with_a_useful_message() {
        let err = "eventually".parse::<State>().unwrap_err();
        assert!(err.contains("todo, doing, done"), "unhelpful error: {err}");
    }
}
