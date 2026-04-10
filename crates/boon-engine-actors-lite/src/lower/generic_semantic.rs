//! Generic semantic lowering: walks any Boon expression tree and produces `IrProgram`.
//!
//! This module is intentionally free of example-name branches, source-marker checks,
//! or business-specific lowering helpers. It handles the full Boon expression language
//! generically.

use crate::ir::{
    CallSiteId, FunctionId, IrFunctionTemplate, IrNode, IrNodeKind, IrNodePersistence, IrProgram,
    MatchArm, NodeId, PersistPolicy, SinkPortId, SourcePortId,
};
use crate::lower::builtin_registry::BuiltinRegistry;
use boon::parser::static_expression::{
    ArithmeticOperator, Comparator, Expression, Literal, Pattern, TextPart,
};
use boon::parser::{Span as SimpleSpan, Spanned, StrSlice};
use boon::platform::browser::kernel::KernelValue;
use std::collections::BTreeMap;

use crate::parse::StaticSpannedExpression;

/// Diagnostic for unsupported constructs during lowering.
#[derive(Debug, Clone)]
pub struct LowerDiagnostic {
    pub span: SimpleSpan,
    pub message: String,
}

/// Context for generic semantic lowering.
pub struct GenericSemanticLowerCtx<'a> {
    /// Top-level bindings: name -> expression.
    pub bindings: &'a BTreeMap<String, &'a StaticSpannedExpression>,
    /// Top-level function definitions: name -> (parameters, body).
    pub functions: &'a BTreeMap<
        String,
        (
            Vec<boon::parser::static_expression::Spanned<StrSlice>>,
            StaticSpannedExpression,
        ),
    >,
    /// Builtin registry for function-call dispatch.
    pub builtins: &'a BuiltinRegistry,
    /// Nodes accumulated during lowering.
    pub nodes: Vec<IrNode>,
    /// Function templates accumulated during lowering.
    pub functions_ir: Vec<IrFunctionTemplate>,
    /// Persistence entries.
    pub persistence: Vec<IrNodePersistence>,
    /// Diagnostics for unsupported constructs.
    pub diagnostics: Vec<LowerDiagnostic>,
    /// Next node id.
    next_node: u32,
    /// Next function id.
    next_function: u32,
    /// Next call-site id.
    next_call_site: u32,
    /// Next source port id.
    next_source_port: u32,
    /// Next sink port id.
    next_sink_port: u32,
    /// Next mirror cell id.
    next_mirror_cell: u32,
    /// Resolved source ports for alias paths (e.g., `increment_button.event.press`).
    pub source_ports: BTreeMap<Vec<String>, SourcePortId>,
    /// Resolved sink ports for binding names.
    pub sink_ports: BTreeMap<String, SinkPortId>,
    /// Resolved variable -> node mapping within the current scope.
    pub var_nodes: BTreeMap<String, NodeId>,
}

impl<'a> GenericSemanticLowerCtx<'a> {
    pub fn new(
        bindings: &'a BTreeMap<String, &'a StaticSpannedExpression>,
        functions: &'a BTreeMap<
            String,
            (
                Vec<boon::parser::static_expression::Spanned<StrSlice>>,
                StaticSpannedExpression,
            ),
        >,
        builtins: &'a BuiltinRegistry,
    ) -> Self {
        Self {
            bindings,
            functions,
            builtins,
            nodes: Vec::new(),
            functions_ir: Vec::new(),
            persistence: Vec::new(),
            diagnostics: Vec::new(),
            next_node: 1,
            next_function: 1,
            next_call_site: 1,
            next_source_port: 1,
            next_sink_port: 1,
            next_mirror_cell: 1,
            source_ports: BTreeMap::new(),
            sink_ports: BTreeMap::new(),
            var_nodes: BTreeMap::new(),
        }
    }

    fn alloc_node(&mut self, kind: IrNodeKind, source_span: Option<SimpleSpan>) -> NodeId {
        let id = NodeId(self.next_node);
        self.next_node += 1;
        self.nodes.push(IrNode {
            id,
            source_expr: source_span
                .map(|s| boon::platform::browser::kernel::ExprId(s.start as u32)),
            kind,
        });
        id
    }

    fn alloc_source_port(&mut self) -> SourcePortId {
        let id = SourcePortId(self.next_source_port);
        self.next_source_port += 1;
        id
    }

    fn alloc_sink_port(&mut self) -> SinkPortId {
        let id = SinkPortId(self.next_sink_port);
        self.next_sink_port += 1;
        id
    }

    fn alloc_function_id(&mut self) -> FunctionId {
        let id = FunctionId(self.next_function);
        self.next_function += 1;
        id
    }

    fn alloc_call_site_id(&mut self) -> CallSiteId {
        let id = CallSiteId(self.next_call_site);
        self.next_call_site += 1;
        id
    }

    fn add_persistence(&mut self, node: NodeId, policy: PersistPolicy) {
        if !matches!(policy, PersistPolicy::None) {
            self.persistence.push(IrNodePersistence { node, policy });
        }
    }

    fn unsupported(&mut self, span: SimpleSpan, message: String) {
        self.diagnostics.push(LowerDiagnostic { span, message });
    }

