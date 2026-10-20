//! Regression cases from the release review of 2026-10-20, round 1
//! (findings F1, F2, F5, F7, F8 hint, F9). The Org-side cases with an Emacs
//! result live in `fixtures/emacs-oracle-r1.json`; these are the Markdown
//! layouts (no Emacs oracle) and the input budgets.

use std::time::{Duration, Instant};
use tine_plugin_table_formulas::table::Dialect;
use tine_plugin_table_formulas::{process_text, recalc_block, BlockResult, Laps};

fn md(raw: &str) -> (String, Vec<String>) {
    let (out, report) = process_text(raw, Dialect::Markdown, Laps::Times(1)).expect("no abort");
    (out, report.skipped.iter().map(|s| format!("{}: {}", s.formula, s.reason)).collect())
}

/// The block is left byte for byte, with a notice.
fn assert_declined(raw: &str, dialect: Dialect) {
    let (out, report) = process_text(raw, dialect, Laps::Times(1)).expect("no abort");
    assert_eq!(out, raw, "declined input must be kept");
    assert!(!report.skipped.is_empty(), "a decline must say why");
    assert!(report.declined_tables >= 1);
}

/// Run `f` on a thread with a 1 MiB stack (the Wasm guest's default stack
/// size) and a deadline, so a missing budget fails the test instead of
/// hanging or aborting the test binary.
fn bounded<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    let start = Instant::now();
    let h = std::thread::Builder::new().stack_size(1 << 20).spawn(f).expect("thread");
    let v = h.join().expect("no stack overflow or panic");
    assert!(start.elapsed() < Duration::from_secs(5), "took {:?}", start.elapsed());
    v
}

// ---- F2: Markdown has one rule, the delimiter row

#[test]
fn f2_negative_markdown_row_is_data() {
    let raw = "| a | b |\n|---|---|\n|-5|7|\n| 2 | 3 |\n<!-- TBLFM: $2=$1+1 -->";
    let (out, skipped) = md(raw);
    assert!(skipped.is_empty(), "{skipped:?}");
    assert_eq!(out, "|  a |  b |\n| -- | -- |\n| -5 | -4 |\n|  2 |  3 |\n<!-- TBLFM: $2=$1+1 -->");
}

#[test]
fn f2_rule_like_markdown_body_rows_keep_cells_and_row_numbers() {
    let raw = "| a | b | c |\n|---|---|---|\n|---|---|---|\n|-x|y|z|\n<!-- TBLFM: @>$3=@# -->";
    let (out, skipped) = md(raw);
    assert!(skipped.is_empty(), "{skipped:?}");
    assert_eq!(out, "| a   | b   | c   |\n| --- | --- | --- |\n| --- | --- | --- |\n| -x  | y   | 3   |\n<!-- TBLFM: @>$3=@# -->");
}

// ---- F7: Markdown table recognition

#[test]
fn f7_backtick_line_with_text_does_not_close_a_fence() {
    let raw = "```markdown\n```not-a-close\n| a | b |\n|---|---|\n| 1 | 0 |\n<!-- TBLFM: $2=3 -->\n```";
    assert_eq!(recalc_block(raw, Dialect::Markdown).unwrap(), BlockResult::NoTable);
    assert_eq!(md(raw).0, raw);
}

#[test]
fn f7_a_real_closing_fence_ends_the_code() {
    // Fence tracking still ends the code at the real closing fence, so the
    // table after it is found. Since review round 2 (B2) any fence-like line
    // declines the block's tables: the plugin cannot see Tine's parse.
    let raw = "~~~~\ncode\n~~~~~ \n| a | b |\n|---|---|\n| 1 | |\n<!-- TBLFM: $2=$1+1 -->";
    let lines: Vec<&str> = raw.split('\n').collect();
    let found = tine_plugin_table_formulas::table::find_tables(&lines, Dialect::Markdown);
    assert_eq!(found.len(), 1);
    assert_eq!((found[0].start, found[0].end), (3, 6));
    assert!(found[0].decline.as_deref().is_some_and(|w| w.contains("code fence")));
    let (out, skipped) = md(raw);
    assert_eq!(out, raw);
    assert_eq!(skipped.len(), 1);
}

#[test]
fn f7_mixed_optional_leading_pipes_are_declined() {
    assert_declined("a | b\n---|---\n| 2 | 0 |\n<!-- TBLFM: @1$2=9 -->", Dialect::Markdown);
    assert_declined("| a | b |\n|---|---|\n| 1 | 0 |\n2 | 0\n| 3 | 0 |\n<!-- TBLFM: $2=9 -->", Dialect::Markdown);
}

