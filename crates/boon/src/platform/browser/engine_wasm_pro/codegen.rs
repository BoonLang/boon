use boon_scene::{RenderDiffBatch, RenderOp};

use super::exec_ir::ExecProgram;

pub fn emit_render_batch(program: &ExecProgram) -> RenderDiffBatch {
    let mut ops = Vec::with_capacity(1 + program.setup_ops.len());
    ops.push(RenderOp::ReplaceRoot(program.root.clone()));
    ops.extend(program.setup_ops.clone());
    RenderDiffBatch { ops }
}

#[cfg(test)]
mod tests {
    use boon_scene::RenderRoot;

    use super::emit_render_batch;
    use crate::platform::browser::engine_wasm_pro::exec_ir::ExecProgram;
    use crate::platform::browser::engine_wasm_pro::semantic_ir::{
        RuntimeModel, SemanticNode, SemanticProgram,
    };

    #[test]
    fn emit_render_batch_replaces_root_from_exec_program() {
        let exec = ExecProgram::from_semantic(&SemanticProgram {
            root: SemanticNode::element(
                "div",
                Some("root".to_string()),
                vec![("role".to_string(), "status".to_string())],
                Vec::new(),
                Vec::new(),
            ),
            runtime: RuntimeModel::Static,
        });
        let batch = emit_render_batch(&exec);

        assert_eq!(batch.ops.len(), 2);
        let boon_scene::RenderOp::ReplaceRoot(RenderRoot::UiTree(_)) = &batch.ops[0] else {
            panic!("expected ReplaceRoot(UiTree)");
        };
    }
}
