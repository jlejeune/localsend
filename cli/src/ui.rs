use crate::util;
use crossterm::style::{Color, Print, Stylize};
use crossterm::terminal::{Clear, ClearType};
use crossterm::{QueueableCommand, cursor};
use std::io::{Stdout, Write, stdout};

/// The category of a log event, shown as its colored `D` / `R` / `S` tag.
#[derive(Clone, Copy, Debug)]
pub enum Category {
    Discovery,
    Receive,
    Send,
}

impl Category {
    pub fn tag(self) -> &'static str {
        match self {
            Category::Discovery => "D",
            Category::Receive => "R",
            Category::Send => "S",
        }
    }

    fn color(self) -> Color {
        match self {
            Category::Discovery => Color::Cyan,
            Category::Receive => Color::Green,
            Category::Send => Color::Magenta,
        }
    }

    pub fn colored_tag(self) -> String {
        self.tag().with(self.color()).to_string()
    }
}

/// How a [Ui] writes its output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    /// A terminal in raw mode: colored tags, `\r\n` line breaks and a status
    /// line.
    Terminal,

    /// A log stream (a pipe, a file or journald): no colors, `\n` line breaks
    /// and no status line.
    Plain,
}

/// An append-only log (like `docker logs`) with a single in-place updated
/// status line at the bottom for transfer progress.
///
/// In [Style::Terminal] the terminal is in raw mode, so every line break must
/// be `\r\n`.
pub struct Ui {
    out: Stdout,
    style: Style,
    status: Option<String>,

    /// While suspended (the file picker owns the alternate screen), log lines
    /// are buffered and flushed on [Ui::resume].
    suspended: bool,
    buffer: Vec<String>,
}

impl Ui {
    pub fn new() -> Self {
        Self::with_style(Style::Terminal)
    }

    /// A UI for the unattended mode: output goes to a log, not to a terminal.
    pub fn plain() -> Self {
        Self::with_style(Style::Plain)
    }

    fn with_style(style: Style) -> Self {
        Self {
            out: stdout(),
            style,
            status: None,
            suspended: false,
            buffer: Vec::new(),
        }
    }

    /// Stops writing to the terminal; log lines are buffered instead. The
    /// picker draws on the alternate screen, so the main screen stays intact
    /// underneath.
    pub fn suspend(&mut self) {
        self.suspended = true;
    }

    /// Resumes terminal output and flushes the buffered log lines.
    pub fn resume(&mut self) {
        self.suspended = false;
        for line in std::mem::take(&mut self.buffer) {
            let _ = self.out.queue(Print(line));
        }
        self.redraw_status();
        let _ = self.out.flush();
    }

    /// Prints a log block: the first line is prefixed with the category tag,
    /// further lines are indented below it.
    pub fn log(&mut self, category: Category, text: &str) {
        self.print_block(Some(category), text);
    }

    /// Prints a log line that is not tied to an event category.
    pub fn log_plain(&mut self, text: &str) {
        self.print_block(None, text);
    }

    fn print_block(&mut self, category: Option<Category>, text: &str) {
        self.clear_status_line();
        for formatted in format_block(category, text, self.style) {
            match self.suspended {
                true => self.buffer.push(formatted),
                false => {
                    let _ = self.out.queue(Print(formatted));
                }
            }
        }
        self.redraw_status();
        let _ = self.out.flush();
    }

    /// Replaces the status line at the bottom, or removes it with `None`.
    pub fn set_status(&mut self, status: Option<String>) {
        // A log stream cannot redraw a line in place.
        if self.style == Style::Plain || status == self.status {
            return;
        }
        self.clear_status_line();
        self.status = status;
        self.redraw_status();
        let _ = self.out.flush();
    }

    fn clear_status_line(&mut self) {
        if !self.suspended && self.status.is_some() {
            let _ = self.out.queue(cursor::MoveToColumn(0));
            let _ = self.out.queue(Clear(ClearType::CurrentLine));
        }
    }

    fn redraw_status(&mut self) {
        if self.suspended {
            return;
        }
        if let Some(status) = &self.status {
            let line = truncate_visible(status, util::terminal_width().saturating_sub(1));
            let _ = self.out.queue(Print(line));
        }
    }
}

/// Formats a log block into one string per line: the first line carries the
/// category tag, further lines are indented below it.
fn format_block(category: Option<Category>, text: &str, style: Style) -> Vec<String> {
    let line_break = match style {
        Style::Terminal => "\r\n",
        Style::Plain => "\n",
    };
    text.lines()
        .enumerate()
        .map(|(i, line)| match (i, category) {
            (0, Some(category)) => {
                let tag = match style {
                    Style::Terminal => category.colored_tag(),
                    Style::Plain => category.tag().to_string(),
                };
                format!("{tag} {line}{line_break}")
            }
            (_, Some(_)) => format!("  {line}{line_break}"),
            (_, None) => format!("{line}{line_break}"),
        })
        .collect()
}

/// A piece of a string that may contain ANSI escape sequences: either a
/// sequence, which takes no columns, or a single visible character.
enum Segment<'a> {
    Escape(&'a str),
    Visible(char),
}

/// Splits `s` into escape sequences and visible characters, so that everything
/// measuring or cutting a formatted string agrees on what occupies a column.
///
/// A trailing escape sequence that is never terminated runs to the end of `s`.
fn segments(s: &str) -> impl Iterator<Item = Segment<'_>> {
    let mut iter = s.char_indices();
    std::iter::from_fn(move || {
        let (start, c) = iter.next()?;
        if c != '\u{1b}' {
            return Some(Segment::Visible(c));
        }
        // A CSI sequence like `\x1b[36m` is terminated by a letter.
        let mut end = start + c.len_utf8();
        for (i, c) in iter.by_ref() {
            end = i + c.len_utf8();
            if c.is_ascii_alphabetic() {
                break;
            }
        }
        Some(Segment::Escape(&s[start..end]))
    })
}

/// Truncates to at most `max` visible columns, keeping ANSI escape sequences
/// intact (they take no columns and must not be cut in half).
fn truncate_visible(s: &str, max: usize) -> String {
    let mut out = String::new();
    let mut visible = 0usize;
    for segment in segments(s) {
        match segment {
            Segment::Escape(escape) => out.push_str(escape),
            Segment::Visible(_) if visible == max => break,
            Segment::Visible(c) => {
                visible += 1;
                out.push(c);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_log_line_has_an_uncolored_tag_and_a_bare_line_feed() {
        let lines = format_block(
            Some(Category::Receive),
            "phone: Received 1 file",
            Style::Plain,
        );

        assert_eq!(lines, ["R phone: Received 1 file\n"]);
    }

    #[test]
    fn plain_log_block_indents_the_lines_after_the_first() {
        let lines = format_block(Some(Category::Receive), "phone\nFiles (1):", Style::Plain);

        assert_eq!(lines, ["R phone\n", "  Files (1):\n"]);
    }

    #[test]
    fn plain_log_line_without_a_category_has_no_tag() {
        let lines = format_block(None, "Ready to accept requests.", Style::Plain);

        assert_eq!(lines, ["Ready to accept requests.\n"]);
    }

    #[test]
    fn terminal_log_line_keeps_the_raw_mode_line_break() {
        let lines = format_block(Some(Category::Discovery), "phone", Style::Terminal);

        assert_eq!(lines.len(), 1);
        assert!(lines[0].ends_with("phone\r\n"));
    }

    #[test]
    fn plain_ui_has_no_status_line() {
        let mut ui = Ui::plain();

        ui.set_status(Some("50%".to_string()));

        assert!(ui.status.is_none());
    }
}
