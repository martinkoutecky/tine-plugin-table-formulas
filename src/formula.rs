//! Org spreadsheet formulas over a parsed [`Table`]: reading the formula
//! line, expanding references the way `org-table-eval-formula` does, and one
//! recalculation pass of the whole table (`C-c C-c` on the `#+TBLFM:` line).
//!
//! Written from the Org manual (section "The spreadsheet") and from reading
//! org-table.el to pin the order of substitutions; no Org code is copied.

use crate::calc::{self, FloatKind, Modes, Outcome};
use crate::printf::{self, FormatError};
use crate::table::{Line, Table};
use regex::Regex;
use std::collections::HashSet;
use std::sync::OnceLock;

/// An Org `user-error` (or a Lisp error Org would signal): the whole command
/// stops and nothing is written.
#[derive(Clone, Debug, PartialEq)]
pub struct Abort {
    pub formula: Option<String>,
    pub message: String,
}

fn abort<T>(formula: Option<&str>, message: impl Into<String>) -> Result<T, Abort> {
    Err(Abort { formula: formula.map(str::to_string), message: message.into() })
}

/// A stored formula, `lhs=rhs` as written.
#[derive(Clone, Debug, PartialEq)]
pub struct Stored {
    pub lhs: String,
    pub rhs: String,
}

impl Stored {
    pub fn text(&self) -> String {
        format!("{}={}", self.lhs, self.rhs)
    }
}

fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pat).expect("static regex"))
}

/// Split a `#+TBLFM:` value into formulas. Entries that are not `lhs=rhs`
/// are ignored, as Org ignores them; a repeated left side is an error.
pub fn parse_tblfm(s: &str) -> Result<Vec<Stored>, Abort> {
    static ENTRY: OnceLock<Regex> = OnceLock::new();
    let entry = re(&ENTRY, r"^(@[-+I<>0-9.$@]+|\$([_a-zA-Z0-9]+|[<>]+)) *= *(.*[^ \t])");
    static SEP: OnceLock<Regex> = OnceLock::new();
    let sep = re(&SEP, r" *:: *");
    let mut out: Vec<Stored> = Vec::new();
    for piece in sep.split(s) {
        let Some(c) = entry.captures(piece) else { continue };
        let mut lhs = c.get(1).map_or("", |m| m.as_str()).to_string();
        // `$name=` (not a column number or `$<`/`$>`) names a field; Org
        // stores it under the bare name.
        if let Some(name) = c.get(2) {
            let n = name.as_str();
            if !(n.bytes().all(|b| b.is_ascii_digit()) || n.bytes().all(|b| b == b'<') || n.bytes().all(|b| b == b'>')) {
                lhs = n.to_string();
            }
        }
        let rhs = c.get(3).map_or("", |m| m.as_str()).to_string();
        if out.iter().any(|f| f.lhs == lhs) {
            return abort(Some(&format!("{lhs}={rhs}")), format!("double definition `{lhs}=` in the formula line"));
        }
        out.push(Stored { lhs, rhs });
    }
    Ok(out)
}

/// Why a formula left its target cell unchanged.
#[derive(Clone, Debug, PartialEq)]
pub struct Skipped {
    pub formula: String,
    pub reason: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Report {
    /// Formulas the plugin could not evaluate exactly. A table with one is
    /// left entirely as it was.
    pub skipped: Vec<Skipped>,
    /// Formulas whose result was `#ERROR`.
    pub errors: Vec<String>,
    /// Tables left unchanged because of a skipped formula.
    pub declined_tables: usize,
    /// A Markdown table followed by an Org `#+TBLFM:` line (not read).
    pub org_line_in_markdown: bool,
}

impl Report {
    pub fn skip(&mut self, formula: &str, reason: &str) {
        if !self.skipped.iter().any(|s| s.formula == formula && s.reason == reason) {
            self.skipped.push(Skipped { formula: formula.to_string(), reason: reason.to_string() });
        }
    }
    fn error(&mut self, formula: &str) {
        if !self.errors.iter().any(|e| e == formula) {
            self.errors.push(formula.to_string());
        }
    }
}

/// The value a reference stands for: one field or a list of fields, or a
/// reason the plugin cannot reproduce what Org does with it.
enum RangeVal {
    One(String),
    Many(Vec<String>),
    Decline(String),
}

enum EvalResult {
    Value(String),
    Error,
    Skip(String),
}

struct Engine<'a> {
    table: &'a mut Table,
    /// Line types plus the imaginary hline Org appends after the last line.
    types: Vec<bool>, // true = hline
    dlines: Vec<usize>,
    ncol: usize,
    /// Work done so far in this recalculation ([`MAX_WORK`]).
    work: std::cell::Cell<usize>,
}

const MAX_SUBSTITUTIONS: usize = 2000;