#[test]
fn f7_a_pipe_block_without_header_and_delimiter_is_declined() {
    assert_declined("| a | b |\n| 1 | 0 |\n<!-- TBLFM: $2=3 -->", Dialect::Markdown);
    // An Org rule is not a GFM delimiter row.
    assert_declined("| a | b |\n|---+---|\n| 1 | 0 |\n<!-- TBLFM: $2=3 -->", Dialect::Markdown);
    // Header and delimiter must have as many cells.
    assert_declined("| a | b |\n|---|\n| 1 | 0 |\n<!-- TBLFM: $2=3 -->", Dialect::Markdown);
}

#[test]
fn f7_more_cells_than_the_header_is_declined() {
    assert_declined("| a | b |\n|---|---|\n| 1 | 0 | 9 |\n<!-- TBLFM: $2=3 -->", Dialect::Markdown);
}

#[test]
fn f7_indented_code_block_quote_and_html_are_declined() {
    assert_declined("    | a | b |\n    |---|---|\n    | 1 | 0 |\n<!-- TBLFM: $2=3 -->", Dialect::Markdown);
    assert_declined("> | a | b |\n> |---|---|\n> | 1 | 0 |\n> <!-- TBLFM: $2=3 -->", Dialect::Markdown);
    assert_declined("<div>\n| a | b |\n|---|---|\n| 1 | 0 |\n<!-- TBLFM: $2=3 -->\n</div>", Dialect::Markdown);
}

#[test]
fn f7_tables_inside_html_comments_and_raw_blocks_are_literal() {
    let raw = "<!--\n| a | b |\n|---|---|\n| 1 | 0 |\n<!-- TBLFM: $2=3 -->\n-->";
    assert_eq!(recalc_block(raw, Dialect::Markdown).unwrap(), BlockResult::NoTable);
    let raw = "<pre>\n\n| a | b |\n|---|---|\n| 1 | 0 |\n<!-- TBLFM: $2=3 -->\n</pre>";
    assert_eq!(recalc_block(raw, Dialect::Markdown).unwrap(), BlockResult::NoTable);
}

#[test]
fn f7_a_one_line_comment_above_a_table_is_fine() {
    // Round 1 accepted a one-line comment above a table. Review round 2 (B1)
    // showed an inner comment can hide an enclosing HTML block, so any HTML
    // line other than a formula comment now declines the block; only the
    // formula comment itself is not HTML context.
    let raw = "<!-- note -->\n| a | b |\n|---|---|\n| 1 | |\n<!-- TBLFM: $2=$1+1 -->";
    let (out, skipped) = md(raw);
    assert_eq!(out, raw);
    assert_eq!(skipped.len(), 1, "{skipped:?}");
    let raw = "| a | b |\n|---|---|\n| 1 | |\n<!-- TBLFM: $2=$1+1 -->";
    let (out, skipped) = md(raw);
    assert!(skipped.is_empty(), "{skipped:?}");
    assert!(out.contains("| 1 | 2 |"), "{out}");
}

// ---- F1 in Markdown, and two tables in one block

#[test]
fn f1_markdown_dependent_formula_is_not_run_on_a_stale_cell() {
    assert_declined("| x | r | s |\n|---|---|---|\n| 4 | 9 | 0 |\n<!-- TBLFM: $2=sqrt($1)::$3=$2+1 -->", Dialect::Markdown);
}

#[test]
fn f1_a_declined_table_does_not_stop_another_table() {
    let raw = "| 4 | 9 | 0 |\n#+TBLFM: $2=sqrt($1)::$3=$2+1\n\n| 1 | |\n#+TBLFM: $2=$1+1";
    let (out, report) = process_text(raw, Dialect::Org, Laps::Times(1)).unwrap();
    assert_eq!(out, "| 4 | 9 | 0 |\n#+TBLFM: $2=sqrt($1)::$3=$2+1\n\n| 1 | 2 |\n#+TBLFM: $2=$1+1");
    assert_eq!(report.declined_tables, 1);
}

// ---- F8: the `#+TBLFM:` hint in Markdown is given even when another table recalculates

#[test]
fn f8_org_formula_line_in_markdown_is_reported_alongside_a_recalculated_table() {
    let raw = "| a |\n|---|\n| 1 |\n#+TBLFM: $1=2\n\n| b | c |\n|---|---|\n| 1 | |\n<!-- TBLFM: $2=$1+1 -->";
    let (_, report) = process_text(raw, Dialect::Markdown, Laps::Times(1)).unwrap();
    assert!(report.org_line_in_markdown);
}

// ---- F9: budgets

#[test]
fn f9_huge_relative_row_offset_stops_at_once() {
    let r = bounded(|| process_text("| 1 |\n#+TBLFM: $1=@+1000000000000000000$1", Dialect::Org, Laps::Times(1)));
    assert!(r.is_err(), "Org reports a row outside the table: {r:?}");
}

