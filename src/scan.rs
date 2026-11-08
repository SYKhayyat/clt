//! Harvesting `TODO(comtai):` markers out of the source tree.
//!
//! This is the feature that only works because the tool is repo-scoped, and
//! it's the one that keeps the list honest: a task list drifts away from the
//! code the moment the two are maintained separately. Here the comment is the
//! source of truth — write the marker, and the task appears; delete the marker,
//! and the task closes.
//!
//! File enumeration is delegated to `git ls-files`, which gets us correct
//! `.gitignore` semantics (including nested ignore files, negations and the
//! global excludes file) for free. Reimplementing that with a directory walker
//! is a well-known way to spend a weekend and still scan `node_modules`.

use anyhow::{Context, Result, bail};
use std::process::{Command, Stdio};

use crate::git::Repo;

/// Markers we recognise. The `(comtai)` qualifier is mandatory: harvesting every
/// bare `TODO` in a real codebase would produce hundreds of tasks nobody asked
/// for and instantly discredit the feature.
const MARKERS: &[&str] = &["TODO(comtai)", "FIXME(comtai)"];

/// Files above this size are skipped. Source files are not 2MB; things that are
/// tend to be vendored bundles, fixtures or lockfiles.
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    /// Stable identity, derived from the marker text.
    pub key: String,
    pub title: String,
    pub file: String,
    pub line: u32,
}

/// Scans the working tree for markers.
///
/// `patterns` is the `.comtaiignore` set. It is applied here, with [`wants`],
/// before anything is opened: the caller used to collect every hit and throw
/// the excluded ones away afterwards, which meant reading every file in
/// `git ls-files` — including whatever a project keeps in the tree but out of
/// the scan — to discard most of the answers (#3). Same hits, less reading.
pub fn scan(repo: &Repo, patterns: &[String]) -> Result<Vec<Hit>> {
    let files = list_files(repo)?;
    let mut hits = Vec::new();

    for rel in files {
        if !wants(&rel, patterns) {
            continue;
        }
        let abs = repo.root.join(&rel);
        let Ok(meta) = std::fs::metadata(&abs) else {
            continue; // deleted between listing and reading
        };
        if !meta.is_file() || meta.len() > MAX_FILE_BYTES {
            continue;
        }
        // Invalid UTF-8 means binary, near enough. `read_to_string` failing is
        // the cheapest binary check available and it can't false-positive on
        // text we could have parsed.
        let Ok(text) = std::fs::read_to_string(&abs) else {
            continue;
        };
        if text.as_bytes().contains(&0) {
            continue;
        }
        hits.extend(scan_text(&text, &rel));
    }

    // Deterministic order so `comtai scan` produces the same ids run to run.
    hits.sort_by(|a, b| (&a.file, a.line).cmp(&(&b.file, b.line)));

    // Collapse duplicates, keeping the first by sort order. Markers sharing a
    // key are one task by design; without this, each duplicate would rewrite
    // the task's location and every scan would report a spurious move.
    let mut seen = std::collections::HashSet::new();
    hits.retain(|h| seen.insert(h.key.clone()));
    Ok(hits)
}

/// Extracts markers from one file's contents. Split out so it's testable
/// without a repo on disk.
pub fn scan_text(text: &str, file: &str) -> Vec<Hit> {
    let mut hits = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        let Some(body) = find_marker(line) else {
            continue;
        };
        let title = clean(body);
        if title.is_empty() {
            continue; // a bare `TODO(comtai):` with no text says nothing
        }
        hits.push(Hit {
            key: key_for(&title),
            title,
            file: file.replace('\\', "/"),
            line: idx as u32 + 1,
        });
    }
    hits
}

/// Comment openers we recognise, longest-first where they share a suffix.
///
/// Covers `//`, `#`, `/* */`, javadoc continuation `*`, HTML/JSX `<!--`,
/// SQL/Lua/Haskell `--`, Lisp `;`, TeX `%`, and Python docstrings.
const COMMENT_OPENERS: &[&str] = &[
    "//", "#", "/*", "*", "<!--", "--", ";", "%", "\"\"\"", "'''",
];