// Plugin-local budgets (malformed input: a formula or table that would make
// the guest run past the host's invocation timeout or memory cap). Hitting
// one declines the table with a notice instead of trapping the worker.

/// Fields one range reference may cover (the left-hand-side expansion has
/// the same cap).
pub const MAX_RANGE_FIELDS: usize = 100_000;
/// Work one table's recalculation may do: one unit per field read through a
/// reference, [`EVAL_WORK`] per evaluated field. Measured on the release
/// guest under Node (2026-10-20): about 2.5 us per field read and 10-15 us
/// per evaluation, so the cap stays near 125 ms, half the host's 250 ms
/// invocation timeout.
pub const MAX_WORK: usize = 50_000;
pub const EVAL_WORK: usize = 8;
/// Length of one formula after its references are substituted.
pub const MAX_EXPANDED_FORMULA: usize = 256 * 1024;

impl<'a> Engine<'a> {
    fn new(table: &'a mut Table) -> Result<Self, Abort> {
        let mut types: Vec<bool> = table.lines.iter().map(|l| matches!(l, Line::Hline)).collect();
        types.push(true);
        let dlines: Vec<usize> = table
            .lines
            .iter()
            .enumerate()
            .filter(|(_, l)| matches!(l, Line::Data(_)))
            .map(|(i, _)| i)
            .collect();
        let Some(&first) = dlines.first() else {
            return abort(None, "the table has no data lines");
        };
        let ncol = match &table.lines[first] {
            Line::Data(f) => f.len(),
            Line::Hline => 0,
        };
        Ok(Engine { table, types, dlines, ncol, work: std::cell::Cell::new(0) })
    }

    fn fields(&self, line: usize) -> &[String] {
        match self.table.lines.get(line) {
            Some(Line::Data(f)) => f,
            _ => &[],
        }
    }

    fn field(&self, line: usize, col: i64) -> String {
        if col < 1 {
            return String::new();
        }
        self.fields(line).get(col as usize - 1).cloned().unwrap_or_default()
    }

    fn is_dline(&self, line: usize) -> bool {
        matches!(self.table.lines.get(line), Some(Line::Data(_)))
    }

    /// The 1-based data line number of a table line (Org's `@#`).
    fn dline_number(&self, line: usize) -> usize {
        self.dlines.iter().position(|&l| l == line).map_or(0, |p| p + 1)
    }

    fn row_type(&self, want_hline: bool, n: usize, start: i64, backwards: bool, desc: &str, f: &str) -> Result<usize, Abort> {
        let l = self.types.len() as i64;
        if n as i64 >= l {
            // Each step moves at least one line, so this count cannot stay
            // inside the table (and looping it would not end in time).
            return abort(Some(f), format!("row descriptor {desc} leads outside the table"));
        }
        let mut i = start;
        for _ in 0..n {
            loop {
                i += if backwards { -1 } else { 1 };
                if i < 0 || i >= l || self.types[i as usize] == want_hline {
                    break;
                }
            }
        }
        if i < 0 || i >= l {
            return abort(Some(f), format!("row descriptor {desc} leads outside the table"));
        }
        if i == l - 1 {
            return Ok((i - 1) as usize);
        }
        Ok(i as usize)
    }

    /// Org's row descriptor (`3`, `I`, `II+1`, `-2`, `+I`...) to a table line.
    /// `None` is `@0`, which means the current line.
    fn descriptor_line(&self, desc: &str, cline: usize, f: &str) -> Result<Option<usize>, Abort> {
        if !desc.is_empty() && desc.bytes().all(|b| b.is_ascii_digit()) {
            let n: usize = desc.parse().unwrap_or(usize::MAX);
            if n == 0 {
                return Ok(None);
            }
            return match self.dlines.get(n - 1) {
                Some(&l) => Ok(Some(l)),
                None => abort(Some(f), format!("invalid row number @{desc}")),
            };
        }
        static R: OnceLock<Regex> = OnceLock::new();
        let r = re(&R, r"^(([-+])?(I+))?(([-+])?([0-9]+))?");
        let c = r.captures(desc);
        let ok = c.as_ref().is_some_and(|c| {
            let has_i = c.get(3).is_some();
            let has_n = c.get(6).is_some();
            (has_i || has_n) && !(has_i && has_n && c.get(5).is_none())
        });
        let Some(c) = c.filter(|_| ok) else {
            return abort(Some(f), format!("invalid row descriptor `{desc}`"));
        };
        let mut hn = c.get(3).map(|m| m.as_str().len());
        let mut hdir = c.get(2).map(|m| m.as_str());
        let odir = c.get(5).map(|m| m.as_str());
        // A count that does not fit is far outside any table; `row_type`
        // reports it as leading outside.
        let on: Option<usize> = c.get(6).map(|m| m.as_str().parse().unwrap_or(usize::MAX));
        let mut cline = cline as i64;
        if hn.is_some() && hdir.is_none() {
            cline = 0;
            hdir = Some("+");
            if self.types[0] {
                hn = hn.map(|h| h - 1);
            }
        }
        if let Some(h) = hn {
            cline = self.row_type(true, h, cline, hdir == Some("-"), desc, f)? as i64;
        }
        if let Some(o) = on {
            cline = self.row_type(false, o, cline, odir == Some("-"), desc, f)? as i64;
        }
        Ok(Some(cline as usize))
    }