    /// Lower a single expression into an IR node, returning the node id.
    pub fn lower_expr(&mut self, expr: &StaticSpannedExpression) -> NodeId {
        match &expr.node {
            Expression::Literal(lit) => self.lower_literal(lit, expr.span),
            Expression::Variable(var) => self.lower_variable(var),
            Expression::Alias(alias) => self.lower_alias(alias, expr.span),
            Expression::Link => self.lower_link(expr.span),
            Expression::Skip => self.lower_skip(expr.span),
            Expression::Pipe { from, to } => self.lower_pipe(from, to, expr.span),
            Expression::Block { variables, output } => {
                self.lower_block(variables, output, expr.span)
            }
            Expression::Comparator(comp) => self.lower_comparator(comp, expr.span),
            Expression::ArithmeticOperator(op) => self.lower_arithmetic(op, expr.span),
            Expression::Latest { inputs } => self.lower_latest(inputs, expr.span),
            Expression::Hold { state_param, body } => self.lower_hold(state_param, body, expr.span),
            Expression::Then { body } => self.lower_then(body, expr.span),
            Expression::When { arms } => self.lower_when(arms, expr.span),
            Expression::While { arms } => self.lower_while(arms, expr.span),
            Expression::FunctionCall { path, arguments } => {
                self.lower_function_call(path, arguments, expr.span)
            }
            Expression::List { items } => self.lower_list_literal(items, expr.span),
            Expression::Object(obj) => self.lower_object(obj, expr.span),
            Expression::TextLiteral { parts, .. } => self.lower_text_literal(parts, expr.span),
            Expression::TaggedObject { tag, object } => {
                self.lower_tagged_object(tag, object, expr.span)
            }
            Expression::FieldAccess { path } => self.lower_field_access(path, expr.span),
            Expression::PostfixFieldAccess { expr: inner, field } => {
                self.lower_postfix_field_access(inner, field, expr.span)
            }
            Expression::Flush { value } => self.lower_flush(value, expr.span),
            Expression::Spread { value } => self.lower_spread(value, expr.span),
            Expression::Map { entries } => self.lower_map(entries, expr.span),
            Expression::Function {
                name,
                parameters,
                body,
            } => self.lower_function_def(name, parameters, body, expr.span),
            Expression::LinkSetter { alias } => self.lower_link_setter(alias, expr.span),
            Expression::Bits { .. } | Expression::Memory { .. } | Expression::Bytes { .. } => {
                self.unsupported(
                    expr.span,
                    format!(
                        "hardware type {:?} not supported in generic lowering",
                        expr.node
                    ),
                );
                self.alloc_node(
                    IrNodeKind::Literal(KernelValue::Number(f64::NAN)),
                    Some(expr.span),
                )
            }
        }
    }

    fn lower_literal(&mut self, lit: &Literal, span: SimpleSpan) -> NodeId {
        let value = match lit {
            Literal::Number(n) => KernelValue::Number(*n),
            Literal::Tag(tag) => KernelValue::Tag(tag.as_str().to_string()),
            Literal::Text(text) => KernelValue::Text(text.as_str().to_string()),
        };
        self.alloc_node(IrNodeKind::Literal(value), Some(span))
    }

    fn lower_variable(&mut self, var: &boon::parser::static_expression::Variable) -> NodeId {
        let name = var.name.as_str().to_string();
        if let Some(&node_id) = self.var_nodes.get(&name) {
            node_id
        } else if let Some(binding_expr) = self.bindings.get(&name) {
            let node_id = self.lower_expr(binding_expr);
            self.var_nodes.insert(name, node_id);
            node_id
        } else {
            // Unresolved variable — emit diagnostic and return a placeholder
            self.unsupported(var.value.span, format!("unresolved variable: {name}"));
            self.alloc_node(
                IrNodeKind::Literal(KernelValue::Number(f64::NAN)),
                Some(var.value.span),
            )
        }
    }

    fn lower_alias(
        &mut self,
        alias: &boon::parser::static_expression::Alias,
        span: SimpleSpan,
    ) -> NodeId {
        match alias {
            boon::parser::static_expression::Alias::WithoutPassed { parts, .. } => {
                // An alias like `counter` or `elements.filter_buttons.all`
                let full_path: Vec<String> = parts.iter().map(|s| s.as_str().to_string()).collect();

                // Check if this is a known source port path
                if let Some(&port) = self.source_ports.get(&full_path) {
                    return self.alloc_node(IrNodeKind::SourcePort(port), Some(span));
                }

                // Otherwise treat as a variable binding
                if full_path.len() == 1 {
                    let name = &full_path[0];
                    if let Some(binding_expr) = self.bindings.get(name.as_str()) {
                        return self.lower_expr(binding_expr);
                    }
                    // Check if it's a parameter in current scope
                    if let Some(&node_id) = self.var_nodes.get(name) {
                        return node_id;
                    }
                }

                self.unsupported(
                    span,
                    format!("unresolved alias path: {}", full_path.join(".")),
                );
                self.alloc_node(
                    IrNodeKind::Literal(KernelValue::Number(f64::NAN)),
                    Some(span),
                )
            }
            boon::parser::static_expression::Alias::WithPassed { extra_parts, .. } => {
                // PASSED aliases like `PASSED.store.todos` or `PASSED.store.elements.filter_buttons.all`
                // These refer to values passed from the parent function's scope via PASS.
                // We create LINK-based access: a LINK cell that will be bound at runtime.
                let full_path: Vec<String> =
                    extra_parts.iter().map(|s| s.as_str().to_string()).collect();
                let path_str = full_path.join(".");

                // Check if this is a known source port path within PASSED scope
                if let Some(&port) = self.source_ports.get(&full_path) {
                    return self.alloc_node(IrNodeKind::SourcePort(port), Some(span));
                }

                // Create a LINK read - the LINK will be bound at runtime via PASS
                let link_cell = self.alloc_node(IrNodeKind::LinkCell, Some(span));
                self.alloc_node(IrNodeKind::LinkRead { cell: link_cell }, Some(span))
            }
        }
    }

