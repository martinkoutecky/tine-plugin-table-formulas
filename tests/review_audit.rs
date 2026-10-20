//! Regression cases from the registry audit of 2026-10-20 (AI findings on
//! the work budget). The host's 16 MiB memory and 250 ms timeout contain
//! these inputs, but the plugin's own documented refusal must come first:
//! one budget per command invocation, charged for every field read, every
//! scan of a formula's text and the Calc work of each evaluation.

use std::sync::mpsc;
use std::time::Duration;
use tine_plugin_table_formulas::formula::Report;
use tine_plugin_table_formulas::table::Dialect;
use tine_plugin_table_formulas::{process_text, Laps};

/// Run `f` with a deadline; a missing bound fails instead of hanging.
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

fn org(raw: String) -> (String, String, Report) {
    let (out, report) = within(30, {
        let raw = raw.clone();
        move || process_text(&raw, Dialect::Org, Laps::Times(1)).expect("no abort")
    });
    (raw, out, report)
}

/// The whole block is left as it was, with a work-budget notice.
fn assert_budget_decline(raw: String, reason: &str) {
    let (raw, out, report) = org(raw);
    assert!(out == raw, "declined input must be kept");
    let why: Vec<&str> = report.skipped.iter().map(|s| s.reason.as_str()).collect();
    assert!(why.iter().any(|w| w.contains(reason)), "{why:?}");
}

fn rows(n: usize, row: impl Fn(usize) -> String) -> String {
    (0..n).map(row).collect()
}

// ---- Left-hand ranges: the right-hand side is not copied per target, and
// the aggregate (targets x formula size) is refused before any target runs.

#[test]
fn a_large_lhs_range_with_a_long_formula_is_refused_up_front() {
    // 5,000 targets times a 20,001-character formula (before this fix, each
    // target got its own copy of the formula text).
    let raw = rows(100, |_| format!("|{}\n", "0|".repeat(50)))
        + &format!("#+TBLFM: @1$1..@100$50=1{}", "+0".repeat(10_000));
    assert_budget_decline(raw, "target fields");
}

#[test]
fn several_lhs_ranges_share_one_aggregate_check() {
    // Each range alone fits; together they do not.
    let raw = rows(100, |_| format!("|{}\n", "0|".repeat(50)))
        + &format!(
            "#+TBLFM: @1$1..@50$50=1{pad}::@51$1..@100$50=2{pad}",
            pad = "+0".repeat(80)
        );
    assert_budget_decline(raw, "target fields");
}

// ---- One budget per command invocation, not per table.

#[test]
fn the_budget_is_shared_by_every_table_in_the_block() {
    let table = rows(2500, |i| format!("| {i} | |\n")) + "#+TBLFM: $2=$1\n";
    let raw = format!("{table}\n{table}");
    let (_, out, report) = org(raw);
    assert_eq!(report.declined_tables, 1, "{:?}", report.skipped);
    assert!(report.skipped[0].reason.contains("computation"));
    // The first table was recalculated, the second left as it was.
    let (first, second) = out.split_once("\n\n").unwrap();
    assert!(first.contains("| 2499 | 2499 |"), "{}", &first[..40]);
    assert_eq!(second, table);
}

#[test]
fn one_large_table_alone_fits_the_budget() {
    let raw = rows(2500, |i| format!("| {i} | |\n")) + "#+TBLFM: $2=$1\n";
    let (_, out, report) = org(raw);
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);
    assert!(out.contains("| 2499 | 2499 |"));
}

// ---- Every field read is charged: single fields and current-row ranges.

#[test]
fn single_field_substitutions_are_charged() {
    // 1999 `$1` references: each substitution rescans the growing formula
    // (about 350 ms in the release guest before this fix).
    let raw = format!("| 1 | |\n#+TBLFM: $2=vsum({})", vec!["$1"; 1999].join(","));
    assert_budget_decline(raw, "computation");
}

#[test]
fn current_row_ranges_are_charged_per_field() {
    let raw = rows(200, |_| format!("|{}\n", "1|".repeat(50)))
        + &format!("#+TBLFM: $50=vsum({})", vec!["$1..$49"; 40].join(","));
    assert_budget_decline(raw, "computation");
}

// ---- Evaluation is charged by expression size and big-integer work.

#[test]
fn big_integer_work_is_charged() {
    let term = "(2^500)^6*(2^500)^6";
    let raw = format!("| 0 |\n#+TBLFM: $1=vsum({})", vec![term; 4000].join(","));
    assert_budget_decline(raw, "computation");
}

#[test]
fn a_long_expression_is_charged_by_its_size() {
    let raw = format!("| 0 |\n#+TBLFM: $1=vsum({})", vec!["1"; 60_000].join(","));
    assert_budget_decline(raw, "computation");
}

#[test]
fn modest_big_integer_and_long_formulas_still_compute() {
    let term = "(2^500)^6*(2^500)^6";
    let (_, out, report) = org(format!(
        "| 0 |\n#+TBLFM: $1=vsum({})",
        vec![term; 100].join(",")
    ));
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);
    assert!(!out.starts_with("| 0 |"));
    let (_, out, report) = org(format!(
        "| 0 |\n#+TBLFM: $1=vsum({})",
        vec!["1"; 2000].join(",")
    ));
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);
    assert!(out.starts_with("| 2000 |"), "{}", &out[..20]);
}
