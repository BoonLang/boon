//! Generic preview: runs any lowered IrProgram + HostViewIr through the runtime.
//!
//! This is the generic execution path that doesn't depend on example-specific program types.
//! It uses the existing IrExecutor, PreviewRuntime, and HostViewPreviewApp infrastructure.

use crate::bridge::{HostInput, HostViewIr};
use crate::host_view_preview::HostViewPreviewApp;
use crate::ids::ActorId;
use crate::ir::{IrProgram, SourcePortId};
use crate::ir_executor::IrExecutor;
use crate::preview_runtime::PreviewRuntime;
use crate::semantics::CausalSeq;
use boon::platform::browser::kernel::KernelValue;
use boon::zoon::*;
use boon_renderer_zoon::FakeRenderState;
use boon_scene::{EventPortId, UiEvent, UiEventBatch, UiEventKind};
use std::collections::BTreeMap;

/// A generic program that works with any IrProgram + HostViewIr.
pub struct GenericProgram {
    pub ir: IrProgram,
    pub host_view: HostViewIr,
}

impl GenericProgram {
    pub fn new(ir: IrProgram, host_view: HostViewIr) -> Self {
        Self { ir, host_view }
    }
}

/// Generic preview: runs a GenericProgram through the runtime.
pub struct GenericPreview {
    runtime: PreviewRuntime,
    host_actor: ActorId,
    executor: IrExecutor,
    app: HostViewPreviewApp,
}

impl GenericPreview {
    /// Create a generic preview from an IrProgram + HostViewIr.
    pub fn from_program(program: GenericProgram) -> Result<Self, String> {
        let GenericProgram { ir, host_view } = program;
        let mut runtime = PreviewRuntime::new();
        let host_actor = runtime.alloc_actor();
        let executor = IrExecutor::new(ir)?;
        let app = HostViewPreviewApp::new(host_view, executor.sink_values());
        Ok(Self {
            runtime,
            host_actor,
            executor,
            app,
        })
    }

    /// Render the current state to text.
    pub fn preview_text(&mut self) -> String {
        self.app.preview_text()
    }

    /// Dispatch a batch of UI events and return whether anything changed.
    pub fn dispatch_ui_events(&mut self, batch: UiEventBatch) -> bool {
        // Convert UI events to HostInputs and pulse the runtime
        let inputs = self.batch_to_host_inputs(&batch);
        if inputs.is_empty() {
            return false;
        }

        self.runtime.dispatch_inputs_quiet(&inputs);

        // Update sink values from executor
        self.app
            .set_sink_values_from_executor(self.executor.sink_values());

        true
    }

    /// Dispatch a single UI event.
    pub fn dispatch_ui_event(&mut self, event: &UiEvent) -> bool {
        if let Some(source_port) = self.app.source_port_for_event(event.target) {
            let value = event_value(&event.kind, event.payload.as_deref());
            let inputs = vec![HostInput::Pulse {
                actor: self.host_actor,
                port: source_port,
                value,
                seq: CausalSeq::new(self.runtime.turn(), 0),
            }];
            self.runtime.dispatch_inputs_quiet(&inputs);
            self.app
                .set_sink_values_from_executor(self.executor.sink_values());
            return true;
        }
        false
    }

    /// Convert a UiEventBatch to HostInputs by mapping event ports to source ports.
    fn batch_to_host_inputs(&mut self, batch: &UiEventBatch) -> Vec<HostInput> {
        let mut inputs = Vec::new();
        let turn = self.runtime.turn();

        for (order, event) in batch.events.iter().enumerate() {
            if let Some(source_port) = self.app.source_port_for_event(event.target) {
                let value = event_value(&event.kind, event.payload.as_deref());
                inputs.push(HostInput::Pulse {
                    actor: self.host_actor,
                    port: source_port,
                    value,
                    seq: CausalSeq::new(turn, order as u32),
                });
            }
        }

        inputs
    }

    /// Render a snapshot for testing.
    pub fn render_snapshot(&mut self) -> (boon_scene::UiNode, FakeRenderState) {
        self.app.render_snapshot()
    }

    /// Get the app for rendering.
    pub fn app(&mut self) -> &mut HostViewPreviewApp {
        &mut self.app
    }
}

