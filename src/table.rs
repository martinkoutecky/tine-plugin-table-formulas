//! Finding pipe tables and their formula lines in a block, and writing a table
//! back aligned the way `org-table-align` aligns it.
//!
//! Written from Org's documented table behaviour (and from reading
//! `org-table-align`/`org-table-to-lisp` to pin the details); no Org code is
//! copied.

use regex::Regex;
use std::sync::OnceLock;
use unicode_width::UnicodeWidthStr;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dialect {
    Markdown,
    Org,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Line {
    Hline,
    Data(Vec<String>),
}

/// Markdown column alignment from the delimiter row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MdAlign {
    None,
    Left,
    Right,
    Center,
}

/// One table of a block, with the formula lines that follow it.
#[derive(Clone, Debug)]
pub struct Found {
    /// Index of the first table line in the block's lines.
    pub start: usize,
    /// One past the last table line.
    pub end: usize,
    /// The formula entries (one string, `::`-separated as in `#+TBLFM:`),
    /// or `None` when the table has no formula line.
    pub tblfm: Option<String>,
    /// A `#+TBLFM:` line under a Markdown table (not read; the comment form is).
    pub md_has_org_tblfm: bool,
}

/// A table parsed into lines.
#[derive(Clone, Debug)]
pub struct Table {
    pub lines: Vec<Line>,
    /// Leading whitespace of the first table line (Org reuses it for all).
    pub indent: String,
    /// Markdown: line 2 is the GFM delimiter row, with these alignments.
    pub md_delimiter: Option<Vec<MdAlign>>,
    pub dialect: Dialect,
}

fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pat).expect("static regex"))
}

fn tblfm_org_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"^[ \t]*#\+[Tt][Bb][Ll][Ff][Mm]: *(.*)$")
}

fn tblfm_md_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"^[ \t]*<!--[ \t]*TBLFM:[ \t]*(.*?)[ \t]*-->[ \t]*$")
}

fn gfm_delimiter_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"^[ \t]*\|?[ \t]*:?-+:?[ \t]*(\|[ \t]*:?-+:?[ \t]*)*\|?[ \t]*$")
}

pub fn is_table_line(line: &str) -> bool {
    line.trim_start_matches([' ', '\t']).starts_with('|')
}

fn is_blank(line: &str) -> bool {
    line.trim_matches([' ', '\t']).is_empty()
}

fn fence_marker(line: &str, dialect: Dialect) -> Option<String> {
    let t = line.trim_start_matches([' ', '\t']);
    match dialect {
        Dialect::Markdown => {
            for f in ["```", "~~~"] {
                if t.starts_with(f) {
                    let n = t.chars().take_while(|c| *c == f.chars().next().unwrap_or('`')).count();
                    return Some(f.chars().next().unwrap_or('`').to_string().repeat(n));
                }
            }
            None
        }
        Dialect::Org => {
            let lower = t.to_ascii_lowercase();
            if lower.starts_with("#+begin_") {
                Some("#+end_".to_string())
            } else {
                None
            }
        }
    }
}

/// Find the tables of a block. Tables inside code fences (Markdown) or
/// `#+BEGIN_...` blocks (Org) are not tables.
pub fn find_tables(lines: &[&str], dialect: Dialect) -> Vec<Found> {
    let mut out = Vec::new();
    let mut i = 0;
    let mut fence: Option<String> = None;
    while i < lines.len() {
        let line = lines[i];
        if let Some(f) = &fence {
            let t = line.trim_start_matches([' ', '\t']);
            let closes = match dialect {
                Dialect::Markdown => t.starts_with(f.as_str()),
                Dialect::Org => t.to_ascii_lowercase().starts_with("#+end_"),
            };
            if closes {
                fence = None;
            }
            i += 1;
            continue;
        }
        if let Some(f) = fence_marker(line, dialect) {
            fence = Some(f);
            i += 1;
            continue;
        }
        if !is_table_line(line) {
            i += 1;
            continue;
        }
        let start = i;
        while i < lines.len() && is_table_line(lines[i]) {
            i += 1;
        }
        let end = i;
        let mut j = end;
        while j < lines.len() && is_blank(lines[j]) {
            j += 1;
        }
        let mut tblfm = None;
        let mut md_has_org_tblfm = false;
        match dialect {
            Dialect::Org => {
                if let Some(c) = lines.get(j).and_then(|l| tblfm_org_re().captures(l)) {
                    tblfm = Some(c.get(1).map_or("", |m| m.as_str()).to_string());
                }
            }
            Dialect::Markdown => {
                let mut parts = Vec::new();
                let mut k = j;
                while let Some(c) = lines.get(k).and_then(|l| tblfm_md_re().captures(l)) {
                    parts.push(c.get(1).map_or("", |m| m.as_str()).to_string());
                    k += 1;
                }
                if !parts.is_empty() {
                    tblfm = Some(parts.join("::"));
                } else if lines.get(j).is_some_and(|l| tblfm_org_re().is_match(l)) {
                    md_has_org_tblfm = true;
                }
            }
        }
        out.push(Found { start, end, tblfm, md_has_org_tblfm });
    }
    out
}

