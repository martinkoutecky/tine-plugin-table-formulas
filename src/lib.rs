//! Tine plugin: Org table formulas (`#+TBLFM:` in Org, `<!-- TBLFM: ... -->`
//! in Markdown) recalculated in the focused block.

pub mod calc;
pub mod formula;
pub mod printf;
pub mod table;
pub mod work;

use formula::{Abort, Report};
use table::{Dialect, Table};
use tine_plugin_sdk::{BlockFormat, Effect, Event, NoticeLevel};

/// The host refuses a replacement longer than this (UTF-16 code units).
pub const MAX_RAW_UTF16: usize = 128 * 1024;
/// The host refuses a longer notice.
const MAX_NOTICE: usize = 500;

#[derive(Debug, PartialEq)]
pub enum BlockResult {
    /// No pipe table in the block.
    NoTable,
    /// Tables, but none has a formula line. `org_line_in_markdown` is set when
    /// a Markdown table is followed by `#+TBLFM:` instead of the comment form.
    NoFormulas { org_line_in_markdown: bool },
    /// Recalculated; the text did not change.
    Unchanged { tables: usize, report: Report },
    Changed {
        raw: String,
        tables: usize,
        report: Report,
    },
}

/// How many recalculation passes to run before aligning.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Laps {
    /// Only align (Org's `org-table-align`), for tables with or without formulas.
    AlignOnly,
    /// `org-table-recalculate 'all` this many times.
    Times(u32),
    /// `org-table-iterate`: until the table no longer changes, at most 10 passes.
    Iterate,
}

/// Recalculate every table of `raw` that has a formula line, and align it.
/// Lines outside those tables are kept byte for byte.
pub fn recalc_block(raw: &str, dialect: Dialect) -> Result<BlockResult, Abort> {
    let _run = work::Run::start();
    let lines: Vec<&str> = raw.split('\n').collect();
    let bare: Vec<&str> = lines
        .iter()
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .collect();
    let found = find(raw, &bare, dialect);
    if found.is_empty() {
        return Ok(BlockResult::NoTable);
    }
    if !found.iter().any(|f| f.tblfm.is_some()) {
        return Ok(BlockResult::NoFormulas {
            org_line_in_markdown: found.iter().any(|f| f.md_has_org_tblfm),
        });
    }
    let tables = found.iter().filter(|f| f.tblfm.is_some()).count();
    let (new_raw, report) = process_found(raw, &lines, &bare, &found, dialect, Laps::Times(1))?;
    if new_raw == raw {
        Ok(BlockResult::Unchanged { tables, report })
    } else {
        Ok(BlockResult::Changed {
            raw: new_raw,
            tables,
            report,
        })
    }
}

/// The text with its formula tables recalculated (`laps`) and aligned.
///
/// A table with a formula the plugin cannot evaluate exactly is left byte
/// for byte as it was (not even aligned), and the report names the formula:
/// a partial recalculation could write values Org would not (review F1).
pub fn process_text(raw: &str, dialect: Dialect, laps: Laps) -> Result<(String, Report), Abort> {
    // One work budget for the whole command: every table, every pass.
    let _run = work::Run::start();
    let lines: Vec<&str> = raw.split('\n').collect();
    let bare: Vec<&str> = lines
        .iter()
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .collect();
    let found = find(raw, &bare, dialect);
    process_found(raw, &lines, &bare, &found, dialect, laps)
}

/// Split the block and find its tables, charged: a few regex passes per
/// line (table, fence, HTML and formula-line checks).
fn find(raw: &str, bare: &[&str], dialect: Dialect) -> Vec<table::Found> {
    let _ = work::ticks(
        (bare.len() as u64)
            .saturating_mul(8 * work::scan_ticks(0))
            .saturating_add(8 * raw.len() as u64),
    );
    table::find_tables(bare, dialect)
}