    fn lower_link(&mut self, span: SimpleSpan) -> NodeId {
        self.alloc_node(IrNodeKind::LinkCell, Some(span))
    }

    fn lower_skip(&mut self, span: SimpleSpan) -> NodeId {
        self.alloc_node(IrNodeKind::Skip, Some(span))
    }

    fn lower_pipe(
        &mut self,
        from: &StaticSpannedExpression,
        to: &StaticSpannedExpression,
        span: SimpleSpan,
    ) -> NodeId {
        // `from |> to` — the piped value becomes the implicit first argument of `to`
        let from_node = self.lower_expr(from);

        // Check if `to` is a function call, field access, or control flow
        match &to.node {
            Expression::FunctionCall { path, arguments } => {
                self.lower_piped_function_call(from_node, path, arguments, span)
            }
            Expression::FieldAccess { path } => {
                self.lower_piped_field_access(from_node, path, span)
            }
            Expression::When { arms } => self.lower_piped_when(from_node, arms, span),
            Expression::While { arms } => self.lower_piped_while(from_node, arms, span),
            Expression::Then { body } => self.lower_piped_then(from_node, body, span),
            Expression::Hold { state_param, body } => {
                self.lower_piped_hold(from_node, state_param, body, span)
            }
            Expression::Latest { inputs } => {
                // `from |> LATEST { inputs... }` — prepend `from` to inputs
                let mut all_inputs: Vec<StaticSpannedExpression> = vec![from.clone()];
                all_inputs.extend(inputs.iter().cloned());
                self.lower_latest(&all_inputs, span)
            }
            Expression::List { items } => {
                // `from |> LIST { items... }` — prepend `from` to items
                let mut all_items = vec![from.clone()];
                all_items.extend(items.iter().cloned());
                self.lower_list_literal(&all_items, span)
            }
            Expression::Comparator(comp) => {
                // `from |> a == b` — replace operand_a with from if it looks like a placeholder
                self.lower_piped_comparator(from_node, comp, span)
            }
            Expression::ArithmeticOperator(op) => self.lower_piped_arithmetic(from_node, op, span),
            _ => {
                // Generic: lower `to` and create a block with both
                let to_node = self.lower_expr(to);
                self.alloc_node(
                    IrNodeKind::Block {
                        inputs: vec![from_node, to_node],
                    },
                    Some(span),
                )
            }
        }
    }

    fn lower_piped_function_call(
        &mut self,
        receiver: NodeId,
        path: &[StrSlice],
        arguments: &Vec<
            boon::parser::static_expression::Spanned<boon::parser::static_expression::Argument>,
        >,
        span: SimpleSpan,
    ) -> NodeId {
        let path_strs: Vec<String> = path.iter().map(|s| s.as_str().to_string()).collect();

        // Check if this is a builtin
        if let Some(builtin_id) = self.builtins.lookup(&path_strs) {
            let builtin = self.builtins.get(builtin_id);
            let mut args: Vec<NodeId> = vec![receiver];
            for arg in arguments {
                if let Some(ref val) = arg.node.value {
                    args.push(self.lower_expr(val));
                }
            }

            return self.lower_builtin_call(builtin, &args, span);
        }

        // Check if it's a user-defined function call via List/map pattern
        // e.g., `list |> List/map(item, new: make_cell(column: column, row: row_number))`
        if path_strs == ["List", "map"] && arguments.len() >= 1 {
            return self.lower_list_map(receiver, arguments, span);
        }

        if path_strs == ["List", "append"] && arguments.len() >= 1 {
            let item_node = if let Some(ref val) = arguments[0].node.value {
                self.lower_expr(val)
            } else {
                receiver
            };
            return self.alloc_node(
                IrNodeKind::ListAppend {
                    list: receiver,
                    item: item_node,
                },
                Some(span),
            );
        }

        if path_strs == ["List", "remove"] && arguments.len() >= 2 {
            let item_name = if let Some(ref val) = arguments[0].node.value {
                if let Expression::Variable(v) = &val.node {
                    Some(v.name.as_str().to_string())
                } else {
                    None
                }
            } else {
                None
            };
            let on_node = if arguments.len() > 1 {
                if let Some(ref val) = arguments[1].node.value {
                    Some(self.lower_expr(val))
                } else {
                    None
                }
            } else {
                None
            };

            if let (Some(_item_name), Some(on_node)) = (item_name, on_node) {
                // Generic list remove with predicate
                return self.alloc_node(
                    IrNodeKind::ListRemove {
                        list: receiver,
                        predicate: on_node,
                    },
                    Some(span),
                );
            }
        }

        if path_strs == ["List", "retain"] && arguments.len() >= 1 {
            if let Some(ref cond_val) = arguments[0].node.value {
                let predicate = self.lower_expr(cond_val);
                return self.alloc_node(
                    IrNodeKind::ListRetain {
                        list: receiver,
                        predicate,
                    },
                    Some(span),
                );
            }
        }

        if path_strs == ["List", "range"] {
            // List/range(from: from_val, to: to_val)
            let mut from_node =
                self.alloc_node(IrNodeKind::Literal(KernelValue::Number(1.0)), Some(span));
            let mut to_node = receiver;
            for arg in arguments {
                if arg.node.name.as_str() == "from" {
                    if let Some(ref val) = arg.node.value {
                        from_node = self.lower_expr(val);
                    }
                }
                if arg.node.name.as_str() == "to" {
                    if let Some(ref val) = arg.node.value {
                        to_node = self.lower_expr(val);
                    }
                }
            }
            return self.alloc_node(
                IrNodeKind::ListRange {
                    from: from_node,
                    to: to_node,
                },
                Some(span),
            );
        }

        if path_strs == ["List", "count"] {
            return self.alloc_node(IrNodeKind::ListCount { list: receiver }, Some(span));
        }

        if path_strs == ["List", "is_empty"] {
            return self.alloc_node(IrNodeKind::ListIsEmpty { list: receiver }, Some(span));
        }

        if path_strs == ["List", "sum"] {
            return self.alloc_node(IrNodeKind::ListSum { list: receiver }, Some(span));
        }

        if path_strs == ["List", "last"] {
            let index = self.alloc_node(
                IrNodeKind::Literal(KernelValue::Number(f64::NAN)),
                Some(span),
            );
            return self.alloc_node(
                IrNodeKind::ListGet {
                    list: receiver,
                    index,
                },
                Some(span),
            );
        }

        self.unsupported(
            span,
            format!("unsupported piped function call: {}", path_strs.join(".")),
        );
        receiver
    }

