//! The oracle builds undirected legal ties from raw records, then uses
//! union-find. It never reads the fixture's formula or calls an engine resolver.
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, TestRng, TestRunner};
use serde_json::{Value, json};

use super::snap_household::{input, members, number, program, run as fixture_run, tuple};

const CASES: u32 = 96;
const SEED: u8 = 73;
const MASK_OUTPUTS: &[&str] = &[
    "household_mask",
    "participating_mask",
    "tie_mask",
    "two_ties_mask",
];

/// Pair every mask with a count so an accidental duplicate candidate cannot
/// hide behind carries in the sum of powers of two.
fn run(
    mode: &str,
    p: &Value,
    data: &Value,
    applicants: &[&str],
    outputs: &[&str],
) -> Result<Value, String> {
    let mut projected = outputs
        .iter()
        .map(|name| (*name).to_owned())
        .collect::<Vec<_>>();
    for output in outputs {
        if let Some(prefix) = output.strip_suffix("_mask") {
            projected.push(format!("{prefix}_count"));
        }
    }
    let projected = projected.iter().map(String::as_str).collect::<Vec<_>>();
    let response = fixture_run(mode, p, data, applicants, &projected)?;
    for row in 0..applicants.len() {
        for output in outputs {
            if let Some(prefix) = output.strip_suffix("_mask") {
                assert_eq!(
                    number(&response, row, output).count_ones() as i64,
                    number(&response, row, &format!("{prefix}_count")),
                    "{output}: candidates must form a set"
                );
            }
        }
    }
    Ok(response)
}

#[derive(Clone, Debug)]
struct Person {
    meal_group: u8,
    age: u8,
    exception: bool,
    barred: bool,
}

#[derive(Clone, Debug)]
struct Residence {
    people: Vec<Person>,
    spouses: Vec<(usize, usize)>,
    parents: Vec<(usize, usize)>,
    control: Vec<(usize, usize)>,
    supplied_partition: Vec<u8>,
    shift: usize,
}

/// Broad raw residences share the strategy with sparse chains. Sparse cases
/// reach both sides of the guard, including chains longer than two ties, while
/// raw random cases retain arbitrary overlapping parent/spouse/control facts.
fn residences() -> impl Strategy<Value = Residence> {
    (
        1usize..9,
        prop::collection::vec((0u8..8, 0u8..86, any::<bool>(), any::<bool>()), 8),
        prop::collection::vec((0usize..8, 0usize..8), 0..12),
        prop::collection::vec((0usize..8, 0usize..8), 0..12),
        prop::collection::vec((0usize..8, 0usize..8), 0..12),
        prop::collection::vec(0u8..8, 8),
        any::<u16>(),
        prop_oneof![4 => Just(0u8), 2 => Just(1), 2 => Just(2), 2 => Just(3), 2 => Just(4), 2 => Just(5)],
    )
        .prop_map(
            |(size, people, spouses, parents, control, partition, shift, shape)| {
                let mut residence = Residence {
                    people: people
                        .into_iter()
                        .take(size)
                        .map(|(meal_group, age, exception, barred)| Person {
                            meal_group,
                            age,
                            exception,
                            barred,
                        })
                        .collect(),
                    spouses: spouses
                        .into_iter()
                        .map(|(a, b)| (a % size, b % size))
                        .collect(),
                    parents: parents
                        .into_iter()
                        .map(|(a, b)| (a % size, b % size))
                        .collect(),
                    control: control
                        .into_iter()
                        .map(|(a, b)| (a % size, b % size))
                        .collect(),
                    supplied_partition: partition.into_iter().take(size).collect(),
                    shift: usize::from(shift),
                };
                if shape != 0 {
                    residence.spouses.clear();
                    residence.parents.clear();
                    residence.control.clear();
                    for (index, person) in residence.people.iter_mut().enumerate() {
                        person.meal_group = index as u8;
                        person.age = match shape {
                            2 => 21,
                            3 | 4 => 17,
                            _ => 40,
                        };
                    }
                    for person in 1..size {
                        let edge = if (usize::from(shift) >> person) & 1 == 0 {
                            (person - 1, person)
                        } else {
                            (person, person - 1)
                        };
                        match shape {
                            1 => residence.spouses.push(edge),
                            2 => residence.parents.push(edge),
                            3 => residence.control.push(edge),
                            4 => match person % 3 {
                                0 => residence.spouses.push(edge),
                                1 => residence.parents.push(edge),
                                _ => residence.control.push(edge),
                            },
                            // A meal-only chain would become a clique. Keep this
                            // shape as separate pairs at the statutory edges.
                            5 => {
                                if person % 2 == 1 {
                                    let child = edge.1;
                                    residence.people[child].age =
                                        [17, 18, 21, 22][(person / 2) % 4];
                                    if (usize::from(shift) >> (person + 1)) & 1 == 0 {
                                        residence.parents.push(edge);
                                    } else {
                                        residence.control.push((child, edge.0));
                                    }
                                }
                            }
                            _ => unreachable!(),
                        }
                    }
                }
                residence
            },
        )
}

