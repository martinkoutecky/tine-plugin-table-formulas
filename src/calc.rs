//! A small subset of Emacs Calc's algebraic evaluation, as Org table formulas
//! use it (`calc-eval` with `org-calc-default-modes`).
//!
//! Written from the documented behaviour of Calc and from reading its source
//! for the number model (decimal floats rounded to `calc-internal-prec`
//! significant digits, exact integers, the `float`/`fix`/`sci`/`eng` display
//! formats). No Calc or Org code is copied.
//!
//! The evaluator distinguishes three outcomes:
//! * a value Calc would produce, which we can display exactly;
//! * a Calc error (`calc-eval` returns an error, Org writes `#ERROR`);
//! * [`Outcome::Unsupported`]: anything outside the subset (symbolic algebra,
//!   dates, units, unknown functions...). The caller keeps the cell unchanged
//!   and says so, because guessing would write a value Org would not.

use num_bigint::BigInt;
use num_traits::{One, Signed, ToPrimitive, Zero};
use std::cmp::Ordering;

/// Upper bound on decimal exponents we accept; Calc allows far more, but such
/// numbers in a table are almost certainly not intended and cost memory.
const MAX_EXP: i64 = 10_000;

/// Budgets against malformed input, so that it is declined with a notice
/// rather than overflowing the guest's stack or running past the host's
/// invocation timeout. Integers: about 2,000 decimal digits (Calc's bignums are
/// unbounded; multiplying two 20,000-digit numbers took 60 ms in the guest).
pub const MAX_INT_BITS: u64 = 6_644;
/// Nesting depth of a formula's syntax tree (parentheses, operator chains).
pub const MAX_DEPTH: usize = 200;
/// Digits in one number literal.
const MAX_LITERAL_DIGITS: usize = 2_000;

fn int_budget(x: BigInt) -> CResult<BigInt> {
    if x.bits() > MAX_INT_BITS {
        return unsupported("an integer with more than 2000 digits");
    }
    Ok(x)
}

