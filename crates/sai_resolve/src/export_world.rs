use sai_diagnostics::Diagnostic;
use sai_hir::DefId;

/// A package's selected export world and its snapshot-bound diagnostics.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ExportWorld {
    pub world: Option<DefId>,
    pub diagnostics: Vec<Diagnostic>,
}
