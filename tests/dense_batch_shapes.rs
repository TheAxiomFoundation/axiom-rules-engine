//! Batch-shape validation for the dense evaluator.
//!
//! A dense batch arrives from the caller as plain vectors: a root row count,
//! root columns, and per relation an offsets vector plus related columns. The
//! PyO3 extension builds the same `DenseBatchSpec` from numpy arrays. Every
//! shape `bind_batch` accepts must be one the executor can evaluate without
//! panicking, and every row count the executor sizes a buffer by must be
//! justified: by a supplied column of that length, or by an explicit count the
//! caller states. These tests pin the refusals and the counts that remain
//! accepted.
//!
//! Offsets that claim an unallocatable number of rows use values at or above
//! 2^50 (or `usize::MAX`), never a merely large one: against an evaluator
//! without these checks, an allocation that size fails at once rather than
//! paging through memory.

use std::collections::HashMap;

use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::dense::{
    DenseBatchSpec, DenseColumn, DenseCompiledProgram, DenseExecutionResult, DenseOutputValue,
    DenseRelationBatchSpec, DenseRelationKey,
};
use axiom_rules_engine::engine::EvalError;
use axiom_rules_engine::model::{Period, PeriodKind};
use chrono::NaiveDate;

/// A raw relation counted with `len`: the executor reads nothing but offsets.
const HOUSEHOLD_SIZE_RULESPEC: &str = r#"
format: rulespec/v1
rules:
  - name: member_of_household
    kind: data_relation
    data_relation:
      arity: 2
  - name: household_size
    kind: derived
    entity: Household
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: len(member_of_household)
"#;

/// A filtered derived relation whose predicate reads only a current-entity
/// judgment. The relation has no related input, so nothing but its offsets
/// says how many related rows exist, yet the filter materialises one mask
/// entry per related row.
const ACTIVE_UNIT_RULESPEC: &str = r#"
format: rulespec/v1
rules:
  - name: member_of_household
    kind: data_relation
    data_relation:
      arity: 2
  - name: household_active
    kind: derived
    entity: Household
    dtype: Judgment
    versions:
      - effective_from: 2026-01-01
        formula: application_active
  - name: active_unit
    kind: derived_relation
    derived_relation:
      arity: 2
      source_relation: member_of_household
      entity: ActiveUnit
      member_relation: members
      slot_entities: [Person, Household]
    versions:
      - effective_from: 2026-01-01
        formula: member_of_household and household_active
  - name: active_unit_size
    kind: derived
    entity: ActiveUnit
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: len(members)
"#;

/// A derived relation whose predicate is bare membership. It compiles to a
/// literal `true` mask the length of the related rows, with no root or
/// related input at all.
const LISTED_UNIT_RULESPEC: &str = r#"
format: rulespec/v1
rules:
  - name: member_of_household
    kind: data_relation
    data_relation:
      arity: 2
  - name: listed_unit
    kind: derived_relation
    derived_relation:
      arity: 2
      source_relation: member_of_household
      entity: ListedUnit
      member_relation: members
      slot_entities: [Person, Household]
    versions:
      - effective_from: 2026-01-01
        formula: member_of_household
  - name: listed_unit_size
    kind: derived
    entity: ListedUnit
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: len(members)
"#;

/// A relation aggregate that reads a related input column.
const HOUSEHOLD_INCOME_RULESPEC: &str = r#"
format: rulespec/v1
rules:
  - name: member_of_household
    kind: data_relation
    data_relation:
      arity: 2
  - name: household_income
    kind: derived
    entity: Household
    dtype: Money
    unit: USD
    versions:
      - effective_from: 2026-01-01
        formula: sum(member_of_household.income)
"#;

/// A rule with no inputs and no relations: its row count is whatever the
/// caller states, and every output column is a broadcast of that length.
const CONSTANT_RULESPEC: &str = r#"
format: rulespec/v1
rules:
  - name: flat_amount
    kind: derived
    entity: Person
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: 5
"#;

/// Two root inputs, for the order in which length mismatches are reported.
const TWO_INPUT_RULESPEC: &str = r#"
format: rulespec/v1
rules:
  - name: combined
    kind: derived
    entity: Person
    dtype: Integer
    versions:
      - effective_from: 2026-01-01
        formula: alpha + beta
