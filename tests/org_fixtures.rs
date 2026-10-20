//! The org-mode test corpus as an oracle.
//!
//! `fixtures/org-table-tests.json` is extracted mechanically from org-mode's
//! `testing/lisp/test-org-table.el` (see `tools/extract-org-fixtures.py`);
//! `fixtures/org-table-tests-manual.json` holds the other formula tests of
//! that file, transcribed by hand. Every case must either produce exactly
//! the text Org produces, or be listed in `fixtures/expectations.json` as
//! `declined` (and then the plugin must have said so in its report, and every
//! cell it did write must equal Org's) or `abort` (Org stops with an error;
//! the plugin must stop and write nothing).
//!
//! Since review round 1 (finding F1), a declined case must leave the input
//! byte for byte unchanged: one formula the plugin cannot evaluate keeps the
//! whole table, because later formulas could read the cell Org would have
//! changed. A declined case may also be one where Org aborts and the plugin
//! declines instead (both write nothing).

use serde_json::Value;
use tine_plugin_table_formulas::table::{split_row, Dialect};
use tine_plugin_table_formulas::{process_text, Laps};

fn load(name: &str) -> Value {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(&path).expect("fixture file")).expect("fixture json")
}

fn laps_of(v: &Value) -> Laps {
    match v {
        Value::Null => Laps::AlignOnly,
        Value::Number(n) => Laps::Times(n.as_u64().unwrap_or(1) as u32),
        Value::String(s) if s == "iterate" => Laps::Iterate,
        other => panic!("bad laps {other:?}"),
    }
}

fn data_rows(text: &str) -> Vec<Vec<String>> {
    text.split('\n')
        .filter(|l| {
            let t = l.trim_start();
            t.starts_with('|') && !t.starts_with("|-")
        })
        .map(|l| split_row(l, Dialect::Org))
        .collect()
}

/// Every cell the plugin changed must hold Org's value.
fn no_wrong_writes(input: &str, out: &str, expected: &str) -> Result<(), String> {
    let (i, o, e) = (data_rows(input), data_rows(out), data_rows(expected));
    if i.len() != o.len() || o.len() != e.len() {
        return Err(format!("row counts differ: input {}, out {}, expected {}", i.len(), o.len(), e.len()));
    }
    for r in 0..o.len() {
        let width = o[r].len().max(e[r].len()).max(i[r].len());
        for c in 0..width {
            let get = |v: &Vec<String>| v.get(c).cloned().unwrap_or_default();
            let (iv, ov, ev) = (get(&i[r]), get(&o[r]), get(&e[r]));
            if ov != ev && ov != iv {
                return Err(format!("row {} col {}: wrote `{ov}`, Org has `{ev}` (was `{iv}`)", r + 1, c + 1));
            }
        }
    }
    Ok(())
}

#[test]
fn org_mode_corpus() {
    let expectations = load("expectations.json");
    let mut cases: Vec<Value> = Vec::new();
    let mut revisions = Vec::new();
    for file in ["org-table-tests.json", "org-table-tests-manual.json", "emacs-oracle.json", "emacs-oracle-r1.json", "emacs-oracle-r2.json"] {
        let v = load(file);
        revisions.push(v["revision"].as_str().unwrap_or("?").to_string());
        cases.extend(v["cases"].as_array().cloned().unwrap_or_default());
    }
    let (mut matched, mut declined, mut aborted) = (0, 0, 0);
    let mut failures = Vec::new();
    for case in &cases {
        let id = case["id"].as_str().unwrap_or("?");
        let input = case["input"].as_str().unwrap_or("");
        let expect = &expectations[id];
        let status = expect["status"].as_str().unwrap_or("match");
        let result = process_text(input, Dialect::Org, laps_of(&case["laps"]));
        match (status, result) {
            ("abort", Err(_)) => aborted += 1,
            ("declined", Err(a)) => failures.push(format!("{id}: listed as declined, but the plugin aborted: {a:?}")),
            ("abort", Ok((out, _))) => failures.push(format!("{id}: Org aborts, the plugin wrote\n{out}")),
            (_, Err(a)) => failures.push(format!("{id}: unexpected abort {a:?}")),
            (status, Ok((out, report))) => {
                let exact = match (case.get("expected"), case.get("expected_contains"), case.get("expected_regex")) {
                    (Some(Value::String(e)), _, _) => out == *e,
                    (_, Some(Value::String(c)), _) => out.contains(c.as_str()),
                    (_, _, Some(Value::String(r))) => regex::Regex::new(r).expect("fixture regex").is_match(&out),
                    _ => false,
                };
                if status == "match" {
                    if exact {
                        matched += 1;
                    } else {
                        failures.push(format!(
                            "{id}: differs from Org\n--- got\n{out}\n--- Org\n{}",
                            case["expected"].as_str().or(case["expected_contains"].as_str()).unwrap_or("")
                        ));
                    }
                    continue;
                }
                if exact {
                    failures.push(format!("{id}: listed as declined but now matches Org; update expectations.json"));
                    continue;
                }
                if report.skipped.is_empty() {
                    failures.push(format!("{id}: differs from Org without a notice\n{out}"));
                    continue;
                }
                if out != input {
                    failures.push(format!("{id}: declined, but the text changed\n--- got\n{out}\n--- input\n{input}"));
                    continue;
                }
                if let Some(Value::String(e)) = case.get("expected") {
                    if let Err(why) = no_wrong_writes(input, &out, e) {
                        failures.push(format!("{id}: {why}"));
                        continue;
                    }
                }
                if std::env::var_os("TF_VERBOSE").is_some() {
                    let why: Vec<String> = report.skipped.iter().map(|s| format!("{}: {}", s.formula, s.reason)).collect();
                    println!("DECLINED {id}\n    {}", why.join("\n    "));
                }
                declined += 1;
            }
        }
    }
    for (id, _) in expectations.as_object().into_iter().flatten() {
        if !cases.iter().any(|c| c["id"].as_str() == Some(id.as_str())) {
            failures.push(format!("expectations.json names unknown case {id}"));
        }
    }
    println!(
        "org-mode corpus (revision {}): {} cases, {} match Org exactly, {} declined with a notice, {} abort like Org",
        revisions.join(", "),
        cases.len(),
        matched,
        declined,
        aborted
    );
    assert!(failures.is_empty(), "{} failure(s):\n\n{}", failures.len(), failures.join("\n\n"));
}

