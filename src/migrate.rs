//! Corpus migrations: engine-versioned codemods with machine-checked gates.
//!
//! A migration pairs a detector (spec-level, via the engine's own lowering —
//! never text patterns) with a rewriter and an equivalence gate. This module
//! carries the detector side; see the corpus-migrations design (#152).
//!
//! The pilot detector finds hand-expanded exactly-one patterns: an `or` whose
//! n branches are each an `and` of the same n base judgments, branch i
//! asserting base i and negating the rest — the shape `exactly_one(...)`
//! replaces (#142). Detection walks the serialized `ProgramSpec` JSON, the
//! same `kind:`-tagged form compiled artifacts carry, so structural equality
//! of base judgments is exact serialization equality.

use serde_json::Value;

use crate::rulespec::{RuleSpecError, lower_rulespec_str};

/// One detected hand-expanded exactly-one site.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ExpandedExactlyOne {
    /// Name of the derived rule the expression belongs to.
    pub rule: String,
    /// Where in the rule the expression sits: `expr` or `versions[i].expr`.
    pub site: String,
    /// Number of mutually exclusive base judgments.
    pub arity: usize,
    /// Which hand-written idiom matched: `or_of_ands` (branch i asserts base
    /// i, negates the rest) or `pairwise_exclusions` (a disjunction of the
    /// bases conjoined with NOT terms forbidding every unordered pair —
    /// covers both the flat all-pairs and the factored triangular forms).
    pub idiom: &'static str,
}

/// Scan one RuleSpec module source for hand-expanded exactly-one patterns.
///
/// Lowers through the real loader; a source that does not lower is the
/// caller's to report as unscanned rather than silently pattern-free.
pub fn scan_source(source: &str) -> Result<Vec<ExpandedExactlyOne>, RuleSpecError> {
    let program = lower_rulespec_str(source)?;
    let value = serde_json::to_value(&program)
        .expect("ProgramSpec serialization is infallible for lowered programs");
    let mut found = Vec::new();
    if let Some(derived) = value.get("derived").and_then(Value::as_array) {
        for rule in derived {
            let name = rule
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("<unnamed>")
                .to_string();
            // Versions are the authored surface; the rule-level `expr`
            // mirrors one of them, so scanning both would double-count.
            let versions = rule.get("versions").and_then(Value::as_array);
            match versions {
                Some(versions) if !versions.is_empty() => {
                    for (index, version) in versions.iter().enumerate() {
                        if let Some(expr) = version.get("expr") {
                            collect_sites(
                                expr,
                                &name,
                                &format!("versions[{index}].expr"),
                                &mut found,
                            );
                        }
                    }
                }
                _ => {
                    if let Some(expr) = rule.get("expr") {
                        collect_sites(expr, &name, "expr", &mut found);
                    }
                }
            }
        }
    }
    Ok(found)
}

fn collect_sites(value: &Value, rule: &str, site: &str, found: &mut Vec<ExpandedExactlyOne>) {
    let detected = expanded_exactly_one(value)
        .map(|(arity, bases)| (arity, bases, "or_of_ands"))
        .or_else(|| {
            pairwise_exclusions(value).map(|(arity, bases)| (arity, bases, "pairwise_exclusions"))
        });
    if let Some((arity, bases, idiom)) = detected {
        found.push(ExpandedExactlyOne {
            rule: rule.to_string(),
            site: site.to_string(),
            arity,
            idiom,
        });
        // The branches of a detected expansion are its own machinery; only
        // the base judgments can legitimately contain further candidates.
        for base in bases {
            collect_sites(base, rule, site, found);
        }
        return;
    }
    // Only MAXIMAL and/or chains are candidates: descend to the flattened
    // leaves of a chain, never into intermediate same-kind nodes, so a
    // proper sub-chain of a wider `or` cannot fire as its own gate.
    for kind in ["or", "and"] {
        if let Some(leaves) = flatten_chain(value, kind) {
            for leaf in leaves {
                collect_sites(leaf, rule, site, found);
            }
            return;
        }
    }
    match value {
        Value::Object(map) => {
            for child in map.values() {
                collect_sites(child, rule, site, found);
            }
        }
        Value::Array(items) => {
            for child in items {
                collect_sites(child, rule, site, found);
            }
        }
        _ => {}
    }
}

/// Returns the arity and base judgments when `value` is an or-chain of n
/// and-chains over the same n base judgments in the canonical exactly-one
/// expansion shape. Hand-chained `and`/`or` lower as nested binary pairs, so
/// both chains are flattened associatively before the shape check — the flat
/// n-ary form (which only the retired inline desugar produced) matches too.
fn expanded_exactly_one(value: &Value) -> Option<(usize, Vec<&Value>)> {
    let branches = flatten_chain(value, "or")?;
    let n = branches.len();
    if n < 2 {
        return None;
    }
    let first = flatten_chain(branches[0], "and")?;
    if first.len() != n {
        return None;
    }
    // Base judgment j comes from branch 0: positive at position 0, negated
    // elsewhere. Every base must be extractable or the shape does not match.
    let mut base: Vec<&Value> = Vec::with_capacity(n);
    for (position, item) in first.iter().enumerate() {
        if position == 0 {
            base.push(item);
        } else {
            base.push(not_item(item)?);
        }
    }
    for (branch_index, branch) in branches.iter().enumerate() {
        let items = flatten_chain(branch, "and")?;
        if items.len() != n {
            return None;
        }
        for (position, item) in items.iter().enumerate() {
            let matches = if position == branch_index {
                **item == *base[position]
            } else {
                not_item(item).is_some_and(|inner| *inner == *base[position])
            };
            if !matches {
                return None;
            }
        }
    }
    Some((n, base))
}

