//! Active-row masks and per-row errors for the columnar evaluators.
//!
//! The explain interpreter evaluates one row at a time and is lazy: an `if`
//! evaluates only its selected branch, `and`/`or` stop at the first deciding
//! item, and an error stops the row it occurs in. The bulk (fast) and dense
//! evaluators compute one expression node for many rows at once, so they carry
//! that control flow as data (see `docs/execution-semantics.md`):
//!
//! - a [`RowMask`] names the rows whose reference evaluation reaches the node
//!   being evaluated; a node does per-row work, and can fail, only for them;
//! - a [`RowErrors`] records the first error of each row instead of aborting
//!   the batch, and an erroring row leaves the mask for the rest of the
//!   enclosing expression, exactly as the reference evaluation of that row
//!   stops there.

use std::collections::BTreeMap;
use std::rc::Rc;

use crate::engine::EvalError;

/// The rows a columnar node is evaluated for.
#[derive(Clone, Debug)]
pub(crate) struct RowMask {
    len: usize,
    /// `None` means every row is active; the common, check-free case.
    bits: Option<Rc<[bool]>>,
    count: usize,
}

impl RowMask {
    pub(crate) fn all(len: usize) -> Self {
        Self {
            len,
            bits: None,
            count: len,
        }
    }

    pub(crate) fn none(len: usize) -> Self {
        Self::from_bits(vec![false; len])
    }

    pub(crate) fn from_bits(bits: Vec<bool>) -> Self {
        let len = bits.len();
        let count = bits.iter().filter(|active| **active).count();
        if count == len {
            return Self::all(len);
        }
        Self {
            len,
            bits: Some(bits.into()),
            count,
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.len
    }

    pub(crate) fn count(&self) -> usize {
        self.count
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.count == 0
    }

    pub(crate) fn is_all(&self) -> bool {
        self.count == self.len
    }

    pub(crate) fn contains(&self, row: usize) -> bool {
        match &self.bits {
            None => row < self.len,
            Some(bits) => bits.get(row).copied().unwrap_or(false),
        }
    }

    /// The active rows, ascending.
    pub(crate) fn rows(&self) -> RowIter<'_> {
        RowIter {
            mask: self,
            next: 0,
        }
    }

    /// The active rows for which `keep` holds.
    pub(crate) fn filter(&self, mut keep: impl FnMut(usize) -> bool) -> Self {
        let mut bits = vec![false; self.len];
        for row in self.rows() {
            bits[row] = keep(row);
        }
        Self::from_bits(bits)
    }

    /// The active rows that have no recorded error.
    pub(crate) fn without(&self, errors: &RowErrors) -> Self {
        if errors.is_empty() || self.is_empty() {
            return self.clone();
        }
        self.filter(|row| !errors.contains(row))
    }

    /// Rows active here but not in `other`.
    pub(crate) fn difference(&self, other: &RowMask) -> Self {
        if other.is_empty() || self.is_empty() {
            return self.clone();
        }
        if other.is_all() {
            return Self::none(self.len);
        }
        self.filter(|row| !other.contains(row))
    }

    /// Rows active in either mask.
    pub(crate) fn union(&self, other: &RowMask) -> Self {
        if self.is_all() || other.is_empty() {
            return self.clone();
        }
        if other.is_all() || self.is_empty() {
            return other.clone();
        }
        let mut bits = vec![false; self.len];
        for row in self.rows().chain(other.rows()) {
            bits[row] = true;
        }
        Self::from_bits(bits)
    }
}

pub(crate) struct RowIter<'a> {
    mask: &'a RowMask,
    next: usize,
}

impl Iterator for RowIter<'_> {
    type Item = usize;

    fn next(&mut self) -> Option<usize> {
        match &self.mask.bits {
            None => {
                if self.next < self.mask.len {
                    self.next += 1;
                    Some(self.next - 1)
                } else {
                    None
                }
            }
            Some(bits) => {
                while self.next < bits.len() {
                    let row = self.next;
                    self.next += 1;
                    if bits[row] {
                        return Some(row);
                    }
                }
                None
            }
        }
    }
}