    fn lower_list_map(
        &mut self,
        list: NodeId,
        arguments: &Vec<
            boon::parser::static_expression::Spanned<boon::parser::static_expression::Argument>,
        >,
        span: SimpleSpan,
    ) -> NodeId {
        // Find the `new:` argument which contains the function call for mapping
        for arg in arguments {
            if arg.node.name.as_str() == "new" {
                if let Some(ref new_val) = arg.node.value {
                    if let Expression::FunctionCall {
                        path,
                        arguments: inner_args,
                    } = &new_val.node
                    {
                        let func_name = path.first().map(|s| s.as_str()).unwrap_or("");
                        let func_id = self.alloc_function_id();
                        let call_site = self.alloc_call_site_id();

                        // Lower the function body with the map parameter in scope
                        // We need to create a function template
                        let param_name =
                            arguments.first().map(|a| a.node.name.as_str().to_string());

                        // Lower each inner argument with the parameter in scope
                        let mut lowered_args = Vec::new();
                        for inner_arg in inner_args {
                            if let Some(ref val) = inner_arg.node.value {
                                lowered_args.push(self.lower_expr(val));
                            }
                        }

                        // Create a function template with the lowered output
                        let output = lowered_args.last().copied().unwrap_or(list);
                        let param_count = if param_name.is_some() { 1 } else { 0 };

                        self.functions_ir.push(IrFunctionTemplate {
                            id: func_id,
                            parameter_count: param_count,
                            output,
                            nodes: Vec::new(), // Nodes are shared in the main program
                        });

                        return self.alloc_node(
                            IrNodeKind::ListMap {
                                list,
                                function: func_id,
                                call_site,
                            },
                            Some(span),
                        );
                    }
                }
            }
        }

        // Fallback: just return the list
        list
    }

    fn lower_builtin_call(
        &mut self,
        builtin: &crate::lower::builtin_registry::BuiltinFn,
        args: &[NodeId],
        span: SimpleSpan,
    ) -> NodeId {
        let path = &builtin.path;
        let path_str = path.join(".");

        // Pre-compute placeholders from args or create new ones
        let a0 = if args.is_empty() {
            self.alloc_node(
                IrNodeKind::Literal(KernelValue::Number(f64::NAN)),
                Some(span),
            )
        } else {
            args[0]
        };
        let a1 = if args.len() < 2 {
            self.alloc_node(
                IrNodeKind::Literal(KernelValue::Number(f64::NAN)),
                Some(span),
            )
        } else {
            args[1]
        };

        match path_str.as_str() {
            "Math.sum" => self.alloc_node(IrNodeKind::MathSum { input: a0 }, Some(span)),
            "Math.min" => self.alloc_node(IrNodeKind::MathMin { lhs: a0, rhs: a1 }, Some(span)),
            "Math.max" => self.alloc_node(IrNodeKind::MathMin { lhs: a0, rhs: a1 }, Some(span)),
            "Math.round" => self.alloc_node(IrNodeKind::MathRound { input: a0 }, Some(span)),
            "Bool.not" => self.alloc_node(IrNodeKind::BoolNot { input: a0 }, Some(span)),
            "Text.trim" => self.alloc_node(IrNodeKind::TextTrim { input: a0 }, Some(span)),
            "Text.is_not_empty" | "Text.is_empty" => a0,
            "Text.to_number" => self.alloc_node(IrNodeKind::TextToNumber { input: a0 }, Some(span)),
            "Text.empty" => self.alloc_node(
                IrNodeKind::Literal(KernelValue::Text(String::new())),
                Some(span),
            ),
            "Text.space" => self.alloc_node(
                IrNodeKind::Literal(KernelValue::Text(" ".to_string())),
                Some(span),
            ),
            "List.count" => self.alloc_node(IrNodeKind::ListCount { list: a0 }, Some(span)),
            "List.is_empty" => self.alloc_node(IrNodeKind::ListIsEmpty { list: a0 }, Some(span)),
            "List.sum" => self.alloc_node(IrNodeKind::ListSum { list: a0 }, Some(span)),
            _ => {
                self.unsupported(span, format!("unsupported builtin: {path_str}"));
                a0
            }
        }
    }