    fn column_of(spec: Option<&str>, col: i64) -> i64 {
        let Some(s) = spec else { return col };
        let s = &s[1..]; // drop `$`
        let n = printf::small_int(s);
        if n == 0 {
            col
        } else if s.starts_with(['-', '+']) {
            n + col
        } else {
            n
        }
    }

    /// `org-table-get-range`. With `corners`, returns the rectangle's lines
    /// and columns instead of values.
    fn get_range(&self, desc: &str, thisline: usize, col: i64, f: &str) -> Result<RangeVal, Abort> {
        let (r1, c1, r2, c2, rangep) = self.range_corners(desc, thisline, col, f)?;
        if !rangep || (r1 == r2 && c1 == c2) {
            let mut l = r1;
            while !self.is_dline(l) {
                l += 1;
                if l >= self.table.lines.len() {
                    return abort(Some(f), format!("reference {desc} leads outside the table"));
                }
            }
            if c1 < 1 {
                return abort(Some(f), format!("reference {desc} points to column {c1}"));
            }
            return Ok(RangeVal::One(self.field(l, c1)));
        }
        let (mut first, mut last) = (r1.min(r2), r1.max(r2));
        let (fc, lc) = (c1.min(c2), c1.max(c2));
        if fc < 1 {
            return abort(Some(f), format!("reference {desc} points to column {fc}"));
        }
        while !self.is_dline(first) {
            first += 1;
            if first >= self.table.lines.len() {
                return abort(Some(f), format!("range {desc} leads outside the table"));
            }
        }
        while !self.is_dline(last) {
            if last == 0 {
                return abort(Some(f), format!("range {desc} leads outside the table"));
            }
            last -= 1;
        }
        if first > last {
            // Both ends sit on hlines with no data line between them. Org
            // 9.8.7 then reads fields outside the range (it gives 2 for
            // `vsum(@I$1..@I$2)` on `|1|2|`/hline/`|3|4|`); not reproduced.
            return Ok(RangeVal::Decline(format!("the range {desc} has no data lines between its hlines")));
        }
        let (lo, hi) = (first, last);
        let rows = (lo..=hi).filter(|&l| self.is_dline(l)).count();
        let area = rows.saturating_mul((lc - fc + 1) as usize);
        if area > MAX_RANGE_FIELDS {
            return Ok(RangeVal::Decline(format!("the range {desc} covers more than {MAX_RANGE_FIELDS} fields")));
        }
        if !self.charge(area) {
            return Ok(RangeVal::Decline(WORK_REASON.into()));
        }
        let mut out = Vec::with_capacity(area);
        for l in lo..=hi {
            if !self.is_dline(l) {
                continue;
            }
            for c in fc..=lc {
                out.push(self.field(l, c));
            }
        }
        Ok(RangeVal::Many(out))
    }

    fn range_corners(&self, desc: &str, thisline: usize, col: i64, f: &str) -> Result<(usize, i64, usize, i64, bool), Abort> {
        static COLS: OnceLock<Regex> = OnceLock::new();
        let cols = re(&COLS, r"^\$[0-9]+\.\.\$[0-9]+$");
        let desc_owned = if cols.is_match(desc) { desc.replace('$', "@0$") } else { desc.to_string() };
        let c = range_re().captures(&desc_owned);
        let Some(c) = c else {
            return abort(Some(f), format!("invalid table range specifier `{desc}`"));
        };
        let rangep = c.get(3).is_some();
        let line_of = |g: Option<regex::Match>| -> Result<usize, Abort> {
            match g.map(|m| m.as_str()).filter(|s| !s.trim().is_empty()) {
                Some(s) => Ok(self.descriptor_line(s, thisline, f)?.unwrap_or(thisline)),
                None => Ok(thisline),
            }
        };
        let r1 = line_of(c.get(1))?;
        let r2 = line_of(c.get(4))?;
        let c1 = Self::column_of(c.get(2).map(|m| m.as_str()), col);
        let c2 = Self::column_of(c.get(5).map(|m| m.as_str()), col);
        Ok((r1, c1, r2, c2, rangep))
    }