fn id(index: usize) -> String {
    format!("p{index}")
}

fn dataset(residence: &Residence, units: Option<&[usize]>, exceptions: bool) -> Value {
    let mut inputs = vec![];
    let mut relations = vec![];
    for (index, person) in residence.people.iter().enumerate() {
        let person_id = id(index);
        for (name, value) in [
            (
                "meal_group",
                json!({"kind":"integer","value":person.meal_group}),
            ),
            ("age", json!({"kind":"integer","value":person.age})),
            (
                "member_token",
                json!({"kind":"integer","value":1_i64 << index}),
            ),
            (
                "snap_household_exception_may_apply",
                json!({"kind":"bool","value":exceptions && person.exception}),
            ),
            (
                "snap_ineligible_member_under_b7",
                json!({"kind":"bool","value":person.barred}),
            ),
        ] {
            inputs.push(input(&person_id, name, value));
        }
        if let Some(units) = units {
            inputs.push(input(
                &person_id,
                "snap_household_unit",
                json!({"kind":"integer","value":units[index]}),
            ));
        }
        // Every generated person has a roster entry, including singletons.
        relations.push(tuple("member_of_household", &person_id, "residence"));
    }
    for (name, records) in [
        ("spouse_of", &residence.spouses),
        ("parent_of", &residence.parents),
        ("under_parental_control_of", &residence.control),
    ] {
        for &(a, b) in records {
            relations.push(tuple(name, &id(a), &id(b)));
        }
    }
    json!({"inputs":inputs,"relations":relations})
}

/// These are raw request records, not compiled expressions. In particular an
/// exception flag does not identify a foster child: §8 leaves foster cases to
/// supplied household ids and intentionally retains the symmetric tie rule.
#[derive(Debug)]
struct RawRecords {
    ids: Vec<String>,
    meals: BTreeMap<String, i64>,
    ages: BTreeMap<String, i64>,
    spouses: BTreeSet<(String, String)>,
    parents: BTreeSet<(String, String)>,
    control: BTreeSet<(String, String)>,
}

impl RawRecords {
    fn read(data: &Value) -> Self {
        let mut raw = Self {
            ids: vec![],
            meals: BTreeMap::new(),
            ages: BTreeMap::new(),
            spouses: BTreeSet::new(),
            parents: BTreeSet::new(),
            control: BTreeSet::new(),
        };
        for record in data["inputs"].as_array().unwrap() {
            let map = match record["name"].as_str().unwrap() {
                "meal_group" => &mut raw.meals,
                "age" => &mut raw.ages,
                _ => continue,
            };
            map.insert(
                record["entity_id"].as_str().unwrap().to_owned(),
                record["value"]["value"].as_i64().unwrap(),
            );
        }
        for record in data["relations"].as_array().unwrap() {
            let a = record["tuple"][0].as_str().unwrap().to_owned();
            let b = record["tuple"][1].as_str().unwrap().to_owned();
            match record["name"].as_str().unwrap() {
                "member_of_household" => raw.ids.push(a),
                "spouse_of" => {
                    raw.spouses.insert((a, b));
                }
                "parent_of" => {
                    raw.parents.insert((a, b));
                }
                "under_parental_control_of" => {
                    raw.control.insert((a, b));
                }
                other => panic!("unexpected raw relation {other}"),
            }
        }
        raw.ids.sort();
        raw.ids.dedup();
        raw
    }