    fn lower_piped_field_access(
        &mut self,
        receiver: NodeId,
        path: &[StrSlice],
        span: SimpleSpan,
    ) -> NodeId {
        // `expr |> .field.subfield` — field access on piped value
        let mut current = receiver;
        for field in path {
            current = self.alloc_node(
                IrNodeKind::FieldRead {
                    object: current,
                    field: field.as_str().to_string(),
                },
                Some(span),
            );
        }
        current
    }

    fn lower_piped_when(
        &mut self,
        source: NodeId,
        arms: &[boon::parser::static_expression::Arm],
        span: SimpleSpan,
    ) -> NodeId {
        let ir_arms = arms
            .iter()
            .filter_map(|arm| {
                let matcher = self.lower_pattern(&arm.pattern)?;
                let result = self.lower_expr(&arm.body);
                Some(MatchArm { matcher, result })
            })
            .collect();

        // Find a wildcard fallback
        let fallback = self.find_wildcard_fallback(arms, span);

        self.alloc_node(
            IrNodeKind::When {
                source,
                arms: ir_arms,
                fallback,
            },
            Some(span),
        )
    }

    fn lower_piped_while(
        &mut self,
        source: NodeId,
        arms: &[boon::parser::static_expression::Arm],
        span: SimpleSpan,
    ) -> NodeId {
        let ir_arms = arms
            .iter()
            .filter_map(|arm| {
                let matcher = self.lower_pattern(&arm.pattern)?;
                let result = self.lower_expr(&arm.body);
                Some(MatchArm { matcher, result })
            })
            .collect();

        let fallback = self.find_wildcard_fallback(arms, span);

        self.alloc_node(
            IrNodeKind::While {
                source,
                arms: ir_arms,
                fallback,
            },
            Some(span),
        )
    }

    fn lower_piped_then(
        &mut self,
        source: NodeId,
        body: &StaticSpannedExpression,
        span: SimpleSpan,
    ) -> NodeId {
        let body_node = self.lower_expr(body);
        self.alloc_node(
            IrNodeKind::Then {
                source,
                body: body_node,
            },
            Some(span),
        )
    }

    fn lower_piped_hold(
        &mut self,
        seed: NodeId,
        state_param: &StrSlice,
        body: &StaticSpannedExpression,
        span: SimpleSpan,
    ) -> NodeId {
        // Push the state param into scope
        let param_name = state_param.as_str().to_string();
        let old = self.var_nodes.get(&param_name).copied();

        // Create a placeholder node for the state param (will be resolved at runtime)
        let state_param_node = self.alloc_node(IrNodeKind::Parameter { index: 0 }, Some(span));
        self.var_nodes.insert(param_name.clone(), state_param_node);

        let updates_node = self.lower_expr(body);

        // Restore old binding if any
        if let Some(old_node) = old {
            self.var_nodes.insert(param_name, old_node);
        } else {
            self.var_nodes.remove(&param_name);
        }

        self.alloc_node(
            IrNodeKind::Hold {
                seed,
                updates: updates_node,
            },
            Some(span),
        )
    }

    fn lower_latest(&mut self, inputs: &Vec<StaticSpannedExpression>, span: SimpleSpan) -> NodeId {
        let input_nodes: Vec<NodeId> = inputs.iter().map(|e| self.lower_expr(e)).collect();
        self.alloc_node(
            IrNodeKind::Latest {
                inputs: input_nodes,
            },
            Some(span),
        )
    }

    fn lower_block(
        &mut self,
        variables: &Vec<
            boon::parser::static_expression::Spanned<boon::parser::static_expression::Variable>,
        >,
        output: &StaticSpannedExpression,
        span: SimpleSpan,
    ) -> NodeId {
        // Lower block variables into scope first
        let mut input_nodes = Vec::new();
        for var in variables {
            let node_id = self.lower_expr(&var.node.value);
            self.var_nodes
                .insert(var.node.name.as_str().to_string(), node_id);
            input_nodes.push(node_id);
        }

        let output_node = self.lower_expr(output);

        self.alloc_node(
            IrNodeKind::Block {
                inputs: input_nodes,
            },
            Some(span),
        )
    }