    fn line_to_dline(&self, line: usize, above: bool) -> Option<usize> {
        let first = *self.dlines.first()?;
        let last = *self.dlines.last()?;
        if line < first || line > last {
            return None;
        }
        if let Some(p) = self.dlines.iter().position(|&l| l == line) {
            return Some(p + 1);
        }
        if above {
            self.dlines.iter().rposition(|&l| l < line).map(|p| p + 1)
        } else {
            self.dlines.iter().position(|&l| l > line).map(|p| p + 1)
        }
    }
}

const WORK_REASON: &str = "the table needs more computation than the plugin allows in one run (budget: 50000 field reads in total)";

impl Engine<'_> {
    /// Add `units` of work; `false` once over [`MAX_WORK`].
    fn charge(&self, units: usize) -> bool {
        let total = self.work.get().saturating_add(units);
        self.work.set(total);
        total <= MAX_WORK
    }
}

fn range_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    re(
        &R,
        r"@([-+]?I*[-+]?[0-9]*)(\$[-+]?[0-9]+)?(\.\.@?([-+]?I*[-+]?[0-9]*)(\$[-+]?[0-9]+)?)?",
    )
}

/// `@<`, `@>`, `$<`, `$>` (and `@<<`, `$>>`...) to row and column numbers,
/// outside `remote(...)`.
fn handle_first_last(s: &str, nrows: usize, ncol: usize, f: &str) -> Result<String, Abort> {
    static R: OnceLock<Regex> = OnceLock::new();
    let r = re(&R, r"([@$])(<+|>+)|(remote\([^)]+\))");
    let mut out = s.to_string();
    let mut start = 0;
    let mut guard = 0;
    while let Some(c) = r.captures_at(&out, start) {
        guard += 1;
        if guard > MAX_SUBSTITUTIONS {
            return abort(Some(f), "too many references");
        }
        let whole = c.get(0).map_or(0..0, |m| m.range());
        if let Some(m) = c.get(3) {
            start = m.end();
            continue;
        }
        let sigil = c.get(1).map_or("@", |m| m.as_str());
        let marks = c.get(2).map_or("", |m| m.as_str());
        let nmax = if sigil == "@" { nrows } else { ncol } as i64;
        let len = marks.len() as i64;
        let n = if marks.starts_with('<') { len } else { nmax - len + 1 };
        if n < 1 || n > nmax {
            return abort(
                Some(f),
                format!("reference \"{sigil}{marks}\" in expression \"{s}\" points outside the table"),
            );
        }
        let rep = format!("{sigil}{n}");
        out.replace_range(whole.clone(), &rep);
        start = whole.start;
    }
    Ok(out)
}

/// `string-to-number` then `number-to-string`, as the `N` flag converts a
/// field. `Err` is a skip reason (a value the plugin cannot convert exactly).
fn lisp_number_text(x: &str) -> Result<String, String> {
    match printf::string_to_number(x) {
        Some(n) => Ok(printf::number_to_string(&n)),
        None => Err(format!(
            "the N flag on `{}` (an infinity/NaN spelling or an integer over {} digits)",
            x.chars().take(40).collect::<String>(),
            printf::MAX_INT_DIGITS
        )),
    }
}

fn make_reference(v: &RangeVal, keep_empty: bool, numbers: bool) -> Result<String, String> {
    let conv = |x: &str| if numbers { lisp_number_text(x) } else { Ok(x.to_string()) };
    let nonblank = |x: &str| x.chars().any(|c| !c.is_whitespace());
    Ok(match v {
        RangeVal::Decline(why) => return Err(why.clone()),
        RangeVal::One(x) => {
            if nonblank(x) {
                format!("({})", conv(x)?)
            } else if !keep_empty || numbers {
                "(0)".to_string()
            } else {
                "nan".to_string()
            }
        }
        RangeVal::Many(items) => {
            let mut parts: Vec<String> = Vec::with_capacity(items.len());
            for x in items.iter().filter(|x| keep_empty || nonblank(x)) {
                parts.push(if nonblank(x) {
                    conv(x)?
                } else if !keep_empty || numbers {
                    "0".to_string()
                } else {
                    "nan".to_string()
                });
            }
            format!("[{}]", parts.join(","))
        }
    })
}

struct Prepared {
    expr: String,
    fmt: Option<String>,
    modes: Modes,
    numbers: bool,
    keep_empty: bool,
}

