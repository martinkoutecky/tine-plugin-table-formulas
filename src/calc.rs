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

use crate::work;
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
//
// Every step that does more than constant work charges the command's work
// meter first ([`crate::work`]), by the sizes of its operands in limbs.

/// Charge `t` ticks of work; refuse once the command's budget is spent.
fn w(t: u64) -> CResult<()> {
    if work::ticks(t) {
        Ok(())
    } else {
        unsupported(WORK_MSG)
    }
}

fn l(a: &BigInt) -> u64 {
    work::limbs(a)
}

/// 10^n, charged before it is built.
fn pow10(n: i64) -> CResult<BigInt> {
    let n = n.max(0);
    let r = work::digit_limbs(n as usize);
    w(work::mul_ticks(r, r))?;
    Ok(num_traits::pow::pow(BigInt::from(10), n as usize))
}

pub fn numdigs(a: &BigInt) -> CResult<i64> {
    if a.is_zero() {
        return Ok(0);
    }
    if let Some(v) = a.magnitude().to_u128() {
        return Ok(v.ilog10() as i64 + 1);
    }
    w(work::decimal_ticks(l(a)))?;
    Ok(a.magnitude().to_string().len() as i64)
}

/// Multiply `a` by 10^n, rounding half away from zero when n < 0.
fn scale_rounding(a: &BigInt, n: i64) -> CResult<BigInt> {
    if n >= 0 {
        let p = pow10(n)?;
        w(work::mul_ticks(l(a), l(&p)))?;
        return Ok(a * p);
    }
    if a.is_negative() {
        w(work::add_ticks(l(a), 0))?;
        return Ok(-scale_rounding(&-a, n)?);
    }
    let shifted = if n == -1 {
        w(work::add_ticks(l(a), 0))?;
        a.clone()
    } else {
        let p = pow10(-1 - n)?;
        w(work::mul_ticks(l(a), l(&p)))?;
        a / p
    };
    w(2 * work::add_ticks(l(&shifted), 1))?;
    Ok((shifted + BigInt::from(5)) / BigInt::from(10))
}

