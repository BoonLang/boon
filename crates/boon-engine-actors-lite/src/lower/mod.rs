//! Generic lowering modules for ActorsLite.
//!
//! This module provides the canonical generic lowering pipeline:
//! - `lower_program_generic`: semantic + view lowering for any Boon source
//! - `lower_view_generic`: view-only lowering for preview
//!
//! The legacy example-specific lowerers are re-exported from `lower_legacy`
//! and remain available until the generic pipeline covers the same surface.
//! Per the strict unified-engine plan, no new example-specific lowerers
//! should be added.

// Re-export the entire legacy lowering module for backward compatibility
pub use super::lower_legacy::*;

pub mod builtin_registry;
pub mod generic_semantic;

use crate::bridge::{
    HostButtonLabel, HostElementEventBinding, HostStripeDirection, HostTemplatedTextPart,
    HostViewIr, HostViewKind, HostViewNode, HostWidth,
};
use crate::ir::{FunctionInstanceId, SinkPortId, SourcePortId, ViewSiteId};
use crate::lower::builtin_registry::BuiltinRegistry;
use crate::parse::{
    StaticExpression, StaticSpannedExpression, parse_static_expressions, top_level_bindings,
};
use boon::parser::StrSlice;
use boon::parser::static_expression::{Argument, Expression, Literal, Spanned, TextPart, Variable};
use boon_scene::UiEventKind;
use std::collections::BTreeMap;

use self::generic_semantic::LowerDiagnostic;

/// Re-export the generic lowering diagnostic type.
pub use self::generic_semantic::LowerDiagnostic as GenericLowerDiagnostic;

/// Result of generic program lowering.
pub struct GenericLoweredProgram {
    pub ir: crate::ir::IrProgram,
    pub host_view: HostViewIr,
    pub diagnostics: Vec<LowerDiagnostic>,
}

/// Counter for allocating port IDs during view lowering.
#[derive(Default)]
struct PortAllocator {
    next_source: u32,
    next_sink: u32,
}

impl PortAllocator {
    fn alloc_source(&mut self) -> SourcePortId {
        let id = SourcePortId(self.next_source);
        self.next_source += 1;
        id
    }

    fn alloc_sink(&mut self) -> SinkPortId {
        let id = SinkPortId(self.next_sink);
        self.next_sink += 1;
        id
    }
}

/// View site allocator for generating stable retained node keys.
struct ViewSiteCounter {
    next_site: u32,
    function_instance: FunctionInstanceId,
}

impl ViewSiteCounter {
    fn new() -> Self {
        Self {
            next_site: 1,
            function_instance: FunctionInstanceId(1),
        }
    }

    fn next_key(&mut self) -> crate::ir::RetainedNodeKey {
        use crate::ir::RetainedNodeKey;
        let key = RetainedNodeKey {
            view_site: ViewSiteId(self.next_site),
            function_instance: Some(self.function_instance),
            mapped_item_identity: None,
        };
        self.next_site += 1;
        key
    }
}

/// Generic program lowering entry point.
///
/// Takes raw Boon source, parses it, resolves references and persistence,
/// and lowers through the generic semantic + view pipeline.
pub fn lower_program_generic(source: &str) -> Result<GenericLoweredProgram, String> {
    let expressions = parse_static_expressions(source)?;

    // Extract top-level bindings and functions
    let bindings = top_level_bindings(&expressions);
    let (_func_names, func_defs) = extract_top_level_functions(&expressions);

    // Create builtin registry
    let builtins = BuiltinRegistry::new();

    // Lower semantic IR
    let (ir, diagnostics) =
        generic_semantic::generic_lower_semantic(&bindings, &func_defs, &builtins);

    // Lower view IR
    let host_view = lower_view_generic(&expressions, &bindings)?;

    Ok(GenericLoweredProgram {
        ir,
        host_view,
        diagnostics,
    })
}

