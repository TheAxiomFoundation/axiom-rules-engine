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
//! Every row mask, and every buffer the dense evaluator sizes by a row count,
//! goes through [`row_buffer`], [`filled_rows`] or [`copied_rows`], which
//! reserve it fallibly. A dense batch can state more rows than any buffer of
//! them fits in memory, and the failed allocation is then an error naming the
//! count, not an aborted process. The bulk evaluator's own column buffers are
//! not covered.

use std::collections::BTreeMap;
use std::fmt;
use std::rc::Rc;
use std::sync::Arc;

use crate::engine::EvalError;

/// The error for a buffer of `rows` rows the allocator refused.
pub(crate) fn refused(rows: usize) -> EvalError {
    EvalError::TypeMismatch(format!(
        "evaluation needs a buffer of {rows} rows, more than this process can allocate"
    ))
}

/// An empty buffer with room for `rows` elements, reserved fallibly: when
/// the allocator cannot grant it, the error names the count.
pub(crate) fn row_buffer<T>(rows: usize) -> Result<Vec<T>, EvalError> {
    let mut buffer = Vec::new();
    buffer.try_reserve_exact(rows).map_err(|_| refused(rows))?;
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

    /// Whether both masks hold exactly the same rows.
    pub(crate) fn same_rows(&self, other: &RowMask) -> bool {
        self.len == other.len
            && self.count == other.count
            && match (&self.bits, &other.bits) {
                (Some(left), Some(right)) => Rc::ptr_eq(left, right) || left == right,
                // A mask holding every row or none keeps no bits, so equal
                // lengths and counts already decide the rest.
                _ => true,
            }
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

    #[cfg(test)]
    pub(crate) fn shares_storage_with(&self, other: &Self) -> bool {
        match (&self.bits, &other.bits) {
            (Some(left), Some(right)) => Rc::ptr_eq(left, right),
            (None, None) => true,
            _ => false,
        }
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

/// A value an error quotes. A text is the column's own shared value, so
/// recording the error copies nothing; any other value is its short label.
#[derive(Clone, Debug)]
pub(crate) enum QuotedValue {
    Text(Arc<str>),
    Label(String),
}

impl QuotedValue {
    /// The value as explain names it: a text in Rust debug quoting, anything
    /// else as its label.
    fn render(&self) -> Option<String> {
        match self {
            QuotedValue::Text(text) => try_format(format_args!("{:?}", &**text)),
            QuotedValue::Label(label) => try_format(format_args!("{label}")),
        }
    }

    /// The same value: one shared text, or equal labels.
    pub(crate) fn same(&self, other: &QuotedValue) -> bool {
        match (self, other) {
            (QuotedValue::Text(left), QuotedValue::Text(right)) => Arc::ptr_eq(left, right),
            (QuotedValue::Label(left), QuotedValue::Label(right)) => left == right,
            _ => false,
        }
    }
}

/// An error whose message quotes a column's value. A text can be far longer
/// than any row buffer, so the message is not built when a row fails: the
/// rows that fail with the same value share one of these, and its message is
/// built, fallibly, only if it is the error reported.
#[derive(Clone, Debug)]
pub(crate) struct Quoted {
    /// The count of the rows it was raised on. A message the allocator
    /// refuses is the refusal of a buffer of this many rows.
    pub(crate) rows: usize,
    pub(crate) error: QuotedError,
}

/// The errors that quote a value, with the fields of the [`EvalError`] each
/// renders to. Labels fixed at compile time are shared, since they can quote
/// a long text literal.
#[derive(Clone, Debug)]
pub(crate) enum QuotedError {
    NoMatchingArm {
        rule: Arc<str>,
        subject: Arc<str>,
        value: QuotedValue,
        patterns: Arc<str>,
    },
    LifetimePeriodVaryingInput {
        input: Arc<str>,
        first_period: Arc<str>,
        first_value: QuotedValue,
        second_period: Arc<str>,
        second_value: QuotedValue,
    },
}

impl Quoted {
    /// Whether `self` is the error `other` would be: the same kind and
    /// fields, quoting the same values.
    pub(crate) fn same(&self, other: &Quoted) -> bool {
        match (&self.error, &other.error) {
            (
                QuotedError::NoMatchingArm {
                    rule,
                    subject,
                    value,
                    patterns,
                },
                QuotedError::NoMatchingArm {
                    rule: other_rule,
                    subject: other_subject,
                    value: other_value,
                    patterns: other_patterns,
                },
            ) => {
                rule == other_rule
                    && Arc::ptr_eq(subject, other_subject)
                    && Arc::ptr_eq(patterns, other_patterns)
                    && value.same(other_value)
            }
            (
                QuotedError::LifetimePeriodVaryingInput {
                    input,
                    first_period,
                    first_value,
                    second_period,
                    second_value,
                },
                QuotedError::LifetimePeriodVaryingInput {
                    input: other_input,
                    first_period: other_first_period,
                    first_value: other_first_value,
                    second_period: other_second_period,
                    second_value: other_second_value,
                },
            ) => {
                input == other_input
                    && first_period == other_first_period
                    && second_period == other_second_period
                    && first_value.same(other_first_value)
                    && second_value.same(other_second_value)
            }
            _ => false,
        }
    }

    /// The error this quotes, its message built with fallibly reserved
    /// strings. When the allocator refuses one, the refusal of a buffer of
    /// `rows` rows instead.
    pub(crate) fn render(&self) -> EvalError {
        let copy = |text: &str| try_format(format_args!("{text}"));
        let rendered = match &self.error {
            QuotedError::NoMatchingArm {
                rule,
                subject,
                value,
                patterns,
            } => (|| {
                Some(EvalError::NoMatchingArm {
                    rule: copy(rule)?,
                    subject: copy(subject)?,
                    value: value.render()?,
                    patterns: copy(patterns)?,
                })
            })(),
            QuotedError::LifetimePeriodVaryingInput {
                input,
                first_period,
                first_value,
                second_period,
                second_value,
            } => (|| {
                Some(EvalError::LifetimePeriodVaryingInput {
                    input: copy(input)?,
                    first_period: copy(first_period)?,
                    first_value: first_value.render()?,
                    second_period: copy(second_period)?,
                    second_value: second_value.render()?,
                })
            })(),
        };
        rendered.unwrap_or_else(|| refused(self.rows))
    }

    /// Name `rule` in a `match` failure that does not name its rule yet.
    fn within_rule(&self, rule: &str) -> Option<Quoted> {
        match &self.error {
            QuotedError::NoMatchingArm {
                rule: existing,
                subject,
                value,
                patterns,
            } if existing.is_empty() => Some(Quoted {
                rows: self.rows,
                error: QuotedError::NoMatchingArm {
                    rule: Arc::from(rule),
                    subject: Arc::clone(subject),
                    value: value.clone(),
                    patterns: Arc::clone(patterns),
                },
            }),
            _ => None,
        }
    }
}

/// `args` written into a string whose capacity is reserved fallibly. `None`
/// when the allocator refuses it. The length is measured first, by writing
/// to a counter, so the one reservation is exact.
fn try_format(args: fmt::Arguments<'_>) -> Option<String> {
    struct Measure(usize);
    impl fmt::Write for Measure {
        fn write_str(&mut self, text: &str) -> fmt::Result {
            self.0 = self.0.saturating_add(text.len());
            Ok(())
        }
    }
    let mut measure = Measure(0);
    fmt::write(&mut measure, args).ok()?;
    let mut text = String::new();
    text.try_reserve_exact(measure.0).ok()?;
    fmt::write(&mut text, args).ok()?;
    Some(text)
}

/// A row's error: one ready to report, or one that quotes a value and is
/// shared by the rows that fail with the same value.
#[derive(Clone, Debug)]
pub(crate) enum RowError {
    Ready(EvalError),
    Quoted(Rc<Quoted>),
}

impl RowError {
    /// The error to report, a quoted one's message built fallibly.
    pub(crate) fn to_eval(&self) -> EvalError {
        match self {
            RowError::Ready(error) => error.clone(),
            RowError::Quoted(quoted) => quoted.render(),
        }
    }
}

impl fmt::Display for RowError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RowError::Ready(error) => error.fmt(formatter),
            RowError::Quoted(quoted) => quoted.render().fmt(formatter),
        }
    }
}