pub fn make_float(m: BigInt, e: i64, prec: i64) -> CResult<Num> {
    if m.is_zero() {
        return Ok(Num::Float { m, e: 0 });
    }
    let mut m = m;
    let mut e = e;
    let ldiff = prec - numdigs(&m)?;
    if ldiff < 0 {
        m = scale_rounding(&m, ldiff)?;
        e -= ldiff;
    }
    let ten = BigInt::from(10);
    loop {
        // A remainder and a division by 10 per trailing zero.
        w(2 * work::add_ticks(l(&m), 1))?;
        if m.is_zero() || !(&m % &ten).is_zero() {
            break;
        }
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
        Num::Int(i) => {
            w(work::add_ticks(l(i), 0))?;
            match make_float(i.clone(), 0, prec)? {
                Num::Float { m, e } => Ok((m, e)),
                Num::Int(_) => unreachable_num(),
            }
        }
        Num::Float { m, e } => {
            w(work::add_ticks(l(m), 0))?;
            Ok((m.clone(), *e))
        }
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

fn mantissa(a: &Num) -> &BigInt {
    match a {
        Num::Int(i) => i,
        Num::Float { m, .. } => m,
    }
}

/// A copy of `a`, charged.
fn copy(a: &Num) -> CResult<Num> {
    w(work::add_ticks(l(mantissa(a)), 0))?;
    Ok(a.clone())
}

pub fn add(a: &Num, b: &Num, prec: i64) -> CResult<Num> {
    if let (Num::Int(x), Num::Int(y)) = (a, b) {
        w(work::add_ticks(l(x), l(y)))?;
        return Ok(Num::Int(int_budget(x + y)?));
    }
    if is_zero(a) {
        return if is_float(a) && !is_float(b) {
            float_of(b, prec)
        } else {
            copy(b)
        };
    }
    if is_zero(b) {
        return if is_float(b) && !is_float(a) {
            float_of(a, prec)
        } else {
            copy(a)
        };
    }
    let (ma, ea) = to_float(a, prec)?;
    let (mb, eb) = to_float(b, prec)?;
    let ediff = ea - eb;
    let (hi, ehi, lo, elo, ediff) = if ediff >= 0 {
        (ma, ea, mb, eb, ediff)
    } else {
        (mb, eb, ma, ea, -ediff)
    };
    if ediff >= prec + prec {
        return Ok(Num::Float { m: hi, e: ehi });
    }
    let p = pow10(ediff)?;
    w(work::mul_ticks(l(&hi), l(&p)) + work::add_ticks(l(&hi) + l(&p), l(&lo)))?;
    make_float(lo + hi * p, elo, prec)
}

/// `-a`. Its copy is charged without a check (the next charge refuses).
pub fn neg(a: &Num) -> Num {
    let _ = work::ticks(work::add_ticks(l(mantissa(a)), 0));
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
        w(work::mul_ticks(l(x), l(y)))?;
        return Ok(Num::Int(int_budget(x * y)?));
    }
    if is_zero(a) {
        return if is_float(b) && !is_float(a) {
            float_of(a, prec)
        } else {
            copy(a)
        };
    }
    if is_zero(b) {
        return if is_float(a) && !is_float(b) {
            float_of(b, prec)
        } else {
            copy(b)
        };
    }
    let (ma, ea) = to_float(a, prec)?;
    let (mb, eb) = to_float(b, prec)?;
    w(work::mul_ticks(l(&ma), l(&mb)))?;
    make_float(ma * mb, ea + eb, prec)
}

fn div_float(ma: BigInt, ea: i64, mb: BigInt, eb: i64, prec: i64) -> CResult<Num> {
    let ldiff = (prec + 1 - (numdigs(&ma)? - numdigs(&mb)?)).max(0);
    let p = pow10(ldiff)?;
    w(work::mul_ticks(l(&ma), l(&p)))?;
    let n = ma * p;
    w(work::mul_ticks(l(&n), l(&mb)))?;
    let q = n / mb;
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
            copy(a)
        };
    }
    if let (Num::Int(x), Num::Int(y)) = (a, b) {
        // A quotient and a remainder.
        w(2 * work::mul_ticks(l(x), l(y)))?;
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

/// Exact comparison. Signs decide first, then the position of the leading
/// digit; only numbers of the same magnitude are scaled to a common exponent,
/// so `1e9999` against `1e-9999` builds no 20,000-digit power of ten (registry
/// audit, 2026-10-20).
pub fn compare(a: &Num, b: &Num) -> CResult<Ordering> {
    let (ma, ea) = match a {
        Num::Int(i) => (i, 0),
        Num::Float { m, e } => (m, *e),
    };
    let (mb, eb) = match b {
        Num::Int(i) => (i, 0),
        Num::Float { m, e } => (m, *e),
    };
    let (sa, sb) = (ma.sign(), mb.sign());
    if sa != sb || sa == num_bigint::Sign::NoSign {
        // Minus < NoSign < Plus.
        return Ok(sa.cmp(&sb));
    }
    // Same sign, both non-zero: |x| = |m| 10^e with d digits lies in
    // [10^(d-1+e), 10^(d+e)).
    let (da, db) = (numdigs(ma)? + ea, numdigs(mb)? + eb);
    if da != db {
        let ord = da.cmp(&db);
        return Ok(if sa == num_bigint::Sign::Minus {
            ord.reverse()
        } else {
            ord
        });
    }
    let e = ea.min(eb);
    let (pa, pb) = (pow10(ea - e)?, pow10(eb - e)?);
    w(work::mul_ticks(l(ma), l(&pa)) + work::mul_ticks(l(mb), l(&pb)))?;
    let (xa, xb) = (ma * pa, mb * pb);
    w(work::add_ticks(l(&xa), l(&xb)))?;
    Ok(xa.cmp(&xb))
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
    // Squarings up to the result's size, plus one step per exponent bit.
    let r = base.bits().saturating_mul(n as u64) / 64 + 1;
    w(2 * work::mul_ticks(r, r) + 64 * (32 - n.leading_zeros() as u64))?;
    int_budget(num_traits::pow::pow(base.clone(), n as usize))
}

// ---------------------------------------------------------------- reading

/// Read a Calc number token (digits, `.`, optional exponent).
pub fn read_number(s: &str, prec: i64) -> CResult<Num> {
    if s.len() > MAX_LITERAL_DIGITS {
        return unsupported("a number with more than 2000 digits");
    }
    w(work::text_ticks(s.len()) + work::decimal_ticks(work::digit_limbs(s.len())))?;
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

/// `"0".repeat(n)`, charged.
fn zeros(n: i64) -> CResult<String> {
    let n = n.max(0) as usize;
    w(work::text_ticks(n))?;
    Ok("0".repeat(n))
}

/// Decimal text of `m`, charged.
fn decimal(m: &BigInt) -> CResult<String> {
    w(work::decimal_ticks(l(m)))?;
    Ok(m.to_string())
}

fn format_float_pos(m: &BigInt, e: i64, modes: &Modes) -> CResult<String> {
    let mut figs = modes.figs;
    let kind = modes.kind;
    if kind == FloatKind::Fix && (figs < 0 || e + numdigs(m)? > -figs) {
        if figs < 0 {
            figs = -figs;
        }
        let mant = scale_rounding(m, e + figs)?;
        let mut s = decimal(&mant)?;
        if (s.len() as i64) <= figs {
            s = format!("{}{}", zeros(figs - s.len() as i64 + 1)?, s);
        }
        if figs > 0 {
            let cut = s.len() - figs as usize;
            return Ok(format!("{}.{}", &s[..cut], &s[cut..]));
        }
        return Ok(format!("{s}."));
    }
    if figs < 0 {
        figs += modes.prec;
    }
    let mut m = m.clone();
    let mut e = e;
    if figs > 0 {
        let adj = figs - numdigs(&m)?;
        if adj < 0 {
            m = scale_rounding(&m, adj)?;
            e -= adj;
        }
    }
    let s = decimal(&m)?;
    let len = s.len() as i64;
    let dpos = e + len;
    if kind == FloatKind::Float && dpos <= modes.prec && dpos >= -1 {
        Ok(if dpos == 0 {
            format!("0.{s}")
        } else if e <= 0 && dpos > 0 {
            let d = dpos as usize;
            format!("{}.{}", &s[..d], &s[d..])
        } else if e > 0 {
            format!("{s}{}.", zeros(e)?)
        } else {
            format!("0.{}{s}", zeros(-dpos)?)
        })
    } else {
        let eadj = e + len;
        let scale: i64 = if kind == FloatKind::Eng {
            1 + (eadj + 300002).rem_euclid(3)
        } else {
            1
        };
        let mut s = s;
        if scale > s.len() as i64 {
            s.push_str(&zeros(scale - s.len() as i64)?);
        }
        if scale < s.len() as i64 {
            let k = scale as usize;
            s = format!("{}.{}", &s[..k], &s[k..]);
        }
        Ok(format!("{s}e{}", eadj - scale))
    }
}

pub fn format_num(a: &Num, modes: &Modes) -> CResult<String> {
    match a {
        Num::Int(i) => decimal(i),
        Num::Float { m, e } => {
            if m.is_negative() {
                w(work::add_ticks(l(m), 0))?;
                Ok(format!("-{}", format_float_pos(&-m, *e, modes)?))
            } else {
                format_float_pos(m, *e, modes)
            }
        }
    }
}

/// Display a final result the way `calc-eval` returns it.
pub fn display(v: &Value, modes: &Modes) -> CResult<String> {
    match v {
        Value::Num(n) => format_num(n, modes),
        Value::Nan => Ok("nan".to_string()),
        Value::Inf(1) => Ok("inf".to_string()),
        Value::Inf(-1) => Ok("-inf".to_string()),
        Value::Inf(_) => Ok("uinf".to_string()),
        Value::Sym(s) | Value::Text(s) => {
            w(work::text_ticks(s.len()))?;
            Ok(s.clone())
        }
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

/// Work for one token: its allocation, the punctuation table and the
/// parser's steps over it (its characters are charged with the whole text).
const TOKEN_TICKS: u64 = 96;

fn tokenize(src: &str) -> CResult<Vec<Tok>> {
    w(work::text_ticks(src.len()).saturating_mul(4))?;
    let s: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < s.len() {
        w(TOKEN_TICKS)?;
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
        let rest = &s[i..];
        let starts = |m: &str| {
            let mut k = 0;
            m.chars().all(|c| {
                k += 1;
                rest.get(k - 1) == Some(&c)
            })
        };
        if let Some(m) = MULTI.iter().find(|m| starts(m)) {
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
    /// Charged per token it looks at: brackets nested 200 deep each scan
    /// forward, so this is not covered by the per-token charge.
    fn vector_has_comma(&self) -> CResult<bool> {
        let mut depth = 0i32;
        let mut seen = 0u64;
        let mut found = false;
        for t in &self.toks[self.pos..] {
            seen += 1;
            match t {
                Tok::Punc(p) if matches!(p.as_str(), "[" | "(" | "{") => depth += 1,
                Tok::Punc(p) if matches!(p.as_str(), "]" | ")" | "}") => {
                    depth -= 1;
                    if depth < 0 {
                        break;
                    }
                }
                Tok::Punc(p) if p == "," && depth == 0 => {
                    found = true;
                    break;
                }
                Tok::End => break,
                _ => {}
            }
        }
        w(8 + 4 * seen)?;
        Ok(found)
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
        if work::exhausted() {
            return unsupported(WORK_MSG);
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
                    let comma = self.vector_has_comma()?;
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
}

/// Work for one syntax node or one vector element visited, besides its
/// arithmetic (charged by the number functions above).
const NODE_TICKS: u64 = 64;

fn truth(v: &Value) -> CResult<bool> {
    match v {
        Value::Num(n) => Ok(!is_zero(n)),
        _ => unsupported("a condition that is not a number"),
    }
}

/// The elements of nested vectors (a string as its character codes), each
/// copy charged.
fn flatten(v: &Value, out: &mut Vec<Value>) -> CResult<()> {
    match v {
        Value::Vec(items) => {
            for i in items {
                flatten(i, out)?;
            }
        }
        Value::Str(s) => {
            for c in s.chars() {
                w(NODE_TICKS)?;
                out.push(Value::Num(Num::Int(BigInt::from(c as u32))));
            }
        }
        other => {
            let size = match other {
                Value::Num(n) => l(mantissa(n)),
                _ => 1,
            };
            w(NODE_TICKS + size)?;
            out.push(other.clone());
        }
    }
    Ok(())
}

fn flatten_all(vals: &[Value]) -> CResult<Vec<Value>> {
    let mut items = Vec::new();
    for v in vals {
        flatten(v, &mut items)?;
    }
    Ok(items)
}

impl Eval {
    fn prec(&self) -> i64 {
        self.modes.prec
    }

    pub fn eval(&self, e: &Expr) -> CResult<Value> {
        w(NODE_TICKS)?;
        match e {
            Expr::Num(s) => Ok(Value::Num(read_number(s, self.prec())?)),
            Expr::Str(s) => {
                w(work::text_ticks(s.len()))?;
                Ok(Value::Str(s.clone()))
            }
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
            let pick = if op == "&&" {
                if is_zero(xn) {
                    xn
                } else {
                    yn
                }
            } else if is_zero(xn) {
                yn
            } else {
                xn
            };
            return Ok(Value::Num(copy(pick)?));
        }
        let x = self.eval(a)?;
        let y = self.eval(b)?;
        if COMPARISONS.contains(&op) {
            let bool_val =
                |t: bool| Value::Num(Num::Int(if t { BigInt::one() } else { BigInt::zero() }));
            if let (Value::Str(s), Value::Str(t)) = (&x, &y) {
                w(work::text_ticks(s.len().min(t.len())))?;
                return match op {
                    "==" => Ok(bool_val(s == t)),
                    "!=" => Ok(bool_val(s != t)),
                    _ => unsupported("ordering comparison of strings"),
                };
            }
            let (xn, yn) = self.num_pair(x, y, op)?;
            let ord = compare(&xn, &yn)?;
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
        let items = flatten_all(args)?;
        let mut acc: Option<Value> = None;
        for it in items {
            w(NODE_TICKS)?;
            acc = Some(match (acc, it) {
                (None, v @ (Value::Num(_) | Value::Nan | Value::Inf(_))) => v,
                (Some(a), b) if add_not_mul => add_values(&a, &b, inner)?,
                (Some(Value::Num(a)), Value::Num(b)) => Value::Num(mul(&a, &b, inner)?),
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
                let items = flatten_all(&vals)?;
                if items.is_empty() {
                    return unsupported("`vmean` of an empty vector");
                }
                let inner = self.prec() + 2;
                let mut sum: Option<Num> = None;
                for it in &items {
                    w(NODE_TICKS)?;
                    let Value::Num(n) = it else {
                        return unsupported("`vmean` of a vector with a non-number");
                    };
                    sum = Some(match sum {
                        None => copy(n)?,
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
                let items = flatten_all(&vals)?;
                if items.is_empty() {
                    return unsupported(format!(
                        "`{name}` of an empty vector (Calc gives an infinity)"
                    ));
                }
                let mut best: Option<Num> = None;
                for it in &items {
                    w(NODE_TICKS)?;
                    let Value::Num(n) = it else {
                        return unsupported(format!("`{name}` of a vector with a non-number"));
                    };
                    best = Some(match best {
                        None => copy(n)?,
                        Some(b) => {
                            let ord = compare(n, &b)?;
                            let better = if name == "vmax" {
                                ord == Ordering::Greater
                            } else {
                                ord == Ordering::Less
                            };
                            if better {
                                copy(n)?
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
                for v in &vals {
                    if let Value::Sym(s) = v {
                        return unsupported(format!("symbolic value `{s}` in `vcount`"));
                    }
                }
                let items = flatten_all(&vals)?;
                Ok(Value::Num(Num::Int(BigInt::from(items.len()))))
            }
            "vlen" => {
                if vals.len() != 1 {
                    return unsupported("`vlen` without exactly one argument");
                }
                match &vals[0] {
                    Value::Vec(items) => Ok(Value::Num(Num::Int(BigInt::from(items.len())))),
                    Value::Str(s) => {
                        w(work::text_ticks(s.len()))?;
                        Ok(Value::Num(Num::Int(BigInt::from(s.chars().count()))))
                    }
                    Value::Num(_) | Value::Nan | Value::Inf(_) => {
                        Ok(Value::Num(Num::Int(BigInt::zero())))
                    }
                    _ => unsupported("`vlen` of a symbolic value"),
                }
            }
            "string" => match vals.as_slice() {
                [Value::Str(s)] => {
                    w(work::text_ticks(s.len()))?;
                    Ok(Value::Text(s.clone()))
                }
                _ => unsupported("`string` of something other than a string literal"),
            },
            "sum" | "mean" => unsupported(format!(
                "`{name}` is Advanced Tables syntax; Org (Calc) uses `v{name}`"
            )),
            other => unsupported(format!("Calc function `{other}`")),
        }
    }
}

/// The reason given when an evaluation runs out of the command's work budget.
pub const WORK_MSG: &str = "the formulas need more computation than the plugin allows in one run";

/// Evaluate a Calc expression string under `modes`. With `num_only`
/// (`calc-eval ... 'num`), a non-numeric result is a Calc error. Its work is
/// charged to the command's meter ([`crate::work`]); once that is spent the
/// result is [`Outcome::Unsupported`] with [`WORK_MSG`].
pub fn calc_eval(src: &str, modes: Modes, num_only: bool) -> CResult<String> {
    let ev = Eval { modes };
    let expr = parse(src)?;
    let v = ev.eval(&expr)?;
    if num_only && !matches!(v, Value::Num(_)) {
        return match v {
            Value::Sym(_) | Value::Nan | Value::Inf(_) => calc_error("number expected"),
            _ => unsupported("a non-numeric result with the N flag"),
        };
    }
    let out = display(&v, &modes)?;
    if work::exhausted() {
        return unsupported(WORK_MSG);
    }
    Ok(out)
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
