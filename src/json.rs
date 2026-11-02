//! Reading JSON that carries a program (#236).
//!
//! serde_json stops at 128 levels of nesting, so that a hostile document
//! cannot recurse its parser off the stack. A compiled program nests deeper
//! than that. A formula may nest [`MAX_FORMULA_DEPTH`] levels, and its lowered
//! tree serializes to as many as two levels of JSON for each: a sum is an
//! object holding its terms in an array. So `compile` wrote artifacts the
//! loader refused, from a sum of 61 terms or an `elif` chain of 119 branches
//! up.
//!
//! [`from_str`] reads JSON nested up to [`MAX_JSON_DEPTH`] levels. It counts
//! how deep the document's brackets nest first, in a loop, and only then
//! parses it with serde_json's own limit off. The bound is still what keeps
//! the parse on the stack; a document past it is an error, as it was at 128.
//!
//! JSON nested this deep can carry an expression deeper than any formula
//! compiles to, so the program it holds is checked against
//! [`crate::spec::MAX_EXPRESSION_DEPTH`] before anything walks it.

use serde::de::{DeserializeOwned, Error as _};

use crate::formula::MAX_FORMULA_DEPTH;

/// The deepest JSON the engine reads, in nested arrays and objects.
///
/// Four levels for each level a formula may nest. A formula at the bound
/// compiles to an artifact nested about two levels of JSON per level, so this
/// is about twice what `compile` can write. Measured in a release build,
/// deserializing an artifact that holds a formula at the bound took under
/// 0.2 MiB of stack, and loading it, which also checks its contract, 0.4 MiB
/// (`tests/formula_depth.rs` loads them on a 1 MiB thread).
pub const MAX_JSON_DEPTH: usize = 4 * MAX_FORMULA_DEPTH;

/// Deserialize `source` as serde_json does, but from JSON nested up to
/// [`MAX_JSON_DEPTH`] levels instead of 128.
pub(crate) fn from_str<T: DeserializeOwned>(source: &str) -> Result<T, serde_json::Error> {
    check_nesting(source, MAX_JSON_DEPTH)?;
    let mut deserializer = serde_json::Deserializer::from_str(source);
    deserializer.disable_recursion_limit();
    let value = T::deserialize(&mut deserializer)?;
    deserializer.end()?;
    Ok(value)
}