/// The first error of each failing row of a columnar evaluation.
#[derive(Clone, Debug, Default)]
pub(crate) struct RowErrors {
    errors: BTreeMap<usize, RowError>,
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

    pub(crate) fn get(&self, row: usize) -> Option<&RowError> {
        self.errors.get(&row)
    }

    /// Record `error` for `row` unless the row already failed: a row stops at
    /// its first error, so a later one never replaces it.
    pub(crate) fn record(&mut self, row: usize, error: EvalError) {
        self.record_error(row, RowError::Ready(error));
    }

    /// Record `error`, as [`RowErrors::record`] does: a quoted error is
    /// shared, not copied.
    pub(crate) fn record_error(&mut self, row: usize, error: RowError) {
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
            self.record_error(row, error);
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
                .map(|(row, error)| (*row, error.clone())),
        );
        Self { errors }
    }

    /// Name `rule` in each `match` failure that does not name its rule yet:
    /// explain names the innermost rule an error leaves. Renamed in place,
    /// for the same reason `restricted_to` inserts one at a time. Rows that
    /// shared a quoted failure share its renamed one.
    pub(crate) fn within_rule(mut self, rule: &str) -> Self {
        let mut last: Option<(Rc<Quoted>, Rc<Quoted>)> = None;
        for error in self.errors.values_mut() {
            match error {
                RowError::Ready(ready) => {
                    // `within_rule` takes the error by value; a unit variant
                    // holds its place meanwhile.
                    *ready = std::mem::replace(ready, EvalError::DivisionByZero).within_rule(rule);
                }
                RowError::Quoted(quoted) => {
                    if let Some((before, after)) = &last
                        && Rc::ptr_eq(before, quoted)
                    {
                        *quoted = Rc::clone(after);
                        continue;
                    }
                    if let Some(renamed) = quoted.within_rule(rule) {
                        let renamed = Rc::new(renamed);
                        last = Some((Rc::clone(quoted), Rc::clone(&renamed)));
                        *quoted = renamed;
                    }
                }
            }
        }
        self
    }

    /// The lowest failing row and its error.
    pub(crate) fn first(&self) -> Option<(usize, &RowError)> {
        self.errors.iter().next().map(|(row, error)| (*row, error))
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (usize, &RowError)> {
        self.errors.iter().map(|(row, error)| (*row, error))
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
    fn filtering_an_empty_mask_reuses_it() -> Result<(), EvalError> {
        let empty = RowMask::none(1_000);
        let filtered = empty.filter(|_| panic!("no candidate remains"))?;
        assert!(filtered.shares_storage_with(&empty));
        assert_eq!(filtered.len(), 1_000);
        assert_eq!(filtered.count(), 0);

        let cleared = RowMask::from_bits(vec![false; 4]).filter(|_| true)?;
        assert!(cleared.is_empty() && cleared.shares_storage_with(&RowMask::none(4)));
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
        assert!(matches!(
            errors.get(2),
            Some(RowError::Ready(EvalError::DivisionByZero))
        ));

        let mask = RowMask::all(4).without(&errors)?;
        assert_eq!(mask.rows().collect::<Vec<_>>(), vec![0, 1, 3]);

        let mut later = RowErrors::new();
        later.record(0, EvalError::UnknownParameter("p".to_string()));
        errors.absorb(later);
        assert_eq!(errors.first().map(|(row, _)| row), Some(0));
        assert!(errors.restricted_to(&mask).get(2).is_none());
        Ok(())
    }
}
