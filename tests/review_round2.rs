//! Regression cases from the release review of 2026-10-20, round 2
//! (blockers B1-B3 and the row-count cast follow-up), with the reviewer's
//! exact inputs. The plugin cannot see Tine's parse, so a block whose
//! Markdown structure it cannot be sure of is declined whole.

use std::sync::mpsc;
use std::time::Duration;
use tine_plugin_table_formulas::table::Dialect;
use tine_plugin_table_formulas::{process_text, recalc_block, BlockResult, Laps};

/// The block is left byte for byte, with a notice.
fn assert_declined(raw: &str, dialect: Dialect) {
    let (out, report) = process_text(raw, dialect, Laps::Times(1)).expect("no abort");
    assert_eq!(out, raw, "declined input must be kept");
    assert!(!report.skipped.is_empty(), "a decline must say why");
    assert!(report.declined_tables >= 1);
}

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

// ---- B1: an HTML block anywhere in the block hides the table's context

#[test]
fn b1_one_line_comment_inside_a_div_does_not_reset_html_context() {
    // Tine (lsdoc v0.5.10) parses this whole block as one raw_html node.
    assert_declined(
        "<div>\n<!-- note -->\n| a | b |\n|---|---|\n| 1 | 0 |\n<!-- TBLFM: $2=3 -->\n</div>",
        Dialect::Markdown,
    );
}

#[test]
fn b1_any_html_tag_line_in_the_block_declines_every_table() {
    assert_declined(
        "| a | b |\n|---|---|\n| 1 | 0 |\n<!-- TBLFM: $2=3 -->\n\n</div>",
        Dialect::Markdown,
    );
    assert_declined(
        "<!-- note -->\n| a | b |\n|---|---|\n| 1 | 0 |\n<!-- TBLFM: $2=3 -->",
        Dialect::Markdown,
    );
    assert_declined(
        "\t<span>x</span>\n\n| a | b |\n|---|---|\n| 1 | 0 |\n<!-- TBLFM: $2=3 -->",
        Dialect::Markdown,
    );
}

#[test]
fn b1_the_formula_comment_alone_is_not_html_context() {
    let raw = "| a | b |\n|---|---|\n| 1 | 0 |\n<!-- TBLFM: $2=3 -->";
    let (out, report) = process_text(raw, Dialect::Markdown, Laps::Times(1)).unwrap();
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);
    assert!(out.contains("| 1 | 3 |"), "{out}");
}

// ---- B2: a fence-like line anywhere makes the block uncertain

#[test]
fn b2_tab_indented_fence_in_a_list_is_declined() {
    // Tine parses the pipe lines as a `src` node; the plugin must not edit them.
    let raw = "- example:\n\n\t```\n| a | b |\n|---|---|\n| 1 | 0 |\n<!-- TBLFM: $2=3 -->\n\t```";
    assert_declined(raw, Dialect::Markdown);
}

#[test]
fn b2_over_indented_and_tilde_fences_are_declined() {
    assert_declined(
        "    ~~~\n| a | b |\n|---|---|\n| 1 | 0 |\n<!-- TBLFM: $2=3 -->\n    ~~~",
        Dialect::Markdown,
    );
    assert_declined(
        "| a | b |\n|---|---|\n| 1 | 0 |\n<!-- TBLFM: $2=3 -->\n\n  ```js\n  x\n  ```",
        Dialect::Markdown,
    );
    assert_declined("\t```\n| 1 | 0 |\n#+TBLFM: $2=3\n\t```", Dialect::Org);
}

#[test]
fn b2_a_table_only_inside_a_closed_fence_is_still_literal() {
    let raw = "```\n| a | b |\n|---|---|\n| 1 | 0 |\n<!-- TBLFM: $2=3 -->\n```";
    assert_eq!(
        recalc_block(raw, Dialect::Markdown).unwrap(),
        BlockResult::NoTable
    );
}

// ---- B3: an over-long descriptor never turns into a valid reference

fn padded() -> String {
    "0".repeat(2000) + "1"
}

/// Org 9.8.7 (Emacs 31.1) reads the padded descriptor as column 1 and writes 8
/// (`/tmp/table-formulas-review/r2/descriptors-org.json`). Matching that or
/// declining are both acceptable; writing 10 (column 0 = current) is not.
fn assert_org_or_declined(raw: &str, org: &str) {
    let (out, report) = process_text(raw, Dialect::Org, Laps::Times(1)).expect("no abort");
    if out == raw {
        assert!(!report.skipped.is_empty(), "a decline must say why");
    } else {
        assert_eq!(out, org);
    }
}