/// Split off `;flags` and read the mode letters, as Org does before anything
/// else. `Err` carries a skip reason.
fn prepare(rhs: &str) -> Result<Prepared, String> {
    let mut modes = Modes::default();
    let (mut numbers, mut keep_empty) = (false, false);
    let (expr, fmt) = match rhs.rfind(';') {
        Some(p) => (rhs[..p].to_string(), Some(rhs[p + 1..].to_string())),
        None => (rhs.to_string(), None),
    };
    let fmt = match fmt {
        None => None,
        Some(mut fmt) => {
            static NUM: OnceLock<Regex> = OnceLock::new();
            let num = re(&NUM, r"([pnfse])(-?[0-9]+)");
            while let Some(c) = num.captures(&fmt) {
                let letter = c.get(1).map_or("", |m| m.as_str());
                let n: i64 = printf::small_int(c.get(2).map_or("0", |m| m.as_str()));
                match letter {
                    "p" => {
                        if !(3..=200).contains(&n) {
                            return Err(format!("precision p{n}"));
                        }
                        modes.prec = n;
                    }
                    "n" => modes = Modes { kind: FloatKind::Float, figs: n, ..modes },
                    "f" => modes = Modes { kind: FloatKind::Fix, figs: n, ..modes },
                    "s" => modes = Modes { kind: FloatKind::Sci, figs: n, ..modes },
                    _ => modes = Modes { kind: FloatKind::Eng, figs: n, ..modes },
                }
                if modes.figs.abs() > 200 {
                    return Err("a display precision above 200 digits".into());
                }
                let r = c.get(0).map_or(0..0, |m| m.range());
                fmt.replace_range(r, "");
            }
            static FLAG: OnceLock<Regex> = OnceLock::new();
            let flag = re(&FLAG, r"[tTUNLEDRFSu]");
            while let Some(m) = flag.find(&fmt) {
                match m.as_str() {
                    "t" | "T" | "U" => return Err("durations (flags t, T, U) are not supported".into()),
                    "N" => numbers = true,
                    "E" => keep_empty = true,
                    "F" => return Err("fraction mode (flag F) is not supported".into()),
                    "S" => return Err("symbolic mode (flag S) is not supported".into()),
                    "u" => return Err("units mode (flag u) is not supported".into()),
                    _ => {} // L (Lisp only), D/R (angles: no trigonometry here)
                }
                let r = m.range();
                fmt.replace_range(r, "");
            }
            if fmt.chars().all(char::is_whitespace) {
                None
            } else {
                Some(fmt)
            }
        }
    };
    Ok(Prepared { expr, fmt, modes, numbers, keep_empty })
}

