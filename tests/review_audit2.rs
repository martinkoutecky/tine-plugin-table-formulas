//! Regression cases from the second registry audit of 2026-10-20: writes
//! outside a table's columns, and work the per-command budget did not charge
//! (comparisons, vector reductions, the mode-letter passes, and loops found
//! in the audit of every loop that followed).

use std::sync::mpsc;
use std::time::Duration;
use tine_plugin_table_formulas::formula::Report;
use tine_plugin_table_formulas::table::Dialect;
use tine_plugin_table_formulas::{process_text, Laps};

/// Run `f` with a deadline; work the budget does not bound fails the test
/// instead of hanging it.
fn within<T: Send + 'static>(secs: u64, f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .stack_size(1 << 20)
        .spawn(move || {
            let _ = tx.send(f());
        })
        .expect("thread");
    rx.recv_timeout(Duration::from_secs(secs))
        .expect("finished before the deadline")
}

fn run(raw: &str, dialect: Dialect, secs: u64) -> (String, Report) {
    let raw = raw.to_string();
    within(secs, move || {
        process_text(&raw, dialect, Laps::Times(1)).expect("no abort")
    })
}

fn rows(n: usize, row: &str) -> String {
    row.repeat(n)
}

fn assert_declined(raw: &str, dialect: Dialect, secs: u64, reason: &str) {
    let (out, report) = run(raw, dialect, secs);
    assert!(out == raw, "a declined block must be kept byte for byte");
    let why: Vec<&str> = report.skipped.iter().map(|s| s.reason.as_str()).collect();
    assert!(why.iter().any(|w| w.contains(reason)), "{why:?}");
}

// ---- 1. Writes outside the table's columns.

#[test]
fn markdown_writes_beyond_the_header_are_declined() {
    for raw in [
        // A column formula past the header (it would add a column the
        // header and delimiter rows do not have in the source).
        "| a | b |\n|---|---|\n| 1 | 2 |\n<!-- TBLFM: $3=7 -->",
        "| a | b |\n|---|---|\n| 1 | 2 |\n<!-- TBLFM: $5=$1 -->",
        // A marked header row is computed too; still past the header.
        "| # | b |\n|---|---|\n| # | 2 |\n<!-- TBLFM: $3=7 -->",
        // Field formulas.
        "| a | b |\n|---|---|\n| 1 | 2 |\n<!-- TBLFM: @2$3=7 -->",
        "| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |\n<!-- TBLFM: @2$3..@3$3=7 -->",
    ] {
        assert_declined(raw, Dialect::Markdown, 30, "header");
    }
}

#[test]
fn markdown_writes_inside_the_header_still_work() {
    // A body row shorter than the header gets its missing cell.
    let (out, report) = run(
        "| a | b |\n|---|---|\n| 1 |\n<!-- TBLFM: $2=7 -->",
        Dialect::Markdown,
        30,
    );
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);
    assert_eq!(out, "| a | b |\n| - | - |\n| 1 | 7 |\n<!-- TBLFM: $2=7 -->");
}

/// Org 9.8.7 (Emacs 31.1, `tools/emacs-oracle.el`, 2026-10-20) extends the
/// table for a column formula past its last column, and aborts for a field
/// formula there.
#[test]
fn org_column_formulas_past_the_last_column_extend_the_table_as_org_does() {
    let cases = [
        (
            "| 1 | 2 |\n| 3 | 4 |\n#+TBLFM: $3=7",
            "| 1 | 2 | 7 |\n| 3 | 4 | 7 |\n#+TBLFM: $3=7",
        ),
        (
            "| a | b |\n|---+---|\n| 1 | 2 |\n| 3 | 4 |\n#+TBLFM: $3=$1+$2",
            "| a | b |   |\n|---+---+---|\n| 1 | 2 | 3 |\n| 3 | 4 | 7 |\n#+TBLFM: $3=$1+$2",
        ),
        (
            "| a | b |\n|---+---|\n| 1 | 2 |\n#+TBLFM: $5=$1",
            "| a | b |   |   |   |\n|---+---+---+---+---|\n| 1 | 2 |   |   | 1 |\n#+TBLFM: $5=$1",
        ),
        (
            "| a | b | c |\n|---+---+---|\n| 1 | 2 |\n| 3 |\n#+TBLFM: $3=9",
            "| a | b | c |\n|---+---+---|\n| 1 | 2 | 9 |\n| 3 |   | 9 |\n#+TBLFM: $3=9",
        ),
        (
            "| 1 | 2 |\n#+TBLFM: $3=$3+1",
            "| 1 | 2 | 1 |\n#+TBLFM: $3=$3+1",
        ),
    ];
    for (raw, org) in cases {
        let (out, report) = run(raw, Dialect::Org, 30);
        assert!(report.skipped.is_empty(), "{raw}: {:?}", report.skipped);
        assert_eq!(out, org);
    }
    for raw in [
        "| 1 | 2 |\n| 3 | 4 |\n#+TBLFM: @1$3=7",
        "| 1 | 2 |\n| 3 | 4 |\n#+TBLFM: @1$3..@2$3=7",
    ] {
        let raw = raw.to_string();
        let e = within(30, move || process_text(&raw, Dialect::Org, Laps::Times(1)))
            .expect_err("Org: Missing columns in the table.  Aborting");
        assert!(e.message.contains("missing columns"), "{e:?}");
    }
}