    fn lower_comparator(&mut self, comp: &Comparator, span: SimpleSpan) -> NodeId {
        match comp {
            Comparator::Equal {
                operand_a,
                operand_b,
            } => {
                let lhs = self.lower_expr(operand_a);
                let rhs = self.lower_expr(operand_b);
                self.alloc_node(IrNodeKind::Eq { lhs, rhs }, Some(span))
            }
            Comparator::Greater {
                operand_a,
                operand_b,
            } => {
                let lhs = self.lower_expr(operand_a);
                let rhs = self.lower_expr(operand_b);
                self.alloc_node(IrNodeKind::Ge { lhs, rhs }, Some(span))
            }
            Comparator::GreaterOrEqual {
                operand_a,
                operand_b,
            } => {
                let lhs = self.lower_expr(operand_a);
                let rhs = self.lower_expr(operand_b);
                self.alloc_node(IrNodeKind::Ge { lhs, rhs }, Some(span))
            }
            _ => {
                self.unsupported(span, format!("unsupported comparator: {comp:?}"));
                self.alloc_node(
                    IrNodeKind::Literal(KernelValue::Tag("False".to_string())),
                    Some(span),
                )
            }
        }
    }

    fn lower_arithmetic(&mut self, op: &ArithmeticOperator, span: SimpleSpan) -> NodeId {
        match op {
            ArithmeticOperator::Add {
                operand_a,
                operand_b,
            } => {
                let lhs = self.lower_expr(operand_a);
                let rhs = self.lower_expr(operand_b);
                self.alloc_node(IrNodeKind::Add { lhs, rhs }, Some(span))
            }
            ArithmeticOperator::Subtract {
                operand_a,
                operand_b,
            } => {
                let lhs = self.lower_expr(operand_a);
                let rhs = self.lower_expr(operand_b);
                self.alloc_node(IrNodeKind::Sub { lhs, rhs }, Some(span))
            }
            ArithmeticOperator::Multiply {
                operand_a,
                operand_b,
            } => {
                let lhs = self.lower_expr(operand_a);
                let rhs = self.lower_expr(operand_b);
                self.alloc_node(IrNodeKind::Mul { lhs, rhs }, Some(span))
            }
            ArithmeticOperator::Divide {
                operand_a,
                operand_b,
            } => {
                let lhs = self.lower_expr(operand_a);
                let rhs = self.lower_expr(operand_b);
                self.alloc_node(IrNodeKind::Div { lhs, rhs }, Some(span))
            }
            ArithmeticOperator::Negate { operand } => {
                let input = self.lower_expr(operand);
                let zero =
                    self.alloc_node(IrNodeKind::Literal(KernelValue::Number(0.0)), Some(span));
                self.alloc_node(
                    IrNodeKind::Sub {
                        lhs: zero,
                        rhs: input,
                    },
                    Some(span),
                )
            }
        }
    }

    fn lower_piped_comparator(
        &mut self,
        receiver: NodeId,
        comp: &Comparator,
        span: SimpleSpan,
    ) -> NodeId {
        // Replace the first operand with the receiver
        match comp {
            Comparator::Equal {
                operand_a,
                operand_b,
            } => {
                let rhs = self.lower_expr(operand_b);
                self.alloc_node(IrNodeKind::Eq { lhs: receiver, rhs }, Some(span))
            }
            Comparator::Greater {
                operand_a,
                operand_b,
            } => {
                let rhs = self.lower_expr(operand_b);
                self.alloc_node(IrNodeKind::Ge { lhs: receiver, rhs }, Some(span))
            }
            _ => {
                self.unsupported(span, format!("unsupported piped comparator: {comp:?}"));
                receiver
            }
        }
    }

    fn lower_piped_arithmetic(
        &mut self,
        receiver: NodeId,
        op: &ArithmeticOperator,
        span: SimpleSpan,
    ) -> NodeId {
        match op {
            ArithmeticOperator::Add {
                operand_a: _,
                operand_b,
            } => {
                let rhs = self.lower_expr(operand_b);
                self.alloc_node(IrNodeKind::Add { lhs: receiver, rhs }, Some(span))
            }
            ArithmeticOperator::Subtract {
                operand_a: _,
                operand_b,
            } => {
                let rhs = self.lower_expr(operand_b);
                self.alloc_node(IrNodeKind::Sub { lhs: receiver, rhs }, Some(span))
            }
            ArithmeticOperator::Multiply {
                operand_a: _,
                operand_b,
            } => {
                let rhs = self.lower_expr(operand_b);
                self.alloc_node(IrNodeKind::Mul { lhs: receiver, rhs }, Some(span))
            }
            ArithmeticOperator::Divide {
                operand_a: _,
                operand_b,
            } => {
                let rhs = self.lower_expr(operand_b);
                self.alloc_node(IrNodeKind::Div { lhs: receiver, rhs }, Some(span))
            }
            _ => {
                self.unsupported(span, format!("unsupported piped arithmetic: {op:?}"));
                receiver
            }
        }
    }

    fn lower_list_literal(
        &mut self,
        items: &[StaticSpannedExpression],
        span: SimpleSpan,
    ) -> NodeId {
        let item_nodes: Vec<NodeId> = items.iter().map(|e| self.lower_expr(e)).collect();
        self.alloc_node(IrNodeKind::ListLiteral { items: item_nodes }, Some(span))
    }

