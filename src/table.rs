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
    /// Why the plugin will not touch this table (a Markdown layout whose
    /// extent it cannot determine); its formulas are then reported, not run.
    pub decline: Option<String>,
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
    re(
        &R,
        r"^[ \t]*\|?[ \t]*:?-+:?[ \t]*(\|[ \t]*:?-+:?[ \t]*)*\|?[ \t]*$",
    )
}

pub fn is_table_line(line: &str) -> bool {
    line.trim_start_matches([' ', '\t']).starts_with('|')
}

fn is_blank(line: &str) -> bool {
    line.trim_matches([' ', '\t']).is_empty()
}

/// Indentation in columns (a tab advances to the next multiple of 4), and
/// the rest of the line.
fn indent_of(line: &str) -> (usize, &str) {
    let mut cols = 0;
    for (i, c) in line.char_indices() {
        match c {
            ' ' => cols += 1,
            '\t' => cols += 4 - cols % 4,
            _ => return (cols, &line[i..]),
        }
    }
    (cols, "")
}

/// A CommonMark opening code fence: at most 3 spaces of indentation, then at
/// least three backticks or tildes (a backtick fence's info string has no
/// backtick). Returns the fence character and its length.
fn md_opening_fence(line: &str) -> Option<(char, usize)> {
    let (ind, rest) = indent_of(line);
    if ind > 3 {
        return None;
    }
    let ch = rest.chars().next().filter(|c| *c == '`' || *c == '~')?;
    let n = rest.chars().take_while(|c| *c == ch).count();
    if n < 3 {
        return None;
    }
    if ch == '`' && rest[n..].contains('`') {
        return None;
    }
    Some((ch, n))
}

/// A CommonMark closing fence for an opener `(ch, n)`: at most 3 spaces of
/// indentation, at least `n` fence characters, then only spaces or tabs.
fn md_closing_fence(line: &str, ch: char, n: usize) -> bool {
    let (ind, rest) = indent_of(line);
    if ind > 3 {
        return false;
    }
    let k = rest.chars().take_while(|c| *c == ch).count();
    k >= n && is_blank(&rest[k * ch.len_utf8()..])
}

/// CommonMark HTML blocks of kinds 1 to 5, which run until an end marker
/// (possibly on the same line), across blank lines. `Some(None)` is a block
/// that ends on its first line; `Some(Some(marker))` one that continues.
fn md_html_block_start(line: &str) -> Option<Option<&'static str>> {
    let (ind, rest) = indent_of(line);
    if ind > 3 || !rest.starts_with('<') {
        return None;
    }
    let lower = rest.to_ascii_lowercase();
    let mut end: Option<&'static str> = None;
    for (tag, close) in [
        ("script", "</script>"),
        ("pre", "</pre>"),
        ("style", "</style>"),
        ("textarea", "</textarea>"),
    ] {
        if let Some(after) = lower.strip_prefix('<').and_then(|r| r.strip_prefix(tag)) {
            if after.is_empty() || after.starts_with([' ', '\t', '>']) {
                end = Some(close);
            }
        }
    }
    let (end, from) = match end {
        Some(e) => (e, 1),
        None if rest.starts_with("<!--") => ("-->", 4),
        None if rest.starts_with("<?") => ("?>", 2),
        None if rest.starts_with("<![CDATA[") => ("]]>", 9),
        None if rest.starts_with("<!")
            && rest[2..].starts_with(|c: char| c.is_ascii_alphabetic()) =>
        {
            (">", 2)
        }
        None => return None,
    };
    Some(if lower[from..].contains(end) {
        None
    } else {
        Some(end)
    })
}

/// A Markdown block-quote line: the text after its `>` markers.
fn md_unquote(line: &str) -> Option<&str> {
    let (ind, rest) = indent_of(line);
    if ind > 3 || !rest.starts_with('>') {
        return None;
    }
    let mut r = rest;
    while let Some(x) = r.strip_prefix('>') {
        r = x.trim_start_matches([' ', '\t']);
    }
    Some(r)
}

/// Whether the line has a `|` that is not escaped as `\|`.
fn has_unescaped_pipe(line: &str) -> bool {
    let b = line.as_bytes();
    (0..b.len()).any(|i| b[i] == b'|' && (i == 0 || b[i - 1] != b'\\'))
}

fn org_block_start(line: &str) -> bool {
    line.trim_start_matches([' ', '\t'])
        .to_ascii_lowercase()
        .starts_with("#+begin_")
}