#[derive(Clone, Debug, PartialEq)]
pub enum Num {
    Int(BigInt),
    /// mantissa * 10^exp, normalised: no trailing zeros, at most `prec` digits.
    Float {
        m: BigInt,
        e: i64,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Num(Num),
    /// Calc's `nan` (Org inserts it for empty fields with the `E` flag).
    Nan,
    /// Calc's infinities: `inf` (1), `-inf` (-1) and the undirected `uinf` (0).
    Inf(i8),
    /// A plain Calc variable, as Calc reads a text field such as `z`.
    Sym(String),
    /// A string literal `"..."` (a vector of character codes in Calc).
    Str(String),
    Vec(Vec<Value>),
    /// The result of `string(...)`: displayed as raw text.
    Text(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    Unsupported(String),
    CalcError(String),
}

pub type CResult<T> = Result<T, Outcome>;

fn unsupported<T>(why: impl Into<String>) -> CResult<T> {
    Err(Outcome::Unsupported(why.into()))
}

fn calc_error<T>(why: impl Into<String>) -> CResult<T> {
    Err(Outcome::CalcError(why.into()))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FloatKind {
    Float,
    Fix,
    Sci,
    Eng,
}

#[derive(Clone, Copy, Debug)]
pub struct Modes {
    pub prec: i64,
    pub kind: FloatKind,
    pub figs: i64,
}

impl Default for Modes {
    /// `org-calc-default-modes`: internal precision 12, float format (float 8).
    fn default() -> Self {
        Modes {
            prec: 12,
            kind: FloatKind::Float,
            figs: 8,
        }
    }
}

// ---------------------------------------------------------------- numbers

fn pow10(n: i64) -> BigInt {
    num_traits::pow::pow(BigInt::from(10), n.max(0) as usize)
}

pub fn numdigs(a: &BigInt) -> i64 {
    if a.is_zero() {
        0
    } else if let Some(v) = a.magnitude().to_u128() {
        v.ilog10() as i64 + 1
    } else {
        a.magnitude().to_string().len() as i64
    }
}

/// Multiply `a` by 10^n, rounding half away from zero when n < 0.
fn scale_rounding(a: &BigInt, n: i64) -> BigInt {
    if n >= 0 {
        return a * pow10(n);
    }
    if a.is_negative() {
        return -scale_rounding(&-a, n);
    }
    let shifted = if n == -1 {
        a.clone()
    } else {
        a / pow10(-1 - n)
    };
    (shifted + BigInt::from(5)) / BigInt::from(10)
}

pub fn make_float(m: BigInt, e: i64, prec: i64) -> CResult<Num> {
    if m.is_zero() {
        return Ok(Num::Float { m, e: 0 });
    }
    let mut m = m;
    let mut e = e;
    let ldiff = prec - numdigs(&m);
    if ldiff < 0 {
        m = scale_rounding(&m, ldiff);
        e -= ldiff;
    }
    let ten = BigInt::from(10);
    while !m.is_zero() && (&m % &ten).is_zero() {
        m /= &ten;
        e += 1;
    }
    if e.abs() > MAX_EXP {
        return unsupported("number too large or too small");
    }
    Ok(Num::Float { m, e })
}

fn to_float(a: &Num, prec: i64) -> CResult<(BigInt, i64)> {
    match a {
        Num::Int(i) => match make_float(i.clone(), 0, prec)? {
            Num::Float { m, e } => Ok((m, e)),
            Num::Int(_) => unreachable_num(),
        },
        Num::Float { m, e } => Ok((m.clone(), *e)),
    }
}

fn unreachable_num<T>() -> CResult<T> {
    unsupported("internal number error")
}

pub fn is_zero(a: &Num) -> bool {
    match a {
        Num::Int(i) => i.is_zero(),
        Num::Float { m, .. } => m.is_zero(),
    }
}

fn is_float(a: &Num) -> bool {
    matches!(a, Num::Float { .. })
}

fn float_of(a: &Num, prec: i64) -> CResult<Num> {
    let (m, e) = to_float(a, prec)?;
    Ok(Num::Float { m, e })
}

pub fn add(a: &Num, b: &Num, prec: i64) -> CResult<Num> {
    if let (Num::Int(x), Num::Int(y)) = (a, b) {
        return Ok(Num::Int(int_budget(x + y)?));
    }
    if is_zero(a) {
        return if is_float(a) && !is_float(b) {
            float_of(b, prec)
        } else {
            Ok(b.clone())
        };
    }
    if is_zero(b) {
        return if is_float(b) && !is_float(a) {
            float_of(a, prec)
        } else {
            Ok(a.clone())
        };
    }
    let (ma, ea) = to_float(a, prec)?;
    let (mb, eb) = to_float(b, prec)?;
    let ediff = ea - eb;
    if ediff >= 0 {
        if ediff >= prec + prec {
            return Ok(Num::Float { m: ma, e: ea });
        }
        make_float(mb + ma * pow10(ediff), eb, prec)
    } else {
        let ediff = -ediff;
        if ediff >= prec + prec {
            return Ok(Num::Float { m: mb, e: eb });
        }
        make_float(ma + mb * pow10(ediff), ea, prec)
    }
}

pub fn neg(a: &Num) -> Num {
    match a {
        Num::Int(i) => Num::Int(-i),
        Num::Float { m, e } => Num::Float { m: -m, e: *e },
    }
}

pub fn sub(a: &Num, b: &Num, prec: i64) -> CResult<Num> {
    add(a, &neg(b), prec)
}

pub fn mul(a: &Num, b: &Num, prec: i64) -> CResult<Num> {
    if let (Num::Int(x), Num::Int(y)) = (a, b) {
        if x.bits() + y.bits() > MAX_INT_BITS + 1 {
            return unsupported("an integer with more than 2000 digits");
        }
        return Ok(Num::Int(int_budget(x * y)?));
    }
    if is_zero(a) {
        return if is_float(b) && !is_float(a) {
            float_of(a, prec)
        } else {
            Ok(a.clone())
        };
    }
    if is_zero(b) {
        return if is_float(a) && !is_float(b) {
            float_of(b, prec)
        } else {
            Ok(b.clone())
        };
    }
    let (ma, ea) = to_float(a, prec)?;
    let (mb, eb) = to_float(b, prec)?;
    make_float(ma * mb, ea + eb, prec)
}

fn div_float(ma: BigInt, ea: i64, mb: BigInt, eb: i64, prec: i64) -> CResult<Num> {
    let ldiff = (prec + 1 - (numdigs(&ma) - numdigs(&mb))).max(0);
    let q = (ma * pow10(ldiff)) / mb;
    make_float(q, ea - eb - ldiff, prec)
}

pub fn div(a: &Num, b: &Num, prec: i64) -> CResult<Num> {
    if is_zero(b) {
        return unsupported("division by zero (Calc leaves it unevaluated)");
    }
    if is_zero(a) {
        return if is_float(b) && !is_float(a) {
            float_of(a, prec)
        } else {
            Ok(a.clone())
        };
    }
    if let (Num::Int(x), Num::Int(y)) = (a, b) {
        let q = x / y;
        let r = x % y;
        if r.is_zero() {
            return Ok(Num::Int(q));
        }
        let (ma, ea) = to_float(a, prec)?;
        let (mb, eb) = to_float(b, prec)?;
        return div_float(ma, ea, mb, eb, prec);
    }
    let (ma, ea) = to_float(a, prec)?;
    let (mb, eb) = to_float(b, prec)?;
    div_float(ma, ea, mb, eb, prec)
}

pub fn renormalize(a: Num, prec: i64) -> CResult<Num> {
    match a {
        Num::Int(_) => Ok(a),
        Num::Float { m, e } => make_float(m, e, prec),
    }
}

pub fn compare(a: &Num, b: &Num) -> Ordering {
    let (ma, ea) = match a {
        Num::Int(i) => (i.clone(), 0),
        Num::Float { m, e } => (m.clone(), *e),
    };
    let (mb, eb) = match b {
        Num::Int(i) => (i.clone(), 0),
        Num::Float { m, e } => (m.clone(), *e),
    };
    let e = ea.min(eb);
    (ma * pow10(ea - e)).cmp(&(mb * pow10(eb - e)))
}

fn pow_int(base: &BigInt, exp: &BigInt) -> CResult<BigInt> {
    let Some(n) = exp.to_u32() else {
        return unsupported("power exponent out of range");
    };
    if n > 4096 || (base.abs() > BigInt::one() && n > 512) {
        return unsupported("power too large");
    }
    if base.bits().saturating_sub(1).saturating_mul(n as u64) > MAX_INT_BITS {
        return unsupported("an integer with more than 2000 digits");
    }
    int_budget(num_traits::pow::pow(base.clone(), n as usize))
}

// ---------------------------------------------------------------- reading

/// Read a Calc number token (digits, `.`, optional exponent).
pub fn read_number(s: &str, prec: i64) -> CResult<Num> {
    if s.len() > MAX_LITERAL_DIGITS {
        return unsupported("a number with more than 2000 digits");
    }
    let (mant, exp) = match s.find(['e', 'E']) {
        Some(i) => (&s[..i], Some(&s[i + 1..])),
        None => (s, None),
    };
    let base = if mant.is_empty() {
        if exp.is_some() {
            Num::Int(BigInt::one())
        } else {
            return calc_error("bad number");
        }
    } else if mant.bytes().all(|c| c.is_ascii_digit()) {
        Num::Int(
            mant.parse::<BigInt>()
                .map_err(|_| Outcome::CalcError("bad number".into()))?,
        )
    } else if let Some((int, frac)) = mant.split_once('.') {
        if frac.contains('.') || (int.is_empty() && frac.is_empty()) {
            return calc_error("bad number format");
        }
        if !int.bytes().all(|c| c.is_ascii_digit()) || !frac.bytes().all(|c| c.is_ascii_digit()) {
            return calc_error("bad number format");
        }
        let digits = format!("{int}{frac}");
        let m = if digits.is_empty() {
            BigInt::zero()
        } else {
            digits
                .parse::<BigInt>()
                .map_err(|_| Outcome::CalcError("bad number".into()))?
        };
        make_float(m, -(frac.len() as i64), prec)?
    } else {
        return calc_error("bad number format");
    };
    match exp {
        None => Ok(base),
        Some(x) => {
            let x = x.replace('_', "-");
            let n: i64 = x
                .parse()
                .map_err(|_| Outcome::CalcError("bad exponent".into()))?;
            if n.abs() > MAX_EXP {
                return unsupported("exponent too large");
            }
            let (m, e) = to_float(&base, prec)?;
            make_float(m, e + n, prec)
        }
    }
}

// ---------------------------------------------------------------- display

fn format_float_pos(m: &BigInt, e: i64, modes: &Modes) -> String {
    let mut figs = modes.figs;
    let kind = modes.kind;
    if kind == FloatKind::Fix && (figs < 0 || e + numdigs(m) > -figs) {
        if figs < 0 {
            figs = -figs;
        }
        let mant = scale_rounding(m, e + figs);
        let mut s = mant.to_string();
        if (s.len() as i64) <= figs {
            s = format!("{}{}", "0".repeat((figs - s.len() as i64 + 1) as usize), s);
        }
        if figs > 0 {
            let cut = s.len() - figs as usize;
            return format!("{}.{}", &s[..cut], &s[cut..]);
        }
        return format!("{s}.");
    }
    if figs < 0 {
        figs += modes.prec;
    }
    let mut m = m.clone();
    let mut e = e;
    if figs > 0 {
        let adj = figs - numdigs(&m);
        if adj < 0 {
            m = scale_rounding(&m, adj);
            e -= adj;
        }
    }
    let s = m.to_string();
    let len = s.len() as i64;
    let dpos = e + len;
    if kind == FloatKind::Float && dpos <= modes.prec && dpos >= -1 {
        if dpos == 0 {
            format!("0.{s}")
        } else if e <= 0 && dpos > 0 {
            let d = dpos as usize;
            format!("{}.{}", &s[..d], &s[d..])
        } else if e > 0 {
            format!("{s}{}.", "0".repeat(e as usize))
        } else {
            format!("0.{}{s}", "0".repeat((-dpos) as usize))
        }
    } else {
        let eadj = e + len;
        let scale: i64 = if kind == FloatKind::Eng {
            1 + (eadj + 300002).rem_euclid(3)
        } else {
            1
        };
        let mut s = s;
        if scale > s.len() as i64 {
            s.push_str(&"0".repeat((scale - s.len() as i64) as usize));
        }
        if scale < s.len() as i64 {
            let k = scale as usize;
            s = format!("{}.{}", &s[..k], &s[k..]);
        }
        format!("{s}e{}", eadj - scale)
    }
}

pub fn format_num(a: &Num, modes: &Modes) -> String {
    match a {
        Num::Int(i) => i.to_string(),
        Num::Float { m, e } => {
            if m.is_negative() {
                format!("-{}", format_float_pos(&-m, *e, modes))
            } else {
                format_float_pos(m, *e, modes)
            }
        }
    }
}

/// Display a final result the way `calc-eval` returns it.
pub fn display(v: &Value, modes: &Modes) -> CResult<String> {
    match v {
        Value::Num(n) => Ok(format_num(n, modes)),
        Value::Nan => Ok("nan".to_string()),
        Value::Inf(1) => Ok("inf".to_string()),
        Value::Inf(-1) => Ok("-inf".to_string()),
        Value::Inf(_) => Ok("uinf".to_string()),
        Value::Sym(s) => Ok(s.clone()),
        Value::Text(s) => Ok(s.clone()),
        Value::Str(_) => unsupported("a string literal as the result"),
        Value::Vec(_) => unsupported("a vector as the result"),
    }
}

// ---------------------------------------------------------------- tokens

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Num(String),
    Sym(String),
    Str(String),
    Punc(String),
    End,
}

/// Calc constants and special variables: a symbol with one of these names is
/// not a plain unknown, so we do not try to reproduce what Calc does with it.
const SPECIAL_SYMBOLS: &[&str] = &["e", "pi", "i", "gamma", "phi"];

fn tokenize(src: &str) -> CResult<Vec<Tok>> {
    let s: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < s.len() {
        let c = s[i];
        if c == ' ' || c == '\n' || c == '\t' {
            i += 1;
            continue;
        }
        if c.is_ascii_alphabetic() || ('α'..='ω').contains(&c) || ('Α'..='Ω').contains(&c) {
            let start = i;
            while i < s.len()
                && (s[i].is_ascii_alphanumeric()
                    || ('α'..='ω').contains(&s[i])
                    || ('Α'..='Ω').contains(&s[i])
                    || s[i] == '\''
                    || s[i] == '#')
            {
                i += 1;
            }
            let word: String = s[start..i].iter().collect();
            if word.contains(['\'', '#']) || !word.is_ascii() {
                return unsupported(format!("Calc symbol `{word}`"));
            }
            out.push(Tok::Sym(word));
            continue;
        }
        if c.is_ascii_digit() || (c == '.' && s.get(i + 1).is_some_and(|d| d.is_ascii_digit())) {
            let start = i;
            while i < s.len() && (s[i].is_ascii_digit() || s[i] == '.') {
                i += 1;
            }
            if i < s.len() && (s[i] == 'e' || s[i] == 'E') {
                let mut j = i + 1;
                if j < s.len() && (s[j] == '-' || s[j] == '+' || s[j] == '_') {
                    j += 1;
                }
                if j < s.len() && s[j].is_ascii_digit() {
                    while j < s.len() && s[j].is_ascii_digit() {
                        j += 1;
                    }
                    i = j;
                }
            }
            // Radix, HMS, fraction and date forms continue a number in Calc.
            if i < s.len() && matches!(s[i], '#' | '@' | '\'' | ':' | '"' | '^') {
                if s[i] != '^' || s.get(i + 1) == Some(&'^') {
                    return unsupported("Calc number notation (radix, HMS, fraction or date)");
                }
            }
            out.push(Tok::Num(s[start..i].iter().collect()));
            continue;
        }
        if c == '"' {
            let mut j = i + 1;
            let mut text = String::new();
            let mut closed = false;
            while j < s.len() {
                if s[j] == '\\' {
                    return unsupported("escape in a string literal");
                }
                if s[j] == '"' {
                    closed = true;
                    break;
                }
                text.push(s[j]);
                j += 1;
            }
            if !closed {
                return unsupported("unterminated string literal");
            }
            out.push(Tok::Str(text));
            i = j + 1;
            continue;
        }
        if c == '$' || c == '%' && s.get(i + 1) == Some(&'%') {
            return unsupported(format!("`{c}` in a Calc expression"));
        }
        // Multi-character punctuation that Calc reads as one token.
        const MULTI: &[&str] = &[
            "~=", "<=", ">=", "<>", "/=", "+/-", "\\dots", "\\ldots", "**", "<<", ">>", "==", "!=",
            "&&&", "|||", "!!!", "&&", "||", "!!", ":=", "::", "=>",
        ];
        let rest: String = s[i..s.len().min(i + 7)].iter().collect();
        if let Some(m) = MULTI.iter().find(|m| rest.starts_with(**m)) {
            out.push(Tok::Punc((*m).to_string()));
            i += m.chars().count();
            continue;
        }
        out.push(Tok::Punc(c.to_string()));
        i += 1;
    }
    out.push(Tok::End);
    Ok(out)
}

// ---------------------------------------------------------------- parser

#[derive(Clone, Debug)]
pub enum Expr {
    Num(String),
    Sym(String),
    Str(String),
    Vec(Vec<Expr>),
    Neg(Box<Expr>),
    Not(Box<Expr>),
    Bin(&'static str, Box<Expr>, Box<Expr>),
    Call(String, Vec<Expr>),
}

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
    /// Current recursion depth of `expr`.
    depth: usize,
}

const COMPARISONS: &[&str] = &["<", ">", "<=", ">=", "==", "=", "!="];

/// Calc's binary operators in the subset: (token, left precedence, right
/// precedence), with multiplication taking precedence over division
/// (`calc-multiplication-has-precedence`, on by default).
fn binary_op(t: &Tok) -> Option<(&'static str, i32, i32)> {
    let Tok::Punc(p) = t else { return None };
    Some(match p.as_str() {
        "^" | "**" => ("^", 201, 200),
        "*" => ("*", 196, 195),
        "/" => ("/", 190, 191),
        "+" => ("+", 180, 181),
        "-" => ("-", 180, 181),
        "<" => ("<", 160, 161),
        ">" => (">", 160, 161),
        "<=" => ("<=", 160, 161),
        ">=" => (">=", 160, 161),
        "=" | "==" => ("==", 160, 161),
        "!=" => ("!=", 160, 161),
        "&&" => ("&&", 110, 111),
        "||" => ("||", 100, 101),
        _ => return None,
    })
}

/// Punctuation that is a real Calc operator outside our subset.
fn is_foreign_operator(p: &str) -> bool {
    matches!(
        p,
        "%" | "\\"
            | "|"
            | "_"
            | "!"
            | "!!"
            | "?"
            | ":="
            | "::"
            | "=>"
            | "&&&"
            | "|||"
            | "!!!"
            | "<>"
            | "~="
            | "/="
            | "<<"
            | ">>"
            | "+/-"
            | ":"
            | ";"
            | "@"
            | "'"
            | "{"
            | "}"
    )
}

impl Parser {
    fn peek(&self) -> &Tok {
        &self.toks[self.pos.min(self.toks.len() - 1)]
    }
    fn next(&mut self) -> Tok {
        let t = self.peek().clone();
        if self.pos < self.toks.len() - 1 {
            self.pos += 1;
        }
        t
    }
    /// Whether the bracket just opened has a comma at its own level
    /// (`math-check-for-commas`).
    fn vector_has_comma(&self) -> bool {
        let mut depth = 0i32;
        for t in &self.toks[self.pos..] {
            match t {
                Tok::Punc(p) if matches!(p.as_str(), "[" | "(" | "{") => depth += 1,
                Tok::Punc(p) if matches!(p.as_str(), "]" | ")" | "}") => {
                    depth -= 1;
                    if depth < 0 {
                        return false;
                    }
                }
                Tok::Punc(p) if p == "," && depth == 0 => return true,
                Tok::End => return false,
                _ => {}
            }
        }
        false
    }
    fn is_punc(&self, p: &str) -> bool {
        matches!(self.peek(), Tok::Punc(q) if q == p)
    }

    /// An expression and the depth of its syntax tree. Both the recursion
    /// here and the tree (left-nested operator chains grow it without
    /// recursion) are capped at [`MAX_DEPTH`], since evaluating and dropping
    /// the tree recurse over it.
    fn expr(&mut self, min_prec: i32) -> CResult<Expr> {
        self.expr_d(min_prec).map(|(e, _)| e)
    }

    fn expr_d(&mut self, min_prec: i32) -> CResult<(Expr, usize)> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return unsupported("a formula nested too deeply");
        }
        let r = self.expr_inner(min_prec);
        self.depth -= 1;
        r
    }