"#;

/// A per-person-constant input bound outside a reduction, which the lifetime
/// executor checks for period invariance.
const LIFETIME_BASE_RULESPEC: &str = r#"
format: rulespec/v1
rules:
  - name: shifted_total
    kind: derived
    entity: Worker
    dtype: Money
    period: Year
    versions:
      - effective_from: '1990-01-01'
        formula: |-
          sum_over_periods(earnings) + base
"#;

/// An input-sourced `n`, which the lifetime executor requires to be the same
/// in every period.
const LIFETIME_TOP_N_RULESPEC: &str = r#"
format: rulespec/v1
rules:
  - name: top_total
    kind: derived
    entity: Worker
    dtype: Money
    period: Year
    versions:
      - effective_from: '1990-01-01'
        formula: |-
          sum_top_n_over_periods(earnings, n_years)
"#;

fn compile(rulespec: &str, entity: &str) -> DenseCompiledProgram {
    let artifact =
        CompiledProgramArtifact::from_rulespec_str(rulespec).expect("RuleSpec module compiles");
    DenseCompiledProgram::from_artifact(&artifact, Some(entity))
        .expect("dense compilation succeeds")
}

fn month() -> Period {
    Period {
        kind: PeriodKind::Month,
        start: NaiveDate::from_ymd_opt(2026, 1, 1).expect("date"),
        end: NaiveDate::from_ymd_opt(2026, 1, 31).expect("date"),
    }
}

fn year(y: i32) -> Period {
    Period {
        kind: PeriodKind::TaxYear,
        start: NaiveDate::from_ymd_opt(y, 1, 1).expect("date"),
        end: NaiveDate::from_ymd_opt(y, 12, 31).expect("date"),
    }
}

fn household_key() -> DenseRelationKey {
    DenseRelationKey {
        name: "member_of_household".to_string(),
        current_slot: 1,
        related_slot: 0,
    }
}

fn relation(
    offsets: Vec<usize>,
    inputs: Vec<(&str, DenseColumn)>,
) -> HashMap<DenseRelationKey, DenseRelationBatchSpec> {
    relation_stating(offsets, inputs, None)
}

fn relation_stating(
    offsets: Vec<usize>,
    inputs: Vec<(&str, DenseColumn)>,
    related_row_count: Option<usize>,
) -> HashMap<DenseRelationKey, DenseRelationBatchSpec> {
    HashMap::from([(
        household_key(),
        DenseRelationBatchSpec {
            offsets,
            inputs: inputs
                .into_iter()
                .map(|(name, column)| (name.to_string(), column))
                .collect(),
            related_row_count,
        },
    )])
}

fn run(
    program: &DenseCompiledProgram,
    batch: DenseBatchSpec,
    output: &str,
) -> Result<DenseExecutionResult, EvalError> {
    program.execute(&month(), batch, &[output.to_string()])
}

/// The error must be a `TypeMismatch` whose message contains every fragment.
fn assert_type_mismatch(result: Result<DenseExecutionResult, EvalError>, fragments: &[&str]) {
    match result {
        Err(EvalError::TypeMismatch(message)) => {
            for fragment in fragments {
                assert!(
                    message.contains(fragment),
                    "expected `{fragment}` in the error, got: {message}"
                );
            }
        }
        Err(other) => panic!("expected a TypeMismatch, got {other:?}"),
        Ok(result) => panic!(
            "expected a TypeMismatch, but the batch was accepted: {:?}",
            result.outputs
        ),
    }
}

