//! Bounded recursion for the evaluators (#206).
//!
//! Explain, fast and dense evaluate a derived rule by recursing into the rules
//! it references, and dense compiles a rule the same way. A program's
//! dependency chains can be arbitrarily long, so unbounded recursion overflowed
//! the stack on deep, valid programs: the process aborted, in whichever mode
//! ran out first, where the other mode answered.
//!
//! Each of these recursions now counts its nesting levels. At a reference to
//! a rule it has not evaluated yet, once the count reaches
//! [`suspend_depth`], it stops recursing: it unwinds to its driver loop,
//! naming the rule it needs. The driver evaluates that rule from level zero,
//! then retries what it was doing, which now finds the rule cached. Chains
//! longer than the threshold therefore run in segments, each on a stack no
//! deeper than the threshold plus one rule's own expression nesting, whatever
//! the chain's length or the host's stack size (a 2 MiB test thread, a Python
//! thread, a 1 MiB wasm stack).
//!
//! Deferral changes no result. Evaluation of a rule for an entity and period
//! is deterministic, so a retry reaches the same references in the same order
//! and finds every rule it needs already cached; a rule's value, errors and
//! trace do not depend on which stack computed them. Only the amount of work
//! changes: a retry walks again, over cached values, what the interrupted
//! evaluation did before the deferral.
//!
//! What a retry walks again depends on the program, not on the data. Explain
//! (and fast, which evaluates relation aggregations with explain) records the
//! progress of every loop over data a deferral can interrupt: a `count` or
//! `sum` over related entities, and the resolution of a relation's members,
//! which tests a derived relation's predicate on each candidate. The retry
//! resumes an interrupted loop at the member that deferred and reuses a
//! finished one, so a household whose every member defers is not walked once
//! per member. The rest is the rules still open on the interrupted path and
//! the operands they had evaluated. So a chain costs a constant factor more
//! than recursion; a rule with `k` operands that each defer walks its earlier
//! operands `k` times. [`count_visits`] measures this, and
//! `tests/deferral_transparency.rs` pins it.
//!
//! A rule that is deferred while an evaluation it transitively started is
//! still waiting for it depends on itself. The drivers report that as a cycle
//! instead of deferring forever. Checked programs are acyclic (see
//! `compile::validate_dependency_graph`); only a hand-built [`crate::model::Program`]
//! can reach it.

use std::cell::Cell;

/// Levels an evaluator recurses before deferring a rule it has not evaluated.
///
/// Debug frames are about thirty times larger than release frames (explain
/// used about 30 KiB of stack per nesting level unoptimised, about 1 KiB
/// optimised), so debug builds defer early; that also makes the whole debug
/// test suite exercise deferral. In release, an evaluation that never nests
/// 128 levels deep never retries, and a segment's stack stays within a few
/// hundred KiB.
#[cfg(debug_assertions)]
const DEFAULT_SUSPEND_DEPTH: usize = 8;
#[cfg(not(debug_assertions))]
const DEFAULT_SUSPEND_DEPTH: usize = 128;

thread_local! {
    static SUSPEND_DEPTH: Cell<usize> = const { Cell::new(DEFAULT_SUSPEND_DEPTH) };
    static VISITS: Cell<usize> = const { Cell::new(0) };
}

/// The current thread's deferral threshold, in nesting levels. Always at
/// least 1, so every driver task makes progress before it can defer.
pub(crate) fn suspend_depth() -> usize {
    SUSPEND_DEPTH.with(Cell::get)
}

/// Run `f` with a different deferral threshold on this thread, then restore
/// the previous one (also when `f` panics). Tests use it to show that the
/// threshold never changes a result: `usize::MAX` recurses without deferring,
/// and `1` defers at every rule reference it can.
///
/// Not a stable API.
#[doc(hidden)]
pub fn with_suspend_depth<R>(levels: usize, f: impl FnOnce() -> R) -> R {
    struct Restore(usize);
    impl Drop for Restore {
        fn drop(&mut self) {
            SUSPEND_DEPTH.with(|depth| depth.set(self.0));
        }
    }
    let _restore = Restore(SUSPEND_DEPTH.with(|depth| depth.replace(levels.max(1))));
    f()
}

/// Run `f` and count the expression levels the evaluators visited on this
/// thread meanwhile, retries included: explain's and fast's nodes, dense's
/// compiled nodes, and the nodes of the rules the dense compiler compiles
/// (not of what it inlines, which never defers). It measures work
/// deterministically, so tests can show that deferral adds at most a constant
/// factor to what recursion does, whatever the machine's load.
///
/// Not a stable API.
#[doc(hidden)]
pub fn count_visits<R>(f: impl FnOnce() -> R) -> (R, usize) {
    let before = VISITS.with(Cell::get);
    let result = f();
    (result, VISITS.with(Cell::get).wrapping_sub(before))
}

/// Report visits an evaluator counted (see [`count_visits`]).
pub(crate) fn add_visits(visits: usize) {
    VISITS.with(|total| total.set(total.get().wrapping_add(visits)));
}
