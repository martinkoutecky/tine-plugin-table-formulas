# Table formulas for Tine

Org-style spreadsheet formulas for pipe tables, in Org and in Markdown blocks. Put the cursor in a block that holds a table with a formula line and run **Table: recalculate**. The plugin evaluates the formulas, writes the results into the cells and aligns the columns the way Org does.

```
| item | qty | price | total |
|------|-----|-------|-------|
| a    | 2   | 3     |       |
| b    | 4   | 0.5   |       |
<!-- TBLFM: $4=$2*$3 -->
```

becomes

```
| item | qty | price | total |
| ---- | --- | ----- | ----- |
| a    |   2 |     3 |     6 |
| b    |   4 |   0.5 |    2. |
<!-- TBLFM: $4=$2*$3 -->
```

(`2.` is how Org prints a float with no fraction. Add `;%.2f` to the formula for `2.00`.)

## How to use it

Tine plugins are experimental. The plugin API may change incompatibly; no long-term API compatibility is committed.

Install and enable the plugin, edit a block with a table, then press `Ctrl+Alt+R` (`Cmd+Alt+R` on macOS). The command also appears in Ctrl-K and in **Settings -> Keyboard shortcuts**, where you can change the chord. Org's own key is `C-c C-c` on the `#+TBLFM:` line. Tine already uses `Ctrl+C` for copy, so the default is a new chord rather than Org's.

Where the formulas go:

- **Org blocks:** a `#+TBLFM:` line on the line right after the table, as in Org (Org ignores one after a blank line, and so does the plugin). Separate several formulas with `::`.
- **Markdown blocks:** one or more `<!-- TBLFM: ... -->` lines under the table (blank lines between the table and the first comment are allowed). This is the form Kalem's Markdown table formulas use. Tine does not show HTML comments, so the formula line stays out of the rendered page. Consecutive comment lines are joined with `::`. A `#+TBLFM:` line under a Markdown table is not read; when a table has one in place of a comment line, the notice says so (a `#+TBLFM:` line after the comment lines gets no hint).

A Markdown table needs a header row and a delimiter row (`|---|---|`) with the same number of cells, and every line of it must start with `|`. The plugin leaves alone, with a notice, the tables it cannot delimit with certainty: tables in a block quote, tables indented as code, tables whose rows mix leading and missing outer pipes, pipe lines with no valid header and delimiter pair, and tables with a body row wider than the header. Pipe lines inside fenced code or inside HTML blocks (such as `<pre>` or a multi-line comment) are text, not tables, and are never touched.

The plugin cannot see how Tine itself parses the block, so it is conservative about context. If any line of the block looks like a code fence (` ``` ` or `~~~`, at any indentation, also in a list item), or (in Markdown) any line other than the formula comments starts like HTML (`<div>`, `</div>`, `<!-- ... -->`, `<?...`), every table in the block is left unchanged, with a notice. Move the table into a block of its own to recalculate it.

A Markdown table's delimiter row (`|---|---|`) counts as Org's first horizontal line. `@1` is therefore the header row, `@I` is the delimiter, and column formulas skip the header, as they do in Org. It is the only rule in a Markdown table: a body row such as `|-5|7|` is data. Org-style `|---+---|` lines are horizontal lines only in Org tables.

One run is one recalculation pass, like `C-c C-c` in Org. As in Org, the formulas run sorted by their left-hand side, column formulas first and field formulas after them, not in the order they are written; reordering the `::` entries or the comment lines does not change the order. If a formula feeds one that runs before it, run the command again.

**All or nothing per table.** If the plugin cannot match Org on any formula of a table (a feature from the right-hand column below, a result it cannot format exactly, a reference to an empty or crossed range), it leaves the whole table unchanged and the notice names the formula and the reason. Atomicity is per table: other tables in the same block are still recalculated, unless the block itself is declined as described above.

## What is supported

The formula language is the subset of Org's table formulas (Org 9.8) that covers everyday spreadsheet tables. The tests check it against org-mode's own table tests and against Emacs 31.1.

| Supported | Not supported (formula kept, cells untouched, notice) |
|---|---|
| Column formulas `$3=...` and field formulas `@2$3=...` | Emacs Lisp formulas `'(...)`: kept, not evaluated |
| Field ranges on the left side: `@2$1..@4$3=...` | Durations and times (`;T`, `;t`, `;U`), dates and timestamps |
| References `$2`, `@3$2`, `@-1$1`, `$-1`, `@<`, `@>`, `$<`, `$>`, `@>>` | Named fields, named columns and constants (`$name`, `$PI`) |
| Hline references `@I`, `@II`, `@I+1`, `@-I` | Remote references `remote(...)` |
| Ranges `$1..$3`, `@2..@-1`, `@I..@II`, `@2$1..@4$3` | Other Calc functions (`sqrt`, `exp`, `round`, ...) and units |
| `@#` (row number) and `$#` (column number) | `%` (mod), `^` with a non-integer result |
| `+ - * /`, integer `^`, parentheses, comparisons, `&&`, `\|\|`, `!` | Symbolic results (`1/0`, a text cell in arithmetic) |
| `vsum vmean vmax vmin vcount`, plus `vprod vlen if string` | md-advanced-tables' `sum`/`mean` (the notice suggests `vsum`/`vmean`) |
| Exact integers up to 2000 digits; decimals at Calc's 12 digits, shown with 8 | Calc vectors written without commas, such as `[2 3]` |
| printf formats `;%.2f`, `;%d`, `;%e`, `;%g`, `;%5.1f%%`, `;%x`, `;%o` | Flags `t T U F S u`; printf combinations Emacs formats differently from C (`%E`, `%G`, `%#g`, flags on `%s`, ...) |
| Calc modes `;f3`, `;s5`, `;e2`, `;p20`, and flags `N` and `E` | Names defined by Org's `!`, `^`, `_` and `$` rows |
| Org's column alignment (numbers right, text left, `<l>`/`<r>`/`<c>` cookies; in Markdown, delimiter colons take precedence) | A column formula over a `<r>` cookie row (Org writes `#ERROR` there; the plugin keeps the cookie) |
| | Field formulas in a table whose rows have different numbers of cells |

Rows marked `#`, `*`, `!`, `^`, `_` or `$` in the first column are recalculated or skipped as in Org.

Where Org itself writes `#ERROR` into a cell (for example `$2=$1+`, which Calc cannot read), the plugin does too, and the notice names the formula.

A formula that Org rejects outright (an unknown column, a row past the table, two formulas for the same field) stops the command: the notice names the formula and nothing in the block changes. Org edits the table partly before it stops; this plugin writes nothing.

The plugin also has its own limits, meant to stop a pathological formula well before Tine's 250 ms plugin timeout: nesting depth 200, 100,000 fields per range and per formula line's left-hand sides, integers up to 2000 digits, Tine's 128 KiB block limit for the result, and one work budget per run of the command, shared by every table in the block. The budget counts every field read, every pass over a formula's text, and the arithmetic of each evaluation by the size of its numbers; it is calibrated to about 125 ms of work in the release build, and the slowest run measured took about 85 ms. A table over a limit is left unchanged with a notice; when the budget runs out in a block with several tables, the tables already recalculated keep their results and the rest are left unchanged.

Alignment pads by the display width that Unicode assigns to each character. Emacs measures some text differently, notably tabs inside a cell and emoji joined with a zero-width joiner (such as 👩‍💻); in a table that contains them the padding can differ from Org's by a few spaces. The values written are the same.

## Moving tables from Kalem

The comment spelling `<!-- TBLFM: ... -->` is the same as Kalem's, and ordinary tables behave the same. The differences (from Kalem's source, not a runtime comparison):