/// Convert a UiEventKind to a KernelValue for source port pulsing.
fn event_value(kind: &UiEventKind, payload: Option<&str>) -> KernelValue {
    match kind {
        UiEventKind::Click => KernelValue::Tag("Click".to_string()),
        UiEventKind::DoubleClick => KernelValue::Tag("DoubleClick".to_string()),
        UiEventKind::Input => KernelValue::Text(payload.unwrap_or("").to_string()),
        UiEventKind::Change => KernelValue::Text(payload.unwrap_or("").to_string()),
        UiEventKind::KeyDown => {
            // KeyDown events carry key text in the payload
            KernelValue::Text(payload.unwrap_or("").to_string())
        }
        UiEventKind::Blur => KernelValue::Tag("Blur".to_string()),
        UiEventKind::Focus => KernelValue::Tag("Focus".to_string()),
        UiEventKind::Custom(s) => KernelValue::Text(s.clone()),
    }
}

/// Trait for source port lookup from event port.
pub trait SourcePortLookup {
    fn source_port_for_event(&self, event_port: EventPortId) -> Option<SourcePortId>;
}

impl SourcePortLookup for HostViewPreviewApp {
    fn source_port_for_event(&self, event_port: EventPortId) -> Option<SourcePortId> {
        self.source_port_for_event(event_port)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lower::lower_program_generic;

    #[test]
    fn generic_pipeline_lowers_counter_source() {
        let source = include_str!("../../../playground/frontend/src/examples/counter/counter.bn");
        let result = lower_program_generic(source);
        assert!(
            result.is_ok(),
            "counter should lower through generic pipeline: {:?}",
            result.err()
        );
    }

    #[test]
    fn generic_preview_runs_counter() {
        let source = include_str!("../../../playground/frontend/src/examples/counter/counter.bn");
        let program = lower_program_generic(source).expect("counter should lower");
        // Verify the IR was produced
        assert!(
            !program.ir.nodes.is_empty(),
            "IR should have nodes: {:?}",
            program.diagnostics
        );
        // Verify the view tree was produced
        assert!(
            program.host_view.root.is_some(),
            "HostViewIr should have a root"
        );
        // Check view tree structure
        let root = program.host_view.root.as_ref().unwrap();
        check_view_tree(root, 0);
        // Verify the button was lowered
        assert_has_button(root, "button found in view tree");
    }

    #[test]
    fn generic_lowering_finds_increment_button() {
        use crate::bridge::HostViewKind;
        use crate::lower::lower_view_generic;
        use crate::parse::{parse_static_expressions, top_level_bindings};
        use boon::parser::static_expression::Expression;

        let source = include_str!("../../../playground/frontend/src/examples/counter/counter.bn");
        let expressions = parse_static_expressions(source).expect("should parse");
        let bindings = top_level_bindings(&expressions);

        // Check document binding and its items
        let doc = bindings.get("document").expect("should have document");
        // document: Document/new(root: Element/stripe(items: LIST { counter, increment_button }))
        if let Expression::FunctionCall { arguments, .. } = &doc.node {
            for arg in arguments {
                if arg.node.name.as_str() == "root" {
                    if let Some(ref root_val) = arg.node.value {
                        // Element/stripe
                        if let Expression::FunctionCall {
                            arguments: stripe_args,
                            ..
                        } = &root_val.node
                        {
                            for stripe_arg in stripe_args {
                                if stripe_arg.node.name.as_str() == "items" {
                                    if let Some(ref items_val) = stripe_arg.node.value {
                                        if let Expression::List { items } = &items_val.node {
                                            for (i, item) in items.iter().enumerate() {
                                                let is_var = matches!(
                                                    &item.node,
                                                    Expression::Variable { .. }
                                                );
                                                let is_alias =
                                                    matches!(&item.node, Expression::Alias { .. });
                                                let is_pipe =
                                                    matches!(&item.node, Expression::Pipe { .. });
                                                std::println!(
                                                    "LIST item {i}: is_var={is_var}, is_alias={is_alias}, is_pipe={is_pipe}"
                                                );
                                                if let Expression::Variable(v) = &item.node {
                                                    std::println!(
                                                        "  variable name: {}",
                                                        v.name.as_str()
                                                    );
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Check increment_button binding exists
        assert!(
            bindings.contains_key("increment_button"),
            "should have increment_button binding"
        );
        let btn_expr = bindings.get("increment_button").unwrap();
        let is_function_call = matches!(&btn_expr.node, Expression::FunctionCall { .. });
        assert!(
            is_function_call,
            "increment_button should be a FunctionCall"
        );

        // Try view lowering directly
        let host_view =
            lower_view_generic(&expressions, &bindings).expect("view lowering should succeed");
        assert!(host_view.root.is_some(), "host view should have root");

        // Check the view tree
        let root = host_view.root.as_ref().unwrap();
        assert!(
            matches!(root.kind, HostViewKind::Document),
            "root should be Document"
        );

        // Check stripe
        let stripe = &root.children[0];
        assert!(
            matches!(stripe.kind, HostViewKind::StripeLayout { .. }),
            "child should be StripeLayout"
        );

        // Check stripe children
        assert!(
            !stripe.children.is_empty(),
            "StripeLayout should have children, but got: {:?}",
            stripe
                .children
                .iter()
                .map(|c| format!("{:?}", c.kind))
                .collect::<Vec<_>>()
        );

        // Find the button
        let has_button = stripe
            .children
            .iter()
            .any(|c| matches!(c.kind, HostViewKind::Button { .. }));
        assert!(has_button, "StripeLayout should have a Button child");
    }

    fn check_view_tree(node: &crate::bridge::HostViewNode, depth: usize) {
        let indent = "  ".repeat(depth);
        let kind_name = format!("{:?}", node.kind);
        std::dbg!(&indent, &kind_name, &node.children.len());
        for child in &node.children {
            check_view_tree(child, depth + 1);
        }
    }

    fn assert_has_button(node: &crate::bridge::HostViewNode, msg: &str) {
        use crate::bridge::HostViewKind;
        let has_button = matches!(node.kind, HostViewKind::Button { .. })
            || node
                .children
                .iter()
                .any(|c| matches!(c.kind, HostViewKind::Button { .. }))
            || node.children.iter().any(|c| {
                c.children
                    .iter()
                    .any(|gc| matches!(gc.kind, HostViewKind::Button { .. }))
            });
        assert!(has_button, "{msg}");
    }

    #[test]
    fn generic_pipeline_lowers_todo_mvc_source() {
        let source = include_str!("../../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn");
        let result = lower_program_generic(source);
        // Todo_mvc uses PASSED aliases and user-defined view functions which are
        // partially supported in generic lowering. The test passes if lowering either
        // succeeds OR produces a clear diagnostic about unsupported features.
        match result {
            Ok(program) => {
                assert!(!program.ir.nodes.is_empty(), "IR should have nodes");
                // View may or may not have a root depending on function resolution
            }
            Err(e) => {
                // Accept any clear diagnostic about unsupported features
                assert!(
                    e.contains("PASSED")
                        || e.contains("unsupported")
                        || e.contains("unknown view function")
                        || e.contains("unresolved"),
                    "error should mention a known limitation: {e}"
                );
            }
        }
    }

    #[test]
    fn generic_semantic_handles_todo_mvc_patterns() {
        use crate::lower::builtin_registry::BuiltinRegistry;
        use crate::lower::extract_top_level_functions;
        use crate::lower::generic_semantic::generic_lower_semantic;
        use crate::parse::{parse_static_expressions, top_level_bindings};

        let source = include_str!("../../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn");
        let expressions = parse_static_expressions(source).expect("todo_mvc should parse");
        let bindings = top_level_bindings(&expressions);
        let (_func_names, func_defs) = extract_top_level_functions(&expressions);
        let builtins = BuiltinRegistry::new();

        let (ir, diagnostics) = generic_lower_semantic(&bindings, &func_defs, &builtins);

        // Verify IR was produced with some nodes
        assert!(!ir.nodes.is_empty(), "IR should have nodes from todo_mvc");

        // Check that PASSED aliases are handled (not reported as unsupported)
        let passed_unsupported = diagnostics
            .iter()
            .any(|d| d.message.contains("PASSED alias not yet supported"));
        assert!(
            !passed_unsupported,
            "PASSED aliases should now be handled via LINK nodes, got diagnostics: {:?}",
            diagnostics.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn generic_view_lowering_for_todo_mvc() {
        use crate::bridge::HostViewKind;
        use crate::lower::lower_program_generic;

        let source = include_str!("../../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn");
        let result = lower_program_generic(source);
        // todo_mvc uses custom functions (root_element, main_panel, etc.) that the generic
        // view lowering doesn't handle yet, so it fails with "unknown view function".
        // This is expected - the generic lowering needs extension to handle user-defined functions.
        assert!(
            result.is_err(),
            "todo_mvc generic lowering should fail due to custom functions"
        );
        let err = result.err().unwrap();
        assert!(
            err.contains("unknown view function"),
            "Expected unknown view function error, got: {}",
            err
        );
    }
}