/// True when the text before the marker leaves us inside a string literal.
///
/// Counts unescaped double quotes: an odd number means the marker is quoted,
/// and quoted text is data, not a comment.
///
/// Only `"` is counted. Apostrophes were tried and rejected: prose like
/// "it's broken, fix the retry" is ordinary comment text that a `'` counter
/// would silently discard, and a false negative in a scanner is worse than the
/// rare false positive of a shell script that single-quotes a marker.
///
/// Raw string literals (`r#"..."#`) defeat quote counting by design. That's a
/// known limit, and the escape hatch is `.comtaiignore` — see [`load_ignores`].
fn inside_string_literal(prefix: &str) -> bool {
    let bytes = prefix.as_bytes();
    let mut quotes = 0usize;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 1, // skip whatever is escaped, including \"
            b'"' => quotes += 1,
            _ => {}
        }
        i += 1;
    }
    quotes % 2 == 1
}

/// True when everything before the marker on this line is comment syntax.
///
/// This is what stops the scanner harvesting its own test fixtures. Two rules,
/// because either alone is defeated:
///
/// * The prefix must *contain* a comment opener — rejects `foo(MARKER)` while
///   still accepting `// some prose, then the marker`, which is ordinary.
///   "Ends with" was tried first and wrongly rejected every marker that had
///   any comment text in front of it.
/// * The marker must not be inside a string literal — rejects
///   `scan_text("// ...")` fixtures, where the prefix *does* contain `//` but
///   only because the comment opener is itself part of the quoted data.
///
/// Without both, any codebase that tests or documents the marker files garbage
/// tasks against itself, titled with whatever the rest of the source line said.
fn opens_comment(prefix: &str) -> bool {
    if inside_string_literal(prefix) {
        return false;
    }
    let trimmed = prefix.trim_end();
    // Start of line, or inside a block comment whose opener was on an earlier
    // line: nothing in front, so nothing disqualifying.
    if trimmed.trim().is_empty() {
        return true;
    }
    COMMENT_OPENERS.iter().any(|o| trimmed.contains(o))
}

/// Returns the text following a marker on this line, if there is one.
fn find_marker(line: &str) -> Option<&str> {
    for marker in MARKERS {
        let Some(at) = line.find(marker) else {
            continue;
        };
        if !opens_comment(&line[..at]) {
            continue;
        }
        let rest = &line[at + marker.len()..];
        // Require a separator so `TODO(comtai)x` isn't treated as a marker.
        let rest = rest.strip_prefix(':').unwrap_or(rest);
        if rest.is_empty() || rest.starts_with([' ', '\t']) {
            return Some(rest);
        }
    }
    None
}

/// Patterns from a repo-root `.comtaiignore`, listing paths the scanner skips.
///
/// At the repo root and committable, unlike `.comtai/` — excluding your test
/// fixtures and docs from harvesting is a decision the whole team shares, not a
/// per-clone preference.
pub fn load_ignores(root: &std::path::Path) -> Vec<String> {
    let Ok(raw) = std::fs::read_to_string(root.join(".comtaiignore")) else {
        return Vec::new();
    };
    raw.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| l.replace('\\', "/"))
        .collect()
}

/// Matches a repo-relative path against a `.comtaiignore` pattern.
///
/// Deliberately simple: exact match, directory prefix (`vendor/`), and a
/// trailing `*` wildcard. Not gitignore semantics — this is a short opt-out
/// list, and anything needing real globbing should live in `.gitignore`, which
/// the scanner already honours via `git ls-files`.
pub fn is_ignored(path: &str, patterns: &[String]) -> bool {
    patterns.iter().any(|p| match p.strip_suffix('*') {
        Some(prefix) => path.starts_with(prefix),
        None => {
            path == p
                || path.starts_with(&format!("{}/", p.trim_end_matches('/')))
        }
    })
}

/// Strips comment punctuation that would otherwise end up in the task title.
fn clean(s: &str) -> String {
    let mut t = s.trim();
    // Closing delimiters for block comments, JSX/HTML comments, docstrings.
    for tail in ["*/", "-->", "\"\"\"", "'''", "#}", "--}}"] {
        if let Some(stripped) = t.strip_suffix(tail) {
            t = stripped.trim_end();
        }
    }
    // Collapse runs of whitespace so re-indenting a comment doesn't change the
    // task's identity.
    t.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Identity of a marker, from its text alone.
///
/// Text-only — not text-plus-path — so that moving a comment to another file
/// updates the task's location instead of closing one task and opening a
/// duplicate. The trade is that two files carrying byte-identical marker text
/// collapse to a single task, which in practice means: write markers that say
/// something specific.
pub fn key_for(title: &str) -> String {
    let normalized: String = title
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    format!("{:016x}", fnv1a(normalized.as_bytes()))
}

/// FNV-1a, hand-rolled on purpose.
///
/// This value is written to disk and compared across runs and machines, so it
/// must be stable forever. `DefaultHasher` explicitly does not guarantee that
/// between Rust releases, which would silently orphan every scanned task on a
/// toolchain upgrade.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    hash
}