    fn expr_inner(&mut self, min_prec: i32) -> CResult<(Expr, usize)> {
        let (mut x, mut xd) = self.factor()?;
        let mut first = true;
        loop {
            let t = self.peek().clone();
            let (op, lp, rp, implicit) = if let Some((op, lp, rp)) = binary_op(&t) {
                (op, lp, rp, false)
            } else if matches!(t, Tok::Num(_) | Tok::Sym(_))
                || matches!(&t, Tok::Punc(p) if p == "(" || p == "[")
            {
                ("*", 196, 195, true)
            } else {
                if let Tok::Punc(p) = &t {
                    if is_foreign_operator(p) {
                        return unsupported(format!("Calc operator `{p}`"));
                    }
                }
                break;
            };
            if lp < min_prec {
                break;
            }
            if !implicit {
                self.next();
            }
            if COMPARISONS.contains(&op) && !first {
                if let Expr::Bin(prev, _, _) = &x {
                    if COMPARISONS.contains(prev) {
                        return unsupported("chained comparison");
                    }
                }
            }
            let (rhs, rd) = self.expr_d(rp)?;
            xd = xd.max(rd) + 1;
            if xd > MAX_DEPTH {
                return unsupported("a formula nested too deeply");
            }
            x = Expr::Bin(op, Box::new(x), Box::new(rhs));
            first = false;
        }
        Ok((x, xd))
    }