/// Split a table row into trimmed fields, the way `org-table-to-lisp` does:
/// fields end at `|` (in Markdown, not at an escaped `\|`); a trailing
/// non-empty text without a closing `|` is a field too.
pub fn split_row(line: &str, dialect: Dialect) -> Vec<String> {
    let t = line.trim_start_matches([' ', '\t']);
    let Some(rest) = t.strip_prefix('|') else { return Vec::new() };
    let chars: Vec<char> = rest.chars().collect();
    let mut fields = Vec::new();
    let mut i = 0;
    loop {
        while i < chars.len() && (chars[i] == ' ' || chars[i] == '\t') {
            i += 1;
        }
        if i >= chars.len() {
            break;
        }
        let q = i;
        while i < chars.len() && chars[i] != '|' {
            if dialect == Dialect::Markdown && chars[i] == '\\' && chars.get(i + 1) == Some(&'|') {
                i += 2;
                continue;
            }
            i += 1;
        }
        let mut e = i;
        while e > q && (chars[e - 1] == ' ' || chars[e - 1] == '\t') {
            e -= 1;
        }
        fields.push(chars[q..e].iter().collect());
        if i < chars.len() {
            i += 1;
        }
    }
    fields
}

fn md_align_of(cell: &str) -> MdAlign {
    let l = cell.starts_with(':');
    let r = cell.len() > 1 && cell.ends_with(':');
    match (l, r) {
        (true, true) => MdAlign::Center,
        (true, false) => MdAlign::Left,
        (false, true) => MdAlign::Right,
        _ => MdAlign::None,
    }
}

pub fn parse_table(lines: &[&str], dialect: Dialect) -> Table {
    let indent: String = lines
        .first()
        .map(|l| l.chars().take_while(|c| *c == ' ' || *c == '\t').collect())
        .unwrap_or_default();
    let mut out = Vec::new();
    let mut md_delimiter = None;
    for (n, l) in lines.iter().enumerate() {
        let t = l.trim_start_matches([' ', '\t']);
        if t.starts_with("|-") {
            out.push(Line::Hline);
            if dialect == Dialect::Markdown && n == 1 && gfm_delimiter_re().is_match(l) {
                md_delimiter = Some(split_row(l, dialect).iter().map(|c| md_align_of(c)).collect());
            }
            continue;
        }
        if dialect == Dialect::Markdown && n == 1 && gfm_delimiter_re().is_match(l) {
            out.push(Line::Hline);
            md_delimiter = Some(split_row(l, dialect).iter().map(|c| md_align_of(c)).collect());
            continue;
        }
        out.push(Line::Data(split_row(l, dialect)));
    }
    Table { lines: out, indent, md_delimiter, dialect }
}

fn org_link_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"\[\[([^\]\[]*)\](?:\[([^\]\[]*)\])?\]")
}

/// The width Org gives a cell: links count by the text they display.
fn cell_width(cell: &str, dialect: Dialect) -> usize {
    match dialect {
        Dialect::Markdown => UnicodeWidthStr::width(cell),
        Dialect::Org => {
            let shown = org_link_re().replace_all(cell, |c: &regex::Captures| {
                c.get(2).or(c.get(1)).map_or(String::new(), |m| m.as_str().to_string())
            });
            UnicodeWidthStr::width(shown.as_ref())
        }
    }
}

fn number_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    // `org-table-number-regexp`, matched case-insensitively as in a buffer
    // (spelled out, so the regex engine needs no Unicode case tables).
    re(
        &R,
        r"^([<>]?[-+^.0-9]*[0-9][-+^.0-9eEdDxX()%:]*|[<>]?[-+]?0[xX][0-9a-fA-F.]+|[<>]?[-+]?[0-9]+#[0-9a-zA-Z.]+|[nN][aA][nN]|[-+uU]?[iI][nN][fF])$",
    )
}

fn cookie_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"^<([lrc])[0-9]*>$")
}

impl Table {
    pub fn ncols(&self) -> usize {
        self.lines
            .iter()
            .map(|l| match l {
                Line::Data(f) => f.len(),
                Line::Hline => 0,
            })
            .max()
            .unwrap_or(0)
    }

