//! Build script for the dense PyO3 extension (`axiom_rules_engine_dense`).
//!
//! PyO3's `extension-module` feature deliberately leaves the Python C-API
//! symbols unresolved at link time — the host interpreter supplies them when it
//! loads the module. On macOS the system linker rejects those undefined symbols
//! unless it is told to defer them, so an extension-module `cdylib` must be
//! linked with `-undefined dynamic_lookup`. maturin injects those flags itself,
//! which is why `maturin develop` works, but a plain `cargo build` /
//! `cargo rustc` (used in CI and to read full compiler diagnostics locally)
//! does not — pyo3 0.27 no longer auto-emits them; a crate that wants to build
//! outside maturin must opt in from its own build script. Without this the
//! macOS `cdylib` link fails with "symbol(s) not found for architecture arm64"
//! (cargo exit status 101).
//!
//! `add_extension_module_link_args()` only emits flags for macOS (and
//! wasm32-unknown-emscripten); it is a no-op on Linux and Windows, so this is
//! safe on every target the workspace builds.
fn main() {
    pyo3_build_config::add_extension_module_link_args();
}