impl<'a> Engine<'a> {
    /// `org-table-eval-formula` for the field at `line`, column `n0`.
    fn eval(&self, rhs: &str, line: usize, n0: usize, f: &str) -> Result<EvalResult, Abort> {
        if !self.charge(EVAL_WORK) {
            return Ok(EvalResult::Skip(WORK_REASON.into()));
        }
        let p = match prepare(rhs) {
            Ok(p) => p,
            Err(why) => return Ok(EvalResult::Skip(why)),
        };
        static NAME: OnceLock<Regex> = OnceLock::new();
        if re(&NAME, r"\$[a-zA-Z_]").is_match(&p.expr) {
            return Ok(EvalResult::Skip("named columns, parameters and constants are not supported".into()));
        }
        let formula = handle_first_last(&p.expr, self.dlines.len(), self.ncol, f)?;
        let mut fields: Vec<String> = self.fields(line).to_vec();
        if p.numbers {
            for x in fields.iter_mut() {
                if x.chars().any(|c| !c.is_whitespace()) {
                    match lisp_number_text(x) {
                        Ok(t) => *x = t,
                        Err(why) => return Ok(EvalResult::Skip(why)),
                    }
                }
            }
        }
        if formula.len() > 2 && formula.starts_with("'(") {
            return Ok(EvalResult::Skip("Emacs Lisp formula (kept, not evaluated)".into()));
        }
        let mut form = formula.clone();
        let dl = self.dline_number(line);
        form = form.replace("@#", &dl.to_string()).replace("$#", &n0.to_string());
        static OLD: OnceLock<Regex> = OnceLock::new();
        if re(&OLD, r"&[-+0-9I]").is_match(&form) {
            return abort(Some(f), "formula uses the old syntax for vertical references");
        }
        static REMOTE: OnceLock<Regex> = OnceLock::new();
        if re(&REMOTE, r"(?:^|[^A-Za-z0-9_])remote\(").is_match(&form) {
            return Ok(EvalResult::Skip("remote references are not supported".into()));
        }
        // Complex ranges: the first `@...` match, while it is longer than `@`.
        let mut guard = 0;
        loop {
            let Some(m) = range_re().find(&form) else { break };
            if m.as_str().len() <= 1 {
                break;
            }
            guard += 1;
            if guard > MAX_SUBSTITUTIONS {
                return abort(Some(f), "too many references");
            }
            let v = self.get_range(m.as_str(), line, n0 as i64, f)?;
            let rpl = match make_reference(&v, p.keep_empty, p.numbers) {
                Ok(r) => r,
                Err(why) => return Ok(EvalResult::Skip(why)),
            };
            if form.len() - m.as_str().len() + rpl.len() > MAX_EXPANDED_FORMULA {
                return Ok(EvalResult::Skip(format!("the formula grows past {MAX_EXPANDED_FORMULA} bytes")));
            }
            if rpl.contains(&form) {
                return abort(Some(f), format!("invalid reference \"{form}\""));
            }
            let r = m.range();
            form.replace_range(r, &rpl);
        }
        // Ranges within the current row: $a..$b.
        static SIMPLE: OnceLock<Regex> = OnceLock::new();
        let simple = re(&SIMPLE, r"\$(([-+])?[0-9]+)\.\.\$(([-+])?[0-9]+)");
        guard = 0;
        while let Some(c) = simple.captures(&form) {
            guard += 1;
            if guard > MAX_SUBSTITUTIONS {
                return abort(Some(f), "too many references");
            }
            let num = |g: usize| -> i64 { printf::small_int(c.get(g).map_or("0", |m| m.as_str())) };
            let a = num(1) - 1 + if c.get(2).is_some() { n0 as i64 } else { 0 };
            let b = num(3) + if c.get(4).is_some() { n0 as i64 } else { 0 };
            if a < 0 || b > fields.len() as i64 || a > b {
                return abort(Some(f), format!("range {} is outside the row", c.get(0).map_or("", |m| m.as_str())));
            }
            let slice = fields[a as usize..b as usize].to_vec();
            let rpl = match make_reference(&RangeVal::Many(slice), p.keep_empty, p.numbers) {
                Ok(r) => r,
                Err(why) => return Ok(EvalResult::Skip(why)),
            };
            if form.len() + rpl.len() > MAX_EXPANDED_FORMULA {
                return Ok(EvalResult::Skip(format!("the formula grows past {MAX_EXPANDED_FORMULA} bytes")));
            }
            let r = c.get(0).map_or(0..0, |m| m.range());
            form.replace_range(r, &rpl);
        }
        // Single fields of the current row: $n, $+n, $-n.
        static SINGLE: OnceLock<Regex> = OnceLock::new();
        let single = re(&SINGLE, r"\$(([-+])?[0-9]+)");
        guard = 0;
        while let Some(c) = single.captures(&form) {
            guard += 1;
            if guard > MAX_SUBSTITUTIONS {
                return abort(Some(f), "too many references");
            }
            let raw = c.get(1).map_or("0", |m| m.as_str());
            let mut n = printf::small_int(raw);
            if c.get(2).is_some() {
                n += n0 as i64;
            }
            let idx = if n == 0 { n0 as i64 } else { n.max(1) };
            let whole = c.get(0).map_or("", |m| m.as_str()).to_string();
            let Some(x) = fields.get(idx as usize - 1) else {
                return abort(Some(f), format!("invalid field specifier \"{whole}\""));
            };
            let rpl = match make_reference(&RangeVal::One(x.clone()), p.keep_empty, p.numbers) {
                Ok(r) => r,
                Err(why) => return Ok(EvalResult::Skip(why)),
            };
            if form.len() + rpl.len() > MAX_EXPANDED_FORMULA {
                return Ok(EvalResult::Skip(format!("the formula grows past {MAX_EXPANDED_FORMULA} bytes")));
            }
            if rpl.contains(&formula) {
                return abort(Some(f), format!("invalid field specifier \"{whole}\""));
            }
            let r = c.get(0).map_or(0..0, |m| m.range());
            form.replace_range(r, &rpl);
        }
        static TS: OnceLock<Regex> = OnceLock::new();
        if re(&TS, r"[<\[][0-9]{4}-[0-9]{2}-[0-9]{2}").is_match(&form) {
            return Ok(EvalResult::Skip("dates and times are not supported".into()));
        }
        let num_only = p.numbers && !p.keep_empty;
        let ev = match calc::calc_eval(&form, p.modes, num_only) {
            Ok(v) => v,
            Err(Outcome::CalcError(_)) => return Ok(EvalResult::Error),
            Err(Outcome::Unsupported(why)) => return Ok(EvalResult::Skip(why)),
        };
        let out = match &p.fmt {
            Some(fmt) => match printf::string_to_number(&ev).ok_or(FormatError::Unsupported(
                "of a number the plugin cannot convert exactly".into(),
            )).and_then(|n| printf::format_one(fmt, &n)) {
                Ok(s) => s,
                Err(FormatError::TooManySpecs) => {
                    return abort(Some(f), format!("the format `{fmt}` has more than one `%` conversion"));
                }
                Err(FormatError::Unsupported(why)) => return Ok(EvalResult::Skip(format!("format {why}"))),
            },
            None => ev,
        };
        let out = out.replace('\n', "");
        if out.len() > crate::MAX_RAW_UTF16 {
            return Ok(EvalResult::Skip("the result is longer than a block may be".into()));
        }
        if out.contains('|') {
            return Ok(EvalResult::Skip("the result contains `|`".into()));
        }
        Ok(EvalResult::Value(out))
    }

