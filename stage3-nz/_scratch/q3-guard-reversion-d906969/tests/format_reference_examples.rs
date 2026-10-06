//! Every ```yaml block in docs/rulespec-format.md must lower through the real
//! RuleSpec loader, so the format reference cannot drift from the grammar it
//! documents: an example that stops parsing breaks the build.

const REFERENCE: &str = include_str!("../docs/rulespec-format.md");

fn yaml_blocks(markdown: &str) -> Vec<&str> {
    let mut blocks = Vec::new();
    let mut rest = markdown;
    while let Some(start) = rest.find("```yaml\n") {
        let body = &rest[start + "```yaml\n".len()..];
        let end = body.find("\n```").expect("unterminated yaml block");
        blocks.push(&body[..end + 1]);
        rest = &body[end + "\n```".len()..];
    }
    blocks
}

#[test]
fn every_reference_example_lowers() {
    let blocks = yaml_blocks(REFERENCE);
    assert!(
        blocks.len() >= 6,
        "the format reference should keep its worked examples; found {}",
        blocks.len(),
    );
    for (index, block) in blocks.iter().enumerate() {
        axiom_rules_engine::rulespec::lower_rulespec_str(block).unwrap_or_else(|error| {
            panic!("format-reference yaml block {index} no longer lowers: {error}\n---\n{block}")
        });
    }
}
