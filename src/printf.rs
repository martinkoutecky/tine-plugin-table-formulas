//! The Emacs Lisp primitives Org applies around Calc: `string-to-number`,
//! `number-to-string` and a one-argument `format`.
//!
//! Written from the Emacs Lisp reference manual's description of these
//! functions (and checked against the C library conventions they follow);
//! no Emacs code is copied.

use crate::work;
use num_bigint::BigInt;
use num_traits::{Signed, ToPrimitive, Zero};

// The functions here charge the command's work meter ([`crate::work`])
// without failing: the charges are sticky, and the formula engine declines
// the field at its next check once the budget is spent.

/// Integers with more decimal digits than this are not converted: the plugin
/// declines instead (malformed-input budget; Emacs itself has bignums).
pub const MAX_INT_DIGITS: usize = 2_000;

/// An Emacs Lisp number: a fixnum/bignum (exact) or a double.
#[derive(Clone, Debug, PartialEq)]
pub enum LispNum {
    Int(BigInt),
    Float(f64),
}

impl LispNum {
    pub fn int(v: i64) -> LispNum {
        LispNum::Int(BigInt::from(v))
    }
}

/// A column number, mode digit count or similar small integer written as
/// `[-+]?[0-9]+`: read with `string-to-number` semantics but saturated to
/// +-1_000_000, which is far outside every table and every budget. Leading
/// zeros are read exactly, however many there are (Org reads `$0001` as
/// column 1). `None` means the text is not such an integer or cannot be read
/// exactly; callers must decline, never treat it as 0 (review round 2, B3).
pub fn small_int(s: &str) -> Option<i64> {
    const CAP: i64 = 1_000_000;
    let _ = work::text(s.len());
    let t = s.trim_start_matches([' ', '\t', '\n', '\r', '\x0c']);
    let (neg, digits) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
        let sig = digits.trim_start_matches('0');
        let v = if sig.len() > 7 {
            CAP
        } else {
            sig.parse::<i64>().unwrap_or(0).min(CAP)
        };
        return Some(if neg { -v } else { v });
    }
    match string_to_number(s)? {
        LispNum::Int(v) => Some(
            v.to_i64()
                .unwrap_or(if v.is_negative() { -CAP } else { CAP })
                .clamp(-CAP, CAP),
        ),
        LispNum::Float(_) => None,
    }
}

/// `(string-to-number s)` in base 10: leading blanks are skipped, the longest
/// numeric prefix is read, and anything else gives 0. A number without a
/// fraction or exponent is an integer (`"3."` reads as the integer 3), of any
/// size, as Emacs bignums are. `None` means the plugin cannot reproduce the
/// value exactly (Emacs's `1.0e+INF`/`0.0e+NaN` syntax, or an integer above
/// [`MAX_INT_DIGITS`]); callers decline.
pub fn string_to_number(s: &str) -> Option<LispNum> {
    let _ = work::text(s.len());
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() && (b[i] == b' ' || b[i] == b'\t') {
        i += 1;
    }
    let start = i;
    if i < b.len() && (b[i] == b'-' || b[i] == b'+') {
        i += 1;
    }
    let int_start = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    let int_digits = i - int_start;
    let mut frac_digits = 0;
    let mut end_int = i;
    let mut is_float = false;
    if i < b.len() && b[i] == b'.' {
        let mut j = i + 1;
        while j < b.len() && b[j].is_ascii_digit() {
            j += 1;
        }
        frac_digits = j - i - 1;
        if frac_digits > 0 {
            is_float = true;
            i = j;
        } else if int_digits > 0 {
            // "3." is an integer; the dot is consumed.
            end_int = i;
            i += 1;
        }
    }
    if int_digits == 0 && frac_digits == 0 {
        return Some(LispNum::int(0));
    }
    let mut end = if is_float { i } else { end_int };
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        let mut j = i + 1;
        if j < b.len() && (b[j] == b'-' || b[j] == b'+') {
            j += 1;
        }
        let d0 = j;
        while j < b.len() && b[j].is_ascii_digit() {
            j += 1;
        }
        if j > d0 {
            is_float = true;
            end = j;
        } else if s[d0..].starts_with("INF") || s[d0..].starts_with("NaN") {
            // Emacs reads `1.0e+INF` and `0.0e+NaN` as an infinity / NaN.
            return None;
        }
    }
    let text = &s[start..end];
    if is_float {
        // "3.e5" is not a Rust float literal; drop a dot with no fraction.
        let _ = work::text(text.len().saturating_mul(4));
        let cleaned = text.replace(".e", "e").replace(".E", "E");
        Some(LispNum::Float(cleaned.parse::<f64>().unwrap_or(0.0)))
    } else {
        if int_digits > MAX_INT_DIGITS {
            return None;
        }
        let _ = work::ticks(work::decimal_ticks(work::digit_limbs(int_digits)));
        let t = text.trim_end_matches('.');
        let t = t.strip_prefix('+').unwrap_or(t);
        t.parse::<BigInt>().ok().map(LispNum::Int)
    }
}