// ---- 2. Comparisons: Calc compares exactly, whatever the exponents.

#[test]
fn comparisons_across_a_wide_exponent_spread_are_bounded() {
    // Comparing 1e9999 with 1e-9999 built a 19,998-digit power of ten,
    // uncharged (5.5 s for 100 such rows in the release guest).
    let formula = format!("vmax([{}])", vec!["1e9999,1e-9999"; 150].join(","));
    let raw = rows(1000, "| |\n") + &format!("#+TBLFM: $1={formula}");
    let (out, report) = run(&raw, Dialect::Org, 20);
    if report.skipped.is_empty() {
        assert!(out.starts_with("| 1e9999 |\n| 1e9999 |"), "{}", &out[..40]);
    } else {
        assert_eq!(out, raw);
        assert!(report.skipped[0].reason.contains("computation"));
    }
}

/// Emacs 31.1 / Org 9.8.7 (2026-10-20) results for comparisons the exact
/// comparison must keep.
#[test]
fn comparisons_match_emacs() {
    let spread = ["1e9999,1e-9999"; 5].join(",");
    let cases = [
        (format!("vmax([{spread}])"), "1e9999"),
        (format!("vmin([{spread}])"), "1e-9999"),
        (
            "if(1e9999<1e-9999,1,0)+if(-1e9999<1e-9999,10,0)+if(2e-9999>1e-9999,100,0)".into(),
            "110",
        ),
        ("vmax([3, 2.5e1, 1e2, -1e3, 99.99])".into(), "100."),
        (
            "vmin([3, 2.5e1, 1e2, -1e3, 99.99, -0.5e4])".into(),
            "-5000.",
        ),
        ("vmax([-1e-5, -2e-5, -1e-4])".into(), "-1e-5"),
        (
            "if(1e2==100,1,0)+if(0.5==5e-1,10,0)+if(0==0.0,100,0)+if(1000<=1e3,1000,0)".into(),
            "1111",
        ),
    ];
    for (f, want) in cases {
        let raw = format!("| 0 |\n#+TBLFM: $1={f}");
        let (out, report) = run(&raw, Dialect::Org, 30);
        assert!(report.skipped.is_empty(), "{f}: {:?}", report.skipped);
        assert_eq!(out, format!("| {want} |\n#+TBLFM: $1={f}"));
    }
}

// ---- 3. Vector reductions charge each element's arithmetic.

#[test]
fn vector_reductions_charge_each_element() {
    // At 200 digits of precision, each addition of 1e200 and 1e-200 builds
    // and rounds a 400-digit number; the loops charged nothing for it (about
    // 0.85 s for 100 rows in the release guest).
    let list = vec!["1e200,1e-200"; 170].join(",");
    for f in ["vmean", "vsum"] {
        let raw = rows(100, "| |\n") + &format!("#+TBLFM: $1={f}([{list}]);p200");
        assert_declined(&raw, Dialect::Org, 60, "computation");
    }
}

#[test]
fn modest_reductions_at_high_precision_still_compute() {
    let list = ["1e200,1e-200"; 10].join(",");
    let raw = rows(3, "| |\n") + &format!("#+TBLFM: $1=vsum([{list}]);p200");
    let (out, report) = run(&raw, Dialect::Org, 30);
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);
    assert_ne!(out, raw);
}

// ---- 4. The mode-letter and flag passes over a formula's `;` part.

#[test]
fn mode_letter_passes_are_charged() {
    // Each mode letter is found by a search from the start of the format
    // text and removed; 500 of them behind 1000 other characters cost about
    // 8 ms per field, charged as four passes (100 rows were written).
    for tail in ["p5".repeat(500), "N".repeat(1000)] {
        let raw = rows(100, "| |\n") + &format!("#+TBLFM: $1=1;{}{tail}", "x".repeat(1000));
        assert_declined(&raw, Dialect::Org, 60, "computation");
    }
}