fn org_block_end(line: &str) -> bool {
    line.trim_start_matches([' ', '\t'])
        .to_ascii_lowercase()
        .starts_with("#+end_")
}

/// The formula lines under a table that ends before line `end`. Org: the
/// `#+TBLFM:` line directly under the table (Org 9.8.7 does not read one
/// after a blank line). Markdown: `<!-- TBLFM: ... -->` lines, after any
/// blank lines (`unquote` strips a block-quote prefix first).
fn formula_lines(
    lines: &[&str],
    end: usize,
    dialect: Dialect,
    unquote: bool,
) -> (Option<String>, bool) {
    let get = |k: usize| -> Option<&str> {
        let l = *lines.get(k)?;
        if unquote {
            md_unquote(l)
        } else {
            Some(l)
        }
    };
    match dialect {
        Dialect::Org => {
            let tblfm = get(end)
                .and_then(|l| tblfm_org_re().captures(l))
                .map(|c| c.get(1).map_or("", |m| m.as_str()).to_string());
            (tblfm, false)
        }
        Dialect::Markdown => {
            let mut j = end;
            while get(j).is_some_and(is_blank) {
                j += 1;
            }
            let mut parts = Vec::new();
            let mut k = j;
            while let Some(c) = get(k).and_then(|l| tblfm_md_re().captures(l)) {
                parts.push(c.get(1).map_or("", |m| m.as_str()).to_string());
                k += 1;
            }
            if !parts.is_empty() {
                (Some(parts.join("::")), false)
            } else {
                (None, get(j).is_some_and(|l| tblfm_org_re().is_match(l)))
            }
        }
    }
}

/// Why a Markdown pipe block cannot be recalculated safely, if it cannot:
/// it is not a GFM table, or its extent depends on Markdown structure the
/// plugin does not parse (review finding F7).
fn md_table_problem(lines: &[&str], start: usize, end: usize) -> Option<String> {
    let run = &lines[start..end];
    if run.iter().any(|l| indent_of(l).0 > 3) {
        return Some(
            "a table line is indented by four or more spaces (Markdown reads it as code)".into(),
        );
    }
    let header = split_row(run[0], Dialect::Markdown);
    let valid_delim = run.len() >= 2
        && gfm_delimiter_re().is_match(run[1])
        && split_row(run[1], Dialect::Markdown).len() == header.len();
    if !valid_delim {
        return Some(
            "not a Markdown table: it needs a header row, then a delimiter row such as `|---|---|` with as many cells"
                .into(),
        );
    }
    if run[2..]
        .iter()
        .any(|l| split_row(l, Dialect::Markdown).len() > header.len())
    {
        return Some(
            "a row has more cells than the header (Markdown ignores the extra cells)".into(),
        );
    }
    let one_line_comment = |l: &str| {
        let (ind, rest) = indent_of(l);
        ind <= 3 && rest.starts_with("<!--") && rest[4..].contains("-->")
    };
    if start > 0 && has_unescaped_pipe(lines[start - 1]) && !one_line_comment(lines[start - 1]) {
        return Some("the line above the table has a `|` but does not start with one; where the table begins is unclear".into());
    }
    // Inside an HTML block (it runs to the next blank line), a pipe table is
    // literal text. A one-line `<!-- ... -->` comment ends its own block.
    let mut k = start;
    while k > 0 && !is_blank(lines[k - 1]) {
        k -= 1;
        if one_line_comment(lines[k]) {
            break;
        }
        let (ind, rest) = indent_of(lines[k]);
        if ind <= 3 && rest.starts_with('<') {
            return Some(
                "the table follows an HTML line without a blank line; Markdown may read it as HTML"
                    .into(),
            );
        }
    }
    None
}