#[test]
fn f9_deep_nesting_is_declined_not_a_stack_overflow() {
    let deep = format!("| 0 |\n#+TBLFM: $1={}1{}", "(".repeat(12_000), ")".repeat(12_000));
    let (out, report) = bounded(move || process_text(&deep, Dialect::Org, Laps::Times(1)).unwrap());
    assert!(!report.skipped.is_empty());
    assert!(out.starts_with("| 0 |"));
    // A long left-nested chain grows the tree without parser recursion.
    let chain = format!("| 0 |\n#+TBLFM: $1=1{}", "+1".repeat(20_000));
    let (_, report) = bounded(move || process_text(&chain, Dialect::Org, Laps::Times(1)).unwrap());
    assert!(!report.skipped.is_empty());
}

#[test]
fn f9_huge_right_hand_range_is_declined_before_allocating() {
    let (_, report) =
        bounded(|| process_text("| 1 |\n| 2 |\n#+TBLFM: @1$1=vsum(@1$1..@2$1000000000)", Dialect::Org, Laps::Times(1)).unwrap());
    assert!(!report.skipped.is_empty());
}

#[test]
fn f9_cumulative_references_are_capped() {
    // 400 rows, each summing the whole column: 160,000 field reads, about
    // 400 ms in the release guest, past the host's 250 ms timeout.
    let mut raw = String::new();
    for i in 0..400 {
        raw.push_str(&format!("| {i} | |\n"));
    }
    raw.push_str("#+TBLFM: $2=vsum(@1$1..@>$1)");
    let (out, report) = bounded(move || {
        let r = process_text(&raw, Dialect::Org, Laps::Times(1)).unwrap();
        (r.0 == raw, r.1)
    });
    assert!(out, "declined table kept");
    assert!(report.skipped.iter().any(|s| s.reason.contains("computation")), "{:?}", report.skipped);
}

#[test]
fn f9_a_modest_table_stays_within_the_work_budget() {
    // 150 rows, each summing the column: 22,500 reads plus 150 evaluations.
    let mut raw = String::new();
    for i in 0..150 {
        raw.push_str(&format!("| {i} | |\n"));
    }
    raw.push_str("#+TBLFM: $2=vsum(@1$1..@>$1)");
    let (out, report) = process_text(&raw, Dialect::Org, Laps::Times(1)).unwrap();
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);
    assert!(out.contains("| 11175 |"), "{}", &out[..200]);
}

#[test]
fn f9_integer_growth_is_capped() {
    let (_, report) =
        bounded(|| process_text("| 0 |\n#+TBLFM: $1=((2^512)^512)^512", Dialect::Org, Laps::Times(1)).unwrap());
    assert!(!report.skipped.is_empty());
}

#[test]
fn f9_padding_a_compact_ragged_table_is_refused_before_rendering() {
    let mut raw = format!("| {} |\n", "x".repeat(20_000));
    for _ in 0..3000 {
        raw.push_str("|\n");
    }
    raw.push_str("#+TBLFM: $2=1");
    let r = bounded(move || process_text(&raw, Dialect::Org, Laps::Times(1)));
    let e = r.expect_err("refused");
    assert!(e.message.contains("128 KiB"), "{e:?}");
}

// ---- F6 / F3: printf against Emacs's `format`

/// `fixtures/emacs-printf.json` samples a grid of 86,025 format/value pairs
/// (13 flag sets x 4 widths x 6 precisions x 11 conversions x 25 values,
/// plus literal-text formats) run through Emacs 31.1 by
/// `tools/emacs-printf-oracle.el`. On the full grid the plugin matched
/// 29,179 pairs exactly and declined the other 56,846 (round 1). Every
/// sampled pair must match or be declined.
#[test]
fn f6_printf_matches_emacs_or_declines() {
    use tine_plugin_table_formulas::printf::{format_one, string_to_number};
    let path = format!("{}/tests/fixtures/emacs-printf.json", env!("CARGO_MANIFEST_DIR"));
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let (mut matched, mut declined, mut wrong) = (0, 0, Vec::new());
    for c in v["cases"].as_array().unwrap() {
        let (fmt, text) = (c["format"].as_str().unwrap(), c["text"].as_str().unwrap());
        let got = string_to_number(text).map(|n| format_one(fmt, &n));
        match (got, c["emacs"].as_str()) {
            (Some(Ok(s)), Some(e)) if s == e => matched += 1,
            (None, _) | (Some(Err(_)), _) => declined += 1,
            (Some(Ok(s)), e) => wrong.push(format!("{fmt:?} of {text:?}: plugin {s:?}, Emacs {e:?}")),
        }
    }
    println!("printf oracle: {matched} match, {declined} declined");
    assert!(wrong.is_empty(), "{} wrong:\n{}", wrong.len(), wrong.join("\n"));
    assert!(matched >= 3000, "only {matched} accepted cases");
}
