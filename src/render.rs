//! Terminal output.
//!
//! Everything goes through `anstream`, which strips ANSI when stdout isn't a
//! terminal and honours `NO_COLOR`/`CLICOLOR_FORCE`. That's not politeness —
//! it's what makes `clt ls | grep` and `clt ls > file` produce something a
//! human or a script can actually read.

use anstyle::{AnsiColor, Color, Style};
use chrono::{DateTime, Local, Utc};
use unicode_width::UnicodeWidthStr;

use crate::store::{Row, Store};
use crate::task::{State, Task};

fn dim() -> Style {
    Style::new().dimmed()
}
fn bold() -> Style {
    Style::new().bold()
}
fn fg(c: AnsiColor) -> Style {
    Style::new().fg_color(Some(Color::Ansi(c)))
}

fn state_style(state: State) -> Style {
    match state {
        State::Todo => Style::new(),
        State::Doing => fg(AnsiColor::Yellow),
        State::Done => fg(AnsiColor::Green),
    }
}

/// Wraps `text` in `style`. Kept as one helper so no call site has to remember
/// to emit the reset.
fn paint(style: Style, text: &str) -> String {
    format!("{}{}{}", style.render(), text, style.render_reset())
}

pub fn warn(msg: &str) {
    let style = fg(AnsiColor::Yellow);
    let _ = anstream::eprintln!("{}", paint(style, &format!("clt: {msg}")));
}

pub fn note(msg: &str) {
    let _ = anstream::eprintln!("{}", paint(dim(), &format!("clt: {msg}")));
}

/// Compact relative time: `now`, `5m`, `3h`, `2d`, `6w`.
///
/// Two characters wide in the common case, because this column is glanced at,
/// not read. Anything past a year is capped rather than growing the column.
pub fn ago(then: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let secs = (now - then).num_seconds();
    if secs < 0 {
        // Clock skew, or a hand-edited timestamp from the future.
        return "now".into();
    }
    match secs {
        s if s < 60 => "now".to_string(),
        s if s < 3_600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3_600),
        s if s < 604_800 => format!("{}d", s / 86_400),
        s if s < 31_536_000 => format!("{}w", s / 604_800),
        _ => "1y+".to_string(),
    }
}

/// Width in terminal cells.
///
/// Not a char count: CJK ideographs, kana, Hangul and fullwidth forms take two
/// cells, combining marks take none. Counting chars pads those rows short and
/// shears every column to their right out of line.
///
/// `width()` resolves East Asian *Ambiguous* characters — our `○ ● ✓` glyphs
/// among them — as one cell, which is what a Western terminal does. The
/// `width_cjk()` variant is deliberately not used: it would fix those glyphs
/// for CJK-locale terminals and break them for everyone else.
fn width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

fn pad(s: &str, to: usize) -> String {
    let w = width(s);
    if w >= to {
        s.to_string()
    } else {
        format!("{s}{}", " ".repeat(to - w))
    }
}

pub struct ListOpts {
    pub now: DateTime<Utc>,
    /// Show which branch each task belongs to. On by default for `--all` and
    /// for search, where results span branches and the column is the point.
    pub show_branch: bool,
}

