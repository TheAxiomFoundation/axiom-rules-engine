use axiom_rules_engine::rulespec::load_rulespec_file;

#[test]
fn all_rulespec_files_parse_and_lower() {
    fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let p = entry.unwrap().path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.file_name().and_then(|s| s.to_str()) == Some("rules.yaml") {
                out.push(p);
            }
        }
    }
    let mut rulespec_files = Vec::new();
    walk(
        std::path::Path::new("tests/fixtures/rulespec"),
        &mut rulespec_files,
    );
    rulespec_files.sort();
    let mut failures: Vec<String> = Vec::new();
    for p in &rulespec_files {
        match load_rulespec_file(p) {
            Ok(_) => {}
            Err(e) => failures.push(format!("{}: {}", p.display(), e)),
        }
    }
    assert!(
        failures.is_empty(),
        "RuleSpec files failed to load: {}",
        failures.join("\n  ")
    );
    eprintln!("  loaded {} RuleSpec files", rulespec_files.len());
}