/// The tables in `found` processed. Once the run's work budget is spent,
/// every remaining table is declined and kept byte for byte (malformed
/// input bound: a block that would run the guest past the host's
/// invocation timeout).
fn process_found(
    raw: &str,
    lines: &[&str],
    bare: &[&str],
    found: &[table::Found],
    dialect: Dialect,
    laps: Laps,
) -> Result<(String, Report), Abort> {
    let mut report = Report {
        org_line_in_markdown: found.iter().any(|f| f.md_has_org_tblfm),
        ..Report::default()
    };
    let _ = work::ticks(16 * lines.len() as u64 + raw.len() as u64);
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut next = 0;
    for f in found
        .iter()
        .filter(|f| f.tblfm.is_some() || laps == Laps::AlignOnly)
    {
        out.extend(lines[next..f.start].iter().map(|l| l.to_string()));
        next = f.end;
        let original = || lines[f.start..f.end].iter().map(|l| l.to_string());
        let over_budget = |report: &mut Report| {
            report.skip(
                f.tblfm.as_deref().unwrap_or("(alignment)"),
                formula::WORK_REASON,
            );
            report.declined_tables += 1;
        };
        if let Some(why) = &f.decline {
            if let Some(tblfm) = &f.tblfm {
                report.skip(tblfm, why);
                report.declined_tables += 1;
            }
            out.extend(original());
            continue;
        }
        let src = &bare[f.start..f.end];
        let bytes: usize = src.iter().map(|l| l.len()).sum();
        if !work::ticks(
            64 * src.len() as u64 + 2 * work::scan_ticks(0) * src.len() as u64 + 4 * bytes as u64,
        ) {
            over_budget(&mut report);
            out.extend(original());
            continue;
        }
        let mut t: Table = table::parse_table(src, dialect);
        if laps != Laps::AlignOnly {
            let formulas = formula::parse_tblfm(f.tblfm.as_deref().unwrap_or(""))?;
            if work::exhausted() {
                over_budget(&mut report);
                out.extend(original());
                continue;
            }
            let passes = match laps {
                Laps::Times(n) => n,
                _ => 10,
            };
            let mut converged = laps != Laps::Iterate;
            let mut last = Report::default();
            for _ in 0..passes {
                // The copy and the comparison after the pass.
                let _ = work::ticks(2 * table_ticks(&t));
                let before = t.lines.clone();
                last = formula::recalculate_in(&mut t, &formulas)?;
                if !last.skipped.is_empty() {
                    break;
                }
                if laps == Laps::Iterate && t.lines == before {
                    converged = true;
                    break;
                }
            }
            if !last.skipped.is_empty() {
                report.skipped.extend(last.skipped);
                report.declined_tables += 1;
                out.extend(original());
                continue;
            }
            report.errors.extend(last.errors);
            if !converged {
                return Err(Abort {
                    formula: None,
                    message: "no convergence after 10 iterations".into(),
                });
            }
        }
        if !work::ticks(t.render_ticks()) {
            over_budget(&mut report);
            out.extend(original());
            continue;
        }
        let rendered = t.render_checked(MAX_RAW_UTF16).ok_or_else(|| Abort {
            formula: None,
            message: "the aligned table would exceed Tine's 128 KiB block limit".into(),
        })?;
        // The aligned table must read back as the cells it was built from
        // (malformed input: a cell whose text would change how its row
        // splits, which would corrupt the table on write).
        // Finding and parsing it again: a few regex passes per line, one
        // pass over each character.
        let size: u64 = rendered.iter().map(|l| l.len() as u64 + 1).sum();
        if !work::ticks(rendered.len() as u64 * 10 * work::scan_ticks(0) + 8 * size) {
            over_budget(&mut report);
            out.extend(original());
            continue;
        }
        if !t.reads_back(&rendered) {
            report.skip(
                f.tblfm.as_deref().unwrap_or("(alignment)"),
                "the aligned table would not read back as the same cells",
            );
            report.declined_tables += 1;
            out.extend(original());
            continue;
        }
        let cr = lines[f.start..f.end].iter().any(|l| l.ends_with('\r'));
        for l in rendered {
            out.push(if cr { format!("{l}\r") } else { l });
        }
    }
    out.extend(lines[next..].iter().map(|l| l.to_string()));
    let size: usize = out.iter().map(|l| l.len() + 1).sum();
    let _ = work::text(2 * size);
    report.work_used = work::used_units();
    Ok((out.join("\n"), report))
}

/// Ticks to copy or compare a table's cells.
fn table_ticks(t: &Table) -> u64 {
    t.lines
        .iter()
        .map(|l| match l {
            table::Line::Data(f) => 16 + f.iter().map(|c| 16 + c.len() as u64 / 2).sum::<u64>(),
            table::Line::Hline => 16,
        })
        .sum()
}

fn clip(mut s: String) -> String {
    if s.chars().count() > MAX_NOTICE {
        s = s.chars().take(MAX_NOTICE - 1).collect::<String>() + "…";
    }
    s
}

fn notice(message: String, level: NoticeLevel) -> Effect {
    Effect::Notice {
        message: clip(message),
        level: Some(level),
    }
}

const ORG_LINE_HINT: &str =
    "In Markdown, write the formulas as `<!-- TBLFM: $3=$1*$2 -->` under the table (the `#+TBLFM:` line is not read).";

fn report_notice(report: &Report, tables: usize, changed: bool) -> Option<Effect> {
    let hint = if report.org_line_in_markdown {
        format!(" {ORG_LINE_HINT}")
    } else {
        String::new()
    };
    if report.errors.is_empty() && report.skipped.is_empty() {
        return if changed && hint.is_empty() {
            None
        } else if changed {
            Some(notice(hint.trim_start().to_string(), NoticeLevel::Info))
        } else {
            let up = if tables == 1 {
                "Table is up to date."
            } else {
                "Tables are up to date."
            };
            Some(notice(format!("{up}{hint}"), NoticeLevel::Info))
        };
    }
    let mut msg = String::new();
    if !report.skipped.is_empty() {
        let list: Vec<String> = report
            .skipped
            .iter()
            .map(|s| format!("`{}` ({})", s.formula, s.reason))
            .collect();
        let what = if report.declined_tables == 1 {
            "1 table was left unchanged".to_string()
        } else {
            format!("{} tables were left unchanged", report.declined_tables)
        };
        msg.push_str(&format!(
            "{what}, because the plugin cannot match Org on {}. ",
            list.join("; ")
        ));
    }
    if !report.errors.is_empty() {
        let list: Vec<String> = report.errors.iter().map(|e| format!("`{e}`")).collect();
        msg.push_str(&format!("#ERROR from {}. ", list.join(", ")));
    }
    msg.push_str(hint.trim_start());
    Some(notice(msg.trim_end().to_string(), NoticeLevel::Warning))
}

