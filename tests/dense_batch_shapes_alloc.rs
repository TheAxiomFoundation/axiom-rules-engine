//! A stated related row count larger than memory is refused at binding.
//!
//! These cases ask the allocator for petabytes, which no host can grant, so
//! they live in their own test binary: if binding ever reserved its owners
//! infallibly again, the allocation failure would abort this binary only, not
//! hide the results of `dense_batch_shapes`.

use std::collections::HashMap;

use axiom_rules_engine::compile::CompiledProgramArtifact;
use axiom_rules_engine::dense::{
    DenseBatchSpec, DenseColumn, DenseCompiledProgram, DenseRelationBatchSpec,
};
use axiom_rules_engine::engine::EvalError;
use axiom_rules_engine::model::{Period, PeriodKind};
use chrono::NaiveDate;

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

#[test]
fn stated_related_row_count_larger_than_memory_is_refused_at_binding() {
    // 2^50 and 2^55 related rows pass the row bound, but one owner per related
    // row is 8 to 256 PiB. Binding reserves owners fallibly, so the batch is
    // refused instead of aborting the process on the failed allocation.
    for (rulespec, entity, output) in [
        (LISTED_UNIT_RULESPEC, "ListedUnit", "listed_unit_size"),
        (HOUSEHOLD_SIZE_RULESPEC, "Household", "household_size"),
    ] {
        let artifact =
            CompiledProgramArtifact::from_rulespec_str(rulespec).expect("RuleSpec module compiles");
        let program = DenseCompiledProgram::from_artifact(&artifact, Some(entity))
            .expect("dense compilation succeeds");
        let key = program.relations()[0].key.clone();
        for claimed in [1_usize << 55, 1_usize << 50] {
            let result = program.execute(
                &Period {
                    kind: PeriodKind::Month,
                    start: NaiveDate::from_ymd_opt(2026, 1, 1).expect("date"),
                    end: NaiveDate::from_ymd_opt(2026, 1, 31).expect("date"),
                },
                DenseBatchSpec {
                    row_count: 1,
                    inputs: HashMap::<String, DenseColumn>::new(),
                    relations: HashMap::from([(
                        key.clone(),
                        DenseRelationBatchSpec {
                            offsets: vec![0, claimed],
                            inputs: HashMap::new(),
                            related_row_count: Some(claimed),
                        },
                    )]),
                },
                &[output.to_string()],
            );
            match result {
                Err(EvalError::TypeMismatch(message)) => assert!(
                    message.contains(&format!(
                        "claims {claimed} related rows, more than this process can allocate"
                    )),
                    "{message}"
                ),
                other => panic!("expected an allocation refusal, got {other:?}"),
            }
        }
    }
}
