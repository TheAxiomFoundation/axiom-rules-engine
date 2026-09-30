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
//! of rules longer than the threshold therefore run in segments, each on a
//! stack no deeper than the threshold plus one rule's own expression nesting,
//! whatever the chain's length (a 20,000-rule chain answers on a 1 MiB
//! thread). Two evaluations nest one drive inside another and can use twice
//! that: fast's relation aggregations run on the explain engine it embeds,
//! and dense's lifetime reductions drive each period's executor. What this
//! does not bound: expressions dense inlines into a relation aggregation,
//! which have no rule to defer and are capped by `dense::MAX_INLINE_DEPTH`
//! instead, and chains of derived relations (#211).
//!
//! Deferral changes no result for an acyclic program, which every checked
//! program is. Evaluation of a rule for an entity and period is
//! deterministic, so a retry reaches the same references in the same order
//! and finds every rule it needs already cached; a rule's value, errors and
//! trace do not depend on which stack computed them. Only the amount of work
//! changes: a retry walks again, over cached values, what the interrupted
//! evaluation did before the deferral.
//!
//! What a retry walks again depends on the program, not on the data. Within
//! one drive, explain records the progress of every loop over data a deferral
//! can interrupt: a `count` or `sum` over related entities, and the
//! resolution of a relation's members, which tests a derived relation's
//! predicate on each candidate and reads the relation's own tuples once, at
//! the end. The retry resumes an interrupted loop at the member that deferred
//! and reuses a finished one, so a household whose every member defers is not
//! walked once per member. Fast evaluates each row's relation aggregation as
//! a drive of its own on the explain engine it embeds, so the same holds
//! within that row; when fast itself retries a column, it evaluates the
//! aggregation for its rows again, once per retry, which is linear in the
//! rows. The rest is the rules still open on the interrupted path and the
//! operands they had evaluated. So a chain costs a constant factor more
//! than recursion; a rule with `k` operands that each defer walks its earlier
//! operands `k` times. [`count_visits`] measures this, and
//! `tests/deferral_transparency.rs` pins it.
//!
//! A rule that is deferred while an evaluation it transitively started is
//! still waiting for it depends on itself. The drivers report that as a cycle
//! instead of deferring forever. Which rule of the cycle the error names can
//! depend on the threshold, because it is whichever rule deferred twice.
//! Checked programs are acyclic (see `compile::validate_dependency_graph`);
//! only a hand-built [`crate::model::Program`] can reach it.

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