    fn lower_object(
        &mut self,
        obj: &boon::parser::static_expression::Object,
        span: SimpleSpan,
    ) -> NodeId {
        let mut fields = Vec::new();
        for var in &obj.variables {
            let value_node = self.lower_expr(&var.node.value);
            fields.push((var.node.name.as_str().to_string(), value_node));
        }
        self.alloc_node(IrNodeKind::ObjectLiteral { fields }, Some(span))
    }

    fn lower_text_literal(&mut self, parts: &[TextPart], span: SimpleSpan) -> NodeId {
        let mut text = String::new();
        let mut needs_interp = false;
        for part in parts {
            match part {
                TextPart::Text(s) => text.push_str(s.as_str()),
                TextPart::Interpolation { .. } => needs_interp = true,
            }
        }

        if !needs_interp && parts.iter().all(|p| matches!(p, TextPart::Text(_))) {
            return self.alloc_node(IrNodeKind::Literal(KernelValue::Text(text)), Some(span));
        }

        // For interpolated text, create a TextJoin node
        let mut input_nodes = Vec::new();
        for part in parts {
            match part {
                TextPart::Text(s) => {
                    input_nodes.push(self.alloc_node(
                        IrNodeKind::Literal(KernelValue::Text(s.as_str().to_string())),
                        Some(span),
                    ));
                }
                TextPart::Interpolation { var, .. } => {
                    let name = var.as_str().to_string();
                    if let Some(&node_id) = self.var_nodes.get(&name) {
                        input_nodes.push(node_id);
                    } else if let Some(binding) = self.bindings.get(name.as_str()) {
                        input_nodes.push(self.lower_expr(binding));
                    } else {
                        self.unsupported(span, format!("unresolved interpolation var: {name}"));
                        input_nodes.push(self.alloc_node(
                            IrNodeKind::Literal(KernelValue::Text(String::new())),
                            Some(span),
                        ));
                    }
                }
            }
        }

        if input_nodes.len() == 1 {
            input_nodes.remove(0)
        } else {
            self.alloc_node(
                IrNodeKind::TextJoin {
                    inputs: input_nodes,
                },
                Some(span),
            )
        }
    }

    fn lower_tagged_object(
        &mut self,
        tag: &StrSlice,
        object: &boon::parser::static_expression::Object,
        span: SimpleSpan,
    ) -> NodeId {
        let obj_node = self.lower_object(object, span);
        // Tagged objects become object literals with a special __tag field
        // For now, just return the object node
        obj_node
    }

    fn lower_field_access(&mut self, path: &[StrSlice], span: SimpleSpan) -> NodeId {
        if path.is_empty() {
            self.unsupported(span, "empty field access path".to_string());
            return self.alloc_node(
                IrNodeKind::Literal(KernelValue::Number(f64::NAN)),
                Some(span),
            );
        }

        let name = path[0].as_str().to_string();
        let mut current = if let Some(binding) = self.bindings.get(name.as_str()) {
            self.lower_expr(binding)
        } else if let Some(&node_id) = self.var_nodes.get(&name) {
            node_id
        } else {
            self.unsupported(span, format!("unresolved field access base: {name}"));
            return self.alloc_node(
                IrNodeKind::Literal(KernelValue::Number(f64::NAN)),
                Some(span),
            );
        };

        for field in &path[1..] {
            current = self.alloc_node(
                IrNodeKind::FieldRead {
                    object: current,
                    field: field.as_str().to_string(),
                },
                Some(span),
            );
        }

        current
    }

    fn lower_postfix_field_access(
        &mut self,
        inner: &StaticSpannedExpression,
        field: &StrSlice,
        span: SimpleSpan,
    ) -> NodeId {
        let obj_node = self.lower_expr(inner);
        self.alloc_node(
            IrNodeKind::FieldRead {
                object: obj_node,
                field: field.as_str().to_string(),
            },
            Some(span),
        )
    }

    fn lower_flush(&mut self, value: &StaticSpannedExpression, span: SimpleSpan) -> NodeId {
        self.lower_expr(value) // FLUSH is just passthrough semantically
    }

    fn lower_spread(&mut self, value: &StaticSpannedExpression, span: SimpleSpan) -> NodeId {
        self.lower_expr(value)
    }

    fn lower_map(
        &mut self,
        entries: &[boon::parser::static_expression::MapEntry],
        span: SimpleSpan,
    ) -> NodeId {
        // Maps are not yet fully supported in generic lowering
        self.unsupported(
            span,
            "Map expressions not yet supported in generic lowering".to_string(),
        );
        self.alloc_node(
            IrNodeKind::Literal(KernelValue::Number(f64::NAN)),
            Some(span),
        )
    }

    fn lower_function_def(
        &mut self,
        name: &StrSlice,
        parameters: &Vec<boon::parser::static_expression::Spanned<StrSlice>>,
        body: &StaticSpannedExpression,
        span: SimpleSpan,
    ) -> NodeId {
        // Register the function in the functions map for later lowering
        // For now, just return a placeholder
        self.unsupported(
            span,
            format!(
                "FUNCTION definition at expression level not supported in generic lowering: {}",
                name.as_str()
            ),
        );
        self.alloc_node(
            IrNodeKind::Literal(KernelValue::Number(f64::NAN)),
            Some(span),
        )
    }