/// The first error of each failing row of a columnar evaluation.
#[derive(Clone, Debug, Default)]
pub(crate) struct RowErrors {
    errors: BTreeMap<usize, EvalError>,
}

impl RowErrors {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.errors.is_empty()
    }

    pub(crate) fn contains(&self, row: usize) -> bool {
        self.errors.contains_key(&row)
    }

    pub(crate) fn get(&self, row: usize) -> Option<&EvalError> {
        self.errors.get(&row)
    }

    /// Record `error` for `row` unless the row already failed: a row stops at
    /// its first error, so a later one never replaces it.
    pub(crate) fn record(&mut self, row: usize, error: EvalError) {
        self.errors.entry(row).or_insert(error);
    }

    /// Record the same error for every row of `mask`.
    pub(crate) fn record_all(&mut self, mask: &RowMask, error: &EvalError) {
        for row in mask.rows() {
            self.record(row, error.clone());
        }
    }

    /// Merge errors from a later stage of the same evaluation. Rows that
    /// already failed keep their earlier error.
    pub(crate) fn absorb(&mut self, other: RowErrors) {
        if self.errors.is_empty() {
            self.errors = other.errors;
            return;
        }
        for (row, error) in other.errors {
            self.record(row, error);
        }
    }

    /// The errors of the rows active in `mask`.
    pub(crate) fn restricted_to(&self, mask: &RowMask) -> Self {
        if self.errors.is_empty() || mask.is_all() {
            return self.clone();
        }
        Self {
            errors: self
                .errors
                .iter()
                .filter(|(row, _)| mask.contains(**row))
                .map(|(row, error)| (*row, error.clone()))
                .collect(),
        }
    }

    /// Name `rule` in each `match` failure that does not name its rule yet:
    /// explain names the innermost rule an error leaves.
    pub(crate) fn within_rule(self, rule: &str) -> Self {
        if self.errors.is_empty() {
            return self;
        }
        Self {
            errors: self
                .errors
                .into_iter()
                .map(|(row, error)| (row, error.within_rule(rule)))
                .collect(),
        }
    }

    /// The lowest failing row and its error.
    pub(crate) fn first(&self) -> Option<(usize, &EvalError)> {
        self.errors.iter().next().map(|(row, error)| (*row, error))
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (usize, &EvalError)> {
        self.errors.iter().map(|(row, error)| (*row, error))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_track_active_rows_through_set_operations() {
        let all = RowMask::all(4);
        assert!(all.is_all());
        assert_eq!(all.rows().collect::<Vec<_>>(), vec![0, 1, 2, 3]);

        let odd = all.filter(|row| row % 2 == 1);
        assert_eq!(odd.count(), 2);
        assert_eq!(odd.rows().collect::<Vec<_>>(), vec![1, 3]);
        assert!(!odd.contains(0));

        let even = all.difference(&odd);
        assert_eq!(even.rows().collect::<Vec<_>>(), vec![0, 2]);
        assert!(odd.union(&even).is_all());
        assert!(RowMask::none(3).is_empty());
        assert!(RowMask::from_bits(vec![true, true]).is_all());
    }

    #[test]
    fn errors_keep_the_first_failure_of_each_row_and_leave_the_mask() {
        let mut errors = RowErrors::new();
        errors.record(2, EvalError::DivisionByZero);
        errors.record(2, EvalError::UnknownDerived("later".to_string()));
        assert!(matches!(errors.get(2), Some(EvalError::DivisionByZero)));

        let mask = RowMask::all(4).without(&errors);
        assert_eq!(mask.rows().collect::<Vec<_>>(), vec![0, 1, 3]);

        let mut later = RowErrors::new();
        later.record(0, EvalError::UnknownParameter("p".to_string()));
        errors.absorb(later);
        assert_eq!(errors.first().map(|(row, _)| row), Some(0));
        assert!(errors.restricted_to(&mask).get(2).is_none());
    }
}