    /// Render the table aligned: every data row padded to the widest row,
    /// each column as wide as its widest cell (at least 1), numbers to the
    /// right when at least half of a column's non-empty cells are numbers.
    pub fn render(&self) -> Vec<String> {
        let rows: Vec<&Vec<String>> = self
            .lines
            .iter()
            .filter_map(|l| match l {
                Line::Data(f) => Some(f),
                Line::Hline => None,
            })
            .collect();
        let ncols = self.ncols().max(if rows.is_empty() { 1 } else { 0 });
        let mut widths = vec![1usize; ncols];
        let mut aligns = vec!['l'; ncols];
        for c in 0..ncols {
            let mut fixed: Option<char> = None;
            let (mut numbers, mut non_empty) = (0usize, 0usize);
            for r in &rows {
                let cell = r.get(c).map(String::as_str).unwrap_or("");
                widths[c] = widths[c].max(cell_width(cell, self.dialect));
                if fixed.is_some() || cell.is_empty() {
                    continue;
                }
                if let Some(m) = cookie_re().captures(cell) {
                    fixed = m.get(1).and_then(|x| x.as_str().chars().next());
                    continue;
                }
                non_empty += 1;
                if number_re().is_match(cell) {
                    numbers += 1;
                }
            }
            aligns[c] = fixed.unwrap_or(if 2 * numbers >= non_empty { 'r' } else { 'l' });
        }
        if let Some(md) = &self.md_delimiter {
            for c in 0..ncols {
                match md.get(c).copied().unwrap_or(MdAlign::None) {
                    MdAlign::Left => aligns[c] = 'l',
                    MdAlign::Right => aligns[c] = 'r',
                    MdAlign::Center => aligns[c] = 'c',
                    MdAlign::None => {}
                }
                let colons = match md.get(c).copied().unwrap_or(MdAlign::None) {
                    MdAlign::Center => 2,
                    MdAlign::Left | MdAlign::Right => 1,
                    MdAlign::None => 0,
                };
                widths[c] = widths[c].max(colons + 1);
            }
        }
        let rule: String =
            widths.iter().map(|w| "-".repeat(w + 2)).collect::<Vec<_>>().join("+");
        let mut out = Vec::new();
        for (n, l) in self.lines.iter().enumerate() {
            match l {
                Line::Hline => {
                    if n == 1 && self.md_delimiter.is_some() {
                        let md = self.md_delimiter.as_deref().unwrap_or(&[]);
                        let cells: Vec<String> = (0..ncols)
                            .map(|c| {
                                let w = widths[c];
                                let a = md.get(c).copied().unwrap_or(MdAlign::None);
                                let (l, r) = match a {
                                    MdAlign::Center => (":", ":"),
                                    MdAlign::Left => (":", ""),
                                    MdAlign::Right => ("", ":"),
                                    MdAlign::None => ("", ""),
                                };
                                format!(" {l}{}{r} ", "-".repeat(w - l.len() - r.len()))
                            })
                            .collect();
                        out.push(format!("{}|{}|", self.indent, cells.join("|")));
                    } else {
                        out.push(format!("{}|{}|", self.indent, rule));
                    }
                }
                Line::Data(fields) => {
                    let cells: Vec<String> = (0..ncols)
                        .map(|c| {
                            let f = fields.get(c).map(String::as_str).unwrap_or("");
                            let spaces = widths[c].saturating_sub(cell_width(f, self.dialect));
                            let pre = match aligns[c] {
                                'r' => spaces,
                                'c' => spaces / 2,
                                _ => 0,
                            };
                            format!(" {}{}{} ", " ".repeat(pre), f, " ".repeat(spaces - pre))
                        })
                        .collect();
                    out.push(format!("{}|{}|", self.indent, cells.join("|")));
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_like_org() {
        assert_eq!(split_row("| a | b |", Dialect::Org), vec!["a", "b"]);
        assert_eq!(split_row("| a | b", Dialect::Org), vec!["a", "b"]);
        assert_eq!(split_row("| a |  |", Dialect::Org), vec!["a", ""]);
        assert_eq!(split_row("|", Dialect::Org), Vec::<String>::new());
        assert_eq!(split_row("| a \\| b | c |", Dialect::Markdown), vec!["a \\| b", "c"]);
        assert_eq!(split_row("| a \\| b | c |", Dialect::Org), vec!["a \\", "b", "c"]);
    }

    #[test]
    fn aligns_like_org() {
        let src = ["| a | 1 |", "|-", "| bcd | 22 |", "| | x |"];
        let t = parse_table(&src, Dialect::Org);
        assert_eq!(
            t.render(),
            vec!["| a   |  1 |", "|-----+----|", "| bcd | 22 |", "|     |  x |"]
        );
    }

    #[test]
    fn markdown_delimiter_row_stays_gfm() {
        let src = ["| a | b |", "|:-|--:|", "| 1 | 2 |"];
        let t = parse_table(&src, Dialect::Markdown);
        assert_eq!(t.render(), vec!["| a  |  b |", "| :- | -: |", "| 1  |  2 |"]);
        let src2 = ["| x | y |", "| --- | --- |", "| 10 | abc |"];
        let t2 = parse_table(&src2, Dialect::Markdown);
        assert_eq!(t2.render(), vec!["|  x | y   |", "| -- | --- |", "| 10 | abc |"]);
    }

    #[test]
    fn finds_tables_and_formula_lines() {
        let md = ["text", "| a |", "| 1 |", "<!-- TBLFM: $1=2 -->", "<!-- TBLFM: @1$1=3 -->", "```", "| x |", "```"];
        let f = find_tables(&md, Dialect::Markdown);
        assert_eq!(f.len(), 1);
        assert_eq!((f[0].start, f[0].end), (1, 3));
        assert_eq!(f[0].tblfm.as_deref(), Some("$1=2::@1$1=3"));
        let org = ["| a |", "", "#+tblfm: $1=1"];
        let f = find_tables(&org, Dialect::Org);
        assert_eq!(f[0].tblfm.as_deref(), Some("$1=1"));
    }
}