    fn set_field(&mut self, line: usize, col: usize, value: String) {
        if let Some(Line::Data(f)) = self.table.lines.get_mut(line) {
            if f.len() < col {
                f.resize(col, String::new());
            }
            f[col - 1] = value;
        }
    }

    /// Store one result. Returns `false` when the formula was declined: the
    /// caller then stops, and the whole table is left as it was, since later
    /// formulas may read the cell Org would have changed.
    fn apply(&mut self, res: EvalResult, line: usize, col: usize, formula: &str, report: &mut Report) -> bool {
        match res {
            // Org inserts the text as is; aligning the table then trims the
            // field (`%5.1f` padding does not survive).
            EvalResult::Value(v) => {
                self.set_field(line, col, v.trim_matches([' ', '\t', '\r', '\n']).to_string())
            }
            EvalResult::Error => {
                report.error(formula);
                self.set_field(line, col, "#ERROR".to_string());
            }
            EvalResult::Skip(why) => {
                report.skip(formula, &why);
                return false;
            }
        }
        true
    }
}

/// One `org-table-recalculate` pass over the whole table (what `C-c C-c` on
/// the formula line does). The table is changed in place.
///
/// When the report has a skipped formula, the pass stopped there and the
/// table holds a partial result that the caller must discard: a declined
/// formula leaves a cell Org would have changed, so any later formula could
/// compute from the wrong value (review finding F1).
pub fn recalculate(table: &mut Table, formulas: &[Stored]) -> Result<Report, Abort> {
    let mut report = Report::default();
    let mut eng = Engine::new(table)?;
    let nrows = eng.dlines.len();
    let ncol = eng.ncol;
    let mut eqlist: Vec<Stored> = formulas.to_vec();
    eqlist.sort_by(|a, b| a.lhs.as_bytes().cmp(b.lhs.as_bytes()));

    // Field formulas keep the (expanded) target and their original text.
    let mut cols: Vec<(usize, String, String)> = Vec::new();
    let mut field_eqs: Vec<(String, String, String)> = Vec::new();
    static HLINE_LHS: OnceLock<Regex> = OnceLock::new();
    static COL_LHS: OnceLock<Regex> = OnceLock::new();
    for e in &eqlist {
        let text = e.text();
        let f = Some(text.as_str());
        let rhs = handle_first_last(&e.rhs, nrows, ncol, &text)?;
        let lhs = if re(&HLINE_LHS, r"^@-?I+").is_match(&e.lhs) {
            return abort(f, "can't assign to an hline-relative reference");
        } else if e.lhs.starts_with("$<") || e.lhs.starts_with("$>") {
            let new = handle_first_last(&e.lhs, nrows, ncol, &text)?;
            if eqlist.iter().any(|o| o.lhs == new) {
                return abort(f, format!("\"{}=\" tries to overwrite the existing formula for column {new}", e.lhs));
            }
            new
        } else {
            e.lhs.clone()
        };
        let lhs = handle_first_last(&lhs, nrows, ncol, &text)?;
        if let Some(n) = re(&COL_LHS, r"^\$([0-9]+)$").captures(&lhs).and_then(|c| c.get(1)) {
            let n: usize = n.as_str().parse().unwrap_or(0);
            if n == 0 || n > 1000 {
                return abort(f, "formula column target too large");
            }
            cols.push((n, rhs, text));
        } else if !lhs.starts_with('@') && !lhs.starts_with('$') {
            report.skip(&text, "named fields are not supported");
            return Ok(report);
        } else {
            field_eqs.push((lhs, rhs, text));
        }
    }

    // Expand field formulas over their left-hand ranges.
    let mut expanded: Vec<(usize, usize, String, String)> = Vec::new(); // (dline, col, rhs, text)
    let thisline = eng.table.lines.len().saturating_sub(1);
    static ONE_FIELD: OnceLock<Regex> = OnceLock::new();
    static ROW: OnceLock<Regex> = OnceLock::new();
    static ABS_FIELD: OnceLock<Regex> = OnceLock::new();
    let abs_field = re(&ABS_FIELD, r"^@([1-9][0-9]*)\$([1-9][0-9]*)$");
    for (lhs, rhs, text) in &field_eqs {
        let f = Some(text.as_str());
        let mut targets: Vec<String> = Vec::new();
        if re(&ONE_FIELD, r"^@[-+0-9]+\$-?[0-9]+$").is_match(lhs) {
            targets.push(lhs.clone());
        } else if re(&ROW, r"^@[0-9]+$").is_match(lhs) {
            for c in 1..=ncol {
                targets.push(format!("{lhs}${c}"));
            }
        } else {
            let (a, c1, b, c2, _) = eng.range_corners(lhs, thisline, 1, text)?;
            let (first, last) = (a.min(b), a.max(b));
            let (fc, lc) = (c1.min(c2), c1.max(c2));
            let (Some(r1), Some(r2)) = (eng.line_to_dline(first, false), eng.line_to_dline(last, true)) else {
                return abort(f, format!("the range {lhs} has no data lines"));
            };
            if r1 > r2 {
                report.skip(text, &format!("the target range {lhs} has no data lines between its hlines"));
                return Ok(report);
            }
            if fc < 1 || lc > 1000 {
                return abort(f, format!("the range {lhs} has an invalid column"));
            }
            if (r2 + 1).saturating_sub(r1) as i64 * (lc - fc + 1) > MAX_RANGE_FIELDS as i64 {
                return abort(f, format!("the range {lhs} is too large"));
            }
            for r in r1..=r2 {
                for c in fc..=lc {
                    targets.push(format!("@{r}${c}"));
                }
            }
        }
        for t in targets {
            let Some(c) = abs_field.captures(&t) else {
                return abort(f, format!("unknown field {t}"));
            };
            let r: usize = c.get(1).map_or("0", |m| m.as_str()).parse().unwrap_or(usize::MAX);
            let col: usize = c.get(2).map_or("0", |m| m.as_str()).parse().unwrap_or(usize::MAX);
            if r == 0 || r > nrows {
                return abort(f, format!("invalid row number in {t}"));
            }
            expanded.push((r, col, rhs.clone(), text.clone()));
        }
    }
    let mut seen: HashSet<(usize, usize)> = HashSet::new();
    for (r, c, _, text) in &expanded {
        if !seen.insert((*r, *c)) {
            return abort(Some(text), format!("several field/range formulas try to set @{r}${c}"));
        }
    }
    let untouchable: HashSet<(usize, usize)> =
        expanded.iter().map(|(r, c, _, _)| (eng.dlines[r - 1], *c)).collect();

    // Which rows the column formulas run on.
    let first_field = |eng: &Engine, l: usize| eng.fields(l).first().cloned().unwrap_or_default();
    let marked = eng.dlines.iter().any(|&l| matches!(first_field(&eng, l).trim(), "!" | "$" | "^" | "_" | "#" | "*"));
    let rows: Vec<usize> = if marked {
        eng.dlines.iter().copied().filter(|&l| matches!(first_field(&eng, l).trim(), "#" | "*")).collect()
    } else {
        let lines = &eng.table.lines;
        let begin = lines.iter().position(|l| matches!(l, Line::Data(_))).and_then(|d| {
            let h = d + lines[d..].iter().position(|l| matches!(l, Line::Hline))?;
            Some(h + lines[h..].iter().position(|l| matches!(l, Line::Data(_)))?)
        });
        let begin = begin.unwrap_or(0);
        eng.dlines.iter().copied().filter(|&l| l >= begin).collect()
    };
    if !cols.is_empty() {
        for &l in &rows {
            let ff = first_field(&eng, l);
            if matches!(ff.trim(), "_" | "^" | "!" | "$" | "/") {
                continue;
            }
            for (c, rhs, text) in &cols {
                if eng.fields(l).len() < *c {
                    eng.set_field(l, *c, String::new());
                }
                if untouchable.contains(&(l, *c)) {
                    continue;
                }
                let res = eng.eval(rhs, l, *c, text)?;
                if !eng.apply(res, l, *c, text, &mut report) {
                    return Ok(report);
                }
            }
        }
    }
    if let Some((_, _, _, text)) = expanded.first() {
        // Org checks "Missing columns" against a column count it takes at
        // the cursor position, which moves between field formulas; with rows
        // of different lengths that is not reproduced (review finding F5).
        let widths: HashSet<usize> = eng.dlines.iter().map(|&l| eng.fields(l).len()).collect();
        if widths.len() > 1 {
            report.skip(text, "field formulas in a table whose rows have different numbers of fields");
            return Ok(report);
        }
    }
    let width = eng.dlines.first().map_or(0, |&l| eng.fields(l).len());
    for (r, c, rhs, text) in &expanded {
        let l = eng.dlines[r - 1];
        if *c > 1000 {
            return abort(Some(text), "formula column target too large");
        }
        if *c > width {
            return abort(Some(text), format!("missing columns in the table for @{r}${c}"));
        }
        let res = eng.eval(rhs, l, *c, text)?;
        if !eng.apply(res, l, *c, text, &mut report) {
            return Ok(report);
        }
    }
    Ok(report)
}
