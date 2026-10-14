//! Batch-shape validation for the dense evaluator.
//!
//! A dense batch arrives from the caller as plain vectors: a root row count,
//! root columns, and per relation an offsets vector plus related columns. The
//! PyO3 extension builds the same `DenseBatchSpec` from numpy arrays. Binding
//! sizes buffers by the root row count and each relation's related row count
//! (its last offset), so every count must be justified: by a supplied column of
//! that length, or by a count the caller states. These tests pin the refusals
//! and the counts that remain accepted.
//!
//! Where a refusal guards an allocation, the offending count is `usize::MAX` or
//! another value whose buffer no `Vec` can hold, and it comes first in each
//! loop: against a binder without the check, that allocation panics with
//! "capacity overflow", a named test failure, before any count that would ask
//! the allocator for real memory.

use std::collections::HashMap;

use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::dense::{
    DenseBatchSpec, DenseColumn, DenseCompiledProgram, DenseExecutionResult, DenseOutputValue,
    DenseRelationBatchSpec, DenseRelationKey,
};
use axiom_rules_engine::engine::EvalError;
use axiom_rules_engine::model::{Period, PeriodKind};
use chrono::NaiveDate;

/// A raw relation counted with `len`: nothing reads a related column.
const HOUSEHOLD_SIZE_RULESPEC: &str = r#"
format: rulespec/v1
rules:
  - name: member_of_household
    kind: data_relation
    data_relation:
      arity: 2
      arguments: [Person, Household]
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
/// says how many related rows exist.
const ACTIVE_UNIT_RULESPEC: &str = r#"
format: rulespec/v1
rules:
  - name: member_of_household
    kind: data_relation
    data_relation:
      arity: 2
      arguments: [Person, Household]
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

/// A derived relation whose predicate is bare membership, with no root or
/// related input at all.
const LISTED_UNIT_RULESPEC: &str = r#"
format: rulespec/v1
rules:
  - name: member_of_household
    kind: data_relation
    data_relation:
      arity: 2
      arguments: [Person, Household]
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
      arguments: [Person, Household]
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
/// caller states, and its output is a broadcast of that length.
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

/// A text broadcast: its buffer holds one `String` per row, the widest
/// element a dense buffer holds on 64-bit targets.
const TEXT_RULESPEC: &str = r#"
format: rulespec/v1
rules:
  - name: label
    kind: derived
    entity: Person
    dtype: Text
    versions:
      - effective_from: 2026-01-01
        formula: |-
          "abc"
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

/// The program's only relation key (the source relation every derived
/// relation over it is keyed to).
fn only_key(program: &DenseCompiledProgram) -> DenseRelationKey {
    let mut keys = program
        .relations()
        .iter()
        .map(|schema| schema.key.clone())
        .collect::<Vec<_>>();
    keys.dedup();
    assert_eq!(keys.len(), 1, "expected one relation key, got {keys:?}");
    keys.pop().expect("one key")
}

fn relation(
    program: &DenseCompiledProgram,
    offsets: Vec<usize>,
    inputs: Vec<(&str, DenseColumn)>,
) -> HashMap<DenseRelationKey, DenseRelationBatchSpec> {
    relation_stating(program, offsets, inputs, None)
}