/// Returns the arity and bases when `value` is the disjunction+exclusions
/// idiom: an and-chain containing exactly one or-chain over n distinct bases
/// (at least one holds) plus NOT terms whose forbidden pairs cover every
/// unordered pair of bases (at most one holds) — and nothing else. Each NOT
/// body must be `x and y` or `x and (y1 or y2 or ...)` with every operand a
/// base; coverage is checked as a set, so the flat all-pairs form and the
/// factored triangular form both match, in any order.
fn pairwise_exclusions(value: &Value) -> Option<(usize, Vec<&Value>)> {
    let leaves = flatten_chain(value, "and")?;
    let mut bases: Option<Vec<&Value>> = None;
    let mut exclusion_bodies: Vec<&Value> = Vec::new();
    for leaf in &leaves {
        if flatten_chain(leaf, "or").is_some() {
            if bases.is_some() {
                return None;
            }
            bases = Some(flatten_chain(leaf, "or")?);
        } else if let Some(body) = not_item(leaf) {
            exclusion_bodies.push(body);
        } else {
            return None;
        }
    }
    let bases = bases?;
    let n = bases.len();
    if n < 2 || exclusion_bodies.is_empty() {
        return None;
    }
    let keys: Vec<String> = bases.iter().map(|base| base.to_string()).collect();
    let index_of = |value: &Value| -> Option<usize> {
        let key = value.to_string();
        keys.iter().position(|candidate| *candidate == key)
    };
    if keys.iter().collect::<std::collections::HashSet<_>>().len() != n {
        return None;
    }
    let mut covered = std::collections::HashSet::new();
    for body in exclusion_bodies {
        let pair = flatten_chain(body, "and")?;
        if pair.len() != 2 {
            return None;
        }
        // Accept either orientation: `x and (…)` or `(…) and x`.
        let (single, group) = if index_of(pair[0]).is_some() {
            (pair[0], pair[1])
        } else {
            (pair[1], pair[0])
        };
        let left = index_of(single)?;
        match flatten_chain(group, "or") {
            Some(rest) => {
                for member in rest {
                    let right = index_of(member)?;
                    if right == left {
                        return None;
                    }
                    covered.insert((left.min(right), left.max(right)));
                }
            }
            None => {
                let right = index_of(group)?;
                if right == left {
                    return None;
                }
                covered.insert((left.min(right), left.max(right)));
            }
        }
    }
    if covered.len() != n * (n - 1) / 2 {
        return None;
    }
    Some((n, bases))
}

/// Flatten an associative `and`/`or` chain into its leaves, in source order.
/// Nested binary nodes of the same kind dissolve; anything else is a leaf.
fn flatten_chain<'v>(value: &'v Value, kind: &str) -> Option<Vec<&'v Value>> {
    let map = value.as_object()?;
    if map.get("kind")?.as_str()? != kind {
        return None;
    }
    let items = map.get("items")?.as_array()?;
    let mut leaves = Vec::new();
    for item in items {
        match flatten_chain(item, kind) {
            Some(nested) => leaves.extend(nested),
            None => leaves.push(item),
        }
    }
    Some(leaves)
}

fn not_item(value: &Value) -> Option<&Value> {
    let map = value.as_object()?;
    if map.get("kind")?.as_str()? != "not" {
        return None;
    }
    map.get("item")
}

/// A planned rewrite of one detected site into `exactly_one(...)`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RewritePlan {
    pub rule: String,
    pub site: String,
    pub arity: usize,
    pub idiom: &'static str,
    /// Base judgments as bare fact names, in read order.
    pub bases: Vec<String>,
    /// The replacement formula source.
    pub replacement: String,
}

/// A site that cannot be rewritten mechanically and stays for hands.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ManualSite {
    pub rule: String,
    pub site: String,
    pub arity: usize,
    pub idiom: &'static str,
    pub reason: String,
}

/// Plan `exactly_one` rewrites for every detected site whose bases are all
/// bare fact references; anything richer is reported for hands, never
/// guessed at.
pub fn plan_rewrites(source: &str) -> Result<(Vec<RewritePlan>, Vec<ManualSite>), RuleSpecError> {
    let program = lower_rulespec_str(source)?;
    let value = serde_json::to_value(&program)
        .expect("ProgramSpec serialization is infallible for lowered programs");
    let mut plans = Vec::new();
    let mut manual = Vec::new();
    if let Some(derived) = value.get("derived").and_then(Value::as_array) {
        for rule in derived {
            let name = rule
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("<unnamed>")
                .to_string();
            let Some(versions) = rule.get("versions").and_then(Value::as_array) else {
                continue;
            };
            for (index, version) in versions.iter().enumerate() {
                let Some(expr) = version.get("expr") else {
                    continue;
                };
                let site = format!("versions[{index}].expr");
                let detected = expanded_exactly_one(expr)
                    .map(|(arity, bases)| (arity, bases, "or_of_ands"))
                    .or_else(|| {
                        pairwise_exclusions(expr)
                            .map(|(arity, bases)| (arity, bases, "pairwise_exclusions"))
                    });
                let Some((arity, bases, idiom)) = detected else {
                    continue;
                };
                match bases
                    .iter()
                    .map(|base| base_fact_name(base))
                    .collect::<Option<Vec<_>>>()
                {
                    Some(names) => {
                        let replacement = format!("exactly_one({})", names.join(", "));
                        plans.push(RewritePlan {
                            rule: name.clone(),
                            site,
                            arity,
                            idiom,
                            bases: names,
                            replacement,
                        });
                    }
                    None => manual.push(ManualSite {
                        rule: name.clone(),
                        site,
                        arity,
                        idiom,
                        reason: "a base judgment is not a bare fact reference".to_string(),
                    }),
                }
            }
        }
    }
    Ok((plans, manual))
}