/// Extract top-level function definitions from parsed expressions.
pub(crate) fn extract_top_level_functions(
    expressions: &[StaticSpannedExpression],
) -> (
    Vec<String>,
    BTreeMap<String, (Vec<Spanned<StrSlice>>, StaticSpannedExpression)>,
) {
    let mut names = Vec::new();
    let mut defs = BTreeMap::new();

    for expr in expressions {
        if let Expression::Variable(var) = &expr.node {
            if let Expression::Function {
                name,
                parameters,
                body,
            } = &var.value.node
            {
                let func_name = name.as_str().to_string();
                names.push(func_name.clone());
                defs.insert(func_name, (parameters.clone(), body.as_ref().clone()));
            }
        }
    }

    (names, defs)
}

/// Generic view lowering entry point.
///
/// Takes parsed expressions and produces a `HostViewIr` by walking the
/// `document` top-level binding.
pub fn lower_view_generic(
    expressions: &[StaticSpannedExpression],
    bindings: &BTreeMap<String, &StaticSpannedExpression>,
) -> Result<HostViewIr, String> {
    let doc_expr = bindings
        .get("document")
        .ok_or_else(|| "generic view lowering: no 'document' binding found".to_string())?;

    let mut port_alloc = PortAllocator::default();
    let mut site_alloc = ViewSiteCounter::new();

    let root = lower_view_expr(doc_expr, bindings, &mut port_alloc, &mut site_alloc)?;

    Ok(HostViewIr { root: Some(root) })
}

fn lower_view_expr(
    expr: &StaticSpannedExpression,
    bindings: &BTreeMap<String, &StaticSpannedExpression>,
    port_alloc: &mut PortAllocator,
    site_alloc: &mut ViewSiteCounter,
) -> Result<HostViewNode, String> {
    match &expr.node {
        Expression::Variable(var) => {
            let name = var.name.as_str();
            if let Some(binding) = bindings.get(name) {
                lower_view_expr(binding, bindings, port_alloc, site_alloc)
            } else {
                Err(format!("unresolved binding in view: {name}"))
            }
        }
        Expression::Alias(alias) => {
            // After reference resolution, simple names become Aliases.
            // Extract the name from the alias path and look up the binding.
            if let boon::parser::static_expression::Alias::WithoutPassed { parts, .. } = alias {
                if let Some(first) = parts.first() {
                    let name = first.as_str();
                    if let Some(binding) = bindings.get(name) {
                        return lower_view_expr(binding, bindings, port_alloc, site_alloc);
                    }
                }
            }
            Err("unresolved alias in view".to_string())
        }
        Expression::FunctionCall { path, arguments } => {
            lower_view_function_call(path, arguments, bindings, port_alloc, site_alloc)
        }
        Expression::Pipe { to, .. } => lower_view_expr(to, bindings, port_alloc, site_alloc),
        _ => Err(format!("unsupported expression type in view lowering")),
    }
}

fn lower_view_function_call(
    path: &Vec<StrSlice>,
    arguments: &Vec<Spanned<Argument>>,
    bindings: &BTreeMap<String, &StaticSpannedExpression>,
    port_alloc: &mut PortAllocator,
    site_alloc: &mut ViewSiteCounter,
) -> Result<HostViewNode, String> {
    let path_strs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();

    match path_strs.as_slice() {
        ["Document", "new"] => lower_document(arguments, bindings, port_alloc, site_alloc),
        ["Element", "stripe"] => lower_stripe(arguments, bindings, port_alloc, site_alloc),
        ["Element", "button"] => lower_button(arguments, bindings, port_alloc, site_alloc),
        ["Element", "label"] => lower_element_label(arguments, bindings, port_alloc, site_alloc),
        ["Element", "container"] => lower_container(arguments, bindings, port_alloc, site_alloc),
        ["Element", "checkbox"] => lower_checkbox(arguments, bindings, port_alloc, site_alloc),
        ["Element", "text_input"] => lower_text_input(arguments, bindings, port_alloc, site_alloc),
        ["Element", "paragraph"] => lower_paragraph(arguments, bindings, port_alloc, site_alloc),
        ["Element", "link"] => lower_link(arguments, bindings, port_alloc, site_alloc),
        _ => {
            let func_name = path_strs.first().copied().unwrap_or("");
            if let Some(binding) = bindings.get(func_name) {
                lower_view_expr(binding, bindings, port_alloc, site_alloc)
            } else {
                Err(format!("unknown view function: {}", path_strs.join("/")))
            }
        }
    }
}