/// C-style `%e` with precision `p`.
fn fmt_e(x: f64, p: usize, upper: bool) -> String {
    let s = format!("{:.*e}", p, x);
    let (mant, exp) = s.split_once('e').unwrap_or((&s, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let sign = if exp < 0 { '-' } else { '+' };
    let e = if upper { 'E' } else { 'e' };
    format!("{mant}{e}{sign}{:02}", exp.abs())
}

fn strip_trailing_zeros(s: &str) -> String {
    if let Some(epos) = s.find(['e', 'E']) {
        let (m, e) = s.split_at(epos);
        if m.contains('.') {
            let m = m.trim_end_matches('0').trim_end_matches('.');
            return format!("{m}{e}");
        }
        return s.to_string();
    }
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s.to_string()
    }
}

/// C-style `%g` (`alt` is the `#` flag: trailing zeros and the point stay).
fn fmt_g(x: f64, p: usize, alt: bool, upper: bool) -> String {
    let p = if p == 0 { 1 } else { p };
    let out = if x == 0.0 {
        format!("{:.*}", p - 1, x)
    } else {
        let probe = format!("{:.*e}", p - 1, x);
        let exp: i64 = probe
            .split_once('e')
            .and_then(|(_, e)| e.parse().ok())
            .unwrap_or(0);
        if exp < -4 || exp >= p as i64 {
            fmt_e(x, p - 1, upper)
        } else {
            format!("{:.*}", (p as i64 - 1 - exp).max(0) as usize, x)
        }
    };
    if alt {
        force_point(&out)
    } else {
        strip_trailing_zeros(&out)
    }
}

/// The `#` flag's "always a decimal point" for `%e`/`%g`: `1e+00` -> `1.e+00`.
fn force_point(s: &str) -> String {
    if s.contains('.') {
        return s.to_string();
    }
    match s.find(['e', 'E']) {
        Some(p) => format!("{}.{}", &s[..p], &s[p..]),
        None => format!("{s}."),
    }
}

/// Emacs `number-to-string`: integers plainly; floats with the shortest
/// `%.Ng` (N from 15) that reads back exactly, plus `.0` when that text has
/// neither a dot nor an exponent.
pub fn number_to_string(n: &LispNum) -> String {
    match n {
        LispNum::Int(i) => {
            let _ = work::ticks(work::decimal_ticks(work::limbs(i)));
            i.to_string()
        }
        LispNum::Float(x) => {
            // Up to three shortest-digits conversions and a read back.
            let _ = work::ticks(FLOAT_TEXT_TICKS);
            let x = *x;
            if x.is_nan() {
                return if x.is_sign_negative() {
                    "-0.0e+NaN".into()
                } else {
                    "0.0e+NaN".into()
                };
            }
            if x.is_infinite() {
                return if x < 0.0 {
                    "-1.0e+INF".into()
                } else {
                    "1.0e+INF".into()
                };
            }
            let mut s = String::new();
            for p in 15..=17 {
                s = fmt_g(x, p, false, false);
                if s.parse::<f64>().ok() == Some(x) {
                    break;
                }
            }
            if !s.contains(['.', 'e', 'E']) || s.contains("inf") {
                s.push_str(".0");
            }
            s
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum FormatError {
    /// More than one `%` conversion: Org would call `format` with too few
    /// arguments, an Emacs error.
    TooManySpecs,
    /// A conversion, flag or precision we do not reproduce exactly (`%c`,
    /// `%S`, `%+x`, `%.3s`...).
    Unsupported(String),
}

fn unsupported<T>(why: impl Into<String>) -> Result<T, FormatError> {
    Err(FormatError::Unsupported(why.into()))
}

/// Integers up to 2^53 convert to a double exactly; beyond that the plugin
/// does not reproduce Emacs's bignum-to-float rounding and declines.
const EXACT_FLOAT_INT: i64 = 1 << 53;

/// Emacs `(format fmt n)` for a format with at most one conversion.
///
/// Every flag/precision combination accepted here was checked against Emacs
/// 31.1 (`tools/emacs-printf-oracle.el`); anything else is declined:
/// * `d i`: flags `- + space 0`, width, precision (minimum digits; `%.0d` of
///   0 is empty, as in C);
/// * `o x X`: flags `- 0 #`, width, precision; a negative value is printed
///   with a minus sign (Emacs's default, `binary-as-unsigned` nil);
/// * `f e E g G`: flags `- + space 0 #`, width, precision;
/// * `s`: flag `-` and width, no precision.
/// Converting a double to or from text, up to 100 digits.
const FLOAT_TEXT_TICKS: u64 = 4096;

pub fn format_one(fmt: &str, n: &LispNum) -> Result<String, FormatError> {
    let _ = work::text(fmt.len().saturating_mul(2));
    let chars: Vec<char> = fmt.chars().collect();
    let mut out = String::new();
    let mut used = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c != '%' {
            out.push(c);
            i += 1;
            continue;
        }
        i += 1;
        if i < chars.len() && chars[i] == '%' {
            out.push('%');
            i += 1;
            continue;
        }
        // Emacs allows a field number `N$`; not supported here (the `$`
        // ends up as an unknown conversion).
        let (mut minus, mut plus, mut space, mut zero, mut alt) =
            (false, false, false, false, false);
        while i < chars.len() && "-+ 0#".contains(chars[i]) {
            match chars[i] {
                '-' => minus = true,
                '+' => plus = true,
                ' ' => space = true,
                '0' => zero = true,
                _ => alt = true,
            }
            i += 1;
        }
        let mut width = 0usize;
        while i < chars.len() && chars[i].is_ascii_digit() {
            width = width
                .saturating_mul(10)
                .saturating_add(chars[i] as usize - '0' as usize);
            i += 1;
        }
        let mut precision: Option<usize> = None;
        if i < chars.len() && chars[i] == '.' {
            i += 1;
            let mut p = 0usize;
            while i < chars.len() && chars[i].is_ascii_digit() {
                p = p
                    .saturating_mul(10)
                    .saturating_add(chars[i] as usize - '0' as usize);
                i += 1;
            }
            precision = Some(p);
        }
        if width > 1000 || precision.is_some_and(|p| p > 100) {
            return unsupported("a huge width or precision");
        }
        let _ = work::text(width + precision.unwrap_or(0));
        let Some(&conv) = chars.get(i) else {
            return unsupported("a format ending in `%`");
        };
        i += 1;
        if used {
            return Err(FormatError::TooManySpecs);
        }
        used = true;
        let (neg, radix_prefix, body) = match conv {
            'd' | 'i' | 'o' | 'x' | 'X' => {
                let integer_flag_ok = match conv {
                    'd' | 'i' => !alt,
                    _ => !plus && !space,
                };
                if !integer_flag_ok {
                    return unsupported(format!("these flags with `%{conv}`"));
                }
                let v: BigInt = match n {
                    LispNum::Int(v) => v.clone(),
                    LispNum::Float(x) => {
                        if !x.is_finite() || x.abs() > 1e30 {
                            return unsupported("an integer conversion of a huge float");
                        }
                        if precision.is_some() {
                            // Emacs prints a float's integer part without C's
                            // precision rule (`%.0d` of 0.5 is `0`).
                            return unsupported(format!(
                                "a precision with `%{conv}` of a fraction"
                            ));
                        }
                        BigInt::from(x.trunc() as i128)
                    }
                };
                let a = v.abs();
                let _ = work::ticks(work::decimal_ticks(work::limbs(&a)));
                let mut b = if precision == Some(0) && a.is_zero() {
                    String::new()
                } else {
                    match conv {
                        'd' | 'i' => a.to_str_radix(10),
                        'o' => a.to_str_radix(8),
                        'x' => a.to_str_radix(16),
                        _ => a.to_str_radix(16).to_ascii_uppercase(),
                    }
                };
                if let Some(p) = precision {
                    if b.len() < p {
                        b = format!("{}{b}", "0".repeat(p - b.len()));
                    }
                }
                let mut radix_prefix = "";
                if alt {
                    match conv {
                        'o' if !b.starts_with('0') => b = format!("0{b}"),
                        'x' if !a.is_zero() => radix_prefix = "0x",
                        'X' if !a.is_zero() => radix_prefix = "0X",
                        _ => {}
                    }
                }
                (v.is_negative(), radix_prefix, b)
            }
            // Emacs has no `%E` or `%G` (it signals an error).
            'f' | 'e' | 'g' => {
                if alt && conv == 'g' {
                    // Emacs 31.1 gives `1.e+05` for `%#.5g` of 99999.5
                    // where C gives `1.0000e+05`; not reproduced.
                    return unsupported("the `#` flag with `%g`");
                }
                let as_f = match n {
                    LispNum::Int(v) => match v.to_i64() {
                        Some(k) if k.abs() <= EXACT_FLOAT_INT => k as f64,
                        _ => return unsupported("a float conversion of an integer above 2^53"),
                    },
                    LispNum::Float(x) => *x,
                };
                if !as_f.is_finite() {
                    return unsupported("formatting a non-finite number");
                }
                let p = precision.unwrap_or(6);
                let a = as_f.abs();
                let _ = work::ticks(FLOAT_TEXT_TICKS);
                let mut b = match conv {
                    'f' => format!("{:.*}", p, a),
                    'e' => fmt_e(a, p, false),
                    _ => fmt_g(a, p, alt, false),
                };
                if alt {
                    b = force_point(&b);
                }
                (as_f.is_sign_negative(), "", b)
            }
            's' => {
                if plus || space || zero || alt || precision.is_some() {
                    return unsupported("flags or a precision with `%s`");
                }
                let b = number_to_string(n);
                let pad = width.saturating_sub(b.chars().count());
                if minus {
                    out.push_str(&b);
                    out.push_str(&" ".repeat(pad));
                } else {
                    out.push_str(&" ".repeat(pad));
                    out.push_str(&b);
                }
                continue;
            }
            other => return unsupported(format!("`%{other}`")),
        };
        let signed = matches!(conv, 'd' | 'i' | 'f' | 'e' | 'g');
        let sign = if neg {
            "-"
        } else if plus && signed {
            "+"
        } else if space && signed {
            " "
        } else {
            ""
        };
        // Zero padding goes after the sign and a `0x` prefix, as in C.
        let sign = format!("{sign}{radix_prefix}");
        let sign = sign.as_str();
        let len = sign.len() + body.chars().count();
        let pad = width.saturating_sub(len);
        if minus {
            out.push_str(sign);
            out.push_str(&body);
            out.push_str(&" ".repeat(pad));
        } else if zero && !(precision.is_some() && matches!(conv, 'd' | 'i' | 'o' | 'x' | 'X')) {
            out.push_str(sign);
            out.push_str(&"0".repeat(pad));
            out.push_str(&body);
        } else {
            out.push_str(&" ".repeat(pad));
            out.push_str(sign);
            out.push_str(&body);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_numbers_like_emacs() {
        let int = |v: i64| Some(LispNum::int(v));
        let float = |v: f64| Some(LispNum::Float(v));
        assert_eq!(string_to_number("3"), int(3));
        assert_eq!(string_to_number("3."), int(3));
        assert_eq!(string_to_number(" -3.5abc"), float(-3.5));
        assert_eq!(string_to_number(".5"), float(0.5));
        assert_eq!(string_to_number("1e3"), float(1000.0));
        assert_eq!(string_to_number("1.234e-3"), float(0.001234));
        assert_eq!(string_to_number("abc"), int(0));
        assert_eq!(string_to_number(""), int(0));
        assert_eq!(string_to_number("12e"), int(12));
    }

    #[test]
    fn prints_numbers_like_emacs() {
        assert_eq!(number_to_string(&LispNum::Float(3.0)), "3.0");
        assert_eq!(number_to_string(&LispNum::Float(0.1)), "0.1");
        assert_eq!(number_to_string(&LispNum::Float(1000.0)), "1000.0");
        assert_eq!(number_to_string(&LispNum::Float(1e20)), "1e+20");
        assert_eq!(number_to_string(&LispNum::Float(1e-5)), "1e-05");
        assert_eq!(
            number_to_string(&LispNum::Float(0.1 + 0.2)),
            "0.30000000000000004"
        );
        assert_eq!(number_to_string(&LispNum::Int(BigInt::from(-7))), "-7");
    }

    #[test]
    fn formats_like_emacs() {
        assert_eq!(
            format_one("%.2f", &LispNum::Float(1.0 / 3.0)).unwrap(),
            "0.33"
        );
        assert_eq!(format_one("%.2f", &LispNum::int(3)).unwrap(), "3.00");
        assert_eq!(format_one("%.1f", &LispNum::Float(0.25)).unwrap(), "0.2");
        assert_eq!(format_one("%d", &LispNum::Float(-2.7)).unwrap(), "-2");
        assert_eq!(format_one("%d", &LispNum::Float(-0.7)).unwrap(), "0");
        assert_eq!(
            format_one("%5.1f%%", &LispNum::Float(12.345)).unwrap(),
            " 12.3%"
        );
        assert_eq!(
            format_one("%e", &LispNum::Float(1234.5)).unwrap(),
            "1.234500e+03"
        );
        assert_eq!(format_one("%g", &LispNum::Float(0.0001)).unwrap(), "0.0001");
        assert_eq!(
            format_one("%g", &LispNum::Float(1234567.0)).unwrap(),
            "1.23457e+06"
        );
        assert_eq!(format_one("%05d", &LispNum::int(42)).unwrap(), "00042");
        assert_eq!(format_one("EUR %s", &LispNum::int(5)).unwrap(), "EUR 5");
        assert_eq!(format_one("x", &LispNum::int(5)).unwrap(), "x");
        assert_eq!(
            format_one("%d %d", &LispNum::int(5)),
            Err(FormatError::TooManySpecs)
        );
    }
}
