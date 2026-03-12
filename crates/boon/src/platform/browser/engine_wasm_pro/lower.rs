use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, btree_map::Entry};

use crate::parser::{
    Input as _, Parser as _, SourceCode, Token, lexer, parser, reset_expression_depth,
    resolve_references, span_at, static_expression,
};

use super::ExternalFunction;
use super::semantic_ir::{
    DerivedArithmeticOp, DerivedScalarOperand, DerivedScalarSpec, IntCompareOp, ItemScalarUpdate,
    ItemTextUpdate, ObjectDerivedScalarOperand, ObjectItemActionKind, ObjectItemActionSpec,
    ObjectListFilter, ObjectListItem, ObjectListUpdate, RuntimeModel, ScalarRuntimeModel,
    ScalarUpdate, SemanticAction, SemanticEventBinding, SemanticFactBinding, SemanticFactKind,
    SemanticNode, SemanticInputValue, SemanticProgram, SemanticStyleFragment, SemanticTextPart,
    StateRuntimeModel, TextListFilter, TextListTemplate, TextListUpdate, TextUpdate,
};

type StaticExpression = static_expression::Expression;
type StaticSpannedExpression = static_expression::Spanned<StaticExpression>;
type StaticArgument = static_expression::Argument;
type StaticObject = static_expression::Object;
type StaticTextPart = static_expression::TextPart;

#[derive(Debug, Clone, PartialEq, Eq)]
enum PassedScope {
    Path(String),
    Bindings(BTreeMap<String, String>),
}

#[derive(Debug, Clone)]
struct CounterSpec {
    initial: i64,
    events: Vec<EventDeltaSpec>,
}

#[derive(Debug, Clone)]
struct EventDeltaSpec {
    trigger_binding: String,
    event_name: String,
    delta: i64,
}

#[derive(Debug, Clone)]
struct LatestValueSpec {
    initial_value: i64,
    static_sum: i64,
    event_values: Vec<EventValueSpec>,
}

#[derive(Debug, Clone)]
struct EventValueSpec {
    trigger_binding: String,
    event_name: String,
    value: i64,
}

#[derive(Debug, Clone)]
struct FunctionSpec<'a> {
    parameters: Vec<String>,
    body: &'a StaticSpannedExpression,
}

#[derive(Debug, Clone, Default)]
struct ScalarPlan {
    initial_values: BTreeMap<String, i64>,
    event_updates: BTreeMap<(String, String), Vec<ScalarUpdate>>,
    derived_scalars: Vec<DerivedScalarSpec>,
}

#[derive(Debug, Clone, Default)]
struct ListPlan {
    initial_values: BTreeMap<String, Vec<String>>,
    event_updates: BTreeMap<(String, String), Vec<TextListUpdate>>,
}

#[derive(Debug, Clone, Default)]
struct TextPlan {
    initial_values: BTreeMap<String, String>,
    event_updates: BTreeMap<(String, String), Vec<TextUpdate>>,
}

#[derive(Debug, Clone, Default)]
struct ObjectListPlan {
    initial_values: BTreeMap<String, Vec<ObjectListItem>>,
    event_updates: BTreeMap<(String, String), Vec<ObjectListUpdate>>,
    item_actions: BTreeMap<String, Vec<ObjectItemActionSpec>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ObjectFieldKind {
    Scalar,
    Text,
}

#[derive(Debug, Clone, Default)]
struct TopLevelObjectFieldPlan {
    scalar_initials: BTreeMap<String, i64>,
    text_initials: BTreeMap<String, String>,
    item_actions: BTreeMap<String, Vec<ObjectItemActionSpec>>,
}

#[derive(Debug, Clone, Default)]
struct StaticObjectListPlan {
    item_bases: BTreeMap<String, Vec<String>>,
}

#[derive(Debug, Clone)]
struct DetectedStaticObjectList {
    items: Vec<StaticObjectRuntimeSpec>,
    remove: Option<StaticObjectRemoveSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RuntimeTextListRef {
    binding: String,
    filter: Option<TextListFilter>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RuntimeObjectListRef {
    binding: String,
    filter: Option<ObjectListFilter>,
}

#[derive(Debug, Clone, Default)]
struct LocalBinding<'a> {
    expr: Option<&'a StaticSpannedExpression>,
    object_base: Option<String>,
}

#[derive(Debug, Default)]
struct LowerContext<'a> {
    bindings: BTreeMap<String, &'a StaticSpannedExpression>,
    path_bindings: BTreeMap<String, &'a StaticSpannedExpression>,
    functions: BTreeMap<String, FunctionSpec<'a>>,
    scalar_plan: ScalarPlan,
    text_plan: TextPlan,
    list_plan: ListPlan,
    object_list_plan: ObjectListPlan,
    static_object_lists: StaticObjectListPlan,
    scalar_eval_cache: RefCell<BTreeMap<String, Option<i64>>>,
    text_eval_cache: RefCell<BTreeMap<String, String>>,
    scalar_eval_in_progress: RefCell<BTreeSet<String>>,
    text_eval_in_progress: RefCell<BTreeSet<String>>,
}

type LocalScopes<'a> = Vec<BTreeMap<String, LocalBinding<'a>>>;
type PassedScopes = Vec<PassedScope>;

pub fn lower_to_semantic(
    source: &str,
    _external_functions: Option<&[ExternalFunction]>,
    _persistence_enabled: bool,
) -> SemanticProgram {
    match parse_and_lower(source) {
        Ok(program) => program,
        Err(error) => unsupported_program(error),
    }
}

fn parse_and_lower(source: &str) -> Result<SemanticProgram, String> {
    let expressions = parse_static_expressions(source)?;
    let bindings = top_level_bindings(&expressions);
    let functions = top_level_functions(&expressions);
    let path_bindings = flatten_binding_paths(&bindings);
    let mut scalar_plan = detect_scalar_plan(&path_bindings, &functions)
        .map_err(|error| format!("detect_scalar_plan: {error}"))?;
    let static_object_lists = detect_static_object_list_plan(
        &path_bindings,
        &functions,
        &mut scalar_plan,
    )
    .map_err(|error| format!("detect_static_object_list_plan: {error}"))?;
    let text_plan = detect_text_plan(&path_bindings)
        .map_err(|error| format!("detect_text_plan: {error}"))?;
    let object_list_plan = detect_object_list_plan(&path_bindings, &functions)
        .map_err(|error| format!("detect_object_list_plan: {error}"))?;
    let mut context = LowerContext {
        text_plan,
        list_plan: detect_list_plan(&path_bindings)
            .map_err(|error| format!("detect_list_plan: {error}"))?,
        object_list_plan,
        scalar_plan,
        static_object_lists,
        bindings,
        path_bindings,
        functions,
        ..LowerContext::default()
    };
    augment_top_level_object_field_runtime(&mut context)
        .map_err(|error| format!("augment_top_level_object_field_runtime: {error}"))?;
    augment_top_level_bool_item_runtime(&mut context)
        .map_err(|error| format!("augment_top_level_bool_item_runtime: {error}"))?;

    let document = find_document_expression(&expressions, &context.bindings)
        .map_err(|error| format!("find_document_expression: {error}"))?;
    let root = lower_document_root(
        document,
        &context,
        &mut Vec::new(),
        &mut Vec::new(),
        &mut Vec::new(),
        None,
    )
    .map_err(|error| format!("lower_document_root: {error}"))?;

    Ok(SemanticProgram {
        root,
        runtime: runtime_model_for(&context),
    })
}

fn parse_static_expressions(source: &str) -> Result<Vec<StaticSpannedExpression>, String> {
    let source_code = SourceCode::new(source.to_string());
    let parse_source = source_code.clone();
    let source = parse_source.as_str();
    let (tokens, lex_errors) = lexer().parse(source).into_output_errors();
    if let Some(error) = lex_errors.into_iter().next() {
        return Err(format!("lex error: {error}"));
    }
    let mut tokens = tokens.ok_or_else(|| "lex error: no tokens produced".to_string())?;
    tokens.retain(|spanned_token| !matches!(spanned_token.node, Token::Comment(_)));

    reset_expression_depth();
    let (ast, parse_errors) = parser()
        .parse(tokens.map(
            span_at(source.len()),
            |crate::parser::Spanned {
                 node,
                 span,
                 persistence: _,
             }| { (node, span) },
        ))
        .into_output_errors();
    if let Some(error) = parse_errors.into_iter().next() {
        return Err(format!("parse error: {error}"));
    }
    let ast = ast.ok_or_else(|| "parse error: no AST produced".to_string())?;
    let ast = resolve_references(ast).map_err(|errors| {
        errors.into_iter().next().map_or_else(
            || "reference error".to_string(),
            |error| format!("reference error: {error}"),
        )
    })?;

    Ok(static_expression::convert_expressions(source_code, ast))
}

fn top_level_bindings<'a>(
    expressions: &'a [StaticSpannedExpression],
) -> BTreeMap<String, &'a StaticSpannedExpression> {
    expressions
        .iter()
        .filter_map(|expression| match &expression.node {
            StaticExpression::Variable(variable) => {
                Some((variable.name.as_str().to_string(), &variable.value))
            }
            _ => None,
        })
        .collect()
}

fn top_level_functions<'a>(
    expressions: &'a [StaticSpannedExpression],
) -> BTreeMap<String, FunctionSpec<'a>> {
    expressions
        .iter()
        .filter_map(|expression| match &expression.node {
            StaticExpression::Function {
                name,
                parameters,
                body,
            } => Some((
                name.as_str().to_string(),
                FunctionSpec {
                    parameters: parameters
                        .iter()
                        .map(|parameter| parameter.node.as_str().to_string())
                        .collect(),
                    body,
                },
            )),
            _ => None,
        })
        .collect()
}

fn flatten_binding_paths<'a>(
    bindings: &BTreeMap<String, &'a StaticSpannedExpression>,
) -> BTreeMap<String, &'a StaticSpannedExpression> {
    let mut path_bindings = BTreeMap::new();
    for (name, expression) in bindings {
        path_bindings.insert(name.clone(), *expression);
        collect_nested_binding_paths(name, expression, &mut path_bindings);
    }
    path_bindings
}

fn collect_nested_binding_paths<'a>(
    base_path: &str,
    expression: &'a StaticSpannedExpression,
    path_bindings: &mut BTreeMap<String, &'a StaticSpannedExpression>,
) {
    let Some(object) = resolve_object(expression) else {
        return;
    };
    for variable in &object.variables {
        if variable.node.name.is_empty() {
            continue;
        }
        let path = format!("{base_path}.{}", variable.node.name.as_str());
        path_bindings.insert(path.clone(), &variable.node.value);
        collect_nested_binding_paths(&path, &variable.node.value, path_bindings);
    }
}

fn find_document_expression<'a>(
    expressions: &'a [StaticSpannedExpression],
    bindings: &'a BTreeMap<String, &'a StaticSpannedExpression>,
) -> Result<&'a StaticSpannedExpression, String> {
    if let Some(document) = bindings.get("document") {
        return Ok(document);
    }
    if let [expression] = expressions {
        return Ok(expression);
    }
    Err("expected top-level `document: Document/new(root: ...)` binding".to_string())
}

fn lower_document_root<'a>(
    expression: &'a StaticSpannedExpression,
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
    current_binding: Option<&str>,
) -> Result<SemanticNode, String> {
    let expression = resolve_alias(expression, context, locals, passed, stack)?;
    match &expression.node {
        StaticExpression::FunctionCall { path, arguments }
            if path_matches(path, &["Document", "new"]) =>
        {
            let root = find_named_argument(arguments, "root")
                .ok_or_else(|| "Document/new requires a `root` argument".to_string())?;
            lower_ui_node(root, context, stack, locals, passed, current_binding)
        }
        StaticExpression::Pipe { from, to } => {
            let to = resolve_alias(to, context, locals, passed, stack)?;
            match &to.node {
                StaticExpression::FunctionCall { path, arguments }
                    if path_matches(path, &["Document", "new"]) =>
                {
                    if find_named_argument(arguments, "root").is_some() {
                        return Err(
                            "pipe form Document/new(root: ...) is not supported yet".to_string()
                        );
                    }
                    lower_ui_node(from, context, stack, locals, passed, current_binding)
                }
                _ => Err("document must be produced by Document/new(...)".to_string()),
            }
        }
        _ => Err("document must be produced by Document/new(...)".to_string()),
    }
}

fn lower_ui_node<'a>(
    expression: &'a StaticSpannedExpression,
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
    current_binding: Option<&str>,
) -> Result<SemanticNode, String> {
    if let Some(field) = placeholder_object_field(expression, context, locals, passed)? {
        return Ok(SemanticNode::ObjectFieldValue { field });
    }

    if let Some(bool_text) =
        lower_placeholder_object_bool_text_node(expression, context, stack, locals, passed)?
    {
        return Ok(bool_text);
    }

    if let Some(bool_text) = lower_bool_text_node(expression, context, stack, locals, passed)? {
        return Ok(bool_text);
    }

    if let Some(matched) = lower_value_when_node(expression, context, stack, locals, passed)? {
        return Ok(matched);
    }

    if let Some((binding_name, value)) =
        resolve_scalar_reference(expression, context, locals, passed, stack)?
    {
        return Ok(SemanticNode::ScalarValue {
            binding: binding_name,
            value,
        });
    }

    if let Some((binding_name, value)) =
        resolve_text_reference(expression, context, locals, passed, stack)?
    {
        return Ok(SemanticNode::text_template(
            vec![SemanticTextPart::TextBinding(binding_name)],
            value,
        ));
    }

    if let Some(branch) = lower_ui_branch_node(expression, context, stack, locals, passed)? {
        return Ok(branch);
    }

    if let Some(binding_name) = alias_binding_name(expression)? {
        let resolved = resolve_named_binding(binding_name, context, locals, stack)?;
        let result = lower_ui_node(resolved, context, stack, locals, passed, Some(binding_name));
        stack.pop();
        return result;
    }

    let expression = resolve_alias(expression, context, locals, passed, stack)?;
    match &expression.node {
        StaticExpression::Pipe { from, to } => {
            if let StaticExpression::LinkSetter { alias } = &to.node {
                let Some(target_path) =
                    canonical_alias_path(&alias.node, context, locals, passed, stack)?
                else {
                    return Err("LINK target must resolve to a binding path".to_string());
                };
                return lower_ui_node(
                    from,
                    context,
                    stack,
                    locals,
                    passed,
                    Some(target_path.as_str()),
                );
            }
            if let Some((function_name, arguments)) =
                function_invocation_target(to, context, locals, passed, stack)?
            {
                return invoke_function(
                    function_name,
                    arguments,
                    Some(from.as_ref()),
                    context,
                    stack,
                    locals,
                    passed,
                    current_binding,
                );
            }
            Err(format!(
                "Wasm Pro parser-backed lowering does not support expression `{}` yet",
                describe_expression_detailed(expression)
            ))
        }
        StaticExpression::FunctionCall { path, arguments }
            if path.len() == 1 && context.functions.contains_key(path[0].as_str()) =>
        {
            invoke_function(
                path[0].as_str(),
                arguments,
                None,
                context,
                stack,
                locals,
                passed,
                current_binding,
            )
        }
        StaticExpression::FunctionCall { path, arguments }
            if path_matches(path, &["Text", "space"]) && arguments.is_empty() =>
        {
            Ok(SemanticNode::text(" "))
        }
        StaticExpression::FunctionCall { path, arguments }
            if path_matches(path, &["Text", "empty"]) && arguments.is_empty() =>
        {
            Ok(SemanticNode::text(""))
        }
        StaticExpression::Literal(static_expression::Literal::Number(number)) => {
            Ok(SemanticNode::text(trim_number(*number)))
        }
        StaticExpression::Literal(static_expression::Literal::Tag(tag))
            if tag.as_str() == "NoElement" =>
        {
            Ok(SemanticNode::Fragment(Vec::new()))
        }
        StaticExpression::Literal(static_expression::Literal::Text(text))
        | StaticExpression::Literal(static_expression::Literal::Tag(text)) => {
            Ok(SemanticNode::text(text.as_str()))
        }
        StaticExpression::Block { variables, output } => {
            let mut scope = BTreeMap::new();
            for variable in variables {
                let object_base = infer_argument_object_base(
                    &variable.node.value,
                    context,
                    locals,
                    passed,
                )
                .or_else(|| active_object_scope(locals));
                scope.insert(
                    variable.node.name.as_str().to_string(),
                    LocalBinding {
                        expr: Some(&variable.node.value),
                        object_base,
                    },
                );
            }
            locals.push(scope);
            let lowered = lower_ui_node(output, context, stack, locals, passed, current_binding);
            locals.pop();
            lowered
        }
        StaticExpression::TextLiteral { parts, .. } => {
            lower_text_node(parts, context, stack, locals, passed)
        }
        StaticExpression::FunctionCall { path, arguments }
            if path_matches(path, &["Element", "label"]) =>
        {
            lower_label(arguments, context, stack, locals, passed, current_binding)
        }
        StaticExpression::FunctionCall { path, arguments }
            if path_matches(path, &["Element", "button"]) =>
        {
            lower_button(arguments, context, stack, locals, passed, current_binding)
        }
        StaticExpression::FunctionCall { path, arguments }
            if path_matches(path, &["Element", "container"]) =>
        {
            lower_container(arguments, context, stack, locals, passed, current_binding)
        }
        StaticExpression::FunctionCall { path, arguments }
            if path_matches(path, &["Element", "stripe"]) =>
        {
            lower_stripe(arguments, context, stack, locals, passed, current_binding)
        }
        StaticExpression::FunctionCall { path, arguments }
            if path_matches(path, &["Element", "text_input"]) =>
        {
            lower_text_input(arguments, context, stack, locals, passed, current_binding)
        }
        StaticExpression::FunctionCall { path, arguments }
            if path_matches(path, &["Element", "checkbox"]) =>
        {
            lower_checkbox(arguments, context, stack, locals, passed, current_binding)
        }
        StaticExpression::FunctionCall { path, arguments }
            if path_matches(path, &["Element", "paragraph"]) =>
        {
            lower_paragraph(arguments, context, stack, locals, passed, current_binding)
        }
        StaticExpression::FunctionCall { path, arguments }
            if path_matches(path, &["Element", "link"]) =>
        {
            lower_link(arguments, context, stack, locals, passed, current_binding)
        }
        _ => Err(format!(
            "Wasm Pro parser-backed lowering does not support expression `{}` yet",
            describe_expression_detailed(expression)
        )),
    }
}

fn lower_ui_branch_node<'a>(
    expression: &'a StaticSpannedExpression,
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<Option<SemanticNode>, String> {
    let expression = resolve_alias(expression, context, locals, passed, stack)?;
    let StaticExpression::Pipe { from, to } = &expression.node else {
        return Ok(None);
    };
    let StaticExpression::While { arms } = &to.node else {
        return Ok(None);
    };
    let Some((truthy_expr, falsy_expr)) = bool_ui_branch_arms(arms)? else {
        return Ok(None);
    };
    lower_bool_condition_branch(
        from,
        truthy_expr,
        falsy_expr,
        context,
        stack,
        locals,
        passed,
    )
}

fn lower_bool_condition_branch<'a>(
    expression: &'a StaticSpannedExpression,
    truthy_expr: &'a StaticSpannedExpression,
    falsy_expr: &'a StaticSpannedExpression,
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<Option<SemanticNode>, String> {
    if let StaticExpression::Pipe { from, to } = &expression.node {
        if let StaticExpression::When { arms } = &to.node {
            if let Some((truthy_body, falsy_body)) = bool_condition_arm_bodies(arms) {
                let truthy_node = lower_bool_condition_result(
                    truthy_body,
                    truthy_expr,
                    falsy_expr,
                    context,
                    stack,
                    locals,
                    passed,
                )?;
                let falsy_node = lower_bool_condition_result(
                    falsy_body,
                    truthy_expr,
                    falsy_expr,
                    context,
                    stack,
                    locals,
                    passed,
                )?;
                return lower_bool_condition_from_nodes(
                    from,
                    truthy_node,
                    falsy_node,
                    context,
                    stack,
                    locals,
                    passed,
                );
            }
        }
    }
    let truthy_node = lower_ui_node(truthy_expr, context, stack, locals, passed, None)?;
    let falsy_node = lower_ui_node(falsy_expr, context, stack, locals, passed, None)?;
    lower_bool_condition_from_nodes(
        expression,
        truthy_node,
        falsy_node,
        context,
        stack,
        locals,
        passed,
    )
}

fn lower_bool_condition_from_nodes<'a>(
    expression: &'a StaticSpannedExpression,
    truthy_node: SemanticNode,
    falsy_node: SemanticNode,
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<Option<SemanticNode>, String> {
    if let Some(binding_name) = alias_binding_name(expression)? {
        let resolved = resolve_named_binding(binding_name, context, locals, stack)?;
        let result = lower_bool_condition_from_nodes(
            resolved,
            truthy_node,
            falsy_node,
            context,
            stack,
            locals,
            passed,
        );
        stack.pop();
        return result;
    }
    if let StaticExpression::Pipe { from, to } = &expression.node {
        if let StaticExpression::When { arms } | StaticExpression::While { arms } = &to.node {
            if let Some((truthy_body, falsy_body)) = bool_condition_arm_bodies(arms) {
                let nested_truthy_node = lower_bool_condition_from_nodes(
                    truthy_body,
                    truthy_node.clone(),
                    falsy_node.clone(),
                    context,
                    stack,
                    locals,
                    passed,
                )?
                .ok_or_else(|| {
                    format!(
                        "Wasm Pro parser-backed lowering does not support boolean condition `{}` yet",
                        describe_expression_detailed(truthy_body)
                    )
                })?;
                let nested_falsy_node = lower_bool_condition_from_nodes(
                    falsy_body,
                    truthy_node.clone(),
                    falsy_node.clone(),
                    context,
                    stack,
                    locals,
                    passed,
                )?
                .ok_or_else(|| {
                    format!(
                        "Wasm Pro parser-backed lowering does not support boolean condition `{}` yet",
                        describe_expression_detailed(falsy_body)
                    )
                })?;
                return lower_bool_condition_from_nodes(
                    from,
                    nested_truthy_node,
                    nested_falsy_node,
                    context,
                    stack,
                    locals,
                    passed,
                );
            }
        }
    }
    if let Some(field) = placeholder_object_field(expression, context, locals, passed)? {
        return Ok(Some(SemanticNode::object_bool_field_branch(
            field,
            truthy_node.clone(),
            falsy_node.clone(),
        )));
    }
    if let Some((field, invert)) =
        object_text_empty_branch_condition(expression, context, locals, passed, stack)?
    {
        return Ok(Some(SemanticNode::object_text_field_branch(
            field,
            invert,
            truthy_node.clone(),
            falsy_node.clone(),
        )));
    }
    if let Some((binding, invert)) =
        text_empty_branch_condition(expression, context, locals, passed, stack)?
    {
        return Ok(Some(SemanticNode::text_binding_branch(
            binding,
            invert,
            truthy_node.clone(),
            falsy_node.clone(),
        )));
    }
    if let Some(binding) = bool_binding_path(expression, context, locals, passed, stack)? {
        return Ok(Some(SemanticNode::bool_branch(
            binding,
            truthy_node.clone(),
            falsy_node.clone(),
        )));
    }
    if let Some((binding, object_items, invert)) =
        list_empty_branch_condition(expression, context, locals, passed, stack)?
    {
        return Ok(Some(SemanticNode::list_empty_branch(
            binding,
            object_items,
            invert,
            truthy_node.clone(),
            falsy_node.clone(),
        )));
    }
    if let Some((left, op, right)) =
        scalar_compare_branch_operands(expression, context, locals, passed, stack)?
    {
        return Ok(Some(SemanticNode::scalar_compare_branch(
            left,
            op,
            right,
            truthy_node.clone(),
            falsy_node.clone(),
        )));
    }
    if let Some((left, op, right)) =
        object_scalar_compare_branch_operands(expression, context, locals, passed, stack)?
    {
        return Ok(Some(SemanticNode::object_scalar_compare_branch(
            left,
            op,
            right,
            truthy_node.clone(),
            falsy_node.clone(),
        )));
    }
    if let Some(selected) = initial_bool_expression(expression, context, stack, locals, passed)? {
        return Ok(Some(if selected { truthy_node } else { falsy_node }));
    }
    if let Some((function_name, arguments)) =
        function_invocation_target(expression, context, locals, passed, stack)?
    {
        return with_invoked_function_scope(
            function_name,
            arguments,
            None,
            context,
            stack,
            locals,
            passed,
            |body, context, stack, locals, passed| {
                lower_bool_condition_from_nodes(
                    body,
                    truthy_node,
                    falsy_node,
                    context,
                    stack,
                    locals,
                    passed,
                )
            },
        );
    }
    Ok(None)
}

fn text_empty_branch_condition(
    expression: &StaticSpannedExpression,
    context: &LowerContext<'_>,
    locals: &LocalScopes<'_>,
    passed: &PassedScopes,
    stack: &mut Vec<String>,
) -> Result<Option<(String, bool)>, String> {
    let expression = resolve_alias(expression, context, locals, passed, stack)?;
    let StaticExpression::Pipe { from, to } = &expression.node else {
        return Ok(None);
    };
    let Some(binding) =
        canonical_expression_path(from, context, locals, passed, &mut Vec::new())
            .ok()
            .filter(|path| context.text_plan.initial_values.contains_key(path))
    else {
        return Ok(None);
    };
    let StaticExpression::FunctionCall { path, arguments } = &to.node else {
        return Ok(None);
    };
    if !arguments.is_empty() {
        return Ok(None);
    }
    if path_matches(path, &["Text", "is_empty"]) {
        return Ok(Some((binding, false)));
    }
    if path_matches(path, &["Text", "is_not_empty"]) {
        return Ok(Some((binding, true)));
    }
    Ok(None)
}

fn object_text_empty_branch_condition(
    expression: &StaticSpannedExpression,
    context: &LowerContext<'_>,
    locals: &LocalScopes<'_>,
    passed: &PassedScopes,
    stack: &mut Vec<String>,
) -> Result<Option<(String, bool)>, String> {
    let expression = resolve_alias(expression, context, locals, passed, stack)?;
    let StaticExpression::Pipe { from, to } = &expression.node else {
        return Ok(None);
    };
    let Some(field) = placeholder_object_field(from, context, locals, passed)? else {
        return Ok(None);
    };
    let StaticExpression::FunctionCall { path, arguments } = &to.node else {
        return Ok(None);
    };
    if !arguments.is_empty() {
        return Ok(None);
    }
    if path_matches(path, &["Text", "is_empty"]) {
        return Ok(Some((field, false)));
    }
    if path_matches(path, &["Text", "is_not_empty"]) {
        return Ok(Some((field, true)));
    }
    Ok(None)
}

fn lower_bool_condition_result<'a>(
    expression: &'a StaticSpannedExpression,
    truthy_expr: &'a StaticSpannedExpression,
    falsy_expr: &'a StaticSpannedExpression,
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<SemanticNode, String> {
    lower_bool_condition_branch(
        expression,
        truthy_expr,
        falsy_expr,
        context,
        stack,
        locals,
        passed,
    )?
    .ok_or_else(|| {
        format!(
            "Wasm Pro parser-backed lowering does not support boolean condition `{}` yet",
            describe_expression_detailed(expression)
        )
    })
}

fn bool_ui_branch_arms<'a>(
    arms: &'a [static_expression::Arm],
) -> Result<Option<(&'a StaticSpannedExpression, &'a StaticSpannedExpression)>, String> {
    let mut truthy = None;
    let mut falsy = None;
    for arm in arms {
        match &arm.pattern {
            static_expression::Pattern::Literal(static_expression::Literal::Tag(tag))
                if tag.as_str() == "True" =>
            {
                truthy = Some(&arm.body);
            }
            static_expression::Pattern::Literal(static_expression::Literal::Tag(tag))
                if tag.as_str() == "False" =>
            {
                falsy = Some(&arm.body);
            }
            static_expression::Pattern::WildCard => {
                falsy = Some(&arm.body);
            }
            _ => {}
        }
    }
    Ok(truthy.zip(falsy))
}

fn bool_condition_arm_bodies<'a>(
    arms: &'a [static_expression::Arm],
) -> Option<(&'a StaticSpannedExpression, &'a StaticSpannedExpression)> {
    let mut truthy = None;
    let mut falsy = None;
    for arm in arms {
        match &arm.pattern {
            static_expression::Pattern::Literal(static_expression::Literal::Tag(tag))
                if tag.as_str() == "True" =>
            {
                truthy = Some(&arm.body);
            }
            static_expression::Pattern::Literal(static_expression::Literal::Tag(tag))
                if tag.as_str() == "False" =>
            {
                falsy = Some(&arm.body);
            }
            static_expression::Pattern::WildCard => {
                falsy = Some(&arm.body);
            }
            _ => {}
        }
    }
    truthy.zip(falsy)
}

fn list_empty_branch_condition(
    expression: &StaticSpannedExpression,
    context: &LowerContext<'_>,
    locals: &LocalScopes<'_>,
    passed: &PassedScopes,
    stack: &mut Vec<String>,
) -> Result<Option<(String, bool, bool)>, String> {
    let expression = resolve_alias(expression, context, locals, passed, stack)?;
    let StaticExpression::Pipe { from, to } = &expression.node else {
        return Ok(None);
    };
    let StaticExpression::FunctionCall { path, arguments } = &to.node else {
        return Ok(None);
    };
    if path_matches(path, &["Bool", "not"]) && arguments.is_empty() {
        if let Some((binding, object_items, invert)) =
            list_empty_branch_condition(from, context, locals, passed, stack)?
        {
            return Ok(Some((binding, object_items, !invert)));
        }
    }
    if !path_matches(path, &["List", "is_empty"]) || !arguments.is_empty() {
        return Ok(None);
    }
    if let Some(binding) = runtime_object_list_binding_path(from, context, locals, passed, stack)? {
        return Ok(Some((binding, true, false)));
    }
    if let Some(binding) = runtime_list_binding_path(from, context, locals, passed, stack)? {
        return Ok(Some((binding, false, false)));
    }
    Ok(None)
}

fn initial_bool_expression<'a>(
    expression: &'a StaticSpannedExpression,
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<Option<bool>, String> {
    let expression = resolve_alias(expression, context, locals, passed, stack)?;
    if let Some(value) =
        initial_scalar_value_in_context(expression, context, stack, locals, passed)?
    {
        return Ok(Some(value != 0));
    }
    match &expression.node {
        StaticExpression::Pipe { from, to }
            if matches!(
                &to.node,
                StaticExpression::FunctionCall { path, arguments }
                    if path_matches(path, &["Bool", "not"]) && arguments.is_empty()
            ) =>
        {
            Ok(initial_bool_expression(from, context, stack, locals, passed)?.map(|value| !value))
        }
        StaticExpression::Pipe { from, to } => {
            let StaticExpression::FunctionCall { path, arguments } = &to.node else {
                return Ok(None);
            };
            if path_matches(path, &["Text", "is_empty"]) && arguments.is_empty() {
                if let Ok(value) =
                    lower_text_value(from, context, stack, locals, passed).or_else(|_| {
                        lower_text_input_initial_value(from, context, stack, locals, passed)
                    })
                {
                    return Ok(Some(value.is_empty()));
                }
            }
            if path_matches(path, &["Text", "is_not_empty"]) && arguments.is_empty() {
                if let Ok(value) =
                    lower_text_value(from, context, stack, locals, passed).or_else(|_| {
                        lower_text_input_initial_value(from, context, stack, locals, passed)
                    })
                {
                    return Ok(Some(!value.is_empty()));
                }
            }
            Ok(None)
        }
        _ => Ok(None),
    }
}

fn conditional_alias_arm<'a>(
    arms: &'a [static_expression::Arm],
) -> Option<(&'a str, &'a StaticSpannedExpression)> {
    arms.iter().find_map(|arm| match &arm.pattern {
        static_expression::Pattern::Alias { name } => Some((name.as_str(), &arm.body)),
        _ => None,
    })
}

fn scalar_compare_branch_operands(
    expression: &StaticSpannedExpression,
    context: &LowerContext<'_>,
    locals: &LocalScopes<'_>,
    passed: &PassedScopes,
    stack: &mut Vec<String>,
) -> Result<Option<(DerivedScalarOperand, IntCompareOp, DerivedScalarOperand)>, String> {
    let expression = resolve_alias(expression, context, locals, passed, stack)?;
    let StaticExpression::Comparator(comparator) = &expression.node else {
        return Ok(None);
    };
    let (left_expr, right_expr, op) = match comparator {
        static_expression::Comparator::Equal {
            operand_a,
            operand_b,
        } => (operand_a.as_ref(), operand_b.as_ref(), IntCompareOp::Equal),
        static_expression::Comparator::NotEqual {
            operand_a,
            operand_b,
        } => (
            operand_a.as_ref(),
            operand_b.as_ref(),
            IntCompareOp::NotEqual,
        ),
        static_expression::Comparator::Greater {
            operand_a,
            operand_b,
        } => (
            operand_a.as_ref(),
            operand_b.as_ref(),
            IntCompareOp::Greater,
        ),
        static_expression::Comparator::GreaterOrEqual {
            operand_a,
            operand_b,
        } => (
            operand_a.as_ref(),
            operand_b.as_ref(),
            IntCompareOp::GreaterOrEqual,
        ),
        static_expression::Comparator::Less {
            operand_a,
            operand_b,
        } => (operand_a.as_ref(), operand_b.as_ref(), IntCompareOp::Less),
        static_expression::Comparator::LessOrEqual {
            operand_a,
            operand_b,
        } => (
            operand_a.as_ref(),
            operand_b.as_ref(),
            IntCompareOp::LessOrEqual,
        ),
    };
    let Some(left) = derived_scalar_operand_in_context(left_expr, context, locals, passed, stack)?
    else {
        return Ok(None);
    };
    let Some(right) =
        derived_scalar_operand_in_context(right_expr, context, locals, passed, stack)?
    else {
        return Ok(None);
    };
    Ok(Some((left, op, right)))
}

fn object_scalar_compare_branch_operands(
    expression: &StaticSpannedExpression,
    context: &LowerContext<'_>,
    locals: &LocalScopes<'_>,
    passed: &PassedScopes,
    stack: &mut Vec<String>,
) -> Result<Option<(ObjectDerivedScalarOperand, IntCompareOp, ObjectDerivedScalarOperand)>, String>
{
    let expression = resolve_alias(expression, context, locals, passed, stack)?;
    let StaticExpression::Comparator(comparator) = &expression.node else {
        return Ok(None);
    };
    let (left_expr, right_expr, op) = match comparator {
        static_expression::Comparator::Equal {
            operand_a,
            operand_b,
        } => (operand_a.as_ref(), operand_b.as_ref(), IntCompareOp::Equal),
        static_expression::Comparator::NotEqual {
            operand_a,
            operand_b,
        } => (
            operand_a.as_ref(),
            operand_b.as_ref(),
            IntCompareOp::NotEqual,
        ),
        static_expression::Comparator::Greater {
            operand_a,
            operand_b,
        } => (
            operand_a.as_ref(),
            operand_b.as_ref(),
            IntCompareOp::Greater,
        ),
        static_expression::Comparator::GreaterOrEqual {
            operand_a,
            operand_b,
        } => (
            operand_a.as_ref(),
            operand_b.as_ref(),
            IntCompareOp::GreaterOrEqual,
        ),
        static_expression::Comparator::Less {
            operand_a,
            operand_b,
        } => (operand_a.as_ref(), operand_b.as_ref(), IntCompareOp::Less),
        static_expression::Comparator::LessOrEqual {
            operand_a,
            operand_b,
        } => (
            operand_a.as_ref(),
            operand_b.as_ref(),
            IntCompareOp::LessOrEqual,
        ),
    };
    let Some(left) =
        object_derived_scalar_operand_in_context(left_expr, context, locals, passed, stack)?
    else {
        return Ok(None);
    };
    let Some(right) =
        object_derived_scalar_operand_in_context(right_expr, context, locals, passed, stack)?
    else {
        return Ok(None);
    };
    let has_field = matches!(left, ObjectDerivedScalarOperand::Field(_))
        || matches!(right, ObjectDerivedScalarOperand::Field(_));
    Ok(has_field.then_some((left, op, right)))
}

fn object_derived_scalar_operand_in_context(
    expression: &StaticSpannedExpression,
    context: &LowerContext<'_>,
    locals: &LocalScopes<'_>,
    passed: &PassedScopes,
    stack: &mut Vec<String>,
) -> Result<Option<ObjectDerivedScalarOperand>, String> {
    if let Some(field) = placeholder_object_field(expression, context, locals, passed)? {
        return Ok(Some(ObjectDerivedScalarOperand::Field(field)));
    }
    if let Some(value) = extract_integer_literal_opt(expression)? {
        return Ok(Some(ObjectDerivedScalarOperand::Literal(value)));
    }
    if let Some(value) = extract_bool_literal_opt(expression)? {
        return Ok(Some(ObjectDerivedScalarOperand::Literal(i64::from(value))));
    }
    if let Some(value) = extract_filter_tag_value(expression)? {
        return Ok(Some(ObjectDerivedScalarOperand::Literal(value)));
    }
    if let Some((binding, _)) =
        resolve_scalar_reference(expression, context, locals, passed, stack)?
    {
        return Ok(Some(ObjectDerivedScalarOperand::Binding(binding)));
    }
    let expression = resolve_alias(expression, context, locals, passed, stack)?;
    if let Some(field) = placeholder_object_field(expression, context, locals, passed)? {
        return Ok(Some(ObjectDerivedScalarOperand::Field(field)));
    }
    if let Some(value) = extract_integer_literal_opt(expression)? {
        return Ok(Some(ObjectDerivedScalarOperand::Literal(value)));
    }
    if let Some(value) = extract_bool_literal_opt(expression)? {
        return Ok(Some(ObjectDerivedScalarOperand::Literal(i64::from(value))));
    }
    if let Some(value) = extract_filter_tag_value(expression)? {
        return Ok(Some(ObjectDerivedScalarOperand::Literal(value)));
    }
    Ok(None)
}

fn derived_scalar_operand_in_context(
    expression: &StaticSpannedExpression,
    context: &LowerContext<'_>,
    locals: &LocalScopes<'_>,
    passed: &PassedScopes,
    stack: &mut Vec<String>,
) -> Result<Option<DerivedScalarOperand>, String> {
    if let Some(value) = extract_integer_literal_opt(expression)? {
        return Ok(Some(DerivedScalarOperand::Literal(value)));
    }
    if let Some(value) = extract_bool_literal_opt(expression)? {
        return Ok(Some(DerivedScalarOperand::Literal(i64::from(value))));
    }
    if let Some(value) = extract_filter_tag_value(expression)? {
        return Ok(Some(DerivedScalarOperand::Literal(value)));
    }
    if let Some((binding, _)) =
        resolve_scalar_reference(expression, context, locals, passed, stack)?
    {
        return Ok(Some(DerivedScalarOperand::Binding(binding)));
    }
    let expression = resolve_alias(expression, context, locals, passed, stack)?;
    if let Some(value) = extract_integer_literal_opt(expression)? {
        return Ok(Some(DerivedScalarOperand::Literal(value)));
    }
    if let Some(value) = extract_bool_literal_opt(expression)? {
        return Ok(Some(DerivedScalarOperand::Literal(i64::from(value))));
    }
    if let Some(value) = extract_filter_tag_value(expression)? {
        return Ok(Some(DerivedScalarOperand::Literal(value)));
    }
    Ok(None)
}

fn lower_label<'a>(
    arguments: &'a [static_expression::Spanned<StaticArgument>],
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
    current_binding: Option<&str>,
) -> Result<SemanticNode, String> {
    let label = find_named_argument(arguments, "label")
        .ok_or_else(|| "Element/label requires `label`".to_string())?;
    let element = find_named_argument(arguments, "element");
    let style = find_named_argument(arguments, "style");

    finalize_element_node(
        SemanticNode::element_with_facts(
            infer_tag(element, "span"),
            None,
            collect_common_properties(element),
            collect_event_bindings(element, current_binding, context),
            collect_fact_bindings(element),
            vec![lower_ui_node(label, context, stack, locals, passed, None)?],
        ),
        style,
        context,
        stack,
        locals,
        passed,
    )
}

fn lower_paragraph<'a>(
    arguments: &'a [static_expression::Spanned<StaticArgument>],
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
    current_binding: Option<&str>,
) -> Result<SemanticNode, String> {
    let contents = find_named_argument(arguments, "contents")
        .ok_or_else(|| "Element/paragraph requires `contents`".to_string())?;
    let element = find_named_argument(arguments, "element");
    let style = find_named_argument(arguments, "style");

    finalize_element_node(
        SemanticNode::element_with_facts(
            infer_tag(element, "p"),
            None,
            collect_common_properties(element),
            collect_event_bindings(element, current_binding, context),
            collect_fact_bindings(element),
            lower_list_items(contents, context, stack, locals, passed)?,
        ),
        style,
        context,
        stack,
        locals,
        passed,
    )
}

fn lower_link<'a>(
    arguments: &'a [static_expression::Spanned<StaticArgument>],
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
    current_binding: Option<&str>,
) -> Result<SemanticNode, String> {
    let label = find_named_argument(arguments, "label")
        .ok_or_else(|| "Element/link requires `label`".to_string())?;
    let to = find_named_argument(arguments, "to");
    let new_tab = find_named_argument(arguments, "new_tab");
    let element = find_named_argument(arguments, "element");
    let style = find_named_argument(arguments, "style");

    let mut properties = collect_common_properties(element);
    if let Some(to) = to.and_then(|to| lower_text_value(to, context, stack, locals, passed).ok()) {
        properties.push(("href".to_string(), to));
    }
    if new_tab.is_some() {
        properties.push(("target".to_string(), "_blank".to_string()));
    }

    finalize_element_node(
        SemanticNode::element_with_facts(
            infer_tag(element, "a"),
            None,
            properties,
            collect_event_bindings(element, current_binding, context),
            collect_fact_bindings(element),
            vec![lower_ui_node(label, context, stack, locals, passed, None)?],
        ),
        style,
        context,
        stack,
        locals,
        passed,
    )
}

fn lower_button<'a>(
    arguments: &'a [static_expression::Spanned<StaticArgument>],
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
    current_binding: Option<&str>,
) -> Result<SemanticNode, String> {
    let label = find_named_argument(arguments, "label")
        .ok_or_else(|| "Element/button requires `label`".to_string())?;
    let element = find_named_argument(arguments, "element");
    let style = find_named_argument(arguments, "style");
    let mut properties = collect_common_properties(element);
    if !properties.iter().any(|(name, _)| name == "type") {
        properties.push(("type".to_string(), "button".to_string()));
    }

    finalize_element_node(
        SemanticNode::element_with_facts(
            infer_tag(element, "button"),
            None,
            properties,
            collect_event_bindings(element, current_binding, context),
            collect_fact_bindings(element),
            vec![lower_ui_node(label, context, stack, locals, passed, None)?],
        ),
        style,
        context,
        stack,
        locals,
        passed,
    )
}

fn lower_container<'a>(
    arguments: &'a [static_expression::Spanned<StaticArgument>],
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
    current_binding: Option<&str>,
) -> Result<SemanticNode, String> {
    let child = find_named_argument(arguments, "child")
        .ok_or_else(|| "Element/container requires `child`".to_string())?;
    let element = find_named_argument(arguments, "element");
    let style = find_named_argument(arguments, "style");

    finalize_element_node(
        SemanticNode::element_with_facts(
            infer_tag(element, "div"),
            None,
            collect_common_properties(element),
            collect_event_bindings(element, current_binding, context),
            collect_fact_bindings(element),
            vec![lower_ui_node(child, context, stack, locals, passed, None)?],
        ),
        style,
        context,
        stack,
        locals,
        passed,
    )
}

fn lower_stripe<'a>(
    arguments: &'a [static_expression::Spanned<StaticArgument>],
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
    current_binding: Option<&str>,
) -> Result<SemanticNode, String> {
    let items = find_named_argument(arguments, "items")
        .ok_or_else(|| "Element/stripe requires `items`".to_string())?;
    let direction = find_named_argument(arguments, "direction");
    let gap = find_named_argument(arguments, "gap");
    let element = find_named_argument(arguments, "element");
    let style = find_named_argument(arguments, "style");

    let children = lower_list_items(items, context, stack, locals, passed)?;
    let mut properties = collect_common_properties(element);
    let mut style_parts = vec!["display:flex".to_string()];

    let direction = direction
        .and_then(extract_tag_name)
        .map_or("column", |direction| {
            if direction.eq_ignore_ascii_case("Row") {
                "row"
            } else {
                "column"
            }
        });
    style_parts.push(format!("flex-direction:{direction}"));

    if let Some(gap) = gap.and_then(extract_number) {
        style_parts.push(format!("gap:{}px", trim_number(gap)));
    }

    merge_style_property(&mut properties, style_parts);

    finalize_element_node(
        SemanticNode::element_with_facts(
            infer_tag(element, "div"),
            None,
            properties,
            collect_event_bindings(element, current_binding, context),
            collect_fact_bindings(element),
            children,
        ),
        style,
        context,
        stack,
        locals,
        passed,
    )
}

fn lower_text_input<'a>(
    arguments: &'a [static_expression::Spanned<StaticArgument>],
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
    current_binding: Option<&str>,
) -> Result<SemanticNode, String> {
    let element = find_named_argument(arguments, "element");
    let text = find_named_argument(arguments, "text");
    let placeholder = find_named_argument(arguments, "placeholder");
    let focus = find_named_argument(arguments, "focus");
    let style = find_named_argument(arguments, "style");

    let mut properties = collect_common_properties(element);
    properties.push(("type".to_string(), "text".to_string()));
    let mut input_value = None;

    if let Some(text) = text {
        let value = lower_text_input_initial_value(text, context, stack, locals, passed)?;
        properties.push(("value".to_string(), value));
        input_value = lower_text_input_value_source(text, context, stack, locals, passed)?;
    }
    if let Some(placeholder) = placeholder.and_then(extract_placeholder_text) {
        properties.push(("placeholder".to_string(), placeholder));
    }
    if focus.map(extract_bool_literal_opt).transpose()? == Some(Some(true)) {
        properties.push(("autofocus".to_string(), "true".to_string()));
    }

    let node = SemanticNode::element_with_facts(
        infer_tag(element, "input"),
        None,
        properties,
        collect_event_bindings(element, current_binding, context),
        collect_fact_bindings(element),
        Vec::new(),
    );
    let node = match node {
        SemanticNode::Element {
            tag,
            text,
            properties,
            style_fragments,
            event_bindings,
            fact_bindings,
            children,
            ..
        } => SemanticNode::Element {
            tag,
            text,
            properties,
            input_value,
            style_fragments,
            event_bindings,
            fact_bindings,
            children,
        },
        _ => unreachable!(),
    };

    finalize_element_node(node, style, context, stack, locals, passed)
}

fn lower_checkbox<'a>(
    arguments: &'a [static_expression::Spanned<StaticArgument>],
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
    current_binding: Option<&str>,
) -> Result<SemanticNode, String> {
    let element = find_named_argument(arguments, "element");
    let icon = find_named_argument(arguments, "icon");
    let checked = find_named_argument(arguments, "checked");
    let style = find_named_argument(arguments, "style");
    let mut properties = collect_common_properties(element);
    if !properties.iter().any(|(name, _)| name == "type") {
        properties.push(("type".to_string(), "button".to_string()));
    }
    if !properties.iter().any(|(name, _)| name == "role") {
        properties.push(("role".to_string(), "checkbox".to_string()));
    }

    let child = if let Some(icon) = icon {
        lower_ui_node(icon, context, stack, locals, passed, None)?
    } else if let Some(checked) = checked {
        lower_bool_text_node(checked, context, stack, locals, passed)?
            .unwrap_or_else(|| SemanticNode::text("[ ]"))
    } else {
        SemanticNode::text("[ ]")
    };

    finalize_element_node(
        SemanticNode::element_with_facts(
            infer_tag(element, "button"),
            None,
            properties,
            collect_event_bindings(element, current_binding, context),
            collect_fact_bindings(element),
            vec![child],
        ),
        style,
        context,
        stack,
        locals,
        passed,
    )
}

fn finalize_element_node<'a>(
    node: SemanticNode,
    style: Option<&'a StaticSpannedExpression>,
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<SemanticNode, String> {
    apply_supported_style(node, style, context, stack, locals, passed)
}

fn apply_supported_style<'a>(
    mut node: SemanticNode,
    style: Option<&'a StaticSpannedExpression>,
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<SemanticNode, String> {
    let Some(style) = style else {
        return Ok(node);
    };

    if let Some(outline) = outline_style_fragment_expr(style, context, stack, locals, passed)? {
        node = push_style_fragment(node, outline);
    }
    if let Some(underline) = underline_style_fragment_expr(style, context, stack, locals, passed)? {
        node = push_style_fragment(node, underline);
    }
    if let Some(strikethrough) =
        strikethrough_style_fragment_expr(style, context, stack, locals, passed)?
    {
        node = push_style_fragment(node, strikethrough);
    }
    if let Some(color) = color_style_fragment_expr(style, context, stack, locals, passed)? {
        node = push_style_fragment(node, color);
    }
    if let Some(background) =
        background_url_style_fragment_expr(style, context, stack, locals, passed)?
    {
        node = push_style_fragment(node, background);
    }
    if let Some(visibility) = visible_style_fragment_expr(style, context, stack, locals, passed)? {
        node = push_style_fragment(node, visibility);
    }

    Ok(node)
}

fn push_style_fragment(node: SemanticNode, fragment: SemanticStyleFragment) -> SemanticNode {
    match node {
        SemanticNode::Element {
            tag,
            text,
            properties,
            input_value,
            mut style_fragments,
            event_bindings,
            fact_bindings,
            children,
        } => {
            style_fragments.push(fragment);
            SemanticNode::Element {
                tag,
                text,
                properties,
                input_value,
                style_fragments,
                event_bindings,
                fact_bindings,
                children,
            }
        }
        other => other,
    }
}

fn outline_style_fragment_expr<'a>(
    style: &'a StaticSpannedExpression,
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<Option<SemanticStyleFragment>, String> {
    let Some(style_object) = resolved_object(style, context, locals, passed, stack)? else {
        return Ok(None);
    };
    let Some(outline) = find_object_field(style_object, "outline") else {
        return Ok(None);
    };
    lower_outline_style_fragment(outline, context, stack, locals, passed)
}

fn lower_outline_style_fragment<'a>(
    expression: &'a StaticSpannedExpression,
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<Option<SemanticStyleFragment>, String> {
    let expression = resolve_alias(expression, context, locals, passed, stack)?;
    match &expression.node {
        StaticExpression::Block { variables, output } => {
            let mut scope = BTreeMap::new();
            for variable in variables {
                scope.insert(
                    variable.node.name.as_str().to_string(),
                    LocalBinding {
                        expr: Some(&variable.node.value),
                        object_base: None,
                    },
                );
            }
            locals.push(scope);
            let lowered = lower_outline_style_fragment(output, context, stack, locals, passed);
            locals.pop();
            lowered
        }
        StaticExpression::Literal(static_expression::Literal::Tag(tag))
            if tag.as_str() == "NoOutline" =>
        {
            Ok(Some(SemanticStyleFragment::Static(None)))
        }
        StaticExpression::Object(object) => Ok(outline_style_from_object(object)
            .map(Some)
            .map(SemanticStyleFragment::Static)),
        StaticExpression::Pipe { from, to } => {
            let arms = match &to.node {
                StaticExpression::While { arms } | StaticExpression::When { arms } => arms,
                _ => return Ok(None),
            };
            let Some((truthy, falsy)) = bool_ui_branch_arms(arms)? else {
                return Ok(None);
            };
            let Some(condition) = style_condition_fragment(from, context, locals, passed, stack)?
            else {
                return Ok(None);
            };
            let Some(truthy) =
                lower_outline_style_fragment(truthy, context, stack, locals, passed)?
            else {
                return Ok(None);
            };
            let Some(falsy) = lower_outline_style_fragment(falsy, context, stack, locals, passed)?
            else {
                return Ok(None);
            };
            Ok(Some(wrap_style_condition(condition, truthy, falsy)))
        }
        _ => Ok(None),
    }
}

fn underline_style_fragment_expr<'a>(
    style: &'a StaticSpannedExpression,
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<Option<SemanticStyleFragment>, String> {
    let Some(style_object) = resolved_object(style, context, locals, passed, stack)? else {
        return Ok(None);
    };
    let Some(font_object) = find_object_field(style_object, "font").and_then(resolve_object) else {
        return Ok(None);
    };
    let Some(line_object) = find_object_field(font_object, "line").and_then(resolve_object) else {
        return Ok(None);
    };
    let Some(underline) = find_object_field(line_object, "underline") else {
        return Ok(None);
    };
    lower_boolean_style_fragment(
        underline,
        "text-decoration:underline",
        None,
        context,
        stack,
        locals,
        passed,
    )
}

fn strikethrough_style_fragment_expr<'a>(
    style: &'a StaticSpannedExpression,
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<Option<SemanticStyleFragment>, String> {
    let Some(style_object) = resolved_object(style, context, locals, passed, stack)? else {
        return Ok(None);
    };
    let Some(font_object) = find_object_field(style_object, "font").and_then(resolve_object) else {
        return Ok(None);
    };
    let Some(line_object) = find_object_field(font_object, "line").and_then(resolve_object) else {
        return Ok(None);
    };
    let Some(strikethrough) = find_object_field(line_object, "strikethrough") else {
        return Ok(None);
    };
    lower_boolean_style_fragment(
        strikethrough,
        "text-decoration:line-through",
        None,
        context,
        stack,
        locals,
        passed,
    )
}

fn color_style_fragment_expr<'a>(
    style: &'a StaticSpannedExpression,
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<Option<SemanticStyleFragment>, String> {
    let Some(style_object) = resolved_object(style, context, locals, passed, stack)? else {
        return Ok(None);
    };
    let Some(font_object) = find_object_field(style_object, "font").and_then(resolve_object) else {
        return Ok(None);
    };
    let Some(color) = find_object_field(font_object, "color") else {
        return Ok(None);
    };
    lower_color_style_fragment(color, context, stack, locals, passed)
}

fn visible_style_fragment_expr<'a>(
    style: &'a StaticSpannedExpression,
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<Option<SemanticStyleFragment>, String> {
    let Some(style_object) = resolved_object(style, context, locals, passed, stack)? else {
        return Ok(None);
    };
    let Some(visible) = find_object_field(style_object, "visible") else {
        return Ok(None);
    };
    lower_boolean_style_fragment(
        visible,
        None,
        Some("display:none"),
        context,
        stack,
        locals,
        passed,
    )
}

fn background_url_style_fragment_expr<'a>(
    style: &'a StaticSpannedExpression,
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<Option<SemanticStyleFragment>, String> {
    let Some(style_object) = resolved_object(style, context, locals, passed, stack)? else {
        return Ok(None);
    };
    let Some(background_object) =
        find_object_field(style_object, "background").and_then(resolve_object)
    else {
        return Ok(None);
    };
    let Some(url) = find_object_field(background_object, "url") else {
        return Ok(None);
    };
    lower_background_url_style_fragment(url, context, stack, locals, passed)
}

fn lower_boolean_style_fragment<'a>(
    expression: &'a StaticSpannedExpression,
    truthy: impl Into<Option<&'static str>>,
    falsy: impl Into<Option<&'static str>>,
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<Option<SemanticStyleFragment>, String> {
    if let StaticExpression::Block { variables, output } = &expression.node {
        let mut scope = BTreeMap::new();
        for variable in variables {
            scope.insert(
                variable.node.name.as_str().to_string(),
                LocalBinding {
                    expr: Some(&variable.node.value),
                    object_base: None,
                },
            );
        }
        locals.push(scope);
        let lowered =
            lower_boolean_style_fragment(output, truthy, falsy, context, stack, locals, passed);
        locals.pop();
        return lowered;
    }
    let truthy = truthy.into().map(str::to_string);
    let falsy = falsy.into().map(str::to_string);
    if let Some(value) = extract_bool_literal_opt(expression)? {
        return Ok(Some(SemanticStyleFragment::Static(if value {
            truthy
        } else {
            falsy
        })));
    }
    if let Some(condition) = style_condition_fragment(expression, context, locals, passed, stack)? {
        return Ok(Some(wrap_style_condition(
            condition,
            SemanticStyleFragment::Static(truthy),
            SemanticStyleFragment::Static(falsy),
        )));
    }
    Ok(None)
}

fn lower_background_url_style_fragment<'a>(
    expression: &'a StaticSpannedExpression,
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<Option<SemanticStyleFragment>, String> {
    if let StaticExpression::Block { variables, output } = &expression.node {
        let mut scope = BTreeMap::new();
        for variable in variables {
            scope.insert(
                variable.node.name.as_str().to_string(),
                LocalBinding {
                    expr: Some(&variable.node.value),
                    object_base: None,
                },
            );
        }
        locals.push(scope);
        let lowered = lower_background_url_style_fragment(output, context, stack, locals, passed);
        locals.pop();
        return lowered;
    }
    let expression = resolve_alias(expression, context, locals, passed, stack)?;
    if let StaticExpression::Pipe { from, to } = &expression.node {
        let arms = match &to.node {
            StaticExpression::While { arms } | StaticExpression::When { arms } => Some(arms),
            _ => None,
        };
        if let Some(arms) = arms {
            if let Some((truthy, falsy)) = bool_ui_branch_arms(arms)? {
                if let Some(condition) =
                    style_condition_fragment(from, context, locals, passed, stack)?
                {
                    let Some(truthy) =
                        lower_background_url_style_fragment(truthy, context, stack, locals, passed)?
                    else {
                        return Ok(None);
                    };
                    let Some(falsy) =
                        lower_background_url_style_fragment(falsy, context, stack, locals, passed)?
                    else {
                        return Ok(None);
                    };
                    return Ok(Some(wrap_style_condition(condition, truthy, falsy)));
                }
            }
        }
    }
    if let Ok(url) = lower_text_value(expression, context, stack, locals, passed) {
        return Ok(Some(SemanticStyleFragment::Static(Some(format!(
            "background-image:url({url})"
        )))));
    }
    Ok(None)
}

enum StyleConditionFragment {
    BoolBinding(String),
    ScalarCompare {
        left: DerivedScalarOperand,
        op: IntCompareOp,
        right: DerivedScalarOperand,
    },
    ObjectBoolField(String),
}

fn style_condition_fragment(
    expression: &StaticSpannedExpression,
    context: &LowerContext<'_>,
    locals: &LocalScopes<'_>,
    passed: &PassedScopes,
    stack: &mut Vec<String>,
) -> Result<Option<StyleConditionFragment>, String> {
    if let Some(field) = placeholder_object_field(expression, context, locals, passed)? {
        return Ok(Some(StyleConditionFragment::ObjectBoolField(field)));
    }
    if let Some(binding) = bool_binding_path(expression, context, locals, passed, stack)? {
        return Ok(Some(StyleConditionFragment::BoolBinding(binding)));
    }
    if let Some((left, op, right)) =
        scalar_compare_branch_operands(expression, context, locals, passed, stack)?
    {
        return Ok(Some(StyleConditionFragment::ScalarCompare {
            left,
            op,
            right,
        }));
    }
    Ok(None)
}

fn wrap_style_condition(
    condition: StyleConditionFragment,
    truthy: SemanticStyleFragment,
    falsy: SemanticStyleFragment,
) -> SemanticStyleFragment {
    match condition {
        StyleConditionFragment::BoolBinding(binding) => SemanticStyleFragment::BoolBinding {
            binding,
            truthy: Box::new(truthy),
            falsy: Box::new(falsy),
        },
        StyleConditionFragment::ScalarCompare { left, op, right } => {
            SemanticStyleFragment::ScalarCompare {
                left,
                op,
                right,
                truthy: Box::new(truthy),
                falsy: Box::new(falsy),
            }
        }
        StyleConditionFragment::ObjectBoolField(field) => SemanticStyleFragment::ObjectBoolField {
            field,
            truthy: Box::new(truthy),
            falsy: Box::new(falsy),
        },
    }
}

fn lower_color_style_fragment<'a>(
    expression: &'a StaticSpannedExpression,
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<Option<SemanticStyleFragment>, String> {
    if let StaticExpression::Block { variables, output } = &expression.node {
        let mut scope = BTreeMap::new();
        for variable in variables {
            scope.insert(
                variable.node.name.as_str().to_string(),
                LocalBinding {
                    expr: Some(&variable.node.value),
                    object_base: None,
                },
            );
        }
        locals.push(scope);
        let lowered = lower_color_style_fragment(output, context, stack, locals, passed);
        locals.pop();
        return lowered;
    }
    let expression = resolve_alias(expression, context, locals, passed, stack)?;
    if let Some(color) = static_css_color(expression) {
        return Ok(Some(SemanticStyleFragment::Static(Some(format!(
            "color:{color}"
        )))));
    }
    if let Some(fragment) =
        lower_dynamic_oklch_color_style_fragment(expression, context, stack, locals, passed)?
    {
        return Ok(Some(fragment));
    }
    let StaticExpression::Pipe { from, to } = &expression.node else {
        return Ok(None);
    };
    let arms = match &to.node {
        StaticExpression::While { arms } | StaticExpression::When { arms } => arms,
        _ => return Ok(None),
    };
    let Some((truthy, falsy)) = bool_ui_branch_arms(arms)? else {
        return Ok(None);
    };
    let Some(condition) = style_condition_fragment(from, context, locals, passed, stack)? else {
        return Ok(None);
    };
    let Some(truthy) = lower_color_style_fragment(truthy, context, stack, locals, passed)? else {
        return Ok(None);
    };
    let Some(falsy) = lower_color_style_fragment(falsy, context, stack, locals, passed)? else {
        return Ok(None);
    };
    Ok(Some(wrap_style_condition(condition, truthy, falsy)))
}

fn lower_dynamic_oklch_color_style_fragment<'a>(
    expression: &'a StaticSpannedExpression,
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<Option<SemanticStyleFragment>, String> {
    let StaticExpression::TaggedObject { tag, object } = &expression.node else {
        return Ok(None);
    };
    if tag.as_str() != "Oklch" {
        return Ok(None);
    }
    for field_name in ["lightness", "chroma", "hue", "alpha"] {
        let Some(field_expression) = find_object_field(object, field_name) else {
            continue;
        };
        let field_expression = resolve_alias(field_expression, context, locals, passed, stack)?;
        if extract_number(field_expression).is_some() {
            continue;
        }
        let StaticExpression::Pipe { from, to } = &field_expression.node else {
            return Ok(None);
        };
        let arms = match &to.node {
            StaticExpression::While { arms } | StaticExpression::When { arms } => arms,
            _ => return Ok(None),
        };
        let Some((truthy, falsy)) = bool_ui_branch_arms(arms)? else {
            return Ok(None);
        };
        let Some(condition) = style_condition_fragment(from, context, locals, passed, stack)?
        else {
            return Ok(None);
        };
        let Some(truthy) = resolved_style_number(truthy, context, stack, locals, passed)? else {
            return Ok(None);
        };
        let Some(falsy) = resolved_style_number(falsy, context, stack, locals, passed)? else {
            return Ok(None);
        };
        let Some(truthy_css) =
            build_oklch_css_color(object, field_name, truthy, context, stack, locals, passed)?
        else {
            return Ok(None);
        };
        let Some(falsy_css) =
            build_oklch_css_color(object, field_name, falsy, context, stack, locals, passed)?
        else {
            return Ok(None);
        };
        return Ok(Some(wrap_style_condition(
            condition,
            SemanticStyleFragment::Static(Some(format!("color:{truthy_css}"))),
            SemanticStyleFragment::Static(Some(format!("color:{falsy_css}"))),
        )));
    }
    Ok(None)
}

fn build_oklch_css_color<'a>(
    object: &'a StaticObject,
    override_field: &str,
    override_value: f64,
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<Option<String>, String> {
    let lightness = resolve_oklch_component(
        object,
        "lightness",
        0.5,
        override_field,
        override_value,
        context,
        stack,
        locals,
        passed,
    )?;
    let chroma = resolve_oklch_component(
        object,
        "chroma",
        0.0,
        override_field,
        override_value,
        context,
        stack,
        locals,
        passed,
    )?;
    let hue = resolve_oklch_component(
        object,
        "hue",
        0.0,
        override_field,
        override_value,
        context,
        stack,
        locals,
        passed,
    )?;
    let alpha = resolve_oklch_component(
        object,
        "alpha",
        1.0,
        override_field,
        override_value,
        context,
        stack,
        locals,
        passed,
    )?;
    Ok(Some(format_oklch_color(lightness, chroma, hue, alpha)))
}

fn resolve_oklch_component<'a>(
    object: &'a StaticObject,
    field_name: &str,
    default: f64,
    override_field: &str,
    override_value: f64,
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<f64, String> {
    if field_name == override_field {
        return Ok(override_value);
    }
    let Some(expression) = find_object_field(object, field_name) else {
        return Ok(default);
    };
    resolved_style_number(expression, context, stack, locals, passed)?
        .ok_or_else(|| format!("dynamic Oklch `{field_name}` style field is not supported yet"))
}

fn resolved_style_number<'a>(
    expression: &'a StaticSpannedExpression,
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<Option<f64>, String> {
    let expression = resolve_alias(expression, context, locals, passed, stack)?;
    Ok(extract_number(expression))
}

fn format_oklch_color(lightness: f64, chroma: f64, hue: f64, alpha: f64) -> String {
    if alpha < 1.0 {
        format!(
            "oklch({}% {} {} / {})",
            lightness * 100.0,
            chroma,
            hue,
            alpha
        )
    } else {
        format!("oklch({}% {} {})", lightness * 100.0, chroma, hue)
    }
}

fn resolved_object<'a>(
    expression: &'a StaticSpannedExpression,
    context: &'a LowerContext<'a>,
    locals: &LocalScopes<'a>,
    passed: &PassedScopes,
    stack: &mut Vec<String>,
) -> Result<Option<&'a StaticObject>, String> {
    Ok(resolve_object(resolve_alias(
        expression, context, locals, passed, stack,
    )?))
}

fn outline_style_from_object(object: &StaticObject) -> Option<String> {
    let color = find_object_field(object, "color")
        .and_then(static_css_color)
        .unwrap_or_else(|| "currentColor".to_string());
    let mut parts = vec![format!("outline:1px solid {color}")];
    if find_object_field(object, "side")
        .and_then(extract_tag_name)
        .is_some_and(|side| side.eq_ignore_ascii_case("Inner"))
    {
        parts.push("outline-offset:-1px".to_string());
    }
    Some(parts.join(";"))
}

fn static_css_color(expression: &StaticSpannedExpression) -> Option<String> {
    match &expression.node {
        StaticExpression::Literal(static_expression::Literal::Tag(tag))
        | StaticExpression::Literal(static_expression::Literal::Text(tag)) => {
            static_named_color(tag.as_str()).map(ToString::to_string)
        }
        StaticExpression::TaggedObject { tag, object } if tag.as_str() == "Oklch" => {
            let lightness = static_oklch_component(object, "lightness", 0.5)?;
            let chroma = static_oklch_component(object, "chroma", 0.0)?;
            let hue = static_oklch_component(object, "hue", 0.0)?;
            let alpha = static_oklch_component(object, "alpha", 1.0)?;
            Some(format_oklch_color(lightness, chroma, hue, alpha))
        }
        _ => None,
    }
}

fn static_oklch_component(object: &StaticObject, field_name: &str, default: f64) -> Option<f64> {
    let Some(expression) = find_object_field(object, field_name) else {
        return Some(default);
    };
    extract_number(expression)
}

fn static_named_color(name: &str) -> Option<&'static str> {
    match name {
        "White" => Some("white"),
        "Black" => Some("black"),
        "Red" => Some("red"),
        "Green" => Some("green"),
        "Blue" => Some("blue"),
        "Yellow" => Some("yellow"),
        "Cyan" => Some("cyan"),
        "Magenta" => Some("magenta"),
        "Orange" => Some("orange"),
        "Purple" => Some("purple"),
        "Pink" => Some("pink"),
        "Brown" => Some("brown"),
        "Gray" | "Grey" => Some("gray"),
        "Transparent" => Some("transparent"),
        _ => None,
    }
}

fn lower_text_input_initial_value<'a>(
    expression: &'a StaticSpannedExpression,
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<String, String> {
    if let Ok(value) = lower_text_value(expression, context, stack, locals, passed) {
        return Ok(value);
    }
    if placeholder_object_field(expression, context, locals, passed)?.is_some() {
        return Ok(String::new());
    }
    if let Some(binding_name) = alias_binding_name(expression)? {
        let resolved = resolve_named_binding(binding_name, context, locals, stack)?;
        let result = lower_text_input_initial_value(resolved, context, stack, locals, passed);
        stack.pop();
        return result;
    }

    let expression = resolve_alias(expression, context, locals, passed, stack)?;
    match &expression.node {
        StaticExpression::Block { variables, output } => {
            let scope = eager_block_scope(variables, context, stack, locals, passed);
            locals.push(scope);
            let lowered = lower_text_input_initial_value(output, context, stack, locals, passed);
            locals.pop();
            lowered
        }
        StaticExpression::FunctionCall { path, arguments }
            if path.len() == 1 && context.functions.contains_key(path[0].as_str()) =>
        {
            with_invoked_function_scope(
                path[0].as_str(),
                arguments,
                None,
                context,
                stack,
                locals,
                passed,
                |body, context, stack, locals, passed| {
                    lower_text_input_initial_value(body, context, stack, locals, passed)
                },
            )
        }
        StaticExpression::Pipe { from, to }
            if function_invocation_target(to, context, locals, passed, stack)?.is_some() =>
        {
            let Some((function_name, arguments)) =
                function_invocation_target(to, context, locals, passed, stack)?
            else {
                unreachable!();
            };
            with_invoked_function_scope(
                function_name,
                arguments,
                Some(from.as_ref()),
                context,
                stack,
                locals,
                passed,
                |body, context, stack, locals, passed| {
                    lower_text_input_initial_value(body, context, stack, locals, passed)
                },
            )
        }
        StaticExpression::Pipe { from, to }
            if matches!(&to.node, StaticExpression::When { .. }) =>
        {
            let StaticExpression::When { arms } = &to.node else {
                unreachable!();
            };
            let Some(body) = select_when_arm_body(from, arms, context, stack, locals, passed)?
            else {
                return Err("dynamic text_input text must select an initial branch".to_string());
            };
            if let Ok(value) = lower_text_value(body, context, stack, locals, passed) {
                Ok(value)
            } else if placeholder_object_field(body, context, locals, passed)?.is_some() {
                Ok(String::new())
            } else {
                lower_text_input_initial_value(body, context, stack, locals, passed)
            }
        }
        StaticExpression::Latest { inputs } => {
            for input in inputs {
                if let Some(value) = text_empty_literal(input)? {
                    return Ok(value);
                }
                if let Ok(value) = lower_text_value(input, context, stack, locals, passed) {
                    return Ok(value);
                }
                if placeholder_object_field(input, context, locals, passed)?.is_some() {
                    return Ok(String::new());
                }
            }
            Err("dynamic text_input text must include Text/empty() seed".to_string())
        }
        StaticExpression::Pipe { from, to }
            if matches!(&to.node, StaticExpression::Hold { .. }) =>
        {
            if let Ok(value) = lower_text_value(from, context, stack, locals, passed) {
                Ok(value)
            } else if placeholder_object_field(from, context, locals, passed)?.is_some() {
                Ok(String::new())
            } else {
                Err(format!(
                    "Wasm Pro parser-backed lowering does not support text_input text expression `{}` yet",
                    describe_expression_detailed(expression)
                ))
            }
        }
        _ => Err(format!(
            "Wasm Pro parser-backed lowering does not support text_input text expression `{}` yet",
            describe_expression_detailed(expression)
        )),
    }
}

fn lower_text_input_value_source<'a>(
    expression: &'a StaticSpannedExpression,
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<Option<SemanticInputValue>, String> {
    if let Some((binding, value)) =
        resolve_text_reference(expression, context, locals, passed, stack)?
    {
        return Ok(Some(SemanticInputValue::TextParts {
            parts: vec![SemanticTextPart::TextBinding(binding)],
            value,
        }));
    }
    if let Some(field) = placeholder_object_field(expression, context, locals, passed)? {
        return Ok(Some(SemanticInputValue::TextParts {
            parts: vec![SemanticTextPart::ObjectFieldBinding(field)],
            value: String::new(),
        }));
    }
    if let Some(branch) =
        lower_text_input_branch_source(expression, context, stack, locals, passed)?
    {
        return Ok(Some(branch));
    }
    let expression = resolve_alias(expression, context, locals, passed, stack)?;
    match &expression.node {
        StaticExpression::TextLiteral { parts, .. } => {
            let lowered = lower_text_parts(parts, context, stack, locals, passed)?;
            let value = render_semantic_text_parts(&lowered, context);
            let has_dynamic = lowered.iter().any(|part| {
                matches!(
                    part,
                    SemanticTextPart::TextBinding(_)
                        | SemanticTextPart::ObjectFieldBinding(_)
                        | SemanticTextPart::ScalarBinding(_)
                        | SemanticTextPart::ListCountBinding(_)
                        | SemanticTextPart::ObjectListCountBinding(_)
                        | SemanticTextPart::FilteredListCountBinding { .. }
                        | SemanticTextPart::FilteredObjectListCountBinding { .. }
                        | SemanticTextPart::BoolBindingText { .. }
                        | SemanticTextPart::ObjectBoolFieldText { .. }
                )
            });
            if has_dynamic {
                Ok(Some(SemanticInputValue::TextParts {
                    parts: lowered,
                    value,
                }))
            } else {
                Ok(None)
            }
        }
        StaticExpression::Block { variables, output } => {
            let scope = eager_block_scope(variables, context, stack, locals, passed);
            locals.push(scope);
            let lowered = lower_text_input_value_source(output, context, stack, locals, passed);
            locals.pop();
            lowered
        }
        StaticExpression::FunctionCall { path, arguments }
            if path.len() == 1 && context.functions.contains_key(path[0].as_str()) =>
        {
            with_invoked_function_scope(
                path[0].as_str(),
                arguments,
                None,
                context,
                stack,
                locals,
                passed,
                |body, context, stack, locals, passed| {
                    lower_text_input_value_source(body, context, stack, locals, passed)
                },
            )
        }
        StaticExpression::Pipe { from, to }
            if function_invocation_target(to, context, locals, passed, stack)?.is_some() =>
        {
            let Some((function_name, arguments)) =
                function_invocation_target(to, context, locals, passed, stack)?
            else {
                unreachable!();
            };
            with_invoked_function_scope(
                function_name,
                arguments,
                Some(from.as_ref()),
                context,
                stack,
                locals,
                passed,
                |body, context, stack, locals, passed| {
                    lower_text_input_value_source(body, context, stack, locals, passed)
                },
            )
        }
        _ => Ok(None),
    }
}

fn lower_text_input_branch_source<'a>(
    expression: &'a StaticSpannedExpression,
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<Option<SemanticInputValue>, String> {
    let expression = resolve_alias(expression, context, locals, passed, stack)?;
    let StaticExpression::Pipe { from, to } = &expression.node else {
        return Ok(None);
    };
    let (StaticExpression::When { arms } | StaticExpression::While { arms }) = &to.node else {
        return Ok(None);
    };
    let Some((truthy_body, falsy_body)) = bool_condition_arm_bodies(arms) else {
        return Ok(None);
    };
    let truthy = lower_text_input_value_source(truthy_body, context, stack, locals, passed)?
        .unwrap_or(SemanticInputValue::Static(lower_text_input_initial_value(
            truthy_body, context, stack, locals, passed,
        )?));
    let falsy = lower_text_input_value_source(falsy_body, context, stack, locals, passed)?
        .unwrap_or(SemanticInputValue::Static(lower_text_input_initial_value(
            falsy_body, context, stack, locals, passed,
        )?));

    if let Some((binding, invert)) =
        text_empty_branch_condition(from, context, locals, passed, stack)?
    {
        return Ok(Some(SemanticInputValue::TextBindingBranch {
            binding,
            invert,
            truthy: Box::new(truthy),
            falsy: Box::new(falsy),
        }));
    }
    if let Some((field, invert)) =
        object_text_empty_branch_condition(from, context, locals, passed, stack)?
    {
        return Ok(Some(SemanticInputValue::ObjectTextFieldBranch {
            field,
            invert,
            truthy: Box::new(truthy),
            falsy: Box::new(falsy),
        }));
    }
    Ok(None)
}

fn lower_list_items<'a>(
    expression: &'a StaticSpannedExpression,
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<Vec<SemanticNode>, String> {
    if let Some(dynamic) = lower_dynamic_list_items(expression, context, stack, locals, passed)? {
        return Ok(dynamic);
    }

    let expression = resolve_alias(expression, context, locals, passed, stack)?;
    match &expression.node {
        StaticExpression::Pipe { from, to } => {
            let StaticExpression::FunctionCall { path, arguments } = &to.node else {
                return Err("Element/stripe `items` must be a static LIST".to_string());
            };
            if path_matches(path, &["List", "map"]) {
                let mapper_name = find_positional_parameter_name(arguments)
                    .ok_or_else(|| "List/map requires an item parameter name".to_string())?;
                let new = find_named_argument(arguments, "new")
                    .ok_or_else(|| "List/map requires `new`".to_string())?;
                let items = resolve_static_list_items(from, context, stack, locals, passed)?;
                let item_bases =
                    resolve_static_object_item_bases(from, context, stack, locals, passed)?;
                let mut nodes = Vec::with_capacity(items.len());
                for (index, item) in items.into_iter().enumerate() {
                    let mut scope = BTreeMap::new();
                    let object_base = item_bases
                        .as_ref()
                        .and_then(|bases| bases.get(index))
                        .cloned();
                    scope.insert(
                        mapper_name.to_string(),
                        LocalBinding {
                            expr: Some(item),
                            object_base: object_base.clone(),
                        },
                    );
                    locals.push(scope);
                    let lowered = lower_ui_node(new, context, stack, locals, passed, None);
                    locals.pop();
                    let lowered = lowered?;
                    nodes.push(
                        object_base
                            .filter(|base| {
                                context
                                    .scalar_plan
                                    .initial_values
                                    .contains_key(&format!("{base}.__removed"))
                            })
                            .map_or(lowered.clone(), |base| {
                                SemanticNode::bool_branch(
                                    format!("{base}.__removed"),
                                    SemanticNode::Fragment(Vec::new()),
                                    lowered,
                                )
                            }),
                    );
                }
                Ok(nodes)
            } else {
                resolve_static_list_items(expression, context, stack, locals, passed)?
                    .into_iter()
                    .map(|item| lower_ui_node(item, context, stack, locals, passed, None))
                    .collect()
            }
        }
        _ => resolve_static_list_items(expression, context, stack, locals, passed)?
            .into_iter()
            .map(|item| lower_ui_node(item, context, stack, locals, passed, None))
            .collect(),
    }
}

fn lower_dynamic_list_items<'a>(
    expression: &'a StaticSpannedExpression,
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<Option<Vec<SemanticNode>>, String> {
    if let Some((function_name, arguments)) =
        function_invocation_target(expression, context, locals, passed, stack)?
    {
        return with_invoked_function_scope(
            function_name,
            arguments,
            None,
            context,
            stack,
            locals,
            passed,
            |body, context, stack, locals, passed| {
                lower_dynamic_list_items(body, context, stack, locals, passed)
            },
        );
    }
    let expression = resolve_alias(expression, context, locals, passed, stack)?;
    let StaticExpression::Pipe { from, to } = &expression.node else {
        return Ok(None);
    };
    let StaticExpression::FunctionCall { path, arguments } = &to.node else {
        return Ok(None);
    };
    if !path_matches(path, &["List", "map"]) {
        return Ok(None);
    }
    let mapper_name = find_positional_parameter_name(arguments)
        .ok_or_else(|| "List/map requires an item parameter name".to_string())?;
    let new = find_named_argument(arguments, "new")
        .ok_or_else(|| "List/map requires `new`".to_string())?;
    if let Some(list_ref) = runtime_object_list_ref(from, context, locals, passed, stack)? {
        let mut scope = BTreeMap::new();
        scope.insert(
            mapper_name.to_string(),
            LocalBinding {
                expr: None,
                object_base: Some("__item__".to_string()),
            },
        );
        locals.push(scope);
        let template = lower_ui_node(new, context, stack, locals, passed, None);
        locals.pop();
        return Ok(Some(vec![SemanticNode::object_list(
            list_ref.binding.clone(),
            list_ref.filter,
            context
                .object_list_plan
                .item_actions
                .get(&list_ref.binding)
                .cloned()
                .unwrap_or_default(),
            template?,
        )]));
    }
    let Some(list_ref) = runtime_text_list_ref(from, context, locals, passed, stack)? else {
        return Ok(None);
    };
    let template = lower_text_list_template(
        new,
        mapper_name,
        &list_ref.binding,
        context,
        stack,
        locals,
        passed,
    )?;
    Ok(Some(vec![SemanticNode::text_list(
        list_ref.binding.clone(),
        filtered_runtime_text_list_values(
            context
                .list_plan
                .initial_values
                .get(&list_ref.binding)
                .map(Vec::as_slice)
                .unwrap_or(&[]),
            list_ref.filter.as_ref(),
        ),
        list_ref.filter.clone(),
        template,
    )]))
}

fn resolve_static_object_item_bases<'a>(
    expression: &'a StaticSpannedExpression,
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<Option<Vec<String>>, String> {
    if let Ok(path) = canonical_expression_path(expression, context, locals, passed, stack) {
        if let Some(bases) = context.static_object_lists.item_bases.get(&path) {
            return Ok(Some(bases.clone()));
        }
    }

    let expression = resolve_alias(expression, context, locals, passed, stack)?;
    let StaticExpression::Pipe { from, to } = &expression.node else {
        return Ok(None);
    };
    let StaticExpression::FunctionCall { path, arguments } = &to.node else {
        return Ok(None);
    };

    if path_matches(path, &["List", "remove"]) {
        return resolve_static_object_item_bases(from, context, stack, locals, passed);
    }

    if path_matches(path, &["List", "retain"]) {
        let Some(mut bases) =
            resolve_static_object_item_bases(from, context, stack, locals, passed)?
        else {
            return Ok(None);
        };
        let predicate_name = find_positional_parameter_name(arguments)
            .ok_or_else(|| "List/retain requires an item parameter name".to_string())?;
        let condition = find_named_argument(arguments, "if")
            .ok_or_else(|| "List/retain requires `if`".to_string())?;
        let items = resolve_static_list_items(from, context, stack, locals, passed)?;
        let mut retained = Vec::new();
        for (item, base) in items.into_iter().zip(bases.drain(..)) {
            let mut scope = BTreeMap::new();
            scope.insert(
                predicate_name.to_string(),
                LocalBinding {
                    expr: Some(item),
                    object_base: Some(base.clone()),
                },
            );
            locals.push(scope);
            let keep = eval_static_bool(condition, context, stack, locals, passed);
            locals.pop();
            if keep? {
                retained.push(base);
            }
        }
        return Ok(Some(retained));
    }

    Ok(None)
}

fn lower_text_value<'a>(
    expression: &'a StaticSpannedExpression,
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<String, String> {
    let cache_key = format!(
        "text:{}",
        expression_fingerprint(expression, context, locals, passed, &mut Vec::new())?
    );
    if let Some(value) = context.text_eval_cache.borrow().get(&cache_key).cloned() {
        return Ok(value);
    }
    {
        let mut in_progress = context.text_eval_in_progress.borrow_mut();
        if !in_progress.insert(cache_key.clone()) {
            return Ok(String::new());
        }
    }
    let result = lower_text_value_inner(expression, context, stack, locals, passed);
    context
        .text_eval_in_progress
        .borrow_mut()
        .remove(&cache_key);
    if let Ok(value) = &result {
        context
            .text_eval_cache
            .borrow_mut()
            .insert(cache_key, value.clone());
    }
    result
}

fn lower_text_value_inner<'a>(
    expression: &'a StaticSpannedExpression,
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<String, String> {
    if let Some(value) =
        initial_scalar_value_in_context(expression, context, stack, locals, passed)?
    {
        return Ok(value.to_string());
    }

    if let Some((_, value)) = resolve_scalar_reference(expression, context, locals, passed, stack)?
    {
        return Ok(value.to_string());
    }

    if let Some((_, value)) = resolve_text_reference(expression, context, locals, passed, stack)? {
        return Ok(value);
    }

    if let Some(binding_name) = alias_binding_name(expression)? {
        let resolved = resolve_named_binding(binding_name, context, locals, stack)?;
        let result = lower_text_value(resolved, context, stack, locals, passed);
        stack.pop();
        return result;
    }

    let expression = resolve_alias(expression, context, locals, passed, stack)?;
    match &expression.node {
        StaticExpression::PostfixFieldAccess { expr, field } => {
            if let Some(field_expression) = resolve_postfix_field_expression(
                expr,
                field,
                context,
                locals,
                passed,
                stack,
            )? {
                return lower_text_value(field_expression, context, stack, locals, passed);
            }
            Err(format!(
                "expected a static text value, found `{}`",
                describe_expression_detailed(expression)
            ))
        }
        StaticExpression::Pipe { from, to } => {
            if let StaticExpression::When { arms } | StaticExpression::While { arms } = &to.node {
                if let [arm] = arms.as_slice() {
                    if let static_expression::Pattern::Alias { name } = &arm.pattern {
                        let mut scope = BTreeMap::new();
                        scope.insert(
                            name.as_str().to_string(),
                            LocalBinding {
                                expr: Some(from),
                                object_base: infer_argument_object_base(
                                    from, context, locals, passed,
                                ),
                            },
                        );
                        locals.push(scope);
                        let lowered = lower_text_value(&arm.body, context, stack, locals, passed);
                        locals.pop();
                        return lowered;
                    }
                }
                if let Some(body) =
                    select_when_arm_body(from, arms, context, stack, locals, passed)?
                {
                    return lower_text_value(body, context, stack, locals, passed);
                }
                if let Some((name, body)) = conditional_alias_arm(arms) {
                    let mut scope = BTreeMap::new();
                    scope.insert(
                        name.to_string(),
                        LocalBinding {
                            expr: Some(from),
                            object_base: infer_argument_object_base(from, context, locals, passed),
                        },
                    );
                    locals.push(scope);
                    let lowered = lower_text_value(body, context, stack, locals, passed);
                    locals.pop();
                    return lowered;
                }
            }
            if let Some((function_name, arguments)) =
                function_invocation_target(to, context, locals, passed, stack)?
            {
                let marker = format!(
                    "eval-text-fn:{}",
                    invocation_marker(
                        function_name,
                        arguments,
                        Some(from.as_ref()),
                        context,
                        locals,
                        passed,
                    )?
                );
                if stack.iter().any(|entry| entry == &marker) {
                    return Ok(String::new());
                }
                stack.push(marker);
                return with_invoked_function_scope(
                    function_name,
                    arguments,
                    Some(from.as_ref()),
                    context,
                    stack,
                    locals,
                    passed,
                    |body, context, stack, locals, passed| {
                        lower_text_value(body, context, stack, locals, passed)
                    },
                )
                .inspect(|_| {
                    let _ = stack.pop();
                })
                .inspect_err(|_| {
                    let _ = stack.pop();
                });
            }
            let StaticExpression::FunctionCall { path, arguments } = &to.node else {
                return Err(format!(
                    "expected a static text value, found `{}`",
                    describe_expression_detailed(expression)
                ));
            };
            if path_matches(path, &["List", "count"]) && arguments.is_empty() {
                if let Some(count) = initial_list_count_value(from, context, stack, locals, passed)?
                {
                    return Ok(count.to_string());
                }
            }
            if path_matches(path, &["Text", "substring"]) {
                let source = lower_text_value(from, context, stack, locals, passed)?;
                let start = find_named_argument(arguments, "start")
                    .ok_or_else(|| "Text/substring requires `start`".to_string())
                    .and_then(|argument| {
                        initial_scalar_value_in_context(argument, context, stack, locals, passed)?
                            .ok_or_else(|| {
                                "Text/substring requires an initial integer `start`".to_string()
                            })
                    })? as usize;
                let length = find_named_argument(arguments, "length")
                    .ok_or_else(|| "Text/substring requires `length`".to_string())
                    .and_then(|argument| {
                        initial_scalar_value_in_context(argument, context, stack, locals, passed)?
                            .ok_or_else(|| {
                                "Text/substring requires an initial integer `length`".to_string()
                            })
                    })? as usize;
                let value = source.chars().skip(start).take(length).collect::<String>();
                return Ok(value);
            }
            if path_matches(path, &["Text", "trim"]) && arguments.is_empty() {
                let source = lower_text_value(from, context, stack, locals, passed)?;
                return Ok(source.trim().to_string());
            }
            Err(format!(
                "expected a static text value, found `{}`",
                describe_expression_detailed(expression)
            ))
        }
        StaticExpression::Literal(static_expression::Literal::Number(number)) => {
            Ok(trim_number(*number))
        }
        StaticExpression::Literal(static_expression::Literal::Text(text))
        | StaticExpression::Literal(static_expression::Literal::Tag(text)) => {
            Ok(text.as_str().to_string())
        }
        StaticExpression::FunctionCall { path, arguments }
            if path_matches(path, &["Text", "empty"]) && arguments.is_empty() =>
        {
            Ok(String::new())
        }
        StaticExpression::FunctionCall { path, arguments }
            if path_matches(path, &["Text", "space"]) && arguments.is_empty() =>
        {
            Ok(" ".to_string())
        }
        StaticExpression::TextLiteral { parts, .. } => {
            render_text_literal(parts, context, stack, locals, passed)
        }
        StaticExpression::Block { variables, output } => {
            let mut scope = BTreeMap::new();
            for variable in variables {
                scope.insert(
                    variable.node.name.as_str().to_string(),
                    LocalBinding {
                        expr: Some(&variable.node.value),
                        object_base: infer_argument_object_base(
                            &variable.node.value,
                            context,
                            locals,
                            passed,
                        ),
                    },
                );
            }
            locals.push(scope);
            let lowered = lower_text_value(output, context, stack, locals, passed);
            locals.pop();
            lowered
        }
        StaticExpression::FunctionCall { path, arguments }
            if path.len() == 1 && context.functions.contains_key(path[0].as_str()) =>
        {
            let function_name = path[0].as_str();
            let marker = format!(
                "eval-text-fn:{}",
                invocation_marker(function_name, arguments, None, context, locals, passed)?
            );
            if stack.iter().any(|entry| entry == &marker) {
                return Ok(String::new());
            }
            stack.push(marker);
            with_invoked_function_scope(
                function_name,
                arguments,
                None,
                context,
                stack,
                locals,
                passed,
                |body, context, stack, locals, passed| {
                    lower_text_value(body, context, stack, locals, passed)
                },
            )
            .inspect(|_| {
                let _ = stack.pop();
            })
            .inspect_err(|_| {
                let _ = stack.pop();
            })
        }
        StaticExpression::ArithmeticOperator(static_expression::ArithmeticOperator::Add {
            operand_a,
            operand_b,
        }) => Err(format!(
            "expected a static text value, found arithmetic operator `{} + {}`",
            describe_expression_detailed(operand_a),
            describe_expression_detailed(operand_b)
        )),
        StaticExpression::ArithmeticOperator(static_expression::ArithmeticOperator::Subtract {
            operand_a,
            operand_b,
        }) => Err(format!(
            "expected a static text value, found arithmetic operator `{} - {}`",
            describe_expression_detailed(operand_a),
            describe_expression_detailed(operand_b)
        )),
        _ => Err(format!(
            "expected a static text value, found `{}`",
            describe_expression_detailed(expression)
        )),
    }
}

fn initial_scalar_value_in_context<'a>(
    expression: &'a StaticSpannedExpression,
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<Option<i64>, String> {
    let cache_key = format!(
        "scalar:{}",
        expression_fingerprint(expression, context, locals, passed, &mut Vec::new())?
    );
    if let Some(value) = context.scalar_eval_cache.borrow().get(&cache_key).copied() {
        return Ok(value);
    }
    {
        let mut in_progress = context.scalar_eval_in_progress.borrow_mut();
        if !in_progress.insert(cache_key.clone()) {
            return Ok(None);
        }
    }
    let result = initial_scalar_value_in_context_inner(expression, context, stack, locals, passed);
    context
        .scalar_eval_in_progress
        .borrow_mut()
        .remove(&cache_key);
    if let Ok(value) = result {
        context
            .scalar_eval_cache
            .borrow_mut()
            .insert(cache_key, value);
        return Ok(value);
    }
    result
}

fn initial_scalar_value_in_context_inner<'a>(
    expression: &'a StaticSpannedExpression,
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<Option<i64>, String> {
    if let Some((_, value)) = resolve_scalar_reference(expression, context, locals, passed, stack)?
    {
        return Ok(Some(value));
    }

    let expression = resolve_alias(expression, context, locals, passed, stack)?;
    match &expression.node {
        StaticExpression::PostfixFieldAccess { expr, field } => {
            if let Some(field_expression) = resolve_postfix_field_expression(
                expr,
                field,
                context,
                locals,
                passed,
                stack,
            )? {
                return initial_scalar_value_in_context(
                    field_expression,
                    context,
                    stack,
                    locals,
                    passed,
                );
            }
            Ok(None)
        }
        StaticExpression::Block { variables, output } => {
            let scope = eager_block_scope(variables, context, stack, locals, passed);
            locals.push(scope);
            let lowered = initial_scalar_value_in_context(output, context, stack, locals, passed);
            locals.pop();
            lowered
        }
        StaticExpression::Literal(static_expression::Literal::Number(_)) => {
            extract_integer_literal_opt(expression)
        }
        StaticExpression::Literal(static_expression::Literal::Tag(tag))
            if tag.as_str() == "True" =>
        {
            Ok(Some(1))
        }
        StaticExpression::Literal(static_expression::Literal::Tag(tag))
            if tag.as_str() == "False" =>
        {
            Ok(Some(0))
        }
        StaticExpression::Literal(static_expression::Literal::Tag(_)) => {
            extract_filter_tag_value(expression)
        }
        StaticExpression::ArithmeticOperator(static_expression::ArithmeticOperator::Add {
            operand_a,
            operand_b,
        }) => Ok(
            initial_scalar_value_in_context(operand_a, context, stack, locals, passed)?
                .zip(initial_scalar_value_in_context(
                    operand_b, context, stack, locals, passed,
                )?)
                .map(|(a, b)| a + b),
        ),
        StaticExpression::ArithmeticOperator(static_expression::ArithmeticOperator::Subtract {
            operand_a,
            operand_b,
        }) => Ok(
            initial_scalar_value_in_context(operand_a, context, stack, locals, passed)?
                .zip(initial_scalar_value_in_context(
                    operand_b, context, stack, locals, passed,
                )?)
                .map(|(a, b)| a - b),
        ),
        StaticExpression::Comparator(comparator) => {
            let (operand_a, operand_b, op) = match comparator {
                static_expression::Comparator::Equal {
                    operand_a,
                    operand_b,
                } => (operand_a.as_ref(), operand_b.as_ref(), IntCompareOp::Equal),
                static_expression::Comparator::NotEqual {
                    operand_a,
                    operand_b,
                } => (
                    operand_a.as_ref(),
                    operand_b.as_ref(),
                    IntCompareOp::NotEqual,
                ),
                static_expression::Comparator::Greater {
                    operand_a,
                    operand_b,
                } => (
                    operand_a.as_ref(),
                    operand_b.as_ref(),
                    IntCompareOp::Greater,
                ),
                static_expression::Comparator::GreaterOrEqual {
                    operand_a,
                    operand_b,
                } => (
                    operand_a.as_ref(),
                    operand_b.as_ref(),
                    IntCompareOp::GreaterOrEqual,
                ),
                static_expression::Comparator::Less {
                    operand_a,
                    operand_b,
                } => (operand_a.as_ref(), operand_b.as_ref(), IntCompareOp::Less),
                static_expression::Comparator::LessOrEqual {
                    operand_a,
                    operand_b,
                } => (
                    operand_a.as_ref(),
                    operand_b.as_ref(),
                    IntCompareOp::LessOrEqual,
                ),
            };
            Ok(
                initial_scalar_value_in_context(operand_a, context, stack, locals, passed)?
                    .zip(initial_scalar_value_in_context(
                        operand_b, context, stack, locals, passed,
                    )?)
                    .map(|(a, b)| {
                        i64::from(match op {
                            IntCompareOp::Equal => a == b,
                            IntCompareOp::NotEqual => a != b,
                            IntCompareOp::Greater => a > b,
                            IntCompareOp::GreaterOrEqual => a >= b,
                            IntCompareOp::Less => a < b,
                            IntCompareOp::LessOrEqual => a <= b,
                        })
                    }),
            )
        }
        StaticExpression::FunctionCall { path, arguments }
            if path.len() == 1 && context.functions.contains_key(path[0].as_str()) =>
        {
            let function_name = path[0].as_str();
            let marker = format!(
                "eval-scalar-fn:{}",
                invocation_marker(function_name, arguments, None, context, locals, passed)?
            );
            if stack.iter().any(|entry| entry == &marker) {
                return Ok(None);
            }
            stack.push(marker);
            with_invoked_function_scope(
                function_name,
                arguments,
                None,
                context,
                stack,
                locals,
                passed,
                |body, context, stack, locals, passed| {
                    initial_scalar_value_in_context(body, context, stack, locals, passed)
                },
            )
            .inspect(|_| {
                let _ = stack.pop();
            })
            .inspect_err(|_| {
                let _ = stack.pop();
            })
        }
        StaticExpression::Pipe { from, to } => {
            if let StaticExpression::When { arms } | StaticExpression::While { arms } = &to.node {
                if let Some(body) =
                    select_when_arm_body(from, arms, context, stack, locals, passed)?
                {
                    return initial_scalar_value_in_context(body, context, stack, locals, passed);
                }
                if let Some((name, body)) = conditional_alias_arm(arms) {
                    let mut scope = BTreeMap::new();
                    scope.insert(
                        name.to_string(),
                        LocalBinding {
                            expr: Some(from),
                            object_base: infer_argument_object_base(from, context, locals, passed),
                        },
                    );
                    locals.push(scope);
                    let lowered =
                        initial_scalar_value_in_context(body, context, stack, locals, passed);
                    locals.pop();
                    return lowered;
                }
            }
            if let Some((function_name, arguments)) =
                function_invocation_target(to, context, locals, passed, stack)?
            {
                let marker = format!(
                    "eval-scalar-fn:{}",
                    invocation_marker(
                        function_name,
                        arguments,
                        Some(from.as_ref()),
                        context,
                        locals,
                        passed,
                    )?
                );
                if stack.iter().any(|entry| entry == &marker) {
                    return Ok(None);
                }
                stack.push(marker);
                return with_invoked_function_scope(
                    function_name,
                    arguments,
                    Some(from.as_ref()),
                    context,
                    stack,
                    locals,
                    passed,
                    |body, context, stack, locals, passed| {
                        initial_scalar_value_in_context(body, context, stack, locals, passed)
                    },
                )
                .inspect(|_| {
                    let _ = stack.pop();
                })
                .inspect_err(|_| {
                    let _ = stack.pop();
                });
            }
            let StaticExpression::FunctionCall { path, arguments } = &to.node else {
                return Ok(None);
            };
            if path_matches(path, &["List", "count"]) && arguments.is_empty() {
                return initial_list_count_value(from, context, stack, locals, passed);
            }
            if path_matches(path, &["Text", "length"]) && arguments.is_empty() {
                let source = lower_text_value(from, context, stack, locals, passed)?;
                return Ok(Some(source.chars().count() as i64));
            }
            if path_matches(path, &["Text", "find"]) {
                let source = lower_text_value(from, context, stack, locals, passed)?;
                let search = find_named_argument(arguments, "search")
                    .ok_or_else(|| "Text/find requires `search`".to_string())
                    .and_then(|search| lower_text_value(search, context, stack, locals, passed))?;
                return Ok(Some(
                    source
                        .find(search.as_str())
                        .map(|index| index as i64)
                        .unwrap_or(-1),
                ));
            }
            if path_matches(path, &["Text", "starts_with"]) {
                let source = lower_text_value(from, context, stack, locals, passed)?;
                let prefix = find_named_argument(arguments, "prefix")
                    .ok_or_else(|| "Text/starts_with requires `prefix`".to_string())
                    .and_then(|prefix| lower_text_value(prefix, context, stack, locals, passed))?;
                return Ok(Some(i64::from(source.starts_with(prefix.as_str()))));
            }
            if path_matches(path, &["Text", "to_number"]) && arguments.is_empty() {
                let source = lower_text_value(from, context, stack, locals, passed)?;
                return Ok(parse_static_number(source.trim()));
            }
            Ok(None)
        }
        _ => Ok(None),
    }
}

fn parse_static_number(text: &str) -> Option<i64> {
    let value = text.parse::<f64>().ok()?;
    if !value.is_finite() || value.fract() != 0.0 {
        return None;
    }
    Some(value as i64)
}

fn initial_list_count_value<'a>(
    expression: &'a StaticSpannedExpression,
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<Option<i64>, String> {
    if let Some(binding) =
        runtime_object_list_binding_path(expression, context, locals, passed, stack)?
    {
        return Ok(Some(
            context
                .object_list_plan
                .initial_values
                .get(&binding)
                .map_or(0, Vec::len) as i64,
        ));
    }
    if let Some(binding) = runtime_list_binding_path(expression, context, locals, passed, stack)? {
        return Ok(Some(
            context
                .list_plan
                .initial_values
                .get(&binding)
                .map_or(0, Vec::len) as i64,
        ));
    }
    if let Ok(items) = resolve_static_list_items(expression, context, stack, locals, passed) {
        return Ok(Some(items.len() as i64));
    }

    let expression = resolve_alias(expression, context, locals, passed, stack)?;
    match &expression.node {
        StaticExpression::Pipe { from, to }
            if matches!(&to.node, StaticExpression::Hold { .. }) =>
        {
            initial_list_count_value(from, context, stack, locals, passed)
        }
        StaticExpression::Pipe { from, to } => {
            let StaticExpression::FunctionCall { path, arguments } = &to.node else {
                return Ok(None);
            };
            if path_matches(path, &["List", "map"]) && !arguments.is_empty() {
                return initial_list_count_value(from, context, stack, locals, passed);
            }
            Ok(None)
        }
        StaticExpression::FunctionCall { path, arguments }
            if path_matches(path, &["List", "range"]) =>
        {
            let from = find_named_argument(arguments, "from")
                .ok_or_else(|| "List/range requires `from`".to_string())?;
            let to = find_named_argument(arguments, "to")
                .ok_or_else(|| "List/range requires `to`".to_string())?;
            let start = extract_integer_literal(from)?;
            let end = extract_integer_literal(to)?;
            Ok(Some((end - start + 1).max(0)))
        }
        _ => Ok(None),
    }
}

fn lower_text_list_template<'a>(
    expression: &'a StaticSpannedExpression,
    item_name: &str,
    _list_binding: &str,
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<TextListTemplate, String> {
    let expression = resolve_alias(expression, context, locals, passed, stack)?;
    let StaticExpression::FunctionCall { path, arguments } = &expression.node else {
        return Err("runtime List/map subset expects an element constructor".to_string());
    };
    if !path_matches(path, &["Element", "label"]) {
        return Err("runtime List/map subset currently supports Element/label only".to_string());
    }
    let label = find_named_argument(arguments, "label")
        .ok_or_else(|| "Element/label requires `label`".to_string())?;
    let element = find_named_argument(arguments, "element");

    let (prefix, suffix) =
        lower_text_list_label_parts(label, item_name, context, stack, locals, passed)?;
    Ok(TextListTemplate {
        tag: infer_tag(element, "span"),
        properties: collect_common_properties(element),
        prefix,
        suffix,
    })
}

fn lower_text_list_label_parts<'a>(
    expression: &'a StaticSpannedExpression,
    item_name: &str,
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<(String, String), String> {
    if matches!(
        &expression.node,
        StaticExpression::Alias(static_expression::Alias::WithoutPassed { parts, .. })
            if parts.len() == 1 && parts[0].as_str() == item_name
    ) {
        return Ok((String::new(), String::new()));
    }
    let expression = resolve_alias(expression, context, locals, passed, stack)?;
    match &expression.node {
        StaticExpression::TextLiteral { parts, .. } => {
            let mut prefix = String::new();
            let mut suffix = String::new();
            let mut seen_item = false;
            for part in parts {
                match part {
                    StaticTextPart::Text(text) => {
                        if seen_item {
                            suffix.push_str(text.as_str());
                        } else {
                            prefix.push_str(text.as_str());
                        }
                    }
                    StaticTextPart::Interpolation { var, .. } if var.as_str() == item_name => {
                        if seen_item {
                            return Err(
                                "runtime List/map subset supports only one item interpolation"
                                    .to_string(),
                            );
                        }
                        seen_item = true;
                    }
                    _ => {
                        return Err(
                            "runtime List/map subset supports only item interpolation text"
                                .to_string(),
                        );
                    }
                }
            }
            if !seen_item {
                return Err(
                    "runtime List/map subset requires the mapped item to appear in the label"
                        .to_string(),
                );
            }
            Ok((prefix, suffix))
        }
        _ => Err(
            "runtime List/map subset expects `label: item` or `TEXT { ...{item}... }`".to_string(),
        ),
    }
}

fn lower_text_node<'a>(
    parts: &[StaticTextPart],
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<SemanticNode, String> {
    let parts = lower_text_parts(parts, context, stack, locals, passed)?;
    let rendered = render_semantic_text_parts(&parts, context);
    let has_dynamic_part = parts.iter().any(|part| {
        matches!(
            part,
            SemanticTextPart::TextBinding(_)
                | SemanticTextPart::ScalarBinding(_)
                | SemanticTextPart::ObjectFieldBinding(_)
                | SemanticTextPart::ListCountBinding(_)
                | SemanticTextPart::ObjectListCountBinding(_)
                | SemanticTextPart::FilteredListCountBinding { .. }
                | SemanticTextPart::FilteredObjectListCountBinding { .. }
                | SemanticTextPart::BoolBindingText { .. }
                | SemanticTextPart::ObjectBoolFieldText { .. }
        )
    });
    Ok(if has_dynamic_part {
        SemanticNode::text_template(parts, rendered)
    } else {
        SemanticNode::text(rendered)
    })
}

fn render_text_literal<'a>(
    parts: &[StaticTextPart],
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<String, String> {
    let parts = lower_text_parts(parts, context, stack, locals, passed)?;
    Ok(render_semantic_text_parts(&parts, context))
}

fn lower_text_parts<'a>(
    parts: &[StaticTextPart],
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<Vec<SemanticTextPart>, String> {
    let mut output = Vec::with_capacity(parts.len());
    for part in parts {
        match part {
            StaticTextPart::Text(text) => {
                output.push(SemanticTextPart::Static(text.as_str().to_string()))
            }
            StaticTextPart::Interpolation { var, .. } => {
                if let Some(binding_path) =
                    text_binding_path(var.as_str(), context, locals, passed)
                {
                    output.push(SemanticTextPart::TextBinding(binding_path));
                    continue;
                }
                if let Some(field) =
                    placeholder_object_field_name(var.as_str(), context, locals, passed)
                {
                    output.push(SemanticTextPart::ObjectFieldBinding(field));
                    continue;
                }
                if let Some(binding_path) =
                    scalar_binding_path(var.as_str(), context, locals, passed)
                {
                    output.push(SemanticTextPart::ScalarBinding(binding_path));
                    continue;
                }
                let expression = resolve_named_binding(var.as_str(), context, locals, stack)?;
                if let Some(field) =
                    placeholder_object_field(expression, context, locals, passed)?
                {
                    stack.pop();
                    output.push(SemanticTextPart::ObjectFieldBinding(field));
                    continue;
                }
                if let Some(list_part) =
                    dynamic_list_count_part(expression, context, locals, passed, stack)?
                {
                    stack.pop();
                    output.push(list_part);
                    continue;
                }
                let part = if let Some((binding, _)) =
                    resolve_scalar_reference(expression, context, locals, passed, stack)?
                {
                    SemanticTextPart::ScalarBinding(binding)
                } else {
                    SemanticTextPart::Static(lower_text_value(
                        expression, context, stack, locals, passed,
                    )?)
                };
                stack.pop();
                output.push(part);
            }
        }
    }
    Ok(output)
}

fn render_semantic_text_parts(parts: &[SemanticTextPart], context: &LowerContext<'_>) -> String {
    let mut output = String::new();
    for part in parts {
        match part {
            SemanticTextPart::Static(text) => output.push_str(text),
            SemanticTextPart::TextBinding(binding) => output.push_str(
                context
                    .text_plan
                    .initial_values
                    .get(binding)
                    .map(String::as_str)
                    .unwrap_or_default(),
            ),
            SemanticTextPart::ScalarBinding(binding) => output.push_str(
                &context
                    .scalar_plan
                    .initial_values
                    .get(binding)
                    .copied()
                    .unwrap_or_default()
                    .to_string(),
            ),
            SemanticTextPart::ObjectFieldBinding(_) => {}
            SemanticTextPart::ListCountBinding(binding) => output.push_str(
                &context
                    .list_plan
                    .initial_values
                    .get(binding)
                    .map_or(0, Vec::len)
                    .to_string(),
            ),
            SemanticTextPart::ObjectListCountBinding(binding) => output.push_str(
                &context
                    .object_list_plan
                    .initial_values
                    .get(binding)
                    .map_or(0, Vec::len)
                    .to_string(),
            ),
            SemanticTextPart::FilteredListCountBinding { binding, filter } => output.push_str(
                &context
                    .list_plan
                    .initial_values
                    .get(binding)
                    .map(|values| filtered_runtime_text_list_values(values, Some(filter)).len())
                    .unwrap_or_default()
                    .to_string(),
            ),
            SemanticTextPart::FilteredObjectListCountBinding { binding, filter } => output
                .push_str(
                    &context
                        .object_list_plan
                        .initial_values
                        .get(binding)
                        .map(|items| {
                            items
                                .iter()
                                .filter(|item| {
                                    initial_object_list_item_matches_filter(item, filter, context)
                                })
                                .count()
                        })
                        .unwrap_or_default()
                        .to_string(),
                ),
            SemanticTextPart::BoolBindingText {
                binding,
                true_text,
                false_text,
            } => output.push_str(
                if context
                    .scalar_plan
                    .initial_values
                    .get(binding)
                    .copied()
                    .unwrap_or_default()
                    != 0
                {
                    true_text
                } else {
                    false_text
                },
            ),
            SemanticTextPart::ObjectBoolFieldText { false_text, .. } => output.push_str(false_text),
        }
    }
    output
}

fn lower_placeholder_object_bool_text_node<'a>(
    expression: &'a StaticSpannedExpression,
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<Option<SemanticNode>, String> {
    let expression = resolve_alias(expression, context, locals, passed, stack)?;
    let StaticExpression::Pipe { from, to } = &expression.node else {
        return Ok(None);
    };
    let StaticExpression::When { arms } = &to.node else {
        return Ok(None);
    };
    let Some(field) = placeholder_object_field(from, context, locals, passed)? else {
        return Ok(None);
    };
    let Some((true_text, false_text)) = bool_text_arms(arms, context, stack, locals, passed)?
    else {
        return Ok(None);
    };
    Ok(Some(SemanticNode::text_template(
        vec![SemanticTextPart::ObjectBoolFieldText {
            field,
            true_text,
            false_text: false_text.clone(),
        }],
        false_text,
    )))
}

fn placeholder_object_field(
    expression: &StaticSpannedExpression,
    context: &LowerContext<'_>,
    locals: &LocalScopes<'_>,
    passed: &PassedScopes,
) -> Result<Option<String>, String> {
    let StaticExpression::Alias(static_expression::Alias::WithoutPassed { parts, .. }) =
        &expression.node
    else {
        return Ok(None);
    };
    if parts.len() == 1 {
        if let Some(field) = local_object_virtual_field_name(parts[0].as_str(), locals) {
            return Ok(Some(field));
        }
    }
    let path = canonical_without_passed_path(parts, context, locals, passed)?;
    Ok(path.strip_prefix("__item__.").map(ToString::to_string))
}

fn placeholder_object_field_name(
    name: &str,
    _context: &LowerContext<'_>,
    locals: &LocalScopes<'_>,
    _passed: &PassedScopes,
) -> Option<String> {
    if let Some(field) = local_object_virtual_field_name(name, locals) {
        return Some(field);
    }
    let (first, rest) = name.split_once('.')?;
    let base = lookup_local_object_base(first, locals)?;
    (base == "__item__").then_some(rest.to_string())
}

fn local_object_virtual_field_name(name: &str, locals: &LocalScopes<'_>) -> Option<String> {
    let base = lookup_local_object_base(name, locals)?;
    (base == "__item__").then_some(name.to_string())
}

fn lower_bool_text_node<'a>(
    expression: &'a StaticSpannedExpression,
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<Option<SemanticNode>, String> {
    let expression = resolve_alias(expression, context, locals, passed, stack)?;
    let StaticExpression::Pipe { from, to } = &expression.node else {
        return Ok(None);
    };
    let StaticExpression::When { arms } = &to.node else {
        return Ok(None);
    };
    let Some(binding) = bool_binding_path(from, context, locals, passed, stack)? else {
        return Ok(None);
    };
    let Some((true_text, false_text)) = bool_text_arms(arms, context, stack, locals, passed)?
    else {
        return Ok(None);
    };
    let current = if context
        .scalar_plan
        .initial_values
        .get(&binding)
        .copied()
        .unwrap_or_default()
        != 0
    {
        true_text.clone()
    } else {
        false_text.clone()
    };
    Ok(Some(SemanticNode::text_template(
        vec![SemanticTextPart::BoolBindingText {
            binding,
            true_text,
            false_text,
        }],
        current,
    )))
}

fn lower_value_when_node<'a>(
    expression: &'a StaticSpannedExpression,
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<Option<SemanticNode>, String> {
    let expression = resolve_alias(expression, context, locals, passed, stack)?;
    let StaticExpression::Pipe { from, to } = &expression.node else {
        return Ok(None);
    };
    let StaticExpression::When { arms } = &to.node else {
        return Ok(None);
    };

    if let [arm] = arms.as_slice() {
        if let static_expression::Pattern::Alias { name } = &arm.pattern {
            let mut scope = BTreeMap::new();
            scope.insert(
                name.as_str().to_string(),
                LocalBinding {
                    expr: Some(from),
                    object_base: infer_argument_object_base(from, context, locals, passed),
                },
            );
            locals.push(scope);
            let lowered = lower_ui_node(&arm.body, context, stack, locals, passed, None);
            locals.pop();
            return lowered.map(Some);
        }
    }

    let Some(selected_body) = select_when_arm_body(from, arms, context, stack, locals, passed)?
    else {
        return Ok(None);
    };
    lower_ui_node(selected_body, context, stack, locals, passed, None).map(Some)
}

fn select_when_arm_body<'a>(
    source: &'a StaticSpannedExpression,
    arms: &'a [static_expression::Arm],
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<Option<&'a StaticSpannedExpression>, String> {
    let source = resolve_alias(source, context, locals, passed, stack)?;
    for arm in arms {
        match &arm.pattern {
            static_expression::Pattern::Literal(static_expression::Literal::Tag(tag))
                if initial_when_value_matches_tag(
                    source,
                    tag.as_str(),
                    context,
                    stack,
                    locals,
                    passed,
                )? =>
            {
                return Ok(Some(&arm.body));
            }
            static_expression::Pattern::Literal(static_expression::Literal::Text(text))
                if initial_when_value_matches_text(
                    source,
                    text.as_str(),
                    context,
                    stack,
                    locals,
                    passed,
                )? =>
            {
                return Ok(Some(&arm.body));
            }
            static_expression::Pattern::Literal(static_expression::Literal::Number(number))
                if initial_when_value_matches_number(
                    source,
                    *number as i64,
                    context,
                    stack,
                    locals,
                    passed,
                )? =>
            {
                return Ok(Some(&arm.body));
            }
            static_expression::Pattern::WildCard => return Ok(Some(&arm.body)),
            _ => {}
        }
    }
    Ok(None)
}

fn initial_when_value_matches_tag<'a>(
    expression: &'a StaticSpannedExpression,
    expected: &str,
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<bool, String> {
    let expression = resolve_alias(expression, context, locals, passed, stack)?;
    if expected == "NaN"
        && matches!(
            &expression.node,
            StaticExpression::Pipe { to, .. }
                if matches!(
                    &to.node,
                    StaticExpression::FunctionCall { path, arguments }
                        if path_matches(path, &["Text", "to_number"]) && arguments.is_empty()
                )
        )
    {
        let StaticExpression::Pipe { from, .. } = &expression.node else {
            unreachable!();
        };
        if let Ok(value) = lower_text_value(from, context, stack, locals, passed) {
            return Ok(parse_static_number(value.trim()).is_none());
        }
    }
    if (expected == "True" || expected == "False")
        && initial_bool_expression(expression, context, stack, locals, passed)?
            .is_some_and(|value| value == (expected == "True"))
    {
        return Ok(true);
    }
    Ok(match &expression.node {
        StaticExpression::Literal(static_expression::Literal::Tag(tag))
        | StaticExpression::Literal(static_expression::Literal::Text(tag)) => {
            tag.as_str() == expected
        }
        _ => false,
    })
}

fn initial_when_value_matches_text(
    expression: &StaticSpannedExpression,
    expected: &str,
    context: &LowerContext<'_>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'_>,
    passed: &mut PassedScopes,
) -> Result<bool, String> {
    let expression = resolve_alias(expression, context, locals, passed, stack)?;
    Ok(match &expression.node {
        StaticExpression::Literal(static_expression::Literal::Text(text))
        | StaticExpression::Literal(static_expression::Literal::Tag(text)) => {
            text.as_str() == expected
        }
        StaticExpression::TextLiteral { .. } => static_text_item(expression)
            .map(|value| value == expected)
            .unwrap_or(false),
        _ => false,
    })
}

fn initial_when_value_matches_number(
    expression: &StaticSpannedExpression,
    expected: i64,
    context: &LowerContext<'_>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'_>,
    passed: &mut PassedScopes,
) -> Result<bool, String> {
    if let Some((_, value)) = resolve_scalar_reference(expression, context, locals, passed, stack)?
    {
        return Ok(value == expected);
    }
    let expression = resolve_alias(expression, context, locals, passed, stack)?;
    Ok(match &expression.node {
        StaticExpression::Literal(static_expression::Literal::Number(number)) => {
            number.fract() == 0.0 && *number as i64 == expected
        }
        _ => false,
    })
}

fn bool_binding_path(
    expression: &StaticSpannedExpression,
    context: &LowerContext<'_>,
    locals: &LocalScopes<'_>,
    passed: &PassedScopes,
    stack: &mut Vec<String>,
) -> Result<Option<String>, String> {
    if let Some(binding) = element_local_binding_path(expression) {
        return Ok(Some(binding));
    }
    let Some((binding, _)) = resolve_scalar_reference(expression, context, locals, passed, stack)?
    else {
        return Ok(None);
    };
    Ok(Some(binding))
}

fn element_local_binding_path(expression: &StaticSpannedExpression) -> Option<String> {
    let StaticExpression::Alias(static_expression::Alias::WithoutPassed { parts, .. }) =
        &expression.node
    else {
        return None;
    };
    if parts.first().map(crate::parser::StrSlice::as_str) != Some("element") {
        return None;
    }
    let suffix = parts[1..]
        .iter()
        .map(crate::parser::StrSlice::as_str)
        .collect::<Vec<_>>()
        .join(".");
    if suffix.is_empty() {
        Some("__element__".to_string())
    } else {
        Some(format!("__element__.{suffix}"))
    }
}

fn bool_text_arms<'a>(
    arms: &'a [static_expression::Arm],
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<Option<(String, String)>, String> {
    let mut true_text = None;
    let mut false_text = None;
    for arm in arms {
        match &arm.pattern {
            static_expression::Pattern::Literal(static_expression::Literal::Tag(tag))
                if tag.as_str() == "True" =>
            {
                true_text = Some(lower_text_value(&arm.body, context, stack, locals, passed)?);
            }
            static_expression::Pattern::Literal(static_expression::Literal::Tag(tag))
                if tag.as_str() == "False" =>
            {
                false_text = Some(lower_text_value(&arm.body, context, stack, locals, passed)?);
            }
            static_expression::Pattern::WildCard => {
                false_text = Some(lower_text_value(&arm.body, context, stack, locals, passed)?);
            }
            _ => {}
        }
    }
    Ok(true_text.zip(false_text))
}

fn filtered_runtime_text_list_values(
    values: &[String],
    filter: Option<&TextListFilter>,
) -> Vec<String> {
    match filter {
        None => values.to_vec(),
        Some(filter) => values
            .iter()
            .filter(|value| text_list_matches_filter(value, filter))
            .cloned()
            .collect(),
    }
}

fn initial_object_list_item_matches_filter(
    item: &ObjectListItem,
    filter: &ObjectListFilter,
    context: &LowerContext<'_>,
) -> bool {
    match filter {
        ObjectListFilter::BoolFieldEquals { field, value } => {
            let actual = match field.as_str() {
                "completed" => item.completed,
                _ => false,
            };
            actual == *value
        }
        ObjectListFilter::SelectedCompletedByScalar { binding } => {
            match context
                .scalar_plan
                .initial_values
                .get(binding)
                .copied()
                .unwrap_or_default()
            {
                0 => true,
                1 => !item.completed,
                2 => item.completed,
                _ => true,
            }
        }
    }
}

fn runtime_text_list_filter(
    expression: &StaticSpannedExpression,
    item_name: &str,
) -> Result<TextListFilter, String> {
    match &expression.node {
        StaticExpression::Comparator(comparator) => {
            let (operand_a, operand_b, op) = match comparator {
                static_expression::Comparator::Equal {
                    operand_a,
                    operand_b,
                } => (operand_a.as_ref(), operand_b.as_ref(), IntCompareOp::Equal),
                static_expression::Comparator::NotEqual {
                    operand_a,
                    operand_b,
                } => (
                    operand_a.as_ref(),
                    operand_b.as_ref(),
                    IntCompareOp::NotEqual,
                ),
                static_expression::Comparator::Greater {
                    operand_a,
                    operand_b,
                } => (
                    operand_a.as_ref(),
                    operand_b.as_ref(),
                    IntCompareOp::Greater,
                ),
                static_expression::Comparator::GreaterOrEqual {
                    operand_a,
                    operand_b,
                } => (
                    operand_a.as_ref(),
                    operand_b.as_ref(),
                    IntCompareOp::GreaterOrEqual,
                ),
                static_expression::Comparator::Less {
                    operand_a,
                    operand_b,
                } => (operand_a.as_ref(), operand_b.as_ref(), IntCompareOp::Less),
                static_expression::Comparator::LessOrEqual {
                    operand_a,
                    operand_b,
                } => (
                    operand_a.as_ref(),
                    operand_b.as_ref(),
                    IntCompareOp::LessOrEqual,
                ),
            };
            let (value_operand, op) = if item_alias_operand(operand_a, item_name) {
                (operand_b, op)
            } else if item_alias_operand(operand_b, item_name) {
                (operand_a, invert_int_compare_op(op))
            } else {
                return Err(
                    "runtime List/retain subset requires comparator against the item".to_string(),
                );
            };
            let value = extract_integer_literal(value_operand)?;
            Ok(TextListFilter::IntCompare { op, value })
        }
        _ => Err(
            "runtime List/retain subset currently supports simple item comparators only"
                .to_string(),
        ),
    }
}

fn runtime_object_list_filter(
    expression: &StaticSpannedExpression,
    item_name: &str,
    context: &LowerContext<'_>,
    locals: &LocalScopes<'_>,
    passed: &PassedScopes,
) -> Result<Option<ObjectListFilter>, String> {
    if let StaticExpression::Alias(static_expression::Alias::WithoutPassed { parts, .. }) =
        &expression.node
    {
        if parts.len() == 2 && parts[0].as_str() == item_name && parts[1].as_str() == "completed" {
            return Ok(Some(ObjectListFilter::BoolFieldEquals {
                field: "completed".to_string(),
                value: true,
            }));
        }
    }
    let StaticExpression::Pipe { from, to } = &expression.node else {
        return Ok(None);
    };
    let selector_binding =
        canonical_expression_path(from, context, locals, passed, &mut Vec::new())
            .ok()
            .filter(|binding| context.scalar_plan.initial_values.contains_key(binding));
    if let (Some(binding), StaticExpression::While { arms }) = (selector_binding, &to.node) {
        if selected_completed_filter_arms(arms, item_name) {
            return Ok(Some(ObjectListFilter::SelectedCompletedByScalar {
                binding,
            }));
        }
    }
    let StaticExpression::Alias(static_expression::Alias::WithoutPassed { parts, .. }) = &from.node
    else {
        return Ok(None);
    };
    let StaticExpression::FunctionCall { path, arguments } = &to.node else {
        return Ok(None);
    };
    if !path_matches(path, &["Bool", "not"]) || !arguments.is_empty() {
        return Ok(None);
    }
    if parts.len() == 2 && parts[0].as_str() == item_name && parts[1].as_str() == "completed" {
        return Ok(Some(ObjectListFilter::BoolFieldEquals {
            field: "completed".to_string(),
            value: false,
        }));
    }
    Ok(None)
}

fn selected_completed_filter_arms(arms: &[static_expression::Arm], item_name: &str) -> bool {
    let mut saw_all = false;
    let mut saw_active = false;
    let mut saw_completed = false;

    for arm in arms {
        match &arm.pattern {
            static_expression::Pattern::Literal(static_expression::Literal::Tag(tag))
                if tag.as_str() == "All" =>
            {
                saw_all = matches!(
                    &arm.body.node,
                    StaticExpression::Literal(static_expression::Literal::Tag(value))
                        if value.as_str() == "True"
                );
            }
            static_expression::Pattern::Literal(static_expression::Literal::Tag(tag))
                if tag.as_str() == "Active" =>
            {
                saw_active = matches!(
                    &arm.body.node,
                    StaticExpression::Pipe { from, to }
                        if matches!(
                            (&from.node, &to.node),
                            (
                                StaticExpression::Alias(static_expression::Alias::WithoutPassed {
                                    parts,
                                    ..
                                }),
                                StaticExpression::FunctionCall { path, arguments },
                            ) if parts.len() == 2
                                && parts[0].as_str() == item_name
                                && parts[1].as_str() == "completed"
                                && path_matches(path, &["Bool", "not"])
                                && arguments.is_empty()
                        )
                );
            }
            static_expression::Pattern::Literal(static_expression::Literal::Tag(tag))
                if tag.as_str() == "Completed" =>
            {
                saw_completed = matches!(
                    &arm.body.node,
                    StaticExpression::Alias(static_expression::Alias::WithoutPassed { parts, .. })
                        if parts.len() == 2
                            && parts[0].as_str() == item_name
                            && parts[1].as_str() == "completed"
                );
            }
            static_expression::Pattern::WildCard => {}
            _ => return false,
        }
    }

    saw_all && saw_active && saw_completed
}

fn invert_int_compare_op(op: IntCompareOp) -> IntCompareOp {
    match op {
        IntCompareOp::Equal => IntCompareOp::Equal,
        IntCompareOp::NotEqual => IntCompareOp::NotEqual,
        IntCompareOp::Greater => IntCompareOp::Less,
        IntCompareOp::GreaterOrEqual => IntCompareOp::LessOrEqual,
        IntCompareOp::Less => IntCompareOp::Greater,
        IntCompareOp::LessOrEqual => IntCompareOp::GreaterOrEqual,
    }
}

fn item_alias_operand(expression: &StaticSpannedExpression, item_name: &str) -> bool {
    matches!(
        &expression.node,
        StaticExpression::Alias(static_expression::Alias::WithoutPassed { parts, .. })
            if parts.len() == 1 && parts[0].as_str() == item_name
    )
}

fn text_list_matches_filter(value: &str, filter: &TextListFilter) -> bool {
    match filter {
        TextListFilter::IntCompare { op, value: target } => {
            let Ok(parsed) = value.parse::<i64>() else {
                return false;
            };
            match op {
                IntCompareOp::Equal => parsed == *target,
                IntCompareOp::NotEqual => parsed != *target,
                IntCompareOp::Greater => parsed > *target,
                IntCompareOp::GreaterOrEqual => parsed >= *target,
                IntCompareOp::Less => parsed < *target,
                IntCompareOp::LessOrEqual => parsed <= *target,
            }
        }
    }
}

fn resolve_alias<'a>(
    expression: &'a StaticSpannedExpression,
    context: &'a LowerContext<'a>,
    locals: &LocalScopes<'a>,
    passed: &PassedScopes,
    stack: &mut Vec<String>,
) -> Result<&'a StaticSpannedExpression, String> {
    let original_len = stack.len();
    let mut current = expression;
    loop {
        match alias_reference(current, context, locals, passed)? {
            Some(AliasReference::Local { name, expression }) => {
                let marker = format!("local:{name}");
                if !stack.iter().any(|entry| entry == &marker) {
                    stack.push(marker);
                    current = expression;
                    continue;
                }
            }
            Some(AliasReference::Path { path, expression }) => {
                let marker = format!("binding:{path}");
                if stack.iter().any(|entry| entry == &marker) {
                    return Err(format!("cyclic alias detected for `{path}`"));
                }
                stack.push(marker);
                current = expression;
                continue;
            }
            None => break,
        }
        break;
    }
    stack.truncate(original_len);
    Ok(current)
}

enum AliasReference<'a> {
    Local {
        name: String,
        expression: &'a StaticSpannedExpression,
    },
    Path {
        path: String,
        expression: &'a StaticSpannedExpression,
    },
}

fn alias_reference<'a>(
    expression: &'a StaticSpannedExpression,
    context: &'a LowerContext<'a>,
    locals: &LocalScopes<'a>,
    passed: &PassedScopes,
) -> Result<Option<AliasReference<'a>>, String> {
    match &expression.node {
        StaticExpression::Alias(static_expression::Alias::WithoutPassed { parts, .. }) => {
            if parts.len() == 1 {
                let name = parts[0].as_str();
                if let Some(local) = lookup_local_binding_expr(name, locals) {
                    return Ok(Some(AliasReference::Local {
                        name: name.to_string(),
                        expression: local,
                    }));
                }
            }
            if let Some((first, rest)) = parts.split_first() {
                if let Some(local) =
                    resolve_local_field_expression(first.as_str(), rest, context, locals, passed)?
                {
                    return Ok(Some(AliasReference::Local {
                        name: parts
                            .iter()
                            .map(crate::parser::StrSlice::as_str)
                            .collect::<Vec<_>>()
                            .join("."),
                        expression: local,
                    }));
                }
                if let Some(top_level) = context.bindings.get(first.as_str()).copied() {
                    if let Some(field_expression) =
                        resolve_binding_field_expression(top_level, rest, context, locals, passed)?
                    {
                        return Ok(Some(AliasReference::Path {
                            path: parts
                                .iter()
                                .map(crate::parser::StrSlice::as_str)
                                .collect::<Vec<_>>()
                                .join("."),
                            expression: field_expression,
                        }));
                    }
                }
            }
            let path = canonical_without_passed_path(parts, context, locals, passed)?;
            let resolved_path = context
                .path_bindings
                .contains_key(&path)
                .then_some(path.clone())
                .or_else(|| unique_suffix_binding_path(&path, &context.path_bindings));
            let Some(path) = resolved_path else {
                if path.starts_with("__item__.") || path.starts_with("__element__.") {
                    return Ok(None);
                }
                return Err(format!("unknown binding path `{path}`"));
            };
            let Some(expression) = context.path_bindings.get(&path).copied() else {
                return Err(format!("unknown binding path `{path}`"));
            };
            Ok(Some(AliasReference::Path { path, expression }))
        }
        StaticExpression::Alias(static_expression::Alias::WithPassed { extra_parts }) => {
            let path = canonical_passed_path(extra_parts, passed)?;
            let Some(expression) = context.path_bindings.get(&path).copied() else {
                return Err(format!("unknown binding path `{path}`"));
            };
            Ok(Some(AliasReference::Path { path, expression }))
        }
        _ => Ok(None),
    }
}

fn unique_suffix_binding_path(
    suffix: &str,
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
) -> Option<String> {
    let dotted_suffix = format!(".{suffix}");
    let mut matches = path_bindings
        .keys()
        .filter(|candidate| candidate.ends_with(&dotted_suffix))
        .cloned();
    let first = matches.next()?;
    matches.next().is_none().then_some(first)
}

fn lookup_binding<'a>(
    binding_name: &str,
    context: &'a LowerContext<'a>,
    locals: &LocalScopes<'a>,
) -> Option<&'a StaticSpannedExpression> {
    lookup_local_binding_expr(binding_name, locals)
        .or_else(|| context.bindings.get(binding_name).copied())
}

fn lookup_local_binding_expr<'a>(
    binding_name: &str,
    locals: &LocalScopes<'a>,
) -> Option<&'a StaticSpannedExpression> {
    locals
        .iter()
        .rev()
        .find_map(|scope| match scope.get(binding_name) {
            Some(LocalBinding {
                expr: Some(expression),
                ..
            }) => Some(*expression),
            _ => None,
        })
}

fn lookup_local_object_base<'a>(
    binding_name: &str,
    locals: &'a LocalScopes<'_>,
) -> Option<&'a str> {
    locals
        .iter()
        .rev()
        .find_map(|scope| match scope.get(binding_name) {
            Some(LocalBinding {
                object_base: Some(base),
                ..
            }) => Some(base.as_str()),
            _ => None,
        })
}

fn active_object_scope(locals: &LocalScopes<'_>) -> Option<String> {
    locals
        .iter()
        .rev()
        .find_map(|scope| {
            scope.values().find_map(|binding| {
                binding
                    .object_base
                    .as_ref()
                    .filter(|base| base.starts_with("__item__"))
                    .cloned()
            })
        })
}

fn resolve_local_field_expression<'a>(
    binding_name: &str,
    extra_parts: &[crate::parser::StrSlice],
    context: &'a LowerContext<'a>,
    locals: &LocalScopes<'a>,
    passed: &PassedScopes,
) -> Result<Option<&'a StaticSpannedExpression>, String> {
    let Some(expression) = lookup_local_binding_expr(binding_name, locals) else {
        return Ok(None);
    };
    resolve_local_field_expression_with_scopes(
        expression,
        extra_parts,
        context,
        locals,
        passed,
        &mut Vec::new(),
    )
}

fn resolve_binding_field_expression<'a>(
    expression: &'a StaticSpannedExpression,
    extra_parts: &[crate::parser::StrSlice],
    context: &'a LowerContext<'a>,
    locals: &LocalScopes<'a>,
    passed: &PassedScopes,
) -> Result<Option<&'a StaticSpannedExpression>, String> {
    resolve_local_field_expression_with_scopes(
        expression,
        extra_parts,
        context,
        locals,
        passed,
        &mut Vec::new(),
    )
}

fn resolve_local_field_expression_with_scopes<'a>(
    expression: &'a StaticSpannedExpression,
    extra_parts: &[crate::parser::StrSlice],
    context: &'a LowerContext<'a>,
    locals: &LocalScopes<'a>,
    passed: &PassedScopes,
    stack: &mut Vec<String>,
) -> Result<Option<&'a StaticSpannedExpression>, String> {
    if extra_parts.is_empty() {
        return resolve_alias(expression, context, locals, passed, stack).map(Some);
    }
    let expression = resolve_alias(expression, context, locals, passed, stack)?;
    if let Some(object) = resolve_object(expression) {
        let Some(field_expression) = find_object_field(object, extra_parts[0].as_str()) else {
            return Ok(None);
        };
        return resolve_local_field_expression_with_scopes(
            field_expression,
            &extra_parts[1..],
            context,
            locals,
            passed,
            stack,
        );
    }
    match &expression.node {
        StaticExpression::Block { variables, output } => {
            let mut nested_locals = locals.clone();
            let mut scope = BTreeMap::new();
            let mut variable_parts = Vec::with_capacity(variables.len());
            for variable in variables {
                variable_parts.push(format!(
                    "{}={}",
                    variable.node.name.as_str(),
                    expression_fingerprint(&variable.node.value, context, locals, passed, stack)?
                ));
                scope.insert(
                    variable.node.name.as_str().to_string(),
                    LocalBinding {
                        expr: Some(&variable.node.value),
                        object_base: infer_argument_object_base(
                            &variable.node.value,
                            context,
                            locals,
                            passed,
                        ),
                    },
                );
            }
            nested_locals.push(scope);
            return resolve_local_field_expression_with_scopes(
                output,
                extra_parts,
                context,
                &nested_locals,
                passed,
                stack,
            );
        }
        StaticExpression::Pipe { from, to } => {
            if matches!(&to.node, StaticExpression::Hold { .. }) {
                return resolve_local_field_expression_with_scopes(
                    from,
                    extra_parts,
                    context,
                    locals,
                    passed,
                    stack,
                );
            }
            if let StaticExpression::When { arms } = &to.node {
                let mut nested_locals = locals.clone();
                let mut nested_passed = passed.clone();
                if let Some(body) = select_when_arm_body(
                    from,
                    arms,
                    context,
                    stack,
                    &mut nested_locals,
                    &mut nested_passed,
                )? {
                    return resolve_local_field_expression_with_scopes(
                        body,
                        extra_parts,
                        context,
                        &nested_locals,
                        &nested_passed,
                        stack,
                    );
                }
            }
        }
        StaticExpression::Latest { inputs } => {
            for input in inputs.iter().rev() {
                if let Some(resolved) = resolve_local_field_expression_with_scopes(
                    input,
                    extra_parts,
                    context,
                    locals,
                    passed,
                    stack,
                )? {
                    return Ok(Some(resolved));
                }
            }
        }
        StaticExpression::FunctionCall { path, arguments }
            if path.len() == 1 && context.functions.contains_key(path[0].as_str()) =>
        {
            let function = context
                .functions
                .get(path[0].as_str())
                .ok_or_else(|| format!("unknown function `{}`", path[0].as_str()))?;
            let mut nested_locals = locals.clone();
            let mut scope = BTreeMap::new();
            for parameter in &function.parameters {
                if let Some(argument) = find_named_argument(arguments, parameter) {
                    scope.insert(
                        parameter.clone(),
                        LocalBinding {
                            expr: Some(argument),
                            object_base: infer_argument_object_base(
                                argument, context, locals, passed,
                            ),
                        },
                    );
                }
            }
            nested_locals.push(scope);
            let mut nested_passed = passed.clone();
            if let Some(argument) = find_named_argument(arguments, "PASS") {
                nested_passed.push(passed_scope_for_expression(
                    argument, context, locals, passed, stack,
                )?);
            }
            return resolve_local_field_expression_with_scopes(
                function.body,
                extra_parts,
                context,
                &nested_locals,
                &nested_passed,
                stack,
            );
        }
        _ => {}
    }
    Ok(None)
}

fn resolve_object_field_expression<'a>(
    expression: &'a StaticSpannedExpression,
    extra_parts: &[crate::parser::StrSlice],
    context: &'a LowerContext<'a>,
) -> Result<Option<&'a StaticSpannedExpression>, String> {
    if extra_parts.is_empty() {
        return Ok(Some(expression));
    }
    if let Some(object) = resolve_object(expression) {
        let Some(field_expression) = find_object_field(object, extra_parts[0].as_str()) else {
            return Ok(None);
        };
        return resolve_object_field_expression(field_expression, &extra_parts[1..], context);
    }
    let StaticExpression::FunctionCall { path, arguments } = &expression.node else {
        return Ok(None);
    };
    if path.len() != 1 {
        return Ok(None);
    }
    let Some(function) = context.functions.get(path[0].as_str()) else {
        return Ok(None);
    };
    let Some(object) = resolve_object(function.body) else {
        return Ok(None);
    };
    let Some(field_expression) = find_object_field(object, extra_parts[0].as_str()) else {
        return Ok(None);
    };
    let resolved_field =
        if let StaticExpression::Alias(static_expression::Alias::WithoutPassed { parts, .. }) =
            &field_expression.node
        {
            if parts.len() == 1 {
                if let Some(argument) = find_named_argument(arguments, parts[0].as_str()) {
                    argument
                } else {
                    field_expression
                }
            } else {
                field_expression
            }
        } else {
            field_expression
    };
    resolve_object_field_expression(resolved_field, &extra_parts[1..], context)
}

fn resolve_postfix_field_expression<'a>(
    expression: &'a StaticSpannedExpression,
    field: &crate::parser::StrSlice,
    context: &'a LowerContext<'a>,
    locals: &LocalScopes<'a>,
    passed: &PassedScopes,
    stack: &mut Vec<String>,
) -> Result<Option<&'a StaticSpannedExpression>, String> {
    resolve_local_field_expression_with_scopes(
        expression,
        std::slice::from_ref(field),
        context,
        locals,
        passed,
        stack,
    )
}

fn canonical_without_passed_path(
    parts: &[crate::parser::StrSlice],
    context: &LowerContext<'_>,
    locals: &LocalScopes<'_>,
    passed: &PassedScopes,
) -> Result<String, String> {
    if parts.is_empty() {
        return Err("empty alias path is not supported".to_string());
    }
    let joined = parts
        .iter()
        .map(crate::parser::StrSlice::as_str)
        .collect::<Vec<_>>()
        .join(".");
    if parts[0].as_str() == "element" {
        if parts.len() == 1 {
            return Ok("__element__".to_string());
        }
        let rest = parts[1..]
            .iter()
            .map(crate::parser::StrSlice::as_str)
            .collect::<Vec<_>>()
            .join(".");
        return Ok(format!("__element__.{rest}"));
    }
    if context.path_bindings.contains_key(&joined) {
        return Ok(joined);
    }
    if let Some(base) = lookup_local_object_base(parts[0].as_str(), locals) {
        if parts.len() == 1 {
            return Ok(base.to_string());
        }
        let rest = parts[1..]
            .iter()
            .map(crate::parser::StrSlice::as_str)
            .collect::<Vec<_>>()
            .join(".");
        return Ok(format!("{base}.{rest}"));
    }
    for scope in passed.iter().rev() {
        for candidate in passed_scope_candidates(scope, &joined) {
            if context.path_bindings.contains_key(&candidate) {
                return Ok(candidate);
            }
        }
    }
    Ok(joined)
}

fn canonical_passed_path(
    extra_parts: &[crate::parser::StrSlice],
    passed: &PassedScopes,
) -> Result<String, String> {
    let Some(scope) = passed.last() else {
        return Err("PASSED is not available outside a PASS context".to_string());
    };
    passed_scope_path(scope, extra_parts)
}

fn passed_scope_candidates(scope: &PassedScope, joined: &str) -> Vec<String> {
    match scope {
        PassedScope::Path(base) => vec![format!("{base}.{joined}")],
        PassedScope::Bindings(bindings) => bindings
            .iter()
            .filter_map(|(name, target)| {
                if joined == name {
                    Some(target.clone())
                } else if let Some(rest) = joined.strip_prefix(&format!("{name}.")) {
                    Some(format!("{target}.{rest}"))
                } else {
                    None
                }
            })
            .collect(),
    }
}

fn passed_scope_path(
    scope: &PassedScope,
    extra_parts: &[crate::parser::StrSlice],
) -> Result<String, String> {
    match scope {
        PassedScope::Path(base) => {
            if extra_parts.is_empty() {
                return Ok(base.clone());
            }
            Ok(format!(
                "{base}.{}",
                extra_parts
                    .iter()
                    .map(crate::parser::StrSlice::as_str)
                    .collect::<Vec<_>>()
                    .join(".")
            ))
        }
        PassedScope::Bindings(bindings) => {
            let Some((first, rest)) = extra_parts.split_first() else {
                return Err("object PASS requires a named PASSED entry".to_string());
            };
            let Some(base) = bindings.get(first.as_str()) else {
                return Err(format!(
                    "unknown PASSED binding `{}` in object PASS",
                    first.as_str()
                ));
            };
            if rest.is_empty() {
                Ok(base.clone())
            } else {
                Ok(format!(
                    "{base}.{}",
                    rest.iter()
                        .map(crate::parser::StrSlice::as_str)
                        .collect::<Vec<_>>()
                        .join(".")
                ))
            }
        }
    }
}

fn canonical_expression_path(
    expression: &StaticSpannedExpression,
    context: &LowerContext<'_>,
    locals: &LocalScopes<'_>,
    passed: &PassedScopes,
    stack: &mut Vec<String>,
) -> Result<String, String> {
    if let StaticExpression::Alias(static_expression::Alias::WithoutPassed { parts, .. }) =
        &expression.node
    {
        let has_local_expr_shadow =
            parts.len() == 1 && lookup_local_binding_expr(parts[0].as_str(), locals).is_some();
        if !has_local_expr_shadow {
            let path = canonical_without_passed_path(parts, context, locals, passed)?;
            if context.path_bindings.contains_key(&path)
                || context.scalar_plan.initial_values.contains_key(&path)
                || context.list_plan.initial_values.contains_key(&path)
            {
                return Ok(path);
            }
        }
    }
    match alias_reference(expression, context, locals, passed)? {
        Some(AliasReference::Local { name, expression }) => {
            let marker = format!("local:{name}");
            if stack.iter().any(|entry| entry == &marker) {
                return Ok(name);
            }
            stack.push(marker);
            let resolved = canonical_expression_path(expression, context, locals, passed, stack);
            stack.pop();
            resolved
        }
        Some(AliasReference::Path { path, .. }) => Ok(path),
        None => Err(format!(
            "expression `{}` cannot be used as a binding path",
            describe_expression(expression)
        )),
    }
}

fn canonical_alias_path(
    alias: &static_expression::Alias,
    context: &LowerContext<'_>,
    locals: &LocalScopes<'_>,
    passed: &PassedScopes,
    _stack: &mut Vec<String>,
) -> Result<Option<String>, String> {
    match alias {
        static_expression::Alias::WithoutPassed { parts, .. } => Ok(Some(
            canonical_without_passed_path(parts, context, locals, passed)?,
        )),
        static_expression::Alias::WithPassed { extra_parts } => {
            Ok(Some(canonical_passed_path(extra_parts, passed)?))
        }
    }
}

fn scalar_binding_path(
    name: &str,
    context: &LowerContext<'_>,
    locals: &LocalScopes<'_>,
    passed: &PassedScopes,
) -> Option<String> {
    if let Some(expression) = lookup_local_binding_expr(name, locals) {
        return match &expression.node {
            StaticExpression::Alias(_) => {
                canonical_expression_path(expression, context, locals, passed, &mut Vec::new())
                    .ok()
                    .filter(|path| context.scalar_plan.initial_values.contains_key(path))
            }
            _ => None,
        };
    }
    if let Some((first, rest)) = name.split_once('.') {
        if let Some(base) = lookup_local_object_base(first, locals) {
            let candidate = format!("{base}.{rest}");
            if context.scalar_plan.initial_values.contains_key(&candidate) {
                return Some(candidate);
            }
        }
    }
    if context.scalar_plan.initial_values.contains_key(name) {
        return Some(name.to_string());
    }
    for scope in passed.iter().rev() {
        for candidate in passed_scope_candidates(scope, name) {
            if context.scalar_plan.initial_values.contains_key(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

fn text_binding_path(
    name: &str,
    context: &LowerContext<'_>,
    locals: &LocalScopes<'_>,
    passed: &PassedScopes,
) -> Option<String> {
    if let Some(expression) = lookup_local_binding_expr(name, locals) {
        return match &expression.node {
            StaticExpression::Alias(_) => {
                canonical_expression_path(expression, context, locals, passed, &mut Vec::new())
                    .ok()
                    .filter(|path| context.text_plan.initial_values.contains_key(path))
            }
            _ => None,
        };
    }
    if context.text_plan.initial_values.contains_key(name) {
        return Some(name.to_string());
    }
    for scope in passed.iter().rev() {
        for candidate in passed_scope_candidates(scope, name) {
            if context.text_plan.initial_values.contains_key(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

fn resolve_text_reference<'a>(
    expression: &'a StaticSpannedExpression,
    context: &'a LowerContext<'a>,
    locals: &LocalScopes<'a>,
    passed: &PassedScopes,
    stack: &mut Vec<String>,
) -> Result<Option<(String, String)>, String> {
    if let StaticExpression::Alias(static_expression::Alias::WithoutPassed { parts, .. }) =
        &expression.node
    {
        let has_local_expr_shadow =
            parts.len() == 1 && lookup_local_binding_expr(parts[0].as_str(), locals).is_some();
        if !has_local_expr_shadow {
            let path = canonical_without_passed_path(parts, context, locals, passed)?;
            if let Some(value) = context.text_plan.initial_values.get(&path) {
                return Ok(Some((path, value.clone())));
            }
        }
    }
    match alias_reference(expression, context, locals, passed)? {
        Some(AliasReference::Local { name, expression }) => {
            let marker = format!("local:{name}");
            if stack.iter().any(|entry| entry == &marker) {
                if let Some(value) = context.text_plan.initial_values.get(&name) {
                    return Ok(Some((name, value.clone())));
                }
                let Some(expression) = context.path_bindings.get(&name).copied() else {
                    return Ok(None);
                };
                let marker = format!("binding:{name}");
                if stack.iter().any(|entry| entry == &marker) {
                    return Ok(None);
                }
                stack.push(marker);
                let resolved = resolve_text_reference(expression, context, locals, passed, stack)?;
                stack.pop();
                return Ok(resolved);
            }
            stack.push(marker);
            let resolved = resolve_text_reference(expression, context, locals, passed, stack)?;
            stack.pop();
            Ok(resolved)
        }
        Some(AliasReference::Path { path, expression }) => {
            if let Some(value) = context.text_plan.initial_values.get(&path) {
                return Ok(Some((path, value.clone())));
            }
            let marker = format!("binding:{path}");
            if stack.iter().any(|entry| entry == &marker) {
                return Ok(None);
            }
            stack.push(marker);
            let resolved = resolve_text_reference(expression, context, locals, passed, stack)?;
            stack.pop();
            Ok(resolved)
        }
        None => Ok(None),
    }
}

fn runtime_list_binding_path(
    expression: &StaticSpannedExpression,
    context: &LowerContext<'_>,
    locals: &LocalScopes<'_>,
    passed: &PassedScopes,
    stack: &mut Vec<String>,
) -> Result<Option<String>, String> {
    let path = canonical_expression_path(expression, context, locals, passed, stack).ok();
    Ok(path.filter(|path| context.list_plan.initial_values.contains_key(path)))
}

fn runtime_object_list_binding_path(
    expression: &StaticSpannedExpression,
    context: &LowerContext<'_>,
    locals: &LocalScopes<'_>,
    passed: &PassedScopes,
    stack: &mut Vec<String>,
) -> Result<Option<String>, String> {
    if let Some(field) = placeholder_object_field(expression, context, locals, passed)? {
        return Ok(Some(format!("__item__.{field}")));
    }
    let resolved = resolve_alias(expression, context, locals, passed, stack)?;
    if let Some(field) = placeholder_object_field(resolved, context, locals, passed)? {
        return Ok(Some(format!("__item__.{field}")));
    }
    let path = canonical_expression_path(expression, context, locals, passed, stack).ok();
    Ok(path.filter(|path| {
        path.starts_with("__item__.") || context.object_list_plan.initial_values.contains_key(path)
    }))
}

fn runtime_object_list_ref(
    expression: &StaticSpannedExpression,
    context: &LowerContext<'_>,
    locals: &LocalScopes<'_>,
    passed: &PassedScopes,
    stack: &mut Vec<String>,
) -> Result<Option<RuntimeObjectListRef>, String> {
    if let Some(binding) =
        runtime_object_list_binding_path(expression, context, locals, passed, stack)?
    {
        return Ok(Some(RuntimeObjectListRef {
            binding,
            filter: None,
        }));
    }
    let expression = resolve_alias(expression, context, locals, passed, stack)?;
    let StaticExpression::Pipe { from, to } = &expression.node else {
        return Ok(None);
    };
    let StaticExpression::FunctionCall { path, arguments } = &to.node else {
        return Ok(None);
    };
    if !path_matches(path, &["List", "retain"]) {
        return Ok(None);
    }
    let Some(binding) = runtime_object_list_binding_path(from, context, locals, passed, stack)?
    else {
        return Ok(None);
    };
    let item_name = find_positional_parameter_name(arguments)
        .ok_or_else(|| "List/retain requires an item parameter name".to_string())?;
    let condition = find_named_argument(arguments, "if")
        .ok_or_else(|| "List/retain requires `if`".to_string())?;
    if matches!(
        &condition.node,
        StaticExpression::Literal(static_expression::Literal::Tag(tag)) if tag.as_str() == "True"
    ) {
        return Ok(Some(RuntimeObjectListRef {
            binding,
            filter: None,
        }));
    }
    let Some(filter) = runtime_object_list_filter(condition, item_name, context, locals, passed)?
    else {
        return Ok(None);
    };
    Ok(Some(RuntimeObjectListRef {
        binding,
        filter: Some(filter),
    }))
}

fn runtime_text_list_ref(
    expression: &StaticSpannedExpression,
    context: &LowerContext<'_>,
    locals: &LocalScopes<'_>,
    passed: &PassedScopes,
    stack: &mut Vec<String>,
) -> Result<Option<RuntimeTextListRef>, String> {
    if let Some(binding) = runtime_list_binding_path(expression, context, locals, passed, stack)? {
        return Ok(Some(RuntimeTextListRef {
            binding,
            filter: None,
        }));
    }
    let expression = resolve_alias(expression, context, locals, passed, stack)?;
    let StaticExpression::Pipe { from, to } = &expression.node else {
        return Ok(None);
    };
    let StaticExpression::FunctionCall { path, arguments } = &to.node else {
        return Ok(None);
    };
    if !path_matches(path, &["List", "retain"]) {
        return Ok(None);
    }
    let Some(binding) = runtime_list_binding_path(from, context, locals, passed, stack)? else {
        return Ok(None);
    };
    let item_name = find_positional_parameter_name(arguments)
        .ok_or_else(|| "List/retain requires an item parameter name".to_string())?;
    let condition = find_named_argument(arguments, "if")
        .ok_or_else(|| "List/retain requires `if`".to_string())?;
    let condition = resolve_alias(condition, context, locals, passed, stack)?;
    if matches!(
        &condition.node,
        StaticExpression::Literal(static_expression::Literal::Tag(tag)) if tag.as_str() == "True"
    ) {
        return Ok(Some(RuntimeTextListRef {
            binding,
            filter: None,
        }));
    }
    let filter = runtime_text_list_filter(condition, item_name)?;
    Ok(Some(RuntimeTextListRef {
        binding,
        filter: Some(filter),
    }))
}

fn dynamic_list_count_part(
    expression: &StaticSpannedExpression,
    context: &LowerContext<'_>,
    locals: &LocalScopes<'_>,
    passed: &PassedScopes,
    stack: &mut Vec<String>,
) -> Result<Option<SemanticTextPart>, String> {
    if let Some(list_ref) = runtime_object_list_ref(expression, context, locals, passed, stack)? {
        return Ok(Some(match list_ref.filter {
            Some(filter) => SemanticTextPart::FilteredObjectListCountBinding {
                binding: list_ref.binding,
                filter,
            },
            None => SemanticTextPart::ObjectListCountBinding(list_ref.binding),
        }));
    }
    if let Some(list_ref) = runtime_text_list_ref(expression, context, locals, passed, stack)? {
        return Ok(Some(match list_ref.filter {
            Some(filter) => SemanticTextPart::FilteredListCountBinding {
                binding: list_ref.binding,
                filter,
            },
            None => SemanticTextPart::ListCountBinding(list_ref.binding),
        }));
    }
    let expression = resolve_alias(expression, context, locals, passed, stack)?;
    match &expression.node {
        StaticExpression::Pipe { from, to } => {
            let StaticExpression::FunctionCall { path, arguments } = &to.node else {
                return Ok(None);
            };
            if path_matches(path, &["List", "count"]) && arguments.is_empty() {
                if let Some(part) = dynamic_list_count_part(from, context, locals, passed, stack)? {
                    return Ok(Some(part));
                }
            }
            Ok(None)
        }
        _ => Ok(None),
    }
}

fn find_named_argument<'a>(
    arguments: &'a [static_expression::Spanned<StaticArgument>],
    name: &str,
) -> Option<&'a StaticSpannedExpression> {
    arguments
        .iter()
        .find(|argument| argument.node.name.as_str() == name)
        .and_then(|argument| argument.node.value.as_ref())
}

fn infer_tag(element: Option<&StaticSpannedExpression>, default_tag: &str) -> String {
    element
        .and_then(resolve_object)
        .and_then(|object| find_object_field(object, "tag"))
        .and_then(extract_tag_name)
        .map_or_else(|| default_tag.to_string(), |tag| tag.to_lowercase())
}

fn collect_common_properties(element: Option<&StaticSpannedExpression>) -> Vec<(String, String)> {
    let Some(object) = element.and_then(resolve_object) else {
        return Vec::new();
    };
    let mut properties = Vec::new();
    if let Some(role) = find_object_field(object, "role").and_then(extract_tag_name) {
        properties.push(("role".to_string(), role.to_string()));
    }
    properties
}

fn collect_event_bindings(
    element: Option<&StaticSpannedExpression>,
    current_binding: Option<&str>,
    context: &LowerContext<'_>,
) -> Vec<SemanticEventBinding> {
    let Some(object) = element.and_then(resolve_object) else {
        return Vec::new();
    };
    let Some(event_object) = find_object_field(object, "event").and_then(resolve_object) else {
        return Vec::new();
    };

    event_object
        .variables
        .iter()
        .filter_map(|variable| {
            let event_name = variable.node.name.as_str();
            let kind = ui_event_kind_for_name(event_name)?;
            matches!(variable.node.value.node, StaticExpression::Link).then_some(
                SemanticEventBinding {
                    kind,
                    source_binding: current_binding.map(ToString::to_string),
                    action: current_binding
                        .and_then(|binding| event_action_for(binding, event_name, context)),
                },
            )
        })
        .collect()
}

fn collect_fact_bindings(element: Option<&StaticSpannedExpression>) -> Vec<SemanticFactBinding> {
    let Some(object) = element.and_then(resolve_object) else {
        return Vec::new();
    };

    object
        .variables
        .iter()
        .filter_map(|variable| {
            matches!(variable.node.value.node, StaticExpression::Link).then_some(
                fact_kind_for_name(variable.node.name.as_str()).map(|kind| SemanticFactBinding {
                    kind,
                    binding: format!("__element__.{}", variable.node.name.as_str()),
                }),
            )?
        })
        .collect()
}

fn fact_kind_for_name(name: &str) -> Option<SemanticFactKind> {
    match name {
        "hovered" => Some(SemanticFactKind::Hovered),
        "focused" => Some(SemanticFactKind::Focused),
        _ => None,
    }
}

fn event_action_for(
    binding_name: &str,
    event_name: &str,
    context: &LowerContext<'_>,
) -> Option<SemanticAction> {
    if let Some(updates) = context
        .scalar_plan
        .event_updates
        .get(&(binding_name.to_string(), event_name.to_string()))
        .cloned()
    {
        return Some(SemanticAction::UpdateScalars { updates });
    }
    if let Some(updates) = context
        .text_plan
        .event_updates
        .get(&(binding_name.to_string(), event_name.to_string()))
        .cloned()
    {
        return Some(SemanticAction::UpdateTexts { updates });
    }
    context
        .list_plan
        .event_updates
        .get(&(binding_name.to_string(), event_name.to_string()))
        .cloned()
        .map(|updates| SemanticAction::UpdateTextLists { updates })
        .or_else(|| {
            context
                .object_list_plan
                .event_updates
                .get(&(binding_name.to_string(), event_name.to_string()))
                .cloned()
                .map(|updates| SemanticAction::UpdateObjectLists { updates })
        })
}

fn ui_event_kind_for_name(event_name: &str) -> Option<boon_scene::UiEventKind> {
    match event_name {
        "press" | "click" => Some(boon_scene::UiEventKind::Click),
        "double_click" => Some(boon_scene::UiEventKind::DoubleClick),
        "change" => Some(boon_scene::UiEventKind::Input),
        "blur" => Some(boon_scene::UiEventKind::Blur),
        "focus" => Some(boon_scene::UiEventKind::Focus),
        "key_down" => Some(boon_scene::UiEventKind::KeyDown),
        _ => None,
    }
}

fn alias_binding_name<'a>(
    expression: &'a StaticSpannedExpression,
) -> Result<Option<&'a str>, String> {
    match &expression.node {
        StaticExpression::Alias(static_expression::Alias::WithoutPassed { parts, .. }) => {
            if parts.len() == 1 {
                return Ok(Some(parts[0].as_str()));
            }
            Ok(None)
        }
        StaticExpression::Alias(static_expression::Alias::WithPassed { .. }) => Ok(None),
        _ => Ok(None),
    }
}

fn resolve_named_binding<'a>(
    binding_name: &str,
    context: &'a LowerContext<'a>,
    locals: &LocalScopes<'a>,
    stack: &mut Vec<String>,
) -> Result<&'a StaticSpannedExpression, String> {
    if let Some(next) = lookup_local_binding_expr(binding_name, locals) {
        let marker = format!("local:{binding_name}");
        if !stack.iter().any(|entry| entry == &marker) {
            stack.push(marker);
            return Ok(next);
        }
        if !context.bindings.contains_key(binding_name)
            && alias_binding_name(next)? != Some(binding_name)
        {
            return Ok(next);
        }
    }
    let marker = format!("binding:{binding_name}");
    if stack.iter().any(|entry| entry == &marker) {
        return Err(format!("cyclic alias detected for `{binding_name}`"));
    }
    let next = context
        .bindings
        .get(binding_name)
        .copied()
        .ok_or_else(|| format!("unknown top-level binding `{binding_name}`"))?;
    stack.push(marker);
    Ok(next)
}

fn resolve_scalar_reference<'a>(
    expression: &'a StaticSpannedExpression,
    context: &'a LowerContext<'a>,
    locals: &LocalScopes<'a>,
    passed: &PassedScopes,
    stack: &mut Vec<String>,
) -> Result<Option<(String, i64)>, String> {
    if let StaticExpression::Alias(static_expression::Alias::WithoutPassed { parts, .. }) =
        &expression.node
    {
        let has_local_expr_shadow =
            parts.len() == 1 && lookup_local_binding_expr(parts[0].as_str(), locals).is_some();
        if !has_local_expr_shadow {
            let path = canonical_without_passed_path(parts, context, locals, passed)?;
            if let Some(value) = context.scalar_plan.initial_values.get(&path) {
                return Ok(Some((path, *value)));
            }
        }
    }
    match alias_reference(expression, context, locals, passed)? {
        Some(AliasReference::Local { name, expression }) => {
            let marker = format!("local:{name}");
            if stack.iter().any(|entry| entry == &marker) {
                if let Some(value) = context.scalar_plan.initial_values.get(&name) {
                    return Ok(Some((name, *value)));
                }
                let Some(expression) = context.path_bindings.get(&name).copied() else {
                    return Ok(None);
                };
                let marker = format!("binding:{name}");
                if stack.iter().any(|entry| entry == &marker) {
                    return Ok(None);
                }
                stack.push(marker);
                let resolved =
                    resolve_scalar_reference(expression, context, locals, passed, stack)?;
                stack.pop();
                return Ok(resolved);
            }
            stack.push(marker);
            let resolved = resolve_scalar_reference(expression, context, locals, passed, stack)?;
            stack.pop();
            Ok(resolved)
        }
        Some(AliasReference::Path { path, expression }) => {
            if let Some(value) = context.scalar_plan.initial_values.get(&path) {
                return Ok(Some((path, *value)));
            }
            let marker = format!("binding:{path}");
            if stack.iter().any(|entry| entry == &marker) {
                return Err(format!("cyclic alias detected for `{path}`"));
            }
            stack.push(marker);
            let resolved = resolve_scalar_reference(expression, context, locals, passed, stack)?;
            stack.pop();
            Ok(resolved)
        }
        None => Ok(None),
    }
}

fn function_invocation_target<'a>(
    expression: &'a StaticSpannedExpression,
    context: &'a LowerContext<'a>,
    locals: &LocalScopes<'a>,
    passed: &PassedScopes,
    stack: &mut Vec<String>,
) -> Result<Option<(&'a str, &'a [static_expression::Spanned<StaticArgument>])>, String> {
    if let Some(function_name) = alias_binding_name(expression)?
        .filter(|function_name| context.functions.contains_key(*function_name))
    {
        return Ok(Some((function_name, &[])));
    }
    let expression = resolve_alias(expression, context, locals, passed, stack)?;
    match &expression.node {
        StaticExpression::FunctionCall { path, arguments }
            if path.len() == 1 && context.functions.contains_key(path[0].as_str()) =>
        {
            Ok(Some((path[0].as_str(), arguments)))
        }
        _ => Ok(None),
    }
}

fn invoke_function<'a>(
    function_name: &str,
    arguments: &'a [static_expression::Spanned<StaticArgument>],
    passed_value: Option<&'a StaticSpannedExpression>,
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
    current_binding: Option<&str>,
) -> Result<SemanticNode, String> {
    with_invoked_function_scope(
        function_name,
        arguments,
        passed_value,
        context,
        stack,
        locals,
        passed,
        |body, context, stack, locals, passed| {
            lower_ui_node(body, context, stack, locals, passed, current_binding)
        },
    )
}

fn synthetic_text_expression(
    value: &str,
    template: &StaticSpannedExpression,
) -> &'static StaticSpannedExpression {
    let source = SourceCode::new(value.to_string());
    let slice = source.slice(0, source.len());
    Box::leak(Box::new(static_expression::Spanned {
        span: template.span,
        persistence: template.persistence,
        node: StaticExpression::Literal(static_expression::Literal::Text(slice)),
    }))
}

fn eager_block_scope<'a>(
    variables: &'a [static_expression::Spanned<static_expression::Variable>],
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &LocalScopes<'a>,
    passed: &PassedScopes,
) -> BTreeMap<String, LocalBinding<'a>> {
    let mut scope = BTreeMap::new();
    for variable in variables {
        let mut eval_locals = locals.clone();
        eval_locals.push(scope.clone());
        let mut eval_passed = passed.clone();
        let eager_expr = if resolve_scalar_reference(
            &variable.node.value,
            context,
            &eval_locals,
            &eval_passed,
            &mut Vec::new(),
        )
        .ok()
        .flatten()
        .is_some()
        {
            Some(&variable.node.value)
        } else if let Ok(Some(value)) = initial_scalar_value_in_context(
            &variable.node.value,
            context,
            stack,
            &mut eval_locals,
            &mut eval_passed,
        ) {
            Some(synthetic_integer_expression(value, &variable.node.value))
        } else if let Ok(value) = lower_text_value(
            &variable.node.value,
            context,
            stack,
            &mut eval_locals,
            &mut eval_passed,
        ) {
            Some(synthetic_text_expression(&value, &variable.node.value))
        } else {
            Some(&variable.node.value)
        };
        let object_base = infer_argument_object_base(&variable.node.value, context, locals, passed)
            .or_else(|| active_object_scope(locals));
        scope.insert(
            variable.node.name.as_str().to_string(),
            LocalBinding {
                expr: eager_expr,
                object_base,
            },
        );
    }
    scope
}

fn invocation_marker<'a>(
    function_name: &str,
    arguments: &'a [static_expression::Spanned<StaticArgument>],
    passed_value: Option<&'a StaticSpannedExpression>,
    context: &'a LowerContext<'a>,
    locals: &LocalScopes<'a>,
    passed: &PassedScopes,
) -> Result<String, String> {
    let Some(function) = context.functions.get(function_name) else {
        return Ok(function_name.to_string());
    };
    let mut parts = Vec::new();
    if let [parameter] = function.parameters.as_slice() {
        if let Some(passed_value) = passed_value {
            let resolved = resolve_invocation_argument(
                parameter,
                passed_value,
                context,
                locals,
                passed,
                &mut Vec::new(),
            )?;
            parts.push(format!(
                "{parameter}={}",
                expression_fingerprint(resolved, context, locals, passed, &mut Vec::new())?
            ));
        }
    }
    for parameter in &function.parameters {
        if let Some(argument) = find_named_argument(arguments, parameter) {
            let resolved = resolve_invocation_argument(
                parameter,
                argument,
                context,
                locals,
                passed,
                &mut Vec::new(),
            )?;
            parts.push(format!(
                "{parameter}={}",
                expression_fingerprint(resolved, context, locals, passed, &mut Vec::new())?
            ));
        }
    }
    Ok(format!("{function_name}({})", parts.join(",")))
}

fn expression_fingerprint<'a>(
    expression: &'a StaticSpannedExpression,
    context: &'a LowerContext<'a>,
    locals: &LocalScopes<'a>,
    passed: &PassedScopes,
    stack: &mut Vec<String>,
) -> Result<String, String> {
    let expression = resolve_alias(expression, context, locals, passed, stack)?;
    match &expression.node {
        StaticExpression::Literal(static_expression::Literal::Number(number)) => {
            Ok(trim_number(*number))
        }
        StaticExpression::Literal(static_expression::Literal::Text(text))
        | StaticExpression::Literal(static_expression::Literal::Tag(text)) => {
            Ok(text.as_str().to_string())
        }
        StaticExpression::TextLiteral { parts, .. } => Ok(text_parts_fingerprint(parts)),
        StaticExpression::Block { variables, output } => {
            let mut nested_locals = locals.clone();
            let mut scope = BTreeMap::new();
            let mut variable_parts = Vec::with_capacity(variables.len());
            for variable in variables {
                variable_parts.push(format!(
                    "{}={}",
                    variable.node.name.as_str(),
                    expression_fingerprint(&variable.node.value, context, locals, passed, stack)?
                ));
                scope.insert(
                    variable.node.name.as_str().to_string(),
                    LocalBinding {
                        expr: Some(&variable.node.value),
                        object_base: infer_argument_object_base(
                            &variable.node.value,
                            context,
                            locals,
                            passed,
                        ),
                    },
                );
            }
            nested_locals.push(scope);
            Ok(format!(
                "block{{{}->{}}}",
                variable_parts.join(","),
                expression_fingerprint(output, context, &nested_locals, passed, stack)?
            ))
        }
        StaticExpression::ArithmeticOperator(static_expression::ArithmeticOperator::Add {
            operand_a,
            operand_b,
        }) => Ok(format!(
            "({}+{})",
            expression_fingerprint(operand_a, context, locals, passed, stack)?,
            expression_fingerprint(operand_b, context, locals, passed, stack)?,
        )),
        StaticExpression::ArithmeticOperator(static_expression::ArithmeticOperator::Subtract {
            operand_a,
            operand_b,
        }) => Ok(format!(
            "({}-{})",
            expression_fingerprint(operand_a, context, locals, passed, stack)?,
            expression_fingerprint(operand_b, context, locals, passed, stack)?,
        )),
        StaticExpression::FunctionCall { path, arguments }
            if path.len() == 1 && context.functions.contains_key(path[0].as_str()) =>
        {
            invocation_marker(path[0].as_str(), arguments, None, context, locals, passed)
        }
        StaticExpression::Pipe { from, to }
            if function_invocation_target(to, context, locals, passed, stack)?.is_some() =>
        {
            let Some((function_name, arguments)) =
                function_invocation_target(to, context, locals, passed, stack)?
            else {
                unreachable!();
            };
            invocation_marker(
                function_name,
                arguments,
                Some(from.as_ref()),
                context,
                locals,
                passed,
            )
        }
        _ => Ok(describe_expression_detailed(expression)),
    }
}

fn text_parts_fingerprint(parts: &[StaticTextPart]) -> String {
    let mut output = String::from("text{");
    for part in parts {
        match part {
            StaticTextPart::Text(text) => {
                output.push_str("t:");
                output.push_str(text.as_str());
                output.push('|');
            }
            StaticTextPart::Interpolation { var, .. } => {
                output.push_str("i:");
                output.push_str(var.as_str());
                output.push('|');
            }
        }
    }
    output.push('}');
    output
}

fn with_invoked_function_scope<'a, T>(
    function_name: &str,
    arguments: &'a [static_expression::Spanned<StaticArgument>],
    passed_value: Option<&'a StaticSpannedExpression>,
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
    run: impl FnOnce(
        &'a StaticSpannedExpression,
        &'a LowerContext<'a>,
        &mut Vec<String>,
        &mut LocalScopes<'a>,
        &mut PassedScopes,
    ) -> Result<T, String>,
) -> Result<T, String> {
    let function = context
        .functions
        .get(function_name)
        .ok_or_else(|| format!("unknown function `{function_name}`"))?;
    let mut scope = BTreeMap::new();

    match function.parameters.as_slice() {
        [parameter] if passed_value.is_some() => {
            let passed_value = passed_value.expect("passed value already checked");
            let resolved_passed = resolve_invocation_argument(
                parameter,
                passed_value,
                context,
                locals,
                passed,
                stack,
            )?;
            let object_base = infer_argument_object_base(resolved_passed, context, locals, passed);
            scope.insert(
                parameter.clone(),
                LocalBinding {
                    expr: Some(resolved_passed),
                    object_base,
                },
            );
        }
        _ if passed_value.is_some() => {
            return Err(format!(
                "pipe call into `{function_name}` requires exactly one parameter"
            ));
        }
        _ => {}
    }

    for parameter in &function.parameters {
        if let Some(argument) = find_named_argument(arguments, parameter) {
            let resolved_argument =
                resolve_invocation_argument(parameter, argument, context, locals, passed, stack)?;
            let object_base =
                infer_argument_object_base(resolved_argument, context, locals, passed);
            scope.insert(
                parameter.clone(),
                LocalBinding {
                    expr: Some(resolved_argument),
                    object_base,
                },
            );
        }
    }

    for parameter in &function.parameters {
        if !scope.contains_key(parameter) {
            return Err(format!(
                "function `{function_name}` requires argument `{parameter}`"
            ));
        }
    }

    let explicit_pass = find_named_argument(arguments, "PASS")
        .map(|argument| passed_scope_for_expression(argument, context, locals, passed, stack))
        .transpose()?;

    if let Some(pass_scope) = &explicit_pass {
        passed.push(pass_scope.clone());
    }
    locals.push(scope);
    let result = run(function.body, context, stack, locals, passed);
    locals.pop();
    if explicit_pass.is_some() {
        passed.pop();
    }
    result
}

fn resolve_invocation_argument<'a>(
    parameter: &str,
    argument: &'a StaticSpannedExpression,
    context: &'a LowerContext<'a>,
    locals: &LocalScopes<'a>,
    passed: &PassedScopes,
    stack: &mut Vec<String>,
) -> Result<&'a StaticSpannedExpression, String> {
    let StaticExpression::Alias(static_expression::Alias::WithoutPassed { parts, .. }) =
        &argument.node
    else {
        return Ok(argument);
    };
    if parts.len() != 1 || parts[0].as_str() != parameter {
        return Ok(argument);
    }
    if lookup_local_object_base(parameter, locals).is_some() {
        return Ok(argument);
    }
    let Some(local) = lookup_local_binding_expr(parameter, locals) else {
        return Ok(argument);
    };
    resolve_alias(local, context, locals, passed, stack)
}

fn infer_argument_object_base(
    argument: &StaticSpannedExpression,
    context: &LowerContext<'_>,
    locals: &LocalScopes<'_>,
    passed: &PassedScopes,
) -> Option<String> {
    if let StaticExpression::Alias(static_expression::Alias::WithoutPassed { parts, .. }) =
        &argument.node
    {
        if parts.len() == 1 {
            if let Some(base) = lookup_local_object_base(parts[0].as_str(), locals) {
                return Some(base.to_string());
            }
        }
    }
    let path =
        canonical_expression_path(argument, context, locals, passed, &mut Vec::new()).ok()?;
    let has_nested_fields = context
        .path_bindings
        .keys()
        .any(|candidate| candidate.starts_with(&format!("{path}.")));
    if has_nested_fields {
        return Some(path);
    }
    expression_depends_on_item_scope(argument, context, locals, passed)
        .then_some("__item__".to_string())
}

fn expression_depends_on_item_scope(
    expression: &StaticSpannedExpression,
    context: &LowerContext<'_>,
    locals: &LocalScopes<'_>,
    passed: &PassedScopes,
) -> bool {
    if placeholder_object_field(expression, context, locals, passed)
        .ok()
        .flatten()
        .is_some()
    {
        return true;
    }
    let expression = match resolve_alias(expression, context, locals, passed, &mut Vec::new()) {
        Ok(expression) => expression,
        Err(_) => expression,
    };
    match &expression.node {
        StaticExpression::Alias(static_expression::Alias::WithoutPassed { parts, .. }) => {
            parts.len() == 1 && lookup_local_object_base(parts[0].as_str(), locals).is_some()
        }
        StaticExpression::Pipe { from, to } => {
            expression_depends_on_item_scope(from, context, locals, passed)
                || expression_depends_on_item_scope(to, context, locals, passed)
        }
        StaticExpression::FunctionCall { arguments, .. } => arguments.iter().any(|argument| {
            argument
                .node
                .value
                .as_ref()
                .is_some_and(|value| {
                    expression_depends_on_item_scope(value, context, locals, passed)
                })
        }),
        StaticExpression::Block { variables, output } => {
            variables.iter().any(|variable| {
                expression_depends_on_item_scope(&variable.node.value, context, locals, passed)
            }) || expression_depends_on_item_scope(output, context, locals, passed)
        }
        StaticExpression::Latest { inputs } | StaticExpression::List { items: inputs } => inputs
            .iter()
            .any(|item| expression_depends_on_item_scope(item, context, locals, passed)),
        StaticExpression::When { arms } | StaticExpression::While { arms } => arms.iter().any(|arm| {
            expression_depends_on_item_scope(&arm.body, context, locals, passed)
        }),
        StaticExpression::Object(object) | StaticExpression::TaggedObject { object, .. } => object
            .variables
            .iter()
            .any(|variable| {
                expression_depends_on_item_scope(&variable.node.value, context, locals, passed)
            }),
        StaticExpression::TextLiteral { parts, .. } => parts.iter().any(|part| match part {
            StaticTextPart::Text(_) => false,
            StaticTextPart::Interpolation { var, .. } => {
                placeholder_object_field_name(var.as_str(), context, locals, passed).is_some()
            }
        }),
        StaticExpression::PostfixFieldAccess { expr, .. } => {
            expression_depends_on_item_scope(expr, context, locals, passed)
        }
        _ => false,
    }
}

fn passed_scope_for_expression(
    expression: &StaticSpannedExpression,
    context: &LowerContext<'_>,
    locals: &LocalScopes<'_>,
    passed: &PassedScopes,
    stack: &mut Vec<String>,
) -> Result<PassedScope, String> {
    if let Ok(path) = canonical_expression_path(expression, context, locals, passed, stack) {
        return Ok(PassedScope::Path(path));
    }
    let Some(object) = resolve_object(expression) else {
        return Err(format!(
            "expression `{}` cannot be used as a PASS context",
            describe_expression(expression)
        ));
    };
    let mut bindings = BTreeMap::new();
    for variable in &object.variables {
        if variable.node.name.is_empty() {
            return Err("spread entries are not supported in PASS objects yet".to_string());
        }
        let path = canonical_expression_path(&variable.node.value, context, locals, passed, stack)?;
        bindings.insert(variable.node.name.as_str().to_string(), path);
    }
    Ok(PassedScope::Bindings(bindings))
}

fn detect_scalar_plan<'a>(
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    functions: &BTreeMap<String, FunctionSpec<'a>>,
) -> Result<ScalarPlan, String> {
    let latest_value_specs = detect_latest_value_specs(path_bindings)?;
    let bool_specs = detect_bool_scalar_specs(path_bindings)?;
    let list_count_specs = detect_list_count_scalar_specs(path_bindings, functions)?;
    let arithmetic_specs = detect_arithmetic_scalar_specs(path_bindings, functions)?;
    let comparison_specs = detect_comparison_scalar_specs(path_bindings, functions)?;
    let selected_filter_specs = detect_selected_filter_specs(path_bindings)?;
    let mut plan = ScalarPlan {
        derived_scalars: detect_derived_scalar_specs(path_bindings, functions)?,
        ..ScalarPlan::default()
    };

    for (binding_name, spec) in &latest_value_specs {
        plan.initial_values
            .insert(binding_name.clone(), spec.initial_value);
        for event in &spec.event_values {
            push_event_update(
                &mut plan,
                &event.trigger_binding,
                &event.event_name,
                ScalarUpdate::Set {
                    binding: binding_name.clone(),
                    value: event.value,
                },
            );
        }
    }

    for (binding_name, spec) in &selected_filter_specs {
        plan.initial_values
            .insert(binding_name.clone(), spec.initial_value);
        for event in &spec.event_values {
            push_event_update(
                &mut plan,
                &event.trigger_binding,
                &event.event_name,
                ScalarUpdate::Set {
                    binding: binding_name.clone(),
                    value: event.value,
                },
            );
        }
    }

    for (binding_name, spec) in &bool_specs {
        plan.initial_values
            .insert(binding_name.clone(), i64::from(spec.initial));
        for event in &spec.events {
            let update = match &event.update {
                BoolEventUpdate::Toggle => ScalarUpdate::ToggleBool {
                    binding: binding_name.clone(),
                },
                BoolEventUpdate::Set(value) => ScalarUpdate::Set {
                    binding: binding_name.clone(),
                    value: i64::from(*value),
                },
            };
            push_event_update(&mut plan, &event.trigger_binding, &event.event_name, update);
        }
    }

    for (binding_name, value) in &list_count_specs {
        plan.initial_values
            .entry(binding_name.clone())
            .or_insert(*value);
    }

    for (binding_name, value) in &arithmetic_specs {
        plan.initial_values
            .entry(binding_name.clone())
            .or_insert(*value);
    }

    for (binding_name, value) in &comparison_specs {
        plan.initial_values
            .entry(binding_name.clone())
            .or_insert(*value);
    }

    for (binding_name, expression) in path_bindings {
        if let Some(spec) = counter_spec_for_expression(expression, path_bindings, binding_name)? {
            plan.initial_values
                .insert(binding_name.clone(), spec.initial);
            for event in spec.events {
                push_event_update(
                    &mut plan,
                    &event.trigger_binding,
                    &event.event_name,
                    ScalarUpdate::Add {
                        binding: binding_name.clone(),
                        delta: event.delta,
                    },
                );
            }
        }
        if let Some(source_binding) = sum_source_binding(expression, path_bindings, binding_name)? {
            let Some(source) = latest_value_specs.get(&source_binding) else {
                continue;
            };
            match plan.initial_values.entry(binding_name.clone()) {
                Entry::Vacant(entry) => {
                    entry.insert(source.static_sum);
                }
                Entry::Occupied(_) => {}
            }
            for event in &source.event_values {
                push_event_update(
                    &mut plan,
                    &event.trigger_binding,
                    &event.event_name,
                    ScalarUpdate::Add {
                        binding: binding_name.clone(),
                        delta: event.value,
                    },
                );
            }
        }
    }
    Ok(plan)
}

fn detect_text_plan(
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
) -> Result<TextPlan, String> {
    let mut plan = TextPlan::default();
    for (binding_name, expression) in path_bindings {
        if let Some((initial_value, event_updates)) =
            latest_text_spec_for_expression(expression, path_bindings, binding_name)?
        {
            plan.initial_values.insert(binding_name.clone(), initial_value);
            for ((trigger_binding, event_name), update) in event_updates {
                plan.event_updates
                    .entry((trigger_binding, event_name))
                    .or_default()
                    .push(update);
            }
            continue;
        }
        if let Some((initial_value, event_updates)) =
            hold_text_spec_for_expression(expression, path_bindings, binding_name)?
        {
            plan.initial_values.insert(binding_name.clone(), initial_value);
            for ((trigger_binding, event_name), update) in event_updates {
                plan.event_updates
                    .entry((trigger_binding, event_name))
                    .or_default()
                    .push(update);
            }
        }
    }
    Ok(plan)
}

fn detect_derived_scalar_specs(
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    functions: &BTreeMap<String, FunctionSpec<'_>>,
) -> Result<Vec<DerivedScalarSpec>, String> {
    let mut specs = Vec::new();
    for (binding_name, expression) in path_bindings {
        if let Some(spec) =
            derived_list_count_scalar_spec(expression, path_bindings, functions, binding_name)?
        {
            specs.push(spec);
            continue;
        }
        if let Some(spec) = derived_arithmetic_scalar_spec(expression, path_bindings, binding_name)?
        {
            specs.push(spec);
            continue;
        }
        if let Some(spec) = derived_comparison_scalar_spec(expression, path_bindings, binding_name)?
        {
            specs.push(spec);
        }
    }
    Ok(specs)
}

fn detect_bool_scalar_specs(
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
) -> Result<BTreeMap<String, BoolSpec>, String> {
    let mut specs = BTreeMap::new();
    for (binding_name, expression) in path_bindings {
        if let Some(spec) = detect_top_level_bool_spec(expression, path_bindings, binding_name)? {
            specs.insert(binding_name.clone(), spec);
        }
    }
    Ok(specs)
}

fn detect_top_level_bool_spec(
    expression: &StaticSpannedExpression,
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    binding_path: &str,
) -> Result<Option<BoolSpec>, String> {
    let StaticExpression::Pipe { from, to } = &expression.node else {
        return Ok(None);
    };
    let StaticExpression::Hold { body, .. } = &to.node else {
        return Ok(None);
    };
    let Some(initial) = extract_bool_literal_opt(from)? else {
        return Ok(None);
    };
    let mut events = Vec::new();
    match &body.node {
        StaticExpression::Pipe {
            from: trigger_source,
            to: trigger_then,
        } => {
            let Some(detected_events) = top_level_bool_event_spec(
                trigger_source,
                trigger_then,
                path_bindings,
                binding_path,
            )?
            else {
                return Ok(None);
            };
            events.extend(detected_events);
        }
        StaticExpression::Latest { inputs } => {
            for input in inputs {
                let StaticExpression::Pipe {
                    from: trigger_source,
                    to: trigger_then,
                } = &input.node
                else {
                    return Ok(None);
                };
                let Some(detected_events) = top_level_bool_event_spec(
                    trigger_source,
                    trigger_then,
                    path_bindings,
                    binding_path,
                )?
                else {
                    return Ok(None);
                };
                events.extend(detected_events);
            }
        }
        _ => return Ok(None),
    }
    if events.is_empty() {
        return Ok(None);
    }
    Ok(Some(BoolSpec { initial, events }))
}

fn top_level_bool_event_spec(
    trigger_source: &StaticSpannedExpression,
    trigger_then: &StaticSpannedExpression,
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    binding_path: &str,
) -> Result<Option<Vec<BoolEventSpec>>, String> {
    let Some((trigger_binding, event_name)) =
        canonical_event_source_path(trigger_source, path_bindings, binding_path)?
    else {
        return Ok(None);
    };
    match &trigger_then.node {
        StaticExpression::Then { body } => {
            let Some(update) = bool_event_update(body)? else {
                return Ok(None);
            };
            Ok(Some(vec![BoolEventSpec {
                trigger_binding,
                event_name,
                update,
                payload_filter: None,
            }]))
        }
        StaticExpression::When { arms } => {
            let mut events = Vec::new();
            for arm in arms {
                let static_expression::Pattern::Literal(pattern_literal) = &arm.pattern else {
                    continue;
                };
                let payload_filter = match pattern_literal {
                    static_expression::Literal::Tag(tag)
                    | static_expression::Literal::Text(tag) => tag.as_str().to_string(),
                    _ => continue,
                };
                let Some(value) = extract_bool_literal_opt(&arm.body)? else {
                    continue;
                };
                events.push(BoolEventSpec {
                    trigger_binding: trigger_binding.clone(),
                    event_name: event_name.clone(),
                    update: BoolEventUpdate::Set(value),
                    payload_filter: Some(payload_filter),
                });
            }
            if events.is_empty() {
                return Ok(None);
            }
            Ok(Some(events))
        }
        _ => Ok(None),
    }
}

fn detect_list_count_scalar_specs(
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    functions: &BTreeMap<String, FunctionSpec<'_>>,
) -> Result<BTreeMap<String, i64>, String> {
    let mut specs = BTreeMap::new();
    for (binding_name, expression) in path_bindings {
        if let Some(value) =
            list_count_scalar_value(expression, path_bindings, functions, binding_name)?
        {
            specs.insert(binding_name.clone(), value);
        }
    }
    Ok(specs)
}

fn list_count_scalar_value(
    expression: &StaticSpannedExpression,
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    functions: &BTreeMap<String, FunctionSpec<'_>>,
    binding_path: &str,
) -> Result<Option<i64>, String> {
    let StaticExpression::Pipe { from, to } = &expression.node else {
        return Ok(None);
    };
    let StaticExpression::FunctionCall { path, arguments } = &to.node else {
        return Ok(None);
    };
    if !path_matches(path, &["List", "count"]) || !arguments.is_empty() {
        return Ok(None);
    }
    if let Some(count) =
        filtered_object_list_count_scalar_value(from, path_bindings, functions, binding_path)?
    {
        return Ok(Some(count));
    }
    if let Some(binding) =
        runtime_object_list_binding_path_for_scalar(from, path_bindings, binding_path)?
    {
        return Ok(Some(
            path_bindings
                .get(&binding)
                .and_then(|source| {
                    detect_object_list_pipeline(source, path_bindings, functions, &binding)
                        .ok()
                        .flatten()
                        .map(|(items, _, _, _)| items.len() as i64)
                })
                .unwrap_or_default(),
        ));
    }
    if let Some(binding) = canonical_reference_path(from, path_bindings, binding_path)? {
        return Ok(Some(
            path_bindings
                .get(&binding)
                .and_then(|source| {
                    detect_text_list_pipeline(source, path_bindings, &binding)
                        .ok()
                        .flatten()
                        .map(|(items, _)| items.len() as i64)
                })
                .unwrap_or_default(),
        ));
    }
    Ok(None)
}

fn derived_list_count_scalar_spec(
    expression: &StaticSpannedExpression,
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    _functions: &BTreeMap<String, FunctionSpec<'_>>,
    binding_path: &str,
) -> Result<Option<DerivedScalarSpec>, String> {
    let StaticExpression::Pipe { from, to } = &expression.node else {
        return Ok(None);
    };
    let StaticExpression::FunctionCall { path, arguments } = &to.node else {
        return Ok(None);
    };
    if !path_matches(path, &["List", "count"]) || !arguments.is_empty() {
        return Ok(None);
    }
    if let Some((binding, filter)) = derived_object_list_ref(from, path_bindings, binding_path)? {
        return Ok(Some(DerivedScalarSpec::ObjectListCount {
            binding,
            filter,
            target: binding_path.to_string(),
        }));
    }
    if let Some((binding, filter)) = derived_text_list_ref(from, path_bindings, binding_path)? {
        return Ok(Some(DerivedScalarSpec::TextListCount {
            binding,
            filter,
            target: binding_path.to_string(),
        }));
    }
    Ok(None)
}

fn derived_object_list_ref(
    expression: &StaticSpannedExpression,
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    binding_path: &str,
) -> Result<Option<(String, Option<ObjectListFilter>)>, String> {
    if let Some(binding) =
        runtime_object_list_binding_path_for_scalar(expression, path_bindings, binding_path)?
    {
        return Ok(Some((binding, None)));
    }
    let StaticExpression::Pipe { from, to } = &expression.node else {
        return Ok(None);
    };
    let StaticExpression::FunctionCall { path, arguments } = &to.node else {
        return Ok(None);
    };
    if !path_matches(path, &["List", "retain"]) {
        return Ok(None);
    }
    let Some(binding) =
        runtime_object_list_binding_path_for_scalar(from, path_bindings, binding_path)?
    else {
        return Ok(None);
    };
    let item_name = find_positional_parameter_name(arguments)
        .ok_or_else(|| "List/retain requires an item parameter name".to_string())?;
    let condition = find_named_argument(arguments, "if")
        .ok_or_else(|| "List/retain requires `if`".to_string())?;
    if matches!(
        &condition.node,
        StaticExpression::Literal(static_expression::Literal::Tag(tag)) if tag.as_str() == "True"
    ) {
        return Ok(Some((binding, None)));
    }
    let Some(filter) =
        derived_object_list_filter(condition, item_name, path_bindings, binding_path)?
    else {
        return Ok(None);
    };
    Ok(Some((binding, Some(filter))))
}

fn derived_text_list_ref(
    expression: &StaticSpannedExpression,
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    binding_path: &str,
) -> Result<Option<(String, Option<TextListFilter>)>, String> {
    if let Some(binding) = canonical_reference_path(expression, path_bindings, binding_path)? {
        if let Some(source_expression) = path_bindings.get(&binding).copied() {
            if detect_text_list_pipeline(source_expression, path_bindings, &binding)?.is_some() {
                return Ok(Some((binding, None)));
            }
        }
    }
    let StaticExpression::Pipe { from, to } = &expression.node else {
        return Ok(None);
    };
    let StaticExpression::FunctionCall { path, arguments } = &to.node else {
        return Ok(None);
    };
    if !path_matches(path, &["List", "retain"]) {
        return Ok(None);
    }
    let Some((binding, None)) = derived_text_list_ref(from, path_bindings, binding_path)? else {
        return Ok(None);
    };
    let item_name = find_positional_parameter_name(arguments)
        .ok_or_else(|| "List/retain requires an item parameter name".to_string())?;
    let condition = find_named_argument(arguments, "if")
        .ok_or_else(|| "List/retain requires `if`".to_string())?;
    let filter = runtime_text_list_filter(condition, item_name)?;
    Ok(Some((binding, Some(filter))))
}

fn derived_object_list_filter(
    expression: &StaticSpannedExpression,
    item_name: &str,
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    binding_path: &str,
) -> Result<Option<ObjectListFilter>, String> {
    if let StaticExpression::Alias(static_expression::Alias::WithoutPassed { parts, .. }) =
        &expression.node
    {
        if parts.len() == 2 && parts[0].as_str() == item_name && parts[1].as_str() == "completed" {
            return Ok(Some(ObjectListFilter::BoolFieldEquals {
                field: "completed".to_string(),
                value: true,
            }));
        }
    }
    let StaticExpression::Pipe { from, to } = &expression.node else {
        return Ok(None);
    };
    if let Some(selector_binding) = canonical_reference_path(from, path_bindings, binding_path)? {
        if let StaticExpression::While { arms } = &to.node {
            if selected_completed_filter_arms(arms, item_name) {
                return Ok(Some(ObjectListFilter::SelectedCompletedByScalar {
                    binding: selector_binding,
                }));
            }
        }
    }
    let StaticExpression::Alias(static_expression::Alias::WithoutPassed { parts, .. }) = &from.node
    else {
        return Ok(None);
    };
    let StaticExpression::FunctionCall { path, arguments } = &to.node else {
        return Ok(None);
    };
    if !path_matches(path, &["Bool", "not"]) || !arguments.is_empty() {
        return Ok(None);
    }
    if parts.len() == 2 && parts[0].as_str() == item_name && parts[1].as_str() == "completed" {
        return Ok(Some(ObjectListFilter::BoolFieldEquals {
            field: "completed".to_string(),
            value: false,
        }));
    }
    Ok(None)
}

fn filtered_object_list_count_scalar_value(
    expression: &StaticSpannedExpression,
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    functions: &BTreeMap<String, FunctionSpec<'_>>,
    binding_path: &str,
) -> Result<Option<i64>, String> {
    let StaticExpression::Pipe { from, to } = &expression.node else {
        return Ok(None);
    };
    let StaticExpression::FunctionCall { path, arguments } = &to.node else {
        return Ok(None);
    };
    if !path_matches(path, &["List", "retain"]) {
        return Ok(None);
    }
    let item_name = find_positional_parameter_name(arguments)
        .ok_or_else(|| "List/retain requires an item parameter name".to_string())?;
    let condition = find_named_argument(arguments, "if")
        .ok_or_else(|| "List/retain requires `if`".to_string())?;
    let Some(binding) = canonical_reference_path(from, path_bindings, binding_path)? else {
        return Ok(None);
    };
    let Some(source_expression) = path_bindings.get(&binding).copied() else {
        return Ok(None);
    };
    let Some((items, _, _, _)) =
        detect_object_list_pipeline(source_expression, path_bindings, functions, &binding)?
    else {
        return Ok(None);
    };
    let keep_completed = match &condition.node {
        StaticExpression::Alias(static_expression::Alias::WithoutPassed { parts, .. })
            if parts.len() == 2
                && parts[0].as_str() == item_name
                && parts[1].as_str() == "completed" =>
        {
            Some(true)
        }
        StaticExpression::Pipe { from, to }
            if matches!(
                (&from.node, &to.node),
                (
                    StaticExpression::Alias(static_expression::Alias::WithoutPassed {
                        parts,
                        ..
                    }),
                    StaticExpression::FunctionCall { path, arguments },
                ) if parts.len() == 2
                    && parts[0].as_str() == item_name
                    && parts[1].as_str() == "completed"
                    && path_matches(path, &["Bool", "not"])
                    && arguments.is_empty()
            ) =>
        {
            Some(false)
        }
        _ => None,
    };
    let Some(keep_completed) = keep_completed else {
        return Ok(None);
    };
    Ok(Some(
        items
            .iter()
            .filter(|item| item.completed == keep_completed)
            .count() as i64,
    ))
}

fn detect_arithmetic_scalar_specs(
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    functions: &BTreeMap<String, FunctionSpec<'_>>,
) -> Result<BTreeMap<String, i64>, String> {
    let mut specs = BTreeMap::new();
    for (binding_name, expression) in path_bindings {
        if let Some(value) =
            arithmetic_scalar_value(expression, path_bindings, functions, binding_name)?
        {
            specs.insert(binding_name.clone(), value);
        }
    }
    Ok(specs)
}

fn arithmetic_scalar_value(
    expression: &StaticSpannedExpression,
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    functions: &BTreeMap<String, FunctionSpec<'_>>,
    binding_path: &str,
) -> Result<Option<i64>, String> {
    let StaticExpression::ArithmeticOperator(operator) = &expression.node else {
        return Ok(None);
    };
    let (operand_a, operand_b, sign) = match operator {
        static_expression::ArithmeticOperator::Add {
            operand_a,
            operand_b,
        } => (operand_a.as_ref(), operand_b.as_ref(), 1),
        static_expression::ArithmeticOperator::Subtract {
            operand_a,
            operand_b,
        } => (operand_a.as_ref(), operand_b.as_ref(), -1),
        _ => return Ok(None),
    };
    let Some(value_a) = scalar_operand_value(operand_a, path_bindings, functions, binding_path)?
    else {
        return Ok(None);
    };
    let Some(value_b) = scalar_operand_value(operand_b, path_bindings, functions, binding_path)?
    else {
        return Ok(None);
    };
    Ok(Some(value_a + sign * value_b))
}

fn derived_arithmetic_scalar_spec(
    expression: &StaticSpannedExpression,
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    binding_path: &str,
) -> Result<Option<DerivedScalarSpec>, String> {
    let StaticExpression::ArithmeticOperator(operator) = &expression.node else {
        return Ok(None);
    };
    let (left_expr, right_expr, op) = match operator {
        static_expression::ArithmeticOperator::Add {
            operand_a,
            operand_b,
        } => (
            operand_a.as_ref(),
            operand_b.as_ref(),
            DerivedArithmeticOp::Add,
        ),
        static_expression::ArithmeticOperator::Subtract {
            operand_a,
            operand_b,
        } => (
            operand_a.as_ref(),
            operand_b.as_ref(),
            DerivedArithmeticOp::Subtract,
        ),
        _ => return Ok(None),
    };
    let Some(left) = derived_scalar_operand(left_expr, path_bindings, binding_path)? else {
        return Ok(None);
    };
    let Some(right) = derived_scalar_operand(right_expr, path_bindings, binding_path)? else {
        return Ok(None);
    };
    Ok(Some(DerivedScalarSpec::Arithmetic {
        target: binding_path.to_string(),
        op,
        left,
        right,
    }))
}

fn detect_comparison_scalar_specs(
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    functions: &BTreeMap<String, FunctionSpec<'_>>,
) -> Result<BTreeMap<String, i64>, String> {
    let mut specs = BTreeMap::new();
    for (binding_name, expression) in path_bindings {
        if let Some(value) =
            comparison_scalar_value(expression, path_bindings, functions, binding_name)?
        {
            specs.insert(binding_name.clone(), value);
        }
    }
    Ok(specs)
}

fn comparison_scalar_value(
    expression: &StaticSpannedExpression,
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    functions: &BTreeMap<String, FunctionSpec<'_>>,
    binding_path: &str,
) -> Result<Option<i64>, String> {
    let StaticExpression::Comparator(comparator) = &expression.node else {
        return Ok(None);
    };
    let (operand_a, operand_b, op) = match comparator {
        static_expression::Comparator::Equal {
            operand_a,
            operand_b,
        } => (operand_a.as_ref(), operand_b.as_ref(), IntCompareOp::Equal),
        static_expression::Comparator::NotEqual {
            operand_a,
            operand_b,
        } => (
            operand_a.as_ref(),
            operand_b.as_ref(),
            IntCompareOp::NotEqual,
        ),
        static_expression::Comparator::Greater {
            operand_a,
            operand_b,
        } => (
            operand_a.as_ref(),
            operand_b.as_ref(),
            IntCompareOp::Greater,
        ),
        static_expression::Comparator::GreaterOrEqual {
            operand_a,
            operand_b,
        } => (
            operand_a.as_ref(),
            operand_b.as_ref(),
            IntCompareOp::GreaterOrEqual,
        ),
        static_expression::Comparator::Less {
            operand_a,
            operand_b,
        } => (operand_a.as_ref(), operand_b.as_ref(), IntCompareOp::Less),
        static_expression::Comparator::LessOrEqual {
            operand_a,
            operand_b,
        } => (
            operand_a.as_ref(),
            operand_b.as_ref(),
            IntCompareOp::LessOrEqual,
        ),
    };
    let Some(value_a) = scalar_operand_value(operand_a, path_bindings, functions, binding_path)?
    else {
        return Ok(None);
    };
    let Some(value_b) = scalar_operand_value(operand_b, path_bindings, functions, binding_path)?
    else {
        return Ok(None);
    };
    let matched = match op {
        IntCompareOp::Equal => value_a == value_b,
        IntCompareOp::NotEqual => value_a != value_b,
        IntCompareOp::Greater => value_a > value_b,
        IntCompareOp::GreaterOrEqual => value_a >= value_b,
        IntCompareOp::Less => value_a < value_b,
        IntCompareOp::LessOrEqual => value_a <= value_b,
    };
    Ok(Some(i64::from(matched)))
}

fn derived_comparison_scalar_spec(
    expression: &StaticSpannedExpression,
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    binding_path: &str,
) -> Result<Option<DerivedScalarSpec>, String> {
    let StaticExpression::Comparator(comparator) = &expression.node else {
        return Ok(None);
    };
    let (left_expr, right_expr, op) = match comparator {
        static_expression::Comparator::Equal {
            operand_a,
            operand_b,
        } => (operand_a.as_ref(), operand_b.as_ref(), IntCompareOp::Equal),
        static_expression::Comparator::NotEqual {
            operand_a,
            operand_b,
        } => (
            operand_a.as_ref(),
            operand_b.as_ref(),
            IntCompareOp::NotEqual,
        ),
        static_expression::Comparator::Greater {
            operand_a,
            operand_b,
        } => (
            operand_a.as_ref(),
            operand_b.as_ref(),
            IntCompareOp::Greater,
        ),
        static_expression::Comparator::GreaterOrEqual {
            operand_a,
            operand_b,
        } => (
            operand_a.as_ref(),
            operand_b.as_ref(),
            IntCompareOp::GreaterOrEqual,
        ),
        static_expression::Comparator::Less {
            operand_a,
            operand_b,
        } => (operand_a.as_ref(), operand_b.as_ref(), IntCompareOp::Less),
        static_expression::Comparator::LessOrEqual {
            operand_a,
            operand_b,
        } => (
            operand_a.as_ref(),
            operand_b.as_ref(),
            IntCompareOp::LessOrEqual,
        ),
    };
    let Some(left) = derived_scalar_operand(left_expr, path_bindings, binding_path)? else {
        return Ok(None);
    };
    let Some(right) = derived_scalar_operand(right_expr, path_bindings, binding_path)? else {
        return Ok(None);
    };
    Ok(Some(DerivedScalarSpec::Comparison {
        target: binding_path.to_string(),
        op,
        left,
        right,
    }))
}

fn derived_scalar_operand(
    expression: &StaticSpannedExpression,
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    binding_path: &str,
) -> Result<Option<DerivedScalarOperand>, String> {
    if let Some(value) = extract_integer_literal_opt(expression)? {
        return Ok(Some(DerivedScalarOperand::Literal(value)));
    }
    if let Some(value) = extract_bool_literal_opt(expression)? {
        return Ok(Some(DerivedScalarOperand::Literal(i64::from(value))));
    }
    if let Some(value) = extract_filter_tag_value(expression)? {
        return Ok(Some(DerivedScalarOperand::Literal(value)));
    }
    if let Some(binding) = canonical_reference_path(expression, path_bindings, binding_path)? {
        return Ok(Some(DerivedScalarOperand::Binding(binding)));
    }
    Ok(None)
}

fn scalar_operand_value(
    expression: &StaticSpannedExpression,
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    functions: &BTreeMap<String, FunctionSpec<'_>>,
    binding_path: &str,
) -> Result<Option<i64>, String> {
    if let Some(value) = extract_integer_literal_opt(expression)? {
        return Ok(Some(value));
    }
    if let Some(value) = extract_bool_literal_opt(expression)? {
        return Ok(Some(i64::from(value)));
    }
    if let Some(value) = extract_filter_tag_value(expression)? {
        return Ok(Some(value));
    }
    if let Some(path) = canonical_reference_path(expression, path_bindings, binding_path)? {
        if let Some(source_expression) = path_bindings.get(&path).copied() {
            if let Some(value) =
                list_count_scalar_value(source_expression, path_bindings, functions, &path)?
            {
                return Ok(Some(value));
            }
            if let Some(value) =
                arithmetic_scalar_value(source_expression, path_bindings, functions, &path)?
            {
                return Ok(Some(value));
            }
            if let Some(value) =
                comparison_scalar_value(source_expression, path_bindings, functions, &path)?
            {
                return Ok(Some(value));
            }
            if let Some(spec) =
                selected_filter_spec_for_expression(source_expression, path_bindings, &path)?
            {
                return Ok(Some(spec.initial_value));
            }
            if let Some(spec) =
                latest_value_spec_for_expression(source_expression, path_bindings, &path)?
            {
                return Ok(Some(spec.initial_value));
            }
            if let Some(spec) = detect_top_level_bool_spec(source_expression, path_bindings, &path)?
            {
                return Ok(Some(i64::from(spec.initial)));
            }
        }
    }
    Ok(None)
}

fn runtime_object_list_binding_path_for_scalar(
    expression: &StaticSpannedExpression,
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    binding_path: &str,
) -> Result<Option<String>, String> {
    let Some(path) = canonical_reference_path(expression, path_bindings, binding_path)? else {
        return Ok(None);
    };
    if path_bindings.contains_key(&path) {
        return Ok(Some(path));
    }
    Ok(None)
}

fn detect_static_object_list_plan<'a>(
    path_bindings: &BTreeMap<String, &'a StaticSpannedExpression>,
    functions: &BTreeMap<String, FunctionSpec<'a>>,
    scalar_plan: &mut ScalarPlan,
) -> Result<StaticObjectListPlan, String> {
    let mut plan = StaticObjectListPlan::default();
    for (binding_path, expression) in path_bindings {
        let Some(detected) = detect_static_object_list_spec(expression, functions)? else {
            continue;
        };
        let mut item_bases = Vec::with_capacity(detected.items.len());
        for (index, spec) in detected.items.into_iter().enumerate() {
            let base = format!("{binding_path}[{index}]");
            if let Some(counter) = spec.counter {
                let count_binding = format!("{base}.count");
                scalar_plan
                    .initial_values
                    .insert(count_binding.clone(), counter.initial);
                for event in counter.events {
                    push_event_update(
                        scalar_plan,
                        &format!("{base}.{}", event.trigger_binding),
                        &event.event_name,
                        ScalarUpdate::Add {
                            binding: count_binding.clone(),
                            delta: event.delta,
                        },
                    );
                }
            }
            if let Some(boolean) = spec.boolean {
                let bool_binding = format!("{base}.checked");
                scalar_plan
                    .initial_values
                    .insert(bool_binding.clone(), i64::from(boolean.initial));
                for event in boolean.events {
                    push_event_update(
                        scalar_plan,
                        &format!("{base}.{}", event.trigger_binding),
                        &event.event_name,
                        ScalarUpdate::ToggleBool {
                            binding: bool_binding.clone(),
                        },
                    );
                }
            }
            if let Some(remove) = detected.remove.as_ref() {
                let removed_binding = format!("{base}.__removed");
                scalar_plan
                    .initial_values
                    .insert(removed_binding.clone(), 0);
                push_event_update(
                    scalar_plan,
                    &format!("{base}.{}", remove.trigger_binding),
                    &remove.event_name,
                    ScalarUpdate::Set {
                        binding: removed_binding,
                        value: 1,
                    },
                );
            }
            item_bases.push(base);
        }
        if !item_bases.is_empty() {
            plan.item_bases.insert(binding_path.clone(), item_bases);
        }
    }
    Ok(plan)
}

fn detect_static_object_list_spec<'a>(
    expression: &'a StaticSpannedExpression,
    functions: &BTreeMap<String, FunctionSpec<'a>>,
) -> Result<Option<DetectedStaticObjectList>, String> {
    match &expression.node {
        StaticExpression::List { items } => {
            if items.is_empty() {
                return Ok(None);
            }

            let mut specs = Vec::with_capacity(items.len());
            for item in items {
                let Some(spec) = detect_static_object_runtime_item(item, functions)? else {
                    return Ok(None);
                };
                specs.push(spec);
            }
            Ok(Some(DetectedStaticObjectList {
                items: specs,
                remove: None,
            }))
        }
        StaticExpression::Pipe { from, to } => {
            let Some(mut detected) = detect_static_object_list_spec(from, functions)? else {
                return Ok(None);
            };
            let StaticExpression::FunctionCall { path, arguments } = &to.node else {
                return Ok(None);
            };
            if !path_matches(path, &["List", "remove"]) {
                return Ok(None);
            }
            if detected.remove.is_some() {
                return Ok(None);
            }
            let item_name = find_positional_parameter_name(arguments)
                .ok_or_else(|| "List/remove requires an item parameter name".to_string())?;
            let on = find_named_argument(arguments, "on")
                .ok_or_else(|| "List/remove requires `on`".to_string())?;
            let Some(remove) = detect_static_object_remove_spec(on, item_name)? else {
                return Ok(None);
            };
            detected.remove = Some(remove);
            Ok(Some(detected))
        }
        _ => Ok(None),
    }
}

fn detect_static_object_runtime_item<'a>(
    expression: &'a StaticSpannedExpression,
    functions: &BTreeMap<String, FunctionSpec<'a>>,
) -> Result<Option<StaticObjectRuntimeSpec>, String> {
    let object = resolve_static_object_runtime_expression(expression, functions)?;
    let Some(object) = object else {
        return Ok(None);
    };

    let counter = find_object_field(object, "count")
        .map(detect_local_counter_spec)
        .transpose()?
        .flatten();
    let boolean = find_object_field(object, "checked")
        .map(detect_local_bool_spec)
        .transpose()?
        .flatten();

    Ok(Some(StaticObjectRuntimeSpec { counter, boolean }))
}

fn detect_static_object_remove_spec(
    expression: &StaticSpannedExpression,
    item_name: &str,
) -> Result<Option<StaticObjectRemoveSpec>, String> {
    let StaticExpression::Alias(static_expression::Alias::WithoutPassed { parts, .. }) =
        &expression.node
    else {
        return Ok(None);
    };
    if parts.len() < 4 || parts[0].as_str() != item_name {
        return Ok(None);
    }
    let event_index = parts.len() - 2;
    if parts[event_index].as_str() != "event" {
        return Ok(None);
    }
    let trigger_binding = parts[1..event_index]
        .iter()
        .map(crate::parser::StrSlice::as_str)
        .collect::<Vec<_>>()
        .join(".");
    if trigger_binding.is_empty() {
        return Ok(None);
    }
    Ok(Some(StaticObjectRemoveSpec {
        trigger_binding,
        event_name: parts[event_index + 1].as_str().to_string(),
    }))
}

#[derive(Debug, Clone)]
struct StaticObjectRuntimeSpec {
    counter: Option<CounterSpec>,
    boolean: Option<BoolSpec>,
}

#[derive(Debug, Clone)]
struct StaticObjectRemoveSpec {
    trigger_binding: String,
    event_name: String,
}

#[derive(Debug, Clone)]
struct BoolSpec {
    initial: bool,
    events: Vec<BoolEventSpec>,
}

#[derive(Debug, Clone)]
struct BoolEventSpec {
    trigger_binding: String,
    event_name: String,
    update: BoolEventUpdate,
    payload_filter: Option<String>,
}

#[derive(Debug, Clone)]
enum BoolEventUpdate {
    Toggle,
    Set(bool),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PendingObjectGlobalAction {
    ToggleAllCompleted,
}

fn resolve_static_object_runtime_expression<'a>(
    expression: &'a StaticSpannedExpression,
    functions: &BTreeMap<String, FunctionSpec<'a>>,
) -> Result<Option<&'a StaticObject>, String> {
    if let Some(object) = resolve_object(expression) {
        return Ok(Some(object));
    }
    let StaticExpression::FunctionCall { path, arguments } = &expression.node else {
        return Ok(None);
    };
    if path.len() != 1 {
        return Ok(None);
    }
    let Some(function) = functions.get(path[0].as_str()) else {
        return Ok(None);
    };
    if function.parameters.len() != arguments.len() {
        return Ok(None);
    }
    Ok(resolve_object(function.body))
}

fn resolve_static_object_field_expression<'a>(
    expression: &'a StaticSpannedExpression,
    functions: &BTreeMap<String, FunctionSpec<'a>>,
    field_name: &str,
) -> Option<&'a StaticSpannedExpression> {
    if let Some(object) = resolve_object(expression) {
        return find_object_field(object, field_name);
    }
    let StaticExpression::FunctionCall { path, arguments } = &expression.node else {
        return None;
    };
    if path.len() != 1 {
        return None;
    }
    let function = functions.get(path[0].as_str())?;
    let object = resolve_object(function.body)?;
    let field = find_object_field(object, field_name)?;
    if let StaticExpression::Alias(static_expression::Alias::WithoutPassed { parts, .. }) =
        &field.node
    {
        if parts.len() == 1 {
            return find_named_argument(arguments, parts[0].as_str()).or(Some(field));
        }
    }
    Some(field)
}

fn detect_local_counter_spec(
    expression: &StaticSpannedExpression,
) -> Result<Option<CounterSpec>, String> {
    let StaticExpression::Pipe { from, to } = &expression.node else {
        return Ok(None);
    };
    let StaticExpression::Hold { state_param, body } = &to.node else {
        return Ok(None);
    };
    let initial = extract_integer_literal(from)?;
    let mut events = Vec::new();
    match &body.node {
        StaticExpression::Pipe {
            from: trigger_source,
            to: trigger_then,
        } => {
            let Some(event) =
                local_counter_event_spec(trigger_source, trigger_then, state_param.as_str())?
            else {
                return Ok(None);
            };
            events.push(event);
        }
        StaticExpression::Latest { inputs } => {
            for input in inputs {
                let StaticExpression::Pipe {
                    from: trigger_source,
                    to: trigger_then,
                } = &input.node
                else {
                    return Ok(None);
                };
                let Some(event) =
                    local_counter_event_spec(trigger_source, trigger_then, state_param.as_str())?
                else {
                    return Ok(None);
                };
                events.push(event);
            }
        }
        _ => return Ok(None),
    }

    if events.is_empty() {
        return Ok(None);
    }
    Ok(Some(CounterSpec { initial, events }))
}

fn local_counter_event_spec(
    trigger_source: &StaticSpannedExpression,
    trigger_then: &StaticSpannedExpression,
    state_param: &str,
) -> Result<Option<EventDeltaSpec>, String> {
    let Some((trigger_binding, event_name)) = object_event_source_from_expression(trigger_source)?
    else {
        return Ok(None);
    };
    let StaticExpression::Then { body } = &trigger_then.node else {
        return Ok(None);
    };
    Ok(Some(EventDeltaSpec {
        trigger_binding,
        event_name,
        delta: extract_then_delta(body, Some(state_param))?,
    }))
}

fn detect_local_bool_spec(
    expression: &StaticSpannedExpression,
) -> Result<Option<BoolSpec>, String> {
    let StaticExpression::Pipe { from, to } = &expression.node else {
        return Ok(None);
    };
    let StaticExpression::Hold { body, .. } = &to.node else {
        return Ok(None);
    };
    let initial = extract_bool_literal(from)?;
    let mut events = Vec::new();
    match &body.node {
        StaticExpression::Pipe {
            from: trigger_source,
            to: trigger_then,
        } => {
            let Some(detected_events) = local_bool_event_spec(trigger_source, trigger_then)? else {
                return Ok(None);
            };
            events.extend(detected_events);
        }
        StaticExpression::Latest { inputs } => {
            for input in inputs {
                let StaticExpression::Pipe {
                    from: trigger_source,
                    to: trigger_then,
                } = &input.node
                else {
                    return Ok(None);
                };
                let Some(detected_events) = local_bool_event_spec(trigger_source, trigger_then)?
                else {
                    return Ok(None);
                };
                events.extend(detected_events);
            }
        }
        _ => return Ok(None),
    }
    if events.is_empty() {
        return Ok(None);
    }
    Ok(Some(BoolSpec { initial, events }))
}

fn local_bool_event_spec(
    trigger_source: &StaticSpannedExpression,
    trigger_then: &StaticSpannedExpression,
) -> Result<Option<Vec<BoolEventSpec>>, String> {
    let Some((trigger_binding, event_name)) = object_event_source_from_expression(trigger_source)?
    else {
        return Ok(None);
    };
    match &trigger_then.node {
        StaticExpression::Then { body } => {
            let Some(update) = bool_event_update(body)? else {
                return Ok(None);
            };
            Ok(Some(vec![BoolEventSpec {
                trigger_binding,
                event_name,
                update,
                payload_filter: None,
            }]))
        }
        StaticExpression::When { arms } => {
            let mut events = Vec::new();
            for arm in arms {
                let static_expression::Pattern::Literal(pattern_literal) = &arm.pattern else {
                    continue;
                };
                let payload_filter = match pattern_literal {
                    static_expression::Literal::Tag(tag)
                    | static_expression::Literal::Text(tag) => tag.as_str().to_string(),
                    _ => continue,
                };
                let Some(value) = extract_bool_literal_opt(&arm.body)? else {
                    continue;
                };
                events.push(BoolEventSpec {
                    trigger_binding: trigger_binding.clone(),
                    event_name: event_name.clone(),
                    update: BoolEventUpdate::Set(value),
                    payload_filter: Some(payload_filter),
                });
            }
            if events.is_empty() {
                return Ok(None);
            }
            Ok(Some(events))
        }
        _ => Ok(None),
    }
}

fn bool_event_update(
    expression: &StaticSpannedExpression,
) -> Result<Option<BoolEventUpdate>, String> {
    if let Some(value) = extract_bool_literal_opt(expression)? {
        return Ok(Some(BoolEventUpdate::Set(value)));
    }
    if matches!(
        &expression.node,
        StaticExpression::Pipe { from: _, to }
            if matches!(
                &to.node,
                StaticExpression::FunctionCall { path, arguments }
                    if path_matches(path, &["Bool", "not"]) && arguments.is_empty()
            )
    ) {
        return Ok(Some(BoolEventUpdate::Toggle));
    }
    Ok(None)
}

fn object_event_source_from_expression(
    expression: &StaticSpannedExpression,
) -> Result<Option<(String, String)>, String> {
    let StaticExpression::Alias(static_expression::Alias::WithoutPassed { parts, .. }) =
        &expression.node
    else {
        return Ok(None);
    };
    if parts.len() < 3 {
        return Ok(None);
    }
    let event_index = if parts.len() >= 4 && parts[parts.len() - 3].as_str() == "event" {
        parts.len() - 3
    } else {
        let event_index = parts.len() - 2;
        if parts[event_index].as_str() != "event" {
            return Ok(None);
        }
        event_index
    };
    let trigger_binding = parts[..event_index]
        .iter()
        .map(crate::parser::StrSlice::as_str)
        .collect::<Vec<_>>()
        .join(".");
    if trigger_binding.is_empty() {
        return Ok(None);
    }
    Ok(Some((
        trigger_binding,
        parts[event_index + 1].as_str().to_string(),
    )))
}

fn object_event_payload_source_from_expression(
    expression: &StaticSpannedExpression,
) -> Result<Option<(String, String, String)>, String> {
    let StaticExpression::Alias(static_expression::Alias::WithoutPassed { parts, .. }) =
        &expression.node
    else {
        return Ok(None);
    };
    if parts.len() < 4 {
        return Ok(None);
    }
    let event_index = if parts[parts.len() - 3].as_str() == "event" {
        parts.len() - 3
    } else {
        return Ok(None);
    };
    let trigger_binding = parts[..event_index]
        .iter()
        .map(crate::parser::StrSlice::as_str)
        .collect::<Vec<_>>()
        .join(".");
    if trigger_binding.is_empty() {
        return Ok(None);
    }
    Ok(Some((
        trigger_binding,
        parts[event_index + 1].as_str().to_string(),
        parts[event_index + 2].as_str().to_string(),
    )))
}

fn detect_list_plan(
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
) -> Result<ListPlan, String> {
    let mut plan = ListPlan::default();
    for (binding_name, expression) in path_bindings {
        let Some((initial_items, updates)) =
            detect_text_list_pipeline(expression, path_bindings, binding_name)?
        else {
            continue;
        };
        if updates.is_empty() {
            continue;
        }
        plan.initial_values
            .insert(binding_name.clone(), initial_items);
        for ((trigger_binding, event_name), update) in updates {
            plan.event_updates
                .entry((trigger_binding, event_name))
                .or_default()
                .push(update);
        }
    }
    Ok(plan)
}

type ListEventUpdates = Vec<((String, String), TextListUpdate)>;

type ObjectListEventUpdates = Vec<((String, String), ObjectListUpdate)>;
type PendingGlobalObjectUpdates = Vec<(String, String, PendingObjectGlobalAction)>;

fn detect_object_list_plan<'a>(
    path_bindings: &BTreeMap<String, &'a StaticSpannedExpression>,
    functions: &BTreeMap<String, FunctionSpec<'a>>,
) -> Result<ObjectListPlan, String> {
    let mut plan = ObjectListPlan::default();
    for (binding_name, expression) in path_bindings {
        let Some((initial_items, updates, item_actions, global_actions)) =
            detect_object_list_pipeline(expression, path_bindings, functions, binding_name)?
        else {
            continue;
        };
        plan.initial_values
            .insert(binding_name.clone(), initial_items);
        if !item_actions.is_empty() {
            plan.item_actions.insert(binding_name.clone(), item_actions);
        }
        for ((trigger_binding, event_name), update) in updates {
            plan.event_updates
                .entry((trigger_binding, event_name))
                .or_default()
                .push(update);
        }
        for (trigger_binding, event_name, action) in global_actions {
            let update = match action {
                PendingObjectGlobalAction::ToggleAllCompleted => {
                    ObjectListUpdate::ToggleAllBoolField {
                        binding: binding_name.clone(),
                        field: "completed".to_string(),
                    }
                }
            };
            plan.event_updates
                .entry((trigger_binding, event_name))
                .or_default()
                .push(update);
        }
    }
    for (binding_name, expression) in path_bindings {
        if plan.initial_values.contains_key(binding_name) {
            continue;
        }
        let Some(initial_items) =
            detect_static_mapped_object_list_placeholders(expression, functions)?
        else {
            continue;
        };
        plan.initial_values
            .insert(binding_name.clone(), initial_items);
    }
    Ok(plan)
}

fn augment_top_level_object_field_runtime(context: &mut LowerContext<'_>) -> Result<(), String> {
    let binding_names = context.bindings.keys().cloned().collect::<Vec<_>>();
    let mut plans = Vec::new();
    for binding_name in &binding_names {
        let Some(expression) = context.bindings.get(binding_name).copied() else {
            continue;
        };
        let field_kinds =
            detect_initial_object_field_kinds(expression, context, &mut Vec::new(), &mut Vec::new(), &mut Vec::new())?;
        if field_kinds.is_empty() {
            continue;
        }
        let plan = detect_top_level_object_field_plan(
            binding_name,
            expression,
            context,
            &field_kinds,
            &mut Vec::new(),
            &mut Vec::new(),
            &mut Vec::new(),
            None,
        )?;
        plans.push(plan);
    }
    for plan in plans {
        for (binding, value) in plan.scalar_initials {
            context.scalar_plan.initial_values.entry(binding).or_insert(value);
        }
        for (binding, value) in plan.text_initials {
            context.text_plan.initial_values.entry(binding).or_insert(value);
        }
        for (binding, actions) in plan.item_actions {
            context
                .object_list_plan
                .item_actions
                .entry(binding)
                .or_default()
                .extend(actions);
        }
    }
    Ok(())
}

fn augment_top_level_bool_item_runtime(context: &mut LowerContext<'_>) -> Result<(), String> {
    let binding_names = context.bindings.keys().cloned().collect::<Vec<_>>();
    for binding_name in &binding_names {
        let Some(expression) = context.bindings.get(binding_name).copied() else {
            continue;
        };
        let item_actions = detect_top_level_bool_item_actions(
            binding_name,
            expression,
            context,
            &mut Vec::new(),
            &mut Vec::new(),
            &mut Vec::new(),
            None,
        )?;
        if item_actions.is_empty() {
            continue;
        }
        context
            .scalar_plan
            .initial_values
            .entry(binding_name.clone())
            .or_insert(0);
        for (binding, actions) in item_actions {
            context
                .object_list_plan
                .item_actions
                .entry(binding)
                .or_default()
                .extend(actions);
        }
    }
    Ok(())
}

fn detect_initial_object_field_kinds<'a>(
    expression: &'a StaticSpannedExpression,
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<BTreeMap<String, ObjectFieldKind>, String> {
    let mut kinds = BTreeMap::new();
    let expression = resolve_alias(expression, context, locals, passed, stack)?;
    match &expression.node {
        StaticExpression::Object(object) => {
            for variable in &object.variables {
                if variable.node.name.is_empty() {
                    continue;
                }
                let field_name = variable.node.name.as_str().to_string();
                if extract_integer_literal_opt(&variable.node.value)?.is_some()
                    || extract_bool_literal_opt(&variable.node.value)?.is_some()
                {
                    kinds.insert(field_name, ObjectFieldKind::Scalar);
                } else if extract_static_text_value(&variable.node.value)?.is_some()
                    || is_event_text_expression(
                        &variable.node.value,
                        context,
                        locals,
                        passed,
                    )?
                    || expression_path(&variable.node.value, context, locals, passed)?
                        .as_deref()
                        .is_some_and(|path| path.ends_with(".text"))
                {
                    kinds.insert(field_name, ObjectFieldKind::Text);
                } else if placeholder_object_field(&variable.node.value, context, locals, passed)?
                    .is_some()
                    || expression_path(&variable.node.value, context, locals, passed)?
                        .as_deref()
                        .is_some_and(|path| path.ends_with(".row") || path.ends_with(".column"))
                {
                    kinds.insert(field_name, ObjectFieldKind::Scalar);
                }
            }
        }
        StaticExpression::Pipe { from, to } if matches!(&to.node, StaticExpression::Hold { .. }) => {
            return detect_initial_object_field_kinds(from, context, stack, locals, passed);
        }
        StaticExpression::Pipe { from, to } => {
            if let StaticExpression::Then { body } = &to.node {
                let next =
                    detect_initial_object_field_kinds(body, context, stack, locals, passed)?;
                kinds.extend(next);
                return Ok(kinds);
            }
            if let StaticExpression::When { arms } = &to.node {
                for arm in arms {
                    let next = detect_initial_object_field_kinds(
                        &arm.body,
                        context,
                        stack,
                        locals,
                        passed,
                    )?;
                    kinds.extend(next);
                }
                return Ok(kinds);
            }
            if let StaticExpression::FunctionCall { path, arguments } = &to.node {
                if path_matches(path, &["List", "latest"]) && arguments.is_empty() {
                    let next =
                        detect_initial_object_field_kinds(from, context, stack, locals, passed)?;
                    kinds.extend(next);
                    return Ok(kinds);
                }
                if path_matches(path, &["List", "map"]) {
                    let Some(mapper_name) = find_positional_parameter_name(arguments) else {
                        return Ok(kinds);
                    };
                    let Some(new) = find_named_argument(arguments, "new") else {
                        return Ok(kinds);
                    };
                    let mut scope = BTreeMap::new();
                    scope.insert(
                        mapper_name.to_string(),
                        LocalBinding {
                            expr: None,
                            object_base: Some("__item__".to_string()),
                        },
                    );
                    locals.push(scope);
                    let next =
                        detect_initial_object_field_kinds(new, context, stack, locals, passed)?;
                    locals.pop();
                    kinds.extend(next);
                    return Ok(kinds);
                }
            }
        }
        StaticExpression::Latest { inputs } => {
            for input in inputs {
                let next =
                    detect_initial_object_field_kinds(input, context, stack, locals, passed)?;
                kinds.extend(next);
            }
        }
        StaticExpression::FunctionCall { path, arguments }
            if path.len() == 1 && context.functions.contains_key(path[0].as_str()) =>
        {
            with_invoked_function_scope(
                path[0].as_str(),
                arguments,
                None,
                context,
                stack,
                locals,
                passed,
                |body, context, stack, locals, passed| {
                    detect_initial_object_field_kinds(body, context, stack, locals, passed)
                },
            )?
            .into_iter()
            .for_each(|(field, kind)| {
                kinds.insert(field, kind);
            });
        }
        _ => {}
    }
    Ok(kinds)
}

fn detect_top_level_bool_item_actions<'a>(
    target_binding: &str,
    expression: &'a StaticSpannedExpression,
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
    current_object_list_binding: Option<&str>,
) -> Result<BTreeMap<String, Vec<ObjectItemActionSpec>>, String> {
    let mut output = BTreeMap::new();
    let expression = resolve_alias(expression, context, locals, passed, stack)?;
    match &expression.node {
        StaticExpression::Latest { inputs } => {
            for input in inputs {
                let next = detect_top_level_bool_item_actions(
                    target_binding,
                    input,
                    context,
                    stack,
                    locals,
                    passed,
                    current_object_list_binding,
                )?;
                merge_object_item_action_maps(&mut output, next);
            }
        }
        StaticExpression::Pipe { from, to } => {
            if let StaticExpression::FunctionCall { path, arguments } = &to.node {
                if path_matches(path, &["List", "latest"]) && arguments.is_empty() {
                    let next = detect_top_level_bool_item_actions(
                        target_binding,
                        from,
                        context,
                        stack,
                        locals,
                        passed,
                        current_object_list_binding,
                    )?;
                    merge_object_item_action_maps(&mut output, next);
                    return Ok(output);
                }
                if path_matches(path, &["List", "map"]) {
                    let Some(list_ref) =
                        runtime_object_list_ref(from, context, locals, passed, stack)?
                    else {
                        return Ok(output);
                    };
                    let mapper_name = find_positional_parameter_name(arguments)
                        .ok_or_else(|| "List/map requires an item parameter name".to_string())?;
                    let new = find_named_argument(arguments, "new")
                        .ok_or_else(|| "List/map requires `new`".to_string())?;
                    let mut scope = BTreeMap::new();
                    scope.insert(
                        mapper_name.to_string(),
                        LocalBinding {
                            expr: None,
                            object_base: Some("__item__".to_string()),
                        },
                    );
                    locals.push(scope);
                    let next = detect_top_level_bool_item_actions(
                        target_binding,
                        new,
                        context,
                        stack,
                        locals,
                        passed,
                        Some(list_ref.binding.as_str()),
                    )?;
                    locals.pop();
                    merge_object_item_action_maps(&mut output, next);
                    return Ok(output);
                }
            }
            if let Some(source) = detect_item_event_source(from, context, locals, passed)? {
                let (source_suffix, event_name) = source
                    .rsplit_once(':')
                    .ok_or_else(|| format!("invalid item event source `{source}`"))?;
                let kind = ui_event_kind_for_name(event_name)
                    .ok_or_else(|| format!("unsupported event `{event_name}` for item action"))?;
                let Some((value, payload_filter)) = bool_item_event_update(to)? else {
                    return Ok(output);
                };
                if let Some(list_binding) = current_object_list_binding {
                    output
                        .entry(list_binding.to_string())
                        .or_default()
                        .push(ObjectItemActionSpec {
                            source_binding_suffix: source_suffix.to_string(),
                            kind,
                            action: ObjectItemActionKind::UpdateBindings {
                                scalar_updates: vec![ItemScalarUpdate::SetStatic {
                                    binding: target_binding.to_string(),
                                    value: i64::from(value),
                                }],
                                text_updates: Vec::new(),
                                payload_filter,
                            },
                        });
                }
                return Ok(output);
            }
        }
        StaticExpression::FunctionCall { path, arguments }
            if path.len() == 1 && context.functions.contains_key(path[0].as_str()) =>
        {
            let next = with_invoked_function_scope(
                path[0].as_str(),
                arguments,
                None,
                context,
                stack,
                locals,
                passed,
                |body, context, stack, locals, passed| {
                    detect_top_level_bool_item_actions(
                        target_binding,
                        body,
                        context,
                        stack,
                        locals,
                        passed,
                        current_object_list_binding,
                    )
                },
            )?;
            merge_object_item_action_maps(&mut output, next);
        }
        StaticExpression::Alias(_) => {
            if let Some(binding_name) = alias_binding_name(expression)? {
                let resolved = resolve_named_binding(binding_name, context, locals, stack)?;
                let next = detect_top_level_bool_item_actions(
                    target_binding,
                    resolved,
                    context,
                    stack,
                    locals,
                    passed,
                    current_object_list_binding,
                )?;
                stack.pop();
                merge_object_item_action_maps(&mut output, next);
            }
        }
        _ => {}
    }
    Ok(output)
}

fn bool_item_event_update(
    expression: &StaticSpannedExpression,
) -> Result<Option<(bool, Option<String>)>, String> {
    match &expression.node {
        StaticExpression::Then { body } => Ok(extract_bool_literal_opt(body)?.map(|value| (value, None))),
        StaticExpression::When { arms } => {
            for arm in arms {
                let payload_filter = match &arm.pattern {
                    static_expression::Pattern::Literal(static_expression::Literal::Tag(tag))
                    | static_expression::Pattern::Literal(static_expression::Literal::Text(tag)) => {
                        Some(tag.as_str().to_string())
                    }
                    _ => None,
                };
                let Some(value) = extract_bool_literal_opt(&arm.body)? else {
                    continue;
                };
                return Ok(Some((value, payload_filter)));
            }
            Ok(None)
        }
        _ => Ok(None),
    }
}

fn merge_object_item_action_maps(
    into: &mut BTreeMap<String, Vec<ObjectItemActionSpec>>,
    next: BTreeMap<String, Vec<ObjectItemActionSpec>>,
) {
    for (binding, actions) in next {
        into.entry(binding).or_default().extend(actions);
    }
}

fn expression_path(
    expression: &StaticSpannedExpression,
    context: &LowerContext<'_>,
    locals: &LocalScopes<'_>,
    passed: &PassedScopes,
) -> Result<Option<String>, String> {
    let StaticExpression::Alias(static_expression::Alias::WithoutPassed { parts, .. }) =
        &expression.node
    else {
        return Ok(None);
    };
    canonical_without_passed_path(parts, context, locals, passed).map(Some)
}

fn is_event_text_expression(
    expression: &StaticSpannedExpression,
    context: &LowerContext<'_>,
    locals: &LocalScopes<'_>,
    passed: &PassedScopes,
) -> Result<bool, String> {
    let StaticExpression::Alias(static_expression::Alias::WithoutPassed { parts, .. }) =
        &expression.node
    else {
        return Ok(false);
    };
    if parts.len() < 4 {
        return Ok(false);
    }
    let event_index = parts.len() - 3;
    if parts[event_index].as_str() != "event" || parts[parts.len() - 1].as_str() != "text" {
        return Ok(false);
    }
    canonical_without_passed_path(&parts[..event_index], context, locals, passed)?;
    Ok(true)
}

fn detect_top_level_object_field_plan<'a>(
    target_base: &str,
    expression: &'a StaticSpannedExpression,
    context: &'a LowerContext<'a>,
    field_kinds: &BTreeMap<String, ObjectFieldKind>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
    current_object_list_binding: Option<&str>,
) -> Result<TopLevelObjectFieldPlan, String> {
    let mut plan = TopLevelObjectFieldPlan::default();
    let expression = resolve_alias(expression, context, locals, passed, stack)?;
    match &expression.node {
        StaticExpression::Object(object) => {
            for variable in &object.variables {
                if variable.node.name.is_empty() {
                    continue;
                }
                let field_name = variable.node.name.as_str();
                let Some(kind) = field_kinds.get(field_name) else {
                    continue;
                };
                let binding = format!("{target_base}.{field_name}");
                match kind {
                    ObjectFieldKind::Scalar => {
                        if let Some(value) = extract_integer_literal_opt(&variable.node.value)?
                            .or(extract_bool_literal_opt(&variable.node.value)?.map(i64::from))
                        {
                            plan.scalar_initials.insert(binding, value);
                        }
                    }
                    ObjectFieldKind::Text => {
                        if let Some(value) = extract_static_text_value(&variable.node.value)? {
                            plan.text_initials.insert(binding, value);
                        }
                    }
                }
            }
        }
        StaticExpression::Pipe { from, to } if matches!(&to.node, StaticExpression::Hold { .. }) => {
            let initial =
                detect_top_level_object_field_plan(target_base, from, context, field_kinds, stack, locals, passed, current_object_list_binding)?;
            merge_top_level_object_field_plan(&mut plan, initial);
            if let StaticExpression::Hold { body, .. } = &to.node {
                let dynamic = detect_top_level_object_field_plan(
                    target_base,
                    body,
                    context,
                    field_kinds,
                    stack,
                    locals,
                    passed,
                    current_object_list_binding,
                )?;
                merge_top_level_object_field_plan(&mut plan, dynamic);
            }
        }
        StaticExpression::Latest { inputs } => {
            for input in inputs {
                let next = detect_top_level_object_field_plan(
                    target_base,
                    input,
                    context,
                    field_kinds,
                    stack,
                    locals,
                    passed,
                    current_object_list_binding,
                )?;
                merge_top_level_object_field_plan(&mut plan, next);
            }
        }
        StaticExpression::Pipe { from, to } => {
            if let Some(source_binding_name) = alias_binding_name(from)? {
                let resolved = resolve_named_binding(source_binding_name, context, locals, stack)?;
                let rewritten = match &to.node {
                    StaticExpression::Then { body } => Some(
                        rewrite_top_level_object_field_plan_from_body(
                            target_base,
                            source_binding_name,
                            resolved,
                            body,
                            context,
                            field_kinds,
                            stack,
                            locals,
                            passed,
                            current_object_list_binding,
                        )?,
                    ),
                    _ => None,
                };
                stack.pop();
                if let Some(next) = rewritten {
                    merge_top_level_object_field_plan(&mut plan, next);
                    return Ok(plan);
                }
            }
            if let StaticExpression::FunctionCall { path, arguments } = &to.node {
                if path_matches(path, &["List", "latest"]) && arguments.is_empty() {
                    let next = detect_top_level_object_field_plan(
                        target_base,
                        from,
                        context,
                        field_kinds,
                        stack,
                        locals,
                        passed,
                        current_object_list_binding,
                    )?;
                    merge_top_level_object_field_plan(&mut plan, next);
                    return Ok(plan);
                }
                if path_matches(path, &["List", "map"]) {
                    let Some(list_ref) =
                        runtime_object_list_ref(from, context, locals, passed, stack)?
                    else {
                        return Ok(plan);
                    };
                    let mapper_name = find_positional_parameter_name(arguments)
                        .ok_or_else(|| "List/map requires an item parameter name".to_string())?;
                    let new = find_named_argument(arguments, "new")
                        .ok_or_else(|| "List/map requires `new`".to_string())?;
                    let mut scope = BTreeMap::new();
                    scope.insert(
                        mapper_name.to_string(),
                        LocalBinding {
                            expr: None,
                            object_base: Some("__item__".to_string()),
                        },
                    );
                    locals.push(scope);
                    let next = detect_top_level_object_field_plan(
                        target_base,
                        new,
                        context,
                        field_kinds,
                        stack,
                        locals,
                        passed,
                        Some(list_ref.binding.as_str()),
                    )?;
                    locals.pop();
                    merge_top_level_object_field_plan(&mut plan, next);
                    return Ok(plan);
                }
            }
            if let Some(source) = detect_item_event_source(from, context, locals, passed)? {
                if let Some(action) = object_literal_item_update_action(
                    target_base,
                    to,
                    source.as_str(),
                    field_kinds,
                    context,
                    locals,
                    passed,
                )? {
                    if let Some(list_binding) = current_object_list_binding {
                        plan.item_actions
                            .entry(list_binding.to_string())
                            .or_default()
                            .push(action);
                    }
                    return Ok(plan);
                }
            }
        }
        StaticExpression::FunctionCall { path, arguments }
            if path.len() == 1 && context.functions.contains_key(path[0].as_str()) =>
        {
            let next = with_invoked_function_scope(
                path[0].as_str(),
                arguments,
                None,
                context,
                stack,
                locals,
                passed,
                |body, context, stack, locals, passed| {
                    detect_top_level_object_field_plan(
                        target_base,
                        body,
                        context,
                        field_kinds,
                        stack,
                        locals,
                        passed,
                        current_object_list_binding,
                    )
                },
            )?;
            merge_top_level_object_field_plan(&mut plan, next);
        }
        StaticExpression::Alias(_) => {
            if let Some(binding_name) = alias_binding_name(expression)? {
                let resolved = resolve_named_binding(binding_name, context, locals, stack)?;
                let next = detect_top_level_object_field_plan(
                    target_base,
                    resolved,
                    context,
                    field_kinds,
                    stack,
                    locals,
                    passed,
                    current_object_list_binding,
                )?;
                stack.pop();
                merge_top_level_object_field_plan(&mut plan, next);
            }
        }
        _ => {}
    }
    Ok(plan)
}

fn rewrite_top_level_object_field_plan_from_body<'a>(
    target_base: &str,
    source_binding_name: &str,
    source_expression: &'a StaticSpannedExpression,
    body: &'a StaticSpannedExpression,
    context: &'a LowerContext<'a>,
    field_kinds: &BTreeMap<String, ObjectFieldKind>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
    current_object_list_binding: Option<&str>,
) -> Result<TopLevelObjectFieldPlan, String> {
    let source_plan = detect_top_level_object_field_plan(
        target_base,
        source_expression,
        context,
        field_kinds,
        stack,
        locals,
        passed,
        current_object_list_binding,
    )?;
    let body = resolve_alias(body, context, locals, passed, &mut Vec::new())?;
    let Some(object) = resolve_object(body) else {
        return Ok(source_plan);
    };

    let mut rewritten = TopLevelObjectFieldPlan {
        scalar_initials: source_plan.scalar_initials,
        text_initials: source_plan.text_initials,
        item_actions: BTreeMap::new(),
    };
    for (binding, actions) in source_plan.item_actions {
        let mapped_actions = actions
            .into_iter()
            .map(|action| {
                rewrite_object_item_action_from_body(
                    target_base,
                    source_binding_name,
                    &action,
                    object,
                    field_kinds,
                    context,
                    locals,
                    passed,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        rewritten.item_actions.insert(binding, mapped_actions);
    }
    let bool_source_actions = detect_top_level_bool_item_actions(
        source_binding_name,
        source_expression,
        context,
        &mut Vec::new(),
        &mut locals.clone(),
        &mut passed.clone(),
        current_object_list_binding,
    )?;
    for (binding, actions) in bool_source_actions {
        let mapped_actions = actions
            .into_iter()
            .map(|action| {
                rewrite_object_item_action_from_body(
                    target_base,
                    source_binding_name,
                    &action,
                    object,
                    field_kinds,
                    context,
                    locals,
                    passed,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        rewritten
            .item_actions
            .entry(binding)
            .or_default()
            .extend(mapped_actions);
    }
    Ok(rewritten)
}

fn rewrite_object_item_action_from_body(
    target_base: &str,
    source_binding_name: &str,
    action: &ObjectItemActionSpec,
    object: &StaticObject,
    field_kinds: &BTreeMap<String, ObjectFieldKind>,
    context: &LowerContext<'_>,
    locals: &LocalScopes<'_>,
    passed: &PassedScopes,
) -> Result<ObjectItemActionSpec, String> {
    let ObjectItemActionKind::UpdateBindings { payload_filter, .. } = &action.action else {
        return Ok(action.clone());
    };
    let mut scalar_updates = Vec::new();
    let mut text_updates = Vec::new();
    for variable in &object.variables {
        if variable.node.name.is_empty() {
            continue;
        }
        let field_name = variable.node.name.as_str();
        let Some(kind_hint) = field_kinds.get(field_name) else {
            continue;
        };
        let binding = format!("{target_base}.{field_name}");
        match kind_hint {
            ObjectFieldKind::Scalar => {
                if let Some(value) = extract_integer_literal_opt(&variable.node.value)?
                    .or(extract_bool_literal_opt(&variable.node.value)?.map(i64::from))
                {
                    scalar_updates.push(ItemScalarUpdate::SetStatic { binding, value });
                    continue;
                }
                if let Some(field) =
                    placeholder_object_field(&variable.node.value, context, locals, passed)?
                {
                    scalar_updates.push(ItemScalarUpdate::SetFromField { binding, field });
                    continue;
                }
            }
            ObjectFieldKind::Text => {
                if let Some(value) = extract_static_text_value(&variable.node.value)? {
                    text_updates.push(ItemTextUpdate::SetStatic { binding, value });
                    continue;
                }
                if let Some(field) =
                    placeholder_object_field(&variable.node.value, context, locals, passed)?
                {
                    text_updates.push(ItemTextUpdate::SetFromField { binding, field });
                    continue;
                }
            }
        }
    }
    if scalar_updates.is_empty() && text_updates.is_empty() {
        return Ok(action.clone());
    }
    Ok(ObjectItemActionSpec {
        source_binding_suffix: action.source_binding_suffix.clone(),
        kind: action.kind.clone(),
        action: ObjectItemActionKind::UpdateBindings {
            scalar_updates,
            text_updates,
            payload_filter: payload_filter.clone(),
        },
    })
}

fn merge_top_level_object_field_plan(
    into: &mut TopLevelObjectFieldPlan,
    next: TopLevelObjectFieldPlan,
) {
    into.scalar_initials.extend(next.scalar_initials);
    into.text_initials.extend(next.text_initials);
    for (binding, actions) in next.item_actions {
        into.item_actions.entry(binding).or_default().extend(actions);
    }
}

fn detect_item_event_source(
    expression: &StaticSpannedExpression,
    context: &LowerContext<'_>,
    locals: &LocalScopes<'_>,
    passed: &PassedScopes,
) -> Result<Option<String>, String> {
    let StaticExpression::Alias(static_expression::Alias::WithoutPassed { parts, .. }) =
        &expression.node
    else {
        return Ok(None);
    };
    if parts.len() < 3 {
        return Ok(None);
    }
    let event_index = if parts.len() >= 4 && parts[parts.len() - 3].as_str() == "event" {
        parts.len() - 3
    } else {
        let index = parts.len() - 2;
        if parts[index].as_str() != "event" {
            return Ok(None);
        }
        index
    };
    let path = canonical_without_passed_path(&parts[..event_index], context, locals, passed)?;
    let Some(suffix) = path.strip_prefix("__item__.") else {
        return Ok(None);
    };
    Ok(Some(format!("{suffix}:{}", parts[event_index + 1].as_str())))
}

fn object_literal_item_update_action(
    target_base: &str,
    expression: &StaticSpannedExpression,
    source: &str,
    field_kinds: &BTreeMap<String, ObjectFieldKind>,
    context: &LowerContext<'_>,
    locals: &LocalScopes<'_>,
    passed: &PassedScopes,
) -> Result<Option<ObjectItemActionSpec>, String> {
    let (source_suffix, event_name) = source
        .rsplit_once(':')
        .ok_or_else(|| format!("invalid item event source `{source}`"))?;
    let kind = ui_event_kind_for_name(event_name)
        .ok_or_else(|| format!("unsupported event `{event_name}` for item action"))?;
    let mut payload_filter = None;
    let body = match &expression.node {
        StaticExpression::Then { body } => body.as_ref(),
        StaticExpression::When { arms } => {
            let mut selected_body = None;
            for arm in arms {
                match &arm.pattern {
                    static_expression::Pattern::Literal(static_expression::Literal::Tag(tag))
                    | static_expression::Pattern::Literal(static_expression::Literal::Text(tag)) => {
                        payload_filter = Some(tag.as_str().to_string());
                        selected_body = Some(&arm.body);
                        break;
                    }
                    _ => {}
                }
            }
            let Some(body) = selected_body else {
                return Ok(None);
            };
            body
        }
        _ => return Ok(None),
    };
    let body = resolve_alias(body, context, locals, passed, &mut Vec::new())?;
    let Some(object) = resolve_object(body) else {
        return Ok(None);
    };
    let mut scalar_updates = Vec::new();
    let mut text_updates = Vec::new();
    for variable in &object.variables {
        if variable.node.name.is_empty() {
            continue;
        }
        let field_name = variable.node.name.as_str();
        let Some(kind_hint) = field_kinds.get(field_name) else {
            continue;
        };
        let binding = format!("{target_base}.{field_name}");
        match kind_hint {
            ObjectFieldKind::Scalar => {
                if let Some(value) = extract_integer_literal_opt(&variable.node.value)?
                    .or(extract_bool_literal_opt(&variable.node.value)?.map(i64::from))
                {
                    scalar_updates.push(ItemScalarUpdate::SetStatic { binding, value });
                    continue;
                }
                if let Some(field) =
                    placeholder_object_field(&variable.node.value, context, locals, passed)?
                {
                    scalar_updates.push(ItemScalarUpdate::SetFromField { binding, field });
                }
            }
            ObjectFieldKind::Text => {
                if let Some(value) = extract_static_text_value(&variable.node.value)? {
                    text_updates.push(ItemTextUpdate::SetStatic { binding, value });
                    continue;
                }
                if let Some(field) =
                    placeholder_object_field(&variable.node.value, context, locals, passed)?
                {
                    text_updates.push(ItemTextUpdate::SetFromField { binding, field });
                    continue;
                }
                if item_event_text_source(
                    &variable.node.value,
                    context,
                    locals,
                    passed,
                    source_suffix,
                    event_name,
                )? {
                    match event_name {
                        "key_down" => text_updates.push(ItemTextUpdate::SetFromInputSource {
                            binding,
                            source_suffix: source_suffix.to_string(),
                        }),
                        _ => text_updates.push(ItemTextUpdate::SetFromPayload { binding }),
                    }
                }
            }
        }
    }
    if scalar_updates.is_empty() && text_updates.is_empty() {
        return Ok(None);
    }
    Ok(Some(ObjectItemActionSpec {
        source_binding_suffix: source_suffix.to_string(),
        kind,
        action: ObjectItemActionKind::UpdateBindings {
            scalar_updates,
            text_updates,
            payload_filter,
        },
    }))
}

fn item_event_text_source(
    expression: &StaticSpannedExpression,
    context: &LowerContext<'_>,
    locals: &LocalScopes<'_>,
    passed: &PassedScopes,
    expected_suffix: &str,
    expected_event: &str,
) -> Result<bool, String> {
    let StaticExpression::Alias(static_expression::Alias::WithoutPassed { parts, .. }) =
        &expression.node
    else {
        return Ok(false);
    };
    if parts.len() < 4 {
        return Ok(false);
    }
    let event_index = parts.len() - 3;
    if parts[event_index].as_str() != "event"
        || parts[event_index + 1].as_str() != expected_event
        || parts[event_index + 2].as_str() != "text"
    {
        return Ok(false);
    }
    let path = canonical_without_passed_path(&parts[..event_index], context, locals, passed)?;
    Ok(path.strip_prefix("__item__.") == Some(expected_suffix))
}

fn detect_static_mapped_object_list_placeholders<'a>(
    expression: &'a StaticSpannedExpression,
    functions: &BTreeMap<String, FunctionSpec<'a>>,
) -> Result<Option<Vec<ObjectListItem>>, String> {
    let StaticExpression::Pipe { from, to } = &expression.node else {
        return Ok(None);
    };
    let StaticExpression::FunctionCall { path, arguments } = &to.node else {
        return Ok(None);
    };
    if !path_matches(path, &["List", "map"]) {
        return Ok(None);
    }
    let Some(mapper_name) = find_positional_parameter_name(arguments) else {
        return Ok(None);
    };
    let Some(template) = find_named_argument(arguments, "new") else {
        return Ok(None);
    };
    if resolve_static_object_runtime_expression(template, functions)?.is_none() {
        return Ok(None);
    }
    let Some(source_items) = static_object_list_source_items(from, functions) else {
        return Ok(None);
    };
    Ok(Some(
        source_items
            .into_iter()
            .enumerate()
            .map(|(index, item)| {
                let mut bindings = BTreeMap::new();
                bindings.insert(mapper_name.to_string(), item);
                let specialized = specialize_static_expression(template, &bindings);
                build_static_object_list_item(specialized, functions, index as u64 + 1)
            })
            .collect::<Result<Vec<_>, _>>()?,
    ))
}

fn build_static_object_list_item<'a>(
    expression: &'a StaticSpannedExpression,
    functions: &BTreeMap<String, FunctionSpec<'a>>,
    id: u64,
) -> Result<ObjectListItem, String> {
    let object = resolve_static_object_runtime_expression(expression, functions)?
        .ok_or_else(|| "expected static object item".to_string())?;
    let mut item = ObjectListItem {
        id,
        title: String::new(),
        completed: false,
        text_fields: BTreeMap::new(),
        bool_fields: BTreeMap::new(),
        scalar_fields: BTreeMap::new(),
        object_lists: BTreeMap::new(),
    };
    for variable in &object.variables {
        let name = variable.node.name.as_str();
        if name.is_empty() {
            continue;
        }
        let value = resolve_static_object_field_expression(expression, functions, name)
            .unwrap_or(&variable.node.value);
        if name == "title" {
            if let Some(title) = resolve_initial_static_object_text_field(value, functions, "title")?
                .or_else(|| static_text_item(value).ok())
            {
                item.title = title;
                continue;
            }
        }
        if name == "completed" {
            if let Some(value) = extract_bool_literal_opt(value)? {
                item.completed = value;
                continue;
            }
        }
        if let Some(value) = extract_integer_literal_opt(value)? {
            item.scalar_fields.insert(name.to_string(), value);
            continue;
        }
        if let Some(value) = extract_bool_literal_opt(value)? {
            item.bool_fields.insert(name.to_string(), value);
            continue;
        }
        if let Ok(value) = static_text_item(value) {
            item.text_fields.insert(name.to_string(), value);
            continue;
        }
        if let Some(values) = static_object_list_source_items(value, functions) {
            let nested_items = values
                .into_iter()
                .enumerate()
                .map(|(nested_index, nested)| {
                    build_static_object_list_item(nested, functions, nested_index as u64 + 1)
                })
                .collect::<Result<Vec<_>, _>>()?;
            item.object_lists.insert(name.to_string(), nested_items);
            continue;
        }
    }
    Ok(item)
}

fn static_object_list_source_items<'a>(
    expression: &'a StaticSpannedExpression,
    functions: &BTreeMap<String, FunctionSpec<'a>>,
) -> Option<Vec<&'a StaticSpannedExpression>> {
    match &expression.node {
        StaticExpression::List { items } => Some(items.iter().collect()),
        StaticExpression::FunctionCall { path, arguments }
            if path.len() == 1 && functions.contains_key(path[0].as_str()) =>
        {
            let function = functions.get(path[0].as_str())?;
            if function.parameters.len() != arguments.len() {
                return None;
            }
            let bindings = function_argument_bindings(function, arguments).ok()?;
            let specialized = specialize_static_expression(function.body, &bindings);
            static_object_list_source_items(specialized, functions)
        }
        StaticExpression::FunctionCall { path, arguments }
            if path_matches(path, &["List", "range"]) =>
        {
            let from_value = find_named_argument(arguments, "from")
                .and_then(|argument| extract_integer_literal_opt(argument).ok().flatten())?;
            let to_value = find_named_argument(arguments, "to")
                .and_then(|argument| extract_integer_literal_opt(argument).ok().flatten())?;
            let values = if from_value <= to_value {
                (from_value..=to_value).collect::<Vec<_>>()
            } else {
                (to_value..=from_value).rev().collect::<Vec<_>>()
            };
            Some(
                values
                    .into_iter()
                    .map(|value| synthetic_integer_expression(value, expression))
                    .collect(),
            )
        }
        _ => None,
    }
}

fn static_object_list_source_length<'a>(
    expression: &'a StaticSpannedExpression,
    functions: &BTreeMap<String, FunctionSpec<'a>>,
) -> Option<usize> {
    static_object_list_source_items(expression, functions).map(|items| items.len())
}

fn detect_object_list_pipeline<'a>(
    expression: &'a StaticSpannedExpression,
    path_bindings: &BTreeMap<String, &'a StaticSpannedExpression>,
    functions: &BTreeMap<String, FunctionSpec<'a>>,
    binding_path: &str,
) -> Result<
    Option<(
        Vec<ObjectListItem>,
        ObjectListEventUpdates,
        Vec<ObjectItemActionSpec>,
        PendingGlobalObjectUpdates,
    )>,
    String,
> {
    match &expression.node {
        StaticExpression::List { items } => {
            if items.is_empty() {
                return Ok(None);
            }
            let mut initial_items = Vec::with_capacity(items.len());
            let mut shared_actions: Option<Vec<ObjectItemActionSpec>> = None;
            let mut shared_global_actions: Option<PendingGlobalObjectUpdates> = None;
            for (index, item) in items.iter().enumerate() {
                let Some((detected_item, item_actions, global_actions)) =
                    detect_dynamic_object_list_item(item, functions, index as u64 + 1)?
                else {
                    return Ok(None);
                };
                if let Some(existing) = &shared_actions {
                    if existing != &item_actions {
                        return Ok(None);
                    }
                } else {
                    shared_actions = Some(item_actions);
                }
                if let Some(existing) = &shared_global_actions {
                    if existing != &global_actions {
                        return Ok(None);
                    }
                } else {
                    shared_global_actions = Some(global_actions);
                }
                initial_items.push(detected_item);
            }
            Ok(Some((
                initial_items,
                Vec::new(),
                shared_actions.unwrap_or_default(),
                shared_global_actions.unwrap_or_default(),
            )))
        }
        StaticExpression::Pipe { from, to } => {
            let Some((initial_items, mut updates, mut item_actions, global_actions)) =
                detect_object_list_pipeline(from, path_bindings, functions, binding_path)?
            else {
                return Ok(None);
            };
            let StaticExpression::FunctionCall { path, arguments } = &to.node else {
                return Ok(None);
            };
            if path_matches(path, &["List", "append"]) {
                let item = find_named_argument(arguments, "item")
                    .ok_or_else(|| "List/append requires `item`".to_string())?;
                let ((trigger_binding, event_name), update) =
                    detect_object_list_append_update(item, path_bindings, binding_path, functions)?;
                updates.push(((trigger_binding, event_name), update));
                return Ok(Some((initial_items, updates, item_actions, global_actions)));
            }
            if path_matches(path, &["List", "remove"]) {
                let item_name = find_positional_parameter_name(arguments)
                    .ok_or_else(|| "List/remove requires an item parameter name".to_string())?;
                let on = find_named_argument(arguments, "on")
                    .ok_or_else(|| "List/remove requires `on`".to_string())?;
                if let Some(remove_action) = detect_dynamic_object_remove_action(on, item_name)? {
                    item_actions.push(remove_action);
                    return Ok(Some((initial_items, updates, item_actions, global_actions)));
                }
                if let Some(((trigger_binding, event_name), update)) =
                    detect_dynamic_object_bulk_remove_action(
                        on,
                        item_name,
                        path_bindings,
                        binding_path,
                    )?
                {
                    let update = match update {
                        ObjectListUpdate::RemoveMatching { filter, .. } => {
                            ObjectListUpdate::RemoveMatching {
                                binding: binding_path.to_string(),
                                filter,
                            }
                        }
                        other => other,
                    };
                    updates.push(((trigger_binding, event_name), update));
                    return Ok(Some((initial_items, updates, item_actions, global_actions)));
                }
                return Ok(None);
            }
            Ok(None)
        }
        _ => Ok(None),
    }
}

fn detect_dynamic_object_list_item<'a>(
    expression: &'a StaticSpannedExpression,
    functions: &BTreeMap<String, FunctionSpec<'a>>,
    id: u64,
) -> Result<
    Option<(
        ObjectListItem,
        Vec<ObjectItemActionSpec>,
        PendingGlobalObjectUpdates,
    )>,
    String,
> {
    let object = resolve_static_object_runtime_expression(expression, functions)?
        .ok_or_else(|| "expected object runtime item".to_string())?;
    let title = resolve_initial_static_object_text_field(expression, functions, "title")?;
    let Some(title) = title else {
        return Ok(None);
    };
    let bool_fields = detect_dynamic_object_bool_fields(expression, functions)?;
    let mut item_actions = Vec::new();
    let mut global_actions = Vec::new();
    item_actions.extend(detect_dynamic_object_title_actions(expression, functions)?);
    let initial_completed = bool_fields
        .get("completed")
        .map(|spec| spec.initial)
        .unwrap_or(false);
    let mut initial_extra_bools = BTreeMap::new();
    for (field_name, spec) in bool_fields {
        if field_name != "completed" {
            initial_extra_bools.insert(field_name.clone(), spec.initial);
        }
        for event in spec.events {
            if field_name == "completed" && event.trigger_binding.starts_with("store.") {
                global_actions.push((
                    event.trigger_binding,
                    event.event_name,
                    PendingObjectGlobalAction::ToggleAllCompleted,
                ));
                continue;
            }
            let Some(kind) = ui_event_kind_for_name(&event.event_name) else {
                continue;
            };
            let action = match event.update {
                BoolEventUpdate::Toggle => ObjectItemActionKind::ToggleBoolField {
                    field: field_name.clone(),
                },
                BoolEventUpdate::Set(value) => ObjectItemActionKind::SetBoolField {
                    field: field_name.clone(),
                    value,
                    payload_filter: event.payload_filter.clone(),
                },
            };
            item_actions.push(ObjectItemActionSpec {
                source_binding_suffix: event.trigger_binding,
                kind,
                action,
            });
        }
    }
    let mut item = ObjectListItem {
        id,
        title,
        completed: initial_completed,
        text_fields: BTreeMap::new(),
        bool_fields: initial_extra_bools,
        scalar_fields: BTreeMap::new(),
        object_lists: BTreeMap::new(),
    };
    for variable in &object.variables {
        let name = variable.node.name.as_str();
        if name.is_empty() || name == "title" || name == "completed" {
            continue;
        }
        let value = resolve_static_object_field_expression(expression, functions, name)
            .unwrap_or(&variable.node.value);
        if let Some(value) = extract_integer_literal_opt(value)? {
            item.scalar_fields.insert(name.to_string(), value);
            continue;
        }
        if let Some(value) = extract_bool_literal_opt(value)? {
            item.bool_fields.entry(name.to_string()).or_insert(value);
            continue;
        }
        if let Some(value) =
            resolve_initial_static_object_text_field(value, functions, name)?
                .or_else(|| static_text_item(value).ok())
        {
            item.text_fields.insert(name.to_string(), value);
            continue;
        }
        if let Some(values) = static_object_list_source_items(value, functions) {
            let nested_items = values
                .into_iter()
                .enumerate()
                .map(|(nested_index, nested)| {
                    build_static_object_list_item(nested, functions, nested_index as u64 + 1)
                })
                .collect::<Result<Vec<_>, _>>()?;
            item.object_lists.insert(name.to_string(), nested_items);
        }
    }
    Ok(Some((item, item_actions, global_actions)))
}

fn detect_dynamic_object_title_actions<'a>(
    expression: &'a StaticSpannedExpression,
    functions: &BTreeMap<String, FunctionSpec<'a>>,
) -> Result<Vec<ObjectItemActionSpec>, String> {
    let Some(title_expression) =
        resolve_static_object_field_expression(expression, functions, "title")
    else {
        return Ok(Vec::new());
    };
    let mut actions = Vec::new();
    match &title_expression.node {
        StaticExpression::Latest { inputs } => {
            for input in inputs {
                if let Some(action) = detect_dynamic_object_title_action(input)? {
                    actions.push(action);
                }
            }
        }
        _ => {
            if let Some(action) = detect_dynamic_object_title_action(title_expression)? {
                actions.push(action);
            }
        }
    }
    Ok(actions)
}

fn detect_dynamic_object_title_action(
    expression: &StaticSpannedExpression,
) -> Result<Option<ObjectItemActionSpec>, String> {
    let StaticExpression::Pipe {
        from: trigger_source,
        to: trigger_then,
    } = &expression.node
    else {
        return Ok(None);
    };
    let Some((trigger_binding, event_name, payload_field)) =
        object_event_payload_source_from_expression(trigger_source)?
    else {
        return Ok(None);
    };
    if event_name != "change" || payload_field != "text" {
        return Ok(None);
    }
    let Some((trim, reject_empty, payload_filter)) = title_event_update(trigger_then)? else {
        return Ok(None);
    };
    let Some(kind) = ui_event_kind_for_name(&event_name) else {
        return Ok(None);
    };
    Ok(Some(ObjectItemActionSpec {
        source_binding_suffix: trigger_binding,
        kind,
        action: ObjectItemActionKind::SetTitle {
            trim,
            reject_empty,
            payload_filter,
        },
    }))
}

fn title_event_update(
    expression: &StaticSpannedExpression,
) -> Result<Option<(bool, bool, Option<String>)>, String> {
    let StaticExpression::When { arms } = &expression.node else {
        return Ok(None);
    };
    for arm in arms {
        let static_expression::Pattern::Alias { name } = &arm.pattern else {
            continue;
        };
        if let Some((trim, reject_empty)) = title_alias_update_body(&arm.body, name.as_str())? {
            return Ok(Some((trim, reject_empty, None)));
        }
    }
    Ok(None)
}

fn title_alias_update_body(
    expression: &StaticSpannedExpression,
    alias_name: &str,
) -> Result<Option<(bool, bool)>, String> {
    match &expression.node {
        StaticExpression::Alias(static_expression::Alias::WithoutPassed { parts, .. })
            if parts.len() == 1 && parts[0].as_str() == alias_name =>
        {
            return Ok(Some((false, false)));
        }
        StaticExpression::Block { variables, output } => {
            if variables.len() != 1 {
                return Ok(None);
            }
            let trimmed_name = variables[0].node.name.as_str();
            let StaticExpression::Pipe { from, to } = &variables[0].node.value.node else {
                return Ok(None);
            };
            if !matches!(
                &from.node,
                StaticExpression::Alias(static_expression::Alias::WithoutPassed { parts, .. })
                    if parts.len() == 1 && parts[0].as_str() == alias_name
            ) {
                return Ok(None);
            }
            let StaticExpression::FunctionCall { path, arguments } = &to.node else {
                return Ok(None);
            };
            if !path_matches(path, &["Text", "trim"]) || !arguments.is_empty() {
                return Ok(None);
            }
            return Ok(trimmed_non_empty_output(output, trimmed_name)?.then_some((true, true)));
        }
        _ => {}
    }

    let StaticExpression::Pipe {
        from: condition_source,
        to: when_expression,
    } = &expression.node
    else {
        return Ok(None);
    };
    let StaticExpression::Pipe {
        from: condition_from,
        to: condition_to,
    } = &condition_source.node
    else {
        return Ok(None);
    };
    if !matches!(
        &condition_from.node,
        StaticExpression::Alias(static_expression::Alias::WithoutPassed { parts, .. })
            if parts.len() == 1 && parts[0].as_str() == alias_name
    ) {
        return Ok(None);
    }
    let StaticExpression::FunctionCall { path, arguments } = &condition_to.node else {
        return Ok(None);
    };
    if !path_matches(path, &["Text", "is_not_empty"]) || !arguments.is_empty() {
        return Ok(None);
    }
    let StaticExpression::When { arms } = &when_expression.node else {
        return Ok(None);
    };

    let mut has_true_value = false;
    let mut has_false_skip = false;
    for arm in arms {
        match (&arm.pattern, &arm.body.node) {
            (
                static_expression::Pattern::Literal(static_expression::Literal::Tag(tag)),
                StaticExpression::Alias(static_expression::Alias::WithoutPassed { parts, .. }),
            ) if tag.as_str() == "True" && parts.len() == 1 && parts[0].as_str() == alias_name => {
                has_true_value = true;
            }
            (
                static_expression::Pattern::Literal(static_expression::Literal::Tag(tag)),
                StaticExpression::Skip,
            ) if tag.as_str() == "False" => {
                has_false_skip = true;
            }
            _ => {}
        }
    }
    Ok((has_true_value && has_false_skip).then_some((false, true)))
}

fn resolve_initial_static_object_text_field<'a>(
    expression: &'a StaticSpannedExpression,
    functions: &BTreeMap<String, FunctionSpec<'a>>,
    field_name: &str,
) -> Result<Option<String>, String> {
    if let Some(object) = resolve_object(expression) {
        return find_object_field(object, field_name)
            .map(static_text_item)
            .transpose();
    }
    let StaticExpression::FunctionCall { path, arguments } = &expression.node else {
        return Ok(None);
    };
    if path.len() != 1 {
        return Ok(None);
    }
    let Some(function) = functions.get(path[0].as_str()) else {
        return Ok(None);
    };
    let Some(object) = resolve_object(function.body) else {
        return Ok(None);
    };
    let Some(field_expression) = find_object_field(object, field_name) else {
        return Ok(None);
    };
    initial_static_text_with_arguments(field_expression, arguments)
}

fn initial_static_text_with_arguments(
    expression: &StaticSpannedExpression,
    arguments: &[static_expression::Spanned<StaticArgument>],
) -> Result<Option<String>, String> {
    match &expression.node {
        StaticExpression::Alias(static_expression::Alias::WithoutPassed { parts, .. })
            if parts.len() == 1 =>
        {
            find_named_argument(arguments, parts[0].as_str())
                .map(static_text_item)
                .transpose()
        }
        StaticExpression::Latest { inputs } => {
            for input in inputs {
                if let Some(value) = initial_static_text_with_arguments(input, arguments)? {
                    return Ok(Some(value));
                }
            }
            Ok(None)
        }
        _ => static_text_item(expression).map(Some),
    }
}

fn detect_dynamic_object_bool_fields<'a>(
    expression: &'a StaticSpannedExpression,
    functions: &BTreeMap<String, FunctionSpec<'a>>,
) -> Result<BTreeMap<String, BoolSpec>, String> {
    let object = resolve_static_object_runtime_expression(expression, functions)?;
    let Some(object) = object else {
        return Ok(BTreeMap::new());
    };
    let mut output = BTreeMap::new();
    for variable in &object.variables {
        if let Some(spec) = detect_local_bool_spec(&variable.node.value)? {
            output.insert(variable.node.name.as_str().to_string(), spec);
        }
    }
    Ok(output)
}

fn detect_object_list_append_update<'a>(
    expression: &'a StaticSpannedExpression,
    path_bindings: &BTreeMap<String, &'a StaticSpannedExpression>,
    binding_path: &str,
    functions: &BTreeMap<String, FunctionSpec<'a>>,
) -> Result<((String, String), ObjectListUpdate), String> {
    let (source_expression, target_expression) = match &expression.node {
        StaticExpression::Pipe { from, to } => (from.as_ref(), to.as_ref()),
        _ => {
            return Err(
                "runtime object list append subset requires `source |> item_factory()`".to_string(),
            );
        }
    };
    if !object_append_target_supported(target_expression, functions) {
        return Err(
            "runtime object list append subset requires a supported object item factory"
                .to_string(),
        );
    }
    let source_path = canonical_reference_path(source_expression, path_bindings, binding_path)?
        .ok_or_else(|| {
            "runtime object list append subset requires a named source binding".to_string()
        })?;
    let source_expression = path_bindings
        .get(&source_path)
        .copied()
        .ok_or_else(|| format!("unknown append source binding `{source_path}`"))?;
    let Some(spec) = detect_append_from_key_when(source_expression, path_bindings, &source_path)?
    else {
        return Err("runtime object list append subset requires Enter-key WHEN source".to_string());
    };
    let bool_fields = object_append_bool_field_defaults(target_expression, functions)?;
    Ok((
        (spec.trigger_binding, spec.event_name),
        ObjectListUpdate::AppendDraftObject {
            binding: binding_path.to_string(),
            source_binding: spec.source_binding,
            key: Some(spec.key),
            trim: spec.trim,
            reject_empty: spec.reject_empty,
            clear_draft: spec.clear_draft,
            bool_fields,
        },
    ))
}

fn object_append_target_supported<'a>(
    expression: &'a StaticSpannedExpression,
    functions: &BTreeMap<String, FunctionSpec<'a>>,
) -> bool {
    let StaticExpression::FunctionCall { path, .. } = &expression.node else {
        return false;
    };
    path.len() == 1
        && functions.contains_key(path[0].as_str())
        && resolve_static_object_field_expression(expression, functions, "title").is_some()
        && resolve_static_object_field_expression(expression, functions, "completed").is_some()
}

fn object_append_bool_field_defaults<'a>(
    expression: &'a StaticSpannedExpression,
    functions: &BTreeMap<String, FunctionSpec<'a>>,
) -> Result<BTreeMap<String, bool>, String> {
    let object = resolve_static_object_runtime_expression(expression, functions)?;
    let Some(object) = object else {
        return Ok(BTreeMap::new());
    };
    let mut output = BTreeMap::new();
    for variable in &object.variables {
        if variable.node.name.as_str() == "completed" {
            continue;
        }
        if let Some(spec) = detect_local_bool_spec(&variable.node.value)? {
            output.insert(variable.node.name.as_str().to_string(), spec.initial);
        }
    }
    Ok(output)
}

fn detect_dynamic_object_remove_action(
    expression: &StaticSpannedExpression,
    item_name: &str,
) -> Result<Option<ObjectItemActionSpec>, String> {
    let Some(remove) = detect_static_object_remove_spec(expression, item_name)? else {
        return Ok(None);
    };
    let Some(kind) = ui_event_kind_for_name(&remove.event_name) else {
        return Ok(None);
    };
    Ok(Some(ObjectItemActionSpec {
        source_binding_suffix: remove.trigger_binding,
        kind,
        action: ObjectItemActionKind::RemoveSelf,
    }))
}

fn detect_dynamic_object_bulk_remove_action(
    expression: &StaticSpannedExpression,
    item_name: &str,
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    binding_path: &str,
) -> Result<Option<((String, String), ObjectListUpdate)>, String> {
    let StaticExpression::Pipe { from, to } = &expression.node else {
        return Ok(None);
    };
    let Some((trigger_binding, event_name)) =
        canonical_event_source_path(from, path_bindings, binding_path)?
    else {
        return Ok(None);
    };
    let StaticExpression::Then { body } = &to.node else {
        return Ok(None);
    };
    let StaticExpression::Pipe {
        from: condition_source,
        to: condition_when,
    } = &body.node
    else {
        return Ok(None);
    };
    let StaticExpression::Alias(static_expression::Alias::WithoutPassed { parts, .. }) =
        &condition_source.node
    else {
        return Ok(None);
    };
    if parts.len() != 2 || parts[0].as_str() != item_name || parts[1].as_str() != "completed" {
        return Ok(None);
    }
    let StaticExpression::When { arms } = &condition_when.node else {
        return Ok(None);
    };
    let has_true_remove = arms.iter().any(|arm| {
        matches!(
            &arm.pattern,
            static_expression::Pattern::Literal(static_expression::Literal::Tag(tag))
                if tag.as_str() == "True"
        ) && is_remove_signal_body(&arm.body)
    });
    let has_false_skip = arms.iter().any(|arm| {
        matches!(
            &arm.pattern,
            static_expression::Pattern::Literal(static_expression::Literal::Tag(tag))
                if tag.as_str() == "False"
        ) && matches!(&arm.body.node, StaticExpression::Skip)
    });
    if !has_true_remove || !has_false_skip {
        return Ok(None);
    }
    Ok(Some((
        (trigger_binding, event_name),
        ObjectListUpdate::RemoveMatching {
            binding: String::new(),
            filter: ObjectListFilter::BoolFieldEquals {
                field: "completed".to_string(),
                value: true,
            },
        },
    )))
}

fn is_remove_signal_body(expression: &StaticSpannedExpression) -> bool {
    match &expression.node {
        StaticExpression::List { items } => items.is_empty(),
        StaticExpression::Object(object) => object.variables.is_empty(),
        _ => false,
    }
}

fn detect_text_list_pipeline(
    expression: &StaticSpannedExpression,
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    binding_path: &str,
) -> Result<Option<(Vec<String>, ListEventUpdates)>, String> {
    match &expression.node {
        StaticExpression::List { items } => match static_text_list_items(items) {
            Ok(initial_items) => Ok(Some((initial_items, Vec::new()))),
            Err(_) => Ok(None),
        },
        StaticExpression::Pipe { from, to } => {
            let Some((initial_items, mut updates)) =
                detect_text_list_pipeline(from, path_bindings, binding_path)?
            else {
                return Ok(None);
            };
            let StaticExpression::FunctionCall { path, arguments } = &to.node else {
                return Ok(None);
            };
            if path_matches(path, &["List", "append"]) {
                let item = find_named_argument(arguments, "item")
                    .ok_or_else(|| "List/append requires `item`".to_string())?;
                let ((trigger_binding, event_name), update) =
                    detect_text_list_append_update(item, path_bindings, binding_path)?;
                updates.push(((trigger_binding, event_name), update));
                return Ok(Some((initial_items, updates)));
            }
            if path_matches(path, &["List", "clear"]) {
                let on = find_named_argument(arguments, "on")
                    .ok_or_else(|| "List/clear requires `on`".to_string())?;
                let Some((trigger_binding, event_name)) =
                    canonical_event_source_path(on, path_bindings, binding_path)?
                else {
                    return Ok(None);
                };
                updates.push((
                    (trigger_binding, event_name),
                    TextListUpdate::Clear {
                        binding: binding_path.to_string(),
                    },
                ));
                return Ok(Some((initial_items, updates)));
            }
            Ok(None)
        }
        _ => Ok(None),
    }
}

fn static_text_list_items(items: &[StaticSpannedExpression]) -> Result<Vec<String>, String> {
    items.iter().map(static_text_item).collect()
}

fn static_text_item(expression: &StaticSpannedExpression) -> Result<String, String> {
    match &expression.node {
        StaticExpression::Literal(static_expression::Literal::Text(text))
        | StaticExpression::Literal(static_expression::Literal::Tag(text)) => {
            Ok(text.as_str().to_string())
        }
        StaticExpression::Literal(static_expression::Literal::Number(number)) => {
            Ok(trim_number(*number))
        }
        StaticExpression::TextLiteral { parts, .. } => {
            let mut text = String::new();
            for part in parts {
                match part {
                    StaticTextPart::Text(part) => text.push_str(part.as_str()),
                    StaticTextPart::Interpolation { .. } => {
                        return Err(
                            "runtime text list subset does not support interpolated initial items"
                                .to_string(),
                        );
                    }
                }
            }
            Ok(text)
        }
        StaticExpression::Latest { inputs } => inputs
            .iter()
            .rev()
            .find_map(|input| static_text_item(input).ok())
            .ok_or_else(|| {
                "runtime text list subset requires static text items, found `LATEST`".to_string()
            }),
        _ => Err(format!(
            "runtime text list subset requires static text items, found `{}`",
            describe_expression(expression)
        )),
    }
}

fn detect_text_list_append_update(
    expression: &StaticSpannedExpression,
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    binding_path: &str,
) -> Result<((String, String), TextListUpdate), String> {
    let source_path = canonical_reference_path(expression, path_bindings, binding_path)?
        .ok_or_else(|| "List/append runtime subset requires a named source binding".to_string())?;
    let source_expression = path_bindings
        .get(&source_path)
        .copied()
        .ok_or_else(|| format!("unknown append source binding `{source_path}`"))?;
    let Some(spec) = detect_append_from_key_when(source_expression, path_bindings, &source_path)?
    else {
        return Err("runtime List/append subset requires Enter-key WHEN source".to_string());
    };
    Ok((
        (spec.trigger_binding, spec.event_name),
        TextListUpdate::AppendDraftText {
            binding: binding_path.to_string(),
            source_binding: spec.source_binding,
            key: Some(spec.key),
            trim: spec.trim,
            reject_empty: spec.reject_empty,
            clear_draft: spec.clear_draft,
        },
    ))
}

#[derive(Debug, Clone)]
struct AppendFromKeySpec {
    trigger_binding: String,
    event_name: String,
    source_binding: String,
    key: String,
    trim: bool,
    reject_empty: bool,
    clear_draft: bool,
}

fn detect_append_from_key_when(
    expression: &StaticSpannedExpression,
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    binding_path: &str,
) -> Result<Option<AppendFromKeySpec>, String> {
    let StaticExpression::Pipe { from, to } = &expression.node else {
        return Ok(None);
    };
    let Some((trigger_binding, event_name, payload_field)) =
        canonical_event_payload_source_path(from, path_bindings, binding_path)?
    else {
        return Ok(None);
    };
    if event_name != "key_down" || payload_field != "key" {
        return Ok(None);
    }
    let StaticExpression::When { arms } = &to.node else {
        return Ok(None);
    };

    let mut enter_body = None;
    let mut has_skip_wildcard = false;
    for arm in arms {
        match &arm.pattern {
            static_expression::Pattern::Literal(static_expression::Literal::Tag(tag))
                if tag.as_str() == "Enter" =>
            {
                enter_body = Some(&arm.body);
            }
            static_expression::Pattern::WildCard
                if matches!(arm.body.node, StaticExpression::Skip) =>
            {
                has_skip_wildcard = true;
            }
            _ => {}
        }
    }
    let Some(body) = enter_body else {
        return Ok(None);
    };
    if !has_skip_wildcard {
        return Ok(None);
    }
    let Some((source_binding, trim, reject_empty)) =
        append_body_uses_draft_text(body, path_bindings, binding_path)?
    else {
        return Ok(None);
    };
    Ok(Some(AppendFromKeySpec {
        trigger_binding,
        event_name,
        source_binding,
        key: "Enter".to_string(),
        trim,
        reject_empty,
        clear_draft: true,
    }))
}

fn append_body_uses_draft_text(
    expression: &StaticSpannedExpression,
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    binding_path: &str,
) -> Result<Option<(String, bool, bool)>, String> {
    if let Some(source_binding) = text_field_binding(expression, path_bindings, binding_path)? {
        return Ok(Some((source_binding, false, false)));
    }

    let StaticExpression::Block { variables, output } = &expression.node else {
        return Ok(None);
    };
    if variables.len() != 1 {
        return Ok(None);
    }
    let trimmed_name = variables[0].node.name.as_str();
    if trimmed_name.is_empty() {
        return Ok(None);
    }
    let StaticExpression::Pipe { from, to } = &variables[0].node.value.node else {
        return Ok(None);
    };
    let Some(source_binding) = text_field_binding(from, path_bindings, binding_path)? else {
        return Ok(None);
    };
    let StaticExpression::FunctionCall { path, arguments } = &to.node else {
        return Ok(None);
    };
    if !path_matches(path, &["Text", "trim"]) || !arguments.is_empty() {
        return Ok(None);
    }
    if trimmed_non_empty_output(output, trimmed_name)? {
        return Ok(Some((source_binding.clone(), true, true)));
    }
    let StaticExpression::Pipe {
        from: condition_from,
        to: condition_to,
    } = &output.node
    else {
        return Ok(None);
    };
    if !matches!(
        &condition_from.node,
        StaticExpression::Alias(static_expression::Alias::WithoutPassed { parts, .. })
            if parts.len() == 1 && parts[0].as_str() == trimmed_name
    ) {
        return Ok(None);
    }
    let StaticExpression::FunctionCall { path, arguments } = &condition_to.node else {
        return Ok(None);
    };
    if !path_matches(path, &["Text", "is_not_empty"]) || !arguments.is_empty() {
        return Ok(None);
    }
    Ok(Some((source_binding, true, true)))
}

fn trimmed_non_empty_output(
    expression: &StaticSpannedExpression,
    trimmed_name: &str,
) -> Result<bool, String> {
    let StaticExpression::Pipe {
        from: condition_source,
        to: when_expression,
    } = &expression.node
    else {
        return Ok(false);
    };
    let StaticExpression::Pipe {
        from: condition_from,
        to: condition_to,
    } = &condition_source.node
    else {
        return Ok(false);
    };
    if !matches!(
        &condition_from.node,
        StaticExpression::Alias(static_expression::Alias::WithoutPassed { parts, .. })
            if parts.len() == 1 && parts[0].as_str() == trimmed_name
    ) {
        return Ok(false);
    }
    let StaticExpression::FunctionCall { path, arguments } = &condition_to.node else {
        return Ok(false);
    };
    if !path_matches(path, &["Text", "is_not_empty"]) || !arguments.is_empty() {
        return Ok(false);
    }
    let StaticExpression::When { arms } = &when_expression.node else {
        return Ok(false);
    };

    let mut has_true_trimmed = false;
    let mut has_false_skip = false;
    for arm in arms {
        match (&arm.pattern, &arm.body.node) {
            (
                static_expression::Pattern::Literal(static_expression::Literal::Tag(tag)),
                StaticExpression::Alias(static_expression::Alias::WithoutPassed { parts, .. }),
            ) if tag.as_str() == "True"
                && parts.len() == 1
                && parts[0].as_str() == trimmed_name =>
            {
                has_true_trimmed = true;
            }
            (
                static_expression::Pattern::Literal(static_expression::Literal::Tag(tag)),
                StaticExpression::Skip,
            ) if tag.as_str() == "False" => {
                has_false_skip = true;
            }
            _ => {}
        }
    }
    Ok(has_true_trimmed && has_false_skip)
}

fn text_field_binding(
    expression: &StaticSpannedExpression,
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    binding_path: &str,
) -> Result<Option<String>, String> {
    let StaticExpression::Alias(static_expression::Alias::WithoutPassed { parts, .. }) =
        &expression.node
    else {
        return Ok(None);
    };
    if parts.last().map(crate::parser::StrSlice::as_str) != Some("text") {
        return Ok(None);
    }
    let binding = canonical_parts_path(&parts[..parts.len() - 1], path_bindings, binding_path);
    Ok(path_bindings.contains_key(&binding).then_some(binding))
}

fn push_event_update(
    plan: &mut ScalarPlan,
    trigger_binding: &str,
    event_name: &str,
    update: ScalarUpdate,
) {
    plan.event_updates
        .entry((trigger_binding.to_string(), event_name.to_string()))
        .or_default()
        .push(update);
}

fn detect_latest_value_specs(
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
) -> Result<BTreeMap<String, LatestValueSpec>, String> {
    let mut specs = BTreeMap::new();
    for (binding_name, expression) in path_bindings {
        if let Some(spec) =
            latest_value_spec_for_expression(expression, path_bindings, binding_name)?
        {
            specs.insert(binding_name.clone(), spec);
        }
    }
    Ok(specs)
}

fn detect_selected_filter_specs(
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
) -> Result<BTreeMap<String, LatestValueSpec>, String> {
    let mut specs = BTreeMap::new();
    for (binding_name, expression) in path_bindings {
        if let Some(spec) =
            selected_filter_spec_for_expression(expression, path_bindings, binding_name)?
        {
            specs.insert(binding_name.clone(), spec);
        }
    }
    Ok(specs)
}

fn latest_value_spec_for_expression(
    expression: &StaticSpannedExpression,
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    binding_path: &str,
) -> Result<Option<LatestValueSpec>, String> {
    let StaticExpression::Latest { inputs } = &expression.node else {
        return Ok(None);
    };
    if inputs.is_empty() {
        return Ok(None);
    }

    let mut static_emissions = Vec::new();
    let mut event_values = Vec::new();
    for input in inputs {
        if let Some(value) = extract_integer_literal_opt(input)? {
            static_emissions.push(value);
            continue;
        }
        let Some(event_value) = latest_event_value(input, path_bindings, binding_path)? else {
            return Ok(None);
        };
        event_values.push(event_value);
    }

    let Some(initial_value) = static_emissions.last().copied() else {
        return Ok(None);
    };

    Ok(Some(LatestValueSpec {
        initial_value,
        static_sum: static_emissions.iter().sum(),
        event_values,
    }))
}

fn latest_event_value(
    expression: &StaticSpannedExpression,
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    binding_path: &str,
) -> Result<Option<EventValueSpec>, String> {
    let StaticExpression::Pipe {
        from: trigger_source,
        to: trigger_then,
    } = &expression.node
    else {
        return Ok(None);
    };
    let Some((trigger_binding, event_name)) =
        canonical_event_source_path(trigger_source, path_bindings, binding_path)?
    else {
        return Ok(None);
    };
    let StaticExpression::Then { body } = &trigger_then.node else {
        return Ok(None);
    };
    let Some(value) = extract_integer_literal_opt(body)? else {
        return Ok(None);
    };
    Ok(Some(EventValueSpec {
        trigger_binding,
        event_name,
        value,
    }))
}

fn selected_filter_spec_for_expression(
    expression: &StaticSpannedExpression,
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    _binding_path: &str,
) -> Result<Option<LatestValueSpec>, String> {
    let Some(initial_value) = selected_filter_initial_value(expression)? else {
        return Ok(None);
    };

    let mut event_values = Vec::new();
    for (candidate_binding, candidate_expression) in path_bindings {
        if let Some(candidate_events) =
            route_navigation_event_values(candidate_expression, path_bindings, candidate_binding)?
        {
            event_values.extend(candidate_events);
        }
    }

    if event_values.is_empty() {
        return Ok(None);
    }

    Ok(Some(LatestValueSpec {
        initial_value,
        static_sum: initial_value,
        event_values,
    }))
}

fn selected_filter_initial_value(
    expression: &StaticSpannedExpression,
) -> Result<Option<i64>, String> {
    let StaticExpression::Pipe { from, to } = &expression.node else {
        return Ok(None);
    };
    let StaticExpression::FunctionCall { path, arguments } = &from.node else {
        return Ok(None);
    };
    if !path_matches(path, &["Router", "route"]) || !arguments.is_empty() {
        return Ok(None);
    }
    let StaticExpression::While { arms } = &to.node else {
        return Ok(None);
    };

    let mut saw_all = false;
    let mut saw_active = false;
    let mut saw_completed = false;
    let mut default_value = None;

    for arm in arms {
        match &arm.pattern {
            static_expression::Pattern::Literal(static_expression::Literal::Text(text)) => {
                match (text.as_str(), extract_filter_tag_value(&arm.body)?) {
                    ("/", Some(0)) => saw_all = true,
                    ("/active", Some(1)) => saw_active = true,
                    ("/completed", Some(2)) => saw_completed = true,
                    _ => return Ok(None),
                }
            }
            static_expression::Pattern::WildCard => {
                default_value = extract_filter_tag_value(&arm.body)?;
            }
            _ => return Ok(None),
        }
    }

    if saw_all && saw_active && saw_completed {
        return Ok(Some(default_value.unwrap_or(0)));
    }

    Ok(None)
}

fn extract_filter_tag_value(expression: &StaticSpannedExpression) -> Result<Option<i64>, String> {
    match &expression.node {
        StaticExpression::Literal(static_expression::Literal::Tag(tag)) => match tag.as_str() {
            "All" => Ok(Some(0)),
            "Active" => Ok(Some(1)),
            "Completed" => Ok(Some(2)),
            _ => Ok(None),
        },
        _ => Ok(None),
    }
}

fn route_navigation_event_values(
    expression: &StaticSpannedExpression,
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    binding_path: &str,
) -> Result<Option<Vec<EventValueSpec>>, String> {
    let StaticExpression::Pipe { from, to } = &expression.node else {
        return Ok(None);
    };
    let StaticExpression::FunctionCall { path, arguments } = &to.node else {
        return Ok(None);
    };
    if !path_matches(path, &["Router", "go_to"]) || !arguments.is_empty() {
        return Ok(None);
    }
    let StaticExpression::Latest { inputs } = &from.node else {
        return Ok(None);
    };

    let mut event_values = Vec::new();
    for input in inputs {
        let StaticExpression::Pipe {
            from: trigger_source,
            to: trigger_then,
        } = &input.node
        else {
            return Ok(None);
        };
        let Some((trigger_binding, event_name)) =
            canonical_event_source_path(trigger_source, path_bindings, binding_path)?
        else {
            return Ok(None);
        };
        let StaticExpression::Then { body } = &trigger_then.node else {
            return Ok(None);
        };
        let route = static_text_item(body)?;
        let value = match route.as_str() {
            "/" => 0,
            "/active" => 1,
            "/completed" => 2,
            _ => continue,
        };
        event_values.push(EventValueSpec {
            trigger_binding,
            event_name,
            value,
        });
    }

    if event_values.is_empty() {
        return Ok(None);
    }

    Ok(Some(event_values))
}

fn latest_text_spec_for_expression(
    expression: &StaticSpannedExpression,
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    binding_path: &str,
) -> Result<Option<(String, Vec<((String, String), TextUpdate)>)>, String> {
    let StaticExpression::Latest { inputs } = &expression.node else {
        return Ok(None);
    };
    if inputs.is_empty() {
        return Ok(None);
    }

    let mut initial_value = None;
    let mut updates = Vec::new();
    for input in inputs {
        if let Some(value) = extract_static_text_value(input)? {
            initial_value = Some(value);
            continue;
        }
        let Some(update) = text_event_update(input, path_bindings, binding_path)? else {
            return Ok(None);
        };
        updates.push(update);
    }

    if updates.is_empty() {
        return Ok(None);
    }

    Ok(Some((initial_value.unwrap_or_default(), updates)))
}

fn hold_text_spec_for_expression(
    expression: &StaticSpannedExpression,
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    binding_path: &str,
) -> Result<Option<(String, Vec<((String, String), TextUpdate)>)>, String> {
    let StaticExpression::Pipe { from, to } = &expression.node else {
        return Ok(None);
    };
    let StaticExpression::Hold { body, .. } = &to.node else {
        return Ok(None);
    };
    let Some(initial_value) = extract_static_text_value(from)? else {
        return Ok(None);
    };

    let mut updates = Vec::new();
    match &body.node {
        StaticExpression::Latest { inputs } => {
            for input in inputs {
                let Some(update) = text_event_update(input, path_bindings, binding_path)? else {
                    return Ok(None);
                };
                updates.push(update);
            }
        }
        _ => {
            let Some(update) = text_event_update(body, path_bindings, binding_path)? else {
                return Ok(None);
            };
            updates.push(update);
        }
    }

    if updates.is_empty() {
        return Ok(None);
    }

    Ok(Some((initial_value, updates)))
}

fn text_event_update(
    expression: &StaticSpannedExpression,
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    binding_path: &str,
) -> Result<Option<((String, String), TextUpdate)>, String> {
    if let Some((trigger_binding, event_name, payload_field)) =
        canonical_event_payload_source_path(expression, path_bindings, binding_path)?
    {
        if payload_field == "text" {
            return Ok(Some((
                (trigger_binding, event_name),
                TextUpdate::SetFromPayload {
                    binding: binding_path.to_string(),
                },
            )));
        }
    }

    let StaticExpression::Pipe {
        from: trigger_source,
        to: trigger_then,
    } = &expression.node
    else {
        return Ok(None);
    };

    if let Some((trigger_binding, event_name)) =
        canonical_event_source_path(trigger_source, path_bindings, binding_path)?
    {
        if let Some(update) = text_update_from_body(trigger_then, path_bindings, binding_path, None)?
        {
            return Ok(Some(((trigger_binding, event_name), update)));
        }
    }

    let Some((trigger_binding, event_name, payload_field)) =
        canonical_event_payload_source_path(trigger_source, path_bindings, binding_path)?
    else {
        return Ok(None);
    };

    match &trigger_then.node {
        StaticExpression::Then { body } => {
            let Some(update) =
                text_update_from_body(body, path_bindings, binding_path, None)?
            else {
                return Ok(None);
            };
            Ok(Some(((trigger_binding, event_name), update)))
        }
        StaticExpression::When { arms } => {
            for arm in arms {
                match &arm.pattern {
                    static_expression::Pattern::Alias { name } => {
                        if payload_field == "text"
                            && matches!(
                                &arm.body.node,
                                StaticExpression::Alias(static_expression::Alias::WithoutPassed {
                                    parts,
                                    ..
                                }) if parts.len() == 1 && parts[0].as_str() == name.as_str()
                            )
                        {
                            return Ok(Some((
                                (trigger_binding, event_name),
                                TextUpdate::SetFromPayload {
                                    binding: binding_path.to_string(),
                                },
                            )));
                        }
                    }
                    static_expression::Pattern::Literal(static_expression::Literal::Tag(tag))
                    | static_expression::Pattern::Literal(static_expression::Literal::Text(tag)) => {
                        let Some(update) = text_update_from_body(
                            &arm.body,
                            path_bindings,
                            binding_path,
                            Some(tag.as_str().to_string()),
                        )? else {
                            continue;
                        };
                        return Ok(Some(((trigger_binding, event_name), update)));
                    }
                    _ => {}
                }
            }
            Ok(None)
        }
        _ => Ok(None),
    }
}

fn text_update_from_body(
    expression: &StaticSpannedExpression,
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    binding_path: &str,
    payload_filter: Option<String>,
) -> Result<Option<TextUpdate>, String> {
    if let Some(value) = extract_static_text_value(expression)? {
        return Ok(Some(TextUpdate::SetStatic {
            binding: binding_path.to_string(),
            value,
            payload_filter,
        }));
    }
    if let Some(source_binding) = text_field_binding(expression, path_bindings, binding_path)? {
        return Ok(Some(TextUpdate::SetFromInput {
            binding: binding_path.to_string(),
            source_binding,
            payload_filter,
        }));
    }
    Ok(None)
}

fn extract_static_text_value(
    expression: &StaticSpannedExpression,
) -> Result<Option<String>, String> {
    match &expression.node {
        StaticExpression::Literal(static_expression::Literal::Text(text))
        | StaticExpression::Literal(static_expression::Literal::Tag(text)) => {
            Ok(Some(text.as_str().to_string()))
        }
        StaticExpression::FunctionCall { path, arguments }
            if path_matches(path, &["Text", "empty"]) && arguments.is_empty() =>
        {
            Ok(Some(String::new()))
        }
        StaticExpression::FunctionCall { path, arguments }
            if path_matches(path, &["Text", "space"]) && arguments.is_empty() =>
        {
            Ok(Some(" ".to_string()))
        }
        StaticExpression::TextLiteral { .. } => Ok(static_text_item(expression).ok()),
        _ => Ok(None),
    }
}

fn sum_source_binding(
    expression: &StaticSpannedExpression,
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    binding_path: &str,
) -> Result<Option<String>, String> {
    let StaticExpression::Pipe { from, to } = &expression.node else {
        return Ok(None);
    };
    let StaticExpression::FunctionCall { path, arguments } = &to.node else {
        return Ok(None);
    };
    if !path_matches(path, &["Math", "sum"]) || !arguments.is_empty() {
        return Ok(None);
    }
    canonical_reference_path(from, path_bindings, binding_path)
}

fn counter_spec_for_expression(
    expression: &StaticSpannedExpression,
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    binding_path: &str,
) -> Result<Option<CounterSpec>, String> {
    if let Some(spec) = counter_spec_for_hold_expression(expression, path_bindings, binding_path)? {
        return Ok(Some(spec));
    }

    let StaticExpression::Pipe { from, to } = &expression.node else {
        return Ok(None);
    };
    let StaticExpression::FunctionCall { path, arguments } = &to.node else {
        return Ok(None);
    };
    if !path_matches(path, &["Math", "sum"]) || !arguments.is_empty() {
        return Ok(None);
    }

    let StaticExpression::Latest { inputs } = &from.node else {
        return Ok(None);
    };
    if inputs.len() != 2 {
        return Ok(None);
    }

    let Some(initial) = extract_integer_literal_opt(&inputs[0])? else {
        return Ok(None);
    };
    let StaticExpression::Pipe {
        from: trigger_source,
        to: trigger_then,
    } = &inputs[1].node
    else {
        return Ok(None);
    };
    let Some((trigger_binding, event_name)) =
        canonical_event_source_path(trigger_source, path_bindings, binding_path)?
    else {
        return Ok(None);
    };
    if event_name != "press" {
        return Ok(None);
    }
    let StaticExpression::Then { body } = &trigger_then.node else {
        return Ok(None);
    };
    let Some(delta) = extract_then_delta_opt(body, None)? else {
        return Ok(None);
    };

    Ok(Some(CounterSpec {
        initial,
        events: vec![EventDeltaSpec {
            trigger_binding,
            event_name,
            delta,
        }],
    }))
}

fn counter_spec_for_hold_expression(
    expression: &StaticSpannedExpression,
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    binding_path: &str,
) -> Result<Option<CounterSpec>, String> {
    let StaticExpression::Pipe { from, to } = &expression.node else {
        return Ok(None);
    };
    let StaticExpression::Hold { state_param, body } = &to.node else {
        return Ok(None);
    };
    let Some(initial) = extract_integer_literal_opt(from)? else {
        return Ok(None);
    };
    counter_spec_for_hold_body(
        initial,
        state_param.as_str(),
        body,
        path_bindings,
        binding_path,
    )
}

fn counter_spec_for_hold_body(
    initial: i64,
    state_param: &str,
    body: &StaticSpannedExpression,
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    binding_path: &str,
) -> Result<Option<CounterSpec>, String> {
    let mut events = Vec::new();
    match &body.node {
        StaticExpression::Pipe {
            from: trigger_source,
            to: trigger_then,
        } => {
            let Some((trigger_binding, event_name)) =
                canonical_event_source_path(trigger_source, path_bindings, binding_path)?
            else {
                return Ok(None);
            };
            let StaticExpression::Then { body } = &trigger_then.node else {
                return Ok(None);
            };
            let Some(delta) = extract_then_delta_opt(body, Some(state_param))? else {
                return Ok(None);
            };
            events.push(EventDeltaSpec {
                trigger_binding,
                event_name,
                delta,
            });
        }
        StaticExpression::Latest { inputs } => {
            for input in inputs {
                let StaticExpression::Pipe {
                    from: trigger_source,
                    to: trigger_then,
                } = &input.node
                else {
                    return Ok(None);
                };
                let Some((trigger_binding, event_name)) =
                    canonical_event_source_path(trigger_source, path_bindings, binding_path)?
                else {
                    return Ok(None);
                };
                let StaticExpression::Then { body } = &trigger_then.node else {
                    return Ok(None);
                };
                let Some(delta) = extract_then_delta_opt(body, Some(state_param))? else {
                    return Ok(None);
                };
                events.push(EventDeltaSpec {
                    trigger_binding,
                    event_name,
                    delta,
                });
            }
        }
        _ => return Ok(None),
    }

    if events.is_empty() {
        return Ok(None);
    }

    Ok(Some(CounterSpec { initial, events }))
}

fn canonical_reference_path(
    expression: &StaticSpannedExpression,
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    binding_path: &str,
) -> Result<Option<String>, String> {
    match &expression.node {
        StaticExpression::Alias(static_expression::Alias::WithoutPassed { parts, .. }) => Ok(Some(
            canonical_parts_path(parts, path_bindings, binding_path),
        )),
        _ => Ok(None),
    }
}

fn canonical_event_source_path(
    expression: &StaticSpannedExpression,
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    binding_path: &str,
) -> Result<Option<(String, String)>, String> {
    let StaticExpression::Alias(static_expression::Alias::WithoutPassed { parts, .. }) =
        &expression.node
    else {
        return Ok(None);
    };
    if parts.len() < 3 {
        return Ok(None);
    }
    let event_index = parts.len() - 2;
    if parts[event_index].as_str() != "event" {
        return Ok(None);
    }
    let trigger_binding = canonical_parts_path(&parts[..event_index], path_bindings, binding_path);
    let event_name = parts[event_index + 1].as_str().to_string();
    Ok(Some((trigger_binding, event_name)))
}

fn canonical_event_payload_source_path(
    expression: &StaticSpannedExpression,
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    binding_path: &str,
) -> Result<Option<(String, String, String)>, String> {
    let StaticExpression::Alias(static_expression::Alias::WithoutPassed { parts, .. }) =
        &expression.node
    else {
        return Ok(None);
    };
    if parts.len() < 4 {
        return Ok(None);
    }
    let event_index = parts.len() - 2;
    if parts[event_index - 1].as_str() != "event" {
        return Ok(None);
    }
    let trigger_binding =
        canonical_parts_path(&parts[..event_index - 1], path_bindings, binding_path);
    Ok(Some((
        trigger_binding,
        parts[event_index].as_str().to_string(),
        parts[event_index + 1].as_str().to_string(),
    )))
}

fn canonical_parts_path(
    parts: &[crate::parser::StrSlice],
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    binding_path: &str,
) -> String {
    let joined = parts
        .iter()
        .map(crate::parser::StrSlice::as_str)
        .collect::<Vec<_>>()
        .join(".");
    if path_bindings.contains_key(&joined) {
        return joined;
    }
    if let Some(scope_base) = binding_scope_base(binding_path) {
        let candidate = format!("{scope_base}.{joined}");
        if path_bindings.contains_key(&candidate) {
            return candidate;
        }
    }
    joined
}

fn binding_scope_base(binding_path: &str) -> Option<&str> {
    binding_path.rsplit_once('.').map(|(base, _)| base)
}

fn extract_integer_literal_opt(
    expression: &StaticSpannedExpression,
) -> Result<Option<i64>, String> {
    match &expression.node {
        StaticExpression::Literal(static_expression::Literal::Number(_)) => {
            Ok(Some(extract_integer_literal(expression)?))
        }
        _ => Ok(None),
    }
}

fn extract_integer_literal(expression: &StaticSpannedExpression) -> Result<i64, String> {
    let StaticExpression::Literal(static_expression::Literal::Number(number)) = &expression.node
    else {
        return Err(format!(
            "counter subset requires numeric literals, found `{}`",
            describe_expression_detailed(expression)
        ));
    };
    if number.fract() != 0.0 {
        return Err("counter subset requires integer numeric literals".to_string());
    }
    Ok(*number as i64)
}

fn extract_bool_literal(expression: &StaticSpannedExpression) -> Result<bool, String> {
    match &expression.node {
        StaticExpression::Literal(static_expression::Literal::Tag(tag))
            if tag.as_str() == "True" =>
        {
            Ok(true)
        }
        StaticExpression::Literal(static_expression::Literal::Tag(tag))
            if tag.as_str() == "False" =>
        {
            Ok(false)
        }
        _ => Err("bool subset requires True or False literals".to_string()),
    }
}

fn extract_bool_literal_opt(expression: &StaticSpannedExpression) -> Result<Option<bool>, String> {
    match &expression.node {
        StaticExpression::Literal(static_expression::Literal::Tag(tag))
            if tag.as_str() == "True" =>
        {
            Ok(Some(true))
        }
        StaticExpression::Literal(static_expression::Literal::Tag(tag))
            if tag.as_str() == "False" =>
        {
            Ok(Some(false))
        }
        _ => Ok(None),
    }
}

fn extract_then_delta(
    expression: &StaticSpannedExpression,
    state_param: Option<&str>,
) -> Result<i64, String> {
    match &expression.node {
        StaticExpression::Literal(static_expression::Literal::Number(_)) => {
            extract_integer_literal(expression)
        }
        StaticExpression::ArithmeticOperator(static_expression::ArithmeticOperator::Add {
            operand_a,
            operand_b,
        }) => {
            if operand_matches_state(operand_a, state_param) {
                extract_integer_literal(operand_b)
            } else if operand_matches_state(operand_b, state_param) {
                extract_integer_literal(operand_a)
            } else {
                Err("HOLD counter subset expects `state + <integer>`".to_string())
            }
        }
        StaticExpression::ArithmeticOperator(static_expression::ArithmeticOperator::Subtract {
            operand_a,
            operand_b,
        }) => {
            if operand_matches_state(operand_a, state_param) {
                Ok(-extract_integer_literal(operand_b)?)
            } else {
                Err("HOLD counter subset expects `state - <integer>`".to_string())
            }
        }
        _ => Err(
            "counter subset requires THEN { <integer> } or THEN { state +/- <integer> }"
                .to_string(),
        ),
    }
}

fn extract_then_delta_opt(
    expression: &StaticSpannedExpression,
    state_param: Option<&str>,
) -> Result<Option<i64>, String> {
    match extract_then_delta(expression, state_param) {
        Ok(delta) => Ok(Some(delta)),
        Err(error)
            if error.starts_with("counter subset requires numeric literals")
                || error == "counter subset requires integer numeric literals"
                || error
                    == "counter subset requires THEN { <integer> } or THEN { state +/- <integer> }"
                || error == "HOLD counter subset expects `state + <integer>`"
                || error == "HOLD counter subset expects `state - <integer>`" =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

fn operand_matches_state(expression: &StaticSpannedExpression, state_param: Option<&str>) -> bool {
    let Some(state_param) = state_param else {
        return false;
    };
    matches!(
        &expression.node,
        StaticExpression::Alias(static_expression::Alias::WithoutPassed { parts, .. })
            if parts.len() == 1 && parts[0].as_str() == state_param
    )
}

fn runtime_model_for(context: &LowerContext<'_>) -> RuntimeModel {
    if context.scalar_plan.initial_values.is_empty()
        && context.text_plan.initial_values.is_empty()
        && context.list_plan.initial_values.is_empty()
        && context.object_list_plan.initial_values.is_empty()
    {
        RuntimeModel::Static
    } else if context.text_plan.initial_values.is_empty()
        && context.list_plan.initial_values.is_empty()
        && context.object_list_plan.initial_values.is_empty()
    {
        RuntimeModel::Scalars(ScalarRuntimeModel {
            values: context.scalar_plan.initial_values.clone(),
        })
    } else {
        RuntimeModel::State(StateRuntimeModel {
            scalar_values: context.scalar_plan.initial_values.clone(),
            text_values: context.text_plan.initial_values.clone(),
            text_lists: context.list_plan.initial_values.clone(),
            object_lists: context.object_list_plan.initial_values.clone(),
            input_texts: BTreeMap::new(),
            derived_scalars: context.scalar_plan.derived_scalars.clone(),
        })
    }
}

fn resolve_object(expression: &StaticSpannedExpression) -> Option<&StaticObject> {
    match &expression.node {
        StaticExpression::Object(object) => Some(object),
        _ => None,
    }
}

fn resolve_static_list_items<'a>(
    expression: &'a StaticSpannedExpression,
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<Vec<&'a StaticSpannedExpression>, String> {
    let expression = resolve_alias(expression, context, locals, passed, stack)?;
    match &expression.node {
        StaticExpression::List { items } => Ok(items.iter().collect()),
        StaticExpression::FunctionCall { path, arguments }
            if path.len() == 1 && context.functions.contains_key(path[0].as_str()) =>
        {
            let function = &context.functions[path[0].as_str()];
            let bindings = function_argument_bindings(function, arguments)?;
            let specialized = specialize_static_expression(function.body, &bindings);
            resolve_static_list_items(specialized, context, stack, locals, passed)
        }
        StaticExpression::FunctionCall { path, arguments }
            if path_matches(path, &["List", "range"]) =>
        {
            let from = find_named_argument(arguments, "from")
                .ok_or_else(|| "List/range requires `from`".to_string())?;
            let to = find_named_argument(arguments, "to")
                .ok_or_else(|| "List/range requires `to`".to_string())?;
            let from = extract_integer_literal(from)?;
            let to = extract_integer_literal(to)?;
            let values = if from <= to {
                (from..=to).collect::<Vec<_>>()
            } else {
                (to..=from).rev().collect::<Vec<_>>()
            };
            Ok(values
                .into_iter()
                .map(|value| synthetic_integer_expression(value, expression))
                .collect())
        }
        StaticExpression::Pipe { from, to } => {
            let StaticExpression::FunctionCall { path, arguments } = &to.node else {
                return Err(format!(
                    "expected a static LIST source, found `{}`",
                    describe_expression_detailed(expression)
                ));
            };
            if path_matches(path, &["List", "retain"]) {
                let predicate_name = find_positional_parameter_name(arguments)
                    .ok_or_else(|| "List/retain requires an item parameter name".to_string())?;
                let condition = find_named_argument(arguments, "if")
                    .ok_or_else(|| "List/retain requires `if`".to_string())?;
                let items = resolve_static_list_items(from, context, stack, locals, passed)?;
                let mut retained = Vec::new();
                for item in items {
                    let mut scope = BTreeMap::new();
                    scope.insert(
                        predicate_name.to_string(),
                        LocalBinding {
                            expr: Some(item),
                            object_base: None,
                        },
                    );
                    locals.push(scope);
                    let keep = eval_static_bool(condition, context, stack, locals, passed);
                    locals.pop();
                    if keep? {
                        retained.push(item);
                    }
                }
                Ok(retained)
            } else if path_matches(path, &["List", "map"]) {
                let mapper_name = find_positional_parameter_name(arguments)
                    .ok_or_else(|| "List/map requires an item parameter name".to_string())?;
                let new = find_named_argument(arguments, "new")
                    .ok_or_else(|| "List/map requires `new`".to_string())?;
                let items = resolve_static_list_items(from, context, stack, locals, passed)?;
                Ok(items
                    .into_iter()
                    .map(|item| {
                        let mut bindings = BTreeMap::new();
                        bindings.insert(mapper_name.to_string(), item);
                        specialize_static_expression(new, &bindings)
                    })
                    .collect())
            } else if path_matches(path, &["List", "remove"]) {
                resolve_static_list_items(from, context, stack, locals, passed)
            } else {
                Err(format!(
                    "expected a static LIST source, found `{}`",
                    describe_expression_detailed(expression)
                ))
            }
        }
        _ => Err(format!(
            "expected a static LIST source, found `{}`",
            describe_expression_detailed(expression)
        )),
    }
}

fn synthetic_integer_expression(
    value: i64,
    template: &StaticSpannedExpression,
) -> &'static StaticSpannedExpression {
    Box::leak(Box::new(static_expression::Spanned {
        span: template.span,
        persistence: template.persistence,
        node: StaticExpression::Literal(static_expression::Literal::Number(value as f64)),
    }))
}

fn function_argument_bindings<'a>(
    function: &FunctionSpec<'a>,
    arguments: &'a [static_expression::Spanned<StaticArgument>],
) -> Result<BTreeMap<String, &'a StaticSpannedExpression>, String> {
    if function.parameters.len() != arguments.len() {
        return Err("static function expansion requires matching argument count".to_string());
    }
    let mut bindings = BTreeMap::new();
    for parameter in &function.parameters {
        let argument = arguments
            .iter()
            .find(|argument| argument.node.name.as_str() == parameter)
            .and_then(|argument| argument.node.value.as_ref())
            .ok_or_else(|| format!("static function expansion requires argument `{parameter}`"))?;
        bindings.insert(parameter.clone(), argument);
    }
    Ok(bindings)
}

fn specialize_static_expression(
    expression: &StaticSpannedExpression,
    bindings: &BTreeMap<String, &StaticSpannedExpression>,
) -> &'static StaticSpannedExpression {
    Box::leak(Box::new(static_expression::Spanned {
        span: expression.span,
        persistence: expression.persistence,
        node: specialize_static_expression_node(&expression.node, bindings),
    }))
}

fn specialize_static_expression_node(
    expression: &StaticExpression,
    bindings: &BTreeMap<String, &StaticSpannedExpression>,
) -> StaticExpression {
    match expression {
        StaticExpression::Alias(static_expression::Alias::WithoutPassed { parts, .. })
            if parts.len() == 1 =>
        {
            if let Some(bound) = bindings.get(parts[0].as_str()) {
                return bound.node.clone();
            }
            expression.clone()
        }
        StaticExpression::List { items } => StaticExpression::List {
            items: items
                .iter()
                .map(|item| static_expression::Spanned {
                    span: item.span,
                    persistence: item.persistence,
                    node: specialize_static_expression_node(&item.node, bindings),
                })
                .collect(),
        },
        StaticExpression::Object(object) => StaticExpression::Object(static_expression::Object {
            variables: object
                .variables
                .iter()
                .map(|variable| static_expression::Spanned {
                    span: variable.span,
                    persistence: variable.persistence,
                    node: static_expression::Variable {
                        name: variable.node.name.clone(),
                        is_referenced: variable.node.is_referenced,
                        value: static_expression::Spanned {
                            span: variable.node.value.span,
                            persistence: variable.node.value.persistence,
                            node: specialize_static_expression_node(
                                &variable.node.value.node,
                                bindings,
                            ),
                        },
                        value_changed: variable.node.value_changed,
                    },
                })
                .collect(),
        }),
        StaticExpression::TaggedObject { tag, object } => StaticExpression::TaggedObject {
            tag: tag.clone(),
            object: static_expression::Object {
                variables: object
                    .variables
                    .iter()
                    .map(|variable| static_expression::Spanned {
                        span: variable.span,
                        persistence: variable.persistence,
                        node: static_expression::Variable {
                            name: variable.node.name.clone(),
                            is_referenced: variable.node.is_referenced,
                            value: static_expression::Spanned {
                                span: variable.node.value.span,
                                persistence: variable.node.value.persistence,
                                node: specialize_static_expression_node(
                                    &variable.node.value.node,
                                    bindings,
                                ),
                            },
                            value_changed: variable.node.value_changed,
                        },
                    })
                    .collect(),
            },
        },
        StaticExpression::FunctionCall { path, arguments } => StaticExpression::FunctionCall {
            path: path.clone(),
            arguments: arguments
                .iter()
                .map(|argument| static_expression::Spanned {
                    span: argument.span,
                    persistence: argument.persistence,
                    node: static_expression::Argument {
                        name: argument.node.name.clone(),
                        is_referenced: argument.node.is_referenced,
                        value: argument.node.value.as_ref().map(|value| {
                            static_expression::Spanned {
                                span: value.span,
                                persistence: value.persistence,
                                node: specialize_static_expression_node(&value.node, bindings),
                            }
                        }),
                    },
                })
                .collect(),
        },
        StaticExpression::Pipe { from, to } => StaticExpression::Pipe {
            from: Box::new(static_expression::Spanned {
                span: from.span,
                persistence: from.persistence,
                node: specialize_static_expression_node(&from.node, bindings),
            }),
            to: Box::new(static_expression::Spanned {
                span: to.span,
                persistence: to.persistence,
                node: specialize_static_expression_node(&to.node, bindings),
            }),
        },
        StaticExpression::Block { variables, output } => StaticExpression::Block {
            variables: variables
                .iter()
                .map(|variable| static_expression::Spanned {
                    span: variable.span,
                    persistence: variable.persistence,
                    node: static_expression::Variable {
                        name: variable.node.name.clone(),
                        is_referenced: variable.node.is_referenced,
                        value: static_expression::Spanned {
                            span: variable.node.value.span,
                            persistence: variable.node.value.persistence,
                            node: specialize_static_expression_node(
                                &variable.node.value.node,
                                bindings,
                            ),
                        },
                        value_changed: variable.node.value_changed,
                    },
                })
                .collect(),
            output: Box::new(static_expression::Spanned {
                span: output.span,
                persistence: output.persistence,
                node: specialize_static_expression_node(&output.node, bindings),
            }),
        },
        StaticExpression::Latest { inputs } => StaticExpression::Latest {
            inputs: inputs
                .iter()
                .map(|input| static_expression::Spanned {
                    span: input.span,
                    persistence: input.persistence,
                    node: specialize_static_expression_node(&input.node, bindings),
                })
                .collect(),
        },
        StaticExpression::Then { body } => StaticExpression::Then {
            body: Box::new(static_expression::Spanned {
                span: body.span,
                persistence: body.persistence,
                node: specialize_static_expression_node(&body.node, bindings),
            }),
        },
        StaticExpression::When { arms } => StaticExpression::When {
            arms: arms
                .iter()
                .map(|arm| static_expression::Arm {
                    pattern: arm.pattern.clone(),
                    body: static_expression::Spanned {
                        span: arm.body.span,
                        persistence: arm.body.persistence,
                        node: specialize_static_expression_node(&arm.body.node, bindings),
                    },
                })
                .collect(),
        },
        StaticExpression::While { arms } => StaticExpression::While {
            arms: arms
                .iter()
                .map(|arm| static_expression::Arm {
                    pattern: arm.pattern.clone(),
                    body: static_expression::Spanned {
                        span: arm.body.span,
                        persistence: arm.body.persistence,
                        node: specialize_static_expression_node(&arm.body.node, bindings),
                    },
                })
                .collect(),
        },
        StaticExpression::ArithmeticOperator(operator) => {
            use static_expression::ArithmeticOperator;
            StaticExpression::ArithmeticOperator(match operator {
                ArithmeticOperator::Negate { operand } => ArithmeticOperator::Negate {
                    operand: Box::new(static_expression::Spanned {
                        span: operand.span,
                        persistence: operand.persistence,
                        node: specialize_static_expression_node(&operand.node, bindings),
                    }),
                },
                ArithmeticOperator::Add {
                    operand_a,
                    operand_b,
                } => ArithmeticOperator::Add {
                    operand_a: Box::new(static_expression::Spanned {
                        span: operand_a.span,
                        persistence: operand_a.persistence,
                        node: specialize_static_expression_node(&operand_a.node, bindings),
                    }),
                    operand_b: Box::new(static_expression::Spanned {
                        span: operand_b.span,
                        persistence: operand_b.persistence,
                        node: specialize_static_expression_node(&operand_b.node, bindings),
                    }),
                },
                ArithmeticOperator::Subtract {
                    operand_a,
                    operand_b,
                } => ArithmeticOperator::Subtract {
                    operand_a: Box::new(static_expression::Spanned {
                        span: operand_a.span,
                        persistence: operand_a.persistence,
                        node: specialize_static_expression_node(&operand_a.node, bindings),
                    }),
                    operand_b: Box::new(static_expression::Spanned {
                        span: operand_b.span,
                        persistence: operand_b.persistence,
                        node: specialize_static_expression_node(&operand_b.node, bindings),
                    }),
                },
                ArithmeticOperator::Multiply {
                    operand_a,
                    operand_b,
                } => ArithmeticOperator::Multiply {
                    operand_a: Box::new(static_expression::Spanned {
                        span: operand_a.span,
                        persistence: operand_a.persistence,
                        node: specialize_static_expression_node(&operand_a.node, bindings),
                    }),
                    operand_b: Box::new(static_expression::Spanned {
                        span: operand_b.span,
                        persistence: operand_b.persistence,
                        node: specialize_static_expression_node(&operand_b.node, bindings),
                    }),
                },
                ArithmeticOperator::Divide {
                    operand_a,
                    operand_b,
                } => ArithmeticOperator::Divide {
                    operand_a: Box::new(static_expression::Spanned {
                        span: operand_a.span,
                        persistence: operand_a.persistence,
                        node: specialize_static_expression_node(&operand_a.node, bindings),
                    }),
                    operand_b: Box::new(static_expression::Spanned {
                        span: operand_b.span,
                        persistence: operand_b.persistence,
                        node: specialize_static_expression_node(&operand_b.node, bindings),
                    }),
                },
            })
        }
        StaticExpression::Comparator(comparator) => {
            use static_expression::Comparator;
            StaticExpression::Comparator(match comparator {
                Comparator::Equal {
                    operand_a,
                    operand_b,
                } => Comparator::Equal {
                    operand_a: Box::new(static_expression::Spanned {
                        span: operand_a.span,
                        persistence: operand_a.persistence,
                        node: specialize_static_expression_node(&operand_a.node, bindings),
                    }),
                    operand_b: Box::new(static_expression::Spanned {
                        span: operand_b.span,
                        persistence: operand_b.persistence,
                        node: specialize_static_expression_node(&operand_b.node, bindings),
                    }),
                },
                Comparator::NotEqual {
                    operand_a,
                    operand_b,
                } => Comparator::NotEqual {
                    operand_a: Box::new(static_expression::Spanned {
                        span: operand_a.span,
                        persistence: operand_a.persistence,
                        node: specialize_static_expression_node(&operand_a.node, bindings),
                    }),
                    operand_b: Box::new(static_expression::Spanned {
                        span: operand_b.span,
                        persistence: operand_b.persistence,
                        node: specialize_static_expression_node(&operand_b.node, bindings),
                    }),
                },
                Comparator::Greater {
                    operand_a,
                    operand_b,
                } => Comparator::Greater {
                    operand_a: Box::new(static_expression::Spanned {
                        span: operand_a.span,
                        persistence: operand_a.persistence,
                        node: specialize_static_expression_node(&operand_a.node, bindings),
                    }),
                    operand_b: Box::new(static_expression::Spanned {
                        span: operand_b.span,
                        persistence: operand_b.persistence,
                        node: specialize_static_expression_node(&operand_b.node, bindings),
                    }),
                },
                Comparator::GreaterOrEqual {
                    operand_a,
                    operand_b,
                } => Comparator::GreaterOrEqual {
                    operand_a: Box::new(static_expression::Spanned {
                        span: operand_a.span,
                        persistence: operand_a.persistence,
                        node: specialize_static_expression_node(&operand_a.node, bindings),
                    }),
                    operand_b: Box::new(static_expression::Spanned {
                        span: operand_b.span,
                        persistence: operand_b.persistence,
                        node: specialize_static_expression_node(&operand_b.node, bindings),
                    }),
                },
                Comparator::Less {
                    operand_a,
                    operand_b,
                } => Comparator::Less {
                    operand_a: Box::new(static_expression::Spanned {
                        span: operand_a.span,
                        persistence: operand_a.persistence,
                        node: specialize_static_expression_node(&operand_a.node, bindings),
                    }),
                    operand_b: Box::new(static_expression::Spanned {
                        span: operand_b.span,
                        persistence: operand_b.persistence,
                        node: specialize_static_expression_node(&operand_b.node, bindings),
                    }),
                },
                Comparator::LessOrEqual {
                    operand_a,
                    operand_b,
                } => Comparator::LessOrEqual {
                    operand_a: Box::new(static_expression::Spanned {
                        span: operand_a.span,
                        persistence: operand_a.persistence,
                        node: specialize_static_expression_node(&operand_a.node, bindings),
                    }),
                    operand_b: Box::new(static_expression::Spanned {
                        span: operand_b.span,
                        persistence: operand_b.persistence,
                        node: specialize_static_expression_node(&operand_b.node, bindings),
                    }),
                },
            })
        }
        _ => expression.clone(),
    }
}

fn find_positional_parameter_name(
    arguments: &[static_expression::Spanned<StaticArgument>],
) -> Option<&str> {
    arguments
        .iter()
        .find(|argument| argument.node.value.is_none() && argument.node.name.as_str() != "PASS")
        .map(|argument| argument.node.name.as_str())
}

fn eval_static_bool<'a>(
    expression: &'a StaticSpannedExpression,
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<bool, String> {
    let expression = resolve_alias(expression, context, locals, passed, stack)?;
    match &expression.node {
        StaticExpression::Literal(static_expression::Literal::Tag(tag)) => match tag.as_str() {
            "True" => Ok(true),
            "False" => Ok(false),
            _ => Err(format!(
                "expected boolean literal, found `{}`",
                tag.as_str()
            )),
        },
        StaticExpression::Comparator(comparator) => {
            let (left, right, op) = match comparator {
                static_expression::Comparator::Equal {
                    operand_a,
                    operand_b,
                } => (operand_a.as_ref(), operand_b.as_ref(), "=="),
                static_expression::Comparator::NotEqual {
                    operand_a,
                    operand_b,
                } => (operand_a.as_ref(), operand_b.as_ref(), "!="),
                static_expression::Comparator::Greater {
                    operand_a,
                    operand_b,
                } => (operand_a.as_ref(), operand_b.as_ref(), ">"),
                static_expression::Comparator::GreaterOrEqual {
                    operand_a,
                    operand_b,
                } => (operand_a.as_ref(), operand_b.as_ref(), ">="),
                static_expression::Comparator::Less {
                    operand_a,
                    operand_b,
                } => (operand_a.as_ref(), operand_b.as_ref(), "<"),
                static_expression::Comparator::LessOrEqual {
                    operand_a,
                    operand_b,
                } => (operand_a.as_ref(), operand_b.as_ref(), "<="),
            };
            let left = resolve_static_atom(left, context, stack, locals, passed)?;
            let right = resolve_static_atom(right, context, stack, locals, passed)?;
            match (left, right, op) {
                (StaticAtom::Number(a), StaticAtom::Number(b), "==") => Ok(a == b),
                (StaticAtom::Number(a), StaticAtom::Number(b), "!=") => Ok(a != b),
                (StaticAtom::Number(a), StaticAtom::Number(b), ">") => Ok(a > b),
                (StaticAtom::Number(a), StaticAtom::Number(b), ">=") => Ok(a >= b),
                (StaticAtom::Number(a), StaticAtom::Number(b), "<") => Ok(a < b),
                (StaticAtom::Number(a), StaticAtom::Number(b), "<=") => Ok(a <= b),
                (StaticAtom::Text(a), StaticAtom::Text(b), "==") => Ok(a == b),
                (StaticAtom::Text(a), StaticAtom::Text(b), "!=") => Ok(a != b),
                (StaticAtom::Bool(a), StaticAtom::Bool(b), "==") => Ok(a == b),
                (StaticAtom::Bool(a), StaticAtom::Bool(b), "!=") => Ok(a != b),
                _ => Err("unsupported static comparator operands".to_string()),
            }
        }
        _ => match resolve_static_atom(expression, context, stack, locals, passed)? {
            StaticAtom::Bool(value) => Ok(value),
            _ => Err("expected static boolean value".to_string()),
        },
    }
}

enum StaticAtom {
    Number(i64),
    Text(String),
    Bool(bool),
}

fn resolve_static_atom<'a>(
    expression: &'a StaticSpannedExpression,
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<StaticAtom, String> {
    let expression = resolve_alias(expression, context, locals, passed, stack)?;
    match &expression.node {
        StaticExpression::Literal(static_expression::Literal::Number(number)) => {
            if number.fract() != 0.0 {
                return Err("static collection subset requires integer numbers".to_string());
            }
            Ok(StaticAtom::Number(*number as i64))
        }
        StaticExpression::Literal(static_expression::Literal::Text(text))
        | StaticExpression::Literal(static_expression::Literal::Tag(text)) => match text.as_str() {
            "True" => Ok(StaticAtom::Bool(true)),
            "False" => Ok(StaticAtom::Bool(false)),
            value => Ok(StaticAtom::Text(value.to_string())),
        },
        StaticExpression::TextLiteral { parts, .. } => Ok(StaticAtom::Text(render_text_literal(
            parts, context, stack, locals, passed,
        )?)),
        _ => Err(format!(
            "expression `{}` is not a static list operand",
            describe_expression(expression)
        )),
    }
}

fn find_object_field<'a>(
    object: &'a StaticObject,
    name: &str,
) -> Option<&'a StaticSpannedExpression> {
    object
        .variables
        .iter()
        .find(|variable| variable.node.name.as_str() == name)
        .map(|variable| &variable.node.value)
}

fn extract_tag_name(expression: &StaticSpannedExpression) -> Option<&str> {
    match &expression.node {
        StaticExpression::Literal(static_expression::Literal::Tag(tag))
        | StaticExpression::Literal(static_expression::Literal::Text(tag)) => Some(tag.as_str()),
        _ => None,
    }
}

fn extract_number(expression: &StaticSpannedExpression) -> Option<f64> {
    match &expression.node {
        StaticExpression::Literal(static_expression::Literal::Number(number)) => Some(*number),
        _ => None,
    }
}

fn extract_placeholder_text(expression: &StaticSpannedExpression) -> Option<String> {
    let object = resolve_object(expression)?;
    let text = find_object_field(object, "text")?;
    match &text.node {
        StaticExpression::Literal(static_expression::Literal::Text(value))
        | StaticExpression::Literal(static_expression::Literal::Tag(value)) => {
            Some(value.as_str().to_string())
        }
        StaticExpression::TextLiteral { parts, .. } => {
            let mut output = String::new();
            for part in parts {
                match part {
                    StaticTextPart::Text(text) => output.push_str(text.as_str()),
                    StaticTextPart::Interpolation { .. } => return None,
                }
            }
            Some(output)
        }
        _ => None,
    }
}

fn text_empty_literal(expression: &StaticSpannedExpression) -> Result<Option<String>, String> {
    let StaticExpression::FunctionCall { path, arguments } = &expression.node else {
        return Ok(None);
    };
    if path_matches(path, &["Text", "empty"]) && arguments.is_empty() {
        return Ok(Some(String::new()));
    }
    Ok(None)
}

fn merge_style_property(properties: &mut Vec<(String, String)>, style_parts: Vec<String>) {
    if style_parts.is_empty() {
        return;
    }
    properties.push(("style".to_string(), style_parts.join(";")));
}

fn path_matches(path: &[crate::parser::StrSlice], expected: &[&str]) -> bool {
    path.len() == expected.len()
        && path
            .iter()
            .zip(expected)
            .all(|(segment, expected)| segment.as_str() == *expected)
}

fn trim_number(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        value.to_string()
    }
}

fn describe_expression(expression: &StaticSpannedExpression) -> &'static str {
    match &expression.node {
        StaticExpression::Variable(_) => "variable",
        StaticExpression::Literal(_) => "literal",
        StaticExpression::List { .. } => "list",
        StaticExpression::Object(_) => "object",
        StaticExpression::TaggedObject { .. } => "tagged object",
        StaticExpression::Map { .. } => "map",
        StaticExpression::Function { .. } => "function",
        StaticExpression::FunctionCall { .. } => "function call",
        StaticExpression::Alias(_) => "alias",
        StaticExpression::LinkSetter { .. } => "link setter",
        StaticExpression::Link => "link",
        StaticExpression::Latest { .. } => "LATEST",
        StaticExpression::Hold { .. } => "HOLD",
        StaticExpression::Then { .. } => "THEN",
        StaticExpression::Flush { .. } => "FLUSH",
        StaticExpression::Spread { .. } => "spread",
        StaticExpression::When { .. } => "WHEN",
        StaticExpression::While { .. } => "WHILE",
        StaticExpression::Pipe { .. } => "pipe",
        StaticExpression::Skip => "SKIP",
        StaticExpression::Block { .. } => "BLOCK",
        StaticExpression::Comparator(_) => "comparator",
        StaticExpression::ArithmeticOperator(_) => "arithmetic operator",
        StaticExpression::TextLiteral { .. } => "text literal",
        StaticExpression::Bits { .. } => "Bits",
        StaticExpression::Memory { .. } => "Memory",
        StaticExpression::Bytes { .. } => "Bytes",
        StaticExpression::FieldAccess { .. } => "field access",
        StaticExpression::PostfixFieldAccess { .. } => "postfix field access",
    }
}

fn describe_expression_detailed(expression: &StaticSpannedExpression) -> String {
    match &expression.node {
        StaticExpression::Pipe { from, to } => format!(
            "pipe({} -> {})",
            describe_expression_detailed(from),
            describe_expression_detailed(to)
        ),
        StaticExpression::FunctionCall { path, .. } => format!(
            "function call {}",
            path.iter()
                .map(|segment| segment.as_str())
                .collect::<Vec<_>>()
                .join("/")
        ),
        StaticExpression::Alias(alias) => match alias {
            static_expression::Alias::WithoutPassed { parts, .. } => {
                format!(
                    "alias {}",
                    parts
                        .iter()
                        .map(|part| part.as_str())
                        .collect::<Vec<_>>()
                        .join(".")
                )
            }
            static_expression::Alias::WithPassed { extra_parts } => format!(
                "alias PASSED.{}",
                extra_parts
                    .iter()
                    .map(|part| part.as_str())
                    .collect::<Vec<_>>()
                    .join(".")
            ),
        },
        StaticExpression::FieldAccess { path } => {
            format!(
                "field access .{}",
                path.iter()
                    .map(|part| part.as_str())
                    .collect::<Vec<_>>()
                    .join(".")
            )
        }
        StaticExpression::PostfixFieldAccess { field, .. } => {
            format!("postfix field access .{}", field.as_str())
        }
        _ => describe_expression(expression).to_string(),
    }
}

fn unsupported_program(error: String) -> SemanticProgram {
    SemanticProgram {
        root: SemanticNode::element(
            "section",
            Some("WasmPro parser-backed lowering".to_string()),
            Vec::new(),
            Vec::new(),
            vec![
                SemanticNode::text("This example is not supported by Wasm Pro yet."),
                SemanticNode::text(error),
            ],
        ),
        runtime: RuntimeModel::Static,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        LocalBinding, LowerContext, StaticExpression, augment_top_level_bool_item_runtime,
        augment_top_level_object_field_runtime, detect_list_plan,
        detect_object_list_plan, detect_scalar_plan, detect_static_object_list_plan,
        detect_text_plan, flatten_binding_paths, invocation_marker,
        latest_value_spec_for_expression, lower_text_value, lower_to_semantic,
        parse_static_expressions, top_level_bindings, top_level_functions,
        with_invoked_function_scope, StaticSpannedExpression,
    };
    use crate::platform::browser::engine_wasm_pro::semantic_ir::{
        IntCompareOp, ObjectItemActionKind, ObjectListFilter, RuntimeModel, ScalarUpdate,
        SemanticAction, SemanticInputValue, SemanticNode, SemanticTextPart, TextListFilter,
    };

    #[test]
    fn lower_to_semantic_lowers_hello_world_document() {
        let program = lower_to_semantic(
            "document: Document/new(root: TEXT { Hello world! })",
            None,
            false,
        );

        let SemanticNode::Text(text) = &program.root else {
            panic!("expected text root");
        };
        assert_eq!(text, "Hello world!");
    }

    #[test]
    fn lower_to_semantic_lowers_numeric_document_root() {
        let program = lower_to_semantic("document: Document/new(root: 123)", None, false);

        let SemanticNode::Text(text) = &program.root else {
            panic!("expected text root");
        };
        assert_eq!(text, "123");
    }

    #[test]
    fn lower_to_semantic_resolves_top_level_aliases_in_static_stripe() {
        let program = lower_to_semantic(
            r#"
document: Document/new(root: content)

content: Element/stripe(
    element: []
    direction: Row
    gap: 8
    style: []
    items: LIST {
        label
        button
    }
)

label: Element/label(element: [], style: [], label: TEXT { Hello })
button: Element/button(
    element: [event: [press: LINK]]
    style: []
    label: TEXT { Press }
)
"#,
            None,
            false,
        );

        let SemanticNode::Element {
            tag,
            properties,
            children,
            ..
        } = &program.root
        else {
            panic!("expected stripe element");
        };
        assert_eq!(tag, "div");
        assert!(properties.iter().any(|(name, value)| {
            name == "style" && value.contains("flex-direction:row") && value.contains("gap:8px")
        }));
        assert_eq!(children.len(), 2);
    }

    #[test]
    fn lower_to_semantic_lowers_static_latest_document_root() {
        let program = lower_to_semantic(
            r#"
document: Document/new(root: counter)
counter:
    LATEST {
        0
        1
    }
"#,
            None,
            false,
        );

        let RuntimeModel::Scalars(model) = &program.runtime else {
            panic!("expected scalar runtime model");
        };
        assert_eq!(model.values.get("counter"), Some(&1));
        assert!(matches!(
            &program.root,
            SemanticNode::ScalarValue { binding, value }
            if binding == "counter" && *value == 1
        ));
    }

    #[test]
    fn lower_to_semantic_detects_counter_program() {
        let program = lower_to_semantic(
            r#"
document: Document/new(root: Element/stripe(
    element: []
    direction: Column
    gap: 0
    style: []

    items: LIST {
        counter
        increment_button
    }
))

counter:
    LATEST {
        0
        increment_button.event.press |> THEN { 1 }
    }
    |> Math/sum()

increment_button: Element/button(
    element: [event: [press: LINK]]
    style: []
    label: TEXT { + }
)
"#,
            None,
            false,
        );

        let RuntimeModel::Scalars(model) = &program.runtime else {
            panic!("expected scalar runtime model, got {program:?}");
        };
        assert_eq!(model.values.get("counter"), Some(&0));

        let SemanticNode::Element { children, .. } = &program.root else {
            panic!("expected element root");
        };
        assert!(matches!(
            &children[0],
            SemanticNode::ScalarValue { binding, value }
            if binding == "counter" && *value == 0
        ));
        let SemanticNode::Element { event_bindings, .. } = &children[1] else {
            panic!("expected button element");
        };
        assert!(matches!(
            event_bindings.first().and_then(|binding| binding.action.as_ref()),
            Some(SemanticAction::UpdateScalars { updates })
            if matches!(
                updates.as_slice(),
                [ScalarUpdate::Add { binding, delta }]
                if binding == "counter" && *delta == 1
            )
        ));
    }

    #[test]
    fn lower_to_semantic_detects_hold_counter_program() {
        let program = lower_to_semantic(
            r#"
document: Document/new(root: Element/stripe(
    element: []
    direction: Column
    gap: 0
    style: []

    items: LIST {
        counter
        increment_button
    }
))

counter: 0 |> HOLD counter {
    increment_button.event.press |> THEN { counter + 1 }
}

increment_button: Element/button(
    element: [event: [press: LINK]]
    style: []
    label: TEXT { + }
)
"#,
            None,
            false,
        );

        let RuntimeModel::Scalars(model) = &program.runtime else {
            panic!("expected scalar runtime model, got {program:?}");
        };
        assert_eq!(model.values.get("counter"), Some(&0));

        let SemanticNode::Element { children, .. } = &program.root else {
            panic!("expected element root");
        };
        assert!(matches!(
            &children[0],
            SemanticNode::ScalarValue { binding, value }
            if binding == "counter" && *value == 0
        ));
        let SemanticNode::Element { event_bindings, .. } = &children[1] else {
            panic!("expected button element");
        };
        assert!(matches!(
            event_bindings.first().and_then(|binding| binding.action.as_ref()),
            Some(SemanticAction::UpdateScalars { updates })
            if matches!(
                updates.as_slice(),
                [ScalarUpdate::Add { binding, delta }]
                if binding == "counter" && *delta == 1
            )
        ));
    }

    #[test]
    fn lower_to_semantic_detects_latest_program_with_helper_functions() {
        let program = lower_to_semantic(
            r#"
value: LATEST {
    send_1_button.event.press |> THEN { 1 }
    send_2_button.event.press |> THEN { 2 }
    3
    4
}

sum: value |> Math/sum()
send_1_button: send_button(label: TEXT { Send 1 })
send_2_button: send_button(label: TEXT { Send 2 })

document: Document/new(root: Element/stripe(
    element: []
    direction: Column
    style: []

    items: LIST {
        send_1_button
        send_2_button
        value |> value_container
        TEXT { Sum: {sum} } |> value_container
    }
))

FUNCTION send_button(label) {
    Element/button(element: [event: [press: LINK]], style: [], label: label)
}

FUNCTION value_container(value) {
    Element/container(element: [], style: [align: [row: Center]], child: value)
}
"#,
            None,
            false,
        );

        let RuntimeModel::Scalars(model) = &program.runtime else {
            panic!(
                "expected scalar runtime model, got {:?} with root {:?}",
                program.runtime, program.root
            );
        };
        assert_eq!(model.values.get("value"), Some(&4));
        assert_eq!(model.values.get("sum"), Some(&7));

        let SemanticNode::Element { children, .. } = &program.root else {
            panic!("expected stripe root");
        };
        assert_eq!(children.len(), 4);
        let SemanticNode::Element {
            event_bindings: send_1_events,
            ..
        } = &children[0]
        else {
            panic!("expected first helper-expanded button");
        };
        assert!(matches!(
            send_1_events.first().and_then(|binding| binding.action.as_ref()),
            Some(SemanticAction::UpdateScalars { updates })
            if matches!(
                updates.as_slice(),
                [
                    ScalarUpdate::Set { binding: value_binding, value },
                    ScalarUpdate::Add { binding: sum_binding, delta }
                ]
                if value_binding == "value" && *value == 1 && sum_binding == "sum" && *delta == 1
            )
        ));

        let SemanticNode::Element {
            children: value_children,
            ..
        } = &children[2]
        else {
            panic!("expected value container");
        };
        assert!(matches!(
            &value_children[0],
            SemanticNode::ScalarValue { binding, value } if binding == "value" && *value == 4
        ));

        let SemanticNode::Element {
            children: sum_children,
            ..
        } = &children[3]
        else {
            panic!("expected sum container");
        };
        assert!(
            matches!(
                &sum_children[0],
                SemanticNode::TextTemplate { parts, value }
                if value == "Sum: 7"
                    && matches!(
                        parts.as_slice(),
                        [
                            SemanticTextPart::Static(prefix),
                            SemanticTextPart::ScalarBinding(binding)
                        ] if prefix == "Sum: " && binding == "sum"
                    )
            ),
            "unexpected sum child: {:?}",
            sum_children[0]
        );
    }

    #[test]
    fn lower_to_semantic_detects_complex_counter_with_pass_and_links() {
        let program = lower_to_semantic(
            r#"
store: [
    elements: [decrement_button: LINK, increment_button: LINK]

    counter: 0 |> HOLD counter {
        LATEST {
            elements.decrement_button.event.press |> THEN { counter - 1 }
            elements.increment_button.event.press |> THEN { counter + 1 }
        }
    }
]

document: Document/new(root: root_element(PASS: store))

FUNCTION root_element() {
    Element/stripe(
        element: []
        direction: Row
        gap: 15
        style: [align: Center]

        items: LIST {
            counter_button(label: TEXT { - }) |> LINK { PASSED.elements.decrement_button }
            PASSED.counter
            counter_button(label: TEXT { + }) |> LINK { PASSED.elements.increment_button }
        }
    )
}

FUNCTION counter_button(label) {
    Element/button(
        element: [event: [press: LINK], hovered: LINK]
        style: [
            width: 45
            rounded_corners: Fully
            background: [
                color: Oklch[
                    lightness: element.hovered |> WHEN {
                        True => 0.85
                        False => 0.75
                    }
                    chroma: 0.07
                    hue: 320
                ]
            ]
        ]
        label: label
    )
}
"#,
            None,
            false,
        );

        let RuntimeModel::Scalars(model) = &program.runtime else {
            panic!(
                "expected scalar runtime model, got {:?} with root {:?}",
                program.runtime, program.root
            );
        };
        assert_eq!(model.values.get("store.counter"), Some(&0));

        let SemanticNode::Element { children, .. } = &program.root else {
            panic!("expected stripe root");
        };
        assert_eq!(children.len(), 3);
        let SemanticNode::Element {
            event_bindings: decrement_events,
            ..
        } = &children[0]
        else {
            panic!("expected decrement button");
        };
        assert!(matches!(
            decrement_events.first().and_then(|binding| binding.action.as_ref()),
            Some(SemanticAction::UpdateScalars { updates })
            if matches!(
                updates.as_slice(),
                [ScalarUpdate::Add { binding, delta }]
                if binding == "store.counter" && *delta == -1
            )
        ));

        assert!(matches!(
            &children[1],
            SemanticNode::ScalarValue { binding, value }
            if binding == "store.counter" && *value == 0
        ));

        let SemanticNode::Element {
            event_bindings: increment_events,
            ..
        } = &children[2]
        else {
            panic!("expected increment button");
        };
        assert!(matches!(
            increment_events.first().and_then(|binding| binding.action.as_ref()),
            Some(SemanticAction::UpdateScalars { updates })
            if matches!(
                updates.as_slice(),
                [ScalarUpdate::Add { binding, delta }]
                if binding == "store.counter" && *delta == 1
            )
        ));
    }

    #[test]
    fn lower_to_semantic_detects_static_object_list_item_counters() {
        let program = lower_to_semantic(
            include_str!(
                "../../../../../../playground/frontend/src/examples/list_object_state/list_object_state.bn"
            ),
            None,
            false,
        );

        let RuntimeModel::Scalars(model) = &program.runtime else {
            panic!(
                "expected scalar runtime model for list_object_state, got {:?} with root {:?}",
                program.runtime, program.root
            );
        };
        assert_eq!(model.values.get("store.counters[0].count"), Some(&0));
        assert_eq!(model.values.get("store.counters[1].count"), Some(&0));
        assert_eq!(model.values.get("store.counters[2].count"), Some(&0));

        let SemanticNode::Element { children, .. } = &program.root else {
            panic!("expected list_object_state root stripe");
        };
        assert_eq!(children.len(), 2);
        let SemanticNode::Element {
            children: counter_children,
            ..
        } = &children[1]
        else {
            panic!("expected counters stripe");
        };
        assert_eq!(counter_children.len(), 3);

        let SemanticNode::Element {
            children: first_counter_children,
            ..
        } = &counter_children[0]
        else {
            panic!("expected first counter stripe");
        };
        let SemanticNode::Element {
            event_bindings: button_events,
            ..
        } = &first_counter_children[0]
        else {
            panic!("expected first counter button");
        };
        assert!(matches!(
            button_events.first().and_then(|binding| binding.action.as_ref()),
            Some(SemanticAction::UpdateScalars { updates })
                if matches!(
                    updates.as_slice(),
                    [ScalarUpdate::Add { binding, delta }]
                        if binding == "store.counters[0].count" && *delta == 1
                )
        ));
        assert!(matches!(
            &first_counter_children[1],
            SemanticNode::Element { children, .. }
                if matches!(
                    children.as_slice(),
                    [SemanticNode::TextTemplate { value, .. }] if value == "Count: 0"
                )
        ));
    }

    #[test]
    fn lower_to_semantic_detects_static_object_list_item_booleans() {
        let program = lower_to_semantic(
            include_str!("../../../../../../playground/frontend/src/examples/checkbox_test.bn"),
            None,
            false,
        );

        let RuntimeModel::Scalars(model) = &program.runtime else {
            panic!(
                "expected scalar runtime model for checkbox_test, got {:?} with root {:?}",
                program.runtime, program.root
            );
        };
        assert_eq!(model.values.get("store.items[0].checked"), Some(&0));
        assert_eq!(model.values.get("store.items[1].checked"), Some(&0));

        let SemanticNode::Element { children, .. } = &program.root else {
            panic!("expected checkbox_test root stripe");
        };
        assert_eq!(children.len(), 2);

        let SemanticNode::Element {
            children: first_row_children,
            ..
        } = &children[0]
        else {
            panic!("expected first checkbox row");
        };
        let SemanticNode::Element {
            event_bindings: checkbox_events,
            children: checkbox_children,
            ..
        } = &first_row_children[0]
        else {
            panic!("expected checkbox button");
        };
        assert!(matches!(
            checkbox_events.first().and_then(|binding| binding.action.as_ref()),
            Some(SemanticAction::UpdateScalars { updates })
                if matches!(
                    updates.as_slice(),
                    [ScalarUpdate::ToggleBool { binding }]
                        if binding == "store.items[0].checked"
                )
        ));
        assert!(matches!(
            checkbox_children.as_slice(),
            [SemanticNode::TextTemplate { value, .. }] if value == "[ ]"
        ));
        assert!(matches!(
            &first_row_children[1],
            SemanticNode::Element { children, .. }
                if matches!(&children[0], SemanticNode::Text(text) if text == "Item A")
        ));
        assert!(matches!(
            &first_row_children[2],
            SemanticNode::Element { children, .. }
                if matches!(
                    children.as_slice(),
                    [SemanticNode::TextTemplate { value, .. }] if value == "(unchecked)"
                )
        ));
    }

    #[test]
    fn lower_to_semantic_detects_static_object_list_item_remove_buttons() {
        let program = lower_to_semantic(
            r#"
store: [
    items:
        LIST {
            make_item(name: TEXT { Item A })
            make_item(name: TEXT { Item B })
        }
        |> List/remove(item, on: item.remove_button.event.click)
]

FUNCTION make_item(name) {
    [
        remove_button: LINK
        name: name
    ]
}

document: Document/new(root: Element/stripe(
    element: []
    direction: Column
    gap: 10
    style: []

    items: store.items |> List/map(item, new: Element/stripe(
        element: []
        direction: Row
        gap: 10
        style: []

        items: LIST {
            Element/label(element: [], style: [], label: item.name)
            Element/button(
                element: [event: [click: LINK]]
                style: []
                label: TEXT { Remove }
            )
            |> LINK { item.remove_button }
        }
    ))
))
"#,
            None,
            false,
        );

        let RuntimeModel::Scalars(model) = &program.runtime else {
            panic!(
                "expected scalar runtime model for static remove test, got {:?} with root {:?}",
                program.runtime, program.root
            );
        };
        assert_eq!(model.values.get("store.items[0].__removed"), Some(&0));
        assert_eq!(model.values.get("store.items[1].__removed"), Some(&0));

        let SemanticNode::Element { children, .. } = &program.root else {
            panic!("expected remove test stripe root");
        };
        assert_eq!(children.len(), 2);
        let SemanticNode::BoolBranch {
            binding,
            truthy,
            falsy,
        } = &children[0]
        else {
            panic!("expected first row bool branch");
        };
        assert_eq!(binding, "store.items[0].__removed");
        assert!(matches!(&**truthy, SemanticNode::Fragment(nodes) if nodes.is_empty()));

        let SemanticNode::Element {
            children: first_row_children,
            ..
        } = &**falsy
        else {
            panic!("expected first row element in falsy branch");
        };
        assert!(matches!(
            &first_row_children[0],
            SemanticNode::Element { children, .. }
                if matches!(&children[0], SemanticNode::Text(text) if text == "Item A")
        ));
        let SemanticNode::Element {
            event_bindings: remove_events,
            ..
        } = &first_row_children[1]
        else {
            panic!("expected remove button element");
        };
        assert!(matches!(
            remove_events.first().and_then(|binding| binding.action.as_ref()),
            Some(SemanticAction::UpdateScalars { updates })
                if matches!(
                    updates.as_slice(),
                    [ScalarUpdate::Set { binding, value }]
                        if binding == "store.items[0].__removed" && *value == 1
                )
        ));
    }

    #[test]
    fn lower_to_semantic_lowers_static_list_map() {
        let program = lower_to_semantic(
            r#"
numbers: LIST {
    1
    2
    3
}

document: Document/new(root: Element/stripe(
    element: []
    direction: Row
    gap: 5
    style: []

    items: numbers |> List/map(
        item
        new: Element/label(element: [], style: [], label: item)
    )
))
"#,
            None,
            false,
        );

        let SemanticNode::Element { children, .. } = &program.root else {
            panic!("expected stripe root");
        };
        assert_eq!(children.len(), 3);
        for (child, expected) in children.iter().zip(["1", "2", "3"]) {
            let SemanticNode::Element { children, .. } = child else {
                panic!("expected label element child");
            };
            assert!(matches!(&children[0], SemanticNode::Text(text) if text == expected));
        }
    }

    #[test]
    fn lower_to_semantic_lowers_static_list_count_and_retain() {
        let program = lower_to_semantic(
            r#"
numbers: LIST {
    1
    2
    3
    4
}

retained_count: numbers |> List/retain(item, if: item > 2) |> List/count()

document: Document/new(root: TEXT { Retained: {retained_count} })
"#,
            None,
            false,
        );

        let SemanticNode::Text(text) = &program.root else {
            panic!("expected text root");
        };
        assert_eq!(text, "Retained: 2");
    }

    #[test]
    fn lower_to_semantic_detects_runtime_text_list_append_count_and_map() {
        let program = lower_to_semantic(
            r#"
store: [
    input: LINK

    text_to_add: store.input.event.key_down.key |> WHEN {
        Enter => store.input.text
        __ => SKIP
    }

    items:
        LIST {
            TEXT { Initial }
        }
        |> List/append(item: text_to_add)
]

document: Document/new(root: root_element(PASS: [store: store]))

FUNCTION root_element() {
    Element/stripe(
        element: []
        direction: Column
        gap: 10
        style: [padding: 20]

        items: LIST {
            Element/text_input(
                element: [event: [key_down: LINK, change: LINK]]
                style: []
                label: Hidden[text: TEXT { Add item }]

                text: LATEST {
                    Text/empty()
                    element.event.change.text
                }

                placeholder: [text: TEXT { Type and press Enter }]
                focus: True
            )
            |> LINK { PASSED.store.input }

            all_count_label()

            Element/stripe(
                element: []
                direction: Column
                gap: 5
                style: []

                items: PASSED.store.items
                |> List/map(item, new: Element/label(element: [], style: [], label: item))
            )
        }
    )
}

FUNCTION all_count_label() {
    Element/label(element: [], style: [], label: BLOCK {
        count: PASSED.store.items |> List/count()

        TEXT { All count: {count} }
    })
}
"#,
            None,
            false,
        );

        let RuntimeModel::State(model) = &program.runtime else {
            panic!(
                "expected state runtime model, got {:?} with root {:?}",
                program.runtime, program.root
            );
        };
        assert_eq!(
            model.text_lists.get("store.items"),
            Some(&vec!["Initial".to_string()])
        );

        let SemanticNode::Element { children, .. } = &program.root else {
            panic!("expected stripe root");
        };
        assert_eq!(children.len(), 3);
        let SemanticNode::Element {
            children: count_children,
            ..
        } = &children[1]
        else {
            panic!("expected count label");
        };
        assert!(matches!(
            &count_children[0],
            SemanticNode::TextTemplate { parts, value }
                if value == "All count: 1"
                    && matches!(
                        parts.as_slice(),
                        [
                            SemanticTextPart::Static(prefix),
                            SemanticTextPart::ListCountBinding(binding)
                        ] if prefix == "All count: " && binding == "store.items"
                    )
        ));

        let SemanticNode::Element {
            children: list_children,
            ..
        } = &children[2]
        else {
            panic!("expected items stripe");
        };
        assert!(matches!(
            &list_children[0],
            SemanticNode::TextList { binding, values, .. }
                if binding == "store.items" && values == &vec!["Initial".to_string()]
        ));
    }

    #[test]
    fn lower_to_semantic_detects_runtime_text_binding_branch() {
        let program = lower_to_semantic(
            r#"
store: [
    input: LINK
    value: LATEST {
        Text/empty()
        store.input.event.change.text
    }
]

document: Document/new(root: Element/stripe(
    element: []
    direction: Column
    gap: 10
    style: []

    items: LIST {
        Element/text_input(
            element: [event: [change: LINK]]
            style: []
            label: Hidden[text: TEXT { Value }]
            text: Text/empty()
            placeholder: []
            focus: False
        )
        |> LINK { store.input }

        store.value |> Text/is_empty() |> WHILE {
            True => Element/label(element: [], style: [], label: TEXT { Empty })
            False => Element/label(element: [], style: [], label: TEXT { {store.value} })
        }
    }
))
"#,
            None,
            false,
        );

        let RuntimeModel::State(model) = &program.runtime else {
            panic!("expected state runtime model for text binding branch");
        };
        assert_eq!(model.text_values.get("store.value").map(String::as_str), Some(""));

        let SemanticNode::Element { children, .. } = &program.root else {
            panic!("expected text binding root stripe");
        };
        assert!(matches!(
            &children[1],
            SemanticNode::TextBindingBranch { binding, invert, .. }
                if binding == "store.value" && !invert
        ));
    }

    #[test]
    fn lower_to_semantic_detects_object_template_text_input_value_branch() {
        let program = lower_to_semantic(
            r#"
store: [
    source: LINK

    editing_text: LATEST {
        TEXT { Draft }
        store.source.event.change.text
    }

    rows: LIST {
        [
            title: TEXT { A1 }
            formula: Text/empty()
        ]
        [
            title: TEXT { A2 }
            formula: TEXT { =A1 }
        ]
    }
]

document: Document/new(root: Element/stripe(
    element: []
    direction: Column
    gap: 10
    style: []

    items: LIST {
        Element/text_input(
            element: [event: [change: LINK]]
            style: []
            label: Hidden[text: TEXT { Source }]
            text: LATEST {
                TEXT { Draft }
                element.event.change.text
            }
            placeholder: []
            focus: False
        )
        |> LINK { store.source }

        Element/stripe(
            element: []
            direction: Column
            gap: 5
            style: []

            items: store.rows
            |> List/map(item, new: Element/text_input(
                element: []
                style: []
                label: Hidden[text: item.title]
                text: item.formula |> Text/is_empty() |> WHILE {
                    True => TEXT { {store.editing_text} }
                    False => item.formula
                }
                placeholder: []
                focus: False
            ))
        )
    }
))
"#,
            None,
            false,
        );

        let RuntimeModel::State(model) = &program.runtime else {
            panic!("expected state runtime model");
        };
        let rows = model
            .object_lists
            .get("store.rows")
            .expect("store.rows should be runtime-backed");
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[1].text_fields.get("formula").map(String::as_str),
            Some("=A1")
        );

        let SemanticNode::Element { children, .. } = &program.root else {
            panic!("expected root stripe");
        };
        let SemanticNode::Element {
            children: list_children,
            ..
        } = &children[1]
        else {
            panic!("expected rows stripe");
        };
        let SemanticNode::ObjectList { template, .. } = &list_children[0] else {
            panic!("expected object list");
        };
        let SemanticNode::Element { input_value, .. } = template.as_ref() else {
            panic!("expected text input template");
        };
        assert!(matches!(
            input_value,
            Some(SemanticInputValue::ObjectTextFieldBranch {
                field,
                invert,
                truthy,
                falsy,
            }) if field == "formula"
                && !invert
                && matches!(
                    truthy.as_ref(),
                    SemanticInputValue::TextParts { parts, .. }
                        if matches!(
                            parts.as_slice(),
                            [SemanticTextPart::TextBinding(binding)]
                                if binding == "store.editing_text"
                        )
                )
                && matches!(
                    falsy.as_ref(),
                    SemanticInputValue::TextParts { parts, .. }
                        if matches!(
                            parts.as_slice(),
                            [SemanticTextPart::ObjectFieldBinding(field)]
                                if field == "formula"
                        )
                )
        ));
    }

    #[test]
    fn lower_to_semantic_detects_shopping_list_example() {
        let program = lower_to_semantic(
            include_str!(
                "../../../../../../playground/frontend/src/examples/shopping_list/shopping_list.bn"
            ),
            None,
            false,
        );

        let RuntimeModel::State(model) = &program.runtime else {
            panic!(
                "expected state runtime model for shopping_list, got {:?} with root {:?}",
                program.runtime, program.root
            );
        };
        assert_eq!(model.text_lists.get("store.items"), Some(&Vec::new()));

        let SemanticNode::Element { children, .. } = &program.root else {
            panic!("expected shopping_list root stripe");
        };
        assert_eq!(children.len(), 4);
        let SemanticNode::Element {
            children: item_list_children,
            ..
        } = &children[2]
        else {
            panic!("expected items_list stripe");
        };
        assert!(matches!(
            &item_list_children[0],
            SemanticNode::TextList { binding, values, .. }
                if binding == "store.items" && values.is_empty()
        ));
    }

    #[test]
    fn lower_to_semantic_detects_list_retain_count_example() {
        let program = lower_to_semantic(
            include_str!(
                "../../../../../../playground/frontend/src/examples/list_retain_count/list_retain_count.bn"
            ),
            None,
            false,
        );

        let RuntimeModel::State(model) = &program.runtime else {
            panic!(
                "expected state runtime model for list_retain_count, got {:?} with root {:?}",
                program.runtime, program.root
            );
        };
        assert_eq!(
            model.text_lists.get("store.items"),
            Some(&vec!["Initial".to_string()])
        );

        let SemanticNode::Element { children, .. } = &program.root else {
            panic!("expected list_retain_count root stripe");
        };
        assert_eq!(children.len(), 4);
        let SemanticNode::Element {
            children: all_count_children,
            ..
        } = &children[1]
        else {
            panic!("expected all_count label");
        };
        assert!(matches!(
            &all_count_children[0],
            SemanticNode::TextTemplate { value, .. } if value == "All count: 1"
        ));
        let SemanticNode::Element {
            children: retain_count_children,
            ..
        } = &children[2]
        else {
            panic!("expected retain_count label");
        };
        assert!(matches!(
            &retain_count_children[0],
            SemanticNode::TextTemplate { value, .. } if value == "Retain count: 1"
        ));
    }

    #[test]
    fn lower_to_semantic_detects_filtered_runtime_text_list_views() {
        let program = lower_to_semantic(
            r#"
store: [
    input: LINK

    text_to_add: store.input.event.key_down.key |> WHEN {
        Enter => store.input.text
        __ => SKIP
    }

    items:
        LIST {
            1
            3
        }
        |> List/append(item: text_to_add)
]

document: Document/new(root: root_element(PASS: [store: store]))

FUNCTION root_element() {
    Element/stripe(
        element: []
        direction: Column
        gap: 10
        style: []

        items: LIST {
            Element/text_input(
                element: [event: [key_down: LINK, change: LINK]]
                style: []
                label: Hidden[text: TEXT { Add item }]

                text: LATEST {
                    Text/empty()
                    element.event.change.text
                }

                placeholder: [text: TEXT { Type number and press Enter }]
                focus: True
            )
            |> LINK { PASSED.store.input }

            Element/label(element: [], style: [], label: BLOCK {
                count: PASSED.store.items |> List/retain(item, if: item > 2) |> List/count()

                TEXT { Filtered count: {count} }
            })

            Element/stripe(
                element: []
                direction: Column
                gap: 5
                style: []

                items: PASSED.store.items
                |> List/retain(item, if: item > 2)
                |> List/map(item, new: Element/label(element: [], style: [], label: item))
            )
        }
    )
}
"#,
            None,
            false,
        );

        let SemanticNode::Element { children, .. } = &program.root else {
            panic!("expected filtered runtime list root stripe");
        };
        assert_eq!(children.len(), 3);

        let SemanticNode::Element {
            children: count_children,
            ..
        } = &children[1]
        else {
            panic!("expected filtered count label");
        };
        assert!(matches!(
            &count_children[0],
            SemanticNode::TextTemplate { parts, value }
                if value == "Filtered count: 1"
                    && matches!(
                        parts.as_slice(),
                        [
                            SemanticTextPart::Static(prefix),
                            SemanticTextPart::FilteredListCountBinding { binding, filter }
                        ] if prefix == "Filtered count: "
                            && binding == "store.items"
                            && matches!(
                                filter,
                                TextListFilter::IntCompare {
                                    op: IntCompareOp::Greater,
                                    value: 2
                                }
                            )
                    )
        ));

        let SemanticNode::Element {
            children: list_children,
            ..
        } = &children[2]
        else {
            panic!("expected filtered items stripe");
        };
        assert!(matches!(
            &list_children[0],
            SemanticNode::TextList {
                binding,
                values,
                filter: Some(TextListFilter::IntCompare {
                    op: IntCompareOp::Greater,
                    value: 2
                }),
                ..
            } if binding == "store.items" && values == &vec!["3".to_string()]
        ));
    }

    #[test]
    fn latest_value_spec_detects_multi_source_latest() {
        let expressions = parse_static_expressions(
            r#"
send_1_button: send_button(label: TEXT { Send 1 })
send_2_button: send_button(label: TEXT { Send 2 })

value: LATEST {
    send_1_button.event.press |> THEN { 1 }
    send_2_button.event.press |> THEN { 2 }
    3
    4
}

FUNCTION send_button(label) {
    Element/button(element: [event: [press: LINK]], style: [], label: label)
}
"#,
        )
        .expect("source should parse");
        let bindings = top_level_bindings(&expressions);
        let value = bindings.get("value").copied().expect("value binding");

        let path_bindings = super::flatten_binding_paths(&bindings);
        let spec = latest_value_spec_for_expression(value, &path_bindings, "value")
            .expect("latest detector should succeed")
            .expect("latest detector should match");

        assert_eq!(spec.initial_value, 4);
        assert_eq!(spec.static_sum, 7);
        assert_eq!(spec.event_values.len(), 2);
        assert_eq!(spec.event_values[0].trigger_binding, "send_1_button");
        assert_eq!(spec.event_values[0].event_name, "press");
        assert_eq!(spec.event_values[0].value, 1);
        assert_eq!(spec.event_values[1].trigger_binding, "send_2_button");
        assert_eq!(spec.event_values[1].event_name, "press");
        assert_eq!(spec.event_values[1].value, 2);
    }

    #[test]
    fn detect_scalar_plan_handles_latest_example_bindings() {
        let expressions = parse_static_expressions(
            r#"
value: LATEST {
    send_1_button.event.press |> THEN { 1 }
    send_2_button.event.press |> THEN { 2 }
    3
    4
}

sum: value |> Math/sum()
send_1_button: send_button(label: TEXT { Send 1 })
send_2_button: send_button(label: TEXT { Send 2 })

document: Document/new(root: Element/stripe(
    element: []
    direction: Column
    style: []

    items: LIST {
        send_1_button
        send_2_button
        value |> value_container
        TEXT { Sum: {sum} } |> value_container
    }
))

FUNCTION send_button(label) {
    Element/button(element: [event: [press: LINK]], style: [], label: label)
}

FUNCTION value_container(value) {
    Element/container(element: [], style: [align: [row: Center]], child: value)
}
"#,
        )
        .expect("source should parse");
        let bindings = top_level_bindings(&expressions);
        let path_bindings = super::flatten_binding_paths(&bindings);

        let functions = super::top_level_functions(&expressions);
        let plan =
            detect_scalar_plan(&path_bindings, &functions).expect("scalar plan should build");

        assert_eq!(plan.initial_values.get("value"), Some(&4));
        assert_eq!(plan.initial_values.get("sum"), Some(&7));
        assert!(
            plan.event_updates
                .contains_key(&("send_1_button".to_string(), "press".to_string()))
        );
        assert!(
            plan.event_updates
                .contains_key(&("send_2_button".to_string(), "press".to_string()))
        );
    }

    #[test]
    fn lower_to_semantic_detects_runtime_object_list_add_toggle_remove() {
        let program = lower_to_semantic(
            r#"
store: [
    input: LINK

    title_to_add: store.input.event.key_down.key |> WHEN {
        Enter => BLOCK {
            trimmed: store.input.text |> Text/trim()

            trimmed |> Text/is_not_empty() |> WHEN {
                True => trimmed
                False => SKIP
            }
        }
        __ => SKIP
    }

    todos:
        LIST {
            new_todo(title: TEXT { Buy milk })
            new_todo(title: TEXT { Clean room })
        }
        |> List/append(item: title_to_add |> new_todo())
        |> List/remove(item, on: item.remove_button.event.click)
]

FUNCTION new_todo(title) {
    [
        toggle_button: LINK
        remove_button: LINK
        title: title

        completed: False |> HOLD state {
            toggle_button.event.click |> THEN { state |> Bool/not() }
        }
    ]
}

document: Document/new(root: Element/stripe(
    element: []
    direction: Column
    gap: 10
    style: []

    items: LIST {
        Element/text_input(
            element: [event: [key_down: LINK, change: LINK]]
            style: []
            label: Hidden[text: TEXT { Add todo }]

            text: LATEST {
                Text/empty()
                element.event.change.text
            }

            placeholder: [text: TEXT { Type and press Enter }]
            focus: True
        )
        |> LINK { store.input }

        Element/label(element: [], style: [], label: BLOCK {
            count: store.todos |> List/count()

            TEXT { Count: {count} }
        })

        Element/stripe(
            element: []
            direction: Column
            gap: 5
            style: []

            items: store.todos |> List/map(item, new: Element/stripe(
                element: []
                direction: Row
                gap: 10
                style: []

                items: LIST {
                    Element/checkbox(
                        element: [event: [click: LINK]]
                        style: []
                        label: TEXT { Toggle }
                        checked: item.completed

                        icon: item.completed |> WHEN {
                            True => TEXT { [X] }
                            False => TEXT { [ ] }
                        }
                    )
                    |> LINK { item.toggle_button }

                    Element/label(element: [], style: [], label: item.title)

                    Element/label(element: [], style: [], label: item.completed |> WHEN {
                        True => TEXT { (done) }
                        False => TEXT { (active) }
                    })

                    Element/button(
                        element: [event: [click: LINK]]
                        style: []
                        label: TEXT { Remove }
                    )
                    |> LINK { item.remove_button }
                }
            ))
        )
    }
))
"#,
            None,
            false,
        );

        let RuntimeModel::State(model) = &program.runtime else {
            panic!(
                "expected state runtime model for object list test, got {:?} with root {:?}",
                program.runtime, program.root
            );
        };
        assert_eq!(model.object_lists.get("store.todos").map(Vec::len), Some(2));
        assert!(matches!(
            model.object_lists.get("store.todos").and_then(|items| items.first()),
            Some(item) if item.title == "Buy milk" && !item.completed
        ));

        let SemanticNode::Element { children, .. } = &program.root else {
            panic!("expected object list test stripe root");
        };
        assert_eq!(children.len(), 3);
        assert!(matches!(
            &children[1],
            SemanticNode::Element { children, .. }
                if matches!(
                    children.as_slice(),
                    [SemanticNode::TextTemplate { value, .. }] if value == "Count: 2"
                )
        ));
        let SemanticNode::Element {
            children: list_children,
            ..
        } = &children[2]
        else {
            panic!("expected todo list stripe");
        };
        assert!(matches!(
            &list_children[0],
            SemanticNode::ObjectList { binding, item_actions, .. }
                if binding == "store.todos"
                    && item_actions.iter().any(|action| action.source_binding_suffix == "toggle_button")
                    && item_actions.iter().any(|action| action.source_binding_suffix == "remove_button")
        ));
    }

    #[test]
    fn lower_to_semantic_detects_helper_filtered_object_list_counts() {
        let program = lower_to_semantic(
            r#"
store: [
    todos: LIST {
        new_todo(title: TEXT { Buy milk })
        new_todo(title: TEXT { Clean room })
    }
]

FUNCTION new_todo(title) {
    [
        toggle_button: LINK
        title: title

        completed: False |> HOLD state {
            toggle_button.event.click |> THEN { state |> Bool/not() }
        }
    ]
}

FUNCTION todo_row(todo) {
    Element/stripe(
        element: []
        direction: Row
        gap: 10
        style: []

        items: LIST {
            Element/checkbox(
                element: [event: [click: LINK]]
                style: []
                label: TEXT { Toggle }
                checked: todo.completed

                icon: todo.completed |> WHEN {
                    True => TEXT { [X] }
                    False => TEXT { [ ] }
                }
            )
            |> LINK { todo.toggle_button }

            Element/label(element: [], style: [], label: todo.title)
        }
    )
}

document: Document/new(root: Element/stripe(
    element: []
    direction: Column
    gap: 10
    style: []

    items: LIST {
        Element/label(element: [], style: [], label: BLOCK {
            count: store.todos |> List/retain(item, if: item.completed) |> List/count()

            TEXT { Completed: {count} }
        })

        Element/label(element: [], style: [], label: BLOCK {
            count: store.todos |> List/retain(item, if: item.completed |> Bool/not()) |> List/count()

            TEXT { Active: {count} }
        })

        Element/stripe(
            element: []
            direction: Column
            gap: 5
            style: []

            items: store.todos
            |> List/retain(item, if: item.completed)
            |> List/map(item, new: todo_row(todo: item))
        )
    }
))
"#,
            None,
            false,
        );

        let RuntimeModel::State(model) = &program.runtime else {
            panic!(
                "expected state runtime model for filtered object list test, got {:?} with root {:?}",
                program.runtime, program.root
            );
        };
        assert_eq!(model.object_lists.get("store.todos").map(Vec::len), Some(2));

        let SemanticNode::Element { children, .. } = &program.root else {
            panic!("expected filtered object list root stripe");
        };
        assert_eq!(children.len(), 3);
        let SemanticNode::Element {
            children: completed_children,
            ..
        } = &children[0]
        else {
            panic!("expected completed count label");
        };
        assert!(matches!(
            completed_children.as_slice(),
            [SemanticNode::TextTemplate { value, .. }] | [SemanticNode::Text(value)]
                if value == "Completed: 0"
        ));
        let SemanticNode::Element {
            children: active_children,
            ..
        } = &children[1]
        else {
            panic!("expected active count label");
        };
        assert!(matches!(
            active_children.as_slice(),
            [SemanticNode::TextTemplate { value, .. }] | [SemanticNode::Text(value)]
                if value == "Active: 2"
        ));
        let SemanticNode::Element {
            children: list_children,
            ..
        } = &children[2]
        else {
            panic!("expected filtered todos stripe");
        };
        assert!(matches!(
            &list_children[0],
            SemanticNode::ObjectList { binding, filter: Some(_), .. } if binding == "store.todos"
        ));
    }

    #[test]
    fn lower_to_semantic_detects_object_list_bulk_toggle_and_remove_completed() {
        let program = lower_to_semantic(
            r#"
store: [
    elements: [toggle_all: LINK, remove_completed: LINK]

    todos:
        LIST {
            new_todo(title: TEXT { Buy milk })
            new_todo(title: TEXT { Clean room })
        }
        |> List/remove(item, on: elements.remove_completed.event.press |> THEN {
            item.completed |> WHEN {
                True => []
                False => SKIP
            }
        })
]

FUNCTION new_todo(title) {
    [
        todo_elements: [todo_checkbox: LINK]
        title: title

        completed: False |> HOLD state {
            LATEST {
                todo_elements.todo_checkbox.event.click |> THEN { state |> Bool/not() }
                store.elements.toggle_all.event.click |> THEN { state |> Bool/not() }
            }
        }
    ]
}

document: Document/new(root: Element/stripe(
    element: []
    direction: Column
    gap: 10
    style: []

    items: LIST {
        Element/button(element: [event: [click: LINK]], style: [], label: TEXT { Toggle all })
        |> LINK { store.elements.toggle_all }

        Element/button(element: [event: [press: LINK]], style: [], label: TEXT { Remove completed })
        |> LINK { store.elements.remove_completed }

        Element/stripe(
            element: []
            direction: Column
            gap: 5
            style: []

            items: store.todos |> List/map(item, new: Element/stripe(
                element: []
                direction: Row
                gap: 10
                style: []

                items: LIST {
                    Element/checkbox(
                        element: [event: [click: LINK]]
                        style: []
                        label: TEXT { Toggle }
                        checked: item.completed

                        icon: item.completed |> WHEN {
                            True => TEXT { [X] }
                            False => TEXT { [ ] }
                        }
                    )
                    |> LINK { item.todo_elements.todo_checkbox }

                    Element/label(element: [], style: [], label: item.title)
                }
            ))
        )
    }
))
"#,
            None,
            false,
        );

        let RuntimeModel::State(model) = &program.runtime else {
            panic!(
                "expected state runtime model for object bulk test, got {:?} with root {:?}",
                program.runtime, program.root
            );
        };
        assert_eq!(model.object_lists.get("store.todos").map(Vec::len), Some(2));
        let toggle_updates = model; // keep the branch explicit for diagnostics
        let _ = toggle_updates;

        let SemanticNode::Element { children, .. } = &program.root else {
            panic!("expected object bulk root stripe");
        };
        let SemanticNode::Element {
            children: list_children,
            ..
        } = &children[2]
        else {
            panic!("expected todos stripe");
        };
        assert!(matches!(
            &list_children[0],
            SemanticNode::ObjectList { binding, item_actions, .. }
                if binding == "store.todos"
                    && item_actions.iter().any(|action| action.source_binding_suffix == "todo_elements.todo_checkbox")
        ));
    }

    #[test]
    fn lower_to_semantic_detects_router_selected_filter_object_list_view() {
        let program = lower_to_semantic(
            r#"
store: [
    elements: [
        filter_buttons: [all: LINK, active: LINK, completed: LINK]
        remove_completed_button: LINK
        toggle_all_checkbox: LINK
    ]

    navigation_result:
        LATEST {
            elements.filter_buttons.all.event.press |> THEN { TEXT { / } }
            elements.filter_buttons.active.event.press |> THEN { TEXT { /active } }
            elements.filter_buttons.completed.event.press |> THEN { TEXT { /completed } }
        }
        |> Router/go_to()

    selected_filter: Router/route() |> WHILE {
        TEXT { / } => All
        TEXT { /active } => Active
        TEXT { /completed } => Completed
        __ => All
    }

    todos:
        LIST {
            new_todo(title: TEXT { Buy groceries })
            new_todo(title: TEXT { Clean room })
        }
        |> List/remove(item, on: item.todo_elements.remove_todo_button.event.press)
        |> List/remove(item, on: elements.remove_completed_button.event.press |> THEN {
            item.completed |> WHEN {
                True => []
                False => SKIP
            }
        })
]

FUNCTION new_todo(title) {
    [
        todo_elements: [remove_todo_button: LINK, todo_checkbox: LINK]
        title: title

        completed: False |> HOLD state {
            LATEST {
                todo_elements.todo_checkbox.event.click |> THEN { state |> Bool/not() }
                store.elements.toggle_all_checkbox.event.click |> THEN { state |> Bool/not() }
            }
        }
    ]
}

FUNCTION todo_item(todo) {
    Element/stripe(
        element: []
        direction: Row
        gap: 10
        style: []

        items: LIST {
            Element/checkbox(
                element: [event: [click: LINK]]
                style: []
                label: TEXT { Toggle }
                checked: todo.completed

                icon: todo.completed |> WHEN {
                    True => TEXT { [X] }
                    False => TEXT { [ ] }
                }
            )
            |> LINK { todo.todo_elements.todo_checkbox }

            Element/label(element: [], style: [], label: todo.title)

            Element/button(
                element: [event: [press: LINK]]
                style: []
                label: TEXT { Remove }
            )
            |> LINK { todo.todo_elements.remove_todo_button }
        }
    )
}

document: Document/new(root: Element/stripe(
    element: []
    direction: Column
    gap: 10
    style: []

    items: LIST {
        Element/button(element: [event: [press: LINK]], style: [], label: TEXT { All })
        |> LINK { store.elements.filter_buttons.all }

        Element/button(element: [event: [press: LINK]], style: [], label: TEXT { Active })
        |> LINK { store.elements.filter_buttons.active }

        Element/button(element: [event: [press: LINK]], style: [], label: TEXT { Completed })
        |> LINK { store.elements.filter_buttons.completed }

        Element/button(element: [event: [click: LINK]], style: [], label: TEXT { Toggle all })
        |> LINK { store.elements.toggle_all_checkbox }

        Element/button(element: [event: [press: LINK]], style: [], label: TEXT { Remove completed })
        |> LINK { store.elements.remove_completed_button }

        Element/label(element: [], style: [], label: BLOCK {
            count: store.todos |> List/retain(item, if: store.selected_filter |> WHILE {
                All => True
                Active => item.completed |> Bool/not()
                Completed => item.completed
            }) |> List/count()

            TEXT { Visible: {count} }
        })

        Element/stripe(
            element: []
            direction: Column
            gap: 5
            style: []

            items:
                store.todos
                |> List/retain(item, if: store.selected_filter |> WHILE {
                    All => True
                    Active => item.completed |> Bool/not()
                    Completed => item.completed
                })
                |> List/map(item, new: todo_item(todo: item))
        )
    }
))
"#,
            None,
            false,
        );

        let RuntimeModel::State(model) = &program.runtime else {
            panic!(
                "expected state runtime model for router-selected filter test, got {:?} with root {:?}",
                program.runtime, program.root
            );
        };
        assert_eq!(model.object_lists.get("store.todos").map(Vec::len), Some(2));
        assert_eq!(model.scalar_values.get("store.selected_filter"), Some(&0));

        let SemanticNode::Element { children, .. } = &program.root else {
            panic!("expected router-selected filter root stripe");
        };
        let SemanticNode::Element {
            children: list_children,
            ..
        } = &children[6]
        else {
            panic!("expected filtered todos stripe");
        };
        assert!(matches!(
            &list_children[0],
            SemanticNode::ObjectList {
                binding,
                filter: Some(ObjectListFilter::SelectedCompletedByScalar { binding: filter_binding }),
                item_actions,
                ..
            }
                if binding == "store.todos"
                    && filter_binding == "store.selected_filter"
                    && item_actions.iter().any(|action| action.source_binding_suffix == "todo_elements.todo_checkbox")
                    && item_actions.iter().any(|action| action.source_binding_suffix == "todo_elements.remove_todo_button")
        ));
    }

    #[test]
    fn lower_to_semantic_detects_list_is_empty_ui_branch() {
        let program = lower_to_semantic(
            r#"
store: [
    elements: [toggle_all: LINK, remove_completed: LINK]

    todos:
        LIST {
            new_todo(title: TEXT { Buy milk })
            new_todo(title: TEXT { Clean room })
        }
        |> List/remove(item, on: elements.remove_completed.event.press |> THEN {
            item.completed |> WHEN {
                True => []
                False => SKIP
            }
        })
]

FUNCTION new_todo(title) {
    [
        todo_elements: [todo_checkbox: LINK]
        title: title

        completed: False |> HOLD state {
            LATEST {
                todo_elements.todo_checkbox.event.click |> THEN { state |> Bool/not() }
                store.elements.toggle_all.event.click |> THEN { state |> Bool/not() }
            }
        }
    ]
}

document: Document/new(root: Element/stripe(
    element: []
    direction: Column
    gap: 10
    style: []

    items: LIST {
        Element/button(element: [event: [click: LINK]], style: [], label: TEXT { Toggle all })
        |> LINK { store.elements.toggle_all }

        Element/button(element: [event: [press: LINK]], style: [], label: TEXT { Remove completed })
        |> LINK { store.elements.remove_completed }

        store.todos |> List/is_empty() |> WHILE {
            True => NoElement
            False => Element/label(element: [], style: [], label: TEXT { Has todos })
        }
    }
))
"#,
            None,
            false,
        );

        let RuntimeModel::State(model) = &program.runtime else {
            panic!(
                "expected state runtime model for list-is-empty branch test, got {:?} with root {:?}",
                program.runtime, program.root
            );
        };
        assert_eq!(model.object_lists.get("store.todos").map(Vec::len), Some(2));

        let SemanticNode::Element { children, .. } = &program.root else {
            panic!("expected list-is-empty branch root stripe");
        };
        assert!(matches!(
            &children[2],
            SemanticNode::ListEmptyBranch {
                binding,
                object_items: true,
                invert: false,
                ..
            } if binding == "store.todos"
        ));
    }

    #[test]
    fn lower_to_semantic_detects_object_item_editing_branch() {
        let program = lower_to_semantic(
            r#"
store: [
    todos: LIST {
        new_todo(title: TEXT { Buy milk })
    }
]

FUNCTION new_todo(title) {
    [
        todo_elements: [editing_input: LINK, todo_title: LINK]
        title: title

        editing: False |> HOLD state {
            LATEST {
                todo_elements.todo_title.event.double_click |> THEN { True }
                todo_elements.editing_input.event.blur |> THEN { False }
                todo_elements.editing_input.event.key_down.key |> WHEN {
                    Enter => False
                    Escape => False
                    __ => SKIP
                }
            }
        }
    ]
}

document: Document/new(root: Element/stripe(
    element: []
    direction: Column
    gap: 10
    style: []

    items: store.todos |> List/map(item, new: item.editing |> WHILE {
        True =>
            Element/label(element: [event: [blur: LINK, key_down: LINK]], style: [], label: TEXT { Editing })
            |> LINK { item.todo_elements.editing_input }

        False =>
            Element/label(element: [event: [double_click: LINK]], style: [], label: item.title)
            |> LINK { item.todo_elements.todo_title }
    })
))
"#,
            None,
            false,
        );

        let RuntimeModel::State(model) = &program.runtime else {
            panic!(
                "expected state runtime model for editing branch test, got {:?} with root {:?}",
                program.runtime, program.root
            );
        };
        assert_eq!(model.object_lists.get("store.todos").map(Vec::len), Some(1));
        assert!(matches!(
            model.object_lists.get("store.todos").and_then(|items| items.first()),
            Some(item) if item.bool_fields.get("editing") == Some(&false)
        ));

        let SemanticNode::Element { children, .. } = &program.root else {
            panic!("expected editing branch root stripe");
        };
        assert!(matches!(
            &children[0],
            SemanticNode::ObjectList { template, item_actions, .. }
                if matches!(
                    template.as_ref(),
                    SemanticNode::ObjectBoolFieldBranch { field, .. } if field == "editing"
                )
                && item_actions.iter().any(|action| matches!(
                    &action.action,
                    ObjectItemActionKind::SetBoolField { field, value: true, .. }
                        if field == "editing"
                ))
                && item_actions.iter().any(|action| matches!(
                    &action.action,
                    ObjectItemActionKind::SetBoolField {
                        field,
                        value: false,
                        payload_filter: Some(filter),
                    } if field == "editing" && (filter == "Enter" || filter == "Escape")
                ))
        ));
    }

    #[test]
    fn lower_to_semantic_detects_object_item_title_update_action() {
        let program = lower_to_semantic(
            r#"
store: [
    todos: LIST {
        new_todo(title: TEXT { Buy milk })
    }
]

FUNCTION new_todo(title) {
    [
        todo_elements: [editing_input: LINK, todo_title: LINK]

        title: LATEST {
            title

            todo_elements.editing_input.event.change.text |> WHEN {
                changed_text =>
                    changed_text
                    |> Text/is_not_empty()
                    |> WHEN {
                        True => changed_text
                        False => SKIP
                    }
            }
        }

        editing: False |> HOLD state {
            LATEST {
                todo_elements.todo_title.event.double_click |> THEN { True }
                todo_elements.editing_input.event.key_down.key |> WHEN {
                    Enter => False
                    Escape => False
                    __ => SKIP
                }
            }
        }
    ]
}

document: Document/new(root: Element/stripe(
    element: []
    direction: Column
    gap: 10
    style: []

    items: store.todos |> List/map(item, new: item.editing |> WHILE {
        True =>
            Element/text_input(
                element: [event: [change: LINK, key_down: LINK]]
                style: []
                label: Hidden[text: TEXT { Editing }]
                text: item.title
                placeholder: []
                focus: True
            )
            |> LINK { item.todo_elements.editing_input }

        False =>
            Element/label(
                element: [event: [double_click: LINK]]
                style: []
                label: item.title
            )
            |> LINK { item.todo_elements.todo_title }
    })
))
"#,
            None,
            false,
        );

        let RuntimeModel::State(model) = &program.runtime else {
            panic!("expected state runtime model for title update test");
        };
        assert_eq!(model.object_lists.get("store.todos").map(Vec::len), Some(1));

        let SemanticNode::Element { children, .. } = &program.root else {
            panic!("expected object list root");
        };
        assert!(matches!(
            &children[0],
            SemanticNode::ObjectList { item_actions, .. }
                if item_actions.iter().any(|action| matches!(
                    &action.action,
                    ObjectItemActionKind::SetTitle {
                        trim: false,
                        reject_empty: true,
                        payload_filter: None,
                    }
                ))
        ));
    }

    #[test]
    fn lower_to_semantic_collects_focused_fact_binding() {
        let program = lower_to_semantic(
            r#"
document: Document/new(root: Element/button(
    element: [focused: LINK]
    style: []
    label: element.focused |> WHILE {
        True => TEXT { Focused }
        False => TEXT { Idle }
    }
))
"#,
            None,
            false,
        );

        let SemanticNode::Element { fact_bindings, .. } = &program.root else {
            panic!("expected button root");
        };
        assert!(fact_bindings.iter().any(|binding| {
            binding.kind
                == crate::platform::browser::engine_wasm_pro::semantic_ir::SemanticFactKind::Focused
                && binding.binding == "__element__.focused"
        }));
    }

    #[test]
    fn lower_to_semantic_todo_mvc_real_file_smoke() {
        let program = lower_to_semantic(
            include_str!("../../../../../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn"),
            None,
            false,
        );

        let RuntimeModel::State(model) = &program.runtime else {
            panic!(
                "expected state runtime model for todo_mvc, got {:?} with root {:?}",
                program.runtime, program.root
            );
        };
        assert_eq!(model.object_lists.get("store.todos").map(Vec::len), Some(2));
    }

    #[test]
    fn lower_to_semantic_cells_real_file_smoke() {
        let program = super::parse_and_lower(include_str!(
            "../../../../../../playground/frontend/src/examples/cells/cells.bn"
        ))
        .expect("cells should lower");

        let RuntimeModel::State(model) = &program.runtime else {
            panic!(
                "expected state runtime model for cells, got {:?} with root {:?}",
                program.runtime, program.root
            );
        };
        assert!(
            model.object_lists.contains_key("all_row_cells"),
            "expected cells lowering to materialize all_row_cells state, got scalar bindings {:?}, text lists {:?}, object lists {:?}",
            model.scalar_values.keys().collect::<Vec<_>>(),
            model.text_lists.keys().collect::<Vec<_>>(),
            model.object_lists.keys().collect::<Vec<_>>()
        );
        assert_eq!(model.object_lists.get("all_row_cells").map(Vec::len), Some(100));
        let first_row = model
            .object_lists
            .get("all_row_cells")
            .and_then(|rows| rows.first())
            .expect("cells should materialize first row");
        assert_eq!(first_row.scalar_fields.get("row"), Some(&1));
        assert_eq!(first_row.object_lists.get("cells").map(Vec::len), Some(26));

        assert!(
            semantic_tree_contains_object_list_binding(&program.root, "all_row_cells"),
            "cells root should retain runtime object-list node for all_row_cells"
        );
        assert!(
            semantic_tree_contains_object_list_binding(&program.root, "__item__.cells"),
            "cells root should retain nested runtime object-list node for __item__.cells"
        );
        assert!(
            object_list_has_item_action(
                &program.root,
                "__item__.cells",
                "cell_elements.display",
                &boon_scene::UiEventKind::DoubleClick,
            ),
            "cells nested list should expose display DoubleClick item action"
        );
        assert!(
            object_list_has_item_action(
                &program.root,
                "__item__.cells",
                "cell_elements.editing",
                &boon_scene::UiEventKind::Input,
            ),
            "cells nested list should expose editing Input item action; nested actions: {:?}",
            object_list_action_summaries(&program.root, "__item__.cells")
        );
        assert!(
            object_list_has_item_action(
                &program.root,
                "__item__.cells",
                "cell_elements.editing",
                &boon_scene::UiEventKind::KeyDown,
            ),
            "cells nested list should expose editing KeyDown item action"
        );
        assert!(
            semantic_tree_contains_object_text_field_branch(&program.root, "formula_text"),
            "cells root should retain a runtime object-text branch for formula_text"
        );
        assert!(
            semantic_tree_contains_object_text_binding(&program.root, "display_value"),
            "cells root should retain a runtime object-field text binding for display_value"
        );
    }

    #[test]
    fn cells_formula_helper_evaluates_a1_initial_text() {
        let context = cells_test_context("probe: cell_formula(column: 1, row: 1)\n");
        let value = lower_cells_probe_text(&context);
        assert_eq!(value, "5");
    }

    #[test]
    fn cells_default_formula_helper_evaluates_a1_initial_text() {
        let context = cells_test_context("probe: default_formula(column: 1, row: 1)\n");
        let value = lower_cells_probe_text(&context);
        assert_eq!(value, "5");
    }

    #[test]
    fn cells_find_override_formula_base_case_resolves_false_object() {
        let context =
            cells_test_context("probe: find_override_formula(index: 0, column: 1, row: 1).found\n");
        let value = lower_cells_probe_text(&context);
        assert_eq!(value, "0");
    }

    #[test]
    fn cells_default_formula_helper_evaluates_with_local_alias_args() {
        let context = cells_test_context(
            "probe: BLOCK { column: 1 row: 1 default_formula(column: column, row: row) }\n",
        );
        let value = lower_cells_probe_text(&context);
        assert_eq!(value, "5");
    }

    #[test]
    fn cells_default_formula_invocation_marker_resolves_local_alias_args() {
        let context = cells_test_context(
            "probe: BLOCK { column: 1 row: 1 default_formula(column: column, row: row) }\n",
        );
        let probe = context
            .bindings
            .get("probe")
            .copied()
            .expect("probe binding should exist");
        let StaticExpression::Block { variables, output } = &probe.node else {
            panic!("probe should be a block");
        };
        let StaticExpression::FunctionCall { path, arguments } = &output.node else {
            panic!("probe output should be a function call");
        };
        assert_eq!(path[0].as_str(), "default_formula");
        let locals = vec![variables
            .iter()
            .map(|variable| {
                (
                    variable.node.name.as_str().to_string(),
                    LocalBinding {
                        expr: Some(&variable.node.value),
                        object_base: None,
                    },
                )
            })
            .collect()];
        let marker = invocation_marker(
            "default_formula",
            arguments,
            None,
            &context,
            &locals,
            &Vec::new(),
        )
        .expect("invocation marker should build");
        assert_eq!(marker, "default_formula(column=1,row=1)");
    }

    #[test]
    fn cells_default_formula_with_invoked_scope_lowers_body_text() {
        let context = cells_test_context(
            "probe: BLOCK { column: 1 row: 1 default_formula(column: column, row: row) }\n",
        );
        let probe = context
            .bindings
            .get("probe")
            .copied()
            .expect("probe binding should exist");
        let StaticExpression::Block { variables, output } = &probe.node else {
            panic!("probe should be a block");
        };
        let mut locals = vec![variables
            .iter()
            .map(|variable| {
                (
                    variable.node.name.as_str().to_string(),
                    LocalBinding {
                        expr: Some(&variable.node.value),
                        object_base: None,
                    },
                )
            })
            .collect()];
        let StaticExpression::FunctionCall { path, arguments } = &output.node else {
            panic!("probe output should be a function call");
        };
        let value = with_invoked_function_scope(
            path[0].as_str(),
            arguments,
            None,
            &context,
            &mut Vec::new(),
            &mut locals,
            &mut Vec::new(),
            |body, context, stack, locals, passed| {
                lower_text_value(body, context, stack, locals, passed)
            },
        )
        .expect("default_formula invocation should lower");
        assert_eq!(value, "5");
    }

    #[test]
    fn cells_cell_formula_branch_selects_default_formula_when_no_override() {
        let context = cells_test_context(
            "probe: BLOCK {\n\
                override_result: find_override_formula(index: 0, column: 1, row: 1)\n\
                override_result.found |> WHEN {\n\
                    True => override_result.text\n\
                    __ => default_formula(column: 1, row: 1)\n\
                }\n\
            }\n",
        );
        let value = lower_cells_probe_text(&context);
        assert_eq!(value, "5");
    }

    fn cells_test_context(extra_source: &str) -> LowerContext<'static> {
        let source = format!(
            "{}\n{extra_source}",
            include_str!("../../../../../../playground/frontend/src/examples/cells/cells.bn")
        );
        let leaked = Box::leak(source.into_boxed_str());
        let expressions: &'static [StaticSpannedExpression] = Box::leak(
            parse_static_expressions(leaked)
                .expect("cells source should parse")
                .into_boxed_slice(),
        );
        let bindings = top_level_bindings(expressions);
        let functions = top_level_functions(expressions);
        let path_bindings = flatten_binding_paths(&bindings);
        let mut scalar_plan = detect_scalar_plan(&path_bindings, &functions)
            .expect("scalar plan should build");
        let static_object_lists = detect_static_object_list_plan(
            &path_bindings,
            &functions,
            &mut scalar_plan,
        )
        .expect("static object list plan should build");
        let text_plan = detect_text_plan(&path_bindings).expect("text plan should build");
        let object_list_plan =
            detect_object_list_plan(&path_bindings, &functions).expect("object list plan should build");
        let mut context = LowerContext {
            text_plan,
            list_plan: detect_list_plan(&path_bindings).expect("list plan should build"),
            object_list_plan,
            scalar_plan,
            static_object_lists,
            bindings,
            path_bindings,
            functions,
            ..LowerContext::default()
        };
        augment_top_level_object_field_runtime(&mut context)
            .expect("top-level object runtime should augment");
        augment_top_level_bool_item_runtime(&mut context)
            .expect("top-level bool runtime should augment");
        context
    }

    fn lower_cells_probe_text(context: &LowerContext<'_>) -> String {
        let probe = context
            .bindings
            .get("probe")
            .copied()
            .expect("probe binding should exist");
        lower_text_value(
            probe,
            context,
            &mut Vec::new(),
            &mut Vec::new(),
            &mut Vec::new(),
        )
        .expect("probe should lower to text")
    }

    #[test]
    fn detect_scalar_plan_handles_todo_mvc_real_file_counts() {
        let expressions = parse_static_expressions(include_str!(
            "../../../../../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn"
        ))
        .expect("todo_mvc should parse");
        let bindings = top_level_bindings(&expressions);
        let functions = super::top_level_functions(&expressions);
        let path_bindings = super::flatten_binding_paths(&bindings);

        let plan = detect_scalar_plan(&path_bindings, &functions)
            .expect("todo_mvc scalar plan should build");

        assert_eq!(plan.initial_values.get("store.todos_count"), Some(&2));
        assert_eq!(
            plan.initial_values.get("store.completed_todos_count"),
            Some(&0)
        );
        assert_eq!(
            plan.initial_values.get("store.active_todos_count"),
            Some(&2)
        );
        assert_eq!(plan.initial_values.get("store.all_completed"), Some(&0));
        assert_eq!(plan.initial_values.get("store.new_todo_focused"), Some(&1));
    }

    fn semantic_tree_contains_object_list_binding(node: &SemanticNode, expected: &str) -> bool {
        match node {
            SemanticNode::ObjectList {
                binding, template, ..
            } => {
                binding == expected || semantic_tree_contains_object_list_binding(template, expected)
            }
            SemanticNode::Fragment(children) | SemanticNode::Element { children, .. } => children
                .iter()
                .any(|child| semantic_tree_contains_object_list_binding(child, expected)),
            SemanticNode::BoolBranch { truthy, falsy, .. }
            | SemanticNode::ScalarCompareBranch { truthy, falsy, .. }
            | SemanticNode::ObjectBoolFieldBranch { truthy, falsy, .. }
            | SemanticNode::ListEmptyBranch { truthy, falsy, .. } => {
                semantic_tree_contains_object_list_binding(truthy, expected)
                    || semantic_tree_contains_object_list_binding(falsy, expected)
            }
            _ => false,
        }
    }

    fn semantic_tree_contains_object_text_field_branch(
        node: &SemanticNode,
        expected_field: &str,
    ) -> bool {
        match node {
            SemanticNode::ObjectTextFieldBranch {
                field,
                truthy,
                falsy,
                ..
            } => {
                field == expected_field
                    || semantic_tree_contains_object_text_field_branch(truthy, expected_field)
                    || semantic_tree_contains_object_text_field_branch(falsy, expected_field)
            }
            SemanticNode::TextTemplate { parts, .. } => parts.iter().any(|part| {
                matches!(
                    part,
                    SemanticTextPart::ObjectFieldBinding(field) if field == expected_field
                )
            }),
            SemanticNode::Element { children, .. } | SemanticNode::Fragment(children) => children
                .iter()
                .any(|child| semantic_tree_contains_object_text_field_branch(child, expected_field)),
            SemanticNode::BoolBranch { truthy, falsy, .. }
            | SemanticNode::ScalarCompareBranch { truthy, falsy, .. }
            | SemanticNode::ObjectScalarCompareBranch { truthy, falsy, .. }
            | SemanticNode::ObjectBoolFieldBranch { truthy, falsy, .. }
            | SemanticNode::TextBindingBranch { truthy, falsy, .. }
            | SemanticNode::ListEmptyBranch { truthy, falsy, .. } => {
                semantic_tree_contains_object_text_field_branch(truthy, expected_field)
                    || semantic_tree_contains_object_text_field_branch(falsy, expected_field)
            }
            SemanticNode::ObjectList { template, .. } => {
                semantic_tree_contains_object_text_field_branch(template, expected_field)
            }
            _ => false,
        }
    }

    fn semantic_tree_contains_object_text_binding(
        node: &SemanticNode,
        expected_field: &str,
    ) -> bool {
        match node {
            SemanticNode::TextTemplate { parts, .. } => parts.iter().any(|part| {
                matches!(
                    part,
                    SemanticTextPart::ObjectFieldBinding(field) if field == expected_field
                )
            }),
            SemanticNode::Element { children, input_value, .. } => {
                children
                    .iter()
                    .any(|child| semantic_tree_contains_object_text_binding(child, expected_field))
                    || input_value.as_ref().is_some_and(|value| {
                        input_value_contains_object_text_binding(value, expected_field)
                    })
            }
            SemanticNode::Fragment(children) => children
                .iter()
                .any(|child| semantic_tree_contains_object_text_binding(child, expected_field)),
            SemanticNode::BoolBranch { truthy, falsy, .. }
            | SemanticNode::ScalarCompareBranch { truthy, falsy, .. }
            | SemanticNode::ObjectScalarCompareBranch { truthy, falsy, .. }
            | SemanticNode::ObjectBoolFieldBranch { truthy, falsy, .. }
            | SemanticNode::ObjectTextFieldBranch { truthy, falsy, .. }
            | SemanticNode::TextBindingBranch { truthy, falsy, .. }
            | SemanticNode::ListEmptyBranch { truthy, falsy, .. } => {
                semantic_tree_contains_object_text_binding(truthy, expected_field)
                    || semantic_tree_contains_object_text_binding(falsy, expected_field)
            }
            SemanticNode::ObjectList { template, .. } => {
                semantic_tree_contains_object_text_binding(template, expected_field)
            }
            _ => false,
        }
    }

    fn input_value_contains_object_text_binding(
        value: &SemanticInputValue,
        expected_field: &str,
    ) -> bool {
        match value {
            SemanticInputValue::Static(_) => false,
            SemanticInputValue::TextParts { parts, .. } => parts.iter().any(|part| {
                matches!(
                    part,
                    SemanticTextPart::ObjectFieldBinding(field) if field == expected_field
                )
            }),
            SemanticInputValue::TextBindingBranch { truthy, falsy, .. }
            | SemanticInputValue::ObjectTextFieldBranch { truthy, falsy, .. } => {
                input_value_contains_object_text_binding(truthy, expected_field)
                    || input_value_contains_object_text_binding(falsy, expected_field)
            }
        }
    }

    fn object_list_action_summaries(
        node: &SemanticNode,
        expected_binding: &str,
    ) -> Vec<(String, boon_scene::UiEventKind)> {
        match node {
            SemanticNode::ObjectList {
                binding,
                item_actions,
                template,
                ..
            } => {
                let mut output = Vec::new();
                if binding == expected_binding {
                    output.extend(
                        item_actions
                            .iter()
                            .map(|action| {
                                (action.source_binding_suffix.clone(), action.kind.clone())
                            }),
                    );
                }
                output.extend(object_list_action_summaries(template, expected_binding));
                output
            }
            SemanticNode::Element { children, .. } | SemanticNode::Fragment(children) => children
                .iter()
                .flat_map(|child| object_list_action_summaries(child, expected_binding))
                .collect(),
            SemanticNode::BoolBranch { truthy, falsy, .. }
            | SemanticNode::ScalarCompareBranch { truthy, falsy, .. }
            | SemanticNode::ObjectScalarCompareBranch { truthy, falsy, .. }
            | SemanticNode::ObjectBoolFieldBranch { truthy, falsy, .. }
            | SemanticNode::ObjectTextFieldBranch { truthy, falsy, .. }
            | SemanticNode::TextBindingBranch { truthy, falsy, .. }
            | SemanticNode::ListEmptyBranch { truthy, falsy, .. } => {
                let mut output = object_list_action_summaries(truthy, expected_binding);
                output.extend(object_list_action_summaries(falsy, expected_binding));
                output
            }
            _ => Vec::new(),
        }
    }

    fn object_list_has_item_action(
        node: &SemanticNode,
        expected_binding: &str,
        expected_suffix: &str,
        expected_kind: &boon_scene::UiEventKind,
    ) -> bool {
        match node {
            SemanticNode::ObjectList {
                binding,
                item_actions,
                template,
                ..
            } => {
                if binding == expected_binding
                    && item_actions.iter().any(|action| {
                        action.source_binding_suffix == expected_suffix
                            && &action.kind == expected_kind
                    })
                {
                    return true;
                }
                object_list_has_item_action(
                    template,
                    expected_binding,
                    expected_suffix,
                    expected_kind,
                )
            }
            SemanticNode::Element { children, .. } | SemanticNode::Fragment(children) => children
                .iter()
                .any(|child| {
                    object_list_has_item_action(
                        child,
                        expected_binding,
                        expected_suffix,
                        expected_kind,
                    )
                }),
            SemanticNode::BoolBranch { truthy, falsy, .. }
            | SemanticNode::ScalarCompareBranch { truthy, falsy, .. }
            | SemanticNode::ObjectScalarCompareBranch { truthy, falsy, .. }
            | SemanticNode::ObjectBoolFieldBranch { truthy, falsy, .. }
            | SemanticNode::ObjectTextFieldBranch { truthy, falsy, .. }
            | SemanticNode::TextBindingBranch { truthy, falsy, .. }
            | SemanticNode::ListEmptyBranch { truthy, falsy, .. } => {
                object_list_has_item_action(truthy, expected_binding, expected_suffix, expected_kind)
                    || object_list_has_item_action(
                        falsy,
                        expected_binding,
                        expected_suffix,
                        expected_kind,
                    )
            }
            _ => false,
        }
    }
}
