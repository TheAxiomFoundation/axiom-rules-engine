//! Experimental stage-2 prototype of the ratified unit-derivation contract.
//!
//! This module is compiled only with the off-by-default `unit-derivation`
//! Cargo feature. Its runtime configuration is also disabled by default. It
//! deliberately does not change RuleSpec, artifact v2, the ordinary engine,
//! or any release/launch surface.

mod compile;
mod evaluate;
mod interface;
#[cfg(test)]
mod tests;
mod types;

pub use compile::{CompiledConstitution, compile};
pub use evaluate::{derive_units, unit_id};
pub use interface::{
    EntityRegistry, FrozenLedger, PhaseTwoDataSet, PhaseTwoEngine, Prototype, ShadowChannel,
    lift_derived_relation, materialize_phase_two_dataset, serialize_experimental_run,
};
pub use types::*;

/// Experimental identity required by the prototype interface. This value is
/// intentionally not an artifact-format or release-line version.
pub const EXPERIMENTAL_SEMANTICS_VERSION: &str = "unit-derivation-stage2/1";