/// Find the tables of a block. Tables inside code fences or HTML blocks
/// (Markdown) or `#+BEGIN_...` blocks (Org) are not tables. A Markdown pipe
/// block whose extent cannot be determined is returned with
/// [`Found::decline`] set.
pub fn find_tables(lines: &[&str], dialect: Dialect) -> Vec<Found> {
    let mut out = Vec::new();
    let mut i = 0;
    // Markdown: an open code fence or HTML block (with its end marker);
    // Org: an open `#+BEGIN_` block.
    let mut fence: Option<(char, usize)> = None;
    let mut html_end: Option<&'static str> = None;
    let mut org_block = false;
    while i < lines.len() {
        let line = lines[i];
        match dialect {
            Dialect::Org => {
                if org_block {
                    org_block = !org_block_end(line);
                    i += 1;
                    continue;
                }
                if org_block_start(line) {
                    org_block = true;
                    i += 1;
                    continue;
                }
            }
            Dialect::Markdown => {
                if let Some((ch, n)) = fence {
                    if md_closing_fence(line, ch, n) {
                        fence = None;
                    }
                    i += 1;
                    continue;
                }
                if let Some(end) = html_end {
                    if line.to_ascii_lowercase().contains(end) {
                        html_end = None;
                    }
                    i += 1;
                    continue;
                }
                if let Some(f) = md_opening_fence(line) {
                    fence = Some(f);
                    i += 1;
                    continue;
                }
                if let Some(end) = md_html_block_start(line) {
                    html_end = end;
                    i += 1;
                    continue;
                }
                if md_unquote(line).is_some_and(|r| r.starts_with('|')) {
                    let start = i;
                    while i < lines.len()
                        && md_unquote(lines[i]).is_some_and(|r| r.starts_with('|'))
                    {
                        i += 1;
                    }
                    let (tblfm, md_has_org_tblfm) = formula_lines(lines, i, dialect, true);
                    out.push(Found {
                        start,
                        end: i,
                        tblfm,
                        md_has_org_tblfm,
                        decline: Some("tables inside a block quote are not supported".into()),
                    });
                    continue;
                }
            }
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
        let (tblfm, md_has_org_tblfm) = formula_lines(lines, end, dialect, false);
        let decline = match dialect {
            Dialect::Markdown => md_table_problem(lines, start, end),
            Dialect::Org => None,
        };
        out.push(Found {
            start,
            end,
            tblfm,
            md_has_org_tblfm,
            decline,
        });
    }
    if let Some(why) = block_uncertainty(lines, dialect) {
        for f in out.iter_mut().filter(|f| f.decline.is_none()) {
            f.decline = Some(why.clone());
        }
    }
    out
}

/// A line with its leading whitespace, list markers and block-quote markers
/// removed: what a fence or an HTML tag could start with in a list item or
/// its continuation.
fn strip_container_markers(line: &str) -> &str {
    let mut r = line.trim_start();
    loop {
        let before = r;
        if let Some(x) = r.strip_prefix(['-', '*', '+', '>']) {
            if x.is_empty() || x.starts_with(char::is_whitespace) || r.starts_with('>') {
                r = x.trim_start();
            }
        } else {
            let digits = r.bytes().take_while(u8::is_ascii_digit).count();
            if (1..=9).contains(&digits) {
                if let Some(x) = r[digits..].strip_prefix(['.', ')']) {
                    if x.is_empty() || x.starts_with(char::is_whitespace) {
                        r = x.trim_start();
                    }
                }
            }
        }
        if r.len() == before.len() {
            return r;
        }
    }
}

/// Why the plugin cannot be sure that any table of this block is a table in
/// Tine's own parse (review round 2, B1/B2). The plugin cannot see that
/// parse, so it declines the whole block when:
/// - any line looks like a code fence (` ``` ` or `~~~`), at any indentation,
///   tabs and list items included: Tine's parser may read the pipe lines as
///   source code even where CommonMark would not;
/// - (Markdown) any line other than a formula comment starts like an HTML
///   block or tag (`<x`, `</x`, `<!`, `<?`): an enclosing HTML block can make
///   the table literal text, and an inner comment does not end it.
fn block_uncertainty(lines: &[&str], dialect: Dialect) -> Option<String> {
    for (n, line) in lines.iter().enumerate() {
        let r = strip_container_markers(line);
        if r.starts_with("```") || r.starts_with("~~~") {
            return Some(format!(
                "line {} of the block looks like a code fence; the plugin cannot tell whether Tine shows the table as a table or as code",
                n + 1
            ));
        }
        if dialect == Dialect::Markdown && !tblfm_md_re().is_match(line) {
            let mut c = r.chars();
            if c.next() == Some('<')
                && c.next()
                    .is_some_and(|c| c.is_ascii_alphabetic() || matches!(c, '/' | '!' | '?'))
            {
                return Some(format!(
                    "line {} of the block is HTML; the plugin cannot tell whether Tine shows the table as a table or as HTML",
                    n + 1
                ));
            }
        }
    }
    None
}

/// Split a table row into trimmed fields, the way `org-table-to-lisp` does:
/// fields end at `|` (in Markdown, not at an escaped `\|`); a trailing
/// non-empty text without a closing `|` is a field too.
pub fn split_row(line: &str, dialect: Dialect) -> Vec<String> {
    let t = line.trim_start_matches([' ', '\t']);
    let Some(rest) = t.strip_prefix('|') else {
        return Vec::new();
    };
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
        match dialect {
            // Org: every line starting with `|-` is a rule.
            Dialect::Org if t.starts_with("|-") => {
                out.push(Line::Hline);
                continue;
            }
            // Markdown: only the delimiter row (line 2, checked by
            // `find_tables`) is a rule; every later row is data, even
            // `|-5|` or `|---|` (review finding F2).
            Dialect::Markdown if n == 1 && gfm_delimiter_re().is_match(l) => {
                out.push(Line::Hline);
                md_delimiter = Some(
                    split_row(l, dialect)
                        .iter()
                        .map(|c| md_align_of(c))
                        .collect(),
                );
                continue;
            }
            _ => {}
        }
        out.push(Line::Data(split_row(l, dialect)));
    }
    Table {
        lines: out,
        indent,
        md_delimiter,
        dialect,
    }
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
                c.get(2)
                    .or(c.get(1))
                    .map_or(String::new(), |m| m.as_str().to_string())
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

    /// [`Table::render`], unless the aligned table would clearly be larger
    /// than `limit` (UTF-16 units; the estimate counts display columns, so
    /// the caller still checks the exact size). Padding a compact ragged
    /// table into a rectangle can multiply its size; this refuses before
    /// allocating it.
    pub fn render_checked(&self, limit: usize) -> Option<Vec<String>> {
        let ncols = self.ncols().max(1);
        let mut widths = vec![1usize; ncols];
        for l in &self.lines {
            if let Line::Data(f) = l {
                for (c, cell) in f.iter().enumerate() {
                    widths[c] = widths[c].max(cell_width(cell, self.dialect));
                }
            }
        }
        let per_line = widths
            .iter()
            .fold(self.indent.len() + 1, |a, w| a.saturating_add(w + 3));
        if per_line.saturating_mul(self.lines.len()) > limit.saturating_mul(2) {
            return None;
        }
        Some(self.render())
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
        let rule: String = widths
            .iter()
            .map(|w| "-".repeat(w + 2))
            .collect::<Vec<_>>()
            .join("+");
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
        assert_eq!(
            split_row("| a \\| b | c |", Dialect::Markdown),
            vec!["a \\| b", "c"]
        );
        assert_eq!(
            split_row("| a \\| b | c |", Dialect::Org),
            vec!["a \\", "b", "c"]
        );
    }

    #[test]
    fn aligns_like_org() {
        let src = ["| a | 1 |", "|-", "| bcd | 22 |", "| | x |"];
        let t = parse_table(&src, Dialect::Org);
        assert_eq!(
            t.render(),
            vec![
                "| a   |  1 |",
                "|-----+----|",
                "| bcd | 22 |",
                "|     |  x |"
            ]
        );
    }

    #[test]
    fn markdown_delimiter_row_stays_gfm() {
        let src = ["| a | b |", "|:-|--:|", "| 1 | 2 |"];
        let t = parse_table(&src, Dialect::Markdown);
        assert_eq!(
            t.render(),
            vec!["| a  |  b |", "| :- | -: |", "| 1  |  2 |"]
        );
        let src2 = ["| x | y |", "| --- | --- |", "| 10 | abc |"];
        let t2 = parse_table(&src2, Dialect::Markdown);
        assert_eq!(
            t2.render(),
            vec!["|  x | y   |", "| -- | --- |", "| 10 | abc |"]
        );
    }

    #[test]
    fn finds_tables_and_formula_lines() {
        let md = [
            "text",
            "| a |",
            "| 1 |",
            "<!-- TBLFM: $1=2 -->",
            "<!-- TBLFM: @1$1=3 -->",
            "```",
            "| x |",
            "```",
        ];
        let f = find_tables(&md, Dialect::Markdown);
        assert_eq!(f.len(), 1);
        assert_eq!((f[0].start, f[0].end), (1, 3));
        assert_eq!(f[0].tblfm.as_deref(), Some("$1=2::@1$1=3"));
        let org = ["| a |", "#+tblfm: $1=1"];
        let f = find_tables(&org, Dialect::Org);
        assert_eq!(f[0].tblfm.as_deref(), Some("$1=1"));
        // Org 9.8.7 does not read a `#+TBLFM:` line after a blank line
        // (Emacs oracle case r1-org-tblfm-after-blank); this expectation was
        // `Some` before review round 1, which wrote where Org does not.
        let org = ["| a |", "", "#+tblfm: $1=1"];
        let f = find_tables(&org, Dialect::Org);
        assert_eq!(f[0].tblfm, None);
    }
}
