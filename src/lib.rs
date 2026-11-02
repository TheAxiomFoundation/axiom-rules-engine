// Shared integration properties also run inside the library to observe
// test-only evaluator work counters without exposing a runtime API.
#[cfg(test)]
extern crate self as axiom_rules_engine;

/// Core engine version (`CARGO_PKG_VERSION`), for bindings to report provenance.
pub const ENGINE_VERSION: &str = env!("CARGO_PKG_VERSION");

pub mod api;
mod bulk;
pub mod compile;
pub mod dense;
pub mod depth;
mod digest;
pub mod engine;
mod formula;
pub mod interface;
mod lazy;
pub mod migrate;
pub mod model;
mod relation_direction;
pub mod relation_typing;
pub mod rulespec;
#[cfg(feature = "schema")]
pub mod schema;
pub mod selected_year;
pub mod source;
pub mod spec;
#[cfg(feature = "unit-derivation")]
pub mod unit_derivation;
