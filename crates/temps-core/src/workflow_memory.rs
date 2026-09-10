// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Backward-compatibility re-export of `temps-memory`.
//!
//! The workflow memory trait, data types, and bash script moved to the
//! dedicated `temps-memory` crate (see ADR PR 2.1) so they can evolve
//! without touching `temps-core`. Existing consumers continue to import
//! `temps_core::workflow_memory::{WorkflowMemoryProvider, ...}` unchanged.
//!
//! **Deprecation path:** this shim stays indefinitely — `temps-core` is
//! a natural import site for "shared contracts", and moving consumers
//! to the new crate is not urgent. When we next revisit the workspace
//! graph, we'll either inline the re-exports here as plain `pub use`
//! (current state) or remove them after migrating consumers. No rush.

pub use temps_memory::{
    memory_install_command, WorkflowMemoryError, WorkflowMemoryFact, WorkflowMemoryProvider,
    MEMORY_SCRIPT, MEMORY_SCRIPT_DIR, MEMORY_SCRIPT_PATH,
};

#[cfg(test)]
mod tests {
    use super::*;

    /// Guardrail: the re-export must expose every identifier that lived
    /// in the pre-extraction `workflow_memory` module. If a name is
    /// added to `temps-memory` that consumers need, add it to the
    /// `pub use` above and extend this test. If a name is removed, the
    /// compiler catches it here first.
    #[test]
    fn reexport_covers_public_surface() {
        let _: fn() -> Vec<String> = memory_install_command;
        let _: &str = MEMORY_SCRIPT;
        let _: &str = MEMORY_SCRIPT_PATH;
        let _: &str = MEMORY_SCRIPT_DIR;
        // Type existence checks — compile-time assertions that the
        // re-exports resolve to concrete types consumers can name.
        let _fact: Option<WorkflowMemoryFact> = None;
        let _err: Option<WorkflowMemoryError> = None;
        fn _takes_provider<P: WorkflowMemoryProvider>(_: P) {}
    }
}