#[test]
fn ordinary_modes_still_work() {
    let (out, report) = run("| 1 | |\n#+TBLFM: $2=$1/3;p20f5N", Dialect::Org, 30);
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);
    assert_eq!(out, "| 1 | 0.33333 |\n#+TBLFM: $2=$1/3;p20f5N");
}

// ---- 5. Loops found by the audit of every loop.

#[test]
fn many_markdown_tables_without_blank_lines_are_found_in_linear_time() {
    // Each table looked back through every line above it for an HTML line
    // (6.9 s for 10,000 tables in the release guest).
    let raw = "| a |\n|---|\nx\n".repeat(10_000) + "| a |\n|---|\n| 1 |\n<!-- TBLFM: @2$1=2 -->";
    let (out, _) = run(&raw, Dialect::Markdown, 20);
    assert!(
        out.ends_with("| 2 |\n<!-- TBLFM: @2$1=2 -->"),
        "{}",
        &out[out.len() - 60..]
    );
}

#[test]
fn a_long_formula_line_is_split_in_linear_time() {
    // Each entry was compared with every earlier one for a repeated
    // left-hand side (a guard: 0.2 s in the release guest before the fix).
    let entries: Vec<String> = (1..12_000).map(|i| format!("@{i}$9=1")).collect();
    let raw = format!("| 1 |\n#+TBLFM: {}", entries.join("::"));
    let e = within(20, move || process_text(&raw, Dialect::Org, Laps::Times(1)))
        .expect_err("@2 does not exist");
    assert!(e.message.contains("invalid row number"), "{e:?}");
}

#[test]
fn many_declined_tables_keep_a_short_report() {
    // An HTML line declines every table of the block (review round 2).
    let raw = "<p>\n\n".to_string()
        + &(0..3000)
            .map(|i| format!("| a |\n|---|\n<!-- TBLFM: $1={i} -->\n\n"))
            .collect::<String>();
    let (out, report) = run(&raw, Dialect::Markdown, 20);
    assert_eq!(out, raw);
    assert_eq!(report.declined_tables, 3000);
    assert!(report.skipped.len() <= 32, "{}", report.skipped.len());
}

// ---- 6. Backslashes before a separator (registry audit of fae8255).

#[test]
fn markdown_results_with_a_backslash_are_declined() {
    // `1\` written before ` |`: Markdown may read `\|` as an escaped pipe.
    for f in [r"$2=$1;\", r"$2=$1;%d\", r"$2=$1;%d\x"] {
        let raw = format!("| a | b |\n|---|---|\n| 1 | |\n<!-- TBLFM: {f} -->");
        assert_declined(&raw, Dialect::Markdown, 30, "backslash");
    }
}

/// Org splits on every `|`; Org 9.8.7 (Emacs 31.1, 2026-10-20) writes these.
#[test]
fn org_results_with_a_backslash_match_emacs() {
    for (f, cell) in [
        (r"$2=$1;%d\", r"1\"),
        (r"$2=$1;\", r"\"),
        (r"$2=$1;%d\x", r"1\x"),
    ] {
        let (out, report) = run(&format!("| 1 | |\n#+TBLFM: {f}"), Dialect::Org, 30);
        assert!(report.skipped.is_empty(), "{f}: {:?}", report.skipped);
        assert_eq!(out, format!("| 1 | {cell} |\n#+TBLFM: {f}"));
    }
}

#[test]
fn an_untouched_trailing_backslash_keeps_its_cell() {
    let (out, report) = run(
        "| a | b |\n|---|---|\n| x\\ | |\n<!-- TBLFM: $2=1 -->",
        Dialect::Markdown,
        30,
    );
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);
    assert_eq!(
        out,
        "| a  | b |\n| -- | - |\n| x\\ | 1 |\n<!-- TBLFM: $2=1 -->"
    );
}

#[test]
fn a_rendered_row_that_splits_differently_does_not_read_back() {
    use tine_plugin_table_formulas::table::parse_table;
    let t = parse_table(
        &["| a | b |", "|---|---|", "| x\\ | y |"],
        Dialect::Markdown,
    );
    let good: Vec<String> = t.render();
    assert!(t.reads_back(&good), "{good:?}");
    // The backslash right before the separator escapes it: one cell, not two.
    let merged: Vec<String> = ["| a | b |", "| - | - |", "| x\\| y |"]
        .map(String::from)
        .into();
    assert!(!t.reads_back(&merged));
    let org = parse_table(&["| 1 | 2 |"], Dialect::Org);
    assert!(!org.reads_back(&["| 1 | 2 | 3 |".to_string()]));
    assert!(org.reads_back(&org.render()));
}