    /// Read (a)(3) and each clause of (b)(1) as undirected edges. A parent
    /// connects to their child while the child is under 22. A person under 18
    /// under someone else's parental control connects to that controller when
    /// the controller is not their parent. A spouse record connects both ends.
    fn legal_ties(&self) -> Vec<BTreeSet<String>> {
        let mut edges = BTreeSet::new();
        for a in &self.ids {
            for b in &self.ids {
                if self.meals[a] == self.meals[b] {
                    edges.insert((a.clone(), b.clone()));
                }
            }
        }
        edges.extend(self.spouses.iter().cloned());
        for (parent, child) in &self.parents {
            if self.ages[child] < 22 {
                edges.insert((parent.clone(), child.clone()));
            }
        }
        for (child, controller) in &self.control {
            if self.ages[child] < 18 && !self.parents.contains(&(controller.clone(), child.clone()))
            {
                edges.insert((child.clone(), controller.clone()));
            }
        }
        self.ids
            .iter()
            .map(|person| {
                edges
                    .iter()
                    .filter_map(|(a, b)| {
                        if a == person {
                            Some(b.clone())
                        } else if b == person {
                            Some(a.clone())
                        } else {
                            None
                        }
                    })
                    .collect()
            })
            .collect()
    }

    fn household_partition(&self) -> Vec<BTreeSet<String>> {
        let mut parent = (0..self.ids.len()).collect::<Vec<_>>();
        fn root(parent: &mut [usize], person: usize) -> usize {
            if parent[person] != person {
                let next = parent[person];
                parent[person] = root(parent, next);
            }
            parent[person]
        }
        let ties = self.legal_ties();
        for (a, tied) in ties.iter().enumerate() {
            for (b, person) in self.ids.iter().enumerate() {
                if tied.contains(person) {
                    let left = root(&mut parent, a);
                    let right = root(&mut parent, b);
                    parent[right] = left;
                }
            }
        }
        let roots = (0..self.ids.len())
            .map(|person| root(&mut parent, person))
            .collect::<Vec<_>>();
        roots
            .iter()
            .map(|component| {
                self.ids
                    .iter()
                    .zip(&roots)
                    .filter(|(_, other)| *other == component)
                    .map(|(person, _)| person.clone())
                    .collect()
            })
            .collect()
    }
}

/// A complete dataset with one person's household id removed.
fn without_unit(data: &Value, person: &str) -> Value {
    let mut data = data.clone();
    data["inputs"]
        .as_array_mut()
        .unwrap()
        .retain(|record| record["name"] != "snap_household_unit" || record["entity_id"] != person);
    data
}

/// Breadth-first distances classify the graph actually returned by snap_tie.
/// The independent union-find above determines the expected households.
fn distances(ids: &[String], ties: &[BTreeSet<String>], anchor: usize) -> Vec<Option<usize>> {
    let mut distances = vec![None; ids.len()];
    let mut queue = VecDeque::from([anchor]);
    distances[anchor] = Some(0);
    while let Some(person) = queue.pop_front() {
        for (other, other_id) in ids.iter().enumerate() {
            if distances[other].is_none() && ties[person].contains(other_id) {
                distances[other] = Some(distances[person].unwrap() + 1);
                queue.push_back(other);
            }
        }
    }
    distances
}

fn correct_units(ids: &[String], households: &[BTreeSet<String>]) -> Vec<usize> {
    households
        .iter()
        .map(|household| {
            ids.iter()
                .position(|person| household.contains(person))
                .unwrap()
        })
        .collect()
}

fn partition_members(ids: &[String], units: &[usize], anchor: usize) -> BTreeSet<String> {
    ids.iter()
        .zip(units)
        .filter(|(_, unit)| **unit == units[anchor])
        .map(|(person, _)| person.clone())
        .collect()
}

