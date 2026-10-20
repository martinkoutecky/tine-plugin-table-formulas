//! The work meter: one budget per run of the command, shared by every table
//! of the block, every recalculation pass and every step that does more than
//! constant work (registry audit, 2026-10-20).
//!
//! Malformed input (a formula or table that would make the guest run past the
//! host's 250 ms invocation timeout) is declined with a notice instead.
//! Charges are in ticks; [`TICKS_PER_UNIT`] ticks make one unit, and
//! [`MAX_WORK`] units are the budget, about 125 ms in the release guest
//! under Node (each cost is set at about twice its measured time).
//!
//! The rule every step follows: charge before doing work whose size is known
//! (operand sizes, text lengths, loop counts), and charge as it goes where the
//! size is found while working. Charges are sticky: once a run is over
//! budget it stays so, every later charge fails, and the formula engine
//! declines the table at its next check. A step therefore overruns the
//! budget by at most one bounded piece of work. No result is written for a
//! field whose evaluation ended over budget.
//!
//! Outside a run (unit tests of single functions) nothing is limited.

use num_bigint::BigInt;
use std::cell::Cell;

/// Work units one command run may use.
pub const MAX_WORK: usize = 50_000;
/// Ticks per unit; a unit is about 2.5 us in the release guest.
pub const TICKS_PER_UNIT: u64 = 512;
const MAX_TICKS: u64 = MAX_WORK as u64 * TICKS_PER_UNIT;

thread_local! {
    static TICKS: Cell<u64> = const { Cell::new(0) };
    static ACTIVE: Cell<bool> = const { Cell::new(false) };
}

/// One command run's budget. Starting a run while one is active joins it
/// (the budget is shared, not reset); the outermost run ends it on drop.
#[derive(Debug)]
pub struct Run {
    owner: bool,
}

impl Run {
    pub fn start() -> Run {
        let owner = !ACTIVE.with(Cell::get);
        if owner {
            ACTIVE.with(|a| a.set(true));
            TICKS.with(|t| t.set(0));
        }
        Run { owner }
    }

    /// Units used so far (rounded up).
    pub fn used(&self) -> usize {
        used_units()
    }
}

impl Drop for Run {
    fn drop(&mut self) {
        if self.owner {
            ACTIVE.with(|a| a.set(false));
            TICKS.with(|t| t.set(0));
        }
    }
}

/// Charge `t` ticks. `false` when the run is (now) over budget.
pub fn ticks(t: u64) -> bool {
    if !ACTIVE.with(Cell::get) {
        return true;
    }
    TICKS.with(|c| {
        let n = c.get().saturating_add(t);
        c.set(n);
        n <= MAX_TICKS
    })
}

/// Charge `u` units.
pub fn units(u: usize) -> bool {
    ticks((u as u64).saturating_mul(TICKS_PER_UNIT))
}

/// Whether the active run is over budget.
pub fn exhausted() -> bool {
    ACTIVE.with(Cell::get) && TICKS.with(Cell::get) > MAX_TICKS
}

pub fn used_units() -> usize {
    TICKS.with(Cell::get).div_ceil(TICKS_PER_UNIT) as usize
}

/// Units left in the active run ([`MAX_WORK`] outside a run).
pub fn remaining_units() -> usize {
    MAX_WORK.saturating_sub(used_units())
}

// ---------------------------------------------------------------- costs
//
// Measured on the release guest under Node (2026-10-20), charged at about
// twice the time: a tick is about 2.4 ns of work.

/// One regex pass (or rewrite) over `len` bytes of formula text: about
/// 17 ns per byte on short text and 200 ns per byte past 2 KiB, where the
/// regex engine needs its slower general matcher.
pub fn scan_ticks(len: usize) -> u64 {
    let len = len as u64;
    64 + len * 8 + len.saturating_sub(2048) * 64
}

/// Charge one regex pass over `len` bytes.
pub fn scan(len: usize) -> bool {
    ticks(scan_ticks(len))
}

/// Charge a plain pass over `len` bytes: copying, comparing, splitting or
/// iterating characters without a regex.
pub fn text(len: usize) -> bool {
    ticks(text_ticks(len))
}

/// Ticks of [`text`].
pub fn text_ticks(len: usize) -> u64 {
    16 + len as u64 / 2
}

/// Size of a big integer in 64-bit limbs (at least 1).
pub fn limbs(a: &BigInt) -> u64 {
    a.bits() / 64 + 1
}

/// Limbs of the decimal number with `digits` digits.
pub fn digit_limbs(digits: usize) -> u64 {
    digits as u64 / 19 + 1
}

/// Adding, subtracting, comparing or copying numbers of `la` and `lb` limbs.
pub fn add_ticks(la: u64, lb: u64) -> u64 {
    8 + la + lb
}

/// Multiplying (or dividing) numbers of `la` and `lb` limbs, schoolbook.
pub fn mul_ticks(la: u64, lb: u64) -> u64 {
    8 + la.saturating_mul(lb) + la + lb
}

/// Converting an `l`-limb number to or from decimal text (quadratic).
pub fn decimal_ticks(l: u64) -> u64 {
    16 + l.saturating_mul(l).saturating_mul(4)
}
