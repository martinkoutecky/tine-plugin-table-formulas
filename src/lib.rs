//! Tine plugin: Org table formulas (`#+TBLFM:` in Org, `<!-- TBLFM: ... -->`
//! in Markdown) recalculated in the focused block.

pub mod calc;
pub mod formula;
pub mod printf;
pub mod table;

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
    Changed { raw: String, tables: usize, report: Report },
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
    let lines: Vec<&str> = raw.split('\n').collect();
    let bare: Vec<&str> = lines.iter().map(|l| l.strip_suffix('\r').unwrap_or(l)).collect();
    let found = table::find_tables(&bare, dialect);
    if found.is_empty() {
        return Ok(BlockResult::NoTable);
    }
    if !found.iter().any(|f| f.tblfm.is_some()) {
        return Ok(BlockResult::NoFormulas {
            org_line_in_markdown: found.iter().any(|f| f.md_has_org_tblfm),
        });
    }
    let tables = found.iter().filter(|f| f.tblfm.is_some()).count();
    let (new_raw, report) = process_text(raw, dialect, Laps::Times(1))?;
    if new_raw == raw {
        Ok(BlockResult::Unchanged { tables, report })
    } else {
        Ok(BlockResult::Changed { raw: new_raw, tables, report })
    }
}

/// The text with its formula tables recalculated (`laps`) and aligned.
pub fn process_text(raw: &str, dialect: Dialect, laps: Laps) -> Result<(String, Report), Abort> {
    let lines: Vec<&str> = raw.split('\n').collect();
    let bare: Vec<&str> = lines.iter().map(|l| l.strip_suffix('\r').unwrap_or(l)).collect();
    let found = table::find_tables(&bare, dialect);
    let mut report = Report::default();
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut next = 0;
    for f in found.iter().filter(|f| f.tblfm.is_some() || laps == Laps::AlignOnly) {
        out.extend(lines[next..f.start].iter().map(|l| l.to_string()));
        let mut t: Table = table::parse_table(&bare[f.start..f.end], dialect);
        if laps != Laps::AlignOnly {
            let formulas = formula::parse_tblfm(f.tblfm.as_deref().unwrap_or(""))?;
            let passes = match laps {
                Laps::Times(n) => n,
                _ => 10,
            };
            let mut converged = laps != Laps::Iterate;
            let mut last = Report::default();
            for _ in 0..passes {
                let before = t.lines.clone();
                last = formula::recalculate(&mut t, &formulas)?;
                if laps == Laps::Iterate && t.lines == before {
                    converged = true;
                    break;
                }
            }
            report.skipped.extend(last.skipped);
            report.errors.extend(last.errors);
            if !converged {
                return Err(Abort { formula: None, message: "no convergence after 10 iterations".into() });
            }
        }
        let cr = lines[f.start..f.end].iter().any(|l| l.ends_with('\r'));
        for l in t.render() {
            out.push(if cr { format!("{l}\r") } else { l });
        }
        next = f.end;
    }
    out.extend(lines[next..].iter().map(|l| l.to_string()));
    Ok((out.join("\n"), report))
}

fn clip(mut s: String) -> String {
    if s.chars().count() > MAX_NOTICE {
        s = s.chars().take(MAX_NOTICE - 1).collect::<String>() + "…";
    }
    s
}

fn notice(message: String, level: NoticeLevel) -> Effect {
    Effect::Notice { message: clip(message), level: Some(level) }
}

fn report_notice(report: &Report, tables: usize, changed: bool) -> Option<Effect> {
    if report.errors.is_empty() && report.skipped.is_empty() {
        return if changed {
            None
        } else {
            Some(notice(
                if tables == 1 { "Table is up to date.".into() } else { "Tables are up to date.".into() },
                NoticeLevel::Info,
            ))
        };
    }
    let mut msg = String::new();
    if !report.errors.is_empty() {
        let list: Vec<String> = report.errors.iter().map(|e| format!("`{e}`")).collect();
        msg.push_str(&format!("#ERROR from {}. ", list.join(", ")));
    }
    if !report.skipped.is_empty() {
        let list: Vec<String> =
            report.skipped.iter().map(|s| format!("`{}` ({})", s.formula, s.reason)).collect();
        msg.push_str(&format!(
            "Not evaluated, cells kept: {}. Cells that depend on them may be stale.",
            list.join("; ")
        ));
    }
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
    let dialect = if block.format == Some(BlockFormat::Org) { Dialect::Org } else { Dialect::Markdown };
    let result = recalc_block(&block.raw, dialect).map_err(|a| {
        clip(match a.formula {
            Some(f) => format!("Table formula `{f}`: {}. Nothing was changed.", a.message),
            None => format!("Table formulas: {}. Nothing was changed.", a.message),
        })
    })?;
    match result {
        BlockResult::NoTable => Ok(vec![notice("This block has no table.".into(), NoticeLevel::Info)]),
        BlockResult::NoFormulas { org_line_in_markdown } => {
            let hint = match dialect {
                Dialect::Org => "Add a `#+TBLFM: $3=$1*$2` line under the table.",
                Dialect::Markdown if org_line_in_markdown => {
                    "In Markdown, write the formulas as `<!-- TBLFM: $3=$1*$2 -->` under the table (the `#+TBLFM:` line is not read)."
                }
                Dialect::Markdown => "Add a `<!-- TBLFM: $3=$1*$2 -->` line under the table.",
            };
            Ok(vec![notice(format!("No table formulas in this block. {hint}"), NoticeLevel::Info)])
        }
        BlockResult::Unchanged { tables, report } => Ok(report_notice(&report, tables, false).into_iter().collect()),
        BlockResult::Changed { raw, tables, report } => {
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
        assert!(matches!(recalc_block(&out, Dialect::Markdown).unwrap(), BlockResult::Unchanged { .. }));
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
        assert_eq!(changed(raw, Dialect::Org), "| x |\r\n\r\n| 1 | 2 |\r\n#+TBLFM: $2=$1+1\r\ntail");
    }
}
