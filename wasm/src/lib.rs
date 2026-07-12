//! Browser/wasm bindings for the Axiom Rules Engine.
//!
//! The boundary is JSON strings in both directions, reusing the exact serde
//! types the CLI already speaks: [`compile`] returns the same
//! `CompiledProgramArtifact` JSON the CLI's `compile` subcommand writes, and
//! [`execute`] takes a `CompiledExecutionRequest` and returns an
//! `ExecutionResponse` exactly as the CLI's `execute` subcommand reads and
//! writes them. No types are redefined at the boundary, so the wasm host and
//! the CLI cannot drift apart.
//!
//! The core is built with `default-features = false`: no filesystem,
//! environment, or clock access. Module text arrives through an in-memory
//! [`ModuleSource`], so a browser host can compile and execute entirely
//! on-device — household data never leaves the page.

use std::collections::HashMap;

use axiom_rules_engine::api::{CompiledExecutionRequest, execute_compiled_request};
use axiom_rules_engine::compile::{ARTIFACT_FORMAT_VERSION, CompiledProgramArtifact};
use axiom_rules_engine::source::{ModuleSource, SourceError};
use wasm_bindgen::prelude::*;

/// `ModuleSource` over a `{canonical_target: yaml_text}` map — the browser
/// bundle shape anticipated by `axiom_rules_engine::source`.
struct InMemoryModuleSource {
    modules: HashMap<String, String>,
}

impl ModuleSource for InMemoryModuleSource {
    fn load(&self, target: &str) -> Result<Option<String>, SourceError> {
        Ok(self.modules.get(target).cloned())
    }
}

#[wasm_bindgen(start)]
fn init() {
    // Surface engine panics as readable console errors instead of a bare
    // `RuntimeError: unreachable`.
    console_error_panic_hook::set_once();
}

/// Compile the RuleSpec module graph rooted at `root_target`.
///
/// `modules_json` is a JSON object mapping canonical targets (for example
/// `"us:policies/usda/snap/fy-2026-cola/maximum-allotments"`) to RuleSpec
/// YAML text. Every module the root (transitively) imports must be present
/// under its exact canonical target. Imports are absolute canonical targets,
/// so durable ids are identical across hosts.
///
/// Returns the `CompiledProgramArtifact` serialized as JSON — the same
/// artifact format the CLI's `compile` subcommand writes, suitable for
/// caching and for [`execute`].
#[wasm_bindgen]
pub fn compile(modules_json: &str, root_target: &str) -> Result<String, JsError> {
    let modules: HashMap<String, String> = serde_json::from_str(modules_json).map_err(|error| {
        JsError::new(&format!(
            "modules_json must be a JSON object of {{canonical_target: yaml_text}}: {error}"
        ))
    })?;
    let source = InMemoryModuleSource { modules };
    let artifact = CompiledProgramArtifact::from_rulespec_with_source(root_target, &source)
        .map_err(|error| JsError::new(&error.to_string()))?;
    serde_json::to_string(&artifact).map_err(|error| JsError::new(&error.to_string()))
}

/// Execute a `CompiledExecutionRequest` against a compiled artifact.
///
/// `artifact_json` is the JSON produced by [`compile`] (or by the CLI's
/// `compile` subcommand — the formats are identical); `request_json` is a
/// `CompiledExecutionRequest` (`mode`, `dataset`, `queries`). Returns the
/// `ExecutionResponse` as JSON, byte-compatible with the CLI's `execute`
/// subcommand output.
///
/// Missing, older, or newer artifact versions are rejected, mirroring the
/// core's exact prelaunch v1 contract.
#[wasm_bindgen]
pub fn execute(artifact_json: &str, request_json: &str) -> Result<String, JsError> {
    let artifact = CompiledProgramArtifact::from_json_str(artifact_json)
        .map_err(|error| JsError::new(&error.to_string()))?;
    let request: CompiledExecutionRequest = serde_json::from_str(request_json).map_err(|error| {
        JsError::new(&format!(
            "request_json is not a CompiledExecutionRequest: {error}"
        ))
    })?;
    let response = execute_compiled_request(artifact, request)
        .map_err(|error| JsError::new(&error.to_string()))?;
    serde_json::to_string(&response).map_err(|error| JsError::new(&error.to_string()))
}

/// Version of the core `axiom-rules-engine` crate compiled into this binary,
/// for provenance display in UIs. Matches the `engine_version` stamped into
/// artifacts returned by [`compile`].
#[wasm_bindgen]
pub fn engine_version() -> String {
    axiom_rules_engine::ENGINE_VERSION.to_string()
}

/// The exact artifact format version this engine writes and accepts.
#[wasm_bindgen]
pub fn artifact_format_version() -> u32 {
    ARTIFACT_FORMAT_VERSION
}
