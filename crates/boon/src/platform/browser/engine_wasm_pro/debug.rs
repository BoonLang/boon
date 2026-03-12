use super::{exec_ir::ExecProgram, semantic_ir::SemanticProgram};

#[must_use]
pub fn summarize(semantic: &SemanticProgram, exec: &ExecProgram) -> String {
    format!(
        "WasmPro semantic root: {:?}; exec root: {:?}",
        semantic.root, exec.root
    )
}