fn relation_stating(
    program: &DenseCompiledProgram,
    offsets: Vec<usize>,
    inputs: Vec<(&str, DenseColumn)>,
    related_row_count: Option<usize>,
) -> HashMap<DenseRelationKey, DenseRelationBatchSpec> {
    HashMap::from([(
        only_key(program),
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
fn filtered_relation_without_related_data_refuses_unconfirmed_offsets() {
    // The predicate reads a household judgment, so the relation has no related
    // column. Binding sizes the relation's owners by the last offset, so
    // offsets claiming usize::MAX or 2^50 related rows used to panic with
    // "capacity overflow" or abort on the failed allocation.
    let program = compile(ACTIVE_UNIT_RULESPEC, "ActiveUnit");
    for claimed in [usize::MAX, 1_usize << 50] {
        let result = run(
            &program,
            DenseBatchSpec {
                row_count: 1,
                inputs: HashMap::from([(
                    "application_active".to_string(),
                    DenseColumn::Bool(vec![true]),
                )]),
                relations: relation(&program, vec![0, claimed], vec![]),
            },
            "active_unit_size",
        );
        assert_type_mismatch(
            result,
            &[
                "member_of_household",
                &format!("offsets end at {claimed}"),
                "no related column",
                "related_row_count",
            ],
        );
    }
}

#[test]
fn membership_only_relation_refuses_unconfirmed_offsets() {
    let program = compile(LISTED_UNIT_RULESPEC, "ListedUnit");
    for claimed in [usize::MAX, 1_usize << 50] {
        let result = run(
            &program,
            DenseBatchSpec {
                row_count: 1,
                inputs: HashMap::new(),
                relations: relation(&program, vec![0, claimed], vec![]),
            },
            "listed_unit_size",
        );
        assert_type_mismatch(result, &["member_of_household", "no related column"]);
    }
}

#[test]
fn count_only_relation_without_related_data_needs_a_stated_count() {
    // `len` over a raw relation reads no related column. Without one, or a
    // stated count, nothing corroborates the offsets' claim of five related
    // rows, so the batch is refused with instructions.
    let program = compile(HOUSEHOLD_SIZE_RULESPEC, "Household");
    let result = run(
        &program,
        DenseBatchSpec {
            row_count: 2,
            inputs: HashMap::new(),
            relations: relation(&program, vec![0, 3, 5], vec![]),
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
fn unconfirmed_huge_offsets_are_refused_before_counting() {
    // #227, no related input: offsets [0, usize::MAX] used to panic with
    // "capacity overflow" allocating owners, and [0, u32::MAX] to abort on a
    // ~34 GB request. (Before owners existed, the unmasked count cast
    // usize::MAX to -1.) No related column or stated count confirms either.
    let program = compile(HOUSEHOLD_SIZE_RULESPEC, "Household");
    for claimed in [usize::MAX, u32::MAX as usize] {
        let result = run(
            &program,
            DenseBatchSpec {
                row_count: 1,
                inputs: HashMap::new(),
                relations: relation(&program, vec![0, claimed], vec![]),
            },
            "household_size",
        );
        assert_type_mismatch(
            result,
            &[
                "member_of_household",
                &format!("offsets end at {claimed}"),
                "no related column",
            ],
        );
    }
}

#[test]
fn omitted_related_columns_do_not_confirm_huge_offsets() {
    // #227, omitted column: the program reads `income`, but an absent column
    // is a missing input on the rows that read it, not a confirmation of the
    // offsets. Binding refuses them before allocating owners.
    let program = compile(HOUSEHOLD_INCOME_RULESPEC, "Household");
    for claimed in [usize::MAX, u32::MAX as usize] {
        let result = run(
            &program,
            DenseBatchSpec {
                row_count: 1,
                inputs: HashMap::new(),
                relations: relation(&program, vec![0, claimed], vec![]),
            },
            "household_income",
        );
        assert_type_mismatch(
            result,
            &[
                "member_of_household",
                &format!("offsets end at {claimed}"),
                "no related column",
            ],
        );
    }
}

#[test]
fn stated_related_row_count_justifies_a_relation_without_data() {
    let program = compile(HOUSEHOLD_SIZE_RULESPEC, "Household");
    let result = run(
        &program,
        DenseBatchSpec {
            row_count: 2,
            inputs: HashMap::new(),
            relations: relation_stating(&program, vec![0, 3, 5], vec![], Some(5)),
        },
        "household_size",
    )
    .expect("a stated count justifies the offsets");
    assert_eq!(integers(&result, "household_size"), vec![3, 2]);

    // The filtered relation reads its mask through the stated count too.
    let program = compile(ACTIVE_UNIT_RULESPEC, "ActiveUnit");
    let result = run(
        &program,
        DenseBatchSpec {
            row_count: 2,
            inputs: HashMap::from([(
                "application_active".to_string(),
                DenseColumn::Bool(vec![true, false]),
            )]),
            relations: relation_stating(&program, vec![0, 3, 5], vec![], Some(5)),
        },
        "active_unit_size",
    )
    .expect("a stated count justifies the offsets");
    assert_eq!(integers(&result, "active_unit_size"), vec![3, 0]);
}

#[test]
fn stated_related_row_count_must_match_the_offsets() {
    let program = compile(HOUSEHOLD_SIZE_RULESPEC, "Household");
    for stated in [usize::MAX, 4, 6, 1 << 50] {
        let result = run(
            &program,
            DenseBatchSpec {
                row_count: 2,
                inputs: HashMap::new(),
                relations: relation_stating(&program, vec![0, 3, 5], vec![], Some(stated)),
            },
            "household_size",
        );
        assert_type_mismatch(
            result,
            &[
                "member_of_household",
                &format!("offsets end at 5 but related_row_count is {stated}"),
            ],
        );
    }
}

#[test]
fn stated_related_row_count_does_not_excuse_a_wrong_length_column() {
    // The offsets and the stated count agree on 3, but the column has 2 rows.
    let program = compile(HOUSEHOLD_INCOME_RULESPEC, "Household");
    let result = run(
        &program,
        DenseBatchSpec {
            row_count: 2,
            inputs: HashMap::new(),
            relations: relation_stating(
                &program,
                vec![0, 2, 3],
                vec![("income", DenseColumn::Integer(vec![100, 50]))],
                Some(3),
            ),
        },
        "household_income",
    );
    assert_type_mismatch(
        result,
        &[
            "dense relation input `income` for `member_of_household` has length 2 but related row count is 3",
        ],
    );
}

#[test]
fn stated_related_row_count_above_the_row_bound_is_refused() {
    // A stated count is trusted, but not past what a dense buffer can hold.
    let program = compile(ACTIVE_UNIT_RULESPEC, "ActiveUnit");
    for claimed in [usize::MAX, isize::MAX as usize] {
        let result = run(
            &program,
            DenseBatchSpec {
                row_count: 1,
                inputs: HashMap::from([(
                    "application_active".to_string(),
                    DenseColumn::Bool(vec![true]),
                )]),
                relations: relation_stating(&program, vec![0, claimed], vec![], Some(claimed)),
            },
            "active_unit_size",
        );
        assert_type_mismatch(
            result,
            &[
                "member_of_household",
                "claims",
                &claimed.to_string(),
                "can hold",
            ],
        );
    }
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
            relations: relation(&program, vec![0, 0, 0], vec![]),
        },
        "household_size",
    )
    .expect("an empty relation is valid");
    assert_eq!(integers(&result, "household_size"), vec![0, 0]);
}

#[test]
fn a_read_related_column_justifies_the_offsets() {
    let program = compile(HOUSEHOLD_INCOME_RULESPEC, "Household");
    let result = run(
        &program,
        DenseBatchSpec {
            row_count: 2,
            inputs: HashMap::new(),
            relations: relation(
                &program,
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
fn an_unread_related_column_of_the_right_length_justifies_the_offsets() {
    // The program reads no related column, but the caller supplied one with a
    // value per related row: that corroborates the offsets.
    let program = compile(HOUSEHOLD_SIZE_RULESPEC, "Household");
    let result = run(
        &program,
        DenseBatchSpec {
            row_count: 2,
            inputs: HashMap::new(),
            relations: relation(
                &program,
                vec![0, 3, 5],
                vec![("age", DenseColumn::Integer(vec![30, 12, 40, 70, 8]))],
            ),
        },
        "household_size",
    )
    .expect("a supplied column confirms the offsets");
    assert_eq!(integers(&result, "household_size"), vec![3, 2]);
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
                &program,
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
        &[
            "dense relation input `unused_flag` for `member_of_household` has length 1 but related row count is 3",
        ],
    );
}

#[test]
fn a_wrong_length_column_is_named_instead_of_confirming_the_offsets() {
    // A supplied column that disagrees with the offsets confirms nothing; the
    // error names it rather than saying no column was supplied.
    let program = compile(HOUSEHOLD_SIZE_RULESPEC, "Household");
    let result = run(
        &program,
        DenseBatchSpec {
            row_count: 1,
            inputs: HashMap::new(),
            relations: relation(
                &program,
                vec![0, usize::MAX],
                vec![("age", DenseColumn::Integer(vec![30]))],
            ),
        },
        "household_size",
    );
    assert_type_mismatch(
        result,
        &[&format!(
            "dense relation input `age` for `member_of_household` has length 1 but related row count is {}",
            usize::MAX
        )],
    );
}

// ---------------------------------------------------------------------------
// Root row counts
// ---------------------------------------------------------------------------

#[test]
fn row_count_at_usize_max_is_refused() {
    // `row_count + 1` wrapped to 0 in release builds (and panicked in debug
    // builds), so an empty offsets vector passed the length check.
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
                relations: relation(&program, vec![], vec![]),
            },
            output,
        );
        assert_type_mismatch(result, &["row_count", &usize::MAX.to_string()]);
    }
}

#[test]
fn row_count_above_the_row_bound_is_refused_without_relations() {
    // With no input and no relation, the stated row count sizes the broadcast.
    // A count no `Vec` can hold used to panic with "capacity overflow"; it is
    // refused before execution instead.
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
        assert_type_mismatch(result, &["row_count", &row_count.to_string(), "exceeds"]);
    }
}

/// Pins the row bound's divisor: one row past `isize::MAX / size_of::<String>()`
/// is refused. A bound computed for a narrower element would accept it, and the
/// text broadcast's `vec![String; n]` would panic with "capacity overflow"
/// without allocating.
#[cfg(target_pointer_width = "64")]
#[test]
fn text_broadcast_one_row_past_the_bound_is_refused() {
    let program = compile(TEXT_RULESPEC, "Person");
    let row_count = isize::MAX as usize / size_of::<String>() + 1;
    let result = run(
        &program,
        DenseBatchSpec {
            row_count,
            inputs: HashMap::new(),
            relations: HashMap::new(),
        },
        "label",
    );
    assert_type_mismatch(result, &["row_count", &row_count.to_string(), "exceeds"]);
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
    let batch = |offsets: Vec<usize>| DenseBatchSpec {
        row_count: 2,
        inputs: HashMap::new(),
        relations: relation(
            &program,
            offsets,
            vec![("income", DenseColumn::Integer(vec![1, 2, 3]))],
        ),
    };

    assert_type_mismatch(
        run(&program, batch(vec![0, 3]), "household_income"),
        &["member_of_household", "offsets must have length 3", "has 2"],
    );
    assert_type_mismatch(
        run(&program, batch(vec![1, 2, 3]), "household_income"),
        &[
            "member_of_household",
            "offsets must start at 0",
            "starts at 1",
        ],
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
fn root_length_mismatches_are_reported_in_name_order() {
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

#[test]
fn unread_related_length_mismatches_are_reported_in_name_order() {
    let program = compile(HOUSEHOLD_INCOME_RULESPEC, "Household");
    for _ in 0..32 {
        let result = run(
            &program,
            DenseBatchSpec {
                row_count: 2,
                inputs: HashMap::new(),
                relations: relation(
                    &program,
                    vec![0, 2, 3],
                    vec![
                        ("income", DenseColumn::Integer(vec![100, 50, 7])),
                        ("zeta", DenseColumn::Bool(vec![true])),
                        ("eta", DenseColumn::Bool(vec![true, false])),
                    ],
                ),
            },
            "household_income",
        );
        assert_type_mismatch(result, &["`eta`", "length 2"]);
    }
}

// ---------------------------------------------------------------------------
// Lifetime period invariance
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

fn lifetime(
    batches: Vec<DenseBatchSpec>,
    f64_mode: bool,
) -> Result<DenseExecutionResult, EvalError> {
    let program = compile(LIFETIME_BASE_RULESPEC, "Worker");
    let periods = vec![year(2001), year(2002)];
    let outputs = ["shifted_total".to_string()];
    if f64_mode {
        program.execute_lifetime_f64(&periods, batches, &outputs)
    } else {
        program.execute_lifetime(&periods, batches, &outputs)
    }
}

#[test]
fn zero_row_input_that_changes_dtype_binds_without_panicking() {
    // With zero rows no row differs. The error label used to index row 0 of
    // an empty column; now no row is compared and the empty result binds.
    for (first, second) in [
        (DenseColumn::Integer(vec![]), DenseColumn::Text(vec![])),
        (DenseColumn::Bool(vec![]), DenseColumn::Float(vec![])),
    ] {
        for f64_mode in [false, true] {
            let result = lifetime(
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
            )
            .expect("zero rows have nothing to compare");
            assert_eq!(result.row_count, 0);
        }
    }
}

#[test]
fn input_that_changes_dtype_reports_both_values() {
    let error = lifetime(
        vec![
            lifetime_batch(vec![
                ("earnings", DenseColumn::Float(vec![1.0])),
                ("base", DenseColumn::Integer(vec![1985])),
            ]),
            lifetime_batch(vec![
                ("earnings", DenseColumn::Float(vec![1.0])),
                ("base", DenseColumn::Text(vec!["1985".to_string()])),
            ]),
        ],
        false,
    )
    .expect_err("a dtype change is not period-invariant");
    match error {
        EvalError::LifetimePeriodVaryingInput {
            first_value,
            second_value,
            ..
        } => {
            assert_eq!(first_value, "1985");
            assert_eq!(second_value, "\"1985\"");
        }
        other => panic!("unexpected error {other:?}"),
    }
}

#[test]
fn unrepresentable_value_is_reported_at_its_own_row() {
    // Row 0 is 2000 in both periods; row 1 is NaN, which cannot be shown to
    // be invariant. The error quotes row 1, not row 0.
    let error = lifetime(
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
    )
    .expect_err("NaN is not provably invariant");
    match error {
        EvalError::LifetimePeriodVaryingInput {
            first_value,
            second_value,
            ..
        } => {
            assert_eq!(first_value, "NaN");
            assert_eq!(second_value, "NaN");
        }
        other => panic!("unexpected error {other:?}"),
    }
}
