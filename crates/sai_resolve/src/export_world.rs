use sai_diagnostics::{Diagnostic, Location};
use sai_hir::DefId;

/// A package's selected export world and its snapshot-bound diagnostics.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ExportWorld {
    pub world: Option<DefId>,
    pub diagnostics: Vec<Diagnostic>,
    /// The manifest value that selected this world, when the selection came
    /// from a tracked package contract.
    pub manifest_location: Option<Location>,
}
