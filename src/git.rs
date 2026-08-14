//! Git discovery, by shelling out to `git`.
//!
//! No `git2`/`gix` dependency on purpose. This tool's entire audience already
//! has git on PATH, the three commands below are stable across every git
//! version anyone is running, and linking libgit2 to answer "what branch am I
//! on" would be the tail wagging the dog. If git is missing we degrade to the
//! global task list rather than failing.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Debug, Clone)]
pub struct Repo {
    /// Worktree root. For a linked worktree this is that worktree, not the
    /// primary one, which is what you want: tasks follow the checkout.
    pub root: PathBuf,
    /// Shared git dir. For a linked worktree this is the *common* dir, so the
    /// `info/exclude` entry we write is shared by every worktree.
    pub git_common_dir: PathBuf,
    /// Current branch, or `None` when HEAD is detached.
    pub branch: Option<String>,
}

/// Runs a git command in `cwd`, returning trimmed stdout on success.
///
/// Returns `Ok(None)` for a non-zero exit (the "not a repo" / "detached HEAD"
/// signal) and `Err` only when git could not be run at all.
fn git(cwd: &Path, args: &[&str]) -> Result<Option<String>> {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();

    let out = match out {
        Ok(out) => out,
        // git not installed, or not on PATH. Not fatal: caller falls back.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).context("failed to run git"),
    };

    if !out.status.success() {
        return Ok(None);
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() { Ok(None) } else { Ok(Some(s)) }
}

/// Turns a path as git spells it into one the platform spells the same way.
///
/// Git answers in forward slashes everywhere, including on Windows, where
/// everything we join onto its answer uses a backslash. Left alone the two meet
/// inside a single string — `C:/code/app\.clt\tasks.json` — and that string is
/// what `clt path` prints and what `clt path --json` hands to whatever is
/// consuming it. Both separators work when *opening* the file, so this is
/// cosmetic; it is fixed at the boundary anyway, because the alternative is
/// every printing site remembering to tidy up after this one.
///
/// A no-op off Windows, where `/` is already the separator, and safe on it:
/// `/` is not a legal character in a Windows filename.
fn native(path: &str) -> PathBuf {
    #[cfg(windows)]
    {
        PathBuf::from(path.replace('/', "\\"))
    }
    #[cfg(not(windows))]
    {
        PathBuf::from(path)
    }
}

/// Finds the repo containing `cwd`, if any.
///
/// Process spawns dominate this tool's startup — on Windows each `git`
/// invocation costs tens of milliseconds, and `clt` runs dozens of times a day
/// — so the two path queries are answered by a single `rev-parse`, which
/// returns one result per line.
///
/// Deliberately *not* folded into the same call as the branch lookup:
/// `rev-parse --symbolic-full-name HEAD` exits 128 on an unborn HEAD, and a
/// non-zero exit here would make a freshly-`git init`ed repo look like no repo
/// at all.
pub fn discover(cwd: &Path) -> Result<Option<Repo>> {
    let Some(out) = git(cwd, &["rev-parse", "--show-toplevel", "--git-common-dir"])? else {
        return Ok(None);
    };
    let mut lines = out.lines();

    let Some(root) = lines.next().map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    let root = native(root);

    // Git prints `--git-common-dir` relative to the *current directory*, so it
    // must be resolved against `cwd`, not the worktree root — those differ
    // whenever clt is run from a subdirectory.
    let common = lines
        .next()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(".git");
    let common = {
        let p = native(common);
        if p.is_absolute() { p } else { cwd.join(p) }
    };

    // `symbolic-ref` fails on detached HEAD, which is exactly the signal we
    // want. `rev-parse --abbrev-ref HEAD` would return the literal string
    // "HEAD" instead and we'd have to special-case a branch legitimately named
    // HEAD, which git does allow.
    let branch = git(cwd, &["symbolic-ref", "--quiet", "--short", "HEAD"])?;

    Ok(Some(Repo {
        root,
        git_common_dir: common,
        branch,
    }))
}

impl Repo {
    /// Ensures `.clt/` is ignored locally, via `.git/info/exclude`.
    ///
    /// Deliberately not `.gitignore`: that file is committed, and silently
    /// editing a tracked file to install our own storage would be rude and
    /// would show up in someone's PR. `info/exclude` is per-clone and
    /// untracked. Anyone who *wants* to commit their task list can delete the
    /// line, and we won't put it back (we only append when it's absent).
    ///
    /// Best-effort: a read-only or exotic git dir is not a reason to fail a
    /// `clt add`.
    pub fn ensure_excluded(&self) {
        let info = self.git_common_dir.join("info");
        let exclude = info.join("exclude");

        let current = std::fs::read_to_string(&exclude).unwrap_or_default();
        if current
            .lines()
            .any(|l| matches!(l.trim(), ".clt/" | ".clt" | "/.clt/" | "/.clt"))
        {
            return;
        }

        if std::fs::create_dir_all(&info).is_err() {
            return;
        }

        let mut next = current;
        if !next.is_empty() && !next.ends_with('\n') {
            next.push('\n');
        }
        next.push_str("\n# clt task list (local only; delete this line to commit it)\n.clt/\n");
        let _ = std::fs::write(&exclude, next);
    }

    /// Undoes [`Self::ensure_excluded`], so the task list can be committed.
    ///
    /// Removes our line and the comment that introduces it, and nothing else —
    /// `info/exclude` may well contain patterns somebody else put there, and
    /// rewriting the whole file would be a fine way to lose them.
    pub fn unexclude(&self) -> Result<bool> {
        let exclude = self.git_common_dir.join("info").join("exclude");
        let Ok(current) = std::fs::read_to_string(&exclude) else {
            return Ok(false);
        };

        let mut kept: Vec<&str> = Vec::new();
        let mut removed = false;
        for line in current.lines() {
            let t = line.trim();
            if matches!(t, ".clt/" | ".clt" | "/.clt/" | "/.clt") {
                removed = true;
                continue;
            }
            // Our own comment, which would otherwise be left dangling above
            // whatever pattern happens to follow it.
            if t.starts_with("# clt task list") {
                removed = true;
                continue;
            }
            kept.push(line);
        }
        if !removed {
            return Ok(false);
        }

        let mut next = kept.join("\n");
        if !next.is_empty() && !next.ends_with('\n') {
            next.push('\n');
        }
        std::fs::write(&exclude, next)
            .with_context(|| format!("rewriting {}", exclude.display()))?;
        Ok(true)
    }

    /// True when git is tracking `rel` (repo-relative, slash-separated).
    ///
    /// This is how "is the task list shared?" is answered. Deriving it from git
    /// rather than storing a flag of our own means the answer cannot drift from
    /// reality — someone who runs `git rm --cached` by hand has unshared the
    /// list, and clt should agree with them.
    pub fn is_tracked(&self, rel: &str) -> bool {
        matches!(
            git(&self.root, &["ls-files", "--error-unmatch", "--", rel]),
            Ok(Some(_))
        )
    }

    /// True when `rel` is ignored, by any mechanism: `.gitignore`, a nested
    /// ignore file, the global excludes, or `info/exclude`.
    ///
    /// Needed because clt only ever writes to `info/exclude`, and cannot undo an
    /// entry in a *committed* `.gitignore` — that file belongs to the project,
    /// not to us. Detecting it is what lets `clt share` say so instead of
    /// appearing to work and silently doing nothing.
    pub fn ignore_source(&self, rel: &str) -> Option<String> {
        // `check-ignore -v` prints `<source>:<line>:<pattern>\t<path>`.
        let out = git(&self.root, &["check-ignore", "-v", "--", rel]).ok().flatten()?;
        let first = out.lines().next()?;
        let source = first.split(':').next()?.trim();
        (!source.is_empty()).then(|| source.to_string())
    }

    /// Commits reachable from HEAD but not from `since`, oldest first.
    ///
    /// Used by commit linkage. `since` is typically the last commit we scanned.
    pub fn commits_since(&self, since: Option<&str>) -> Result<Vec<Commit>> {
        let range = match since {
            Some(sha) => format!("{sha}..HEAD"),
            None => "HEAD".to_string(),
        };
        // A NUL record separator is the only delimiter guaranteed not to appear
        // in a commit message.
        let fmt = "--format=%H%x1f%s%x1f%b%x1e";
        let args: Vec<&str> = if since.is_some() {
            vec!["log", "--reverse", fmt, &range]
        } else {
            // Without a floor, don't walk the entire history of the repo.
            vec!["log", "--reverse", "-n", "50", fmt, &range]
        };

        let Some(out) = git(&self.root, &args)? else {
            return Ok(Vec::new());
        };

        let mut commits = Vec::new();
        for record in out.split('\x1e') {
            let record = record.trim_start_matches(['\n', '\r']);
            if record.trim().is_empty() {
                continue;
            }
            let mut parts = record.split('\x1f');
            let (Some(sha), Some(subject)) = (parts.next(), parts.next()) else {
                continue;
            };
            let body = parts.next().unwrap_or("");
            commits.push(Commit {
                sha: sha.trim().to_string(),
                message: format!("{subject}\n{body}"),
            });
        }
        Ok(commits)
    }

    /// Every local branch name.
    ///
    /// Used to find tasks stranded on branches that have been merged and
    /// deleted. Local heads only: a task filed on a branch that now exists only
    /// on the remote is still orphaned as far as this checkout is concerned,
    /// which is the question being asked.
    pub fn branches(&self) -> std::collections::HashSet<String> {
        let Ok(Some(out)) = git(
            &self.root,
            &["for-each-ref", "--format=%(refname:short)", "refs/heads/"],
        ) else {
            return std::collections::HashSet::new();
        };
        out.lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_owned)
            .collect()
    }

    /// Resolves HEAD to a full sha, if there is one (a fresh repo has none).
    pub fn head(&self) -> Result<Option<String>> {
        git(&self.root, &["rev-parse", "HEAD"])
    }
}

