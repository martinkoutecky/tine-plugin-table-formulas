//! The Emacs Lisp primitives Org applies around Calc: `string-to-number`,
//! `number-to-string` and a one-argument `format`.
//!
//! Written from the Emacs Lisp reference manual's description of these
//! functions (and checked against the C library conventions they follow);
//! no Emacs code is copied.

/// An Emacs Lisp number: a fixnum/bignum (we keep i128) or a double.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LispNum {
    Int(i128),
    Float(f64),
}

/// `(string-to-number s)` in base 10: leading blanks are skipped, the longest
/// numeric prefix is read, and anything else gives 0. A number without a
/// fraction or exponent is an integer (`"3."` reads as the integer 3).
pub fn string_to_number(s: &str) -> LispNum {
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
        return LispNum::Int(0);
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
        }
    }
    let text = &s[start..end];
    if is_float {
        // "3.e5" is not a Rust float literal; drop a dot with no fraction.
        let cleaned = text.replace(".e", "e").replace(".E", "E");
        LispNum::Float(cleaned.parse::<f64>().unwrap_or(0.0))
    } else {
        let t = text.trim_end_matches('.');
        match t.parse::<i128>() {
            Ok(v) => LispNum::Int(v),
            Err(_) => LispNum::Float(t.parse::<f64>().unwrap_or(0.0)),
        }
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

/// C-style `%g`.
fn fmt_g(x: f64, p: usize, alt: bool, upper: bool) -> String {
    let p = if p == 0 { 1 } else { p };
    if x == 0.0 {
        let s = if alt { format!("{:.*}", p - 1, 0.0) } else { "0".to_string() };
        return if x.is_sign_negative() { format!("-{s}") } else { s };
    }
    let probe = format!("{:.*e}", p - 1, x);
    let exp: i64 = probe.split_once('e').and_then(|(_, e)| e.parse().ok()).unwrap_or(0);
    let out = if exp < -4 || exp >= p as i64 {
        fmt_e(x, p - 1, upper)
    } else {
        format!("{:.*}", (p as i64 - 1 - exp).max(0) as usize, x)
    };
    if alt {
        out
    } else {
        strip_trailing_zeros(&out)
    }
}

/// Emacs `number-to-string`: integers plainly; floats with the shortest
/// `%.Ng` (N from 15) that reads back exactly, plus `.0` when that text has
/// neither a dot nor an exponent.
pub fn number_to_string(n: LispNum) -> String {
    match n {
        LispNum::Int(i) => i.to_string(),
        LispNum::Float(x) => {
            if x.is_nan() {
                return if x.is_sign_negative() { "-0.0e+NaN".into() } else { "0.0e+NaN".into() };
            }
            if x.is_infinite() {
                return if x < 0.0 { "-1.0e+INF".into() } else { "1.0e+INF".into() };
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
    /// A conversion we do not implement (`%c`, `%S`...).
    Unsupported(String),
}

/// Emacs `(format fmt n)` for a format with at most one conversion.
pub fn format_one(fmt: &str, n: LispNum) -> Result<String, FormatError> {
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
        // Emacs allows a field number `N$`; not supported here.
        let (mut minus, mut plus, mut space, mut zero, mut alt) = (false, false, false, false, false);
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
            width = width.saturating_mul(10).saturating_add(chars[i] as usize - '0' as usize);
            i += 1;
        }
        let mut precision: Option<usize> = None;
        if i < chars.len() && chars[i] == '.' {
            i += 1;
            let mut p = 0usize;
            while i < chars.len() && chars[i].is_ascii_digit() {
                p = p.saturating_mul(10).saturating_add(chars[i] as usize - '0' as usize);
                i += 1;
            }
            precision = Some(p);
        }
        if width > 1000 || precision.is_some_and(|p| p > 100) {
            return Err(FormatError::Unsupported("a huge width or precision".into()));
        }
        let Some(&conv) = chars.get(i) else {
            return Err(FormatError::Unsupported("a format ending in `%`".into()));
        };
        i += 1;
        if used {
            return Err(FormatError::TooManySpecs);
        }
        used = true;
        let as_f = match n {
            LispNum::Int(v) => v as f64,
            LispNum::Float(x) => x,
        };
        let as_i = match n {
            LispNum::Int(v) => v,
            LispNum::Float(x) => {
                if !x.is_finite() || x.abs() > 1e30 {
                    return Err(FormatError::Unsupported("an integer conversion of a huge float".into()));
                }
                x.trunc() as i128
            }
        };
        let (neg, body) = match conv {
            'd' | 'i' => {
                let mut b = as_i.unsigned_abs().to_string();
                if let Some(p) = precision {
                    if b.len() < p {
                        b = format!("{}{b}", "0".repeat(p - b.len()));
                    }
                }
                (as_i < 0, b)
            }
            'o' | 'x' | 'X' => {
                let a = as_i.unsigned_abs();
                let mut b = match conv {
                    'o' => format!("{a:o}"),
                    'x' => format!("{a:x}"),
                    _ => format!("{a:X}"),
                };
                if alt && a != 0 {
                    b = match conv {
                        'o' => format!("0{b}"),
                        'x' => format!("0x{b}"),
                        _ => format!("0X{b}"),
                    };
                }
                (as_i < 0, b)
            }
            'f' | 'e' | 'E' | 'g' | 'G' => {
                if !as_f.is_finite() {
                    return Err(FormatError::Unsupported("formatting a non-finite number".into()));
                }
                let p = precision.unwrap_or(6);
                let a = as_f.abs();
                let mut b = match conv {
                    'f' => format!("{:.*}", p, a),
                    'e' => fmt_e(a, p, false),
                    'E' => fmt_e(a, p, true),
                    'g' => fmt_g(a, p, alt, false),
                    _ => fmt_g(a, p, alt, true),
                };
                if alt && conv == 'f' && p == 0 {
                    b.push('.');
                }
                (as_f.is_sign_negative() && as_f != 0.0 || (as_f == 0.0 && as_f.is_sign_negative()), b)
            }
            's' => {
                let mut b = number_to_string(n);
                if let Some(p) = precision {
                    b = b.chars().take(p).collect();
                }
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
            other => return Err(FormatError::Unsupported(format!("`%{other}`"))),
        };
        let sign = if neg {
            "-"
        } else if plus && matches!(conv, 'd' | 'i' | 'f' | 'e' | 'E' | 'g' | 'G') {
            "+"
        } else if space && matches!(conv, 'd' | 'i' | 'f' | 'e' | 'E' | 'g' | 'G') {
            " "
        } else {
            ""
        };
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
        assert_eq!(string_to_number("3"), LispNum::Int(3));
        assert_eq!(string_to_number("3."), LispNum::Int(3));
        assert_eq!(string_to_number(" -3.5abc"), LispNum::Float(-3.5));
        assert_eq!(string_to_number(".5"), LispNum::Float(0.5));
        assert_eq!(string_to_number("1e3"), LispNum::Float(1000.0));
        assert_eq!(string_to_number("1.234e-3"), LispNum::Float(0.001234));
        assert_eq!(string_to_number("abc"), LispNum::Int(0));
        assert_eq!(string_to_number(""), LispNum::Int(0));
        assert_eq!(string_to_number("12e"), LispNum::Int(12));
    }

    #[test]
    fn prints_numbers_like_emacs() {
        assert_eq!(number_to_string(LispNum::Float(3.0)), "3.0");
        assert_eq!(number_to_string(LispNum::Float(0.1)), "0.1");
        assert_eq!(number_to_string(LispNum::Float(1000.0)), "1000.0");
        assert_eq!(number_to_string(LispNum::Float(1e20)), "1e+20");
        assert_eq!(number_to_string(LispNum::Float(1e-5)), "1e-05");
        assert_eq!(number_to_string(LispNum::Float(0.1 + 0.2)), "0.30000000000000004");
        assert_eq!(number_to_string(LispNum::Int(-7)), "-7");
    }

    #[test]
    fn formats_like_emacs() {
        assert_eq!(format_one("%.2f", LispNum::Float(1.0 / 3.0)).unwrap(), "0.33");
        assert_eq!(format_one("%.2f", LispNum::Int(3)).unwrap(), "3.00");
        assert_eq!(format_one("%.1f", LispNum::Float(0.25)).unwrap(), "0.2");
        assert_eq!(format_one("%d", LispNum::Float(-2.7)).unwrap(), "-2");
        assert_eq!(format_one("%d", LispNum::Float(-0.7)).unwrap(), "0");
        assert_eq!(format_one("%5.1f%%", LispNum::Float(12.345)).unwrap(), " 12.3%");
        assert_eq!(format_one("%e", LispNum::Float(1234.5)).unwrap(), "1.234500e+03");
        assert_eq!(format_one("%g", LispNum::Float(0.0001)).unwrap(), "0.0001");
        assert_eq!(format_one("%g", LispNum::Float(1234567.0)).unwrap(), "1.23457e+06");
        assert_eq!(format_one("%05d", LispNum::Int(42)).unwrap(), "00042");
        assert_eq!(format_one("EUR %s", LispNum::Int(5)).unwrap(), "EUR 5");
        assert_eq!(format_one("x", LispNum::Int(5)).unwrap(), "x");
        assert_eq!(format_one("%d %d", LispNum::Int(5)), Err(FormatError::TooManySpecs));
    }
}