fn lower_document(
    arguments: &Vec<Spanned<Argument>>,
    bindings: &BTreeMap<String, &StaticSpannedExpression>,
    port_alloc: &mut PortAllocator,
    site_alloc: &mut ViewSiteCounter,
) -> Result<HostViewNode, String> {
    for arg in arguments {
        if arg.node.name.as_str() == "root" {
            if let Some(ref val) = arg.node.value {
                let child = lower_view_expr(val, bindings, port_alloc, site_alloc)?;
                let key = site_alloc.next_key();
                return Ok(HostViewNode {
                    retained_key: key,
                    kind: HostViewKind::Document,
                    children: vec![child],
                });
            }
        }
    }
    let key = site_alloc.next_key();
    Ok(HostViewNode {
        retained_key: key,
        kind: HostViewKind::Document,
        children: vec![],
    })
}

fn lower_stripe(
    arguments: &Vec<Spanned<Argument>>,
    bindings: &BTreeMap<String, &StaticSpannedExpression>,
    port_alloc: &mut PortAllocator,
    site_alloc: &mut ViewSiteCounter,
) -> Result<HostViewNode, String> {
    let mut children = Vec::new();
    let mut direction = HostStripeDirection::Column;

    for arg in arguments {
        match arg.node.name.as_str() {
            "direction" => {
                if let Some(ref val) = arg.node.value {
                    if let Expression::Variable(v) = &val.node {
                        direction = if v.name.as_str() == "Row" {
                            HostStripeDirection::Row
                        } else {
                            HostStripeDirection::Column
                        };
                    }
                }
            }
            "items" => {
                if let Some(ref val) = arg.node.value {
                    if let Expression::List { items } = &val.node {
                        for item in items {
                            match lower_view_expr(item, bindings, port_alloc, site_alloc) {
                                Ok(child) => children.push(child),
                                Err(_) => {}
                            }
                        }
                    } else if let Expression::Variable(v) = &val.node {
                        if let Some(list_binding) = bindings.get(v.name.as_str()) {
                            if let Expression::List { items } = &list_binding.node {
                                for item in items {
                                    match lower_view_expr(item, bindings, port_alloc, site_alloc) {
                                        Ok(child) => children.push(child),
                                        Err(_) => {}
                                    }
                                }
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    let key = site_alloc.next_key();
    Ok(HostViewNode {
        retained_key: key,
        kind: HostViewKind::StripeLayout {
            direction,
            gap_px: 0,
            padding_px: None,
            width: None,
            align_cross: None,
        },
        children,
    })
}

fn lower_button(
    arguments: &Vec<Spanned<Argument>>,
    bindings: &BTreeMap<String, &StaticSpannedExpression>,
    port_alloc: &mut PortAllocator,
    site_alloc: &mut ViewSiteCounter,
) -> Result<HostViewNode, String> {
    let mut press_port = port_alloc.alloc_source();
    let mut label_text = "Button".to_string();

    for arg in arguments {
        match arg.node.name.as_str() {
            "element" => {
                if let Some(ref val) = arg.node.value {
                    if let Expression::Object(obj) = &val.node {
                        press_port = extract_press_port(&obj.variables, port_alloc);
                    }
                }
            }
            "label" => {
                if let Some(ref val) = arg.node.value {
                    label_text =
                        extract_text(val, bindings).unwrap_or_else(|| "Button".to_string());
                }
            }
            _ => {}
        }
    }

    let key = site_alloc.next_key();
    Ok(HostViewNode {
        retained_key: key,
        kind: HostViewKind::Button {
            label: HostButtonLabel::Static(label_text),
            press_port,
            disabled_sink: None,
        },
        children: vec![],
    })
}

fn lower_element_label(
    arguments: &Vec<Spanned<Argument>>,
    _bindings: &BTreeMap<String, &StaticSpannedExpression>,
    port_alloc: &mut PortAllocator,
    site_alloc: &mut ViewSiteCounter,
) -> Result<HostViewNode, String> {
    let sink = port_alloc.alloc_sink();
    let key = site_alloc.next_key();
    Ok(HostViewNode {
        retained_key: key,
        kind: HostViewKind::Label { sink },
        children: vec![],
    })
}

fn lower_container(
    arguments: &Vec<Spanned<Argument>>,
    bindings: &BTreeMap<String, &StaticSpannedExpression>,
    port_alloc: &mut PortAllocator,
    site_alloc: &mut ViewSiteCounter,
) -> Result<HostViewNode, String> {
    let mut children = Vec::new();

    for arg in arguments {
        if arg.node.name.as_str() == "child" {
            if let Some(ref val) = arg.node.value {
                if let Ok(child) = lower_view_expr(val, bindings, port_alloc, site_alloc) {
                    children.push(child);
                }
            }
        }
    }

    let key = site_alloc.next_key();
    Ok(HostViewNode {
        retained_key: key,
        kind: HostViewKind::Container { center_row: false },
        children,
    })
}

fn lower_checkbox(
    arguments: &Vec<Spanned<Argument>>,
    _bindings: &BTreeMap<String, &StaticSpannedExpression>,
    port_alloc: &mut PortAllocator,
    site_alloc: &mut ViewSiteCounter,
) -> Result<HostViewNode, String> {
    let click_port = port_alloc.alloc_source();
    let checked_sink = port_alloc.alloc_sink();
    let key = site_alloc.next_key();
    Ok(HostViewNode {
        retained_key: key,
        kind: HostViewKind::Checkbox {
            checked_sink,
            click_port,
        },
        children: vec![],
    })
}

fn lower_text_input(
    arguments: &Vec<Spanned<Argument>>,
    bindings: &BTreeMap<String, &StaticSpannedExpression>,
    port_alloc: &mut PortAllocator,
    site_alloc: &mut ViewSiteCounter,
) -> Result<HostViewNode, String> {
    let mut placeholder = String::new();
    let mut focus_on_mount = false;
    let change_port = port_alloc.alloc_source();
    let key_down_port = port_alloc.alloc_source();

    for arg in arguments {
        if arg.node.name.as_str() == "placeholder" {
            if let Some(ref val) = arg.node.value {
                placeholder = extract_text(val, bindings).unwrap_or_default();
            }
        }
        if arg.node.name.as_str() == "focus" {
            if let Some(ref val) = arg.node.value {
                if let Expression::Literal(boon::parser::static_expression::Literal::Tag(tag))
                | Expression::Literal(boon::parser::static_expression::Literal::Text(tag)) =
                    &val.node
                {
                    focus_on_mount = tag.as_str() == "True";
                }
            }
        }
    }

    let value_sink = port_alloc.alloc_sink();
    let key = site_alloc.next_key();
    Ok(HostViewNode {
        retained_key: key,
        kind: HostViewKind::TextInput {
            value_sink,
            placeholder,
            change_port,
            key_down_port,
            blur_port: None,
            focus_port: None,
            focus_on_mount,
            disabled_sink: None,
        },
        children: vec![],
    })
}

fn lower_paragraph(
    arguments: &Vec<Spanned<Argument>>,
    bindings: &BTreeMap<String, &StaticSpannedExpression>,
    _port_alloc: &mut PortAllocator,
    site_alloc: &mut ViewSiteCounter,
) -> Result<HostViewNode, String> {
    let mut children = Vec::new();

    for arg in arguments {
        if arg.node.name.as_str() == "contents" {
            if let Some(ref val) = arg.node.value {
                if let Expression::List { items } = &val.node {
                    for item in items {
                        let text = extract_text(item, bindings).unwrap_or_default();
                        if !text.is_empty() {
                            let key = site_alloc.next_key();
                            children.push(HostViewNode {
                                retained_key: key,
                                kind: HostViewKind::StaticLabel { text },
                                children: vec![],
                            });
                        }
                    }
                }
            }
        }
    }

    let key = site_alloc.next_key();
    Ok(HostViewNode {
        retained_key: key,
        kind: HostViewKind::Paragraph,
        children,
    })
}

fn lower_link(
    arguments: &Vec<Spanned<Argument>>,
    bindings: &BTreeMap<String, &StaticSpannedExpression>,
    _port_alloc: &mut PortAllocator,
    site_alloc: &mut ViewSiteCounter,
) -> Result<HostViewNode, String> {
    let mut href = String::new();
    let mut label = String::new();

    for arg in arguments {
        match arg.node.name.as_str() {
            "to" => {
                if let Some(ref val) = arg.node.value {
                    href = extract_text(val, bindings).unwrap_or_default();
                }
            }
            "label" => {
                if let Some(ref val) = arg.node.value {
                    label = extract_text(val, bindings).unwrap_or_default();
                }
            }
            _ => {}
        }
    }

    let key = site_alloc.next_key();
    let children = if label.is_empty() {
        vec![]
    } else {
        vec![HostViewNode {
            retained_key: site_alloc.next_key(),
            kind: HostViewKind::StaticLabel { text: label },
            children: vec![],
        }]
    };
    Ok(HostViewNode {
        retained_key: key,
        kind: HostViewKind::Link {
            href,
            new_tab: false,
        },
        children,
    })
}

/// Extract press port from event bindings in an object.
fn extract_press_port(
    variables: &[Spanned<Variable>],
    port_alloc: &mut PortAllocator,
) -> SourcePortId {
    for var in variables {
        if var.node.name.as_str() == "event" {
            if let Expression::Object(event_obj) = &var.node.value.node {
                for event_var in &event_obj.variables {
                    if event_var.node.name.as_str() == "press"
                        && matches!(event_var.node.value.node, Expression::Link)
                    {
                        return port_alloc.alloc_source();
                    }
                }
            }
        }
    }
    port_alloc.alloc_source()
}

/// Extract text from an expression.
fn extract_text(
    expr: &StaticSpannedExpression,
    bindings: &BTreeMap<String, &StaticSpannedExpression>,
) -> Option<String> {
    match &expr.node {
        Expression::TextLiteral { parts, .. } => {
            let mut text = String::new();
            for part in parts {
                match part {
                    TextPart::Text(s) => text.push_str(s.as_str()),
                    TextPart::Interpolation { var, .. } => {
                        if let Some(binding) = bindings.get(var.as_str()) {
                            if let Some(t) = extract_text(binding, bindings) {
                                text.push_str(&t);
                            }
                        }
                    }
                }
            }
            if text.is_empty() { None } else { Some(text) }
        }
        Expression::Literal(Literal::Text(s)) => Some(s.as_str().to_string()),
        Expression::Literal(Literal::Tag(s)) => Some(s.as_str().to_string()),
        Expression::Literal(Literal::Number(n)) => Some(n.to_string()),
        Expression::Variable(v) => {
            if let Some(binding) = bindings.get(v.name.as_str()) {
                extract_text(binding, bindings)
            } else {
                None
            }
        }
        _ => None,
    }
}