/// Renders the task tree.
pub fn tasks(store: &Store, rows: &[Row<'_>], opts: &ListOpts) {
    if rows.is_empty() {
        return;
    }

    // Pre-format every cell so columns can be sized from actual content rather
    // than guessed at.
    struct Cell {
        id: String,
        body: String,
        branch: String,
        location: String,
        actor: String,
        age: String,
        style: Style,
        state: State,
    }

    let cells: Vec<Cell> = rows
        .iter()
        .map(|row| {
            let t = row.task;
            let indent = "  ".repeat(row.depth);
            let progress = match store.progress(t.id) {
                Some((done, total)) => format!("  {done}/{total}"),
                None => String::new(),
            };
            Cell {
                id: format!("#{}", t.id),
                body: format!("{indent}{} {}{progress}", t.state.glyph(), t.title),
                branch: if opts.show_branch {
                    t.branch.clone().unwrap_or_else(|| "(repo)".into())
                } else {
                    String::new()
                },
                location: t.location.as_ref().map(ToString::to_string).unwrap_or_default(),
                actor: t.actor.as_ref().map(|a| format!("[{a}]")).unwrap_or_default(),
                age: ago(t.updated, opts.now),
                // Context-only ancestors are dimmed so the eye goes straight to
                // the rows that actually matched.
                style: if row.context {
                    dim()
                } else {
                    state_style(t.state)
                },
                state: t.state,
            }
        })
        .collect();

    let id_w = cells.iter().map(|c| width(&c.id)).max().unwrap_or(0);
    let body_w = cells.iter().map(|c| width(&c.body)).max().unwrap_or(0);
    let branch_w = cells.iter().map(|c| width(&c.branch)).max().unwrap_or(0);
    let loc_w = cells.iter().map(|c| width(&c.location)).max().unwrap_or(0);
    let actor_w = cells.iter().map(|c| width(&c.actor)).max().unwrap_or(0);

    for cell in &cells {
        let mut line = String::new();
        line.push_str("  ");
        line.push_str(&paint(dim(), &pad(&cell.id, id_w)));
        line.push_str("  ");

        let body = pad(&cell.body, body_w);
        // Finished work stays legible but recedes; it's there for the
        // satisfaction, not to be read.
        let body_style = if cell.state == State::Done {
            dim()
        } else {
            cell.style
        };
        line.push_str(&paint(body_style, &body));

        for (text, w, style) in [
            (&cell.branch, branch_w, dim()),
            (&cell.location, loc_w, fg(AnsiColor::Cyan)),
            (&cell.actor, actor_w, fg(AnsiColor::Magenta)),
        ] {
            if w == 0 {
                continue;
            }
            line.push_str("  ");
            line.push_str(&paint(style, &pad(text, w)));
        }

        line.push_str("  ");
        line.push_str(&paint(dim(), &cell.age));
        let _ = anstream::println!("{}", line.trim_end());
    }
}

/// The line printed after a mutation, so you can see what you just did.
pub fn changed(task: &Task) {
    let style = state_style(task.state);
    let _ = anstream::println!(
        "  {} {} {}",
        paint(style, task.state.glyph()),
        paint(dim(), &format!("#{}", task.id)),
        if task.state == State::Done {
            paint(dim(), &task.title)
        } else {
            task.title.clone()
        },
    );
}

/// Footer summarising what's on screen and what isn't.
pub fn summary(open: usize, doing: usize, hidden_done: usize) {
    let mut parts = Vec::new();
    if doing > 0 {
        parts.push(format!("{doing} in progress"));
    }
    parts.push(format!("{open} open"));
    if hidden_done > 0 {
        parts.push(format!("{hidden_done} done (clt ls --done)"));
    }
    let _ = anstream::println!("{}", paint(dim(), &format!("  {}", parts.join(" · "))));
}

/// What to print when the list is empty, which is most people's first contact
/// with the tool. Worth more than "no tasks found".
pub fn empty(branch: Option<&str>, filtered: bool) {
    let where_ = match branch {
        Some(b) => format!("on {}", paint(bold(), b)),
        None => "here".to_string(),
    };
    if filtered {
        let _ = anstream::println!("  {}", paint(dim(), "Nothing matches that filter."));
        let _ = anstream::println!("  {}", paint(dim(), "clt ls --all    every branch, every state"));
        return;
    }
    let _ = anstream::println!("  Nothing {where_}.");
    let _ = anstream::println!();
    let _ = anstream::println!(
        "  {}   {}",
        paint(bold(), "clt add \"the thing\""),
        paint(dim(), "file a task on this branch")
    );
    let _ = anstream::println!(
        "  {}              {}",
        paint(bold(), "clt ls --all"),
        paint(dim(), "see every branch")
    );
}

/// Local-time timestamp for the journal view.
pub fn stamp(ts: DateTime<Utc>) -> String {
    ts.with_timezone(&Local).format("%Y-%m-%d %H:%M").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap()
    }

    #[test]
    fn ago_buckets_read_the_way_you_would_say_them() {
        let now = utc(2026, 8, 9, 12, 0);
        assert_eq!(ago(now, now), "now");
        assert_eq!(ago(utc(2026, 8, 9, 11, 30), now), "30m");
        assert_eq!(ago(utc(2026, 8, 9, 6, 0), now), "6h");
        assert_eq!(ago(utc(2026, 8, 6, 12, 0), now), "3d");
        assert_eq!(ago(utc(2026, 7, 12, 12, 0), now), "4w");
    }

    #[test]
    fn ago_survives_timestamps_from_the_future() {
        // Clock skew across machines, or an agent writing a bad timestamp.
        // Must not panic or print a negative age.
        let now = utc(2026, 8, 9, 12, 0);
        assert_eq!(ago(utc(2027, 1, 1, 0, 0), now), "now");
    }

    #[test]
    fn pad_never_truncates() {
        assert_eq!(pad("abc", 5), "abc  ");
        assert_eq!(pad("abcdef", 3), "abcdef");
    }

    #[test]
    fn width_counts_cells_not_chars() {
        // Four ideographs, eight cells. Counting chars was the old bug.
        assert_eq!("修复登录".chars().count(), 4);
        assert_eq!(width("修复登录"), 8);
        // Combining acute occupies no cell of its own.
        assert_eq!(width("e\u{0301}"), 1);
        // Our state glyphs stay one cell wide (East Asian Ambiguous).
        assert_eq!(width("○"), 1);
        assert_eq!(width("●"), 1);
        assert_eq!(width("✓"), 1);
    }

    #[test]
    fn pad_aligns_wide_titles() {
        // Both cells must end up occupying ten terminal columns, which means
        // the wide one gets *fewer* spaces, not the same number.
        assert_eq!(width(&pad("修复登录", 10)), 10);
        assert_eq!(width(&pad("fix login", 10)), 10);
    }
}
