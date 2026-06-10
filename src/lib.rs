/// Core engine version (`CARGO_PKG_VERSION`), for bindings to report provenance.
pub const ENGINE_VERSION: &str = env!("CARGO_PKG_VERSION");

pub mod api;
mod bulk;
pub mod compile;
pub mod dense;
pub mod engine;
mod formula;
pub mod model;
pub mod rulespec;
pub mod source;
pub mod spec;
