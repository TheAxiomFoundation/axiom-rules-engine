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
//!
//! Every buffer either evaluator sizes by a row count goes through
//! [`row_buffer`] or [`filled_rows`], which reserve it fallibly. A batch can
//! state more rows than any buffer of them fits in memory, and the failed
//! allocation is then an error naming the count, not an aborted process.
//!
//! A text is never copied once per row. Dense text columns share each value
//! (`Arc<str>`), and [`RowErrors`] shares an error among the rows it fails
//! (`Rc<EvalError>`). An error that names a text copies it with
//! [`copied_text`] or [`debug_text`], also fallibly.

use std::collections::BTreeMap;
use std::fmt::{self, Write as _};
use std::rc::Rc;

use crate::engine::EvalError;

/// The error for an allocation the allocator refused, naming the row count of
/// the evaluation that needed it.
fn refusal(rows: usize) -> EvalError {
    EvalError::TypeMismatch(format!(
        "evaluation needs a buffer of {rows} rows, more than this process can allocate"
    ))
}

/// An empty buffer with room for `rows` elements, reserved fallibly: when
/// the allocator cannot grant it, the error names the count.
pub(crate) fn row_buffer<T>(rows: usize) -> Result<Vec<T>, EvalError> {
    let mut buffer = Vec::new();
    buffer.try_reserve_exact(rows).map_err(|_| refusal(rows))?;
    Ok(buffer)
}

/// `rows` copies of `value`: `vec![value; rows]`, reserved fallibly.
pub(crate) fn filled_rows<T: Clone>(rows: usize, value: T) -> Result<Vec<T>, EvalError> {
    let mut buffer = row_buffer(rows)?;
    buffer.resize(rows, value);
    Ok(buffer)
}

/// A copy of `values`, reserved fallibly.
pub(crate) fn copied_rows<T: Clone>(values: &[T]) -> Result<Vec<T>, EvalError> {
    let mut buffer = row_buffer(values.len())?;
    buffer.extend_from_slice(values);
    Ok(buffer)
}

/// A copy of `text`, reserved fallibly. The refusal names `rows`, the row
/// count of the evaluation that copies it.
pub(crate) fn copied_text(text: &str, rows: usize) -> Result<String, EvalError> {
    let mut copy = String::new();
    copy.try_reserve_exact(text.len())
        .map_err(|_| refusal(rows))?;
    copy.push_str(text);
    Ok(copy)
}

/// `text` as `{:?}` renders it, quoted and escaped, reserved fallibly. The
/// rendering is measured first, so writing it never grows the reservation.
pub(crate) fn debug_text(text: &str, rows: usize) -> Result<String, EvalError> {
    struct Length(usize);
    impl fmt::Write for Length {
        fn write_str(&mut self, part: &str) -> fmt::Result {
            self.0 = self.0.saturating_add(part.len());
            Ok(())
        }
    }
    let mut length = Length(0);
    // Neither writer can fail: one counts, and the other has room.
    let _ = write!(length, "{text:?}");
    let mut rendered = String::new();
    rendered
        .try_reserve_exact(length.0)
        .map_err(|_| refusal(rows))?;
    let _ = write!(rendered, "{text:?}");
    Ok(rendered)
}

/// A copy of `error` whose texts are reserved fallibly. Only the errors that
/// name a value of the batch can hold a text as long as the batch's own; any
/// other error holds names from the program and is cloned as it is.
pub(crate) fn copied_error(error: &EvalError, rows: usize) -> Result<EvalError, EvalError> {
    let copy = |text: &str| copied_text(text, rows);
    Ok(match error {
        EvalError::NoMatchingArm {
            rule,
            subject,
            value,
            patterns,
        } => EvalError::NoMatchingArm {
            rule: copy(rule)?,
            subject: copy(subject)?,
            value: copy(value)?,
            patterns: copy(patterns)?,
        },
        EvalError::LifetimePeriodVaryingInput {
            input,
            first_period,
            first_value,
            second_period,
            second_value,
        } => EvalError::LifetimePeriodVaryingInput {
            input: copy(input)?,
            first_period: copy(first_period)?,
            first_value: copy(first_value)?,
            second_period: copy(second_period)?,
            second_value: copy(second_value)?,
        },
        EvalError::OverPeriodsTopNPeriodVarying {
            reduction,
            first_period,
            first_value,
            second_period,
            second_value,
        } => EvalError::OverPeriodsTopNPeriodVarying {
            reduction: *reduction,
            first_period: copy(first_period)?,
            first_value: copy(first_value)?,
            second_period: copy(second_period)?,
            second_value: copy(second_value)?,
        },
        EvalError::OverPeriodsTopNOutOfRange {
            reduction,
            n,
            period_count,
        } => EvalError::OverPeriodsTopNOutOfRange {
            reduction: *reduction,
            n: copy(n)?,
            period_count: *period_count,
        },
        other => other.clone(),
    })
}