/// The Markdown form of the Emacs-oracle cases: a header row and a GFM
/// delimiter row are put on top (the delimiter counts as the first hline, so
/// column formulas skip the header, as in Org), and the formula line becomes
/// `<!-- TBLFM: ... -->`. The body cells must equal Org's. Only cases whose
/// formulas do not address rows (`@`) and whose table has no hline qualify,
/// since the extra header row shifts row numbers.
#[test]
fn markdown_form_matches_org_cells() {
    let expectations = load("expectations.json");
    let cases = load("emacs-oracle.json")["cases"].as_array().cloned().unwrap_or_default();
    let mut checked = 0;
    let mut failures = Vec::new();
    for case in &cases {
        let id = case["id"].as_str().unwrap_or("?");
        let (Some(input), Some(expected)) = (case["input"].as_str(), case["expected"].as_str()) else { continue };
        if expectations.get(id).is_some() {
            continue;
        }
        let (rows, tblfm): (Vec<&str>, Vec<&str>) =
            input.lines().partition(|l| l.trim_start().starts_with('|'));
        let Some(formula) = tblfm.iter().find_map(|l| l.trim_start().strip_prefix("#+TBLFM:")) else { continue };
        if formula.contains('@') || rows.iter().any(|r| r.trim_start().starts_with("|-")) {
            continue;
        }
        let ncol = rows.iter().map(|r| split_row(r, Dialect::Org).len()).max().unwrap_or(1);
        let header = format!("|{}|", (1..=ncol).map(|i| format!(" h{i} ")).collect::<Vec<_>>().join("|"));
        let delim = format!("|{}|", vec!["---"; ncol].join("|"));
        let md = format!("{header}\n{delim}\n{}\n<!-- TBLFM: {} -->\n", rows.join("\n"), formula.trim());
        let (out, report) = match process_text(&md, Dialect::Markdown, Laps::Times(1)) {
            Ok(r) => r,
            Err(a) => {
                failures.push(format!("{id}: aborted {a:?}"));
                continue;
            }
        };
        let body = |t: &str| -> Vec<Vec<String>> {
            t.lines()
                .filter(|l| l.trim_start().starts_with('|'))
                .map(|l| split_row(l, Dialect::Markdown))
                .filter(|r| !r.iter().all(|c| !c.is_empty() && c.chars().all(|ch| matches!(ch, '-' | ':'))))
                .collect()
        };
        let got: Vec<Vec<String>> = body(&out).into_iter().skip(1).collect();
        let want = body(expected);
        if got != want || !report.skipped.is_empty() {
            failures.push(format!("{id}:\n--- markdown in\n{md}--- got\n{out}\n--- Org\n{expected}"));
        }
        checked += 1;
    }
    println!("markdown form: {checked} Emacs-oracle cases checked");
    assert!(checked >= 20, "only {checked} cases qualified");
    assert!(failures.is_empty(), "{} failure(s):\n\n{}", failures.len(), failures.join("\n\n"));
}