- Two formulas for the same column (`$2=$1+1` on one comment line, `$2=$2*2` on another): Kalem runs both in order; the plugin refuses, as Org does.
- Kalem needs the first comment on the line right after the table; the plugin allows blank lines in between.
- Kalem accepts Unicode whitespace (such as a no-break space) around the comment and in cells; the plugin trims only spaces and tabs.
- Kalem uses the Markdown table under the caret, with optional outer pipes and quote prefixes. The plugin recalculates every formula table in the focused block, requires leading pipes, and declines quoted tables.
- A formula result that contains `|`: Kalem escapes it; the plugin declines the table.
- Existing `\|` in cells: Kalem adds another backslash on each run; the plugin keeps the cell as it is.
- Line endings: Kalem rewrites the table with LF; the plugin keeps CRLF tables CRLF.
- Kalem's evaluator differs from this plugin's Calc subset, and Kalem does not report formulas it skipped. Identical comment syntax does not mean identical results for formulas outside the table above.

## Development and safety

Build with `cargo build --release`, then run Tine's `plugin:check` on this directory. `cargo test --target x86_64-unknown-linux-gnu` runs the unit tests and the oracle corpus:

- `tests/fixtures/org-table-tests.json`: extracted from org-mode's `testing/lisp/test-org-table.el` by `tools/extract-org-fixtures.py`, which reads the Lisp as data and runs nothing;
- `tests/fixtures/org-table-tests-manual.json`: the other formula tests of that file, transcribed by hand;
- `tests/fixtures/emacs-oracle.json`: cases run through Emacs with `tools/emacs-oracle.el` (inputs in `tools/emacs-oracle-cases.json`; the script also accepts a fixture file and recomputes its cases);
- `tests/fixtures/emacs-oracle-r1.json`: the first review round's cases, computed by Emacs (the second round's 2,000-digit descriptor cases are in `tests/review_round2.rs` as templates with Emacs's results);
- `tests/fixtures/emacs-printf.json`: 1,549 printf format/value pairs computed by Emacs with `tools/emacs-printf-oracle.el`, a deterministic subset of an 86,025-pair flag/width/precision/conversion grid (every third pair of an earlier 4,510-pair sample, plus one pair of each conversion, flag set, precision and declined combination it would otherwise miss). The full grid matched with 0 differences (29,179 matches, 56,846 declines) in both review rounds; regenerate it with the oracle script to rerun it;
- `tests/fixtures/expectations.json`: every fixture case the plugin declines, with the reason. It lists fixture cases, not every input the evaluator declines. A declined case must produce a notice and leave the block byte for byte unchanged; every other case must equal Org's output.

The plugin asks for two capabilities: registering its command, and writing the focused block. Tine hands it only the focused block's id, text and format; it cannot read other blocks, files or the network. The change is a single expected-text replacement: Tine applies it only if the block still holds the text the plugin read. It is one Undo step and goes through Tine's normal conflict-safe save path. Text outside the recalculated tables is kept byte for byte, line endings included. This depends on the plugin finding the same tables Tine does; where it cannot be sure, it declines (see above).

No Org, Emacs or Calc code was copied. The plugin was written from the documented behaviour and checked against the upstream test suite and against Emacs. This AI-primary plugin is MIT-licensed; see `LICENSE`. The test data in `tests/fixtures/org-table-tests*.json` is derived from GNU Org mode's test suite and keeps its licence (GPL-3.0-or-later); see `tests/fixtures/NOTICE`. It is used only by the tests and is not part of the built plugin.