/// Bare fact name behind a base judgment: a derived reference, or the
/// input/derived `== true` comparison the lowerer builds for bool facts.
fn base_fact_name(base: &Value) -> Option<String> {
    let map = base.as_object()?;
    match map.get("kind")?.as_str()? {
        "derived" => Some(map.get("name")?.as_str()?.to_string()),
        "comparison" => {
            if map.get("op")?.as_str()? != "eq" {
                return None;
            }
            let right = map.get("right")?.as_object()?;
            if right.get("kind")?.as_str()? != "literal" {
                return None;
            }
            let value = right.get("value")?.as_object()?;
            if value.get("kind")?.as_str()? != "bool" || value.get("value")? != &Value::Bool(true) {
                return None;
            }
            let left = map.get("left")?.as_object()?;
            match left.get("kind")?.as_str()? {
                "input" | "derived" => Some(left.get("name")?.as_str()?.to_string()),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Replace the formula of `rule`'s `versions[index]` in RuleSpec source text,
/// preserving everything else byte-for-byte. Handles block (`|-`) and inline
/// scalars. Returns None if the site cannot be located unambiguously.
pub fn replace_version_formula(
    source: &str,
    rule: &str,
    version_index: usize,
    replacement: &str,
) -> Option<String> {
    let lines: Vec<&str> = source.lines().collect();
    let rule_marker = format!("- name: {rule}");
    let rule_start = lines
        .iter()
        .position(|line| line.trim_start().trim_end() == rule_marker)?;
    let rule_indent = indent_of(lines[rule_start]);
    let rule_end = (rule_start + 1..lines.len())
        .find(|&i| {
            let line = lines[i];
            !line.trim().is_empty()
                && indent_of(line) <= rule_indent
                && line.trim_start().starts_with("- ")
        })
        .unwrap_or(lines.len());

    // The index-th `- effective_from` item inside this rule's versions list.
    let mut seen = 0usize;
    let mut version_start = None;
    for i in rule_start + 1..rule_end {
        if lines[i].trim_start().starts_with("- effective_from") {
            if seen == version_index {
                version_start = Some(i);
                break;
            }
            seen += 1;
        }
    }
    let version_start = version_start?;
    let version_indent = indent_of(lines[version_start]);
    let version_end = (version_start + 1..rule_end)
        .find(|&i| {
            let line = lines[i];
            !line.trim().is_empty() && indent_of(line) <= version_indent
        })
        .unwrap_or(rule_end);

    let formula_line =
        (version_start..version_end).find(|&i| lines[i].trim_start().starts_with("formula:"))?;
    let key_indent = indent_of(lines[formula_line]);
    let is_block = lines[formula_line].trim_end().ends_with("|-")
        || lines[formula_line].trim_end().ends_with('|');
    let block_end = if is_block {
        (formula_line + 1..version_end)
            .find(|&i| {
                let line = lines[i];
                !line.trim().is_empty() && indent_of(line) <= key_indent
            })
            .unwrap_or(version_end)
    } else {
        formula_line + 1
    };

    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    out.extend(lines[..formula_line].iter().map(|s| s.to_string()));
    out.push(format!("{}formula: {replacement}", " ".repeat(key_indent)));
    out.extend(lines[block_end..].iter().map(|s| s.to_string()));
    let mut joined = out.join("\n");
    if source.ends_with('\n') {
        joined.push('\n');
    }
    Some(joined)
}

fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// Extract the formula source of `rule`'s `versions[index]` verbatim.
pub fn extract_version_formula(source: &str, rule: &str, version_index: usize) -> Option<String> {
    let lines: Vec<&str> = source.lines().collect();
    let rule_marker = format!("- name: {rule}");
    let rule_start = lines
        .iter()
        .position(|line| line.trim_start().trim_end() == rule_marker)?;
    let rule_indent = indent_of(lines[rule_start]);
    let rule_end = (rule_start + 1..lines.len())
        .find(|&i| {
            let line = lines[i];
            !line.trim().is_empty()
                && indent_of(line) <= rule_indent
                && line.trim_start().starts_with("- ")
        })
        .unwrap_or(lines.len());
    let mut seen = 0usize;
    let mut version_start = None;
    for i in rule_start + 1..rule_end {
        if lines[i].trim_start().starts_with("- effective_from") {
            if seen == version_index {
                version_start = Some(i);
                break;
            }
            seen += 1;
        }
    }
    let version_start = version_start?;
    let version_indent = indent_of(lines[version_start]);
    let version_end = (version_start + 1..rule_end)
        .find(|&i| {
            let line = lines[i];
            !line.trim().is_empty() && indent_of(line) <= version_indent
        })
        .unwrap_or(rule_end);
    let formula_line =
        (version_start..version_end).find(|&i| lines[i].trim_start().starts_with("formula:"))?;
    let key_indent = indent_of(lines[formula_line]);
    let trimmed = lines[formula_line].trim_start();
    if trimmed.trim_end().ends_with("|-") || trimmed.trim_end().ends_with('|') {
        let block_end = (formula_line + 1..version_end)
            .find(|&i| {
                let line = lines[i];
                !line.trim().is_empty() && indent_of(line) <= key_indent
            })
            .unwrap_or(version_end);
        let body: Vec<&str> = lines[formula_line + 1..block_end].iter().copied().collect();
        let strip = body
            .iter()
            .filter(|line| !line.trim().is_empty())
            .map(|line| indent_of(line))
            .min()
            .unwrap_or(0);
        Some(
            body.iter()
                .map(|line| {
                    if line.len() >= strip {
                        &line[strip..]
                    } else {
                        line.trim_start()
                    }
                })
                .collect::<Vec<_>>()
                .join("\n"),
        )
    } else {
        Some(
            trimmed
                .trim_start_matches("formula:")
                .trim()
                .trim_matches('"')
                .to_string(),
        )
    }
}

/// Behavioral equivalence report for one rewrite.
#[derive(Debug, Clone, serde::Serialize)]
pub struct GateReport {
    pub assignments: usize,
    pub outcomes_match: bool,
    pub rescan_clean: bool,
}

/// Prove old and new formulas agree by executing BOTH through the real
/// engine over every Boolean assignment of the bases, then rescanning the
/// rewritten form to confirm the pattern is gone. Read order (and so
/// missing-input fault order) is preserved by construction: the replacement
/// lists bases in detection order, which is leaf read order.
pub fn gate_rewrite(
    old_formula: &str,
    replacement: &str,
    bases: &[String],
) -> Result<GateReport, String> {
    use crate::api::{
        ExecutionMode, ExecutionQuery, ExecutionRequest, OutputValue, execute_request,
    };
    use crate::spec::{
        DatasetSpec, InputRecordSpec, IntervalSpec, PeriodKindSpec, PeriodSpec, ScalarValueSpec,
    };
    if bases.len() > 12 {
        return Err(format!(
            "gate refuses arity {} (>4096 assignments)",
            bases.len()
        ));
    }
    let period = PeriodSpec {
        kind: PeriodKindSpec::Month,
        start: chrono::NaiveDate::from_ymd_opt(2026, 1, 1).expect("valid date"),
        end: chrono::NaiveDate::from_ymd_opt(2026, 1, 31).expect("valid date"),
    };
    let run = |formula: &str, mask: usize| -> Result<String, String> {
        let program = lower_rulespec_str(&probe_module(formula))
            .map_err(|error| format!("probe lowering failed: {error}"))?;
        let dataset = DatasetSpec {
            inputs: bases
                .iter()
                .enumerate()
                .map(|(bit, name)| InputRecordSpec {
                    name: name.clone(),
                    entity: "Household".to_string(),
                    entity_id: "probe-1".to_string(),
                    interval: IntervalSpec {
                        start: period.start,
                        end: period.end,
                    },
                    value: ScalarValueSpec::Bool {
                        value: mask & (1 << bit) != 0,
                    },
                })
                .collect(),
            relations: vec![],
        };
        let response = execute_request(ExecutionRequest {
            relation_binding: Default::default(),
            mode: ExecutionMode::Explain,
            program,
            dataset,
            queries: vec![ExecutionQuery {
                assessment_date: None,
                entity_id: "probe-1".to_string(),
                period: period.clone(),
                outputs: vec!["migration_probe".to_string()],
            }],
        })
        .map_err(|error| format!("probe execution failed: {error}"))?;
        match response.results[0].outputs.get("migration_probe") {
            Some(OutputValue::Judgment { outcome, .. }) => Ok(format!("{outcome:?}")),
            other => Err(format!("probe produced no judgment: {other:?}")),
        }
    };
    let assignments = 1usize << bases.len();
    for mask in 0..assignments {
        // Outcome-or-error must agree on both sides: a replacement that
        // stops referencing a base makes the engine reject that input, and
        // that asymmetry is a gate failure, not a tool crash.
        let old = run(old_formula, mask);
        let new = run(replacement, mask);
        let agree = match (&old, &new) {
            (Ok(old), Ok(new)) => old == new,
            (Err(old), Err(new)) => old == new,
            _ => false,
        };
        if !agree {
            return Ok(GateReport {
                assignments,
                outcomes_match: false,
                rescan_clean: false,
            });
        }
        if let (Err(error), Err(_)) = (&old, &new) {
            return Err(format!("both probes fail identically: {error}"));
        }
    }
    let rescan_clean = scan_source(&probe_module(replacement))
        .map(|hits| hits.is_empty())
        .unwrap_or(false);
    Ok(GateReport {
        assignments,
        outcomes_match: true,
        rescan_clean,
    })
}

fn probe_module(formula: &str) -> String {
    let body = formula
        .lines()
        .map(|line| format!("          {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "format: rulespec/v1\nrules:\n  - name: migration_probe\n    kind: derived\n    \
         entity: Household\n    dtype: Judgment\n    versions:\n      - effective_from: \
         2026-01-01\n        formula: |-\n{body}\n"
    )
}

/// One relation whose declared slot kinds the artifact migration set.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RelationTypingChange {
    pub relation: String,
    /// Declared kinds before migration; empty for an untyped relation.
    pub previous: Vec<String>,
    pub slot_entities: Vec<String>,
    /// `inferred` from executable usage, or `override` from the caller.
    pub source: &'static str,
}

/// An artifact typed by [`migrate_artifact_relation_typing`].
#[derive(Debug, Clone)]
pub struct ArtifactRelationMigration {
    pub artifact: crate::compile::CompiledProgramArtifact,
    pub changes: Vec<RelationTypingChange>,
}

#[derive(Debug, thiserror::Error)]
pub enum ArtifactRelationMigrationError {
    #[error(transparent)]
    Compile(#[from] crate::compile::CompileError),
    #[error(transparent)]
    Spec(#[from] crate::spec::SpecError),
    #[error(
        "--relation-entities names relation `{relation}`, which the artifact does not declare as a data relation{hint}"
    )]
    UnknownRelation { relation: String, hint: String },
    #[error(
        "--relation-entities gives relation `{relation}` {found} kinds, but its arity is {arity}"
    )]
    OverrideArity {
        relation: String,
        arity: usize,
        found: usize,
    },
    #[error(
        "--relation-entities gives relation `{relation}` slot {slot} kind `{given}`, but the artifact's executable nodes read `{executed}` ids there; the migration types an artifact as it executes and never reorders its slots, so recompile from source to change the orientation"
    )]
    OverrideContradictsExecution {
        relation: String,
        slot: usize,
        given: String,
        executed: String,
    },
    #[error(
        "relation `{relation}` declares slot kinds {declared:?}, but the artifact's executable nodes read {executed}; pass `--relation-entities {relation}=<Kind>,...` in executed order to type the artifact as it runs, or recompile from source so the slots follow the declaration"
    )]
    DeclarationContradictsExecution {
        relation: String,
        declared: Vec<String>,
        executed: String,
    },
    #[error(
        "cannot infer every slot kind from executable usage; pass `--relation-entities <relation>=<Kind>,<Kind>` (kinds in tuple order) for:\n{0}"
    )]
    Uninferable(String),
    #[error(
        "the migrated artifact still fails relation entity typing; recompile it from source:\n{0}"
    )]
    StillIllTyped(crate::relation_typing::RelationTypingReport),
}