    /// Arguments up to `close`, and the deepest argument's tree depth.
    fn args(&mut self, close: &str) -> CResult<(Vec<Expr>, usize)> {
        let mut out = Vec::new();
        let mut d = 0;
        if self.is_punc(close) {
            self.next();
            return Ok((out, d));
        }
        loop {
            let (e, ed) = self.expr_d(0)?;
            d = d.max(ed);
            out.push(e);
            match self.next() {
                Tok::Punc(p) if p == "," => continue,
                Tok::Punc(p) if p == close => return Ok((out, d)),
                // Calc accepts the end of the formula in place of the closing
                // bracket (`math-read-factor`, `math-read-brackets`).
                Tok::End => return Ok((out, d)),
                Tok::Punc(p) if p == ";" || p == ".." || p == "." => {
                    return unsupported("matrix or interval syntax");
                }
                other => {
                    return unsupported(format!("unexpected {other:?} in an argument list"));
                }
            }
        }
    }

    fn factor(&mut self) -> CResult<(Expr, usize)> {
        match self.next() {
            Tok::Num(n) => Ok((Expr::Num(n), 1)),
            Tok::Str(s) => Ok((Expr::Str(s), 1)),
            Tok::Sym(name) => {
                if self.is_punc("(") {
                    self.next();
                    let (args, d) = self.args(")")?;
                    Ok((Expr::Call(name, args), d + 1))
                } else {
                    Ok((Expr::Sym(name), 1))
                }
            }
            Tok::Punc(p) => match p.as_str() {
                "-" => {
                    let (e, d) = self.expr_d(197)?;
                    Ok((Expr::Neg(Box::new(e)), d + 1))
                }
                "+" => self.expr_d(197),
                "!" => {
                    let (e, d) = self.expr_d(1000)?;
                    Ok((Expr::Not(Box::new(e)), d + 1))
                }
                "(" => {
                    let inner = self.expr_d(0)?;
                    match self.next() {
                        Tok::Punc(q) if q == ")" => Ok(inner),
                        Tok::End => Ok(inner),
                        Tok::Punc(q) if q == "," || q == ".." || q == ";" => {
                            unsupported("complex number or interval syntax")
                        }
                        other => unsupported(format!("unexpected {other:?} after an expression")),
                    }
                }
                "[" => {
                    // A bracket with no top-level comma is read by Calc with
                    // spaces as separators (`[2 3]` is a two-element vector,
                    // `[2 3, 4]` is `[6, 4]`). The tokens do not keep spaces,
                    // so only a single plain element is accepted there.
                    let comma = self.vector_has_comma();
                    let (items, d) = self.args("]")?;
                    let plain = |e: &Expr| match e {
                        Expr::Num(_) | Expr::Str(_) | Expr::Sym(_) => true,
                        Expr::Neg(inner) => matches!(**inner, Expr::Num(_)),
                        _ => false,
                    };
                    if !comma && !items.iter().all(plain) {
                        return unsupported(
                            "a vector without commas (Calc separates its elements by spaces)",
                        );
                    }
                    Ok((Expr::Vec(items), d + 1))
                }
                ")" | "]" | "," => calc_error(format!("expected a number before `{p}`")),
                _ => unsupported(format!("Calc syntax `{p}`")),
            },
            Tok::End => calc_error("expected a number"),
        }
    }
}