fn output_values(response: &Value) -> Vec<&Value> {
    response["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| &row["outputs"])
        .collect()
}

fn runner(seed: u8, cases: u32) -> TestRunner {
    TestRunner::new_with_rng(
        Config {
            cases,
            failure_persistence: None,
            ..Config::default()
        },
        TestRng::from_seed(RngAlgorithm::ChaCha, &[seed; 32]),
    )
}

#[derive(Default, Debug)]
struct Coverage {
    exact_no_ids: usize,
    escaping_tie: usize,
    exception: usize,
    id_failure: usize,
    partial_id_failure: usize,
    exception_changes_partition: usize,
    chain_lengths: [usize; 4],
}

impl Coverage {
    fn floors(&self) {
        for (label, count, floor) in [
            ("exact with no ids read", self.exact_no_ids, 15),
            ("tie leaves two-tie set", self.escaping_tie, 12),
            ("exception in residence", self.exception, 64),
            ("evaluation fails naming unit id", self.id_failure, 12),
            (
                "one missing id fails naming it",
                self.partial_id_failure,
                12,
            ),
            (
                "exception changes supplied partition",
                self.exception_changes_partition,
                24,
            ),
            ("chain length 1", self.chain_lengths[0], 4),
            ("chain length 2", self.chain_lengths[1], 4),
            ("chain length 3", self.chain_lengths[2], 4),
            ("chain length >3", self.chain_lengths[3], 4),
        ] {
            eprintln!("SNAP fixed seed {SEED}: {label}={count}, floor={floor}");
            assert!(count >= floor, "{label}: observed {count}, floor {floor}");
        }
    }
}

#[test]
fn guarded_households_never_silently_disagree_with_independent_union_find() {
    let p = program();
    let coverage = RefCell::new(Coverage::default());
    runner(SEED, CASES)
        .run(&residences(), |mut residence| {
            let no_ids = dataset(&residence, None, false);
            let raw = RawRecords::read(&no_ids);
            let expected = raw.household_partition();
            let expected_ties = raw.legal_ties();
            let units = correct_units(&raw.ids, &expected);
            let supplied = dataset(&residence, Some(&units), false);
            let ids = raw.ids.iter().map(String::as_str).collect::<Vec<_>>();
            let mut runs = vec![];
            for mode in ["explain", "fast"] {
                runs.push(
                    run(mode, &p, &supplied, &ids, MASK_OUTPUTS).unwrap_or_else(|error| {
                        panic!("correct supplied ids: {error}; {residence:?}")
                    }),
                );
            }
            prop_assert_eq!(output_values(&runs[0]), output_values(&runs[1]));
            let observed_ties = (0..ids.len())
                .map(|index| members(&runs[0], index, "tie_mask", &ids))
                .collect::<Vec<_>>();
            let observed_households = (0..ids.len())
                .map(|index| members(&runs[0], index, "household_mask", &ids))
                .collect::<Vec<_>>();
            prop_assert_eq!(
                &observed_ties,
                &expected_ties,
                "raw legal tie oracle disagrees with §8 formula: {:?}",
                residence
            );
            prop_assert_eq!(
                &observed_households,
                &expected,
                "raw union-find oracle disagrees with §8 household: {:?}",
                residence
            );

            let mut exact_no_ids = false;
            let mut escaping_tie = false;
            let mut id_failure = false;
            let mut partial_id_failure = false;
            let mut chain_lengths = [false; 4];
            let one_missing = without_unit(&supplied, ids[residence.shift % ids.len()]);
            for (index, person) in ids.iter().enumerate() {
                prop_assert!(observed_households[index].contains(*person));
                for other in 0..ids.len() {
                    prop_assert_eq!(
                        observed_ties[index].contains(ids[other]),
                        observed_ties[other].contains(*person),
                        "ties must stay symmetric"
                    );
                    prop_assert!(
                        observed_households[index] == observed_households[other]
                            || observed_households[index].is_disjoint(&observed_households[other]),
                        "closures must partition a residence"
                    );
                }
                let distance = distances(&raw.ids, &observed_ties, index);
                let maximum = distance.iter().flatten().copied().max().unwrap();
                if maximum > 0 {
                    chain_lengths[maximum.min(4) - 1] = true;
                }
                let observed_two = members(&runs[0], index, "two_ties_mask", &ids);
                let expected_two = raw
                    .ids
                    .iter()
                    .zip(&distance)
                    .filter(|(_, depth)| depth.is_some_and(|depth| depth <= 2))
                    .map(|(person, _)| person.clone())
                    .collect::<BTreeSet<_>>();
                prop_assert_eq!(&observed_two, &expected_two);
                let leaves_set = observed_two.iter().any(|person| {
                    let from = raw.ids.iter().position(|id| id == person).unwrap();
                    !observed_ties[from].is_subset(&observed_two)
                });
                let mut absent_results = vec![];
                for mode in ["explain", "fast"] {
                    absent_results.push(run(mode, &p, &no_ids, &[*person], &["household_mask"]));
                }
                match (&absent_results[0], &absent_results[1]) {
                    (Ok(explain), Ok(fast)) => {
                        prop_assert_eq!(output_values(explain), output_values(fast));
                        prop_assert_eq!(
                            members(explain, 0, "household_mask", &ids),
                            expected[index].clone()
                        );
                        prop_assert!(!leaves_set, "a tie left the set, yet no id was read");
                        exact_no_ids = true;
                    }
                    (Err(explain), Err(fast)) => {
                        prop_assert_eq!(explain, fast);
                        prop_assert!(explain.contains("snap_household_unit"), "{}", explain);
                        prop_assert!(leaves_set, "a closed set read an id: {}", explain);
                        id_failure = true;
                        escaping_tie = true;
                    }
                    (explain, fast) => {
                        prop_assert!(false, "modes differ: {:?} / {:?}", explain, fast)
                    }
                }
                // Every co-resident is a candidate, so one missing id fails
                // exactly the applicants whose guard selects the supplied ids.
                match run("explain", &p, &one_missing, &[*person], &["household_mask"]) {
                    Ok(response) => {
                        prop_assert!(!leaves_set, "a tie left the set past a missing id");
                        prop_assert_eq!(
                            members(&response, 0, "household_mask", &ids),
                            expected[index].clone()
                        );
                    }
                    Err(error) => {
                        prop_assert!(error.contains("snap_household_unit"), "{}", error);
                        prop_assert!(leaves_set, "a closed set read an id: {}", error);
                        partial_id_failure = true;
                    }
                }
            }

            // An exception may split or join legal tie components. The test
            // states its own arbitrary ids and checks that the engine obeys
            // them; that intended difference is not a tie-oracle disagreement.
            let flagged = residence.shift % residence.people.len();
            residence.people[flagged].exception = true;
            let arbitrary_units = residence
                .supplied_partition
                .iter()
                .map(|unit| usize::from(*unit))
                .collect::<Vec<_>>();
            let exceptional = dataset(&residence, Some(&arbitrary_units), true);
            let exception_expected = (0..ids.len())
                .map(|index| partition_members(&raw.ids, &arbitrary_units, index))
                .collect::<Vec<_>>();
            let mut exception_runs = vec![];
            let exception_no_ids = dataset(&residence, None, true);
            for mode in ["explain", "fast"] {
                for person in &ids {
                    let error = run(mode, &p, &exception_no_ids, &[*person], &["household_mask"])
                        .unwrap_err();
                    prop_assert!(error.contains("snap_household_unit"), "{}", error);
                }
                let response = run(
                    mode,
                    &p,
                    &exceptional,
                    &ids,
                    &[
                        "household_mask",
                        "participating_mask",
                        "tie_mask",
                        "snap_exception_in_residence",
                    ],
                ).unwrap();
                for index in 0..ids.len() {
                    prop_assert_eq!(
                        members(&response, index, "household_mask", &ids),
                        exception_expected[index].clone()
                    );
                    prop_assert_eq!(
                        members(&response, index, "tie_mask", &ids),
                        expected_ties[index].clone()
                    );
                    let participating = exception_expected[index].iter().filter(|person| {
                        let candidate = raw.ids.iter().position(|id| id == *person).unwrap();
                        !residence.people[candidate].barred
                    }).cloned().collect::<BTreeSet<_>>();
                    prop_assert_eq!(
                        members(&response, index, "participating_mask", &ids),
                        participating
                    );
                    prop_assert_eq!(
                        response["results"][index]["outputs"]["snap_exception_in_residence"]["outcome"].as_str(),
                        Some("holds")
                    );
                }
                exception_runs.push(response);
            }
            prop_assert_eq!(
                output_values(&exception_runs[0]),
                output_values(&exception_runs[1])
            );
            // Count only outcomes that completed all comparisons successfully.
            let mut counts = coverage.borrow_mut();
            counts.exact_no_ids += usize::from(exact_no_ids);
            counts.escaping_tie += usize::from(escaping_tie);
            counts.id_failure += usize::from(id_failure);
            counts.partial_id_failure += usize::from(partial_id_failure);
            counts.exception += 1;
            counts.exception_changes_partition += usize::from(exception_expected != expected);
            for (count, reached) in counts.chain_lengths.iter_mut().zip(chain_lengths) {
                *count += usize::from(reached);
            }
            Ok(())
        })
        .unwrap();
    coverage.borrow().floors();
}

#[test]
fn participation_filters_composition_and_flipping_b7_never_changes_the_household() {
    let p = program();
    runner(74, 64)
        .run(&residences(), |residence| {
            let raw = RawRecords::read(&dataset(&residence, None, false));
            let households = raw.household_partition();
            let units = correct_units(&raw.ids, &households);
            let ids = raw.ids.iter().map(String::as_str).collect::<Vec<_>>();
            let mut flipped = residence.clone();
            for person in &mut flipped.people {
                person.barred = !person.barred;
            }
            for mode in ["explain", "fast"] {
                for (case, data) in [
                    (&residence, dataset(&residence, Some(&units), false)),
                    (&flipped, dataset(&flipped, Some(&units), false)),
                ] {
                    let response = run(
                        mode,
                        &p,
                        &data,
                        &ids,
                        &["household_mask", "participating_mask"],
                    )
                    .unwrap();
                    for index in 0..ids.len() {
                        let expected = households[index]
                            .iter()
                            .filter(|person| {
                                let candidate =
                                    raw.ids.iter().position(|id| id == *person).unwrap();
                                !case.people[candidate].barred
                            })
                            .cloned()
                            .collect::<BTreeSet<_>>();
                        prop_assert_eq!(
                            members(&response, index, "household_mask", &ids),
                            households[index].clone()
                        );
                        prop_assert_eq!(
                            members(&response, index, "participating_mask", &ids),
                            expected
                        );
                    }
                }
            }
            Ok(())
        })
        .unwrap();
}

#[test]
fn records_and_rosters_preserve_output_and_trace_bytes_across_repeats_modes_and_deferral() {
    let p = program();
    runner(75, 64)
        .run(&residences(), |mut residence| {
            let raw = RawRecords::read(&dataset(&residence, None, false));
            let units = correct_units(&raw.ids, &raw.household_partition());
            let ids = raw.ids.iter().map(String::as_str).collect::<Vec<_>>();
            residence.people[0].exception = true;
            let arbitrary_units = residence
                .supplied_partition
                .iter()
                .map(|unit| usize::from(*unit))
                .collect::<Vec<_>>();
            for data in [
                dataset(&residence, Some(&units), false),
                dataset(&residence, None, false),
                dataset(&residence, Some(&arbitrary_units), true),
            ] {
                let mut reordered = data.clone();
                for key in ["inputs", "relations"] {
                    let records = reordered[key].as_array_mut().unwrap();
                    records.reverse();
                    let length = records.len();
                    if length > 0 {
                        records.rotate_left(residence.shift % length);
                    }
                }
                let mut modes = vec![];
                for mode in ["explain", "fast"] {
                    let first = run(mode, &p, &data, &ids, MASK_OUTPUTS);
                    let repeated = run(mode, &p, &data, &ids, MASK_OUTPUTS);
                    let permuted = run(mode, &p, &reordered, &ids, MASK_OUTPUTS);
                    prop_assert_eq!(&first, &repeated);
                    prop_assert_eq!(&first, &permuted);
                    prop_assert_eq!(
                        serde_json::to_vec(&first).unwrap(),
                        serde_json::to_vec(&repeated).unwrap()
                    );
                    prop_assert_eq!(
                        serde_json::to_vec(&first).unwrap(),
                        serde_json::to_vec(&permuted).unwrap()
                    );
                    modes.push(first);
                }
                match (&modes[0], &modes[1]) {
                    (Ok(explain), Ok(fast)) => {
                        prop_assert_eq!(output_values(explain), output_values(fast))
                    }
                    (Err(explain), Err(fast)) => {
                        prop_assert_eq!(explain, fast);
                        prop_assert!(explain.contains("snap_household_unit"), "{}", explain);
                    }
                    (explain, fast) => {
                        prop_assert!(false, "modes differ: {:?} / {:?}", explain, fast)
                    }
                }
            }
            Ok(())
        })
        .unwrap();
}