    fn lower_link_setter(
        &mut self,
        alias: &boon::parser::static_expression::Spanned<boon::parser::static_expression::Alias>,
        span: SimpleSpan,
    ) -> NodeId {
        self.unsupported(
            span,
            "LinkSetter (LINK {{ ... }}) not supported in semantic lowering; handled in view lowering".to_string(),
        );
        self.alloc_node(
            IrNodeKind::Literal(KernelValue::Number(f64::NAN)),
            Some(span),
        )
    }

    fn lower_pattern(&mut self, pattern: &Pattern) -> Option<KernelValue> {
        match pattern {
            Pattern::Literal(lit) => Some(match lit {
                Literal::Number(n) => KernelValue::Number(*n),
                Literal::Tag(tag) => KernelValue::Tag(tag.as_str().to_string()),
                Literal::Text(text) => KernelValue::Text(text.as_str().to_string()),
            }),
            Pattern::WildCard => Some(KernelValue::Number(f64::NEG_INFINITY)), // Sentinel for wildcard
            Pattern::ValueComparison { path, .. } => {
                // Pattern comparisons are handled specially; return a placeholder
                Some(KernelValue::Number(f64::NAN))
            }
            _ => None, // Complex patterns not yet supported
        }
    }

    fn find_wildcard_fallback(
        &mut self,
        arms: &[boon::parser::static_expression::Arm],
        span: SimpleSpan,
    ) -> NodeId {
        for arm in arms {
            if matches!(arm.pattern, Pattern::WildCard) {
                return self.lower_expr(&arm.body);
            }
        }
        // No wildcard — use a default Skip
        self.alloc_node(IrNodeKind::Skip, Some(span))
    }

    fn lower_hold(
        &mut self,
        state_param: &StrSlice,
        body: &StaticSpannedExpression,
        span: SimpleSpan,
    ) -> NodeId {
        // HOLD without a seed is unusual — normally it's `seed |> HOLD state { body }`.
        // If encountered standalone, create a placeholder seed and delegate to lower_piped_hold.
        let seed = self.alloc_node(
            IrNodeKind::Literal(KernelValue::Number(f64::NAN)),
            Some(span),
        );
        self.lower_piped_hold(seed, state_param, body, span)
    }

    fn lower_when(
        &mut self,
        arms: &[boon::parser::static_expression::Arm],
        span: SimpleSpan,
    ) -> NodeId {
        self.unsupported(
            span,
            "WHEN without a piped source is not supported".to_string(),
        );
        self.alloc_node(IrNodeKind::Skip, Some(span))
    }

    fn lower_while(
        &mut self,
        arms: &[boon::parser::static_expression::Arm],
        span: SimpleSpan,
    ) -> NodeId {
        self.unsupported(
            span,
            "WHILE without a piped source is not supported".to_string(),
        );
        self.alloc_node(IrNodeKind::Skip, Some(span))
    }

    fn lower_then(&mut self, body: &StaticSpannedExpression, span: SimpleSpan) -> NodeId {
        self.unsupported(
            span,
            "THEN without a piped source is not supported".to_string(),
        );
        self.lower_expr(body)
    }

    fn lower_function_call(
        &mut self,
        path: &[StrSlice],
        arguments: &Vec<
            boon::parser::static_expression::Spanned<boon::parser::static_expression::Argument>,
        >,
        span: SimpleSpan,
    ) -> NodeId {
        let path_strs: Vec<String> = path.iter().map(|s| s.as_str().to_string()).collect();

        if let Some(builtin_id) = self.builtins.lookup(&path_strs) {
            let builtin = self.builtins.get(builtin_id);
            let args: Vec<NodeId> = arguments
                .iter()
                .filter_map(|arg| arg.node.value.as_ref().map(|v| self.lower_expr(v)))
                .collect();
            return self.lower_builtin_call(builtin, &args, span);
        }

        self.unsupported(
            span,
            format!("unsupported function call: {}", path_strs.join(".")),
        );
        self.alloc_node(
            IrNodeKind::Literal(KernelValue::Number(f64::NAN)),
            Some(span),
        )
    }

    /// Finalize lowering and produce the complete `IrProgram`.
    pub fn finalize(self) -> IrProgram {
        IrProgram {
            nodes: self.nodes,
            functions: self.functions_ir,
            persistence: self.persistence,
        }
    }
}

/// Generic semantic lowering entry point.
///
/// Takes parsed expressions with resolved references and persistence,
/// and produces an `IrProgram` plus any lowering diagnostics.
pub fn generic_lower_semantic(
    bindings: &BTreeMap<String, &StaticSpannedExpression>,
    functions: &BTreeMap<
        String,
        (
            Vec<boon::parser::static_expression::Spanned<StrSlice>>,
            StaticSpannedExpression,
        ),
    >,
    builtins: &BuiltinRegistry,
) -> (IrProgram, Vec<LowerDiagnostic>) {
    let mut ctx = GenericSemanticLowerCtx::new(bindings, functions, builtins);

    // Lower all top-level bindings
    for (_name, expr) in bindings {
        ctx.lower_expr(expr);
    }

    let diagnostics = std::mem::take(&mut ctx.diagnostics);
    let program = ctx.finalize();
    (program, diagnostics)
}