pub fn parse(src: &str) -> CResult<Expr> {
    let toks = tokenize(src)?;
    let mut p = Parser {
        toks,
        pos: 0,
        depth: 0,
    };
    let e = p.expr(0)?;
    match p.peek() {
        Tok::End => Ok(e),
        Tok::Punc(q) if q == ")" || q == "]" => calc_error("mismatched delimiters"),
        other => unsupported(format!("unexpected {other:?}")),
    }
}

// ---------------------------------------------------------------- evaluation

fn neg_value(v: &Value) -> CResult<Value> {
    match v {
        Value::Num(n) => Ok(Value::Num(neg(n))),
        Value::Nan => Ok(Value::Nan),
        Value::Inf(s) => Ok(Value::Inf(-s)),
        Value::Sym(s) => unsupported(format!("symbolic value `{s}`")),
        _ => unsupported("negating a non-number"),
    }
}

/// Addition with Calc's `nan` and infinities: `nan` absorbs everything, an
/// infinity absorbs finite numbers, and `inf + inf = inf`. `inf - inf` and
/// sums with `uinf` on both sides are left out (Calc gives `nan`; not needed).
fn add_values(a: &Value, b: &Value, prec: i64) -> CResult<Value> {
    match (a, b) {
        (Value::Num(x), Value::Num(y)) => Ok(Value::Num(add(x, y, prec)?)),
        (Value::Nan, Value::Num(_) | Value::Nan | Value::Inf(_))
        | (Value::Num(_) | Value::Inf(_), Value::Nan) => Ok(Value::Nan),
        (Value::Inf(s), Value::Num(_)) | (Value::Num(_), Value::Inf(s)) => Ok(Value::Inf(*s)),
        (Value::Inf(s), Value::Inf(t)) if s == t && *s != 0 => Ok(Value::Inf(*s)),
        (Value::Sym(s), _) | (_, Value::Sym(s)) => unsupported(format!(
            "symbolic value `{s}` (Calc would keep the sum as algebra)"
        )),
        _ => unsupported("adding these values"),
    }
}

pub struct Eval {
    pub modes: Modes,
    /// Work done so far, in ticks ([`TICKS_PER_UNIT`] make one unit of the
    /// formula engine's per-command budget).
    ticks: std::cell::Cell<u64>,
    /// Ticks this evaluation may use before it is refused.
    max_ticks: u64,
}

/// Calc work is counted in ticks: [`NODE_TICKS`] per syntax node, and for
/// arithmetic the operands' sizes in 64-bit limbs, added for `+`, `-` and
/// comparisons and multiplied for `*` and `/` (schoolbook cost). A power is
/// charged as the square of its result's size, reading a literal and
/// printing the result as four times the square of their size (decimal
/// conversion is quadratic). [`TICKS_PER_UNIT`] ticks are one unit of the
/// formula engine's budget (about 2.5 us in the release guest); measured
/// there (2026-10-20), one unit's time does about 1,000 ticks of this work,
/// so it is charged at about twice its time.
pub const TICKS_PER_UNIT: u64 = 512;
const NODE_TICKS: u64 = 64;

fn limbs_num(n: &Num) -> u64 {
    match n {
        Num::Int(i) => i.bits() / 64 + 1,
        Num::Float { m, .. } => m.bits() / 64 + 1,
    }
}

