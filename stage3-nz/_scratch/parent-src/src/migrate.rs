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