/// Every file git considers part of the working tree: tracked, plus untracked
/// files that aren't ignored.
fn list_files(repo: &Repo) -> Result<Vec<String>> {
    let out = Command::new("git")
        .args([
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ])
        .current_dir(&repo.root)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .context("running `git ls-files` (is git on PATH?)")?;

    if !out.status.success() {
        bail!("`git ls-files` failed — comtai scan needs a working git repository");
    }

    // NUL-separated: filenames may contain newlines, and git will happily
    // quote-escape them into unusable garbage without -z.
    Ok(String::from_utf8_lossy(&out.stdout)
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect())
}

/// True if `path` is comtai's own metadata, which is never scanned.
///
/// `.comtai/` holds task titles, which would otherwise harvest themselves.
/// `.comtaiignore` is exempt because it documents the marker syntax — the file
/// whose job is suppressing false positives should not be the source of one.
pub fn is_own_storage(path: &str) -> bool {
    path == ".comtai" || path.starts_with(".comtai/") || path == ".comtaiignore"
}

/// Whether a listed file is one the scan is supposed to read at all.
///
/// The two exclusions are the same file-level ones the caller used to apply to
/// the finished hits. They live here because the answer decides whether to open
/// the file, and a filter that runs after the read is a filter that paid for
/// the read (#3).
fn wants(rel: &str, patterns: &[String]) -> bool {
    !is_own_storage(rel) && !is_ignored(rel, patterns)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_a_plain_marker() {
        let hits = scan_text("// TODO(comtai): retry on 429 too\n", "src/http.rs");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "retry on 429 too");
        assert_eq!(hits[0].line, 1);
        assert_eq!(hits[0].file, "src/http.rs");
    }

    #[test]
    fn ignores_bare_todos() {
        // The whole feature dies if this ever returns a hit.
        let hits = scan_text("// TODO: everything\n# TODO fix\n", "a.rs");
        assert!(hits.is_empty(), "unqualified TODOs must not be harvested");
    }

    #[test]
    fn requires_a_separator_after_the_marker() {
        assert!(scan_text("// TODO(comtai)x: nope\n", "a.rs").is_empty());
    }

    #[test]
    fn accepts_fixme_and_reports_line_numbers() {
        let src = "one\ntwo\n/* FIXME(comtai) the parser eats commas */\n";
        let hits = scan_text(src, "p.rs");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].line, 3);
        assert_eq!(hits[0].title, "the parser eats commas");
    }

    #[test]
    fn strips_block_and_markup_comment_tails() {
        assert_eq!(scan_text("/* TODO(comtai): a */", "x")[0].title, "a");
        assert_eq!(scan_text("<!-- TODO(comtai): b -->", "x")[0].title, "b");
    }

    #[test]
    fn skips_markers_with_nothing_to_say() {
        assert!(scan_text("// TODO(comtai):\n", "x").is_empty());
    }

    #[test]
    fn key_is_stable_across_reindentation_and_case() {
        // Reformatting a file must not orphan its task.
        let a = key_for("Retry on 429 too");
        let b = key_for("retry   on 429    too");
        assert_eq!(a, b);
    }

    #[test]
    fn key_differs_for_different_text() {
        assert_ne!(key_for("retry on 429"), key_for("retry on 503"));
    }

    #[test]
    fn key_is_pinned_so_upgrades_do_not_orphan_tasks() {
        // If this assertion ever fails, every scanned task in every existing
        // install just lost its identity. Change the algorithm only with a
        // format-version bump and a migration.
        assert_eq!(key_for("retry on 429 too"), "d441342cf10dab0d");
    }

    #[test]
    fn markers_inside_string_literals_are_not_harvested() {
        // The bug this prevents: comtai scanning its own test fixtures and filing
        // tasks titled `retry on 429 too\n", "src/http.rs");`.
        let src = r#"    let hits = scan_text("// TODO(comtai): retry on 429 too\n", "a.rs");"#;
        assert!(
            scan_text(src, "src/scan.rs").is_empty(),
            "a marker in a string literal is code, not a comment"
        );
    }

    #[test]
    fn quote_counting_respects_escapes() {
        assert!(!inside_string_literal("let x = \"a\";  // "));
        assert!(inside_string_literal("let x = \"open "));
        // An escaped quote doesn't close the literal.
        assert!(inside_string_literal("let x = \"he said \\\" "));
    }

    #[test]
    fn a_comment_containing_a_quoted_phrase_still_scans() {
        // Balanced quotes before the marker must not disqualify a real comment.
        let hits = scan_text("// the \"auth\" flow: TODO(comtai): fix the retry", "a.rs");
        assert_eq!(hits.len(), 1, "balanced quotes are not a string literal");
    }

    #[test]
    fn duplicate_markers_collapse_to_one_task() {
        // Two identical markers in one file share a key; the scanner must not
        // report the second as a move of the first, forever.
        let src = "// TODO(comtai): same\n// TODO(comtai): same\n";
        let hits = scan_text(src, "a.rs");
        assert_eq!(hits.len(), 2, "scan_text reports raw hits");
        assert_eq!(hits[0].key, hits[1].key, "and they share an identity");
    }

    #[test]
    fn prose_mentions_of_the_marker_are_not_harvested() {
        // Documentation that names the marker shouldn't file a task about it.
        let src = "//! Harvesting `TODO(comtai):` markers out of the source tree.";
        assert!(scan_text(src, "src/scan.rs").is_empty());
    }

    #[test]
    fn recognises_the_comment_styles_people_actually_use() {
        for line in [
            "// TODO(comtai): a",
            "  # TODO(comtai): a",
            "/* TODO(comtai): a */",
            " * TODO(comtai): a",
            "<!-- TODO(comtai): a -->",
            "-- TODO(comtai): a",
            "; TODO(comtai): a",
            "% TODO(comtai): a",
        ] {
            assert_eq!(
                scan_text(line, "f").len(),
                1,
                "should have matched: {line}"
            );
        }
    }

    #[test]
    fn comtai_never_scans_its_own_metadata() {
        assert!(is_own_storage(".comtai/tasks.json"));
        assert!(is_own_storage(".comtaiignore"));
        assert!(!is_own_storage("src/comtai.rs"));
    }

    #[test]
    fn comtaiignore_matches_files_directories_and_wildcards() {
        let patterns = vec![
            "README.md".to_string(),
            "vendor/".to_string(),
            "tests/fixtures*".to_string(),
        ];
        assert!(is_ignored("README.md", &patterns));
        assert!(is_ignored("vendor/junk.js", &patterns));
        assert!(is_ignored("tests/fixtures/a.rs", &patterns));
        assert!(!is_ignored("src/main.rs", &patterns));
        // A prefix must stop at a path boundary, or `vendor` would also
        // swallow `vendored-thing/`.
        assert!(!is_ignored("vendorish/a.rs", &["vendor".to_string()]));
    }

    /// The two exclusions decide whether a file is opened, so they are checked
    /// here rather than on the hits a read has already produced (#3).
    #[test]
    fn excludes_own_storage_and_ignored_paths_before_reading() {
        let patterns = vec!["vendor/".to_string(), "*.min.js".to_string()];

        assert!(wants("src/http.rs", &patterns));

        // comtai's own metadata, which would otherwise harvest itself.
        assert!(!wants(".comtai/tasks.json", &patterns));
        assert!(!wants(".comtai/node_modules/x.js", &patterns));

        // And `.comtaiignore` itself, because it documents the marker syntax and
        // the file that suppresses false positives must not be one.
        assert!(!wants(".comtaiignore", &patterns));

        assert!(!wants("vendor/lib.js", &patterns));
    }

    #[test]
    fn moving_a_marker_between_files_keeps_its_identity() {
        let a = &scan_text("// TODO(comtai): same text", "old.rs")[0];
        let b = &scan_text("// TODO(comtai): same text", "new.rs")[0];
        assert_eq!(a.key, b.key);
        assert_ne!(a.file, b.file);
    }
}