#[test]
fn b3_zero_padded_column_in_a_field_range() {
    let raw = format!("| 7 | 9 |\n#+TBLFM: @1$2=@1${}+1\n", padded());
    let org = format!("| 7 | 8 |\n#+TBLFM: @1$2=@1${}+1\n", padded());
    assert_org_or_declined(&raw, &org);
}

#[test]
fn b3_zero_padded_column_in_the_current_row() {
    let raw = format!("| 7 | 9 |\n#+TBLFM: $2=${}+1\n", padded());
    let org = format!("| 7 | 8 |\n#+TBLFM: $2=${}+1\n", padded());
    assert_org_or_declined(&raw, &org);
}

#[test]
fn b3_zero_padded_column_ranges_and_lhs() {
    let p = padded();
    let raw = format!("| 7 | 9 | 0 |\n#+TBLFM: $3=vsum(${p}..$2)\n");
    assert_org_or_declined(
        &raw,
        &format!("| 7 | 9 | 16 |\n#+TBLFM: $3=vsum(${p}..$2)\n"),
    );
    let raw = format!("| 7 | 9 |\n#+TBLFM: ${}2=$1+1\n", "0".repeat(2000));
    assert_org_or_declined(
        &raw,
        &format!("| 7 | 8 |\n#+TBLFM: ${}2=$1+1\n", "0".repeat(2000)),
    );
}

/// The B3 descriptor cases computed by Emacs 31.1 / Org 9.8.7 with
/// `tools/emacs-oracle.el` (2026-10-20). They are stored as templates to keep
/// the repository small: `<P>` stands for 2,000 zeros then `1`, `<Z>` for
/// 2,000 zeros. Expand them to rerun the oracle. The plugin must write
/// exactly what Org wrote.
const PADDED_DESCRIPTORS_EMACS: &[(&str, &str, &str)] = &[
    (
        "r2-b3-range",
        "| 7 | 9 |\n#+TBLFM: @1$2=@1$<P>+1\n",
        "| 7 | 8 |\n#+TBLFM: @1$2=@1$<P>+1\n",
    ),
    (
        "r2-b3-rhs",
        "| 7 | 9 |\n#+TBLFM: $2=$<P>+1\n",
        "| 7 | 8 |\n#+TBLFM: $2=$<P>+1\n",
    ),
    (
        "r2-b3-vsum",
        "| 7 | 9 | 0 |\n#+TBLFM: $3=vsum($<P>..$2)\n",
        "| 7 | 9 | 16 |\n#+TBLFM: $3=vsum($<P>..$2)\n",
    ),
    (
        "r2-b3-lhs",
        "| 7 | 9 |\n#+TBLFM: $<Z>2=$1+1\n",
        "| 7 | 8 |\n#+TBLFM: $<Z>2=$1+1\n",
    ),
    (
        "r2-b3-rowrange",
        "| 7 | 9 |\n| 1 | 2 |\n#+TBLFM: @2$1=@<P>$1+1\n",
        "| 7 | 9 |\n| 8 | 2 |\n#+TBLFM: @2$1=@<P>$1+1\n",
    ),
    (
        "r2-b3-mode",
        "| 7 | 9 |\n#+TBLFM: $2=$1/3;f<Z>2\n",
        "| 7 | 2.33 |\n#+TBLFM: $2=$1/3;f<Z>2\n",
    ),
    (
        "r2-b3-relcol",
        "| 7 | 9 |\n#+TBLFM: $2=$-<P>+1\n",
        "| 7 | 8 |\n#+TBLFM: $2=$-<P>+1\n",
    ),
];

#[test]
fn b3_padded_descriptors_match_emacs() {
    let expand = |s: &str| {
        s.replace("<P>", &padded())
            .replace("<Z>", &"0".repeat(2000))
    };
    for (id, input, org) in PADDED_DESCRIPTORS_EMACS {
        let (input, org) = (expand(input), expand(org));
        let (out, report) = process_text(&input, Dialect::Org, Laps::Times(1)).expect("no abort");
        assert!(report.skipped.is_empty(), "{id}: {:?}", report.skipped);
        assert_eq!(out, org, "{id}");
    }
}

// ---- Follow-up: row counts at or past 2^63 must not wrap (native builds)

#[test]
fn row_counts_past_i64_stop_at_once() {
    for f in [
        "@+9223372036854775808$1",
        "@+18446744073709551616$1",
        "@-9223372036854775808$1",
    ] {
        let raw = format!("| 1 |\n#+TBLFM: $1={f}");
        let r = within(5, move || process_text(&raw, Dialect::Org, Laps::Times(1)));
        assert!(
            r.is_err(),
            "{f}: Org reports the descriptor as leading outside the table"
        );
    }
}