fn integers(result: &DenseExecutionResult, output: &str) -> Vec<i64> {
    match result.outputs.get(output) {
        Some(DenseOutputValue::Scalar(DenseColumn::Integer(values))) => values.clone(),
        other => panic!("expected an integer column for `{output}`, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Related row counts must be justified
// ---------------------------------------------------------------------------

#[test]
fn filtered_relation_without_related_data_refuses_huge_offsets() {
    // The predicate reads a household judgment, so the relation has no related
    // column. Offsets claiming 2^50 related rows used to size the filter mask,
    // and the allocation failure aborted the process.
    let program = compile(ACTIVE_UNIT_RULESPEC, "ActiveUnit");
    for claimed in [1_usize << 50, usize::MAX] {
        let result = run(
            &program,
            DenseBatchSpec {
                row_count: 1,
                inputs: HashMap::from([(
                    "application_active".to_string(),
                    DenseColumn::Bool(vec![true]),
                )]),
                relations: relation(vec![0, claimed], vec![]),
            },
            "active_unit_size",
        );
        assert_type_mismatch(
            result,
            &[
                "member_of_household",
                &claimed.to_string(),
                "no related column",
                "related_row_count",
            ],
        );
    }
}

#[test]
fn membership_only_relation_refuses_huge_offsets() {
    // Bare membership compiles to a literal mask: `vec![true; related_count]`.
    // Nothing else in the batch bounds that count.
    let program = compile(LISTED_UNIT_RULESPEC, "ListedUnit");
    for claimed in [1_usize << 50, usize::MAX] {
        let result = run(
            &program,
            DenseBatchSpec {
                row_count: 1,
                inputs: HashMap::new(),
                relations: relation(vec![0, claimed], vec![]),
            },
            "listed_unit_size",
        );
        assert_type_mismatch(result, &["member_of_household", "no related column"]);
    }
}

#[test]
fn count_only_relation_without_related_data_needs_a_stated_count() {
    // `len` over a raw relation reads only offsets. Without a related column
    // or an explicit count, nothing corroborates the offsets' claim of five
    // related rows, so the batch is refused with instructions.
    let program = compile(HOUSEHOLD_SIZE_RULESPEC, "Household");
    let result = run(
        &program,
        DenseBatchSpec {
            row_count: 2,
            inputs: HashMap::new(),
            relations: relation(vec![0, 3, 5], vec![]),
        },
        "household_size",
    );
    assert_type_mismatch(
        result,
        &[
            "member_of_household",
            "offsets end at 5",
            "no related column",
            "related_row_count",
        ],
    );
}

#[test]
fn stated_related_row_count_justifies_a_relation_without_data() {
    let program = compile(HOUSEHOLD_SIZE_RULESPEC, "Household");
    let result = run(
        &program,
        DenseBatchSpec {
            row_count: 2,
            inputs: HashMap::new(),
            relations: relation_stating(vec![0, 3, 5], vec![], Some(5)),
        },
        "household_size",
    )
    .expect("a stated count justifies the offsets");
    assert_eq!(integers(&result, "household_size"), vec![3, 2]);

    // The filtered relation materialises its mask from the stated count.
    let program = compile(ACTIVE_UNIT_RULESPEC, "ActiveUnit");
    let result = run(
        &program,
        DenseBatchSpec {
            row_count: 2,
            inputs: HashMap::from([(
                "application_active".to_string(),
                DenseColumn::Bool(vec![true, false]),
            )]),
            relations: relation_stating(vec![0, 3, 5], vec![], Some(5)),
        },
        "active_unit_size",
    )
    .expect("a stated count justifies the offsets");
    assert_eq!(integers(&result, "active_unit_size"), vec![3, 0]);
}

#[test]
fn stated_related_row_count_must_match_the_offsets() {
    let program = compile(HOUSEHOLD_SIZE_RULESPEC, "Household");
    for stated in [4, 6, 1 << 50, usize::MAX] {
        let result = run(
            &program,
            DenseBatchSpec {
                row_count: 2,
                inputs: HashMap::new(),
                relations: relation_stating(vec![0, 3, 5], vec![], Some(stated)),
            },
            "household_size",
        );
        assert_type_mismatch(
            result,
            &[
                "member_of_household",
                "offsets end at 5",
                &format!("related_row_count is {stated}"),
            ],
        );
    }
}

#[test]
fn stated_related_row_count_must_match_the_related_columns() {
    // The offsets and the stated count agree on 3, but the column has 2 rows.
    let program = compile(HOUSEHOLD_INCOME_RULESPEC, "Household");
    let result = run(
        &program,
        DenseBatchSpec {
            row_count: 2,
            inputs: HashMap::new(),
            relations: relation_stating(
                vec![0, 2, 3],
                vec![("income", DenseColumn::Integer(vec![100, 50]))],
                Some(3),
            ),
        },
        "household_income",
    );
    assert_type_mismatch(result, &["`income`", "length 2", "3 related rows"]);
}

#[test]
fn stated_related_row_count_beyond_any_allocation_is_refused() {
    // A stated count is trusted, but not past what an executor buffer could
    // hold: the filtered relation would size its mask by it.
    let program = compile(ACTIVE_UNIT_RULESPEC, "ActiveUnit");
    let claimed = usize::MAX;
    let result = run(
        &program,
        DenseBatchSpec {
            row_count: 1,
            inputs: HashMap::from([(
                "application_active".to_string(),
                DenseColumn::Bool(vec![true]),
            )]),
            relations: relation_stating(vec![0, claimed], vec![], Some(claimed)),
        },
        "active_unit_size",
    );
    assert_type_mismatch(
        result,
        &["member_of_household", "claims", &claimed.to_string()],
    );
}

#[test]
fn stated_related_row_count_is_reported_without_materialising_rows() {
    // `len` over a raw relation reads only offsets, so a large stated count
    // costs nothing: the count is the caller's claim, and it is reported as
    // stated.
    let program = compile(HOUSEHOLD_SIZE_RULESPEC, "Household");
    let claimed = 1_usize << 50;
    let result = run(
        &program,
        DenseBatchSpec {
            row_count: 1,
            inputs: HashMap::new(),
            relations: relation_stating(vec![0, claimed], vec![], Some(claimed)),
        },
        "household_size",
    )
    .expect("a stated count is trusted");
    assert_eq!(integers(&result, "household_size"), vec![1_i64 << 50]);
}

#[test]
fn zero_related_rows_need_no_justification() {
    // Offsets that claim no related rows allocate nothing, so an empty
    // relation is accepted without columns or a stated count.
    let program = compile(HOUSEHOLD_SIZE_RULESPEC, "Household");
    let result = run(
        &program,
        DenseBatchSpec {
            row_count: 2,
            inputs: HashMap::new(),
            relations: relation(vec![0, 0, 0], vec![]),
        },
        "household_size",
    )
    .expect("an empty relation is valid");
    assert_eq!(integers(&result, "household_size"), vec![0, 0]);
}

#[test]
fn related_data_justifies_the_offsets() {
    // Offsets ending at 3 are corroborated by a three-row related column.
    let program = compile(HOUSEHOLD_INCOME_RULESPEC, "Household");
    let result = run(
        &program,
        DenseBatchSpec {
            row_count: 2,
            inputs: HashMap::new(),
            relations: relation(
                vec![0, 2, 3],
                vec![("income", DenseColumn::Integer(vec![100, 50, 7]))],
            ),
        },
        "household_income",
    )
    .expect("a data-backed relation is valid");
    match result.outputs.get("household_income") {
        Some(DenseOutputValue::Scalar(DenseColumn::Decimal(values))) => {
            assert_eq!(
                values.iter().map(ToString::to_string).collect::<Vec<_>>(),
                ["150", "7"]
            );
        }
        other => panic!("unexpected household income {other:?}"),
    }
}

#[test]
fn every_supplied_related_column_must_match_the_offsets() {
    // `income` is the only related column the program reads, but a supplied
    // column of any other length is a malformed batch too, as it already is
    // for root columns.
    let program = compile(HOUSEHOLD_INCOME_RULESPEC, "Household");
    let result = run(
        &program,
        DenseBatchSpec {
            row_count: 2,
            inputs: HashMap::new(),
            relations: relation(
                vec![0, 2, 3],
                vec![
                    ("income", DenseColumn::Integer(vec![100, 50, 7])),
                    ("unused_flag", DenseColumn::Bool(vec![true])),
                ],
            ),
        },
        "household_income",
    );
    assert_type_mismatch(
        result,
        &["unused_flag", "member_of_household", "length 1", "3"],
    );
}

// ---------------------------------------------------------------------------
// Counts never wrap
// ---------------------------------------------------------------------------

#[test]
fn count_related_never_wraps_to_a_negative_count() {
    // Offsets [0, usize::MAX] used to report a household size of -1: the
    // unmasked count cast `usize::MAX as i64`.
    let program = compile(HOUSEHOLD_SIZE_RULESPEC, "Household");
    let result = run(
        &program,
        DenseBatchSpec {
            row_count: 1,
            inputs: HashMap::new(),
            relations: relation(vec![0, usize::MAX], vec![]),
        },
        "household_size",
    );
    assert_type_mismatch(result, &["member_of_household"]);
}

#[test]
fn row_count_at_usize_max_is_refused() {
    // `row_count + 1` wrapped to 0 in release builds (and panicked in debug
    // builds), so an empty offsets vector passed the length check. The
    // count-only program then returned zero outputs for usize::MAX rows; the
    // filtered one panicked at its first row-sized allocation.
    for (rulespec, entity, output) in [
        (HOUSEHOLD_SIZE_RULESPEC, "Household", "household_size"),
        (LISTED_UNIT_RULESPEC, "ListedUnit", "listed_unit_size"),
    ] {
        let program = compile(rulespec, entity);
        let result = run(
            &program,
            DenseBatchSpec {
                row_count: usize::MAX,
                inputs: HashMap::new(),
                relations: relation(vec![], vec![]),
            },
            output,
        );
        assert_type_mismatch(result, &[&usize::MAX.to_string()]);
    }
}

#[test]
fn row_count_beyond_any_allocation_is_refused_without_relations() {
    // With no input and no relation, the stated row count sizes every
    // broadcast column. A count no `Vec` can hold used to panic with
    // "capacity overflow"; it is refused before execution instead.
    let program = compile(CONSTANT_RULESPEC, "Person");
    for row_count in [usize::MAX, isize::MAX as usize] {
        let result = run(
            &program,
            DenseBatchSpec {
                row_count,
                inputs: HashMap::new(),
                relations: HashMap::new(),
            },
            "flat_amount",
        );
        assert_type_mismatch(result, &["row_count", &row_count.to_string()]);
    }
}

#[test]
fn stated_row_count_is_still_honoured_for_broadcast_rules() {
    let program = compile(CONSTANT_RULESPEC, "Person");
    let result = run(
        &program,
        DenseBatchSpec {
            row_count: 3,
            inputs: HashMap::new(),
            relations: HashMap::new(),
        },
        "flat_amount",
    )
    .expect("a small stated row count is valid");
    assert_eq!(integers(&result, "flat_amount"), vec![5, 5, 5]);
}

// ---------------------------------------------------------------------------
// Offsets errors say what is wrong
// ---------------------------------------------------------------------------

#[test]
fn malformed_offsets_are_described() {
    let program = compile(HOUSEHOLD_INCOME_RULESPEC, "Household");
    let income = || vec![("income", DenseColumn::Integer(vec![1, 2, 3]))];
    let batch = |offsets: Vec<usize>| DenseBatchSpec {
        row_count: 2,
        inputs: HashMap::new(),
        relations: relation(offsets, income()),
    };

    assert_type_mismatch(
        run(&program, batch(vec![0, 3]), "household_income"),
        &["member_of_household", "length 3", "has 2"],
    );
    assert_type_mismatch(
        run(&program, batch(vec![1, 2, 3]), "household_income"),
        &["member_of_household", "start at 0", "starts at 1"],
    );
    assert_type_mismatch(
        run(&program, batch(vec![0, 3, 2]), "household_income"),
        &[
            "member_of_household",
            "non-decreasing",
            "offsets[1] = 3",
            "offsets[2] = 2",
        ],
    );
}

#[test]
fn length_mismatches_are_reported_in_a_fixed_order() {
    // Root columns are checked in name order, so a batch with two malformed
    // columns reports the same one every time (a HashMap's iteration order
    // differs between instances).
    let program = compile(TWO_INPUT_RULESPEC, "Person");
    for _ in 0..32 {
        let result = run(
            &program,
            DenseBatchSpec {
                row_count: 2,
                inputs: HashMap::from([
                    ("beta".to_string(), DenseColumn::Integer(vec![1])),
                    ("alpha".to_string(), DenseColumn::Integer(vec![1, 2, 3])),
                ]),
                relations: HashMap::new(),
            },
            "combined",
        );
        assert_type_mismatch(result, &["`alpha`", "length 3", "row_count is 2"]);
    }
}

// ---------------------------------------------------------------------------
// Lifetime error labels never index past a column
// ---------------------------------------------------------------------------

fn lifetime_batch(columns: Vec<(&str, DenseColumn)>) -> DenseBatchSpec {
    let row_count = columns.first().map(|(_, column)| column.len()).unwrap_or(0);
    DenseBatchSpec {
        row_count,
        inputs: columns
            .into_iter()
            .map(|(name, column)| (name.to_string(), column))
            .collect(),
        relations: HashMap::new(),
    }
}

fn lifetime_error(
    rulespec: &str,
    output: &str,
    batches: Vec<DenseBatchSpec>,
    f64_mode: bool,
) -> EvalError {
    let program = compile(rulespec, "Worker");
    let periods = vec![year(2001), year(2002)];
    let outputs = [output.to_string()];
    let result = if f64_mode {
        program.execute_lifetime_f64(&periods, batches, &outputs)
    } else {
        program.execute_lifetime(&periods, batches, &outputs)
    };
    result.expect_err("a dtype change between periods must error")
}

#[test]
fn zero_row_input_that_changes_dtype_reports_instead_of_panicking() {
    // With zero rows there is no row 0 to quote, but a dtype change is still a
    // structurally different value. The error used to panic while formatting
    // `values[0]` of an empty column.
    for (first, second, first_label, second_label) in [
        (
            DenseColumn::Integer(vec![]),
            DenseColumn::Text(vec![]),
            "an empty integer column",
            "an empty text column",
        ),
        (
            DenseColumn::Bool(vec![]),
            DenseColumn::Float(vec![]),
            "an empty bool column",
            "an empty float column",
        ),
    ] {
        for f64_mode in [false, true] {
            let error = lifetime_error(
                LIFETIME_BASE_RULESPEC,
                "shifted_total",
                vec![
                    lifetime_batch(vec![
                        ("earnings", DenseColumn::Float(vec![])),
                        ("base", first.clone()),
                    ]),
                    lifetime_batch(vec![
                        ("earnings", DenseColumn::Float(vec![])),
                        ("base", second.clone()),
                    ]),
                ],
                f64_mode,
            );
            let message = error.to_string();
            assert!(
                matches!(error, EvalError::LifetimePeriodVaryingInput { .. })
                    && message.contains("`base`")
                    && message.contains(first_label)
                    && message.contains(second_label),
                "unexpected error: {message}"
            );
        }
    }
}

#[test]
fn zero_row_top_n_that_changes_dtype_reports_instead_of_panicking() {
    let error = lifetime_error(
        LIFETIME_TOP_N_RULESPEC,
        "top_total",
        vec![
            lifetime_batch(vec![
                ("earnings", DenseColumn::Float(vec![])),
                ("n_years", DenseColumn::Integer(vec![])),
            ]),
            lifetime_batch(vec![
                ("earnings", DenseColumn::Float(vec![])),
                ("n_years", DenseColumn::Bool(vec![])),
            ]),
        ],
        false,
    );
    let message = error.to_string();
    assert!(
        matches!(error, EvalError::OverPeriodsTopNPeriodVarying { .. })
            && message.contains("an empty integer column")
            && message.contains("an empty bool column"),
        "unexpected error: {message}"
    );
}

#[test]
fn unrepresentable_value_is_reported_at_its_own_row() {
    // Row 0 is 2000 in both periods; row 1 is NaN, which cannot be proven
    // invariant. The error used to quote row 0 (2000 and 2000) as the
    // difference.
    let error = lifetime_error(
        LIFETIME_BASE_RULESPEC,
        "shifted_total",
        vec![
            lifetime_batch(vec![
                ("earnings", DenseColumn::Float(vec![1.0, 1.0])),
                ("base", DenseColumn::Float(vec![2000.0, f64::NAN])),
            ]),
            lifetime_batch(vec![
                ("earnings", DenseColumn::Float(vec![1.0, 1.0])),
                ("base", DenseColumn::Float(vec![2000.0, f64::NAN])),
            ]),
        ],
        true,
    );
    let message = error.to_string();
    assert!(
        matches!(error, EvalError::LifetimePeriodVaryingInput { .. })
            && message.contains("it is NaN but")
            && !message.contains("2000"),
        "unexpected error: {message}"
    );
}
