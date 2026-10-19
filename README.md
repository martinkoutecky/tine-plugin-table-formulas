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

Install and enable the plugin, edit a block with a table, then press `Ctrl+Alt+R` (`Cmd+Alt+R` on macOS). The command also appears in Ctrl-K and in **Settings -> Keyboard shortcuts**, where you can change the chord. Org's own key is `C-c C-c` on the `#+TBLFM:` line. Tine already uses `Ctrl+C` for copy, so the default is a new chord rather than Org's.

Where the formulas go:

- **Org blocks:** a `#+TBLFM:` line directly under the table, as in Org. Separate several formulas with `::`.
- **Markdown blocks:** one or more `<!-- TBLFM: ... -->` lines directly under the table. This is the form Kalem's Markdown table formulas use. Tine does not show HTML comments, so the formula line stays out of the rendered page. Consecutive comment lines are joined with `::`. A `#+TBLFM:` line under a Markdown table is not read; the plugin says so.

A Markdown table's delimiter row (`|---|---|`) counts as Org's first horizontal line. `@1` is therefore the header row, `@I` is the delimiter, and column formulas skip the header, as they do in Org.

One run is one recalculation pass, like `C-c C-c` in Org. If one formula feeds another formula listed before it, run the command again.

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
| Exact integers of any size; decimals at Calc's 12 digits, shown with 8 | Calc vectors written without commas, such as `[2 3]` |
| printf formats `;%.2f`, `;%d`, `;%e`, `;%g`, `;%5.1f%%` | Flags `t T U F S u` |
| Calc modes `;f3`, `;s5`, `;e2`, `;p20`, and flags `N` and `E` | Names defined by Org's `!`, `^`, `_` and `$` rows |
| Org's column alignment (numbers right, text left, `<l>`/`<r>`/`<c>` cookies) | A column formula over a `<r>` cookie row (Org writes `#ERROR` there; the plugin keeps the cookie) |

Rows marked `#`, `*`, `!`, `^`, `_` or `$` in the first column are recalculated or skipped as in Org.

Where Org itself writes `#ERROR` into a cell (for example `$2=$1+`, which Calc cannot read), the plugin does too, and the notice names the formula.

A formula that Org rejects outright (an unknown column, a row past the table, two formulas for the same field) stops the command: the notice names the formula and nothing in the block changes. Org edits the table partly before it stops; this plugin writes nothing.

## Development and safety

Build with `cargo build --release`, then run Tine's `plugin:check` on this directory. `cargo test --target x86_64-unknown-linux-gnu` runs the unit tests and the oracle corpus:

- `tests/fixtures/org-table-tests.json`: extracted from org-mode's `testing/lisp/test-org-table.el` by `tools/extract-org-fixtures.py`, which reads the Lisp as data and runs nothing;
- `tests/fixtures/org-table-tests-manual.json`: the other formula tests of that file, transcribed by hand;
- `tests/fixtures/emacs-oracle.json`: cases run through Emacs with `tools/emacs-oracle.el`;
- `tests/fixtures/expectations.json`: every case the plugin declines, with the reason. A declined case must produce a notice, and every cell the plugin does write must equal Org's.

The plugin asks for two capabilities: registering its command, and writing the focused block. Tine hands it only the focused block's id, text and format; it cannot read other blocks, files or the network. The change is a single expected-text replacement: Tine applies it only if the block still holds the text the plugin read. It is one Undo step and goes through Tine's normal conflict-safe save path. Text outside the recalculated tables is kept byte for byte, line endings included.

No Org, Emacs or Calc code was copied. The plugin was written from the documented behaviour and checked against the upstream test suite and against Emacs. This AI-primary plugin is MIT-licensed; see `LICENSE`.