/// Type the relations of a compiled artifact the loader rejects because it
/// executes untyped relations.
///
/// Each untyped data relation an executable node reads is stamped with the
/// slot kinds execution establishes (see [`execution_evidence`]); nothing
/// else, declarations included, vouches for a kind. `overrides` (relation
/// name, or its unique short name, to kinds in tuple order) supply slots that
/// leaves open and retype relations whose declaration contradicts how the
/// artifact executes. The migration never moves an aggregate's slots, so
/// datasets that bound correctly before still bind; datasets in the other
/// orientation now get a binding diagnostic (an error under the default
/// strict binding) instead of aggregating nothing silently. The result must
/// pass the same typing check the loader enforces.
pub fn migrate_artifact_relation_typing(
    source: &str,
    path: &str,
    overrides: &std::collections::BTreeMap<String, Vec<String>>,
) -> Result<ArtifactRelationMigration, ArtifactRelationMigrationError> {
    let mut artifact =
        crate::compile::CompiledProgramArtifact::from_json_str_for_relation_migration(
            source, path,
        )?;
    let mut resolved_overrides = std::collections::BTreeMap::<String, Vec<String>>::new();
    for (name, kinds) in overrides {
        let data_relations = artifact
            .program
            .relations
            .iter()
            .filter(|relation| relation.derivation.is_none())
            .map(|relation| relation.name.as_str())
            .collect::<Vec<_>>();
        let resolved = if data_relations.contains(&name.as_str()) {
            name.clone()
        } else {
            let suffix = format!("#relation.{name}");
            let matches = data_relations
                .iter()
                .filter(|relation| relation.ends_with(&suffix))
                .collect::<Vec<_>>();
            match matches.as_slice() {
                [only] => (**only).to_string(),
                [] => {
                    return Err(ArtifactRelationMigrationError::UnknownRelation {
                        relation: name.clone(),
                        hint: String::new(),
                    });
                }
                many => {
                    return Err(ArtifactRelationMigrationError::UnknownRelation {
                        relation: name.clone(),
                        hint: format!(
                            " uniquely; it matches {}",
                            many.iter()
                                .map(|relation| format!("`{relation}`"))
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                    });
                }
            }
        };
        resolved_overrides.insert(resolved, kinds.clone());
    }

    let format_executed = |slots: &[Option<String>]| {
        format!(
            "[{}]",
            slots
                .iter()
                .map(|slot| slot.as_deref().unwrap_or("?"))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let is_data = |relation: &crate::spec::RelationSpec| relation.derivation.is_none();
    for relation in artifact.program.relations.iter().filter(|r| is_data(r)) {
        if let Some(kinds) = resolved_overrides.get(&relation.name)
            && kinds.len() != relation.arity
        {
            return Err(ArtifactRelationMigrationError::OverrideArity {
                relation: relation.name.clone(),
                arity: relation.arity,
                found: kinds.len(),
            });
        }
    }
    let original = artifact
        .program
        .relations
        .iter()
        .filter(|r| is_data(r))
        .map(|relation| (relation.name.clone(), relation.slot_entities.clone()))
        .collect::<std::collections::BTreeMap<_, _>>();

    // Apply overrides first, so the filtered-entity mapping and usage see the
    // source kinds the caller asserts. Then stamp every untyped relation whose
    // usage fixes all its slots, and repeat: a newly typed source can map a
    // filtered entity that settles another relation's usage.
    for relation in &mut artifact.program.relations {
        if let Some(kinds) = resolved_overrides.get(&relation.name)
            && is_data(relation)
        {
            relation.slot_entities = kinds.clone();
        }
    }
    let executed = loop {
        let model = artifact.program.to_program()?;
        let executed = execution_evidence(&model);
        let mut stamped = false;
        for relation in &mut artifact.program.relations {
            if !is_data(relation) || !relation.slot_entities.is_empty() || relation.arity == 0 {
                continue;
            }
            if let Some(usage) = executed.get(&relation.name)
                && usage.len() == relation.arity
                && usage.iter().all(Option::is_some)
            {
                relation.slot_entities = usage.iter().flatten().cloned().collect();
                stamped = true;
            }
        }
        if !stamped {
            break executed;
        }
    };
    // An override is checked against what execution establishes without it,
    // since its own kinds can map a filtered entity that then reads as
    // agreement.
    let unhinted = {
        let mut program = artifact.program.clone();
        for relation in &mut program.relations {
            if resolved_overrides.contains_key(&relation.name) {
                relation.slot_entities.clear();
            }
        }
        execution_evidence(&program.to_program()?)
    };

    // Only relations the typing check requires (those an executed node
    // reads) must be typed; one read only by an unused derived relation may
    // stay untyped.
    let required = match crate::relation_typing::check_program(&artifact.program.to_program()?) {
        Ok(()) => std::collections::BTreeSet::new(),
        Err(report) => report
            .untyped_relations()
            .into_iter()
            .map(str::to_string)
            .collect(),
    };
    let contested = execution_evidence_contested(&artifact.program.to_program()?, true);
    let mut changes = Vec::new();
    let mut uninferable = Vec::new();
    for relation in artifact.program.relations.iter().filter(|r| is_data(r)) {
        let usage = executed.get(&relation.name);
        let previous = original.get(&relation.name).cloned().unwrap_or_default();
        if let Some(given) = resolved_overrides.get(&relation.name) {
            if let Some(usage) = unhinted.get(&relation.name) {
                for (slot, (given, executed)) in given.iter().zip(usage).enumerate() {
                    if let Some(executed) = executed
                        && executed != given
                    {
                        return Err(
                            ArtifactRelationMigrationError::OverrideContradictsExecution {
                                relation: relation.name.clone(),
                                slot,
                                given: given.clone(),
                                executed: executed.clone(),
                            },
                        );
                    }
                }
            }
            if &previous != given {
                changes.push(RelationTypingChange {
                    relation: relation.name.clone(),
                    previous,
                    slot_entities: given.clone(),
                    source: "override",
                });
            }
            continue;
        }
        let unknown = vec![None; relation.arity];
        let usage = usage.unwrap_or(&unknown);
        if !previous.is_empty() {
            let contradicts = previous.iter().zip(usage).any(|(declared, executed)| {
                executed
                    .as_ref()
                    .is_some_and(|executed| executed != declared)
            });
            if contradicts {
                return Err(
                    ArtifactRelationMigrationError::DeclarationContradictsExecution {
                        relation: relation.name.clone(),
                        declared: previous,
                        executed: format_executed(usage),
                    },
                );
            }
            continue;
        }
        if relation.slot_entities.is_empty() {
            if !required.contains(&relation.name) {
                continue;
            }
            uninferable.push(format!(
                "  {} (arity {}; executable usage determines {})",
                relation.name,
                relation.arity,
                format_executed(usage)
            ));
        } else if contested.get(&relation.name).is_some_and(|slots| {
            slots.len() != relation.slot_entities.len() || slots.iter().any(Option::is_none)
        }) {
            uninferable.push(format!(
                "  {} (arity {}; a filtered entity whose derivations are inconsistent also reads it; executable usage determines {})",
                relation.name,
                relation.arity,
                format_executed(&contested[&relation.name])
            ));
        } else {
            changes.push(RelationTypingChange {
                relation: relation.name.clone(),
                previous,
                slot_entities: relation.slot_entities.clone(),
                source: "inferred",
            });
        }
    }
    if !uninferable.is_empty() {
        let mut detail = uninferable.join("\n");
        let hints = untrusted_filter_hints(&artifact.program.to_program()?);
        if !hints.is_empty() {
            detail.push_str(
                "\nthese filtered entities have no trusted kind, so the rules evaluated for them say nothing:\n",
            );
            detail.push_str(&hints.join("\n"));
        }
        return Err(ArtifactRelationMigrationError::Uninferable(detail));
    }
    crate::relation_typing::check_program(&artifact.program.to_program()?)
        .map_err(ArtifactRelationMigrationError::StillIllTyped)?;
    Ok(ArtifactRelationMigration { artifact, changes })
}

/// What execution establishes about each data relation's slot kinds.
///
/// Evidence comes only from aggregations the runtimes evaluate (a versioned
/// rule's versions, otherwise its base semantics, including aggregates in a
/// `match` fallback's labels and over-periods reductions, which dense and
/// lifetime execution read):
/// - the slot an aggregate keys on holds ids of the evaluating rule's entity;
/// - the other slot holds ids of the entity of the rules its `where` clause
///   (outside nested aggregations) and summed value read, when they agree;
/// - a nested aggregation in a `where` clause runs on those related ids.
///
/// An aggregate over a derived relation reads its data source through the
/// derivation's slots, so it is evidence about that source only when every
/// link of the chain keeps those slots. A rule whose entity is a filtered
/// entity (a derived relation's `entity`) reads ids of that entity's physical
/// kind only when every derivation defining the name is consistent with its
/// source chain and they agree on one physical kind; otherwise it says
/// nothing. Declarations never vouch for a kind, and derived-relation
/// predicates and membership tests give no evidence. A slot is known when
/// every use that constrains it agrees.
fn execution_evidence(
    model: &crate::model::Program,
) -> std::collections::BTreeMap<String, Vec<Option<String>>> {
    execution_evidence_contested(model, false)
}

/// [`execution_evidence`], where with `contest` a use by a filtered entity the
/// typing check maps but migration does not trust leaves its slot unsettled
/// instead of adding nothing. Run on the final model only: during stamping, a
/// filter can become trusted once its source is typed.
fn execution_evidence_contested(
    model: &crate::model::Program,
    contest: bool,
) -> std::collections::BTreeMap<String, Vec<Option<String>>> {
    let trusted = trusted_filter_kinds(model);
    let contest = if contest {
        crate::relation_typing::filtered_entity_kinds(model)
            .into_keys()
            .filter(|entity| !trusted.contains_key(entity))
            .collect()
    } else {
        std::collections::BTreeSet::new()
    };
    let collector = EvidenceCollector {
        model,
        filters: crate::relation_typing::filtered_entities(model),
        trusted,
        contest,
    };
    let mut slots =
        std::collections::BTreeMap::<String, Vec<std::collections::BTreeSet<String>>>::new();
    let mut names = model.derived.keys().collect::<Vec<_>>();
    names.sort();
    for name in names {
        let rule = &model.derived[name];
        let semantics = if rule.versions.is_empty() {
            vec![&rule.semantics]
        } else {
            rule.versions
                .iter()
                .map(|version| &version.semantics)
                .collect()
        };
        let entity = collector.physical(&rule.entity);
        for semantics in semantics {
            match semantics {
                crate::model::DerivedSemantics::Scalar(expr) => {
                    collector.scalar(expr, entity.as_deref(), &mut slots)
                }
                crate::model::DerivedSemantics::Judgment(expr) => {
                    collector.judgment(expr, entity.as_deref(), &mut slots)
                }
            }
        }
    }
    slots
        .into_iter()
        .map(|(relation, slots)| {
            let kinds = slots
                .into_iter()
                .map(|kinds| {
                    (kinds.len() == 1 && !kinds.contains(CONTESTED))
                        .then(|| kinds.into_iter().next())
                        .flatten()
                })
                .collect();
            (relation, kinds)
        })
        .collect()
}

/// For each filtered entity a rule is evaluated for but migration does not
/// trust, what would settle it: typing the data relation at the bottom of a
/// consistent defining chain (`--relation-entities` takes data relations
/// only), or recompiling when a chain is inconsistent.
fn untrusted_filter_hints(model: &crate::model::Program) -> Vec<String> {
    let trusted = trusted_filter_kinds(model);
    let consistent = consistent_derivations(model);
    let data_source = |relation: &str| {
        let mut name = relation.to_string();
        let mut visited = std::collections::BTreeSet::new();
        while let Some(derivation) = model
            .relations
            .get(&name)
            .and_then(|schema| schema.derivation.as_ref())
        {
            if !visited.insert(name.clone()) {
                return None;
            }
            name = derivation.source_relation.clone();
        }
        Some(name)
    };
    let evaluated = model
        .derived
        .values()
        .map(|rule| rule.entity.clone())
        .collect::<std::collections::BTreeSet<_>>();
    let mut definitions = std::collections::BTreeMap::<String, Vec<String>>::new();
    for (name, schema) in &model.relations {
        let Some(derivation) = &schema.derivation else {
            continue;
        };
        let Some(entity) = &derivation.entity else {
            continue;
        };
        if trusted.contains_key(entity) || !evaluated.contains(entity) {
            continue;
        }
        let remedy = match data_source(name) {
            Some(source) if consistent.contains(name) => {
                format!("type its data relation `{source}`")
            }
            _ => "its source chain is inconsistent; recompile from source".to_string(),
        };
        definitions
            .entry(entity.clone())
            .or_default()
            .push(format!("`{name}` ({remedy})"));
    }
    definitions
        .into_iter()
        .map(|(entity, mut definitions)| {
            definitions.sort();
            format!("  {entity}: defined by {}", definitions.join(", "))
        })
        .collect()
}

/// Filtered entity -> the physical kind of the ids it is queried with, for
/// names every defining derivation agrees on through consistent source
/// chains. A filter of a filter resolves to the innermost kind.
fn trusted_filter_kinds(
    model: &crate::model::Program,
) -> std::collections::BTreeMap<String, String> {
    let consistent = consistent_derivations(model);
    let mut direct = std::collections::BTreeMap::<String, Option<String>>::new();
    for (name, schema) in &model.relations {
        let Some(derivation) = &schema.derivation else {
            continue;
        };
        let Some(entity) = &derivation.entity else {
            continue;
        };
        let kind = consistent
            .contains(name)
            .then(|| {
                crate::relation_typing::effective_slot_entities(model, name)
                    .and_then(|kinds| kinds.get(derivation.current_slot).cloned())
            })
            .flatten();
        direct
            .entry(entity.clone())
            .and_modify(|known| {
                if *known != kind {
                    *known = None;
                }
            })
            .or_insert(kind);
    }
    direct
        .keys()
        .filter_map(|entity| {
            let mut kind = entity.clone();
            let mut visited = std::collections::BTreeSet::new();
            loop {
                let Some(entry) = direct.get(&kind) else {
                    return Some((entity.clone(), kind));
                };
                let next = entry.clone()?;
                if next == kind {
                    return Some((entity.clone(), kind));
                }
                if !visited.insert(kind.clone()) {
                    return None;
                }
                kind = next;
            }
        })
        .collect()
}

/// Derived relations whose declared kinds agree with their source's, and
/// whose slots match a derived source's, all the way down their source chain:
/// a filter that declares nothing still inherits whatever an ancestor
/// declares, so one contradicting ancestor poisons every derived relation
/// above it. A cyclic chain is never consistent.
fn consistent_derivations(model: &crate::model::Program) -> std::collections::BTreeSet<String> {
    let locally_consistent = |name: &str| {
        let Some(schema) = model.relations.get(name) else {
            return false;
        };
        let Some(derivation) = schema.derivation.as_ref() else {
            return true;
        };
        if let Some(source) = model
            .relations
            .get(&derivation.source_relation)
            .and_then(|source| source.derivation.as_ref())
            && (source.current_slot, source.related_slot)
                != (derivation.current_slot, derivation.related_slot)
        {
            return false;
        }
        let source =
            crate::relation_typing::effective_slot_entities(model, &derivation.source_relation);
        let agrees = |declared: &Vec<String>| {
            declared.is_empty() || source.as_ref().is_some_and(|source| source == declared)
        };
        agrees(&derivation.slot_entities) && agrees(&schema.slot_entities)
    };
    model
        .relations
        .iter()
        .filter(|(_, schema)| schema.derivation.is_some())
        .filter(|(name, _)| {
            let mut current = name.to_string();
            let mut visited = std::collections::BTreeSet::new();
            loop {
                if !visited.insert(current.clone()) || !locally_consistent(&current) {
                    return false;
                }
                match model
                    .relations
                    .get(&current)
                    .and_then(|schema| schema.derivation.as_ref())
                {
                    Some(derivation) => current = derivation.source_relation.clone(),
                    None => return true,
                }
            }
        })
        .map(|(name, _)| name.clone())
        .collect()
}

/// The kind recorded for a use by a filtered entity that the typing check
/// maps (from the derivation's apparent kind) but migration does not trust:
/// it contests the slot, so the slot is never settled from the other uses.
const CONTESTED: &str = "\u{1}contested";

struct EvidenceCollector<'a> {
    model: &'a crate::model::Program,
    filters: std::collections::BTreeSet<String>,
    trusted: std::collections::BTreeMap<String, String>,
    contest: std::collections::BTreeSet<String>,
}

type Evidence = std::collections::BTreeMap<String, Vec<std::collections::BTreeSet<String>>>;

impl EvidenceCollector<'_> {
    /// The physical kind of the ids a rule of `entity` evaluates, if known.
    fn physical(&self, entity: &str) -> Option<String> {
        if entity == crate::model::SCALAR_ENTITY {
            return None;
        }
        if self.filters.contains(entity) {
            if self.contest.contains(entity) {
                return Some(CONTESTED.to_string());
            }
            return self.trusted.get(entity).cloned();
        }
        Some(entity.to_string())
    }

    /// The data relation an aggregate over `relation` reads, and the slots
    /// it reads it with, when that is unambiguous.
    fn data_source(
        &self,
        relation: &str,
        current_slot: usize,
        related_slot: usize,
    ) -> Option<(String, usize, usize)> {
        let mut name = relation.to_string();
        let mut slots = (current_slot, related_slot);
        let mut visited = std::collections::BTreeSet::new();
        loop {
            if !visited.insert(name.clone()) {
                return None;
            }
            let schema = self.model.relations.get(&name)?;
            let Some(derivation) = &schema.derivation else {
                return Some((name, slots.0, slots.1));
            };
            let derivation_slots = (derivation.current_slot, derivation.related_slot);
            if name != relation && derivation_slots != slots {
                return None;
            }
            slots = derivation_slots;
            name = derivation.source_relation.clone();
        }
    }

    /// The kind of the related ids: the agreed physical entity of every rule
    /// the `where` clause (outside nested aggregations) and value read.
    fn related_kind(
        &self,
        value: Option<&crate::model::RelatedValueRef>,
        where_clause: Option<&crate::model::JudgmentExpr>,
    ) -> Option<String> {
        let mut rules = std::collections::BTreeSet::new();
        if let Some(crate::model::RelatedValueRef::Derived(name)) = value {
            rules.insert(name.clone());
        }
        if let Some(where_clause) = where_clause {
            rules.extend(crate::relation_typing::judgment_rule_references(
                where_clause,
            ));
        }
        let mut kinds = std::collections::BTreeSet::new();
        for name in rules {
            let rule = self.model.derived.get(&name)?;
            if rule.entity == crate::model::SCALAR_ENTITY {
                continue;
            }
            kinds.insert(self.physical(&rule.entity)?);
        }
        if kinds.contains(CONTESTED) {
            return Some(CONTESTED.to_string());
        }
        (kinds.len() == 1)
            .then(|| kinds.into_iter().next())
            .flatten()
    }

    #[allow(clippy::too_many_arguments)]
    fn aggregate(
        &self,
        relation: &str,
        current_slot: usize,
        related_slot: usize,
        value: Option<&crate::model::RelatedValueRef>,
        where_clause: Option<&crate::model::JudgmentExpr>,
        entity: Option<&str>,
        slots: &mut Evidence,
    ) {
        let related = self.related_kind(value, where_clause);
        if let Some((source, current, related_index)) =
            self.data_source(relation, current_slot, related_slot)
            && let Some(schema) = self.model.relations.get(&source)
            && current < schema.arity
            && related_index < schema.arity
        {
            let entry = slots.entry(source).or_default();
            if entry.len() < schema.arity {
                entry.resize_with(schema.arity, Default::default);
            }
            if let Some(entity) = entity {
                entry[current].insert(entity.to_string());
            }
            if let Some(related) = &related {
                entry[related_index].insert(related.clone());
            }
        }
        if let Some(where_clause) = where_clause {
            self.judgment(where_clause, related.as_deref(), slots);
        }
    }

    fn scalar(&self, expr: &crate::model::ScalarExpr, entity: Option<&str>, slots: &mut Evidence) {
        use crate::model::ScalarExpr;
        match expr {
            ScalarExpr::CountRelated {
                relation,
                current_slot,
                related_slot,
                where_clause,
            } => self.aggregate(
                relation,
                *current_slot,
                *related_slot,
                None,
                where_clause.as_deref(),
                entity,
                slots,
            ),
            ScalarExpr::SumRelated {
                relation,
                current_slot,
                related_slot,
                value,
                where_clause,
            } => self.aggregate(
                relation,
                *current_slot,
                *related_slot,
                Some(value),
                where_clause.as_deref(),
                entity,
                slots,
            ),
            ScalarExpr::ParameterLookup { index, .. }
            | ScalarExpr::Ceil(index)
            | ScalarExpr::Floor(index) => self.scalar(index, entity, slots),
            ScalarExpr::Add(items) | ScalarExpr::Max(items) | ScalarExpr::Min(items) => {
                for item in items {
                    self.scalar(item, entity, slots);
                }
            }
            ScalarExpr::Sub(left, right)
            | ScalarExpr::Mul(left, right)
            | ScalarExpr::Div(left, right)
            | ScalarExpr::DateAddDays {
                date: left,
                days: right,
            }
            | ScalarExpr::DateAddMonths {
                date: left,
                months: right,
            }
            | ScalarExpr::DateAddYears {
                date: left,
                years: right,
            }
            | ScalarExpr::DaysBetween {
                from: left,
                to: right,
            } => {
                self.scalar(left, entity, slots);
                self.scalar(right, entity, slots);
            }
            ScalarExpr::If {
                condition,
                then_expr,
                else_expr,
            } => {
                self.judgment(condition, entity, slots);
                self.scalar(then_expr, entity, slots);
                self.scalar(else_expr, entity, slots);
            }
            ScalarExpr::NoMatch { subject, patterns } => {
                self.scalar(subject, entity, slots);
                for pattern in patterns {
                    self.scalar(pattern, entity, slots);
                }
            }
            ScalarExpr::OverPeriods { value, n, .. } => {
                self.scalar(value, entity, slots);
                if let Some(n) = n {
                    self.scalar(n, entity, slots);
                }
            }
            ScalarExpr::Literal(_)
            | ScalarExpr::Input(_)
            | ScalarExpr::InputOrElse { .. }
            | ScalarExpr::Derived(_)
            | ScalarExpr::PeriodStart
            | ScalarExpr::PeriodEnd => {}
        }
    }

    fn judgment(
        &self,
        expr: &crate::model::JudgmentExpr,
        entity: Option<&str>,
        slots: &mut Evidence,
    ) {
        use crate::model::JudgmentExpr;
        match expr {
            JudgmentExpr::Comparison { left, right, .. } => {
                self.scalar(left, entity, slots);
                self.scalar(right, entity, slots);
            }
            JudgmentExpr::And(items) | JudgmentExpr::Or(items) => {
                for item in items {
                    self.judgment(item, entity, slots);
                }
            }
            JudgmentExpr::Not(item) => self.judgment(item, entity, slots),
            JudgmentExpr::Derived(_) | JudgmentExpr::RelationMember { .. } => {}
        }
    }
}