#[derive(Debug, Clone)]
pub struct Commit {
    pub sha: String,
    pub message: String,
}

impl Commit {
    pub fn short(&self) -> &str {
        &self.sha[..self.sha.len().min(7)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_git_path_joins_without_mixing_separators() {
        // The shape git hands us on Windows. Whatever we join onto it has to
        // come out in one separator style, because this string gets printed.
        let joined = native("C:/code/app").join(".clt").join("tasks.json");
        let shown = joined.display().to_string();
        assert!(
            !(shown.contains('/') && shown.contains('\\')),
            "mixed separators in {shown:?}"
        );
        assert!(shown.ends_with("tasks.json"));
    }

    #[test]
    fn a_git_path_still_points_at_the_same_place() {
        // Normalising must not change which file is meant, only how it reads.
        assert_eq!(native("C:/code/app"), PathBuf::from("C:/code/app"));
        assert_eq!(native("/home/x/app"), PathBuf::from("/home/x/app"));
    }

    #[test]
    fn a_relative_git_dir_is_normalised_too() {
        // `--git-common-dir` comes back relative in a plain repo.
        assert_eq!(native(".git"), PathBuf::from(".git"));
        let shown = native("../.git/worktrees/x").display().to_string();
        assert!(!(shown.contains('/') && shown.contains('\\')), "{shown:?}");
    }

    #[test]
    fn short_sha_survives_a_short_sha() {
        // Slicing a 40-char sha to 7 is obvious; slicing a hand-written 4-char
        // one to 7 is a panic, which is why `short` takes a min.
        let c = Commit {
            sha: "abc".into(),
            message: String::new(),
        };
        assert_eq!(c.short(), "abc");
    }
}