/// The rows a columnar node is evaluated for.
#[derive(Clone, Debug)]
pub(crate) struct RowMask {
    len: usize,
    /// `None` when every row is active or none is, as `count` says: the
    /// common, check-free cases, which allocate nothing. Sharing the `Vec`
    /// keeps its allocation rather than copying the bits into an `Rc` slice.
    bits: Option<Rc<Vec<bool>>>,
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
        Self {
            len,
            bits: None,
            count: 0,
        }
    }

    pub(crate) fn from_bits(bits: Vec<bool>) -> Self {
        let len = bits.len();
        let count = bits.iter().filter(|active| **active).count();
        if count == len {
            return Self::all(len);
        }
        if count == 0 {
            return Self::none(len);
        }
        Self {
            len,
            bits: Some(Rc::new(bits)),
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
            None => self.is_all() && row < self.len,
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

    /// The active rows for which `keep` holds. `keep` sees only active rows,
    /// so a mask with none allocates nothing.
    pub(crate) fn filter(&self, mut keep: impl FnMut(usize) -> bool) -> Result<Self, EvalError> {
        if self.is_empty() {
            return Ok(self.clone());
        }
        let mut bits = filled_rows(self.len, false)?;
        for row in self.rows() {
            bits[row] = keep(row);
        }
        Ok(Self::from_bits(bits))
    }

    /// The active rows that have no recorded error.
    pub(crate) fn without(&self, errors: &RowErrors) -> Result<Self, EvalError> {
        if errors.is_empty() || self.is_empty() {
            return Ok(self.clone());
        }
        self.filter(|row| !errors.contains(row))
    }

    /// Rows active here but not in `other`.
    pub(crate) fn difference(&self, other: &RowMask) -> Result<Self, EvalError> {
        if other.is_empty() || self.is_empty() {
            return Ok(self.clone());
        }
        if other.is_all() {
            return Ok(Self::none(self.len));
        }
        self.filter(|row| !other.contains(row))
    }

    /// Rows active in either mask.
    pub(crate) fn union(&self, other: &RowMask) -> Result<Self, EvalError> {
        if self.is_all() || other.is_empty() {
            return Ok(self.clone());
        }
        if other.is_all() || self.is_empty() {
            return Ok(other.clone());
        }
        let mut bits = filled_rows(self.len, false)?;
        for row in self.rows().chain(other.rows()) {
            bits[row] = true;
        }
        Ok(Self::from_bits(bits))
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
                if self.mask.is_all() && self.next < self.mask.len {
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

/// The first error of each failing row of a columnar evaluation. Rows that
/// fail with the same error share it, so recording, lifting, lowering or
/// restricting an error copies a pointer, never the texts the error holds.
#[derive(Clone, Debug, Default)]
pub(crate) struct RowErrors {
    errors: BTreeMap<usize, Rc<EvalError>>,
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

    #[cfg(test)]
    pub(crate) fn get(&self, row: usize) -> Option<&EvalError> {
        self.errors.get(&row).map(|error| &**error)
    }

    /// The error of `row`, to record for another row.
    pub(crate) fn get_shared(&self, row: usize) -> Option<&Rc<EvalError>> {
        self.errors.get(&row)
    }

    /// Record `error` for `row` unless the row already failed: a row stops at
    /// its first error, so a later one never replaces it.
    pub(crate) fn record(&mut self, row: usize, error: EvalError) {
        self.errors.entry(row).or_insert_with(|| Rc::new(error));
    }

    /// Record an error another row holds for `row`, sharing it.
    pub(crate) fn record_shared(&mut self, row: usize, error: &Rc<EvalError>) {
        self.errors.entry(row).or_insert_with(|| Rc::clone(error));
    }

    /// Record the same error for every row of `mask`, shared by all of them.
    pub(crate) fn record_all(&mut self, mask: &RowMask, error: &EvalError) {
        if mask.is_empty() {
            return;
        }
        let error = Rc::new(error.clone());
        for row in mask.rows() {
            self.record_shared(row, &error);
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
            self.errors.entry(row).or_insert(error);
        }
    }

    /// The errors of the rows active in `mask`. Entries are inserted one at
    /// a time: collecting into a map would first gather them all in one
    /// buffer as long as the error count.
    pub(crate) fn restricted_to(&self, mask: &RowMask) -> Self {
        if self.errors.is_empty() || mask.is_all() {
            return self.clone();
        }
        let mut errors = BTreeMap::new();
        errors.extend(
            self.errors
                .iter()
                .filter(|(row, _)| mask.contains(**row))
                .map(|(row, error)| (*row, Rc::clone(error))),
        );
        Self { errors }
    }

    /// Name `rule` in each `match` failure that does not name its rule yet:
    /// explain names the innermost rule an error leaves. An error only these
    /// rows hold is renamed in place. One that other rows or evaluations
    /// share is copied once, its texts reserved fallibly (a refusal names
    /// `rows`), and the copy shared by every row here that held it.
    pub(crate) fn within_rule(mut self, rule: &str, rows: usize) -> Result<Self, EvalError> {
        let mut renamed: Option<(Rc<EvalError>, Rc<EvalError>)> = None;
        for error in self.errors.values_mut() {
            if !matches!(&**error, EvalError::NoMatchingArm { rule, .. } if rule.is_empty()) {
                continue;
            }
            if let Some((source, copy)) = &renamed
                && Rc::ptr_eq(error, source)
            {
                *error = Rc::clone(copy);
                continue;
            }
            if let Some(unique) = Rc::get_mut(error) {
                name_rule(unique, rule);
                continue;
            }
            let mut copy = copied_error(error, rows)?;
            name_rule(&mut copy, rule);
            let copy = Rc::new(copy);
            renamed = Some((std::mem::replace(error, Rc::clone(&copy)), copy));
        }
        Ok(self)
    }

    /// The lowest failing row and its error.
    pub(crate) fn first(&self) -> Option<(usize, &EvalError)> {
        self.errors
            .iter()
            .next()
            .map(|(row, error)| (*row, &**error))
    }

    /// Each failing row and its error, to record for another row.
    pub(crate) fn iter_shared(&self) -> impl Iterator<Item = (usize, &Rc<EvalError>)> {
        self.errors.iter().map(|(row, error)| (*row, error))
    }
}

/// Name `rule` in a `match` failure that names none yet.
fn name_rule(error: &mut EvalError, rule: &str) {
    if let EvalError::NoMatchingArm { rule: existing, .. } = error
        && existing.is_empty()
    {
        *existing = rule.to_string();
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use proptest::prelude::*;

    use super::*;

    #[test]
    fn masks_track_active_rows_through_set_operations() -> Result<(), EvalError> {
        let all = RowMask::all(4);
        assert!(all.is_all());
        assert_eq!(all.rows().collect::<Vec<_>>(), vec![0, 1, 2, 3]);

        let odd = all.filter(|row| row % 2 == 1)?;
        assert_eq!(odd.count(), 2);
        assert_eq!(odd.rows().collect::<Vec<_>>(), vec![1, 3]);
        assert!(!odd.contains(0));

        let even = all.difference(&odd)?;
        assert_eq!(even.rows().collect::<Vec<_>>(), vec![0, 2]);
        assert!(odd.union(&even)?.is_all());
        assert!(RowMask::none(3).is_empty());
        assert!(RowMask::from_bits(vec![true, true]).is_all());
        Ok(())
    }

    #[test]
    fn a_mask_with_no_active_row_holds_no_bits() -> Result<(), EvalError> {
        // A row count no buffer fits: none of these may allocate one.
        let rows = usize::MAX;
        let none = RowMask::none(rows);
        assert!(none.is_empty() && !none.is_all());
        assert_eq!(none.len(), rows);
        assert!(!none.contains(0) && !none.contains(rows - 1));
        assert_eq!(none.rows().next(), None);
        let filtered = none.filter(|_| panic!("a mask with no active row has no row to keep"))?;
        assert!(filtered.is_empty());
        assert!(none.difference(&RowMask::all(rows))?.is_empty());
        assert!(
            RowMask::all(rows)
                .difference(&RowMask::all(rows))?
                .is_empty()
        );
        assert!(none.union(&none)?.is_empty());
        assert!(none.union(&RowMask::all(rows))?.is_all());

        let cleared = RowMask::from_bits(vec![false, false, false]);
        assert!(cleared.is_empty() && cleared.bits.is_none());
        Ok(())
    }

    #[test]
    fn a_buffer_larger_than_any_allocation_is_an_error_naming_its_rows() {
        // More bytes than `isize::MAX`, so the reservation fails before it
        // reaches the allocator; a `vec!` here would panic instead.
        let rows = usize::MAX / 4;
        for error in [
            filled_rows(rows, 0_u64).map(|_| ()),
            row_buffer::<u64>(rows).map(|_| ()),
            RowMask::all(rows).filter(|_| true).map(|_| ()),
        ] {
            match error {
                Err(EvalError::TypeMismatch(message)) => assert_eq!(
                    message,
                    format!(
                        "evaluation needs a buffer of {rows} rows, more than this process can allocate"
                    )
                ),
                other => panic!("expected the refusal, got {other:?}"),
            }
        }
    }

    #[test]
    fn fallible_buffers_hold_what_vec_would() -> Result<(), EvalError> {
        assert_eq!(filled_rows(3, 7_i64)?, vec![7; 3]);
        assert_eq!(filled_rows(2, "a".to_string())?, vec!["a".to_string(); 2]);
        assert_eq!(filled_rows::<bool>(0, true)?, Vec::<bool>::new());
        assert_eq!(copied_rows(&[1, 2, 3])?, vec![1, 2, 3]);
        let buffer = row_buffer::<u8>(5)?;
        assert!(buffer.is_empty() && buffer.capacity() >= 5);
        Ok(())
    }

    /// The rows a mask over `bits` holds: ascending, each active once.
    fn active(bits: &[bool]) -> BTreeSet<usize> {
        bits.iter()
            .enumerate()
            .filter_map(|(row, active)| active.then_some(row))
            .collect()
    }

    fn agrees(mask: &RowMask, len: usize, expected: &BTreeSet<usize>) -> Result<(), TestCaseError> {
        prop_assert_eq!(mask.len(), len);
        prop_assert_eq!(mask.count(), expected.len());
        prop_assert_eq!(mask.is_empty(), expected.is_empty());
        prop_assert_eq!(mask.is_all(), expected.len() == len);
        prop_assert_eq!(mask.rows().collect::<BTreeSet<_>>(), expected.clone());
        prop_assert!(mask.rows().zip(mask.rows().skip(1)).all(|(a, b)| a < b));
        for row in 0..len + 2 {
            prop_assert_eq!(mask.contains(row), expected.contains(&row));
        }
        Ok(())
    }

    /// A mask over `bits`, built as the evaluators build one: from bits, or
    /// as all or none when the bits say so.
    fn mask_of(bits: &[bool], shortcut: bool) -> RowMask {
        let expected = active(bits);
        match (shortcut, expected.len()) {
            (true, 0) => RowMask::none(bits.len()),
            (true, count) if count == bits.len() => RowMask::all(bits.len()),
            _ => RowMask::from_bits(bits.to_vec()),
        }
    }

    proptest! {
        /// Invariant: every mask, however it is represented (all, none or
        /// bits), is the set of rows its bits hold, and the set operations
        /// are the set operations on those rows.
        #[test]
        fn masks_are_the_sets_their_bits_hold(
            (left, right, keep) in (0_usize..40).prop_flat_map(|len| (
                prop::collection::vec(any::<bool>(), len),
                prop::collection::vec(any::<bool>(), len),
                prop::collection::vec(any::<bool>(), len),
            )),
            left_shortcut in any::<bool>(),
            right_shortcut in any::<bool>(),
        ) {
            let len = left.len();
            let (a, b) = (active(&left), active(&right));
            let left_mask = mask_of(&left, left_shortcut);
            let right_mask = mask_of(&right, right_shortcut);
            agrees(&left_mask, len, &a)?;
            agrees(&right_mask, len, &b)?;

            let kept = left_mask.filter(|row| keep[row]).expect("small mask");
            let expected = a.iter().copied().filter(|row| keep[*row]).collect();
            agrees(&kept, len, &expected)?;
            agrees(&left_mask.difference(&right_mask).expect("small mask"), len, &(&a - &b))?;
            agrees(&left_mask.union(&right_mask).expect("small mask"), len, &(&a | &b))?;

            let mut errors = RowErrors::new();
            for row in &b {
                errors.record(*row, EvalError::DivisionByZero);
            }
            agrees(&left_mask.without(&errors).expect("small mask"), len, &(&a - &b))?;
        }
    }

    #[test]
    fn errors_keep_the_first_failure_of_each_row_and_leave_the_mask() -> Result<(), EvalError> {
        let mut errors = RowErrors::new();
        errors.record(2, EvalError::DivisionByZero);
        errors.record(2, EvalError::UnknownDerived("later".to_string()));
        assert!(matches!(errors.get(2), Some(EvalError::DivisionByZero)));

        let mask = RowMask::all(4).without(&errors)?;
        assert_eq!(mask.rows().collect::<Vec<_>>(), vec![0, 1, 3]);

        let mut later = RowErrors::new();
        later.record(0, EvalError::UnknownParameter("p".to_string()));
        errors.absorb(later);
        assert_eq!(errors.first().map(|(row, _)| row), Some(0));
        assert!(errors.restricted_to(&mask).get(2).is_none());
        Ok(())
    }

    /// A `match` failure naming no rule yet, its subject's value `value`.
    fn unnamed_match(value: &str) -> EvalError {
        EvalError::NoMatchingArm {
            rule: String::new(),
            subject: "label".to_string(),
            value: value.to_string(),
            patterns: "\"a\"".to_string(),
        }
    }

    fn named(error: Option<&EvalError>) -> Option<&str> {
        match error {
            Some(EvalError::NoMatchingArm { rule, .. }) => Some(rule),
            _ => None,
        }
    }

    #[test]
    fn a_shared_error_is_renamed_once_and_an_unshared_one_in_place() -> Result<(), EvalError> {
        let shared = Rc::new(unnamed_match("\"x\""));
        let mut errors = RowErrors::new();
        for row in 0..3 {
            errors.record_shared(row, &shared);
        }
        errors.record(5, unnamed_match("\"y\""));
        let unshared = Rc::as_ptr(errors.get_shared(5).expect("recorded"));

        let renamed = errors.within_rule("rule", 6)?;
        let copies: Vec<_> = (0..3)
            .map(|row| Rc::clone(renamed.get_shared(row).expect("recorded")))
            .collect();
        // `shared` is held here too, so the rows that held it get one copy.
        assert!(copies.iter().all(|copy| Rc::ptr_eq(copy, &copies[0])));
        assert!(!Rc::ptr_eq(&copies[0], &shared));
        assert_eq!(named(Some(&shared)), Some(""));
        // The error only row 5 held is renamed where it is.
        assert_eq!(
            Rc::as_ptr(renamed.get_shared(5).expect("recorded")),
            unshared
        );
        for row in [0, 1, 2, 5] {
            assert_eq!(named(renamed.get(row)), Some("rule"));
        }
        Ok(())
    }

    /// The errors the model test records: plain, a `match` failure naming
    /// no rule (renamed by `within_rule`), and one naming its rule already.
    fn pool(index: u8) -> EvalError {
        match index % 5 {
            0 => EvalError::DivisionByZero,
            1 => EvalError::UnknownDerived("d".to_string()),
            2 => unnamed_match("\"x\""),
            3 => EvalError::NoMatchingArm {
                rule: "inner".to_string(),
                subject: "label".to_string(),
                value: "1".to_string(),
                patterns: "2".to_string(),
            },
            _ => unnamed_match(&"\"y\"".repeat(40)),
        }
    }

    #[derive(Clone, Debug)]
    enum Op {
        Record(usize, u8),
        /// Record row `.1`'s error, if it has one, for row `.0`.
        RecordShared(usize, usize),
        RecordAll(Vec<bool>, u8),
        Absorb(Vec<(usize, u8)>),
        Restrict(Vec<bool>),
        WithinRule(u8),
    }

    const MODEL_ROWS: usize = 8;

    fn op() -> impl Strategy<Value = Op> {
        let row = 0..MODEL_ROWS;
        let mask = prop::collection::vec(any::<bool>(), MODEL_ROWS);
        prop_oneof![
            (row.clone(), any::<u8>()).prop_map(|(row, error)| Op::Record(row, error)),
            (row.clone(), row.clone()).prop_map(|(row, from)| Op::RecordShared(row, from)),
            (mask.clone(), any::<u8>()).prop_map(|(mask, error)| Op::RecordAll(mask, error)),
            prop::collection::vec((row, any::<u8>()), 0..4).prop_map(Op::Absorb),
            mask.prop_map(Op::Restrict),
            any::<u8>().prop_map(Op::WithinRule),
        ]
    }

    /// The model's state and the shared one's, in comparable form.
    fn rendered<'e>(errors: impl Iterator<Item = (usize, &'e EvalError)>) -> Vec<String> {
        errors
            .map(|(row, error)| format!("{row}: {error:?}"))
            .collect()
    }

    proptest! {
        /// Differential: sharing errors among rows changes nothing a caller
        /// sees. The shared `RowErrors` agrees after every operation with a
        /// model that holds its own copy of each row's error and renames it
        /// with `EvalError::within_rule`, explain's rule.
        #[test]
        fn shared_errors_behave_as_copied_ones(ops in prop::collection::vec(op(), 0..24)) {
            let mut shared = RowErrors::new();
            let mut model: BTreeMap<usize, EvalError> = BTreeMap::new();
            for op in ops {
                match op {
                    Op::Record(row, error) => {
                        shared.record(row, pool(error));
                        model.entry(row).or_insert_with(|| pool(error));
                    }
                    Op::RecordShared(row, from) => {
                        if let Some(error) = shared.get_shared(from).cloned() {
                            shared.record_shared(row, &error);
                        }
                        if let Some(error) = model.get(&from).cloned() {
                            model.entry(row).or_insert(error);
                        }
                    }
                    Op::RecordAll(bits, error) => {
                        let mask = RowMask::from_bits(bits.clone());
                        shared.record_all(&mask, &pool(error));
                        for row in active(&bits) {
                            model.entry(row).or_insert_with(|| pool(error));
                        }
                    }
                    Op::Absorb(entries) => {
                        let mut later = RowErrors::new();
                        let mut later_model = BTreeMap::new();
                        for (row, error) in entries {
                            later.record(row, pool(error));
                            later_model.entry(row).or_insert_with(|| pool(error));
                        }
                        shared.absorb(later);
                        for (row, error) in later_model {
                            model.entry(row).or_insert(error);
                        }
                    }
                    Op::Restrict(bits) => {
                        shared = shared.restricted_to(&RowMask::from_bits(bits.clone()));
                        let kept = active(&bits);
                        model.retain(|row, _| kept.contains(row));
                    }
                    Op::WithinRule(rule) => {
                        let rule = format!("rule{}", rule % 3);
                        shared = shared.within_rule(&rule, MODEL_ROWS).expect("small errors");
                        model = model
                            .into_iter()
                            .map(|(row, error)| (row, error.within_rule(&rule)))
                            .collect();
                    }
                }
                prop_assert_eq!(
                    rendered(shared.iter_shared().map(|(row, error)| (row, &**error))),
                    rendered(model.iter().map(|(row, error)| (*row, error)))
                );
                prop_assert_eq!(
                    shared.first().map(|(row, error)| format!("{row}: {error:?}")),
                    model.iter().next().map(|(row, error)| format!("{row}: {error:?}"))
                );
            }
        }

        /// Differential: the fallible renderings are the infallible ones,
        /// and a text's rendering is measured exactly, so writing it never
        /// grows the reservation.
        #[test]
        fn fallible_texts_are_the_texts(
            text in any::<String>(),
            other in any::<String>(),
            rows in 0_usize..1000,
        ) {
            let rendered = debug_text(&text, rows).expect("small text");
            prop_assert_eq!(&rendered, &format!("{text:?}"));
            prop_assert_eq!(rendered.capacity(), rendered.len());
            prop_assert_eq!(copied_text(&text, rows).expect("small text"), text.clone());
            for error in [
                EvalError::NoMatchingArm {
                    rule: other.clone(),
                    subject: text.clone(),
                    value: rendered.clone(),
                    patterns: other.clone(),
                },
                EvalError::LifetimePeriodVaryingInput {
                    input: other.clone(),
                    first_period: other.clone(),
                    first_value: rendered.clone(),
                    second_period: text.clone(),
                    second_value: other.clone(),
                },
                EvalError::OverPeriodsTopNPeriodVarying {
                    reduction: "sum_top_n_over_periods",
                    first_period: other.clone(),
                    first_value: rendered.clone(),
                    second_period: text.clone(),
                    second_value: other.clone(),
                },
                EvalError::OverPeriodsTopNOutOfRange {
                    reduction: "sum_top_n_over_periods",
                    n: text.clone(),
                    period_count: rows,
                },
                EvalError::TypeMismatch(text.clone()),
            ] {
                prop_assert_eq!(
                    format!("{:?}", copied_error(&error, rows).expect("small error")),
                    format!("{error:?}")
                );
            }
        }
    }
}
