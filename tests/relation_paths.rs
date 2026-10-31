//! Public integration entry point for the shared path properties.
mod actual_path_work {
    pub(super) fn take() -> Option<(usize, usize, usize, usize, [usize; 4])> {
        None
    }
}
#[path = "support/relation_paths.rs"]
mod relation_path_properties;