/// Refuse `source` if its brackets nest more than `max_depth` deep.
///
/// It counts every `[` and `{` outside a string, in the whole document.
/// serde_json's parser nests only at such a bracket, and stops at the first
/// error, so it never nests deeper than this count: the count bounds the
/// parse. It is an upper bound, not the parser's own depth. A malformed
/// document whose brackets nest past the bound after the point where
/// serde_json would have stopped, such as `{} [[[...` with 513 `[`, is
/// refused with this error rather than with serde_json's. Within serde_json's
/// own 128 levels, counted this way, the reader returns exactly what
/// `serde_json::from_str` does.
fn check_nesting(source: &str, max_depth: usize) -> Result<(), serde_json::Error> {
    let mut depth = 0_usize;
    let mut in_string = false;
    let mut escaped = false;
    let (mut line, mut line_start) = (1_usize, 0_usize);
    for (index, byte) in source.bytes().enumerate() {
        if byte == b'\n' {
            line += 1;
            line_start = index + 1;
        }
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'[' | b'{' => {
                depth += 1;
                if depth > max_depth {
                    return Err(serde_json::Error::custom(format!(
                        "JSON nests more than {max_depth} levels deep at line {line} column {}",
                        index - line_start + 1
                    )));
                }
            }
            b']' | b'}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn nested_arrays(levels: usize) -> String {
        format!("{}{}", "[".repeat(levels), "]".repeat(levels))
    }

    /// A deterministic generator, as in `tests/execution.rs`.
    struct Rng(u64);

    impl Rng {
        fn below(&mut self, bound: usize) -> usize {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            usize::try_from((self.0 >> 33) % u64::try_from(bound).expect("bound fits"))
                .expect("value fits")
        }
    }

    /// A JSON value `size` levels deep, with strings that hold brackets,
    /// quotes and backslashes.
    fn generate(rng: &mut Rng, size: usize) -> Value {
        const STRINGS: [&str; 6] = ["", "[", "}]", "a\"b", "\\", "\\\"{[\n"];
        match if size == 0 {
            rng.below(4)
        } else {
            4 + rng.below(4)
        } {
            0 => Value::Null,
            1 => Value::Bool(rng.below(2) == 0),
            2 => Value::from(rng.below(1_000)),
            3 => Value::from(STRINGS[rng.below(STRINGS.len())]),
            4 | 5 => {
                let mut items = Vec::new();
                for item in 0..1 + rng.below(3) {
                    let size = if item == 0 {
                        size - 1
                    } else {
                        rng.below(size.min(4))
                    };
                    items.push(generate(rng, size));
                }
                Value::Array(items)
            }
            _ => {
                let mut fields = serde_json::Map::new();
                for field in 0..1 + rng.below(3) {
                    let size = if field == 0 {
                        size - 1
                    } else {
                        rng.below(size.min(4))
                    };
                    let key = format!("{}{field}", STRINGS[rng.below(STRINGS.len())]);
                    fields.insert(key, generate(rng, size));
                }
                Value::Object(fields)
            }
        }
    }

    fn depth(value: &Value) -> usize {
        match value {
            Value::Array(items) => 1 + items.iter().map(depth).max().unwrap_or(0),
            Value::Object(fields) => 1 + fields.values().map(depth).max().unwrap_or(0),
            _ => 0,
        }
    }

    /// The nesting this module counts in a document is the nesting of the
    /// value serde_json reads from it, whatever its strings hold and however
    /// it is spaced.
    #[test]
    fn the_nesting_counted_is_the_nesting_of_the_value() {
        let mut deepest = 0;
        for seed in 0..2_000_u64 {
            let mut rng = Rng(seed.wrapping_add(0x9e37_79b9_7f4a_7c15));
            let size = usize::try_from(seed % 40).expect("small size");
            let value = generate(&mut rng, size);
            let depth = depth(&value);
            deepest = deepest.max(depth);
            for text in [
                serde_json::to_string(&value).expect("value serializes"),
                serde_json::to_string_pretty(&value).expect("value serializes"),
            ] {
                assert!(check_nesting(&text, depth).is_ok(), "seed {seed}: {text}");
                assert_eq!(
                    check_nesting(&text, depth.saturating_sub(1)).is_err(),
                    depth > 0,
                    "seed {seed}: {text}"
                );
            }
        }
        assert_eq!(deepest, 39, "the generator's deepest document");
    }

    /// Within serde_json's own limit the two readers agree on every document:
    /// the same value, or the same error for a damaged one.
    #[test]
    fn it_reads_what_serde_json_reads() {
        for seed in 0..2_000_u64 {
            let mut rng = Rng(seed.wrapping_add(0x9e37_79b9_7f4a_7c15));
            let size = usize::try_from(seed % 40).expect("small size");
            let mut text =
                serde_json::to_string(&generate(&mut rng, size)).expect("value serializes");
            if seed % 5 == 1 {
                // Trail it with brackets, still within 128 levels in all.
                let levels = rng.below(128 - depth_of_text(&text));
                text.push_str(&" [".repeat(levels));
            } else if seed % 4 == 0 && !text.is_empty() {
                // Damage it: cut it short, or drop one byte.
                let at = rng.below(text.len());
                if text.is_char_boundary(at) && text.is_char_boundary(at + 1) {
                    if seed % 8 == 0 {
                        text.truncate(at);
                    } else {
                        text.remove(at);
                    }
                }
            }
            let ours = from_str::<Value>(&text).map_err(|error| error.to_string());
            let theirs = serde_json::from_str::<Value>(&text).map_err(|error| error.to_string());
            assert_eq!(ours, theirs, "seed {seed}: {text}");
        }
    }

    /// How deep a document's brackets nest, as `check_nesting` counts them.
    fn depth_of_text(text: &str) -> usize {
        (0..=MAX_JSON_DEPTH)
            .find(|levels| check_nesting(text, *levels).is_ok())
            .expect("within the bound")
    }

    /// A malformed document whose brackets nest past the bound after the
    /// value is refused by the count, which bounds the parse rather than
    /// matching it (see `check_nesting`).
    #[test]
    fn brackets_past_the_bound_after_the_value_are_counted() {
        let text = format!("{{}} {}", "[".repeat(MAX_JSON_DEPTH + 1));
        assert_eq!(
            from_str::<Value>(&text)
                .expect_err("past the bound")
                .to_string(),
            format!(
                "JSON nests more than {MAX_JSON_DEPTH} levels deep at line 1 column {}",
                3 + MAX_JSON_DEPTH + 1
            )
        );
        // serde_json stops at the first trailing character.
        assert!(
            serde_json::from_str::<Value>(&text)
                .expect_err("trailing characters")
                .to_string()
                .contains("trailing characters")
        );
    }

    /// The bound: a document at it reads, one level past it is an error that
    /// says where, and a document nested a million levels deep is that error
    /// too, not a stack overflow.
    #[test]
    fn a_document_past_the_bound_is_an_error() {
        // A spawned thread's default stack would do in a release build; a
        // debug build's frames are several times larger.
        let read = |levels: usize| {
            std::thread::Builder::new()
                .stack_size(16 * 1024 * 1024)
                .spawn(move || {
                    from_str::<Value>(&nested_arrays(levels)).map_err(|error| error.to_string())
                })
                .expect("spawn test thread")
                .join()
                .expect("test thread")
        };
        assert_eq!(
            depth(&read(MAX_JSON_DEPTH).expect("at the bound")),
            MAX_JSON_DEPTH
        );
        for levels in [MAX_JSON_DEPTH + 1, 1_000_000] {
            assert_eq!(
                read(levels).expect_err("past the bound"),
                format!(
                    "JSON nests more than {MAX_JSON_DEPTH} levels deep at line 1 column {}",
                    MAX_JSON_DEPTH + 1
                )
            );
        }
        assert_eq!(
            check_nesting("[\n  [\n    [1]\n  ]\n]", 2)
                .expect_err("three levels")
                .to_string(),
            "JSON nests more than 2 levels deep at line 3 column 5"
        );
    }
}