fn handle(event: &Event) -> Result<Vec<Effect>, String> {
    if event.kind != "command" || event.contribution_id.as_deref() != Some("recalculate") {
        return Ok(Vec::new());
    }
    let block = event
        .focused_block
        .as_ref()
        .ok_or_else(|| "Put the cursor in a block with a table, then recalculate.".to_string())?;
    let dialect = if block.format == Some(BlockFormat::Org) {
        Dialect::Org
    } else {
        Dialect::Markdown
    };
    let result = recalc_block(&block.raw, dialect).map_err(|a| {
        clip(match a.formula {
            Some(f) => format!("Table formula `{f}`: {}. Nothing was changed.", a.message),
            None => format!("Table formulas: {}. Nothing was changed.", a.message),
        })
    })?;
    match result {
        BlockResult::NoTable => Ok(vec![notice(
            "This block has no table.".into(),
            NoticeLevel::Info,
        )]),
        BlockResult::NoFormulas {
            org_line_in_markdown,
        } => {
            let hint = match dialect {
                Dialect::Org => "Add a `#+TBLFM: $3=$1*$2` line under the table.",
                Dialect::Markdown if org_line_in_markdown => ORG_LINE_HINT,
                Dialect::Markdown => "Add a `<!-- TBLFM: $3=$1*$2 -->` line under the table.",
            };
            Ok(vec![notice(
                format!("No table formulas in this block. {hint}"),
                NoticeLevel::Info,
            )])
        }
        BlockResult::Unchanged { tables, report } => {
            Ok(report_notice(&report, tables, false).into_iter().collect())
        }
        BlockResult::Changed {
            raw,
            tables,
            report,
        } => {
            if raw.encode_utf16().count() > MAX_RAW_UTF16 {
                // Refused: Tine would reject the oversized replacement anyway
                // (malformed input bound); nothing is written.
                return Err("The recalculated block would exceed Tine's 128 KiB block limit. Nothing was changed.".into());
            }
            let mut effects = vec![Effect::ReplaceBlockText {
                block_id: block.id.clone(),
                expected_raw: block.raw.clone(),
                raw,
            }];
            effects.extend(report_notice(&report, tables, true));
            Ok(effects)
        }
    }
}

tine_plugin_sdk::tine_plugin!(handle);

#[cfg(test)]
mod tests {
    use super::*;

    fn changed(raw: &str, d: Dialect) -> String {
        match recalc_block(raw, d).unwrap() {
            BlockResult::Changed { raw, .. } => raw,
            other => panic!("expected a change, got {other:?}"),
        }
    }

    #[test]
    fn markdown_block_keeps_text_outside_the_table() {
        let raw = "Prices **today**\n| item | n | price | total |\n|---|---|---|---|\n| a | 2 | 3 | |\n| b | 4 | 0.5 | |\n<!-- TBLFM: $4=$2*$3 -->\nid:: 123";
        let out = changed(raw, Dialect::Markdown);
        assert_eq!(
            out,
            "Prices **today**\n| item | n | price | total |\n| ---- | - | ----- | ----- |\n| a    | 2 |     3 |     6 |\n| b    | 4 |   0.5 |    2. |\n<!-- TBLFM: $4=$2*$3 -->\nid:: 123"
        );
        // Idempotent.
        assert!(matches!(
            recalc_block(&out, Dialect::Markdown).unwrap(),
            BlockResult::Unchanged { .. }
        ));
    }

    #[test]
    fn org_block_sums_a_column() {
        let raw = "| a | 1 |\n| b | 2 |\n|---+---|\n| sum | |\n#+TBLFM: @>$2=vsum(@1..@-1)";
        assert_eq!(
            changed(raw, Dialect::Org),
            "| a   | 1 |\n| b   | 2 |\n|-----+---|\n| sum | 3 |\n#+TBLFM: @>$2=vsum(@1..@-1)"
        );
    }

    #[test]
    fn abort_writes_nothing() {
        let raw = "| 1 |\n#+TBLFM: @5$1=1";
        let e = recalc_block(raw, Dialect::Org).unwrap_err();
        assert!(e.message.contains("invalid row"), "{e:?}");
    }

    #[test]
    fn crlf_and_tables_without_formulas_are_kept() {
        let raw = "| x |\r\n\r\n| 1 | |\r\n#+TBLFM: $2=$1+1\r\ntail";
        assert_eq!(
            changed(raw, Dialect::Org),
            "| x |\r\n\r\n| 1 | 2 |\r\n#+TBLFM: $2=$1+1\r\ntail"
        );
    }
}
