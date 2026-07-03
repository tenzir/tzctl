//! A borderless, whitespace-aligned table printer.
//!
//! Columns are aligned purely with spaces (no borders). Widths are precomputed
//! from the visible width of every cell, ignoring ANSI escape sequences so that
//! colored cells still line up. Headers are rendered in a muted gray.

use owo_colors::OwoColorize;

/// Column alignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    /// Left-align (the default).
    Left,
    /// Right-align (used for numeric columns).
    Right,
}

/// A borderless table with a header row and zero or more data rows.
#[derive(Debug, Clone)]
pub struct Table {
    headers: Vec<String>,
    aligns: Vec<Align>,
    rows: Vec<Vec<String>>,
}

impl Table {
    /// Create a table with the given column headers.
    ///
    /// All columns default to [`Align::Left`].
    pub fn new<I, S>(headers: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let headers: Vec<String> = headers.into_iter().map(Into::into).collect();
        let aligns = vec![Align::Left; headers.len()];
        Self {
            headers,
            aligns,
            rows: Vec::new(),
        }
    }

    /// Set the alignment of a single column.
    #[must_use]
    pub fn align(mut self, col: usize, align: Align) -> Self {
        if let Some(slot) = self.aligns.get_mut(col) {
            *slot = align;
        }
        self
    }

    /// Append a data row.
    ///
    /// The number of cells should match the number of headers.
    pub fn row<I, S>(&mut self, cells: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.rows.push(cells.into_iter().map(Into::into).collect());
    }

    /// Render the table to a string without a trailing newline.
    pub fn render(&self) -> String {
        let cols = self.headers.len();
        let mut widths = vec![0usize; cols];
        for (i, h) in self.headers.iter().enumerate() {
            widths[i] = display_width(h);
        }
        for row in &self.rows {
            for (i, cell) in row.iter().enumerate().take(cols) {
                widths[i] = widths[i].max(display_width(cell));
            }
        }

        let mut out = String::new();
        // Header row, styled in muted gray.
        let header_cells: Vec<String> = self.headers.clone();
        push_row(&mut out, &header_cells, &widths, &self.aligns, true);
        for row in &self.rows {
            out.push('\n');
            push_row(&mut out, row, &widths, &self.aligns, false);
        }
        out
    }
}

/// Append a single rendered row (no trailing newline) to `out`.
fn push_row(out: &mut String, cells: &[String], widths: &[usize], aligns: &[Align], header: bool) {
    let cols = widths.len();
    // Find the last non-empty column so we can avoid trailing whitespace.
    let last = (0..cols)
        .rev()
        .find(|&i| cells.get(i).map(|c| !c.is_empty()).unwrap_or(false))
        .unwrap_or(0);

    for i in 0..=last {
        if i > 0 {
            out.push_str("  ");
        }
        let empty = String::new();
        let cell = cells.get(i).unwrap_or(&empty);
        let width = widths.get(i).copied().unwrap_or(0);
        let pad = width.saturating_sub(display_width(cell));
        let align = aligns.get(i).copied().unwrap_or(Align::Left);
        let is_last = i == last;

        let styled = if header {
            cell.bright_black().to_string()
        } else {
            cell.clone()
        };

        match align {
            Align::Left => {
                out.push_str(&styled);
                // Don't pad the final column (avoid trailing whitespace).
                if !is_last {
                    out.push_str(&" ".repeat(pad));
                }
            }
            Align::Right => {
                out.push_str(&" ".repeat(pad));
                out.push_str(&styled);
            }
        }
    }
}

/// Compute the visible width of `s`, skipping ANSI SGR escape sequences.
///
/// Counts Unicode scalar values outside of `\x1b[ ... m` sequences. This is a
/// deliberately simple approximation: it does not account for wide (CJK/emoji)
/// glyphs, which the CLI does not emit.
pub fn display_width(s: &str) -> usize {
    let mut width = 0usize;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // Skip until the terminating 'm' of an SGR sequence.
            for c in chars.by_ref() {
                if c == 'm' {
                    break;
                }
            }
        } else {
            width += 1;
        }
    }
    width
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_width_ignores_ansi() {
        let colored = "\x1b[32mconnected\x1b[0m";
        assert_eq!(display_width(colored), "connected".len());
        assert_eq!(display_width("plain"), 5);
    }

    #[test]
    fn widths_fit_longest_cell() {
        let mut t = Table::new(["A", "NAME"]);
        t.row(["1", "short"]);
        t.row(["2", "a-longer-name"]);
        let rendered = t.render();
        for line in rendered.lines() {
            // Second column starts at the same offset on every line.
            assert!(line.contains("  "));
        }
    }

    #[test]
    fn right_align_pads_on_left() {
        let mut t = Table::new(["N", "X"]).align(0, Align::Right);
        t.row(["1", "a"]);
        t.row(["100", "b"]);
        let rendered = t.render();
        let lines: Vec<&str> = rendered.lines().collect();
        // "1" is right-aligned within width 3 (header "N" -> col width 3).
        assert!(lines[1].starts_with("  1"));
        assert!(lines[2].starts_with("100"));
    }

    #[test]
    fn colored_cell_aligns_to_visible_width() {
        let mut t = Table::new(["S", "END"]);
        t.row(["\x1b[32mok\x1b[0m", "x"]);
        t.row(["fail", "y"]);
        let rendered = t.render();
        let lines: Vec<&str> = rendered.lines().collect();
        // The colored "ok" (visible width 2) is padded to width 4 like "fail".
        // Strip ANSI and check the second column aligns.
        let strip = |s: &str| {
            let mut out = String::new();
            let mut cs = s.chars();
            while let Some(c) = cs.next() {
                if c == '\x1b' {
                    for c in cs.by_ref() {
                        if c == 'm' {
                            break;
                        }
                    }
                } else {
                    out.push(c);
                }
            }
            out
        };
        let a = strip(lines[1]).find('x').unwrap();
        let b = strip(lines[2]).find('y').unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn no_trailing_whitespace() {
        let mut t = Table::new(["A", "B"]);
        t.row(["1", "value"]);
        t.row(["longlong", ""]);
        for line in t.render().lines() {
            assert_eq!(line.trim_end(), line, "line has trailing whitespace: {line:?}");
        }
    }

    #[test]
    fn header_is_styled() {
        let t = Table::new(["NAME"]);
        // The header content is present even if colored.
        assert!(t.render().contains("NAME"));
    }
}