fn limbs(v: &Value) -> u64 {
    match v {
        Value::Num(n) => limbs_num(n),
        Value::Vec(items) => items.iter().map(limbs).fold(1u64, u64::saturating_add),
        Value::Str(s) => s.len() as u64 / 8 + 1,
        _ => 1,
    }
}

fn truth(v: &Value) -> CResult<bool> {
    match v {
        Value::Num(n) => Ok(!is_zero(n)),
        _ => unsupported("a condition that is not a number"),
    }
}

fn flatten(v: &Value, out: &mut Vec<Value>) {
    match v {
        Value::Vec(items) => items.iter().for_each(|i| flatten(i, out)),
        Value::Str(s) => s
            .chars()
            .for_each(|c| out.push(Value::Num(Num::Int(BigInt::from(c as u32))))),
        other => out.push(other.clone()),
    }
}

impl Eval {
    fn prec(&self) -> i64 {
        self.modes.prec
    }

    /// Charge `ticks` of work; refuse once over the budget.
    fn charge(&self, ticks: u64) -> CResult<()> {
        let t = self.ticks.get().saturating_add(ticks);
        self.ticks.set(t);
        if t > self.max_ticks {
            return unsupported(WORK_MSG);
        }
        Ok(())
    }

    pub fn eval(&self, e: &Expr) -> CResult<Value> {
        self.charge(NODE_TICKS)?;
        match e {
            Expr::Num(s) => {
                // Reading decimal digits is quadratic in their number.
                let l = s.len() as u64 / 19 + 1;
                self.charge(l.saturating_mul(l).saturating_mul(4))?;
                Ok(Value::Num(read_number(s, self.prec())?))
            }
            Expr::Str(s) => Ok(Value::Str(s.clone())),
            Expr::Sym(name) => {
                match name.as_str() {
                    "nan" => return Ok(Value::Nan),
                    "inf" => return Ok(Value::Inf(1)),
                    "uinf" => return Ok(Value::Inf(0)),
                    _ => {}
                }
                if SPECIAL_SYMBOLS.contains(&name.as_str()) {
                    return unsupported(format!("Calc constant `{name}`"));
                }
                if name.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
                    // Calc keeps special variables (EvalRules, Holidays...) under
                    // capitalised names; be conservative.
                    return unsupported(format!("Calc variable `{name}`"));
                }
                Ok(Value::Sym(name.clone()))
            }
            Expr::Vec(items) => Ok(Value::Vec(
                items.iter().map(|i| self.eval(i)).collect::<CResult<_>>()?,
            )),
            Expr::Neg(x) => neg_value(&self.eval(x)?),
            Expr::Not(x) => match self.eval(x)? {
                Value::Num(n) => Ok(Value::Num(Num::Int(if is_zero(&n) {
                    BigInt::one()
                } else {
                    BigInt::zero()
                }))),
                _ => unsupported("`!` of a non-number"),
            },
            Expr::Bin(op, a, b) => self.binary(op, a, b),
            Expr::Call(name, args) => self.call(name, args),
        }
    }

    fn num_pair(&self, a: Value, b: Value, op: &str) -> CResult<(Num, Num)> {
        match (a, b) {
            (Value::Num(x), Value::Num(y)) => Ok((x, y)),
            (Value::Sym(s), _) | (_, Value::Sym(s)) => unsupported(format!(
                "symbolic value `{s}` (Calc would keep `{op}` as algebra)"
            )),
            _ => unsupported(format!("`{op}` on a non-number")),
        }
    }

    fn binary(&self, op: &str, a: &Expr, b: &Expr) -> CResult<Value> {
        let prec = self.prec();
        if op == "&&" || op == "||" {
            let x = self.eval(a)?;
            let y = self.eval(b)?;
            let (Value::Num(xn), Value::Num(yn)) = (&x, &y) else {
                return unsupported(format!("`{op}` on a non-number"));
            };
            return Ok(Value::Num(if op == "&&" {
                if is_zero(xn) {
                    xn.clone()
                } else {
                    yn.clone()
                }
            } else if is_zero(xn) {
                yn.clone()
            } else {
                xn.clone()
            }));
        }
        let x = self.eval(a)?;
        let y = self.eval(b)?;
        let (lx, ly) = (limbs(&x), limbs(&y));
        let cost = if op == "^" {
            // The result has about bits(x) * y bits (pow_int refuses big
            // ones before computing them).
            let (bits, e) = match (&x, &y) {
                (Value::Num(Num::Int(b)), Value::Num(Num::Int(n))) => {
                    (b.bits(), n.to_u64().unwrap_or(u64::MAX))
                }
                _ => (64, 1),
            };
            let r = (bits.saturating_mul(e) / 64 + 1).min(1 << 20);
            r.saturating_mul(r)
        } else if op == "*" || op == "/" {
            lx.saturating_mul(ly).saturating_add(lx).saturating_add(ly)
        } else {
            lx.saturating_add(ly)
        };
        self.charge(cost)?;
        if COMPARISONS.contains(&op) {
            let bool_val =
                |t: bool| Value::Num(Num::Int(if t { BigInt::one() } else { BigInt::zero() }));
            if let (Value::Str(s), Value::Str(t)) = (&x, &y) {
                return match op {
                    "==" => Ok(bool_val(s == t)),
                    "!=" => Ok(bool_val(s != t)),
                    _ => unsupported("ordering comparison of strings"),
                };
            }
            let (xn, yn) = self.num_pair(x, y, op)?;
            let ord = compare(&xn, &yn);
            return Ok(bool_val(match op {
                "<" => ord == Ordering::Less,
                ">" => ord == Ordering::Greater,
                "<=" => ord != Ordering::Greater,
                ">=" => ord != Ordering::Less,
                "==" => ord == Ordering::Equal,
                _ => ord != Ordering::Equal,
            }));
        }
        if op == "+" {
            return add_values(&x, &y, prec);
        }
        if op == "-" {
            return add_values(&x, &neg_value(&y)?, prec);
        }
        if matches!(x, Value::Nan | Value::Inf(_)) || matches!(y, Value::Nan | Value::Inf(_)) {
            return unsupported(format!("`{op}` with nan or an infinity"));
        }
        let (xn, yn) = self.num_pair(x, y, op)?;
        Ok(Value::Num(match op {
            "+" => add(&xn, &yn, prec)?,
            "-" => sub(&xn, &yn, prec)?,
            "*" => mul(&xn, &yn, prec)?,
            "/" => div(&xn, &yn, prec)?,
            "^" => match (&xn, &yn) {
                (Num::Int(b), Num::Int(n)) if !n.is_negative() && !(b.is_zero() && n.is_zero()) => {
                    Num::Int(pow_int(b, n)?)
                }
                _ => {
                    return unsupported("`^` other than an integer to a non-negative integer power")
                }
            },
            _ => return unsupported(format!("operator `{op}`")),
        }))
    }

    fn reduce(&self, args: &[Value], add_not_mul: bool) -> CResult<Value> {
        let inner = self.prec() + 2;
        let mut items = Vec::new();
        args.iter().for_each(|a| flatten(a, &mut items));
        let mut acc: Option<Value> = None;
        for it in items {
            let la = acc.as_ref().map_or(1, limbs);
            let lb = limbs(&it);
            let op_ticks = if add_not_mul {
                la.saturating_add(lb)
            } else {
                la.saturating_mul(lb)
            };
            self.charge(op_ticks.saturating_add(NODE_TICKS))?;
            acc = Some(match (acc, it) {
                (None, v @ (Value::Num(_) | Value::Nan | Value::Inf(_))) => v,
                (Some(a), b) if add_not_mul => add_values(&a, &b, inner)?,
                (Some(Value::Num(a)), Value::Num(b)) => Value::Num(if add_not_mul {
                    add(&a, &b, inner)?
                } else {
                    mul(&a, &b, inner)?
                }),
                (_, Value::Sym(s)) => {
                    return unsupported(format!("symbolic value `{s}` in a vector"))
                }
                _ => return unsupported("a vector element that is not a number"),
            });
        }
        match acc {
            None => Ok(Value::Num(Num::Int(if add_not_mul {
                BigInt::zero()
            } else {
                BigInt::one()
            }))),
            Some(Value::Num(n)) => Ok(Value::Num(renormalize(n, self.prec())?)),
            Some(v) => Ok(v),
        }
    }

    fn call(&self, name: &str, args: &[Expr]) -> CResult<Value> {
        if name == "if" {
            if args.len() != 3 {
                return unsupported("`if` without exactly three arguments");
            }
            let c = self.eval(&args[0])?;
            return if truth(&c)? {
                self.eval(&args[1])
            } else {
                self.eval(&args[2])
            };
        }
        let vals = args
            .iter()
            .map(|a| self.eval(a))
            .collect::<CResult<Vec<_>>>()?;
        match name {
            "vsum" => self.reduce(&vals, true),
            "vprod" => self.reduce(&vals, false),
            "vmean" => {
                let mut items = Vec::new();
                vals.iter().for_each(|a| flatten(a, &mut items));
                if items.is_empty() {
                    return unsupported("`vmean` of an empty vector");
                }
                let inner = self.prec() + 2;
                let mut sum: Option<Num> = None;
                for it in &items {
                    let Value::Num(n) = it else {
                        return unsupported("`vmean` of a vector with a non-number");
                    };
                    sum = Some(match sum {
                        None => n.clone(),
                        Some(s) => add(&s, n, inner)?,
                    });
                }
                let Some(sum) = sum else {
                    return unsupported("`vmean` of an empty vector");
                };
                let q = div(&sum, &Num::Int(BigInt::from(items.len())), inner)?;
                Ok(Value::Num(renormalize(q, self.prec())?))
            }
            "vmax" | "vmin" => {
                let mut items = Vec::new();
                vals.iter().for_each(|a| flatten(a, &mut items));
                if items.is_empty() {
                    return unsupported(format!(
                        "`{name}` of an empty vector (Calc gives an infinity)"
                    ));
                }
                let mut best: Option<Num> = None;
                for it in &items {
                    let Value::Num(n) = it else {
                        return unsupported(format!("`{name}` of a vector with a non-number"));
                    };
                    best = Some(match best {
                        None => n.clone(),
                        Some(b) => {
                            let ord = compare(n, &b);
                            let better = if name == "vmax" {
                                ord == Ordering::Greater
                            } else {
                                ord == Ordering::Less
                            };
                            if better {
                                n.clone()
                            } else {
                                b
                            }
                        }
                    });
                }
                match best {
                    Some(b) => Ok(Value::Num(renormalize(b, self.prec())?)),
                    None => unsupported("empty vector"),
                }
            }
            "vcount" => {
                let mut items = Vec::new();
                for v in &vals {
                    if let Value::Sym(s) = v {
                        return unsupported(format!("symbolic value `{s}` in `vcount`"));
                    }
                    flatten(v, &mut items);
                }
                Ok(Value::Num(Num::Int(BigInt::from(items.len()))))
            }
            "vlen" => {
                if vals.len() != 1 {
                    return unsupported("`vlen` without exactly one argument");
                }
                match &vals[0] {
                    Value::Vec(items) => Ok(Value::Num(Num::Int(BigInt::from(items.len())))),
                    Value::Str(s) => Ok(Value::Num(Num::Int(BigInt::from(s.chars().count())))),
                    Value::Num(_) | Value::Nan | Value::Inf(_) => {
                        Ok(Value::Num(Num::Int(BigInt::zero())))
                    }
                    _ => unsupported("`vlen` of a symbolic value"),
                }
            }
            "string" => match vals.as_slice() {
                [Value::Str(s)] => Ok(Value::Text(s.clone())),
                _ => unsupported("`string` of something other than a string literal"),
            },
            "sum" | "mean" => unsupported(format!(
                "`{name}` is Advanced Tables syntax; Org (Calc) uses `v{name}`"
            )),
            other => unsupported(format!("Calc function `{other}`")),
        }
    }
}

