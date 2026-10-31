//! Literal hand-worked households from docs/relation-roles.md section 8.
#[path = "support/snap_household.rs"]
mod support;

use axiom_rules_engine::dense::DenseCompiledProgram;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use support::*;

fn residence(ids: &[&str]) -> Value {
    let mut inputs = Vec::new();
    for (i, id) in ids.iter().enumerate() {
        for (name, value) in [
            ("meal_group", integer(i as i64)),
            ("age", integer(40)),
            ("snap_household_exception_may_apply", judgment(false)),
            ("snap_ineligible_member_under_b7", judgment(false)),
            ("member_token", integer(1 << i)),
        ] {
            inputs.push(input(id, name, value));
        }
    }
    json!({"inputs":inputs,"relations":ids.iter().map(|id|tuple("member_of_household",id,"home")).collect::<Vec<_>>()})
}
fn set(data: &mut Value, id: &str, name: &str, value: Value) {
    let inputs = data["inputs"].as_array_mut().unwrap();
    inputs.retain(|record| !(record["entity_id"] == id && record["name"] == name));
    inputs.push(input(id, name, value));
}
fn edge(data: &mut Value, name: &str, a: &str, b: &str) {
    data["relations"]
        .as_array_mut()
        .unwrap()
        .push(tuple(name, a, b));
}
fn units(data: &mut Value, ids: &[&str], unit: i64) {
    for id in ids {
        set(data, id, "snap_household_unit", integer(unit));
    }
}
fn literal(ids: &[&str]) -> BTreeSet<String> {
    ids.iter().map(|id| (*id).to_owned()).collect()
}
/// Each row compares literal table sets, modes, both depths, repeated runs,
/// and reversed input/tuple/roster order (including complete explain traces).
fn check(data: &Value, ids: &[&str], expected: &[(&str, &[&str])], output: &str) {
    let p = program();
    let count = output.replace("_mask", "_count");
    let outputs_requested = [output, count.as_str()];
    let applicants = expected.iter().map(|(id, _)| *id).collect::<Vec<_>>();
    let mut mode_outputs = None;
    for mode in ["explain", "fast"] {
        let response = run(mode, &p, data, &applicants, &outputs_requested).unwrap();
        assert_eq!(
            response,
            run(mode, &p, data, &applicants, &outputs_requested).unwrap(),
            "repeat {mode}"
        );
        assert_eq!(
            response,
            run(mode, &p, &permuted(data), &applicants, &outputs_requested).unwrap(),
            "order {mode}"
        );
        for (row, (_, want)) in expected.iter().enumerate() {
            assert_eq!(
                members(&response, row, output, ids),
                literal(want),
                "{mode} {}",
                applicants[row]
            );
            assert_eq!(number(&response, row, &count), want.len() as i64);
        }
        let outputs = response["results"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["outputs"].clone())
            .collect::<Vec<_>>();
        if let Some(explain) = &mode_outputs {
            assert_eq!(explain, &outputs);
        }
        mode_outputs = Some(outputs);
    }
}
fn missing_ids(data: &Value, applicants: &[&str]) {
    for applicant in applicants {
        for mode in ["explain", "fast"] {
            let error =
                run(mode, &program(), data, &[*applicant], &["household_mask"]).unwrap_err();
            assert!(
                error.contains("snap_household_unit"),
                "{mode} {applicant}: {error}"
            );
            assert_eq!(
                error,
                run(
                    mode,
                    &program(),
                    &permuted(data),
                    &[*applicant],
                    &["household_mask"]
                )
                .unwrap_err()
            );
        }
    }
}
#[test]
fn fixture_compiles_strictly_and_dense_refuses_by_name() {
    let artifact = artifact();
    let error = DenseCompiledProgram::from_artifact(&artifact, Some("Person"))
        .err()
        .unwrap()
        .to_string();
    // Compile the harness too: the fixture alone has no household output.
    let spec = serde_json::from_value(program()).unwrap();
    let harness = axiom_rules_engine::compile::CompiledProgramArtifact::compile(spec).unwrap();
    let reached = DenseCompiledProgram::from_artifact(&harness, Some("Person"))
        .err()
        .unwrap()
        .to_string();
    assert!(
        error.contains("path source of derived relation `lives_with`") && error.contains("#218"),
        "{error}"
    );
    assert!(
        reached.contains("#218")
            && reached.contains("path source of derived relation `lives_with`"),
        "{reached}"
    );
}
#[test]
fn a_person_living_alone() {
    let ids = ["a"];
    check(&residence(&ids), &ids, &[("a", &["a"])], "household_mask");
}
#[test]
fn married_couple_who_buy_food_separately() {
    let ids = ["a", "b"];
    let mut data = residence(&ids);
    edge(&mut data, "spouse_of", "a", "b");
    check(
        &data,
        &ids,
        &[("a", &["a", "b"]), ("b", &["a", "b"])],
        "household_mask",
    );
}
#[test]
fn roommates_who_buy_food_separately() {
    let ids = ["a", "b"];
    check(
        &residence(&ids),
        &ids,
        &[("a", &["a"]), ("b", &["b"])],
        "household_mask",
    );
}
#[test]
fn applicant_child_under_22_and_other_parent() {
    let ids = ["a", "c", "p"];
    let mut data = residence(&ids);
    set(&mut data, "c", "age", integer(21));
    edge(&mut data, "parent_of", "a", "c");
    edge(&mut data, "parent_of", "p", "c");
    check(&data, &ids, &[("a", &["a", "c", "p"])], "household_mask");
}
#[test]
fn applicant_spouse_and_spouses_meal_group_roommate() {
    let ids = ["a", "b", "x"];
    let mut data = residence(&ids);
    edge(&mut data, "spouse_of", "a", "b");
    set(&mut data, "x", "meal_group", integer(1));
    check(&data, &ids, &[("a", &["a", "b", "x"])], "household_mask");
}
#[test]
fn grandparent_controls_fifteen_year_old_grandchild() {
    let ids = ["g", "c"];
    let mut data = residence(&ids);
    set(&mut data, "c", "age", integer(15));
    edge(&mut data, "under_parental_control_of", "c", "g");
    check(
        &data,
        &ids,
        &[("g", &["g", "c"]), ("c", &["g", "c"])],
        "household_mask",
    );
}
fn parent_chain() -> Value {
    let mut data = residence(&["a", "c", "p", "d"]);
    for child in ["c", "d"] {
        set(&mut data, child, "age", integer(21));
    }
    for (parent, child) in [("a", "c"), ("p", "c"), ("p", "d")] {
        edge(&mut data, "parent_of", parent, child);
    }
    data
}
#[test]
fn applicant_child_other_parent_and_other_parents_child() {
    let ids = ["a", "c", "p", "d"];
    let mut data = parent_chain();
    missing_ids(&data, &["a"]);
    units(&mut data, &ids, 1);
    check(
        &data,
        &ids,
        &[("a", &["a", "c", "p", "d"])],
        "household_mask",
    );
}
#[test]
fn two_separate_three_tie_groups_in_one_residence() {
    let ids = ["a", "b", "c", "d", "e", "f", "g", "h"];
    let mut data = residence(&ids);
    for (a, b) in [
        ("a", "b"),
        ("b", "c"),
        ("c", "d"),
        ("e", "f"),
        ("f", "g"),
        ("g", "h"),
    ] {
        edge(&mut data, "spouse_of", a, b);
    }
    missing_ids(&data, &["a", "e"]);
    units(&mut data, &["a", "b", "c", "d"], 1);
    units(&mut data, &["e", "f", "g", "h"], 2);
    check(
        &data,
        &ids,
        &[("a", &["a", "b", "c", "d"]), ("e", &["e", "f", "g", "h"])],
        "household_mask",
    );
}
#[test]
fn applicants_a_and_b_on_a_three_tie_chain() {
    let ids = ["a", "b", "c", "d"];
    let mut data = residence(&ids);
    for (a, b) in [("a", "b"), ("b", "c"), ("c", "d")] {
        edge(&mut data, "spouse_of", a, b);
    }
    // b's two-tie set is closed: no unit-id record exists for anyone.
    check(
        &data,
        &ids,
        &[("b", &["a", "b", "c", "d"])],
        "household_mask",
    );
    missing_ids(&data, &["a"]);
    units(&mut data, &ids, 1);
    check(
        &data,
        &ids,
        &[("a", &["a", "b", "c", "d"]), ("b", &["a", "b", "c", "d"])],
        "household_mask",
    );
}
#[test]
fn exception_for_older_roommate() {
    let ids = ["a", "r"];
    let mut data = residence(&ids);
    set(&mut data, "r", "age", integer(64));
    set(&mut data, "r", "meal_group", integer(0));
    set(
        &mut data,
        "r",
        "snap_household_exception_may_apply",
        judgment(true),
    );
    missing_ids(&data, &["a"]);
    // Exercise both possible exception outcomes without choosing a legal reading.
    units(&mut data, &["a"], 1);
    units(&mut data, &["r"], 2);
    check(&data, &ids, &[("a", &["a"])], "household_mask");
    units(&mut data, &ids, 1);
    check(&data, &ids, &[("a", &["a", "r"])], "household_mask");
}
#[test]
fn ineligible_student_and_students_spouse() {
    let ids = ["a", "r", "s"];
    let mut data = residence(&ids);
    set(&mut data, "r", "meal_group", integer(0));
    set(
        &mut data,
        "r",
        "snap_ineligible_member_under_b7",
        judgment(true),
    );
    edge(&mut data, "spouse_of", "r", "s");
    check(&data, &ids, &[("a", &["a", "r", "s"])], "household_mask");
    check(&data, &ids, &[("a", &["a", "s"])], "participating_mask");
}