/// Evaluate a Calc expression string under `modes`. With `num_only`
/// (`calc-eval ... 'num`), a non-numeric result is a Calc error.
pub fn calc_eval(src: &str, modes: Modes, num_only: bool) -> CResult<String> {
    calc_eval_budget(src, modes, num_only, u64::MAX / TICKS_PER_UNIT).0
}

/// The reason given when an evaluation runs out of its work budget.
pub const WORK_MSG: &str = "the formulas need more computation than the plugin allows in one run";

/// [`calc_eval`] with a budget of `max_units` engine units; also returns the
/// units used (rounded up), whether or not the evaluation finished.
pub fn calc_eval_budget(
    src: &str,
    modes: Modes,
    num_only: bool,
    max_units: u64,
) -> (CResult<String>, u64) {
    let ev = Eval {
        modes,
        ticks: std::cell::Cell::new(0),
        max_ticks: max_units.saturating_mul(TICKS_PER_UNIT),
    };
    let r = calc_run(&ev, src, num_only);
    (r, ev.ticks.get().div_ceil(TICKS_PER_UNIT))
}

fn calc_run(ev: &Eval, src: &str, num_only: bool) -> CResult<String> {
    let modes = ev.modes;
    let expr = parse(src)?;
    let v = ev.eval(&expr)?;
    // Printing a big integer in decimal is quadratic in its size.
    let l = limbs(&v);
    ev.charge(l.saturating_mul(l).saturating_mul(4))?;
    if num_only && !matches!(v, Value::Num(_)) {
        return match v {
            Value::Sym(_) | Value::Nan | Value::Inf(_) => calc_error("number expected"),
            _ => unsupported("a non-numeric result with the N flag"),
        };
    }
    display(&v, &modes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(s: &str) -> String {
        match calc_eval(s, Modes::default(), false) {
            Ok(v) => v,
            Err(Outcome::CalcError(e)) => format!("#ERROR({e})"),
            Err(Outcome::Unsupported(e)) => format!("UNSUPPORTED({e})"),
        }
    }

    #[test]
    fn integers_stay_exact_and_floats_show_like_calc() {
        assert_eq!(ev("(2)*(3)"), "6");
        assert_eq!(ev("(7)/(2)"), "3.5");
        assert_eq!(ev("(6)/(3)"), "2");
        assert_eq!(ev("(1)/(3)"), "0.33333333");
        assert_eq!(ev("(2)/(3)"), "0.66666667");
        assert_eq!(ev("(1.5)*(2)"), "3.");
        assert_eq!(ev("(3.0)"), "3.");
        assert_eq!(ev("(16.0)"), "16.");
        assert_eq!(ev("4.1+8.2"), "12.3");
        assert_eq!(ev("0.1+0.2"), "0.3");
        assert_eq!(ev("1e3"), "1000.");
        assert_eq!(ev("0.001234"), "1.234e-3");
        assert_eq!(ev("0.01234"), "0.01234");
        assert_eq!(ev("123456789012345."), "1.2345679e14");
        assert_eq!(ev("1.5-1.5"), "0.");
        assert_eq!(ev("-(2.5)"), "-2.5");
        assert_eq!(ev("2^10"), "1024");
    }

    #[test]
    fn multiplication_binds_tighter_than_division() {
        assert_eq!(ev("8/2*2"), "2");
        assert_eq!(ev("8/2/2"), "2");
        assert_eq!(ev("2*8/4"), "4");
        assert_eq!(ev("(3) (4)"), "12");
    }

    #[test]
    fn vector_functions() {
        assert_eq!(ev("vsum([2,4,8])"), "14");
        assert_eq!(ev("vsum([])"), "0");
        assert_eq!(ev("vmean([1,2])"), "1.5");
        assert_eq!(ev("vmean([2,4])"), "3");
        assert_eq!(ev("vmax([1,5.5,3])"), "5.5");
        assert_eq!(ev("vmin([1,5.5,3])"), "1");
        assert_eq!(ev("vcount([1,5.5,3])"), "3");
        assert_eq!(ev("vlen([1,5.5,3])"), "3");
        assert_eq!(ev("vsum([4.1,8.2,16.0,32,64])"), "124.3");
    }

    #[test]
    fn fix_format_follows_calc() {
        let m = Modes {
            kind: FloatKind::Fix,
            figs: -1,
            ..Modes::default()
        };
        assert_eq!(calc_eval("96+.0", m, false).unwrap(), "96.0");
        assert_eq!(calc_eval("4.1+8.2", m, false).unwrap(), "12.3");
        let m2 = Modes {
            kind: FloatKind::Fix,
            figs: 2,
            ..Modes::default()
        };
        assert_eq!(calc_eval("(1)/(3)", m2, false).unwrap(), "0.33");
        assert_eq!(calc_eval("(2)*(3)", m2, false).unwrap(), "6");
        assert_eq!(calc_eval("0.0001", m2, false).unwrap(), "1e-4");
    }

    #[test]
    fn symbols_strings_and_errors() {
        assert_eq!(ev("(z)"), "z");
        assert!(ev("(z)+1").starts_with("UNSUPPORTED"));
        assert_eq!(ev("if(\"(0)\" == \"nan\", string(\"\"), (0)+1)"), "1");
        assert_eq!(ev("if(\"nan\" == \"nan\", string(\"\"), (0)+1)"), "");
        assert_eq!(ev("nan + (1)"), "nan");
        assert!(ev("(3) +").starts_with("#ERROR"));
        // Calc reads the end of the formula as the missing closing brackets
        // (Emacs oracle case emacs-calc-syntax2).
        assert_eq!(ev("((3)"), "3");
        assert_eq!(ev("vsum([1,(2"), "3");
        assert!(ev("((3)))").starts_with("#ERROR"));
        // Comma-free brackets separate by spaces in Calc: `[2 3]` is two elements.
        assert!(ev("vsum([2 3])").starts_with("UNSUPPORTED"));
        assert_eq!(ev("vsum([2 3,4])"), "10");
        assert_eq!(ev("vsum([-2])"), "-2");
        assert!(ev("(1)/(0)").starts_with("UNSUPPORTED"));
        assert!(ev("sum([1,2])").contains("vsum"));
        assert!(ev("(pi)").starts_with("UNSUPPORTED"));
        assert_eq!(ev("(-inf)+(1)"), "-inf");
        assert_eq!(ev("vsum([uinf,1])"), "uinf");
        assert_eq!(ev("vsum([nan,inf])"), "nan");
        assert!(ev("(inf)-(inf)").starts_with("UNSUPPORTED"));
    }
}
