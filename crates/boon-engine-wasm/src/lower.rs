use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, btree_map::Entry};

use boon::parser::{
    Input as _, Parser as _, SourceCode, Token, lexer, parser, reset_expression_depth,
    resolve_references, span_at, static_expression,
};

use super::ExternalFunction;
use super::semantic_ir::{
    DerivedArithmeticOp, DerivedScalarOperand, DerivedScalarSpec, DerivedTextOperand, IntCompareOp,
    ItemScalarUpdate, ItemTextUpdate, NestedObjectListAction, ObjectDerivedScalarOperand,
    ObjectItemActionKind, ObjectItemActionSpec, ObjectListFilter, ObjectListItem, ObjectListUpdate,
    RuntimeModel, ScalarRuntimeModel, ScalarUpdate, SemanticAction, SemanticEventBinding,
    SemanticFactBinding, SemanticFactKind, SemanticInputValue, SemanticNode, SemanticProgram,
    SemanticStyleFragment, SemanticTextPart, StateRuntimeModel, TextListFilter, TextListTemplate,
    TextListUpdate, TextUpdate,
};

type StaticExpression = static_expression::Expression;
type StaticSpannedExpression = static_expression::Spanned<StaticExpression>;
type StaticArgument = static_expression::Argument;
type StaticObject = static_expression::Object;
type StaticTextPart = static_expression::TextPart;

const SYNTHETIC_DOCUMENT_ROOT_BINDING: &str = "__document_root__";
const SYNTHETIC_DOCUMENT_ROOT_TIMER_BINDING: &str = "__document_root_timer__";

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
    update: CounterEventUpdate,
}

#[derive(Debug, Clone)]
enum CounterEventUpdate {
    Add(i64),
    AddTenths(i64),
    Set(i64),
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
struct HoldPayloadScalarSpec {
    initial_value: i64,
    trigger_binding: String,
    event_name: String,
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

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct TriggerSpec {
    trigger_binding: String,
    event_name: String,
    payload_filter: Option<String>,
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
    scalar_event_updates: BTreeMap<(String, String), Vec<ScalarUpdate>>,
    text_event_updates: BTreeMap<(String, String), Vec<TextUpdate>>,
    item_actions: BTreeMap<String, Vec<ObjectItemActionSpec>>,
}

#[derive(Debug, Clone)]
struct LinkedTextInputPlan {
    binding: String,
    initial_value: String,
    event_updates: Vec<((String, String), TextUpdate)>,
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
    timer_bindings: BTreeMap<String, u32>,
    scalar_mirrors: BTreeMap<String, Vec<String>>,
    text_mirrors: BTreeMap<String, Vec<String>>,
    scalar_eval_cache: RefCell<BTreeMap<String, Option<i64>>>,
    text_eval_cache: RefCell<BTreeMap<String, String>>,
    scalar_eval_in_progress: RefCell<BTreeSet<String>>,
    text_eval_in_progress: RefCell<BTreeSet<String>>,
}

type LocalScopes<'a> = Vec<BTreeMap<String, LocalBinding<'a>>>;
type PassedScopes = Vec<PassedScope>;

pub fn lower_to_semantic(
    source: &str,
    external_functions: Option<&[ExternalFunction]>,
    _persistence_enabled: bool,
) -> SemanticProgram {
    try_lower_to_semantic(source, external_functions, _persistence_enabled)
        .unwrap_or_else(|error| panic!("Wasm lowering failed: {error}"))
}

pub fn try_lower_to_semantic(
    source: &str,
    external_functions: Option<&[ExternalFunction]>,
    _persistence_enabled: bool,
) -> Result<SemanticProgram, String> {
    parse_and_lower(source, external_functions)
}

fn parse_and_lower(
    source: &str,
    external_functions: Option<&[ExternalFunction]>,
) -> Result<SemanticProgram, String> {
    let expressions = parse_static_expressions(source)?;
    let bindings = top_level_bindings(&expressions);
    let functions = top_level_functions(&expressions, external_functions);
    let bindings_for_document = bindings.clone();
    let document = find_document_expression(&expressions, &bindings_for_document)
        .map_err(|error| format!("find_document_expression: {error}"))?;
    let mut path_bindings = flatten_binding_paths(&bindings);
    if let Some(root_expression) = synthetic_document_root_expression(document) {
        path_bindings.insert(SYNTHETIC_DOCUMENT_ROOT_BINDING.to_string(), root_expression);
    }
    if let Some(timer_expression) = synthetic_document_root_timer_expression(document) {
        path_bindings.insert(
            SYNTHETIC_DOCUMENT_ROOT_TIMER_BINDING.to_string(),
            timer_expression,
        );
    }
    let timer_bindings = detect_timer_bindings(&path_bindings)
        .map_err(|error| format!("detect_timer_bindings: {error}"))?;
    let mut scalar_plan = detect_scalar_plan(&path_bindings, &functions, &timer_bindings)
        .map_err(|error| format!("detect_scalar_plan: {error}"))?;
    let static_object_lists =
        detect_static_object_list_plan(&path_bindings, &functions, &mut scalar_plan)
            .map_err(|error| format!("detect_static_object_list_plan: {error}"))?;
    let text_plan =
        detect_text_plan(&path_bindings).map_err(|error| format!("detect_text_plan: {error}"))?;
    let object_list_plan =
        detect_object_list_plan(&path_bindings, &functions, &scalar_plan, &text_plan)
            .map_err(|error| format!("detect_object_list_plan: {error}"))?;
    let mut context = LowerContext {
        text_plan,
        list_plan: detect_list_plan(&path_bindings)
            .map_err(|error| format!("detect_list_plan: {error}"))?,
        object_list_plan,
        scalar_plan,
        static_object_lists,
        timer_bindings,
        bindings,
        path_bindings,
        functions,
        ..LowerContext::default()
    };
    augment_top_level_object_field_runtime(&mut context)
        .map_err(|error| format!("augment_top_level_object_field_runtime: {error}"))?;
    augment_hold_alias_runtime(&mut context)
        .map_err(|error| format!("augment_hold_alias_runtime: {error}"))?;
    augment_document_link_forwarder_item_runtime(&mut context)
        .map_err(|error| format!("augment_document_link_forwarder_item_runtime: {error}"))?;
    augment_object_alias_field_runtime(&mut context)
        .map_err(|error| format!("augment_object_alias_field_runtime: {error}"))?;
    augment_linked_text_input_runtime(&mut context)
        .map_err(|error| format!("augment_linked_text_input_runtime: {error}"))?;
    augment_top_level_bool_item_runtime(&mut context)
        .map_err(|error| format!("augment_top_level_bool_item_runtime: {error}"))?;
    augment_dependent_object_list_updates_from_item_bindings(&mut context)
        .map_err(|error| {
            format!("augment_dependent_object_list_updates_from_item_bindings: {error}")
        })?;
    seed_missing_scalar_initial_values(&mut context)
        .map_err(|error| format!("seed_missing_scalar_initial_values: {error}"))?;

    let root = lower_document_root(
        document,
        &context,
        &mut Vec::new(),
        &mut Vec::new(),
        &mut Vec::new(),
        None,
    )
    .map_err(|error| format!("lower_document_root: {error}"))?;
    let root = wrap_with_timer_nodes(root, &context);

    Ok(SemanticProgram {
        root,
        runtime: runtime_model_for(&context),
    })
}

fn synthetic_document_root_expression<'a>(
    document: &'a StaticSpannedExpression,
) -> Option<&'a StaticSpannedExpression> {
    let StaticExpression::Pipe { from, to } = &document.node else {
        return None;
    };
    let StaticExpression::FunctionCall { path, arguments } = &to.node else {
        return None;
    };
    if !(path_matches(path, &["Document", "new"]) || path_matches(path, &["Scene", "new"]))
        || find_named_argument(arguments, "root").is_some()
    {
        return None;
    }
    (!matches!(
        &from.node,
        StaticExpression::Alias(static_expression::Alias::WithoutPassed { .. })
    ))
    .then_some(from.as_ref())
}

fn synthetic_document_root_timer_expression<'a>(
    document: &'a StaticSpannedExpression,
) -> Option<&'a StaticSpannedExpression> {
    let root = synthetic_document_root_expression(document)?;
    let StaticExpression::Pipe { from, to } = &root.node else {
        return None;
    };
    let StaticExpression::FunctionCall { path, arguments } = &to.node else {
        return None;
    };
    if !path_matches(path, &["Math", "sum"]) || !arguments.is_empty() {
        return None;
    }
    let StaticExpression::Pipe {
        from: trigger_source,
        to: trigger_then,
    } = &from.node
    else {
        return None;
    };
    if !matches!(&trigger_then.node, StaticExpression::Then { .. }) {
        return None;
    }
    matches!(timer_interval_millis_for_expression(trigger_source), Ok(Some(_)))
        .then_some(trigger_source.as_ref())
}

fn detect_timer_bindings(
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
) -> Result<BTreeMap<String, u32>, String> {
    let mut timers = BTreeMap::new();
    for binding in path_bindings.keys() {
        if let Some(interval_ms) = timer_interval_millis_for_binding(path_bindings, binding)? {
            timers.insert(binding.clone(), interval_ms);
        }
    }
    Ok(timers)
}

fn timer_interval_millis_for_binding(
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    binding: &str,
) -> Result<Option<u32>, String> {
    let Some(expression) = path_bindings.get(binding).copied() else {
        return Ok(None);
    };
    timer_interval_millis_for_expression(expression)
}

fn timer_interval_millis_for_expression(
    expression: &StaticSpannedExpression,
) -> Result<Option<u32>, String> {
    let StaticExpression::Pipe { from, to } = &expression.node else {
        return Ok(None);
    };
    let StaticExpression::FunctionCall { path, arguments } = &to.node else {
        return Ok(None);
    };
    if !path_matches(path, &["Timer", "interval"]) || !arguments.is_empty() {
        return Ok(None);
    }
    let StaticExpression::TaggedObject { tag, object } = &from.node else {
        return Ok(None);
    };
    if tag.as_str() != "Duration" {
        return Ok(None);
    }
    if let Some(seconds) = find_object_field(object, "seconds").and_then(extract_number) {
        return Ok(Some((seconds * 1000.0).round().max(0.0) as u32));
    }
    if let Some(milliseconds) = find_object_field(object, "milliseconds").and_then(extract_number) {
        return Ok(Some(milliseconds.round().max(0.0) as u32));
    }
    Ok(None)
}

fn wrap_with_timer_nodes(root: SemanticNode, context: &LowerContext<'_>) -> SemanticNode {
    if context.timer_bindings.is_empty() {
        return root;
    }

    let mut nodes = vec![root];
    for (binding, interval_ms) in &context.timer_bindings {
        let Some(action) = event_action_for(binding, "tick", context) else {
            continue;
        };
        nodes.push(SemanticNode::element(
            "div",
            None,
            vec![("style".to_string(), "display:none".to_string())],
            vec![SemanticEventBinding {
                kind: boon_scene::UiEventKind::Custom(format!("timer:{interval_ms}")),
                source_binding: None,
                action: Some(action),
            }],
            Vec::new(),
        ));
    }
    SemanticNode::Fragment(nodes)
}

fn seed_missing_scalar_initial_values(context: &mut LowerContext<'_>) -> Result<(), String> {
    let binding_names = context.path_bindings.keys().cloned().collect::<Vec<_>>();
    for binding_name in binding_names {
        if context
            .scalar_plan
            .initial_values
            .contains_key(&binding_name)
        {
            continue;
        }
        let Some(expression) = context.path_bindings.get(&binding_name).copied() else {
            continue;
        };
        let mut stack = Vec::new();
        let mut locals = Vec::new();
        let mut passed = Vec::new();
        if let Some(value) = initial_scalar_value_in_context(
            expression,
            context,
            &mut stack,
            &mut locals,
            &mut passed,
        )? {
            context
                .scalar_plan
                .initial_values
                .insert(binding_name, value);
        }
    }
    Ok(())
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
            |boon::parser::Spanned {
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
    external_functions: Option<&'a [ExternalFunction]>,
) -> BTreeMap<String, FunctionSpec<'a>> {
    let mut functions = expressions
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
        .collect::<BTreeMap<_, _>>();
    if let Some(external_functions) = external_functions {
        for (qualified_name, parameters, body, _module_name) in external_functions {
            functions.insert(
                qualified_name.clone(),
                FunctionSpec {
                    parameters: parameters.clone(),
                    body,
                },
            );
        }
    }
    functions
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
    if let Some(scene) = bindings.get("scene") {
        return Ok(scene);
    }
    if let [expression] = expressions {
        return Ok(expression);
    }
    Err(
        "expected top-level `document: Document/new(root: ...)` or `scene: Scene/new(root: ...)` binding"
            .to_string(),
    )
}

fn lower_document_root<'a>(
    expression: &'a StaticSpannedExpression,
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
    current_binding: Option<&str>,
) -> Result<SemanticNode, String> {
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
                lower_document_root(body, context, stack, locals, passed, current_binding)
            },
        );
    }
    let expression = resolve_alias(expression, context, locals, passed, stack)?;
    match &expression.node {
        StaticExpression::FunctionCall { path, arguments }
            if path_matches(path, &["Document", "new"])
                || path_matches(path, &["Scene", "new"]) =>
        {
            let root = find_named_argument(arguments, "root")
                .ok_or_else(|| "Document/new and Scene/new require a `root` argument".to_string())?;
            lower_ui_node(root, context, stack, locals, passed, current_binding)
        }
        StaticExpression::Pipe { from, to } => {
            let to = resolve_alias(to, context, locals, passed, stack)?;
            match &to.node {
                StaticExpression::FunctionCall { path, arguments }
                    if path_matches(path, &["Document", "new"])
                        || path_matches(path, &["Scene", "new"]) =>
                {
                    if find_named_argument(arguments, "root").is_some() {
                        return Err(
                            "pipe form Document/new(root: ...) and Scene/new(root: ...) is not supported yet"
                                .to_string(),
                        );
                    }
                    let inline_root_binding = if alias_binding_name(from)?.is_none()
                        && context
                            .path_bindings
                            .contains_key(SYNTHETIC_DOCUMENT_ROOT_BINDING)
                    {
                        Some(SYNTHETIC_DOCUMENT_ROOT_BINDING)
                    } else {
                        current_binding
                    };
                    lower_ui_node(from, context, stack, locals, passed, inline_root_binding)
                }
                _ => Err(
                    "document/scene must be produced by Document/new(...) or Scene/new(...)"
                        .to_string(),
                ),
            }
        }
        _ => Err(
            "document/scene must be produced by Document/new(...) or Scene/new(...)".to_string(),
        ),
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

    if let Some(branch) = lower_ui_branch_node(expression, context, stack, locals, passed)? {
        return Ok(branch);
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

    if let Some(binding_name) =
        current_binding.filter(|binding| scalar_binding_has_runtime_state(binding, context))
    {
        return Ok(SemanticNode::ScalarValue {
            binding: binding_name.to_string(),
            value: context
                .scalar_plan
                .initial_values
                .get(binding_name)
                .copied()
                .unwrap_or_default(),
        });
    }

    if let Some(binding_name) = alias_binding_name(expression)? {
        let resolved = resolve_named_binding(binding_name, context, locals, stack)?;
        let result = lower_ui_node(resolved, context, stack, locals, passed, Some(binding_name));
        stack.pop();
        return result;
    }

    let expression = match resolve_alias(expression, context, locals, passed, stack) {
        Ok(expression) => expression,
        Err(error) if error.starts_with("unknown binding path") => expression,
        Err(error) => return Err(error),
    };
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
            if resolved_function_name_for_path_in_stack(path, context, stack).is_some() =>
        {
            invoke_function(
                resolved_function_name_for_path_in_stack(path, context, stack)
                    .expect("guard ensured function resolution"),
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
                let object_base =
                    infer_argument_object_base(&variable.node.value, context, locals, passed)
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
        StaticExpression::FunctionCall { path, arguments } if path_matches_element(path, "label") =>
        {
            lower_label(arguments, context, stack, locals, passed, current_binding)
        }
        StaticExpression::FunctionCall { path, arguments }
            if path_matches_element(path, "button") =>
        {
            lower_button(arguments, context, stack, locals, passed, current_binding)
        }
        StaticExpression::FunctionCall { path, arguments }
            if path_matches_element(path, "container") =>
        {
            lower_container(arguments, context, stack, locals, passed, current_binding)
        }
        StaticExpression::FunctionCall { path, arguments } if path_matches_element(path, "stack") =>
        {
            lower_stack(arguments, context, stack, locals, passed, current_binding)
        }
        StaticExpression::FunctionCall { path, arguments }
            if path_matches_element(path, "stripe") =>
        {
            lower_stripe(arguments, context, stack, locals, passed, current_binding)
        }
        StaticExpression::FunctionCall { path, arguments }
            if path_matches_element(path, "text_input") =>
        {
            lower_text_input(arguments, context, stack, locals, passed, current_binding)
        }
        StaticExpression::FunctionCall { path, arguments } if path_matches_element(path, "select") =>
        {
            lower_select(arguments, context, stack, locals, passed, current_binding)
        }
        StaticExpression::FunctionCall { path, arguments } if path_matches_element(path, "slider") =>
        {
            lower_slider(arguments, context, stack, locals, passed, current_binding)
        }
        StaticExpression::FunctionCall { path, arguments } if path_matches_element(path, "svg") =>
        {
            lower_svg(arguments, context, stack, locals, passed, current_binding)
        }
        StaticExpression::FunctionCall { path, arguments }
            if path_matches_element(path, "svg_circle") =>
        {
            lower_svg_circle(arguments, context, stack, locals, passed, current_binding)
        }
        StaticExpression::FunctionCall { path, arguments }
            if path_matches_element(path, "checkbox") =>
        {
            lower_checkbox(arguments, context, stack, locals, passed, current_binding)
        }
        StaticExpression::FunctionCall { path, arguments }
            if path_matches_element(path, "paragraph") =>
        {
            lower_paragraph(arguments, context, stack, locals, passed, current_binding)
        }
        StaticExpression::FunctionCall { path, arguments } if path_matches_element(path, "link") =>
        {
            lower_link(arguments, context, stack, locals, passed, current_binding)
        }
        StaticExpression::FunctionCall { path, arguments } if path_matches_element(path, "text") => {
            lower_text(arguments, context, stack, locals, passed, current_binding)
        }
        StaticExpression::FunctionCall { path, arguments }
            if path_matches_element(path, "block") =>
        {
            lower_container(arguments, context, stack, locals, passed, current_binding)
        }
        _ => Err(format!(
            "Wasm Pro parser-backed lowering does not support expression `{}` yet",
            describe_expression_detailed(expression)
        )),
    }
}

fn scalar_binding_has_runtime_state(binding: &str, context: &LowerContext<'_>) -> bool {
    if context.scalar_plan.initial_values.contains_key(binding) {
        return true;
    }
    if context
        .scalar_plan
        .derived_scalars
        .iter()
        .any(|spec| derived_scalar_target(spec) == binding)
    {
        return true;
    }
    context.scalar_plan.event_updates.values().any(|updates| {
        updates.iter().any(|update| match update {
            ScalarUpdate::Set {
                binding: target, ..
            }
            | ScalarUpdate::SetFromPayloadNumber { binding: target }
            | ScalarUpdate::SetFiltered {
                binding: target, ..
            }
            | ScalarUpdate::Add {
                binding: target, ..
            }
            | ScalarUpdate::AddTenths {
                binding: target, ..
            }
            | ScalarUpdate::ToggleBool { binding: target } => target == binding,
        })
    })
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
    let (StaticExpression::When { arms } | StaticExpression::While { arms }) = &to.node else {
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
    let truthy_node = {
        let mut branch_stack = stack.clone();
        let mut branch_locals = locals.clone();
        let mut branch_passed = passed.clone();
        lower_ui_node(
            truthy_expr,
            context,
            &mut branch_stack,
            &mut branch_locals,
            &mut branch_passed,
            None,
        )?
    };
    let falsy_node = {
        let mut branch_stack = stack.clone();
        let mut branch_locals = locals.clone();
        let mut branch_passed = passed.clone();
        lower_ui_node(
            falsy_expr,
            context,
            &mut branch_stack,
            &mut branch_locals,
            &mut branch_passed,
            None,
        )?
    };
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
        if let Some(binding_path) = scalar_binding_path(binding_name, context, locals, passed) {
            return Ok(Some(SemanticNode::bool_branch(
                binding_path,
                truthy_node,
                falsy_node,
            )));
        }
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
                let nested_truthy_result = lower_bool_condition_from_nodes(
                    truthy_body,
                    truthy_node.clone(),
                    falsy_node.clone(),
                    context,
                    stack,
                    locals,
                    passed,
                )?;
                let nested_truthy_node = match nested_truthy_result {
                    Some(node) => node,
                    None => lower_ui_node(truthy_body, context, stack, locals, passed, None)?,
                };
                let nested_falsy_result = lower_bool_condition_from_nodes(
                    falsy_body,
                    truthy_node.clone(),
                    falsy_node.clone(),
                    context,
                    stack,
                    locals,
                    passed,
                )?;
                let nested_falsy_node = match nested_falsy_result {
                    Some(node) => node,
                    None => lower_ui_node(falsy_body, context, stack, locals, passed, None)?,
                };
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
                let lowered = lower_bool_condition_from_nodes(
                    body,
                    truthy_node,
                    falsy_node,
                    context,
                    stack,
                    locals,
                    passed,
                );
                locals.pop();
                return lowered;
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
    if let Some(selected) = initial_bool_expression(expression, context, stack, locals, passed)? {
        return Ok(Some(if selected { truthy_node } else { falsy_node }));
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
    let Some(binding) = canonical_expression_path(from, context, locals, passed, &mut Vec::new())
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
    let invert = if path_matches(path, &["List", "is_empty"]) {
        false
    } else if path_matches(path, &["List", "is_not_empty"]) {
        true
    } else {
        return Ok(None);
    };
    if !arguments.is_empty() {
        return Ok(None);
    }
    if let Some(binding) = runtime_object_list_binding_path(from, context, locals, passed, stack)? {
        return Ok(Some((binding, true, invert)));
    }
    if let Some(binding) = runtime_list_binding_path(from, context, locals, passed, stack)? {
        return Ok(Some((binding, false, invert)));
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

fn identity_bool_pipe_source<'a>(
    expression: &'a StaticSpannedExpression,
) -> Option<&'a StaticSpannedExpression> {
    let StaticExpression::Pipe { from, to } = &expression.node else {
        return None;
    };
    let (StaticExpression::When { arms } | StaticExpression::While { arms }) = &to.node else {
        return None;
    };
    let (truthy, falsy) = bool_condition_arm_bodies(arms)?;
    match (
        extract_bool_literal_opt(truthy).ok().flatten(),
        extract_bool_literal_opt(falsy).ok().flatten(),
    ) {
        (Some(true), Some(false)) => Some(from),
        _ => None,
    }
}

fn preserves_input_value_pipe(
    expression: &StaticSpannedExpression,
) -> bool {
    let StaticExpression::FunctionCall { path, .. } = &expression.node else {
        return false;
    };
    path_matches(path, &["Log", "info"])
}

fn scalar_compare_branch_operands(
    expression: &StaticSpannedExpression,
    context: &LowerContext<'_>,
    locals: &LocalScopes<'_>,
    passed: &PassedScopes,
    stack: &mut Vec<String>,
) -> Result<Option<(DerivedScalarOperand, IntCompareOp, DerivedScalarOperand)>, String> {
    if let Some(operands) =
        list_quantifier_scalar_compare_operands(expression, context, locals, passed, stack)?
    {
        return Ok(Some(operands));
    }
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

fn list_quantifier_scalar_compare_operands(
    expression: &StaticSpannedExpression,
    context: &LowerContext<'_>,
    locals: &LocalScopes<'_>,
    passed: &PassedScopes,
    stack: &mut Vec<String>,
) -> Result<Option<(DerivedScalarOperand, IntCompareOp, DerivedScalarOperand)>, String> {
    let expression = resolve_alias(expression, context, locals, passed, stack)?;
    let StaticExpression::Pipe { from, to } = &expression.node else {
        return Ok(None);
    };
    let StaticExpression::FunctionCall { path, arguments } = &to.node else {
        return Ok(None);
    };
    let quantifier = if path_matches(path, &["List", "any"]) {
        "any"
    } else if path_matches(path, &["List", "every"]) {
        "every"
    } else {
        return Ok(None);
    };
    let item_name = find_positional_parameter_name(arguments)
        .ok_or_else(|| format!("List/{quantifier} requires an item parameter name"))?;
    let condition = find_named_argument(arguments, "if")
        .ok_or_else(|| format!("List/{quantifier} requires `if`"))?;

    if let Some(binding) = runtime_object_list_binding_path(from, context, locals, passed, stack)? {
        let filter = if matches!(
            &condition.node,
            StaticExpression::Literal(static_expression::Literal::Tag(tag)) if tag.as_str() == "True"
        ) {
            None
        } else {
            runtime_object_list_filter(condition, item_name, context, locals, passed)?
        };
        let filtered = DerivedScalarOperand::ObjectListCount {
            binding: binding.clone(),
            filter,
        };
        return Ok(Some(match quantifier {
            "any" => (filtered, IntCompareOp::Greater, DerivedScalarOperand::Literal(0)),
            "every" => (
                filtered,
                IntCompareOp::Equal,
                DerivedScalarOperand::ObjectListCount {
                    binding,
                    filter: None,
                },
            ),
            _ => unreachable!(),
        }));
    }

    if let Some(binding) = runtime_list_binding_path(from, context, locals, passed, stack)? {
        let filter = if matches!(
            &condition.node,
            StaticExpression::Literal(static_expression::Literal::Tag(tag)) if tag.as_str() == "True"
        ) {
            None
        } else {
            Some(runtime_text_list_filter(condition, item_name)?)
        };
        let filtered = DerivedScalarOperand::TextListCount {
            binding: binding.clone(),
            filter,
        };
        return Ok(Some(match quantifier {
            "any" => (filtered, IntCompareOp::Greater, DerivedScalarOperand::Literal(0)),
            "every" => (
                filtered,
                IntCompareOp::Equal,
                DerivedScalarOperand::TextListCount {
                    binding,
                    filter: None,
                },
            ),
            _ => unreachable!(),
        }));
    }

    Ok(None)
}

fn object_scalar_compare_branch_operands(
    expression: &StaticSpannedExpression,
    context: &LowerContext<'_>,
    locals: &LocalScopes<'_>,
    passed: &PassedScopes,
    stack: &mut Vec<String>,
) -> Result<
    Option<(
        ObjectDerivedScalarOperand,
        IntCompareOp,
        ObjectDerivedScalarOperand,
    )>,
    String,
> {
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
    if let Some(source) = identity_bool_pipe_source(expression) {
        return object_derived_scalar_operand_in_context(source, context, locals, passed, stack);
    }
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
    if let Ok(path) = canonical_expression_path(expression, context, locals, passed, stack) {
        if context.scalar_plan.initial_values.contains_key(&path)
            || context.path_bindings.contains_key(&path)
        {
            return Ok(Some(ObjectDerivedScalarOperand::Binding(path)));
        }
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
    if let Some(source) = identity_bool_pipe_source(expression) {
        return derived_scalar_operand_in_context(source, context, locals, passed, stack);
    }
    if let StaticExpression::Pipe { from, to } = &expression.node {
        if preserves_input_value_pipe(to) {
            return derived_scalar_operand_in_context(from, context, locals, passed, stack);
        }
    }
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
    if let Ok(path) = canonical_expression_path(expression, context, locals, passed, stack) {
        if context.scalar_plan.initial_values.contains_key(&path)
            || context.path_bindings.contains_key(&path)
        {
            return Ok(Some(DerivedScalarOperand::Binding(path)));
        }
    }
    let expression = resolve_alias(expression, context, locals, passed, stack)?;
    if let Some(binding) =
        text_to_number_source_binding(expression, context, locals, passed, stack)?
    {
        return Ok(Some(DerivedScalarOperand::TextBindingNumber(binding)));
    }
    if let Some(value) = extract_integer_literal_opt(expression)? {
        return Ok(Some(DerivedScalarOperand::Literal(value)));
    }
    if let Some(value) = extract_bool_literal_opt(expression)? {
        return Ok(Some(DerivedScalarOperand::Literal(i64::from(value))));
    }
    if let Some(value) = extract_filter_tag_value(expression)? {
        return Ok(Some(DerivedScalarOperand::Literal(value)));
    }
    match &expression.node {
        StaticExpression::ArithmeticOperator(operator) => {
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
                static_expression::ArithmeticOperator::Multiply {
                    operand_a,
                    operand_b,
                } => (
                    operand_a.as_ref(),
                    operand_b.as_ref(),
                    DerivedArithmeticOp::Multiply,
                ),
                static_expression::ArithmeticOperator::Divide {
                    operand_a,
                    operand_b,
                } => (
                    operand_a.as_ref(),
                    operand_b.as_ref(),
                    DerivedArithmeticOp::Divide,
                ),
                static_expression::ArithmeticOperator::Negate { .. } => return Ok(None),
            };
            let Some(left) =
                derived_scalar_operand_in_context(left_expr, context, locals, passed, stack)?
            else {
                return Ok(None);
            };
            let Some(right) =
                derived_scalar_operand_in_context(right_expr, context, locals, passed, stack)?
            else {
                return Ok(None);
            };
            return Ok(Some(DerivedScalarOperand::Arithmetic {
                op,
                left: Box::new(left),
                right: Box::new(right),
            }));
        }
        StaticExpression::Pipe { from, to } => {
            let StaticExpression::FunctionCall { path, arguments } = &to.node else {
                return Ok(None);
            };
            if path_matches(path, &["Math", "round"]) && arguments.is_empty() {
                let Some(source) =
                    derived_scalar_operand_in_context(from, context, locals, passed, stack)?
                else {
                    return Ok(None);
                };
                return Ok(Some(DerivedScalarOperand::Round {
                    source: Box::new(source),
                }));
            }
            if path_matches(path, &["Math", "min"]) {
                let Some(left) =
                    derived_scalar_operand_in_context(from, context, locals, passed, stack)?
                else {
                    return Ok(None);
                };
                let right_expr = find_named_argument(arguments, "b")
                    .ok_or_else(|| "Math/min requires `b`".to_string())?;
                let Some(right) =
                    derived_scalar_operand_in_context(right_expr, context, locals, passed, stack)?
                else {
                    return Ok(None);
                };
                return Ok(Some(DerivedScalarOperand::Min {
                    left: Box::new(left),
                    right: Box::new(right),
                }));
            }
        }
        _ => {}
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
            infer_tag(element, "span", context, stack, locals, passed)?,
            None,
            collect_common_properties(element, context, stack, locals, passed)?,
            collect_event_bindings(element, current_binding, context, stack, locals, passed)?,
            collect_fact_bindings(element, context, stack, locals, passed)?,
            vec![lower_ui_node(label, context, stack, locals, passed, None)?],
        ),
        style,
        context,
        stack,
        locals,
        passed,
    )
}

fn lower_text<'a>(
    arguments: &'a [static_expression::Spanned<StaticArgument>],
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
    current_binding: Option<&str>,
) -> Result<SemanticNode, String> {
    let text = find_named_argument(arguments, "text")
        .ok_or_else(|| "Element/text requires `text`".to_string())?;
    let element = find_named_argument(arguments, "element");
    let style = find_named_argument(arguments, "style");

    finalize_element_node(
        SemanticNode::element_with_facts(
            infer_tag(element, "span", context, stack, locals, passed)?,
            None,
            collect_common_properties(element, context, stack, locals, passed)?,
            collect_event_bindings(element, current_binding, context, stack, locals, passed)?,
            collect_fact_bindings(element, context, stack, locals, passed)?,
            vec![lower_ui_node(text, context, stack, locals, passed, None)?],
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
            infer_tag(element, "p", context, stack, locals, passed)?,
            None,
            collect_common_properties(element, context, stack, locals, passed)?,
            collect_event_bindings(element, current_binding, context, stack, locals, passed)?,
            collect_fact_bindings(element, context, stack, locals, passed)?,
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

    let mut properties = collect_common_properties(element, context, stack, locals, passed)?;
    if let Some(to) = to.and_then(|to| lower_text_value(to, context, stack, locals, passed).ok()) {
        properties.push(("href".to_string(), to));
    }
    if new_tab.is_some() {
        properties.push(("target".to_string(), "_blank".to_string()));
    }

    finalize_element_node(
        SemanticNode::element_with_facts(
            infer_tag(element, "a", context, stack, locals, passed)?,
            None,
            properties,
            collect_event_bindings(element, current_binding, context, stack, locals, passed)?,
            collect_fact_bindings(element, context, stack, locals, passed)?,
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
    let mut properties = collect_common_properties(element, context, stack, locals, passed)?;
    if !properties.iter().any(|(name, _)| name == "type") {
        properties.push(("type".to_string(), "button".to_string()));
    }

    finalize_element_node(
        SemanticNode::element_with_facts(
            infer_tag(element, "button", context, stack, locals, passed)?,
            None,
            properties,
            collect_event_bindings(element, current_binding, context, stack, locals, passed)?,
            collect_fact_bindings(element, context, stack, locals, passed)?,
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
    let element = find_named_argument(arguments, "element");
    let style = find_named_argument(arguments, "style");
    let children = find_named_argument(arguments, "child")
        .map(|child| lower_ui_node(child, context, stack, locals, passed, None))
        .transpose()?
        .into_iter()
        .collect();

    finalize_element_node(
        SemanticNode::element_with_facts(
            infer_tag(element, "div", context, stack, locals, passed)?,
            None,
            collect_common_properties(element, context, stack, locals, passed)?,
            collect_event_bindings(element, current_binding, context, stack, locals, passed)?,
            collect_fact_bindings(element, context, stack, locals, passed)?,
            children,
        ),
        style,
        context,
        stack,
        locals,
        passed,
    )
}

fn lower_stack<'a>(
    arguments: &'a [static_expression::Spanned<StaticArgument>],
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
    current_binding: Option<&str>,
) -> Result<SemanticNode, String> {
    let layers = find_named_argument(arguments, "layers")
        .ok_or_else(|| "Element/stack requires `layers`".to_string())?;
    let element = find_named_argument(arguments, "element");
    let style = find_named_argument(arguments, "style");

    let children = lower_list_items(layers, context, stack, locals, passed)?
        .into_iter()
        .map(|child| {
            push_style_fragment(
                child,
                SemanticStyleFragment::Static(Some("grid-area:1 / 1".to_string())),
            )
        })
        .collect();

    let mut properties = collect_common_properties(element, context, stack, locals, passed)?;
    merge_style_property(
        &mut properties,
        vec!["display:grid".to_string(), "position:relative".to_string()],
    );

    finalize_element_node(
        SemanticNode::element_with_facts(
            infer_tag(element, "div", context, stack, locals, passed)?,
            None,
            properties,
            collect_event_bindings(element, current_binding, context, stack, locals, passed)?,
            collect_fact_bindings(element, context, stack, locals, passed)?,
            children,
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
    let mut properties = collect_common_properties(element, context, stack, locals, passed)?;
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
            infer_tag(element, "div", context, stack, locals, passed)?,
            None,
            properties,
            collect_event_bindings(element, current_binding, context, stack, locals, passed)?,
            collect_fact_bindings(element, context, stack, locals, passed)?,
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

    let mut properties = collect_common_properties(element, context, stack, locals, passed)?;
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
        infer_tag(element, "input", context, stack, locals, passed)?,
        None,
        properties,
        collect_event_bindings(element, current_binding, context, stack, locals, passed)?,
        collect_fact_bindings(element, context, stack, locals, passed)?,
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

fn lower_select<'a>(
    arguments: &'a [static_expression::Spanned<StaticArgument>],
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
    current_binding: Option<&str>,
) -> Result<SemanticNode, String> {
    let element = find_named_argument(arguments, "element");
    let options = find_named_argument(arguments, "options")
        .ok_or_else(|| "Element/select requires `options`".to_string())?;
    let selected = find_named_argument(arguments, "selected");
    let style = find_named_argument(arguments, "style");

    let selected_value = selected
        .map(|selected| lower_text_input_initial_value(selected, context, stack, locals, passed))
        .transpose()?;
    let input_value = selected
        .map(|selected| lower_text_input_value_source(selected, context, stack, locals, passed))
        .transpose()?
        .flatten();
    let children = lower_select_options(
        options,
        selected_value.as_deref(),
        context,
        stack,
        locals,
        passed,
    )?;

    let node = SemanticNode::element_with_facts(
        infer_tag(element, "select", context, stack, locals, passed)?,
        None,
        collect_common_properties(element, context, stack, locals, passed)?,
        collect_event_bindings(element, current_binding, context, stack, locals, passed)?,
        collect_fact_bindings(element, context, stack, locals, passed)?,
        children,
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

fn lower_slider<'a>(
    arguments: &'a [static_expression::Spanned<StaticArgument>],
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
    current_binding: Option<&str>,
) -> Result<SemanticNode, String> {
    let element = find_named_argument(arguments, "element");
    let value = find_named_argument(arguments, "value");
    let min = find_named_argument(arguments, "min");
    let max = find_named_argument(arguments, "max");
    let step = find_named_argument(arguments, "step");
    let style = find_named_argument(arguments, "style");

    let mut properties = collect_common_properties(element, context, stack, locals, passed)?;
    properties.push(("type".to_string(), "range".to_string()));

    if let Some(value) = value {
        if let Some(number) =
            initial_scalar_value_in_context(value, context, stack, locals, passed)?
        {
            properties.push(("value".to_string(), number.to_string()));
        }
    }
    if let Some(min) = min.and_then(extract_number) {
        properties.push(("min".to_string(), trim_number(min)));
    }
    if let Some(max) = max.and_then(extract_number) {
        properties.push(("max".to_string(), trim_number(max)));
    }
    if let Some(step) = step.and_then(extract_number) {
        properties.push(("step".to_string(), trim_number(step)));
    }

    finalize_element_node(
        SemanticNode::element_with_facts(
            infer_tag(element, "input", context, stack, locals, passed)?,
            None,
            properties,
            collect_event_bindings(element, current_binding, context, stack, locals, passed)?,
            collect_fact_bindings(element, context, stack, locals, passed)?,
            Vec::new(),
        ),
        style,
        context,
        stack,
        locals,
        passed,
    )
}

fn lower_svg<'a>(
    arguments: &'a [static_expression::Spanned<StaticArgument>],
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
    current_binding: Option<&str>,
) -> Result<SemanticNode, String> {
    let children = find_named_argument(arguments, "children")
        .ok_or_else(|| "Element/svg requires `children`".to_string())?;
    let element = find_named_argument(arguments, "element");
    let style = find_named_argument(arguments, "style");

    let mut properties = collect_common_properties(element, context, stack, locals, passed)?;
    if let Some(style_object) = style.and_then(resolve_object) {
        let mut style_parts = Vec::new();
        if let Some(width) = find_object_field(style_object, "width").and_then(extract_number) {
            style_parts.push(format!("width:{}px", trim_number(width)));
        }
        if let Some(height) = find_object_field(style_object, "height").and_then(extract_number) {
            style_parts.push(format!("height:{}px", trim_number(height)));
        }
        if let Some(background) = find_object_field(style_object, "background") {
            style_parts.push(format!(
                "background:{}",
                lower_text_input_initial_value(background, context, stack, locals, passed)?
            ));
        }
        merge_style_property(&mut properties, style_parts);
    }

    finalize_element_node(
        SemanticNode::element_with_facts(
            infer_tag(element, "svg", context, stack, locals, passed)?,
            None,
            properties,
            collect_event_bindings(element, current_binding, context, stack, locals, passed)?,
            collect_fact_bindings(element, context, stack, locals, passed)?,
            lower_list_items(children, context, stack, locals, passed)?,
        ),
        style,
        context,
        stack,
        locals,
        passed,
    )
}

fn lower_svg_circle<'a>(
    arguments: &'a [static_expression::Spanned<StaticArgument>],
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
    current_binding: Option<&str>,
) -> Result<SemanticNode, String> {
    let cx = find_named_argument(arguments, "cx")
        .ok_or_else(|| "Element/svg_circle requires `cx`".to_string())?;
    let cy = find_named_argument(arguments, "cy")
        .ok_or_else(|| "Element/svg_circle requires `cy`".to_string())?;
    let r = find_named_argument(arguments, "r")
        .ok_or_else(|| "Element/svg_circle requires `r`".to_string())?;
    let element = find_named_argument(arguments, "element");
    let style = find_named_argument(arguments, "style");

    let mut properties = collect_common_properties(element, context, stack, locals, passed)?;
    properties.push((
        "cx".to_string(),
        lower_scalar_property_value(cx, context, stack, locals, passed)
            .map_err(|_| "Element/svg_circle `cx` must resolve to a scalar".to_string())?,
    ));
    properties.push((
        "cy".to_string(),
        lower_scalar_property_value(cy, context, stack, locals, passed)
            .map_err(|_| "Element/svg_circle `cy` must resolve to a scalar".to_string())?,
    ));
    properties.push((
        "r".to_string(),
        lower_scalar_property_value(r, context, stack, locals, passed)
            .map_err(|_| "Element/svg_circle `r` must resolve to a scalar".to_string())?,
    ));
    if let Some(style_object) = style.and_then(resolve_object) {
        if let Some(fill) = find_object_field(style_object, "fill") {
            properties.push((
                "fill".to_string(),
                lower_text_input_initial_value(fill, context, stack, locals, passed)?,
            ));
        }
        if let Some(stroke) = find_object_field(style_object, "stroke") {
            properties.push((
                "stroke".to_string(),
                lower_text_input_initial_value(stroke, context, stack, locals, passed)?,
            ));
        }
        if let Some(stroke_width) =
            find_object_field(style_object, "stroke_width").and_then(extract_number)
        {
            properties.push(("stroke-width".to_string(), trim_number(stroke_width)));
        }
    }

    finalize_element_node(
        SemanticNode::element_with_facts(
            infer_tag(element, "circle", context, stack, locals, passed)?,
            None,
            properties,
            collect_event_bindings(element, current_binding, context, stack, locals, passed)?,
            collect_fact_bindings(element, context, stack, locals, passed)?,
            Vec::new(),
        ),
        style,
        context,
        stack,
        locals,
        passed,
    )
}

fn lower_scalar_property_value<'a>(
    expression: &'a StaticSpannedExpression,
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<String, String> {
    const SCALAR_PROPERTY_TOKEN_PREFIX: &str = "__boon_scalar_binding__:";
    const OBJECT_FIELD_PROPERTY_TOKEN_PREFIX: &str = "__boon_object_field__:";

    if let Some(field) = placeholder_object_field(expression, context, locals, passed)? {
        return Ok(format!("{OBJECT_FIELD_PROPERTY_TOKEN_PREFIX}{field}"));
    }
    if let Some((binding, _)) =
        resolve_scalar_reference(expression, context, locals, passed, stack)?
    {
        return Ok(format!("{SCALAR_PROPERTY_TOKEN_PREFIX}{binding}"));
    }
    initial_scalar_value_in_context(expression, context, stack, locals, passed)?
        .map(|value| value.to_string())
        .ok_or_else(|| "expected scalar property value".to_string())
}

fn lower_select_options<'a>(
    expression: &'a StaticSpannedExpression,
    selected_value: Option<&str>,
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<Vec<SemanticNode>, String> {
    let expression = resolve_alias(expression, context, locals, passed, stack)?;
    let StaticExpression::List { items } = &expression.node else {
        return Err("Element/select `options` must be a static LIST".to_string());
    };

    let mut children = Vec::with_capacity(items.len());
    for item in items {
        let item = resolve_alias(item, context, locals, passed, stack)?;
        let StaticExpression::Object(object) = &item.node else {
            return Err("Element/select options must be objects".to_string());
        };
        let value = find_object_field(object, "value")
            .ok_or_else(|| "Element/select option requires `value`".to_string())?;
        let label = find_object_field(object, "label")
            .ok_or_else(|| "Element/select option requires `label`".to_string())?;

        let value = lower_text_input_initial_value(value, context, stack, locals, passed)?;
        let mut properties = vec![("value".to_string(), value.clone())];
        if selected_value.is_some_and(|selected| selected == value) {
            properties.push(("selected".to_string(), "true".to_string()));
        }
        let label_node = lower_ui_node(label, context, stack, locals, passed, None)?;
        children.push(SemanticNode::element(
            "option",
            None,
            properties,
            Vec::new(),
            vec![label_node],
        ));
    }

    Ok(children)
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
    let mut properties = collect_common_properties(element, context, stack, locals, passed)?;
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
            infer_tag(element, "button", context, stack, locals, passed)?,
            None,
            properties,
            collect_event_bindings(element, current_binding, context, stack, locals, passed)?,
            collect_fact_bindings(element, context, stack, locals, passed)?,
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

    if let Some(static_styles) =
        static_supported_style_fragment_expr(style, context, stack, locals, passed)?
    {
        node = push_style_fragment(node, static_styles);
    }
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

fn static_supported_style_fragment_expr<'a>(
    style: &'a StaticSpannedExpression,
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<Option<SemanticStyleFragment>, String> {
    let Some(style_object) = resolved_object(style, context, locals, passed, stack)? else {
        return Ok(None);
    };
    let mut parts = Vec::new();

    if let Some(width) = find_object_field(style_object, "width") {
        if let Some(width) = static_dimension_css(width, context, stack, locals, passed)? {
            parts.push(format!("width:{width}"));
        }
    }
    if let Some(height) = find_object_field(style_object, "height") {
        if let Some(height) = static_dimension_css(height, context, stack, locals, passed)? {
            parts.push(format!("height:{height}"));
        }
    }
    if let Some(size) = find_object_field(style_object, "size") {
        if let Some(size) = static_dimension_css(size, context, stack, locals, passed)? {
            parts.push(format!("width:{size}"));
            parts.push(format!("height:{size}"));
        }
    }
    if let Some(padding) = find_object_field(style_object, "padding") {
        parts.extend(static_padding_style_parts(
            padding, context, stack, locals, passed,
        )?);
    }
    if let Some(background) = find_object_field(style_object, "background") {
        if let Some(background) =
            static_background_style_part(background, context, stack, locals, passed)?
        {
            parts.push(background);
        }
    }
    if let Some(borders) = find_object_field(style_object, "borders").and_then(resolve_object) {
        parts.extend(static_border_style_parts(borders));
    }
    if let Some(rounded) = find_object_field(style_object, "rounded_corners") {
        if let Some(radius) = static_border_radius_css(rounded, context, stack, locals, passed)? {
            parts.push(format!("border-radius:{radius}"));
        }
    }
    if let Some(cursor) = find_object_field(style_object, "cursor")
        .and_then(extract_tag_name)
        .or_else(|| {
            find_object_field(style_object, "cursor").and_then(|cursor| match &cursor.node {
                StaticExpression::Literal(static_expression::Literal::Text(text)) => {
                    Some(text.as_str())
                }
                _ => None,
            })
        })
    {
        if cursor.eq_ignore_ascii_case("Pointer") {
            parts.push("cursor:pointer".to_string());
        }
    }
    if let Some(scrollbars) = find_object_field(style_object, "scrollbars") {
        match extract_bool_literal_opt(scrollbars)? {
            Some(true) => parts.push("overflow:auto".to_string()),
            Some(false) => parts.push("overflow:hidden".to_string()),
            None => {}
        }
    }
    if let Some(transform) = find_object_field(style_object, "transform").and_then(resolve_object) {
        let move_right = find_object_field(transform, "move_right")
            .and_then(extract_number)
            .unwrap_or(0.0);
        let move_down = find_object_field(transform, "move_down")
            .and_then(extract_number)
            .unwrap_or(0.0);
        let rotate = find_object_field(transform, "rotate")
            .and_then(extract_number)
            .unwrap_or(0.0);
        let mut transforms = Vec::new();
        if move_right != 0.0 || move_down != 0.0 {
            transforms.push(format!(
                "translate({}px,{}px)",
                trim_number(move_right),
                trim_number(move_down)
            ));
        }
        if rotate != 0.0 {
            transforms.push(format!("rotate({}deg)", trim_number(rotate)));
        }
        if !transforms.is_empty() {
            parts.push(format!("transform:{}", transforms.join(" ")));
        }
    }
    if let Some(line_height) =
        find_object_field(style_object, "line_height").and_then(extract_number)
    {
        parts.push(format!("line-height:{}", trim_number(line_height)));
    }
    if let Some(font_smoothing) =
        find_object_field(style_object, "font_smoothing").and_then(extract_tag_name)
    {
        if font_smoothing.eq_ignore_ascii_case("Antialiased") {
            parts.push("-webkit-font-smoothing:antialiased".to_string());
        }
    }
    if let Some(font) = find_object_field(style_object, "font").and_then(resolve_object) {
        if let Some(size) = find_object_field(font, "size").and_then(extract_number) {
            parts.push(format!("font-size:{}px", trim_number(size)));
        }
        if let Some(weight) = find_object_field(font, "weight")
            .and_then(extract_tag_name)
            .or_else(|| {
                find_object_field(font, "weight").and_then(|weight| match &weight.node {
                    StaticExpression::Literal(static_expression::Literal::Text(text)) => {
                        Some(text.as_str())
                    }
                    _ => None,
                })
            })
            .and_then(static_font_weight_css)
        {
            parts.push(format!("font-weight:{weight}"));
        }
        if let Some(align) = find_object_field(font, "align")
            .and_then(extract_tag_name)
            .or_else(|| {
                find_object_field(font, "align").and_then(|align| match &align.node {
                    StaticExpression::Literal(static_expression::Literal::Text(text)) => {
                        Some(text.as_str())
                    }
                    _ => None,
                })
            })
            .and_then(static_text_align_css)
        {
            parts.push(format!("text-align:{align}"));
        }
    }

    if parts.is_empty() {
        Ok(None)
    } else {
        Ok(Some(SemanticStyleFragment::Static(Some(parts.join(";")))))
    }
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
    let expression = match resolve_alias(expression, context, locals, passed, stack) {
        Ok(expression) => expression,
        Err(error) if error.starts_with("unknown binding path") => expression,
        Err(error) => return Err(error),
    };
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
                    let Some(truthy) = lower_background_url_style_fragment(
                        truthy, context, stack, locals, passed,
                    )?
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

fn style_condition_fragment<'a>(
    expression: &'a StaticSpannedExpression,
    context: &'a LowerContext<'a>,
    locals: &mut LocalScopes<'a>,
    passed: &PassedScopes,
    stack: &mut Vec<String>,
) -> Result<Option<StyleConditionFragment>, String> {
    let expression = resolve_alias(expression, context, locals, passed, stack)?;
    if let StaticExpression::Pipe { from, to } = &expression.node {
        if let StaticExpression::When { arms } | StaticExpression::While { arms } = &to.node {
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
                let lowered = style_condition_fragment(body, context, locals, passed, stack);
                locals.pop();
                return lowered;
            }
            if bool_condition_arm_bodies(arms).is_some() {
                return style_condition_fragment(from, context, locals, passed, stack);
            }
        }
    }
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
        let lowered = resolved_style_number(output, context, stack, locals, passed);
        locals.pop();
        return lowered;
    }

    let expression = resolve_alias(expression, context, locals, passed, stack)?;

    if let StaticExpression::Pipe { from, to } = &expression.node {
        if let StaticExpression::When { arms } | StaticExpression::While { arms } = &to.node {
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
                return resolved_style_number(
                    body,
                    context,
                    stack,
                    &mut nested_locals,
                    &mut nested_passed,
                );
            }
        }
        if function_invocation_target(to, context, locals, passed, stack)?.is_some() {
            let Some((function_name, arguments)) =
                function_invocation_target(to, context, locals, passed, stack)?
            else {
                unreachable!();
            };
            return with_invoked_function_scope(
                function_name,
                arguments,
                Some(from.as_ref()),
                context,
                stack,
                locals,
                passed,
                |body, context, stack, locals, passed| {
                    resolved_style_number(body, context, stack, locals, passed)
                },
            );
        }
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
                resolved_style_number(body, context, stack, locals, passed)
            },
        );
    }

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

fn static_dimension_css<'a>(
    expression: &'a StaticSpannedExpression,
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<Option<String>, String> {
    if let Some(number) = resolved_style_number(expression, context, stack, locals, passed)? {
        return Ok(Some(format!("{}px", trim_number(number))));
    }
    let expression = resolve_alias(expression, context, locals, passed, stack)?;
    Ok(
        matches!(
            &expression.node,
            StaticExpression::Literal(static_expression::Literal::Tag(tag))
                | StaticExpression::Literal(static_expression::Literal::Text(tag))
                    if tag.as_str().eq_ignore_ascii_case("Fill")
        )
        .then_some("100%".to_string()),
    )
}

fn static_padding_style_parts<'a>(
    expression: &'a StaticSpannedExpression,
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<Vec<String>, String> {
    let expression = resolve_alias(expression, context, locals, passed, stack)?;
    if let Some(number) = extract_number(expression) {
        return Ok(vec![format!("padding:{}px", trim_number(number))]);
    }
    let Some(object) = resolve_object(expression) else {
        return Ok(Vec::new());
    };

    let row = find_object_field(object, "row")
        .map(|field| resolved_style_number(field, context, stack, locals, passed))
        .transpose()?
        .flatten()
        .map(trim_number);
    let column = find_object_field(object, "column")
        .map(|field| resolved_style_number(field, context, stack, locals, passed))
        .transpose()?
        .flatten()
        .map(trim_number);
    let top = find_object_field(object, "top")
        .map(|field| resolved_style_number(field, context, stack, locals, passed))
        .transpose()?
        .flatten()
        .map(trim_number)
        .or_else(|| row.clone());
    let bottom = find_object_field(object, "bottom")
        .map(|field| resolved_style_number(field, context, stack, locals, passed))
        .transpose()?
        .flatten()
        .map(trim_number)
        .or_else(|| row);
    let left = find_object_field(object, "left")
        .map(|field| resolved_style_number(field, context, stack, locals, passed))
        .transpose()?
        .flatten()
        .map(trim_number)
        .or_else(|| column.clone());
    let right = find_object_field(object, "right")
        .map(|field| resolved_style_number(field, context, stack, locals, passed))
        .transpose()?
        .flatten()
        .map(trim_number)
        .or_else(|| column);

    let mut parts = Vec::new();
    if let Some(top) = top {
        parts.push(format!("padding-top:{top}px"));
    }
    if let Some(right) = right {
        parts.push(format!("padding-right:{right}px"));
    }
    if let Some(bottom) = bottom {
        parts.push(format!("padding-bottom:{bottom}px"));
    }
    if let Some(left) = left {
        parts.push(format!("padding-left:{left}px"));
    }
    Ok(parts)
}

fn static_background_style_part<'a>(
    expression: &'a StaticSpannedExpression,
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<Option<String>, String> {
    let expression = resolve_alias(expression, context, locals, passed, stack)?;
    if let Some(object) = resolve_object(expression) {
        if let Some(color) = find_object_field(object, "color").and_then(static_css_color) {
            return Ok(Some(format!("background:{color}")));
        }
        return Ok(None);
    }
    lower_text_input_initial_value(expression, context, stack, locals, passed)
        .map(|value| Some(format!("background:{value}")))
        .or(Ok(None))
}

fn static_border_style_parts(object: &StaticObject) -> Vec<String> {
    let mut parts = Vec::new();
    for side in ["top", "right", "bottom", "left"] {
        let Some(border) = find_object_field(object, side).and_then(resolve_object) else {
            continue;
        };
        let color = find_object_field(border, "color")
            .and_then(static_css_color)
            .unwrap_or_else(|| "currentColor".to_string());
        parts.push(format!("border-{side}:1px solid {color}"));
    }
    parts
}

fn static_border_radius_css<'a>(
    expression: &'a StaticSpannedExpression,
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<Option<String>, String> {
    if let Some(number) = resolved_style_number(expression, context, stack, locals, passed)? {
        return Ok(Some(format!("{}px", trim_number(number))));
    }
    let expression = resolve_alias(expression, context, locals, passed, stack)?;
    if let StaticExpression::Pipe { from, to } = &expression.node {
        if let StaticExpression::When { arms } | StaticExpression::While { arms } = &to.node {
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
                return static_border_radius_css(
                    body,
                    context,
                    stack,
                    &mut nested_locals,
                    &mut nested_passed,
                );
            }
        }
        if function_invocation_target(to, context, locals, passed, stack)?.is_some() {
            let Some((function_name, arguments)) =
                function_invocation_target(to, context, locals, passed, stack)?
            else {
                unreachable!();
            };
            return with_invoked_function_scope(
                function_name,
                arguments,
                Some(from.as_ref()),
                context,
                stack,
                locals,
                passed,
                |body, context, stack, locals, passed| {
                    static_border_radius_css(body, context, stack, locals, passed)
                },
            );
        }
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
                static_border_radius_css(body, context, stack, locals, passed)
            },
        );
    }
    Ok(match &expression.node {
        StaticExpression::Literal(static_expression::Literal::Number(number)) => {
            Some(format!("{}px", trim_number(*number)))
        }
        StaticExpression::Literal(static_expression::Literal::Tag(tag))
        | StaticExpression::Literal(static_expression::Literal::Text(tag))
            if tag.as_str().eq_ignore_ascii_case("Fully") =>
        {
            Some("9999px".to_string())
        }
        _ => None,
    })
}

fn static_font_weight_css(weight: &str) -> Option<&'static str> {
    if weight.eq_ignore_ascii_case("Thin") {
        Some("100")
    } else if weight.eq_ignore_ascii_case("ExtraLight") {
        Some("200")
    } else if weight.eq_ignore_ascii_case("Light") {
        Some("300")
    } else if weight.eq_ignore_ascii_case("Normal") || weight.eq_ignore_ascii_case("Regular") {
        Some("400")
    } else if weight.eq_ignore_ascii_case("Medium") {
        Some("500")
    } else if weight.eq_ignore_ascii_case("SemiBold") {
        Some("600")
    } else if weight.eq_ignore_ascii_case("Bold") {
        Some("700")
    } else if weight.eq_ignore_ascii_case("ExtraBold") {
        Some("800")
    } else if weight.eq_ignore_ascii_case("Black") {
        Some("900")
    } else {
        None
    }
}

fn static_text_align_css(align: &str) -> Option<&'static str> {
    if align.eq_ignore_ascii_case("Left") {
        Some("left")
    } else if align.eq_ignore_ascii_case("Center") {
        Some("center")
    } else if align.eq_ignore_ascii_case("Right") {
        Some("right")
    } else {
        None
    }
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
            if resolved_function_name_for_path_in_stack(path, context, stack).is_some() =>
        {
            with_invoked_function_scope(
                resolved_function_name_for_path_in_stack(path, context, stack)
                    .expect("guard ensured function resolution"),
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
                        | SemanticTextPart::DerivedScalarExpr(_)
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
            if resolved_function_name_for_path_in_stack(path, context, stack).is_some() =>
        {
            with_invoked_function_scope(
                resolved_function_name_for_path_in_stack(path, context, stack)
                    .expect("guard ensured function resolution"),
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
    if let Some(branch) =
        lower_text_input_number_branch(from, arms, context, stack, locals, passed)?
    {
        return Ok(Some(branch));
    }
    if let Some(branch) =
        lower_text_input_value_match_branch(from, arms, context, stack, locals, passed)?
    {
        return Ok(Some(branch));
    }
    let Some((truthy_body, falsy_body)) = bool_condition_arm_bodies(arms) else {
        return Ok(None);
    };
    let truthy = lower_text_input_value_source(truthy_body, context, stack, locals, passed)?
        .unwrap_or(SemanticInputValue::Static(lower_text_input_initial_value(
            truthy_body,
            context,
            stack,
            locals,
            passed,
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

fn lower_text_input_value_match_branch<'a>(
    source: &'a StaticSpannedExpression,
    arms: &'a [static_expression::Arm],
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<Option<SemanticInputValue>, String> {
    let Some((binding, _)) = resolve_text_reference(source, context, locals, passed, stack)? else {
        return Ok(None);
    };

    let mut wildcard_body = None;
    let mut literal_arms = Vec::new();
    for arm in arms {
        match &arm.pattern {
            static_expression::Pattern::Literal(static_expression::Literal::Tag(tag))
            | static_expression::Pattern::Literal(static_expression::Literal::Text(tag)) => {
                literal_arms.push((tag.as_str().to_string(), &arm.body));
            }
            static_expression::Pattern::WildCard => {
                wildcard_body = Some(&arm.body);
            }
            _ => return Ok(None),
        }
    }

    let Some(wildcard_body) = wildcard_body else {
        return Ok(None);
    };
    if literal_arms.is_empty() {
        return Ok(None);
    }

    let mut branch = lower_text_input_value_body(wildcard_body, context, stack, locals, passed)?;
    for (expected, body) in literal_arms.into_iter().rev() {
        branch = SemanticInputValue::TextValueBranch {
            binding: binding.clone(),
            expected,
            truthy: Box::new(lower_text_input_value_body(
                body, context, stack, locals, passed,
            )?),
            falsy: Box::new(branch),
        };
    }

    Ok(Some(branch))
}

fn lower_text_input_value_body<'a>(
    body: &'a StaticSpannedExpression,
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<SemanticInputValue, String> {
    if let Some(value) = lower_text_input_value_source(body, context, stack, locals, passed)? {
        return Ok(value);
    }
    if let Ok(lowered_node) = lower_ui_node(body, context, stack, locals, passed, None) {
        if semantic_node_can_render_input_value(&lowered_node) {
            return Ok(SemanticInputValue::Node(Box::new(lowered_node)));
        }
    }
    Ok(SemanticInputValue::Static(lower_text_input_initial_value(
        body, context, stack, locals, passed,
    )?))
}

fn lower_text_input_number_branch<'a>(
    source: &'a StaticSpannedExpression,
    arms: &'a [static_expression::Arm],
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<Option<SemanticInputValue>, String> {
    let Some(binding) = text_to_number_source_binding(source, context, locals, passed, stack)?
    else {
        return Ok(None);
    };

    let mut nan_body = None;
    for arm in arms {
        if matches!(
            &arm.pattern,
            static_expression::Pattern::Literal(static_expression::Literal::Tag(tag))
                if tag.as_str() == "NaN"
        ) {
            nan_body = Some(&arm.body);
        }
    }
    let Some(nan_body) = nan_body else {
        return Ok(None);
    };
    let Some((alias_name, number_body)) = conditional_alias_arm(arms) else {
        return Ok(None);
    };

    let mut scoped_locals = locals.clone();
    let mut scope = BTreeMap::new();
    scope.insert(
        alias_name.to_string(),
        LocalBinding {
            expr: Some(source),
            object_base: infer_argument_object_base(source, context, locals, passed),
        },
    );
    scoped_locals.push(scope);

    Ok(Some(SemanticInputValue::ParsedTextBindingBranch {
        binding,
        number: Box::new(lower_text_input_value_body(
            number_body,
            context,
            stack,
            &mut scoped_locals,
            passed,
        )?),
        nan: Box::new(lower_text_input_value_body(
            nan_body, context, stack, locals, passed,
        )?),
    }))
}

fn text_to_number_source_binding(
    expression: &StaticSpannedExpression,
    context: &LowerContext<'_>,
    locals: &LocalScopes<'_>,
    passed: &PassedScopes,
    stack: &mut Vec<String>,
) -> Result<Option<String>, String> {
    let expression = resolve_alias(expression, context, locals, passed, stack)?;
    let StaticExpression::Pipe { from, to } = &expression.node else {
        return Ok(None);
    };
    let StaticExpression::FunctionCall { path, arguments } = &to.node else {
        return Ok(None);
    };
    if !path_matches(path, &["Text", "to_number"]) || !arguments.is_empty() {
        return Ok(None);
    }
    Ok(resolve_text_reference(from, context, locals, passed, stack)?.map(|(binding, _)| binding))
}

fn semantic_node_can_render_input_value(node: &SemanticNode) -> bool {
    match node {
        SemanticNode::Fragment(children) => {
            children.iter().all(semantic_node_can_render_input_value)
        }
        SemanticNode::Text(_)
        | SemanticNode::TextTemplate { .. }
        | SemanticNode::TextBindingBranch { .. }
        | SemanticNode::BoolBranch { .. }
        | SemanticNode::ScalarCompareBranch { .. }
        | SemanticNode::ScalarValue { .. }
        | SemanticNode::ObjectScalarCompareBranch { .. }
        | SemanticNode::ObjectBoolFieldBranch { .. }
        | SemanticNode::ObjectTextFieldBranch { .. }
        | SemanticNode::ObjectFieldValue { .. }
        | SemanticNode::ListEmptyBranch { .. } => true,
        SemanticNode::Element { text, children, .. } => {
            text.is_some() || children.iter().all(semantic_node_can_render_input_value)
        }
        SemanticNode::Keyed { node, .. } => semantic_node_can_render_input_value(node),
        SemanticNode::TextList { .. } | SemanticNode::ObjectList { .. } => false,
    }
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
    if let Some(branch) =
        lower_conditional_list_items(expression, context, stack, locals, passed)?
    {
        return Ok(branch);
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
                    let mut nested_locals = locals.clone();
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
                    nested_locals.push(scope);
                    let mut nested_stack = stack.clone();
                    let mut nested_passed = passed.clone();
                    let lowered = lower_ui_node(
                        new,
                        context,
                        &mut nested_stack,
                        &mut nested_locals,
                        &mut nested_passed,
                        None,
                    );
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
                    .map(|item| {
                        let mut nested_stack = stack.clone();
                        let mut nested_locals = locals.clone();
                        let mut nested_passed = passed.clone();
                        lower_ui_node(
                            item,
                            context,
                            &mut nested_stack,
                            &mut nested_locals,
                            &mut nested_passed,
                            None,
                        )
                    })
                    .collect()
            }
        }
        _ => resolve_static_list_items(expression, context, stack, locals, passed)?
            .into_iter()
            .map(|item| {
                let mut nested_stack = stack.clone();
                let mut nested_locals = locals.clone();
                let mut nested_passed = passed.clone();
                lower_ui_node(
                    item,
                    context,
                    &mut nested_stack,
                    &mut nested_locals,
                    &mut nested_passed,
                    None,
                )
            })
            .collect(),
    }
}

fn lower_conditional_list_items<'a>(
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
                lower_conditional_list_items(body, context, stack, locals, passed)
            },
        );
    }
    let expression = resolve_alias(expression, context, locals, passed, stack)?;
    let StaticExpression::Pipe { from, to } = &expression.node else {
        return Ok(None);
    };
    let StaticExpression::While { arms } = &to.node else {
        return Ok(None);
    };
    let Some((truthy, falsy)) = bool_ui_branch_arms(arms)? else {
        return Ok(None);
    };

    let truthy_nodes = lower_list_branch_nodes(truthy, context, stack, locals, passed)?;
    let falsy_nodes = lower_list_branch_nodes(falsy, context, stack, locals, passed)?;
    let Some(branch) = lower_bool_condition_from_nodes(
        from,
        SemanticNode::Fragment(truthy_nodes),
        SemanticNode::Fragment(falsy_nodes),
        context,
        stack,
        locals,
        passed,
    )? else {
        return Ok(None);
    };
    Ok(Some(vec![branch]))
}

fn lower_list_branch_nodes<'a>(
    expression: &'a StaticSpannedExpression,
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<Vec<SemanticNode>, String> {
    let resolved = resolve_alias(expression, context, locals, passed, stack)?;
    if matches!(
        &resolved.node,
        StaticExpression::Literal(static_expression::Literal::Tag(tag))
            if tag.as_str() == "NoElement"
    ) {
        return Ok(Vec::new());
    }
    let mut nested_stack = stack.clone();
    let mut nested_locals = locals.clone();
    let mut nested_passed = passed.clone();
    lower_list_items(
        expression,
        context,
        &mut nested_stack,
        &mut nested_locals,
        &mut nested_passed,
    )
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
            let keep =
                eval_static_bool(condition, context, stack, locals, passed).map_err(|error| {
                    format!(
                        "static List/retain predicate `{}` failed: {error}",
                        describe_expression_detailed(condition)
                    )
                });
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
            if let Some(field_expression) =
                resolve_postfix_field_expression(expr, field, context, locals, passed, stack)?
            {
                return lower_text_value(field_expression, context, stack, locals, passed);
            }
            Err(format!(
                "expected a static text value, found `{}`",
                describe_expression_detailed(expression)
            ))
        }
        StaticExpression::Pipe { from, to } => {
            let to = resolve_alias(to, context, locals, passed, stack)?;
            if let StaticExpression::FieldAccess { path } = &to.node {
                let Some(field) = path.first() else {
                    return Err("field access requires at least one field".to_string());
                };
                if let Some(value) = initial_object_field_scalar_value_in_context(
                    from,
                    field.as_str(),
                    context,
                    stack,
                    locals,
                    passed,
                )? {
                    return Ok(value.to_string());
                }
                if let Some(field_expression) =
                    resolve_postfix_field_expression(from, field, context, locals, passed, stack)?
                {
                    return lower_text_value(field_expression, context, stack, locals, passed);
                }
                return Err(format!(
                    "expected a static text value, found `{}`",
                    describe_expression_detailed(expression)
                ));
            }
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
            if resolved_function_name_for_path_in_stack(path, context, stack).is_some() =>
        {
            let function_name = resolved_function_name_for_path_in_stack(path, context, stack)
                .expect("guard ensured function resolution");
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
            if let Some(value) = initial_object_field_scalar_value_in_context(
                expr,
                field.as_str(),
                context,
                stack,
                locals,
                passed,
            )? {
                return Ok(Some(value));
            }
            if let Some(field_expression) =
                resolve_postfix_field_expression(expr, field, context, locals, passed, stack)?
            {
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
        StaticExpression::ArithmeticOperator(static_expression::ArithmeticOperator::Multiply {
            operand_a,
            operand_b,
        }) => Ok(
            initial_scalar_value_in_context(operand_a, context, stack, locals, passed)?
                .zip(initial_scalar_value_in_context(
                    operand_b, context, stack, locals, passed,
                )?)
                .map(|(a, b)| a * b),
        ),
        StaticExpression::ArithmeticOperator(static_expression::ArithmeticOperator::Divide {
            operand_a,
            operand_b,
        }) => Ok(
            initial_scalar_value_in_context(operand_a, context, stack, locals, passed)?
                .zip(initial_scalar_value_in_context(
                    operand_b, context, stack, locals, passed,
                )?)
                .and_then(|(a, b)| (b != 0).then_some(a / b)),
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
            if resolved_function_name_for_path_in_stack(path, context, stack).is_some() =>
        {
            let function_name = resolved_function_name_for_path_in_stack(path, context, stack)
                .expect("guard ensured function resolution");
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
            let to = resolve_alias(to, context, locals, passed, stack)?;
            if let StaticExpression::FieldAccess { path } = &to.node {
                let Some(field) = path.first() else {
                    return Ok(None);
                };
                if let Some(value) = initial_object_field_scalar_value_in_context(
                    from,
                    field.as_str(),
                    context,
                    stack,
                    locals,
                    passed,
                )? {
                    return Ok(Some(value));
                }
                if let Some(field_expression) =
                    resolve_postfix_field_expression(from, field, context, locals, passed, stack)?
                {
                    return initial_scalar_value_in_context(
                        field_expression,
                        context,
                        stack,
                        locals,
                        passed,
                    );
                }
                return Ok(None);
            }
            if preserves_input_value_pipe(to) {
                return initial_scalar_value_in_context(from, context, stack, locals, passed);
            }
            if matches!(&to.node, StaticExpression::Hold { .. }) {
                return initial_scalar_value_in_context(from, context, stack, locals, passed);
            }
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
            if path_matches(path, &["Math", "min"]) {
                let left = initial_scalar_value_in_context(from, context, stack, locals, passed)?;
                let right = find_named_argument(arguments, "b")
                    .ok_or_else(|| "Math/min requires `b`".to_string())
                    .and_then(|argument| {
                        initial_scalar_value_in_context(argument, context, stack, locals, passed)?
                            .ok_or_else(|| "Math/min requires an initial numeric `b`".to_string())
                    })?;
                return Ok(left.map(|left| left.min(right)));
            }
            if path_matches(path, &["Math", "round"]) && arguments.is_empty() {
                return initial_scalar_value_in_context(from, context, stack, locals, passed);
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

fn initial_object_field_scalar_value_in_context<'a>(
    expression: &'a StaticSpannedExpression,
    field_name: &str,
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<Option<i64>, String> {
    initial_object_field_scalar_value_after_steps(
        expression,
        field_name,
        0,
        context,
        stack,
        locals,
        passed,
    )
}

fn initial_object_field_scalar_value_after_steps<'a>(
    expression: &'a StaticSpannedExpression,
    field_name: &str,
    steps: usize,
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<Option<i64>, String> {
    let mut current = expression;
    let mut total_steps = steps;

    loop {
        let current_resolved = resolve_alias(current, context, locals, passed, stack)?;
        match &current_resolved.node {
            StaticExpression::Object(object) => {
                if total_steps > 0 {
                    return Ok(None);
                }
                let Some(field_expression) = find_object_field(object, field_name) else {
                    return Ok(None);
                };
                return initial_scalar_value_in_context(
                    field_expression,
                    context,
                    stack,
                    locals,
                    passed,
                );
            }
            StaticExpression::Pipe { from, to } => {
                let to = resolve_alias(to, context, locals, passed, stack)?;
                if preserves_input_value_pipe(to) {
                    current = from;
                    continue;
                }
                if let StaticExpression::FunctionCall { path, arguments } = &to.node {
                    if path_matches(path, &["Stream", "skip"]) {
                        let skip_count = find_named_argument(arguments, "count")
                            .map(|argument| {
                                initial_scalar_value_in_context(
                                    argument,
                                    context,
                                    stack,
                                    locals,
                                    passed,
                                )?
                                .ok_or_else(|| {
                                    "Stream/skip requires an initial integer `count`".to_string()
                                })
                            })
                            .transpose()?
                            .unwrap_or_default();
                        let skip_count = usize::try_from(skip_count)
                            .map_err(|_| "Stream/skip count must be non-negative".to_string())?;
                        total_steps += skip_count;
                        current = from;
                        continue;
                    }
                }
                if let StaticExpression::Hold { state_param, body } = &to.node {
                    if total_steps == 0 {
                        current = from;
                        continue;
                    }
                    let Some(mut state) = evaluate_scalar_object_state(
                        from,
                        None,
                        context,
                        stack,
                        locals,
                        passed,
                    )? else {
                        return Ok(None);
                    };
                    for _ in 0..total_steps {
                        let Some(next_state) = evaluate_hold_step_scalar_state(
                            body,
                            state_param.as_str(),
                            &state,
                            context,
                            stack,
                            locals,
                            passed,
                        )? else {
                            return Ok(None);
                        };
                        state = next_state;
                    }
                    return Ok(state.get(field_name).copied());
                }
                return Ok(None);
            }
            _ => return Ok(None),
        }
    }
}

fn evaluate_scalar_object_state<'a>(
    expression: &'a StaticSpannedExpression,
    state_scope: Option<(&str, &BTreeMap<String, i64>)>,
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<Option<BTreeMap<String, i64>>, String> {
    let expression = resolve_alias(expression, context, locals, passed, stack)?;
    match &expression.node {
        StaticExpression::Object(object) => {
            let mut values = BTreeMap::new();
            for variable in &object.variables {
                let name = variable.node.name.as_str();
                if name.is_empty() {
                    continue;
                }
                let Some(value) = evaluate_scalar_expression_with_state_scope(
                    &variable.node.value,
                    state_scope,
                    context,
                    stack,
                    locals,
                    passed,
                )? else {
                    return Ok(None);
                };
                values.insert(name.to_string(), value);
            }
            Ok(Some(values))
        }
        StaticExpression::Pipe { from, to } if preserves_input_value_pipe(to) => {
            evaluate_scalar_object_state(from, state_scope, context, stack, locals, passed)
        }
        _ => Ok(None),
    }
}

fn evaluate_hold_step_scalar_state<'a>(
    body: &'a StaticSpannedExpression,
    state_name: &str,
    state: &BTreeMap<String, i64>,
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<Option<BTreeMap<String, i64>>, String> {
    let body = resolve_alias(body, context, locals, passed, stack)?;
    let object_expression = match &body.node {
        StaticExpression::Then { body } => body.as_ref(),
        StaticExpression::Pipe { to, .. } => {
            let StaticExpression::Then { body } = &to.node else {
                return Ok(None);
            };
            body.as_ref()
        }
        _ => return Ok(None),
    };
    evaluate_scalar_object_state(
        object_expression,
        Some((state_name, state)),
        context,
        stack,
        locals,
        passed,
    )
}

fn evaluate_scalar_expression_with_state_scope<'a>(
    expression: &'a StaticSpannedExpression,
    state_scope: Option<(&str, &BTreeMap<String, i64>)>,
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<Option<i64>, String> {
    if let Some((state_name, state_values)) = state_scope {
        if let Some(value) =
            state_scope_scalar_value(expression, state_name, state_values, context, locals, passed)?
        {
            return Ok(Some(value));
        }
    }

    let expression = resolve_alias(expression, context, locals, passed, stack)?;
    match &expression.node {
        StaticExpression::ArithmeticOperator(static_expression::ArithmeticOperator::Add {
            operand_a,
            operand_b,
        }) => Ok(
            evaluate_scalar_expression_with_state_scope(
                operand_a, state_scope, context, stack, locals, passed,
            )?
            .zip(evaluate_scalar_expression_with_state_scope(
                operand_b, state_scope, context, stack, locals, passed,
            )?)
            .map(|(a, b)| a + b),
        ),
        StaticExpression::ArithmeticOperator(static_expression::ArithmeticOperator::Subtract {
            operand_a,
            operand_b,
        }) => Ok(
            evaluate_scalar_expression_with_state_scope(
                operand_a, state_scope, context, stack, locals, passed,
            )?
            .zip(evaluate_scalar_expression_with_state_scope(
                operand_b, state_scope, context, stack, locals, passed,
            )?)
            .map(|(a, b)| a - b),
        ),
        StaticExpression::ArithmeticOperator(static_expression::ArithmeticOperator::Multiply {
            operand_a,
            operand_b,
        }) => Ok(
            evaluate_scalar_expression_with_state_scope(
                operand_a, state_scope, context, stack, locals, passed,
            )?
            .zip(evaluate_scalar_expression_with_state_scope(
                operand_b, state_scope, context, stack, locals, passed,
            )?)
            .map(|(a, b)| a * b),
        ),
        StaticExpression::ArithmeticOperator(static_expression::ArithmeticOperator::Divide {
            operand_a,
            operand_b,
        }) => Ok(
            evaluate_scalar_expression_with_state_scope(
                operand_a, state_scope, context, stack, locals, passed,
            )?
            .zip(evaluate_scalar_expression_with_state_scope(
                operand_b, state_scope, context, stack, locals, passed,
            )?)
            .and_then(|(a, b)| (b != 0).then_some(a / b)),
        ),
        StaticExpression::Pipe { from, to } if preserves_input_value_pipe(to) => {
            evaluate_scalar_expression_with_state_scope(from, state_scope, context, stack, locals, passed)
        }
        _ => initial_scalar_value_in_context(expression, context, stack, locals, passed),
    }
}

fn state_scope_scalar_value(
    expression: &StaticSpannedExpression,
    state_name: &str,
    state_values: &BTreeMap<String, i64>,
    context: &LowerContext<'_>,
    locals: &LocalScopes<'_>,
    passed: &PassedScopes,
) -> Result<Option<i64>, String> {
    match &expression.node {
        StaticExpression::Alias(static_expression::Alias::WithoutPassed { parts, .. }) => {
            if parts.first().is_some_and(|part| part.as_str() == state_name) {
                if parts.len() == 2 {
                    return Ok(state_values.get(parts[1].as_str()).copied());
                }
                return Ok(None);
            }
            if parts.len() == 1 {
                if let Some(local_expression) = lookup_local_binding_expr(parts[0].as_str(), locals) {
                    return state_scope_scalar_value(
                        local_expression,
                        state_name,
                        state_values,
                        context,
                        locals,
                        passed,
                    );
                }
            }
            Ok(None)
        }
        StaticExpression::PostfixFieldAccess { expr, field } => {
            if let StaticExpression::Alias(static_expression::Alias::WithoutPassed { parts, .. }) =
                &expr.node
            {
                if parts.len() == 1 && parts[0].as_str() == state_name {
                    return Ok(state_values.get(field.as_str()).copied());
                }
            }
            let _ = (context, passed);
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
    if let Some(list_ref) = runtime_object_list_ref(expression, context, locals, passed, stack)? {
        return Ok(Some(
            context
                .object_list_plan
                .initial_values
                .get(&list_ref.binding)
                .map(|items| match &list_ref.filter {
                    Some(filter) => items
                        .iter()
                        .filter(|item| initial_object_list_item_matches_filter(item, filter, context))
                        .count(),
                    None => items.len(),
                })
                .unwrap_or_default() as i64,
        ));
    }
    if let Some(list_ref) = runtime_text_list_ref(expression, context, locals, passed, stack)? {
        return Ok(Some(
            context
                .list_plan
                .initial_values
                .get(&list_ref.binding)
                .map(|values| filtered_runtime_text_list_values(values, list_ref.filter.as_ref()).len())
                .unwrap_or_default() as i64,
        ));
    }
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
        tag: infer_tag(element, "span", context, stack, locals, passed)?,
        properties: collect_common_properties(element, context, stack, locals, passed)?,
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
                | SemanticTextPart::DerivedScalarExpr(_)
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
                if let Some(binding_path) = text_binding_path(var.as_str(), context, locals, passed)
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
                if let Some(field) = placeholder_object_field(expression, context, locals, passed)?
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
                if let Some(part) =
                    scalar_value_text_part(expression, context, stack, locals, passed)?
                {
                    stack.pop();
                    output.push(part);
                    continue;
                }
                let part = if let Some((binding, _)) =
                    resolve_scalar_reference(expression, context, locals, passed, stack)?
                {
                    SemanticTextPart::ScalarBinding(binding)
                } else if let Some(expr) =
                    derived_scalar_operand_in_context(expression, context, locals, passed, stack)?
                {
                    SemanticTextPart::DerivedScalarExpr(expr)
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

fn scalar_value_text_part<'a>(
    expression: &'a StaticSpannedExpression,
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<Option<SemanticTextPart>, String> {
    let expression = resolve_alias(expression, context, locals, passed, stack)?;
    let StaticExpression::Pipe { from, to } = &expression.node else {
        return Ok(None);
    };
    let (StaticExpression::When { arms } | StaticExpression::While { arms }) = &to.node else {
        return Ok(None);
    };

    let expr = if let Some((binding, _)) =
        resolve_scalar_reference(from, context, locals, passed, stack)?
    {
        DerivedScalarOperand::Binding(binding)
    } else if let Some(expr) =
        derived_scalar_operand_in_context(from, context, locals, passed, stack)?
    {
        expr
    } else {
        return Ok(None);
    };

    let mut branch = None;
    let mut fallback = None;
    for arm in arms {
        match &arm.pattern {
            static_expression::Pattern::Literal(static_expression::Literal::Number(number)) => {
                if number.fract() != 0.0 {
                    return Ok(None);
                }
                let value = *number as i64;
                if branch.is_some() {
                    return Ok(None);
                }
                branch = Some((
                    value,
                    lower_text_value(&arm.body, context, stack, locals, passed)?,
                ));
            }
            static_expression::Pattern::WildCard => {
                fallback = Some(lower_text_value(&arm.body, context, stack, locals, passed)?);
            }
            _ => return Ok(None),
        }
    }

    let Some((expected, true_text)) = branch else {
        return Ok(None);
    };
    let Some(false_text) = fallback else {
        return Ok(None);
    };

    Ok(Some(SemanticTextPart::ScalarValueText {
        expr,
        expected,
        true_text,
        false_text,
    }))
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
            SemanticTextPart::DerivedScalarExpr(expr) => {
                output.push_str(&initial_derived_scalar_operand_value(expr, context).to_string())
            }
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
            SemanticTextPart::ScalarValueText {
                expr,
                expected,
                true_text,
                false_text,
            } => output.push_str(
                if initial_derived_scalar_operand_value(expr, context) == *expected {
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

fn initial_derived_scalar_operand_value(
    operand: &DerivedScalarOperand,
    context: &LowerContext<'_>,
) -> i64 {
    match operand {
        DerivedScalarOperand::Binding(binding) => context
            .scalar_plan
            .initial_values
            .get(binding)
            .copied()
            .unwrap_or_default(),
        DerivedScalarOperand::TextBindingNumber(binding) => context
            .text_plan
            .initial_values
            .get(binding)
            .and_then(|value| value.trim().parse::<i64>().ok())
            .unwrap_or_default(),
        DerivedScalarOperand::TextListCount { binding, filter } => context
            .list_plan
            .initial_values
            .get(binding)
            .map(|values| filtered_runtime_text_list_values(values, filter.as_ref()).len() as i64)
            .unwrap_or_default(),
        DerivedScalarOperand::ObjectListCount { binding, filter } => context
            .object_list_plan
            .initial_values
            .get(binding)
            .map(|items| {
                items.iter()
                    .filter(|item| {
                        filter
                            .as_ref()
                            .is_none_or(|filter| initial_object_list_item_matches_filter(item, filter, context))
                    })
                    .count() as i64
            })
            .unwrap_or_default(),
        DerivedScalarOperand::Literal(value) => *value,
        DerivedScalarOperand::Arithmetic { op, left, right } => {
            let left = initial_derived_scalar_operand_value(left, context);
            let right = initial_derived_scalar_operand_value(right, context);
            match op {
                DerivedArithmeticOp::Add => left + right,
                DerivedArithmeticOp::Subtract => left - right,
                DerivedArithmeticOp::Multiply => left * right,
                DerivedArithmeticOp::Divide => {
                    if right == 0 {
                        0
                    } else {
                        left / right
                    }
                }
            }
        }
        DerivedScalarOperand::Min { left, right } => {
            initial_derived_scalar_operand_value(left, context)
                .min(initial_derived_scalar_operand_value(right, context))
        }
        DerivedScalarOperand::Round { source } => {
            initial_derived_scalar_operand_value(source, context)
        }
    }
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
    let (StaticExpression::When { arms } | StaticExpression::While { arms }) = &to.node else {
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
        if let Some(field) = local_object_field_name(parts[0].as_str(), context, locals, passed)? {
            return Ok(Some(field));
        }
    }
    let path = canonical_without_passed_path(parts, context, locals, passed)?;
    if path.contains(".event.") {
        return Ok(None);
    }
    Ok(path.strip_prefix("__item__.").map(ToString::to_string))
}

fn placeholder_object_field_name(
    name: &str,
    context: &LowerContext<'_>,
    locals: &LocalScopes<'_>,
    passed: &PassedScopes,
) -> Option<String> {
    if let Ok(Some(field)) = local_object_field_name(name, context, locals, passed) {
        return Some(field);
    }
    let (first, rest) = name.split_once('.')?;
    if rest.contains(".event.") {
        return None;
    }
    let base = lookup_local_object_base(first, locals)?;
    (base == "__item__").then_some(rest.to_string())
}

fn local_object_field_name(
    name: &str,
    context: &LowerContext<'_>,
    locals: &LocalScopes<'_>,
    passed: &PassedScopes,
) -> Result<Option<String>, String> {
    let base = lookup_local_object_base(name, locals);
    if base != Some("__item__") {
        return Ok(None);
    }
    let Some(expression) = lookup_local_binding_expr(name, locals) else {
        return Ok(None);
    };
    if matches!(name, "formula_text" | "display_value" | "input_text")
        && expression_depends_on_item_scope(expression, context, locals, passed)
    {
        return Ok(Some(name.to_string()));
    }
    direct_local_object_field_name(expression, context, locals, passed)
}

fn direct_local_object_field_name(
    expression: &StaticSpannedExpression,
    context: &LowerContext<'_>,
    locals: &LocalScopes<'_>,
    passed: &PassedScopes,
) -> Result<Option<String>, String> {
    let expression = resolve_alias(expression, context, locals, passed, &mut Vec::new())?;
    match &expression.node {
        StaticExpression::Alias(static_expression::Alias::WithoutPassed { parts, .. }) => {
            let path = canonical_without_passed_path(parts, context, locals, passed)?;
            if path.contains(".event.") {
                return Ok(None);
            }
            Ok(path.strip_prefix("__item__.").map(ToString::to_string))
        }
        StaticExpression::Pipe { from, to } if matches!(to.node, StaticExpression::Hold { .. }) => {
            direct_local_object_field_name(from, context, locals, passed)
        }
        StaticExpression::Latest { inputs } => {
            for input in inputs {
                if let Some(field) = direct_local_object_field_name(input, context, locals, passed)?
                {
                    return Ok(Some(field));
                }
            }
            Ok(None)
        }
        StaticExpression::Block { output, .. } => {
            direct_local_object_field_name(output, context, locals, passed)
        }
        _ => Ok(None),
    }
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
    let (StaticExpression::When { arms } | StaticExpression::While { arms }) = &to.node else {
        return Ok(None);
    };
    let Some((true_text, false_text)) = bool_text_arms(arms, context, stack, locals, passed)?
    else {
        return Ok(None);
    };
    if let Some(binding) = bool_binding_path(from, context, locals, passed, stack)? {
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
        return Ok(Some(SemanticNode::text_template(
            vec![SemanticTextPart::BoolBindingText {
                binding,
                true_text,
                false_text,
            }],
            current,
        )));
    }

    lower_bool_condition_from_nodes(
        expression,
        SemanticNode::text(true_text),
        SemanticNode::text(false_text),
        context,
        stack,
        locals,
        passed,
    )
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
    let (StaticExpression::When { arms } | StaticExpression::While { arms }) = &to.node else {
        return Ok(None);
    };

    if let [arm] = arms.as_slice() {
        if let static_expression::Pattern::Alias { name } = &arm.pattern {
            if let StaticExpression::Block { variables, output } = &arm.body.node {
                let specialized_block = synthetic_alias_bound_block_expression(
                    name,
                    from.as_ref(),
                    variables,
                    output,
                    &arm.body,
                );
                return lower_ui_node(specialized_block, context, stack, locals, passed, None)
                    .map(Some);
            }
            let mut bindings = BTreeMap::new();
            bindings.insert(name.as_str().to_string(), from.as_ref());
            let specialized_body = specialize_static_expression(&arm.body, &bindings);
            let mut scope = BTreeMap::new();
            scope.insert(
                name.as_str().to_string(),
                LocalBinding {
                    expr: Some(from),
                    object_base: infer_argument_object_base(from, context, locals, passed),
                },
            );
            locals.push(scope);
            let lowered = lower_ui_node(specialized_body, context, stack, locals, passed, None);
            locals.pop();
            return lowered.map(Some);
        }
    }

    if let Some(dynamic_branch) =
        lower_scalar_value_when_node(from, arms, context, stack, locals, passed)?
    {
        return Ok(Some(dynamic_branch));
    }

    let Some(selected_body) = select_when_arm_body(from, arms, context, stack, locals, passed)?
    else {
        return Ok(None);
    };
    lower_ui_node(selected_body, context, stack, locals, passed, None).map(Some)
}

fn lower_scalar_value_when_node<'a>(
    source: &'a StaticSpannedExpression,
    arms: &'a [static_expression::Arm],
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<Option<SemanticNode>, String> {
    let Some(source_binding) = resolve_scalar_reference(source, context, locals, passed, stack)?
        .map(|(binding, _)| binding)
        .or_else(|| canonical_expression_path(source, context, locals, passed, stack).ok())
    else {
        return Ok(None);
    };
    let source_expression = context.path_bindings.get(&source_binding).copied();
    let binding_is_scalar = scalar_binding_has_runtime_state(&source_binding, context);
    if !binding_is_scalar {
        return Ok(None);
    }

    let mut branches = Vec::new();
    let mut fallback = None;
    for arm in arms {
        match &arm.pattern {
            static_expression::Pattern::WildCard => {
                fallback = Some(lower_ui_node(
                    &arm.body, context, stack, locals, passed, None,
                )?);
            }
            _ => {
                let Some(expected_value) =
                    scalar_branch_pattern_value_for_source(&arm.pattern, source_expression)?
                else {
                    return Ok(None);
                };
                branches.push((
                    expected_value,
                    lower_ui_node(&arm.body, context, stack, locals, passed, None)?,
                ));
            }
        }
    }

    if branches.is_empty() {
        return Ok(None);
    }

    if fallback.is_none() {
        let Some(all_values) = source_expression
            .map(scalar_branch_all_values_for_source)
            .transpose()?
            .flatten()
        else {
            return Ok(None);
        };
        let branch_values = branches
            .iter()
            .map(|(value, _)| *value)
            .collect::<BTreeSet<_>>();
        if !all_values.iter().all(|value| branch_values.contains(value)) {
            return Ok(None);
        }
        fallback = branches.pop().map(|(_, node)| node);
    }

    let mut node = fallback.expect("fallback should exist after exhaustive branch reduction");
    for (expected_value, branch_node) in branches.into_iter().rev() {
        node = SemanticNode::scalar_compare_branch(
            DerivedScalarOperand::Binding(source_binding.clone()),
            IntCompareOp::Equal,
            DerivedScalarOperand::Literal(expected_value),
            branch_node,
            node,
        );
    }
    Ok(Some(node))
}

fn derived_scalar_target(spec: &DerivedScalarSpec) -> &str {
    match spec {
        DerivedScalarSpec::TextListCount { target, .. }
        | DerivedScalarSpec::ObjectListCount { target, .. }
        | DerivedScalarSpec::Arithmetic { target, .. }
        | DerivedScalarSpec::TextValueBranch { target, .. }
        | DerivedScalarSpec::Comparison { target, .. }
        | DerivedScalarSpec::TextComparison { target, .. } => target,
    }
}

fn scalar_branch_pattern_value_for_source(
    pattern: &static_expression::Pattern,
    source_expression: Option<&StaticSpannedExpression>,
) -> Result<Option<i64>, String> {
    match pattern {
        static_expression::Pattern::Literal(static_expression::Literal::Number(number)) => {
            if number.fract() == 0.0 {
                Ok(Some(*number as i64))
            } else {
                Ok(None)
            }
        }
        static_expression::Pattern::Literal(static_expression::Literal::Tag(tag))
            if tag.as_str() == "True" =>
        {
            Ok(Some(1))
        }
        static_expression::Pattern::Literal(static_expression::Literal::Tag(tag))
            if tag.as_str() == "False" =>
        {
            Ok(Some(0))
        }
        static_expression::Pattern::Literal(static_expression::Literal::Tag(tag)) => {
            scalar_tag_pattern_value_for_source(tag.as_str(), source_expression)
        }
        static_expression::Pattern::Literal(static_expression::Literal::Text(text)) => {
            Ok(source_expression
                .map(|expression| text_branch_output_scalar_for_name(expression, text.as_str()))
                .transpose()?
                .flatten())
        }
        _ => Ok(None),
    }
}

fn scalar_tag_pattern_value_for_source(
    expected_name: &str,
    source_expression: Option<&StaticSpannedExpression>,
) -> Result<Option<i64>, String> {
    if let Some(value) = source_expression
        .map(|expression| hold_tag_toggle_scalar_value_for_name(expression, expected_name))
        .transpose()?
        .flatten()
    {
        return Ok(Some(value));
    }
    if let Some(value) = source_expression
        .map(|expression| latest_tag_scalar_value_for_name(expression, expected_name))
        .transpose()?
        .flatten()
    {
        return Ok(Some(value));
    }
    if let Some(value) = match expected_name {
        "All" => Some(0),
        "Active" => Some(1),
        "Completed" => Some(2),
        _ => None,
    } {
        return Ok(Some(value));
    }
    Ok(source_expression
        .map(|expression| text_branch_output_scalar_for_name(expression, expected_name))
        .transpose()?
        .flatten())
}

fn scalar_branch_all_values_for_source(
    expression: &StaticSpannedExpression,
) -> Result<Option<BTreeSet<i64>>, String> {
    if let Some(mapping) = hold_tag_toggle_scalar_values_for_expression(expression)? {
        return Ok(Some(mapping.into_values().collect()));
    }
    if let Some(mapping) = latest_tag_scalar_values_for_expression(expression)? {
        return Ok(Some(mapping.into_values().collect()));
    }
    let StaticExpression::Pipe { to, .. } = &expression.node else {
        return Ok(None);
    };
    let (StaticExpression::When { arms } | StaticExpression::While { arms }) = &to.node else {
        return Ok(None);
    };
    let mut dynamic_values = BTreeMap::new();
    let mut values = BTreeSet::new();
    for arm in arms {
        let Some((_name, value)) = text_branch_body_scalar_value(&arm.body, &mut dynamic_values)?
        else {
            return Ok(None);
        };
        values.insert(value);
    }
    Ok(Some(values))
}

fn hold_tag_toggle_scalar_value_for_name(
    expression: &StaticSpannedExpression,
    expected_name: &str,
) -> Result<Option<i64>, String> {
    Ok(hold_tag_toggle_scalar_values_for_expression(expression)?
        .and_then(|mapping| mapping.get(expected_name).copied()))
}

fn latest_tag_scalar_value_for_name(
    expression: &StaticSpannedExpression,
    expected_name: &str,
) -> Result<Option<i64>, String> {
    Ok(latest_tag_scalar_values_for_expression(expression)?
        .and_then(|mapping| mapping.get(expected_name).copied()))
}

fn text_branch_output_scalar_for_name(
    expression: &StaticSpannedExpression,
    expected_name: &str,
) -> Result<Option<i64>, String> {
    let StaticExpression::Pipe { to, .. } = &expression.node else {
        return Ok(None);
    };
    let (StaticExpression::When { arms } | StaticExpression::While { arms }) = &to.node else {
        return Ok(None);
    };
    let mut dynamic_values = BTreeMap::new();
    for arm in arms {
        let Some((name, value)) = text_branch_body_scalar_value(&arm.body, &mut dynamic_values)?
        else {
            return Ok(None);
        };
        if name == expected_name {
            return Ok(Some(value));
        }
    }
    Ok(None)
}

fn select_when_arm_body<'a>(
    source: &'a StaticSpannedExpression,
    arms: &'a [static_expression::Arm],
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<Option<&'a StaticSpannedExpression>, String> {
    for arm in arms {
        match &arm.pattern {
            static_expression::Pattern::Alias { name }
                if initial_when_value_matches_tag(
                    source,
                    name.as_str(),
                    context,
                    stack,
                    locals,
                    passed,
                )? =>
            {
                return Ok(Some(&arm.body));
            }
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
    if expected != "True" && expected != "False" {
        if let Some((binding, value)) =
            resolve_scalar_reference(expression, context, locals, passed, stack)?
        {
            let source_expression = context.path_bindings.get(&binding).copied();
            if let Some(expected_value) =
                scalar_tag_pattern_value_for_source(expected, source_expression)?
            {
                return Ok(value == expected_value);
            }
        }
    }
    let expression = resolve_alias(expression, context, locals, passed, stack)?;
    if let StaticExpression::Pipe { from, to } = &expression.node {
        if let StaticExpression::When { arms } | StaticExpression::While { arms } = &to.node {
            if let Some(body) = select_when_arm_body(from, arms, context, stack, locals, passed)? {
                return initial_when_value_matches_tag(
                    body, expected, context, stack, locals, passed,
                );
            }
        }
    }
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
    if let Some((binding, value)) = resolve_scalar_reference(expression, context, locals, passed, stack)?
    {
        let source_expression = context.path_bindings.get(&binding).copied();
        if let Some(expected_value) =
            scalar_tag_pattern_value_for_source(expected, source_expression)?
        {
            return Ok(value == expected_value);
        }
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
    if let Ok(path) = canonical_expression_path(expression, context, locals, passed, &mut Vec::new())
    {
        if scalar_binding_has_runtime_state(&path, context) {
            return Ok(Some(path));
        }
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
    if parts.first().map(boon::parser::StrSlice::as_str) != Some("element") {
        return None;
    }
    let suffix = parts[1..]
        .iter()
        .map(boon::parser::StrSlice::as_str)
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
                true_text = match lower_ui_node(&arm.body, context, stack, locals, passed, None)? {
                    SemanticNode::Text(text) => Some(text),
                    _ => None,
                };
            }
            static_expression::Pattern::Literal(static_expression::Literal::Tag(tag))
                if tag.as_str() == "False" =>
            {
                false_text = match lower_ui_node(&arm.body, context, stack, locals, passed, None)? {
                    SemanticNode::Text(text) => Some(text),
                    _ => None,
                };
            }
            static_expression::Pattern::WildCard => {
                false_text = match lower_ui_node(&arm.body, context, stack, locals, passed, None)? {
                    SemanticNode::Text(text) => Some(text),
                    _ => None,
                };
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
        ObjectListFilter::TextFieldStartsWithTextBinding { field, binding } => {
            let prefix = context
                .text_plan
                .initial_values
                .get(binding)
                .cloned()
                .unwrap_or_default();
            initial_object_item_text_field(item, field).starts_with(prefix.as_str())
        }
        ObjectListFilter::ItemIdEqualsScalarBinding { binding } => context
            .scalar_plan
            .initial_values
            .get(binding)
            .is_some_and(|value| *value == item.id as i64),
    }
}

fn initial_object_item_text_field(item: &ObjectListItem, field: &str) -> String {
    match field {
        "title" => item.title.clone(),
        other => item.text_fields.get(other).cloned().unwrap_or_default(),
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
    if let Some(filter) =
        runtime_object_text_starts_with_filter(expression, item_name, context, locals, passed)?
    {
        return Ok(Some(filter));
    }
    let StaticExpression::Pipe { from, to } = &expression.node else {
        return Ok(None);
    };
    let resolved_from = resolve_alias(from, context, locals, &mut passed.clone(), &mut Vec::new())
        .unwrap_or(from);
    let selector_binding = canonical_expression_path(from, context, locals, passed, &mut Vec::new())
        .ok()
        .or_else(|| {
            canonical_expression_path(resolved_from, context, locals, passed, &mut Vec::new()).ok()
        })
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
                            .map(boon::parser::StrSlice::as_str)
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
                                .map(boon::parser::StrSlice::as_str)
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
                if path == "__item__"
                    || path == "__element__"
                    || path.starts_with("__item__.")
                    || path.starts_with("__element__.")
                {
                    return Ok(None);
                }
                if path.contains(".event.") {
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
    unique_suffix_key(suffix, path_bindings.keys())
}

fn unique_suffix_key<'a>(
    suffix: &str,
    mut keys: impl Iterator<Item = &'a String>,
) -> Option<String> {
    let dotted_suffix = format!(".{suffix}");
    let mut matches = keys
        .by_ref()
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
    locals.iter().rev().find_map(|scope| {
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
    extra_parts: &[boon::parser::StrSlice],
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
    extra_parts: &[boon::parser::StrSlice],
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
    extra_parts: &[boon::parser::StrSlice],
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
            if preserves_input_value_pipe(to) {
                return resolve_local_field_expression_with_scopes(
                    from,
                    extra_parts,
                    context,
                    locals,
                    passed,
                    stack,
                );
            }
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
            if resolved_function_name_for_path_in_stack(path, context, stack).is_some() =>
        {
            let function_name = resolved_function_name_for_path_in_stack(path, context, stack)
                .expect("guard ensured function resolution");
            let function = context
                .functions
                .get(function_name)
                .ok_or_else(|| format!("unknown function `{function_name}`"))?;
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
    extra_parts: &[boon::parser::StrSlice],
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
    let Some(function_name) = resolved_function_name_for_path(path, context) else {
        return Ok(None);
    };
    let Some(function) = context.functions.get(function_name) else {
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
    field: &boon::parser::StrSlice,
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
    parts: &[boon::parser::StrSlice],
    context: &LowerContext<'_>,
    locals: &LocalScopes<'_>,
    passed: &PassedScopes,
) -> Result<String, String> {
    if parts.is_empty() {
        return Err("empty alias path is not supported".to_string());
    }
    let joined = parts
        .iter()
        .map(boon::parser::StrSlice::as_str)
        .collect::<Vec<_>>()
        .join(".");
    if parts[0].as_str() == "element" {
        if parts.len() == 1 {
            return Ok("__element__".to_string());
        }
        let rest = parts[1..]
            .iter()
            .map(boon::parser::StrSlice::as_str)
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
            .map(boon::parser::StrSlice::as_str)
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
    extra_parts: &[boon::parser::StrSlice],
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
    extra_parts: &[boon::parser::StrSlice],
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
                    .map(boon::parser::StrSlice::as_str)
                    .collect::<Vec<_>>()
                    .join(".")
            ))
        }
        PassedScope::Bindings(bindings) => {
            let Some((first, rest)) = extra_parts.split_first() else {
                return Err("object PASS requires a named PASSED entry".to_string());
            };
            let Some(base) = bindings.get(first.as_str()) else {
                let available = bindings.keys().cloned().collect::<Vec<_>>().join(", ");
                return Err(format!(
                    "unknown PASSED binding `{}` in object PASS (available: [{}])",
                    first.as_str(),
                    available
                ));
            };
            if rest.is_empty() {
                Ok(base.clone())
            } else {
                Ok(format!(
                    "{base}.{}",
                    rest.iter()
                        .map(boon::parser::StrSlice::as_str)
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
        let has_local_expr_shadow = parts.len() == 1
            && lookup_local_binding_expr(parts[0].as_str(), locals).is_some()
            && lookup_local_object_base(parts[0].as_str(), locals).is_none();
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
                runtime_text_binding_path(expression, context, locals, passed)
                    .ok()
                    .flatten()
                    .or_else(|| {
                        canonical_expression_path(
                            expression,
                            context,
                            locals,
                            passed,
                            &mut Vec::new(),
                        )
                        .ok()
                        .filter(|path| context.text_plan.initial_values.contains_key(path))
                    })
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

fn runtime_text_binding_path(
    expression: &StaticSpannedExpression,
    context: &LowerContext<'_>,
    locals: &LocalScopes<'_>,
    passed: &PassedScopes,
) -> Result<Option<String>, String> {
    match &expression.node {
        StaticExpression::Alias(static_expression::Alias::WithoutPassed { parts, .. }) => {
            if parts.len() == 1 {
                if let Some(local_expression) = lookup_local_binding_expr(parts[0].as_str(), locals)
                {
                    return runtime_text_binding_path(local_expression, context, locals, passed);
                }
                if let Some(binding) = text_binding_path(parts[0].as_str(), context, locals, passed)
                {
                    return Ok(Some(binding));
                }
            }
            let path = canonical_without_passed_path(parts, context, locals, passed)?;
            Ok(
                (context.text_plan.initial_values.contains_key(&path) || path.ends_with(".text"))
                    .then_some(path),
            )
        }
        StaticExpression::Alias(static_expression::Alias::WithPassed { extra_parts }) => {
            let path = canonical_passed_path(extra_parts, passed)?;
            Ok(
                (context.text_plan.initial_values.contains_key(&path) || path.ends_with(".text"))
                    .then_some(path),
            )
        }
        _ => Ok(None),
    }
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
        let has_local_expr_shadow = parts.len() == 1
            && lookup_local_binding_expr(parts[0].as_str(), locals).is_some()
            && lookup_local_object_base(parts[0].as_str(), locals).is_none();
        if !has_local_expr_shadow {
            let path = canonical_without_passed_path(parts, context, locals, passed)?;
            if let Some(value) = context.text_plan.initial_values.get(&path) {
                return Ok(Some((path, value.clone())));
            }
        }
    }
    if let StaticExpression::Alias(static_expression::Alias::WithPassed { extra_parts }) =
        &expression.node
    {
        let path = canonical_passed_path(extra_parts, passed)?;
        if let Some(value) = context.text_plan.initial_values.get(&path) {
            return Ok(Some((path, value.clone())));
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
    let resolved = resolve_alias(expression, context, locals, passed, stack)?;
    let path = canonical_expression_path(expression, context, locals, passed, stack)
        .ok()
        .or_else(|| {
            canonical_expression_path(resolved, context, locals, passed, &mut Vec::new()).ok()
        });
    let path = path.and_then(|path| {
        context
            .list_plan
            .initial_values
            .contains_key(&path)
            .then_some(path.clone())
            .or_else(|| unique_suffix_key(&path, context.list_plan.initial_values.keys()))
    });
    if path.is_some() {
        return Ok(path);
    }
    if let StaticExpression::Pipe { from, to } = &resolved.node {
        if list_pipe_preserves_source_binding(&to.node) {
            return runtime_list_binding_path(from, context, locals, passed, stack);
        }
    }
    Ok(path)
}

fn runtime_object_list_binding_path(
    expression: &StaticSpannedExpression,
    context: &LowerContext<'_>,
    locals: &LocalScopes<'_>,
    passed: &PassedScopes,
    stack: &mut Vec<String>,
) -> Result<Option<String>, String> {
    if let StaticExpression::Alias(static_expression::Alias::WithoutPassed { parts, .. }) =
        &expression.node
    {
        if parts.len() == 1 {
            if let Some(base) = lookup_local_object_base(parts[0].as_str(), locals) {
                if base.starts_with("__item__.") {
                    return Ok(Some(base.to_string()));
                }
                if context.object_list_plan.initial_values.contains_key(base) {
                    return Ok(Some(base.to_string()));
                }
                if let Some(binding) =
                    unique_suffix_key(base, context.object_list_plan.initial_values.keys())
                {
                    return Ok(Some(binding));
                }
            }
        }
    }
    if let Some(field) = placeholder_object_field(expression, context, locals, passed)? {
        return Ok(Some(format!("__item__.{field}")));
    }
    let resolved = resolve_alias(expression, context, locals, passed, stack)?;
    if let Some(field) = placeholder_object_field(resolved, context, locals, passed)? {
        return Ok(Some(format!("__item__.{field}")));
    }
    let path = canonical_expression_path(expression, context, locals, passed, stack)
        .ok()
        .or_else(|| {
            canonical_expression_path(resolved, context, locals, passed, &mut Vec::new()).ok()
        });
    let path = path.and_then(|path| {
        if path.starts_with("__item__.") {
            return Some(path);
        }
        context
            .object_list_plan
            .initial_values
            .contains_key(&path)
            .then_some(path.clone())
            .or_else(|| unique_suffix_key(&path, context.object_list_plan.initial_values.keys()))
    });
    if path.is_some() {
        return Ok(path);
    }
    if let StaticExpression::Pipe { from, to } = &resolved.node {
        if object_list_pipe_preserves_source_binding(&to.node) {
            return runtime_object_list_binding_path(from, context, locals, passed, stack);
        }
    }
    Ok(path)
}

fn list_pipe_preserves_source_binding(expression: &StaticExpression) -> bool {
    match expression {
        StaticExpression::Hold { .. } => true,
        StaticExpression::FunctionCall { path, .. } => {
            path_matches(path, &["List", "map"])
                || path_matches(path, &["List", "remove"])
                || path_matches(path, &["List", "append"])
        }
        _ => false,
    }
}

fn object_list_pipe_preserves_source_binding(expression: &StaticExpression) -> bool {
    match expression {
        StaticExpression::Hold { .. } => true,
        StaticExpression::FunctionCall { path, .. } => {
            path_matches(path, &["List", "any"])
                || path_matches(path, &["List", "every"])
                || path_matches(path, &["List", "retain"])
                || path_matches(path, &["List", "remove_last"])
                || path_matches(path, &["List", "remove"])
                || path_matches(path, &["List", "append"])
        }
        _ => false,
    }
}

fn runtime_object_list_ref(
    expression: &StaticSpannedExpression,
    context: &LowerContext<'_>,
    locals: &LocalScopes<'_>,
    passed: &PassedScopes,
    stack: &mut Vec<String>,
) -> Result<Option<RuntimeObjectListRef>, String> {
    let resolved = resolve_alias(expression, context, locals, passed, stack)?;
    for candidate in [expression, resolved] {
        if let StaticExpression::Pipe { from, to } = &candidate.node {
            let StaticExpression::FunctionCall { path, arguments } = &to.node else {
                continue;
            };
            if path_matches(path, &["List", "retain"]) {
                let Some(binding) =
                    runtime_object_list_binding_path(from, context, locals, passed, stack)?
                else {
                    continue;
                };
                let item_name = find_positional_parameter_name(arguments)
                    .ok_or_else(|| "List/retain requires an item parameter name".to_string())?;
                let condition = find_named_argument(arguments, "if")
                    .ok_or_else(|| "List/retain requires `if`".to_string())?;
                if matches!(
                    &condition.node,
                    StaticExpression::Literal(static_expression::Literal::Tag(tag))
                        if tag.as_str() == "True"
                ) {
                    return Ok(Some(RuntimeObjectListRef {
                        binding,
                        filter: None,
                    }));
                }
                let Some(filter) =
                    runtime_object_list_filter(condition, item_name, context, locals, passed)?
                else {
                    continue;
                };
                return Ok(Some(RuntimeObjectListRef {
                    binding,
                    filter: Some(filter),
                }));
            }
        }
    }
    if let Some(binding) = runtime_object_list_binding_path(expression, context, locals, passed, stack)?
    {
        return Ok(Some(RuntimeObjectListRef {
            binding,
            filter: None,
        }));
    }
    if let Some(binding) = runtime_object_list_binding_path(resolved, context, locals, passed, stack)?
    {
        return Ok(Some(RuntimeObjectListRef {
            binding,
            filter: None,
        }));
    }
    Ok(None)
}

fn runtime_text_list_ref(
    expression: &StaticSpannedExpression,
    context: &LowerContext<'_>,
    locals: &LocalScopes<'_>,
    passed: &PassedScopes,
    stack: &mut Vec<String>,
) -> Result<Option<RuntimeTextListRef>, String> {
    let resolved = resolve_alias(expression, context, locals, passed, stack)?;
    for candidate in [expression, resolved] {
        if let StaticExpression::Pipe { from, to } = &candidate.node {
            let StaticExpression::FunctionCall { path, arguments } = &to.node else {
                continue;
            };
            if path_matches(path, &["List", "retain"]) {
                let Some(binding) = runtime_list_binding_path(from, context, locals, passed, stack)?
                else {
                    continue;
                };
                let item_name = find_positional_parameter_name(arguments)
                    .ok_or_else(|| "List/retain requires an item parameter name".to_string())?;
                let condition = find_named_argument(arguments, "if")
                    .ok_or_else(|| "List/retain requires `if`".to_string())?;
                let condition = resolve_alias(condition, context, locals, passed, stack)?;
                if matches!(
                    &condition.node,
                    StaticExpression::Literal(static_expression::Literal::Tag(tag))
                        if tag.as_str() == "True"
                ) {
                    return Ok(Some(RuntimeTextListRef {
                        binding,
                        filter: None,
                    }));
                }
                let filter = runtime_text_list_filter(condition, item_name)?;
                return Ok(Some(RuntimeTextListRef {
                    binding,
                    filter: Some(filter),
                }));
            }
        }
    }
    if let Some(binding) = runtime_list_binding_path(expression, context, locals, passed, stack)? {
        return Ok(Some(RuntimeTextListRef {
            binding,
            filter: None,
        }));
    }
    if let Some(binding) = runtime_list_binding_path(resolved, context, locals, passed, stack)? {
        return Ok(Some(RuntimeTextListRef {
            binding,
            filter: None,
        }));
    }
    Ok(None)
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

fn resolve_element_object<'a>(
    element: Option<&'a StaticSpannedExpression>,
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<Option<&'a StaticObject>, String> {
    let Some(element) = element else {
        return Ok(None);
    };
    Ok(resolve_object(resolve_alias(
        element, context, locals, passed, stack,
    )?))
}

fn infer_tag<'a>(
    element: Option<&'a StaticSpannedExpression>,
    default_tag: &str,
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<String, String> {
    Ok(resolve_element_object(element, context, stack, locals, passed)?
        .and_then(|object| find_object_field(object, "tag"))
        .and_then(extract_tag_name)
        .map_or_else(|| default_tag.to_string(), |tag| tag.to_lowercase()))
}

fn collect_common_properties<'a>(
    element: Option<&'a StaticSpannedExpression>,
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<Vec<(String, String)>, String> {
    let Some(object) = resolve_element_object(element, context, stack, locals, passed)? else {
        return Ok(Vec::new());
    };
    let mut properties = Vec::new();
    if let Some(role) = find_object_field(object, "role").and_then(extract_tag_name) {
        properties.push(("role".to_string(), role.to_string()));
    }
    Ok(properties)
}

fn collect_event_bindings<'a>(
    element: Option<&'a StaticSpannedExpression>,
    current_binding: Option<&str>,
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<Vec<SemanticEventBinding>, String> {
    let Some(object) = resolve_element_object(element, context, stack, locals, passed)? else {
        return Ok(Vec::new());
    };
    let Some(event_object) = find_object_field(object, "event").and_then(resolve_object) else {
        return Ok(Vec::new());
    };
    let source_binding = event_binding_source_path(
        element,
        current_binding,
        context,
        locals,
        passed,
        stack,
    );

    Ok(event_object
        .variables
        .iter()
        .filter_map(|variable| {
            let event_name = variable.node.name.as_str();
            let kind = ui_event_kind_for_name(event_name)?;
            matches!(variable.node.value.node, StaticExpression::Link).then_some(
                SemanticEventBinding {
                    kind,
                    source_binding: source_binding.clone(),
                    action: source_binding
                        .as_deref()
                        .and_then(|binding| event_action_for(binding, event_name, context)),
                },
            )
        })
        .collect())
}

fn event_binding_source_path(
    element: Option<&StaticSpannedExpression>,
    current_binding: Option<&str>,
    context: &LowerContext<'_>,
    locals: &LocalScopes<'_>,
    passed: &PassedScopes,
    _stack: &mut Vec<String>,
) -> Option<String> {
    element
        .and_then(|element| {
            match &element.node {
                StaticExpression::Alias(static_expression::Alias::WithoutPassed { parts, .. })
                    if parts.len() == 1 =>
                {
                    let name = parts[0].as_str();
                    lookup_local_object_base(name, locals)
                        .map(|base| format!("{base}.{name}"))
                        .or_else(|| {
                            canonical_expression_path(
                                element,
                                context,
                                locals,
                                passed,
                                &mut Vec::new(),
                            )
                            .ok()
                        })
                        .or_else(|| alias_binding_name(element).ok().flatten().map(ToString::to_string))
                }
                _ => canonical_expression_path(element, context, locals, passed, &mut Vec::new())
                    .ok()
                    .or_else(|| alias_binding_name(element).ok().flatten().map(ToString::to_string)),
            }
        })
        .or_else(|| current_binding.map(ToString::to_string))
}

fn collect_fact_bindings<'a>(
    element: Option<&'a StaticSpannedExpression>,
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<Vec<SemanticFactBinding>, String> {
    let Some(object) = resolve_element_object(element, context, stack, locals, passed)? else {
        return Ok(Vec::new());
    };

    Ok(object
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
        .collect())
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
    let key = (binding_name.to_string(), event_name.to_string());
    let mut actions = Vec::new();

    if let Some(updates) = context.scalar_plan.event_updates.get(&key).cloned() {
        actions.push(SemanticAction::UpdateScalars { updates });
    }
    if let Some(updates) = context.text_plan.event_updates.get(&key).cloned() {
        actions.push(SemanticAction::UpdateTexts { updates });
    }
    if let Some(updates) = context.list_plan.event_updates.get(&key).cloned() {
        actions.push(SemanticAction::UpdateTextLists { updates });
    }
    if let Some(updates) = context.object_list_plan.event_updates.get(&key).cloned() {
        actions.push(SemanticAction::UpdateObjectLists { updates });
    }

    match actions.len() {
        0 => None,
        1 => actions.into_iter().next(),
        _ => Some(SemanticAction::Batch { actions }),
    }
}

fn ui_event_kind_for_name(event_name: &str) -> Option<boon_scene::UiEventKind> {
    match event_name {
        "press" | "click" => Some(boon_scene::UiEventKind::Click),
        "double_click" => Some(boon_scene::UiEventKind::DoubleClick),
        "change" | "input" => Some(boon_scene::UiEventKind::Input),
        "blur" => Some(boon_scene::UiEventKind::Blur),
        "focus" => Some(boon_scene::UiEventKind::Focus),
        "key_down" => Some(boon_scene::UiEventKind::KeyDown),
        _ => None,
    }
}

fn ui_event_name(kind: &boon_scene::UiEventKind) -> Option<&'static str> {
    match kind {
        boon_scene::UiEventKind::Click => Some("click"),
        boon_scene::UiEventKind::DoubleClick => Some("double_click"),
        boon_scene::UiEventKind::Input => Some("input"),
        boon_scene::UiEventKind::Blur => Some("blur"),
        boon_scene::UiEventKind::Focus => Some("focus"),
        boon_scene::UiEventKind::KeyDown => Some("key_down"),
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

fn resolved_function_name_in<'a>(
    path: &'a [boon::parser::StrSlice],
    functions: &'a BTreeMap<String, FunctionSpec<'a>>,
) -> Option<&'a str> {
    if path.is_empty() {
        return None;
    }
    if path.len() == 1 {
        return functions
            .get_key_value(path[0].as_str())
            .map(|(name, _)| name.as_str());
    }
    let qualified = path
        .iter()
        .map(|part| part.as_str())
        .collect::<Vec<_>>()
        .join("/");
    functions
        .get_key_value(qualified.as_str())
        .map(|(name, _)| name.as_str())
}

fn resolved_function_name_for_path<'a>(
    path: &'a [boon::parser::StrSlice],
    context: &'a LowerContext<'a>,
) -> Option<&'a str> {
    resolved_function_name_in(path, &context.functions)
}

fn active_function_module<'a>(stack: &'a [String]) -> Option<&'a str> {
    stack
        .iter()
        .rev()
        .find_map(|entry| entry.strip_prefix("module:"))
}

fn resolved_function_name_for_path_in_stack<'a>(
    path: &'a [boon::parser::StrSlice],
    context: &'a LowerContext<'a>,
    stack: &[String],
) -> Option<&'a str> {
    resolved_function_name_for_path(path, context).or_else(|| {
        if path.len() != 1 {
            return None;
        }
        let module = active_function_module(stack)?;
        let qualified = format!("{module}/{}", path[0].as_str());
        context
            .functions
            .get_key_value(qualified.as_str())
            .map(|(name, _)| name.as_str())
    })
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
        if !matches!(
            &next.node,
            StaticExpression::Alias(static_expression::Alias::WithoutPassed { parts, .. })
                if parts.len() == 1 && parts[0].as_str() == binding_name
        ) {
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
        let has_local_expr_shadow = parts.len() == 1
            && lookup_local_binding_expr(parts[0].as_str(), locals).is_some()
            && lookup_local_object_base(parts[0].as_str(), locals).is_none();
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
    if let StaticExpression::Alias(static_expression::Alias::WithoutPassed { parts, .. }) =
        &expression.node
    {
        if let Some(function_name) = resolved_function_name_for_path_in_stack(parts, context, stack)
        {
            return Ok(Some((function_name, &[])));
        }
    }
    let expression = resolve_alias(expression, context, locals, passed, stack)?;
    match &expression.node {
        StaticExpression::FunctionCall { path, arguments }
            if resolved_function_name_for_path_in_stack(path, context, stack).is_some() =>
        {
            Ok(Some((
                resolved_function_name_for_path_in_stack(path, context, stack)
                    .expect("guard ensured function resolution"),
                arguments,
            )))
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
            || canonical_expression_path(
                &variable.node.value,
                context,
                &eval_locals,
                &eval_passed,
                &mut Vec::new(),
            )
            .is_ok()
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
        StaticExpression::ArithmeticOperator(static_expression::ArithmeticOperator::Multiply {
            operand_a,
            operand_b,
        }) => Ok(format!(
            "({}*{})",
            expression_fingerprint(operand_a, context, locals, passed, stack)?,
            expression_fingerprint(operand_b, context, locals, passed, stack)?,
        )),
        StaticExpression::ArithmeticOperator(static_expression::ArithmeticOperator::Divide {
            operand_a,
            operand_b,
        }) => Ok(format!(
            "({}/{})",
            expression_fingerprint(operand_a, context, locals, passed, stack)?,
            expression_fingerprint(operand_b, context, locals, passed, stack)?,
        )),
        StaticExpression::FunctionCall { path, arguments }
            if resolved_function_name_for_path_in_stack(path, context, stack).is_some() =>
        {
            invocation_marker(
                resolved_function_name_for_path_in_stack(path, context, stack)
                    .expect("guard ensured function resolution"),
                arguments,
                None,
                context,
                locals,
                passed,
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
            invocation_marker(
                function_name,
                arguments,
                Some(from.as_ref()),
                context,
                locals,
                passed,
            )
        }
        StaticExpression::Pipe { from, to } => Ok(format!(
            "pipe({}->{})",
            expression_fingerprint(from, context, locals, passed, stack)?,
            expression_fingerprint(to, context, locals, passed, stack)?,
        )),
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
    let module_marker = function_name
        .split_once('/')
        .map(|(module, _)| format!("module:{module}"));
    if let Some(marker) = &module_marker {
        stack.push(marker.clone());
    }
    locals.push(scope);
    let result = run(function.body, context, stack, locals, passed);
    locals.pop();
    if module_marker.is_some() {
        stack.pop();
    }
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
            argument.node.value.as_ref().is_some_and(|value| {
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
        StaticExpression::When { arms } | StaticExpression::While { arms } => arms
            .iter()
            .any(|arm| expression_depends_on_item_scope(&arm.body, context, locals, passed)),
        StaticExpression::Object(object) | StaticExpression::TaggedObject { object, .. } => {
            object.variables.iter().any(|variable| {
                expression_depends_on_item_scope(&variable.node.value, context, locals, passed)
            })
        }
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
    timer_bindings: &BTreeMap<String, u32>,
) -> Result<ScalarPlan, String> {
    let latest_value_specs = detect_latest_value_specs(path_bindings)?;
    let tag_toggle_specs = detect_tag_toggle_scalar_specs(path_bindings)?;
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

    for (binding_name, spec) in &tag_toggle_specs {
        plan.initial_values
            .insert(binding_name.clone(), spec.initial_value);
        for (trigger_binding, event_name) in &spec.events {
            push_event_update(
                &mut plan,
                trigger_binding,
                event_name,
                ScalarUpdate::ToggleBool {
                    binding: binding_name.clone(),
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
        if let Some(spec) =
            hold_payload_scalar_spec_for_expression(expression, path_bindings, binding_name)?
        {
            plan.initial_values
                .insert(binding_name.clone(), spec.initial_value);
            push_event_update(
                &mut plan,
                &spec.trigger_binding,
                &spec.event_name,
                ScalarUpdate::SetFromPayloadNumber {
                    binding: binding_name.clone(),
                },
            );
        }
        if let Some(spec) =
            counter_spec_for_expression(expression, path_bindings, binding_name, timer_bindings)?
        {
            plan.initial_values
                .insert(binding_name.clone(), spec.initial);
            for event in spec.events {
                let update = match event.update {
                    CounterEventUpdate::Add(delta) => ScalarUpdate::Add {
                        binding: binding_name.clone(),
                        delta,
                    },
                    CounterEventUpdate::AddTenths(tenths_delta) => ScalarUpdate::AddTenths {
                        binding: binding_name.clone(),
                        tenths_delta,
                    },
                    CounterEventUpdate::Set(value) => ScalarUpdate::Set {
                        binding: binding_name.clone(),
                        value,
                    },
                };
                push_event_update(&mut plan, &event.trigger_binding, &event.event_name, update);
            }
        }
        if let Some(events) = event_only_counter_events_for_expression(
            expression,
            path_bindings,
            binding_name,
            timer_bindings,
        )? {
            for event in events {
                let update = match event.update {
                    CounterEventUpdate::Add(delta) => ScalarUpdate::Add {
                        binding: binding_name.clone(),
                        delta,
                    },
                    CounterEventUpdate::AddTenths(tenths_delta) => ScalarUpdate::AddTenths {
                        binding: binding_name.clone(),
                        tenths_delta,
                    },
                    CounterEventUpdate::Set(value) => ScalarUpdate::Set {
                        binding: binding_name.clone(),
                        value,
                    },
                };
                push_event_update(&mut plan, &event.trigger_binding, &event.event_name, update);
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
            route_text_spec_for_expression(expression, path_bindings, binding_name)?
        {
            plan.initial_values
                .insert(binding_name.clone(), initial_value);
            for ((trigger_binding, event_name), update) in event_updates {
                plan.event_updates
                    .entry((trigger_binding, event_name))
                    .or_default()
                    .push(update);
            }
            continue;
        }
        if let Some((initial_value, event_updates)) =
            latest_text_spec_for_expression(expression, path_bindings, binding_name)?
        {
            plan.initial_values
                .insert(binding_name.clone(), initial_value);
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
            plan.initial_values
                .insert(binding_name.clone(), initial_value);
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
        if let Some(spec) = derived_text_value_branch_spec(expression, path_bindings, binding_name)?
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
                    detect_object_list_pipeline(
                        source,
                        path_bindings,
                        functions,
                        &binding,
                        &ScalarPlan::default(),
                        &TextPlan::default(),
                    )
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
    if let StaticExpression::Pipe { from, to } = &expression.node {
        let StaticExpression::FunctionCall { path, arguments } = &to.node else {
            return Ok(None);
        };
        if path_matches(path, &["List", "retain"]) {
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
                StaticExpression::Literal(static_expression::Literal::Tag(tag))
                    if tag.as_str() == "True"
            ) {
                return Ok(Some((binding, None)));
            }
            let Some(filter) =
                derived_object_list_filter(condition, item_name, path_bindings, binding_path)?
            else {
                return Ok(None);
            };
            return Ok(Some((binding, Some(filter))));
        }
    }
    if let Some(binding) =
        runtime_object_list_binding_path_for_scalar(expression, path_bindings, binding_path)?
    {
        return Ok(Some((binding, None)));
    }
    Ok(None)
}

fn derived_text_list_ref(
    expression: &StaticSpannedExpression,
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    binding_path: &str,
) -> Result<Option<(String, Option<TextListFilter>)>, String> {
    if let StaticExpression::Pipe { from, to } = &expression.node {
        let StaticExpression::FunctionCall { path, arguments } = &to.node else {
            return Ok(None);
        };
        if path_matches(path, &["List", "retain"]) {
            let Some((binding, None)) = derived_text_list_ref(from, path_bindings, binding_path)?
            else {
                return Ok(None);
            };
            let item_name = find_positional_parameter_name(arguments)
                .ok_or_else(|| "List/retain requires an item parameter name".to_string())?;
            let condition = find_named_argument(arguments, "if")
                .ok_or_else(|| "List/retain requires `if`".to_string())?;
            let filter = runtime_text_list_filter(condition, item_name)?;
            return Ok(Some((binding, Some(filter))));
        }
    }
    if let Some(binding) = canonical_reference_path(expression, path_bindings, binding_path)? {
        if let Some(source_expression) = path_bindings.get(&binding).copied() {
            if detect_text_list_pipeline(source_expression, path_bindings, &binding)?.is_some() {
                return Ok(Some((binding, None)));
            }
        }
    }
    Ok(None)
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
    if let Some(filter) =
        derived_object_text_starts_with_filter(expression, item_name, path_bindings, binding_path)?
    {
        return Ok(Some(filter));
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

fn runtime_object_text_starts_with_filter(
    expression: &StaticSpannedExpression,
    item_name: &str,
    context: &LowerContext<'_>,
    locals: &LocalScopes<'_>,
    passed: &PassedScopes,
) -> Result<Option<ObjectListFilter>, String> {
    let StaticExpression::Pipe { from, to } = &expression.node else {
        return Ok(None);
    };
    let StaticExpression::Alias(static_expression::Alias::WithoutPassed { parts, .. }) = &from.node
    else {
        return Ok(None);
    };
    if parts.len() != 2 || parts[0].as_str() != item_name {
        return Ok(None);
    }
    let StaticExpression::FunctionCall { path, arguments } = &to.node else {
        return Ok(None);
    };
    if !path_matches(path, &["Text", "starts_with"]) {
        return Ok(None);
    }
    let prefix = find_named_argument(arguments, "prefix")
        .ok_or_else(|| "Text/starts_with requires `prefix`".to_string())?;
    let Some(binding) = runtime_text_binding_path(prefix, context, locals, passed)? else {
        return Ok(None);
    };
    Ok(Some(ObjectListFilter::TextFieldStartsWithTextBinding {
        field: parts[1].as_str().to_string(),
        binding,
    }))
}

fn derived_object_text_starts_with_filter(
    expression: &StaticSpannedExpression,
    item_name: &str,
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    binding_path: &str,
) -> Result<Option<ObjectListFilter>, String> {
    let StaticExpression::Pipe { from, to } = &expression.node else {
        return Ok(None);
    };
    let StaticExpression::Alias(static_expression::Alias::WithoutPassed { parts, .. }) = &from.node
    else {
        return Ok(None);
    };
    if parts.len() != 2 || parts[0].as_str() != item_name {
        return Ok(None);
    }
    let StaticExpression::FunctionCall { path, arguments } = &to.node else {
        return Ok(None);
    };
    if !path_matches(path, &["Text", "starts_with"]) {
        return Ok(None);
    }
    let prefix = find_named_argument(arguments, "prefix")
        .ok_or_else(|| "Text/starts_with requires `prefix`".to_string())?;
    let Some(binding) = canonical_reference_path(prefix, path_bindings, binding_path)? else {
        return Ok(None);
    };
    Ok(Some(ObjectListFilter::TextFieldStartsWithTextBinding {
        field: parts[1].as_str().to_string(),
        binding,
    }))
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
    let Some(binding) =
        runtime_object_list_binding_path_for_scalar(from, path_bindings, binding_path)?
    else {
        return Ok(None);
    };
    let Some(source_expression) = path_bindings.get(&binding).copied() else {
        return Ok(None);
    };
    let Some((items, _, _, _)) = detect_object_list_pipeline(
        source_expression,
        path_bindings,
        functions,
        &binding,
        &ScalarPlan::default(),
        &TextPlan::default(),
    )?
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
    match &expression.node {
        StaticExpression::ArithmeticOperator(operator) => {
            let (operand_a, operand_b, op) = match operator {
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
                static_expression::ArithmeticOperator::Multiply {
                    operand_a,
                    operand_b,
                } => (
                    operand_a.as_ref(),
                    operand_b.as_ref(),
                    DerivedArithmeticOp::Multiply,
                ),
                static_expression::ArithmeticOperator::Divide {
                    operand_a,
                    operand_b,
                } => (
                    operand_a.as_ref(),
                    operand_b.as_ref(),
                    DerivedArithmeticOp::Divide,
                ),
                static_expression::ArithmeticOperator::Negate { .. } => return Ok(None),
            };
            let Some(value_a) =
                scalar_operand_value(operand_a, path_bindings, functions, binding_path)?
            else {
                return Ok(None);
            };
            let Some(value_b) =
                scalar_operand_value(operand_b, path_bindings, functions, binding_path)?
            else {
                return Ok(None);
            };
            Ok(Some(match op {
                DerivedArithmeticOp::Add => value_a + value_b,
                DerivedArithmeticOp::Subtract => value_a - value_b,
                DerivedArithmeticOp::Multiply => value_a * value_b,
                DerivedArithmeticOp::Divide => {
                    if value_b == 0 {
                        return Ok(None);
                    }
                    value_a / value_b
                }
            }))
        }
        StaticExpression::Pipe { from, to } => {
            let StaticExpression::FunctionCall { path, arguments } = &to.node else {
                return Ok(None);
            };
            if path_matches(path, &["Math", "round"]) && arguments.is_empty() {
                return arithmetic_scalar_value(from, path_bindings, functions, binding_path);
            }
            if path_matches(path, &["Math", "min"]) {
                let Some(left) =
                    scalar_operand_value(from, path_bindings, functions, binding_path)?
                else {
                    return Ok(None);
                };
                let right = find_named_argument(arguments, "b")
                    .ok_or_else(|| "Math/min requires `b`".to_string())
                    .and_then(|argument| {
                        scalar_operand_value(argument, path_bindings, functions, binding_path)?
                            .ok_or_else(|| "Math/min requires a numeric `b`".to_string())
                    })?;
                return Ok(Some(left.min(right)));
            }
            Ok(None)
        }
        _ => Ok(None),
    }
}

fn derived_arithmetic_scalar_spec(
    expression: &StaticSpannedExpression,
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    binding_path: &str,
) -> Result<Option<DerivedScalarSpec>, String> {
    let Some(expr) = derived_scalar_operand(expression, path_bindings, binding_path)? else {
        return Ok(None);
    };
    Ok(Some(DerivedScalarSpec::Arithmetic {
        target: binding_path.to_string(),
        expr,
    }))
}

fn derived_text_value_branch_spec(
    expression: &StaticSpannedExpression,
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    binding_path: &str,
) -> Result<Option<DerivedScalarSpec>, String> {
    let StaticExpression::Pipe { from, to } = &expression.node else {
        return Ok(None);
    };
    let (StaticExpression::When { arms } | StaticExpression::While { arms }) = &to.node else {
        return Ok(None);
    };
    let Some(binding) = canonical_reference_path(from, path_bindings, binding_path)? else {
        return Ok(None);
    };
    if binding_initial_text_value(path_bindings, &binding)?.is_none() {
        return Ok(None);
    }

    let mut branches = Vec::new();
    let mut fallback = None;
    let mut dynamic_values = BTreeMap::new();
    for arm in arms {
        let Some((_name, value)) = text_branch_body_scalar_value(&arm.body, &mut dynamic_values)?
        else {
            return Ok(None);
        };
        match &arm.pattern {
            static_expression::Pattern::Literal(static_expression::Literal::Tag(tag))
            | static_expression::Pattern::Literal(static_expression::Literal::Text(tag)) => {
                branches.push((tag.as_str().to_string(), value));
            }
            static_expression::Pattern::WildCard => {
                fallback = Some(value);
            }
            _ => return Ok(None),
        }
    }

    let Some(fallback) = fallback else {
        return Ok(None);
    };
    if branches.is_empty() {
        return Ok(None);
    }

    Ok(Some(DerivedScalarSpec::TextValueBranch {
        target: binding_path.to_string(),
        binding,
        branches,
        fallback,
    }))
}

fn text_branch_body_scalar_value(
    expression: &StaticSpannedExpression,
    dynamic_values: &mut BTreeMap<String, i64>,
) -> Result<Option<(String, i64)>, String> {
    if let Some(value) = extract_bool_literal_opt(expression)? {
        let name = if value { "True" } else { "False" };
        return Ok(Some((name.to_string(), i64::from(value))));
    }
    if let Some(value) = extract_filter_tag_value(expression)? {
        let StaticExpression::Literal(static_expression::Literal::Tag(tag)) = &expression.node
        else {
            return Ok(None);
        };
        return Ok(Some((tag.as_str().to_string(), value)));
    }
    match &expression.node {
        StaticExpression::Literal(static_expression::Literal::Tag(tag))
        | StaticExpression::Literal(static_expression::Literal::Text(tag)) => {
            let key = tag.as_str().to_string();
            if let Some(value) = dynamic_values.get(&key).copied() {
                return Ok(Some((key, value)));
            }
            let value = 1024 + dynamic_values.len() as i64;
            dynamic_values.insert(key.clone(), value);
            Ok(Some((key, value)))
        }
        _ => Ok(None),
    }
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
    if let Some((value_a, value_b)) =
        text_comparison_operand_values(operand_a, operand_b, path_bindings, binding_path)?
    {
        let matched = match op {
            IntCompareOp::Equal => value_a == value_b,
            IntCompareOp::NotEqual => value_a != value_b,
            IntCompareOp::Greater => value_a > value_b,
            IntCompareOp::GreaterOrEqual => value_a >= value_b,
            IntCompareOp::Less => value_a < value_b,
            IntCompareOp::LessOrEqual => value_a <= value_b,
        };
        return Ok(Some(i64::from(matched)));
    }
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
    if let (Some(left), Some(right)) = (
        derived_text_operand(left_expr, path_bindings, binding_path)?,
        derived_text_operand(right_expr, path_bindings, binding_path)?,
    ) {
        return Ok(Some(DerivedScalarSpec::TextComparison {
            target: binding_path.to_string(),
            op,
            left,
            right,
        }));
    }
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

fn text_comparison_operand_values(
    left: &StaticSpannedExpression,
    right: &StaticSpannedExpression,
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    binding_path: &str,
) -> Result<Option<(String, String)>, String> {
    let Some(left) = derived_text_operand_value(left, path_bindings, binding_path)? else {
        return Ok(None);
    };
    let Some(right) = derived_text_operand_value(right, path_bindings, binding_path)? else {
        return Ok(None);
    };
    Ok(Some((left, right)))
}

fn derived_text_operand_value(
    expression: &StaticSpannedExpression,
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    binding_path: &str,
) -> Result<Option<String>, String> {
    if let Some(value) = static_text_literal_value(expression) {
        return Ok(Some(value));
    }
    let Some(binding) = canonical_reference_path(expression, path_bindings, binding_path)? else {
        return Ok(None);
    };
    binding_initial_text_value(path_bindings, &binding)
}

fn derived_text_operand(
    expression: &StaticSpannedExpression,
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    binding_path: &str,
) -> Result<Option<DerivedTextOperand>, String> {
    if let Some(value) = static_text_literal_value(expression) {
        return Ok(Some(DerivedTextOperand::Literal(value)));
    }
    let Some(binding) = canonical_reference_path(expression, path_bindings, binding_path)? else {
        return Ok(None);
    };
    if binding_initial_text_value(path_bindings, &binding)?.is_some() {
        return Ok(Some(DerivedTextOperand::Binding(binding)));
    }
    Ok(None)
}

fn static_text_literal_value(expression: &StaticSpannedExpression) -> Option<String> {
    match &expression.node {
        StaticExpression::Literal(static_expression::Literal::Text(text))
        | StaticExpression::Literal(static_expression::Literal::Tag(text)) => {
            Some(text.as_str().to_string())
        }
        StaticExpression::TextLiteral { parts, .. } => {
            let mut text = String::new();
            for part in parts {
                match part {
                    StaticTextPart::Text(part) => text.push_str(part.as_str()),
                    StaticTextPart::Interpolation { .. } => return None,
                }
            }
            Some(text)
        }
        _ => None,
    }
}

fn is_router_route_expression(expression: &StaticSpannedExpression) -> bool {
    matches!(
        &expression.node,
        StaticExpression::FunctionCall { path, arguments }
            if path_matches(path, &["Router", "route"]) && arguments.is_empty()
    )
}

fn binding_initial_text_value(
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    binding_path: &str,
) -> Result<Option<String>, String> {
    let Some(expression) = path_bindings.get(binding_path).copied() else {
        return Ok(None);
    };
    if is_router_route_expression(expression) {
        return Ok(Some("/".to_string()));
    }
    if let Some((initial, _)) =
        latest_text_spec_for_expression(expression, path_bindings, binding_path)?
    {
        return Ok(Some(initial));
    }
    if let Some((initial, _)) =
        hold_text_spec_for_expression(expression, path_bindings, binding_path)?
    {
        return Ok(Some(initial));
    }
    Ok(None)
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
    match &expression.node {
        StaticExpression::ArithmeticOperator(operator) => {
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
                static_expression::ArithmeticOperator::Multiply {
                    operand_a,
                    operand_b,
                } => (
                    operand_a.as_ref(),
                    operand_b.as_ref(),
                    DerivedArithmeticOp::Multiply,
                ),
                static_expression::ArithmeticOperator::Divide {
                    operand_a,
                    operand_b,
                } => (
                    operand_a.as_ref(),
                    operand_b.as_ref(),
                    DerivedArithmeticOp::Divide,
                ),
                static_expression::ArithmeticOperator::Negate { .. } => return Ok(None),
            };
            let Some(left) = derived_scalar_operand(left_expr, path_bindings, binding_path)? else {
                return Ok(None);
            };
            let Some(right) = derived_scalar_operand(right_expr, path_bindings, binding_path)?
            else {
                return Ok(None);
            };
            return Ok(Some(DerivedScalarOperand::Arithmetic {
                op,
                left: Box::new(left),
                right: Box::new(right),
            }));
        }
        StaticExpression::Pipe { from, to } => {
            let StaticExpression::FunctionCall { path, arguments } = &to.node else {
                return Ok(None);
            };
            if path_matches(path, &["Math", "round"]) && arguments.is_empty() {
                let Some(source) = derived_scalar_operand(from, path_bindings, binding_path)?
                else {
                    return Ok(None);
                };
                return Ok(Some(DerivedScalarOperand::Round {
                    source: Box::new(source),
                }));
            }
            if path_matches(path, &["Math", "min"]) {
                let Some(left) = derived_scalar_operand(from, path_bindings, binding_path)? else {
                    return Ok(None);
                };
                let right_expr = find_named_argument(arguments, "b")
                    .ok_or_else(|| "Math/min requires `b`".to_string())?;
                let Some(right) = derived_scalar_operand(right_expr, path_bindings, binding_path)?
                else {
                    return Ok(None);
                };
                return Ok(Some(DerivedScalarOperand::Min {
                    left: Box::new(left),
                    right: Box::new(right),
                }));
            }
        }
        _ => {}
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
    if matches!(
        &expression.node,
        StaticExpression::ArithmeticOperator(_) | StaticExpression::Pipe { .. }
    ) {
        if let Some(value) =
            arithmetic_scalar_value(expression, path_bindings, functions, binding_path)?
        {
            return Ok(Some(value));
        }
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
    canonical_reference_path(expression, path_bindings, binding_path)
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
                    let update = match event.update {
                        CounterEventUpdate::Add(delta) => ScalarUpdate::Add {
                            binding: count_binding.clone(),
                            delta,
                        },
                        CounterEventUpdate::AddTenths(_) | CounterEventUpdate::Set(_) => {
                            return Err(
                                "static object counter subset requires additive integer updates"
                                    .to_string(),
                            );
                        }
                    };
                    push_event_update(
                        scalar_plan,
                        &format!("{base}.{}", event.trigger_binding),
                        &event.event_name,
                        update,
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
        .map(boon::parser::StrSlice::as_str)
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
struct TagToggleScalarSpec {
    initial_value: i64,
    events: Vec<(String, String)>,
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
    let Some(function_name) = resolved_function_name_in(path, functions) else {
        return Ok(None);
    };
    let Some(function) = functions.get(function_name) else {
        return Ok(None);
    };
    if function.parameters.len() == arguments.len() {
        let bindings = function_argument_bindings(function, arguments)?;
        let specialized = specialize_static_expression(function.body, &bindings);
        if let Some(object) = resolve_object(specialized) {
            return Ok(Some(object));
        }
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
    let function_name = resolved_function_name_in(path, functions)?;
    let function = functions.get(function_name)?;
    let object = resolve_object(function.body)?;
    let field = find_object_field(object, field_name)?;
    if function.parameters.len() == arguments.len() {
        let bindings = function_argument_bindings(function, arguments).ok()?;
        return Some(specialize_static_expression(field, &bindings));
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
    let CounterEventUpdate::Add(delta) = extract_counter_event_update(body, Some(state_param))?
    else {
        return Ok(None);
    };
    Ok(Some(EventDeltaSpec {
        trigger_binding,
        event_name,
        update: CounterEventUpdate::Add(delta),
    }))
}

fn detect_local_bool_spec(
    expression: &StaticSpannedExpression,
) -> Result<Option<BoolSpec>, String> {
    if let Some(spec) = detect_local_hold_bool_spec(expression)? {
        return Ok(Some(spec));
    }
    let StaticExpression::Pipe { from, to } = &expression.node else {
        return Ok(None);
    };
    let StaticExpression::FunctionCall { path, arguments } = &to.node else {
        return Ok(None);
    };
    if !path_matches(path, &["Bool", "toggle"]) {
        return Ok(None);
    }
    let Some(when) = find_named_argument(arguments, "when") else {
        return Ok(None);
    };
    let Some((trigger_binding, event_name)) = object_event_source_from_expression(when)? else {
        return Ok(None);
    };
    let Some(mut spec) = detect_local_bool_source_spec(from)? else {
        return Ok(None);
    };
    spec.events.push(BoolEventSpec {
        trigger_binding,
        event_name,
        update: BoolEventUpdate::Toggle,
        payload_filter: None,
    });
    Ok(Some(spec))
}

fn detect_local_hold_bool_spec(
    expression: &StaticSpannedExpression,
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

fn detect_local_bool_source_spec(
    expression: &StaticSpannedExpression,
) -> Result<Option<BoolSpec>, String> {
    if let Some(initial) = extract_bool_literal_opt(expression)? {
        return Ok(Some(BoolSpec {
            initial,
            events: Vec::new(),
        }));
    }
    let StaticExpression::Latest { inputs } = &expression.node else {
        return Ok(None);
    };
    let mut initial = None;
    let mut events = Vec::new();
    for input in inputs {
        if let Some(value) = extract_bool_literal_opt(input)? {
            if initial.is_none() {
                initial = Some(value);
            }
            continue;
        }
        let StaticExpression::Pipe {
            from: trigger_source,
            to: trigger_then,
        } = &input.node
        else {
            return Ok(None);
        };
        let Some(detected_events) = local_bool_event_spec(trigger_source, trigger_then)? else {
            return Ok(None);
        };
        events.extend(detected_events);
    }
    let Some(initial) = initial else {
        return Ok(None);
    };
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
        .map(boon::parser::StrSlice::as_str)
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
        .map(boon::parser::StrSlice::as_str)
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
    scalar_plan: &ScalarPlan,
    text_plan: &TextPlan,
) -> Result<ObjectListPlan, String> {
    let mut plan = ObjectListPlan::default();
    for (binding_name, expression) in path_bindings {
        let Some((initial_items, updates, item_actions, global_actions)) =
            detect_object_list_pipeline(
                expression,
                path_bindings,
                functions,
                binding_name,
                scalar_plan,
                text_plan,
            )
            .map_err(|error| format!("{binding_name}: {error}"))?
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
        let field_kinds = detect_initial_object_field_kinds(
            expression,
            context,
            &mut Vec::new(),
            &mut Vec::new(),
            &mut Vec::new(),
        )?;
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
            context
                .scalar_plan
                .initial_values
                .entry(binding)
                .or_insert(value);
        }
        for (binding, value) in plan.text_initials {
            context
                .text_plan
                .initial_values
                .entry(binding)
                .or_insert(value);
        }
        for (trigger, updates) in plan.scalar_event_updates {
            context
                .scalar_plan
                .event_updates
                .entry(trigger)
                .or_default()
                .extend(updates);
        }
        for (trigger, updates) in plan.text_event_updates {
            context
                .text_plan
                .event_updates
                .entry(trigger)
                .or_default()
                .extend(updates);
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
    augment_dependent_object_list_updates_from_item_bindings(context)?;
    Ok(())
}

fn augment_hold_alias_runtime(context: &mut LowerContext<'_>) -> Result<(), String> {
    let binding_names = context.path_bindings.keys().cloned().collect::<Vec<_>>();
    for binding_name in binding_names {
        let Some(expression) = context.path_bindings.get(&binding_name).copied() else {
            continue;
        };
        if let Some((initial_value, source_binding)) =
            hold_alias_scalar_spec_for_expression(expression, &context.path_bindings, &binding_name)?
        {
            context
                .scalar_plan
                .initial_values
                .entry(binding_name.clone())
                .or_insert(initial_value);
            push_runtime_mirror(
                &mut context.scalar_mirrors,
                source_binding,
                binding_name.clone(),
            );
        }
        if let Some((initial_value, source_binding)) =
            hold_alias_text_spec_for_expression(expression, &context.path_bindings, &binding_name)?
        {
            context
                .text_plan
                .initial_values
                .entry(binding_name.clone())
                .or_insert(initial_value);
            push_runtime_mirror(
                &mut context.text_mirrors,
                source_binding,
                binding_name.clone(),
            );
        }
    }
    Ok(())
}

fn augment_object_alias_field_runtime(context: &mut LowerContext<'_>) -> Result<(), String> {
    let binding_names = context.bindings.keys().cloned().collect::<Vec<_>>();
    let scalar_sources = context
        .scalar_plan
        .initial_values
        .keys()
        .cloned()
        .chain(context.scalar_plan.event_updates.values().flat_map(|updates| {
            updates.iter().map(|update| match update {
                ScalarUpdate::Set { binding, .. }
                | ScalarUpdate::SetFromPayloadNumber { binding }
                | ScalarUpdate::SetFiltered { binding, .. }
                | ScalarUpdate::Add { binding, .. }
                | ScalarUpdate::AddTenths { binding, .. }
                | ScalarUpdate::ToggleBool { binding } => binding.clone(),
            })
        }))
        .chain(context.object_list_plan.item_actions.values().flat_map(|actions| {
            actions.iter().flat_map(|action| match &action.action {
                ObjectItemActionKind::UpdateBindings { scalar_updates, .. } => scalar_updates
                    .iter()
                    .map(|update| match update {
                        ItemScalarUpdate::SetStatic { binding, .. }
                        | ItemScalarUpdate::SetFromField { binding, .. } => binding.clone(),
                    })
                    .collect::<Vec<_>>(),
                _ => Vec::new(),
            })
        }))
        .collect::<BTreeSet<_>>();
    let text_sources = context
        .text_plan
        .initial_values
        .keys()
        .cloned()
        .chain(context.text_plan.event_updates.values().flat_map(|updates| {
            updates.iter().map(|update| match update {
                TextUpdate::SetStatic { binding, .. }
                | TextUpdate::SetComputed { binding, .. }
                | TextUpdate::SetComputedBranch { binding, .. }
                | TextUpdate::SetFromInput { binding, .. }
                | TextUpdate::SetFromPayload { binding }
                | TextUpdate::SetFromValueSource { binding, .. } => binding.clone(),
            })
        }))
        .chain(context.object_list_plan.item_actions.values().flat_map(|actions| {
            actions.iter().flat_map(|action| match &action.action {
                ObjectItemActionKind::UpdateBindings { text_updates, .. } => text_updates
                    .iter()
                    .map(|update| match update {
                        ItemTextUpdate::SetStatic { binding, .. }
                        | ItemTextUpdate::SetFromField { binding, .. }
                        | ItemTextUpdate::SetFromPayload { binding }
                        | ItemTextUpdate::SetFromValueSource { binding, .. }
                        | ItemTextUpdate::SetFromInputSource { binding, .. } => binding.clone(),
                    })
                    .collect::<Vec<_>>(),
                _ => Vec::new(),
            })
        }))
        .collect::<BTreeSet<_>>();

    for binding_name in binding_names {
        let Some(expression) = context.bindings.get(&binding_name).copied() else {
            continue;
        };
        let Some(source_base) =
            canonical_expression_path(expression, context, &Vec::new(), &Vec::new(), &mut Vec::new())
                .ok()
        else {
            continue;
        };
        if source_base == binding_name {
            continue;
        }
        mirror_prefixed_runtime_bindings(
            &source_base,
            &binding_name,
            &scalar_sources,
            &mut context.scalar_mirrors,
            &mut context.scalar_plan.initial_values,
        );
        mirror_prefixed_runtime_bindings(
            &source_base,
            &binding_name,
            &text_sources,
            &mut context.text_mirrors,
            &mut context.text_plan.initial_values,
        );
    }

    Ok(())
}

fn mirror_prefixed_runtime_bindings<T: Clone>(
    source_base: &str,
    target_base: &str,
    source_bindings: &BTreeSet<String>,
    mirrors: &mut BTreeMap<String, Vec<String>>,
    initials: &mut BTreeMap<String, T>,
) {
    for source_binding in source_bindings {
        let Some(suffix) = source_binding
            .strip_prefix(source_base)
            .filter(|suffix| suffix.is_empty() || suffix.starts_with('.'))
        else {
            continue;
        };
        let target_binding = format!("{target_base}{suffix}");
        push_runtime_mirror(mirrors, source_binding.clone(), target_binding.clone());
        if let Some(initial_value) = initials.get(source_binding).cloned() {
            initials.entry(target_binding).or_insert(initial_value);
        }
    }
}

fn push_runtime_mirror(
    mirrors: &mut BTreeMap<String, Vec<String>>,
    source_binding: String,
    target_binding: String,
) {
    if source_binding == target_binding {
        return;
    }
    let entry = mirrors.entry(source_binding).or_default();
    if !entry.contains(&target_binding) {
        entry.push(target_binding);
    }
}

fn augment_document_link_forwarder_item_runtime(
    context: &mut LowerContext<'_>,
) -> Result<(), String> {
    let Some(document) = context.bindings.get("document").copied() else {
        return Ok(());
    };
    let mut item_actions = BTreeMap::<String, Vec<ObjectItemActionSpec>>::new();
    collect_document_link_forwarder_item_actions(
        document,
        context,
        &mut Vec::new(),
        &mut Vec::new(),
        &mut Vec::new(),
        None,
        &mut item_actions,
    )?;
    for (binding, actions) in item_actions {
        let entry = context.object_list_plan.item_actions.entry(binding).or_default();
        for action in actions {
            if !entry.contains(&action) {
                entry.push(action);
            }
        }
    }
    Ok(())
}

fn collect_document_link_forwarder_item_actions<'a>(
    expression: &'a StaticSpannedExpression,
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
    current_object_list_binding: Option<&str>,
    item_actions: &mut BTreeMap<String, Vec<ObjectItemActionSpec>>,
) -> Result<(), String> {
    if let Some(binding_name) = alias_binding_name(expression)? {
        let resolved = match resolve_named_binding(binding_name, context, locals, stack) {
            Ok(resolved) => resolved,
            Err(error) if error.starts_with("unknown top-level binding") => return Ok(()),
            Err(error) => return Err(error),
        };
        let result = collect_document_link_forwarder_item_actions(
            resolved,
            context,
            stack,
            locals,
            passed,
            current_object_list_binding,
            item_actions,
        );
        stack.pop();
        return result;
    }

    let expression = resolve_alias(expression, context, locals, passed, stack)?;
    match &expression.node {
        StaticExpression::Pipe { from, to } => {
            if let StaticExpression::LinkSetter { alias } = &to.node {
                if let Some(list_binding) = current_object_list_binding {
                    let Some(target_path) =
                        canonical_alias_path(&alias.node, context, locals, passed, stack)?
                    else {
                        return Err("LINK target must resolve to a binding path".to_string());
                    };
                    if let Some(action) = linked_forwarder_item_action(
                        target_path.as_str(),
                        from,
                        context,
                        locals,
                        passed,
                    )? {
                        item_actions
                            .entry(list_binding.to_string())
                            .or_default()
                            .push(action);
                    }
                }
                return collect_document_link_forwarder_item_actions(
                    from,
                    context,
                    stack,
                    locals,
                    passed,
                    current_object_list_binding,
                    item_actions,
                );
            }
            if let StaticExpression::FunctionCall { path, arguments } = &to.node {
                if path_matches(path, &["List", "map"]) {
                    let Some(list_ref) =
                        runtime_object_list_ref(from, context, locals, passed, stack)?
                    else {
                        return Ok(());
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
                    let result = collect_document_link_forwarder_item_actions(
                        new,
                        context,
                        stack,
                        locals,
                        passed,
                        Some(list_ref.binding.as_str()),
                        item_actions,
                    );
                    locals.pop();
                    return result;
                }
            }
            if let Some((function_name, arguments)) =
                function_invocation_target(to, context, locals, passed, stack)?
            {
                return with_invoked_function_scope(
                    function_name,
                    arguments,
                    Some(from.as_ref()),
                    context,
                    stack,
                    locals,
                    passed,
                    |body, context, stack, locals, passed| {
                        collect_document_link_forwarder_item_actions(
                            body,
                            context,
                            stack,
                            locals,
                            passed,
                            current_object_list_binding,
                            item_actions,
                        )
                    },
                );
            }
            collect_document_link_forwarder_item_actions(
                from,
                context,
                stack,
                locals,
                passed,
                current_object_list_binding,
                item_actions,
            )?;
            collect_document_link_forwarder_item_actions(
                to,
                context,
                stack,
                locals,
                passed,
                current_object_list_binding,
                item_actions,
            )?;
        }
        StaticExpression::Block { variables, output } => {
            let scope = eager_block_scope(variables, context, stack, locals, passed);
            locals.push(scope);
            for variable in variables {
                if !expression_may_contain_link_setter_syntax(&variable.node.value, context) {
                    continue;
                }
                collect_document_link_forwarder_item_actions(
                    &variable.node.value,
                    context,
                    stack,
                    locals,
                    passed,
                    current_object_list_binding,
                    item_actions,
                )?;
            }
            collect_document_link_forwarder_item_actions(
                output,
                context,
                stack,
                locals,
                passed,
                current_object_list_binding,
                item_actions,
            )?;
            locals.pop();
        }
        StaticExpression::FunctionCall { path, arguments }
            if resolved_function_name_for_path_in_stack(path, context, stack).is_some() =>
        {
            with_invoked_function_scope(
                resolved_function_name_for_path_in_stack(path, context, stack)
                    .expect("guard ensured function resolution"),
                arguments,
                None,
                context,
                stack,
                locals,
                passed,
                |body, context, stack, locals, passed| {
                    collect_document_link_forwarder_item_actions(
                        body,
                        context,
                        stack,
                        locals,
                        passed,
                        current_object_list_binding,
                        item_actions,
                    )
                },
            )?;
        }
        StaticExpression::FunctionCall { arguments, .. } => {
            for argument in arguments {
                if let Some(value) = argument
                    .node
                    .value
                    .as_ref()
                    .filter(|value| expression_may_contain_link_setter_syntax(value, context))
                {
                    collect_document_link_forwarder_item_actions(
                        value,
                        context,
                        stack,
                        locals,
                        passed,
                        current_object_list_binding,
                        item_actions,
                    )?;
                }
            }
        }
        StaticExpression::Object(object) => {
            let scope = object
                .variables
                .iter()
                .filter_map(|variable| {
                    let name = variable.node.name.as_str();
                    (!name.is_empty()).then_some((
                        name.to_string(),
                        LocalBinding {
                            expr: Some(&variable.node.value),
                            object_base: infer_argument_object_base(
                                &variable.node.value,
                                context,
                                locals,
                                passed,
                            ),
                        },
                    ))
                })
                .collect::<BTreeMap<_, _>>();
            locals.push(scope);
            for variable in &object.variables {
                if !expression_may_contain_link_setter_syntax(&variable.node.value, context) {
                    continue;
                }
                collect_document_link_forwarder_item_actions(
                    &variable.node.value,
                    context,
                    stack,
                    locals,
                    passed,
                    current_object_list_binding,
                    item_actions,
                )?;
            }
            locals.pop();
        }
        StaticExpression::List { items } => {
            for item in items {
                if !expression_may_contain_link_setter_syntax(item, context) {
                    continue;
                }
                collect_document_link_forwarder_item_actions(
                    item,
                    context,
                    stack,
                    locals,
                    passed,
                    current_object_list_binding,
                    item_actions,
                )?;
            }
        }
        StaticExpression::Latest { inputs } => {
            for input in inputs {
                if !expression_may_contain_link_setter_syntax(input, context) {
                    continue;
                }
                collect_document_link_forwarder_item_actions(
                    input,
                    context,
                    stack,
                    locals,
                    passed,
                    current_object_list_binding,
                    item_actions,
                )?;
            }
        }
        StaticExpression::When { arms } | StaticExpression::While { arms } => {
            for arm in arms {
                if !expression_may_contain_link_setter_syntax(&arm.body, context) {
                    continue;
                }
                collect_document_link_forwarder_item_actions(
                    &arm.body,
                    context,
                    stack,
                    locals,
                    passed,
                    current_object_list_binding,
                    item_actions,
                )?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn expression_may_contain_link_setter_syntax(
    expression: &StaticSpannedExpression,
    context: &LowerContext<'_>,
) -> bool {
    match &expression.node {
        StaticExpression::Pipe { from, to } => {
            matches!(&to.node, StaticExpression::LinkSetter { .. })
                || expression_may_contain_link_setter_syntax(from, context)
                || expression_may_contain_link_setter_syntax(to, context)
        }
        StaticExpression::Block { variables, output } => {
            variables
                .iter()
                .any(|variable| expression_may_contain_link_setter_syntax(&variable.node.value, context))
                || expression_may_contain_link_setter_syntax(output, context)
        }
        StaticExpression::FunctionCall { path, arguments } => {
            resolved_function_name_for_path(path, context).is_some()
                || arguments.iter().any(|argument| {
                    argument.node.value.as_ref().is_some_and(|value| {
                        expression_may_contain_link_setter_syntax(value, context)
                    })
                })
        }
        StaticExpression::Object(object) => object
            .variables
            .iter()
            .any(|variable| expression_may_contain_link_setter_syntax(&variable.node.value, context)),
        StaticExpression::List { items } | StaticExpression::Latest { inputs: items } => items
            .iter()
            .any(|item| expression_may_contain_link_setter_syntax(item, context)),
        StaticExpression::When { arms } | StaticExpression::While { arms } => arms
            .iter()
            .any(|arm| expression_may_contain_link_setter_syntax(&arm.body, context)),
        _ => false,
    }
}

fn linked_forwarder_item_action(
    target_path: &str,
    expression: &StaticSpannedExpression,
    context: &LowerContext<'_>,
    locals: &LocalScopes<'_>,
    passed: &PassedScopes,
) -> Result<Option<ObjectItemActionSpec>, String> {
    let expression = resolve_alias(expression, context, locals, passed, &mut Vec::new())?;
    let StaticExpression::Pipe { from, to } = &expression.node else {
        return Ok(None);
    };
    let Some(source) = detect_item_event_source(from, context, locals, passed)? else {
        return Ok(None);
    };
    let source_binding_name = alias_binding_name(from)?;

    if let Some(field_kinds) = infer_object_literal_field_kinds(to, context, locals, passed)? {
        if !field_kinds.is_empty() {
            return object_literal_item_update_action(
                target_path,
                to,
                source.as_str(),
                source_binding_name,
                &field_kinds,
                context,
                locals,
                passed,
            );
        }
    }

    let Some(kind_hint) = infer_link_target_kind(to, context, locals, passed)? else {
        return Ok(None);
    };
    primitive_item_update_action(
        target_path,
        to,
        source.as_str(),
        source_binding_name,
        &kind_hint,
        context,
        locals,
        passed,
    )
}

fn infer_object_literal_field_kinds(
    expression: &StaticSpannedExpression,
    context: &LowerContext<'_>,
    locals: &LocalScopes<'_>,
    passed: &PassedScopes,
) -> Result<Option<BTreeMap<String, ObjectFieldKind>>, String> {
    let body = match &expression.node {
        StaticExpression::Then { body } => body.as_ref(),
        StaticExpression::When { arms } => arms
            .iter()
            .find_map(|arm| (!matches!(arm.pattern, static_expression::Pattern::WildCard)).then_some(&arm.body))
            .unwrap_or_else(|| arms.first().map(|arm| &arm.body).unwrap_or(expression)),
        _ => expression,
    };
    let body = resolve_alias(body, context, locals, passed, &mut Vec::new())?;
    let Some(object) = resolve_object(body) else {
        return Ok(None);
    };
    let mut kinds = BTreeMap::new();
    let mut scoped_locals = locals.clone();
    let scope = object
        .variables
        .iter()
        .filter_map(|variable| {
            let name = variable.node.name.as_str();
            (!name.is_empty()).then_some((
                name.to_string(),
                LocalBinding {
                    expr: Some(&variable.node.value),
                    object_base: infer_argument_object_base(&variable.node.value, context, locals, passed),
                },
            ))
        })
        .collect::<BTreeMap<_, _>>();
    scoped_locals.push(scope);
    for variable in &object.variables {
        if variable.node.name.is_empty() {
            continue;
        }
        if let Some(kind) =
            infer_object_field_kind(&variable.node.value, context, &mut Vec::new(), &mut scoped_locals, &mut passed.clone())?
        {
            kinds.insert(variable.node.name.as_str().to_string(), kind);
        }
    }
    Ok(Some(kinds))
}

fn infer_link_target_kind(
    expression: &StaticSpannedExpression,
    context: &LowerContext<'_>,
    locals: &LocalScopes<'_>,
    passed: &PassedScopes,
) -> Result<Option<ObjectFieldKind>, String> {
    let body = match &expression.node {
        StaticExpression::Then { body } => body.as_ref(),
        StaticExpression::When { arms } => {
            let mut selected = None;
            for arm in arms {
                if matches!(arm.pattern, static_expression::Pattern::WildCard) {
                    continue;
                }
                selected = Some(&arm.body);
                break;
            }
            selected.or_else(|| arms.first().map(|arm| &arm.body)).unwrap_or(expression)
        }
        _ => expression,
    };
    let inferred = infer_object_field_kind(
        body,
        context,
        &mut Vec::new(),
        &mut locals.clone(),
        &mut passed.clone(),
    )?;
    if inferred.is_some() {
        return Ok(inferred);
    }
    let mut stack = Vec::new();
    let mut scoped_locals = locals.clone();
    let mut scoped_passed = passed.clone();
    if lower_text_input_value_body(
        body,
        context,
        &mut stack,
        &mut scoped_locals,
        &mut scoped_passed,
    )
    .is_ok()
    {
        return Ok(Some(ObjectFieldKind::Text));
    }
    Ok(None)
}

fn augment_linked_text_input_runtime(context: &mut LowerContext<'_>) -> Result<(), String> {
    let Some(document) = context.bindings.get("document").copied() else {
        return Ok(());
    };
    let mut plans = Vec::new();
    collect_linked_text_input_plans(
        document,
        context,
        &mut Vec::new(),
        &mut Vec::new(),
        &mut Vec::new(),
        None,
        &mut plans,
    )?;
    for plan in plans {
        context
            .text_plan
            .initial_values
            .entry(plan.binding.clone())
            .or_insert(plan.initial_value);
        for (trigger, update) in plan.event_updates {
            let entry = context.text_plan.event_updates.entry(trigger).or_default();
            if !entry.contains(&update) {
                entry.push(update);
            }
        }
    }
    Ok(())
}

fn collect_linked_text_input_plans<'a>(
    expression: &'a StaticSpannedExpression,
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
    current_binding: Option<&str>,
    plans: &mut Vec<LinkedTextInputPlan>,
) -> Result<(), String> {
    if let Some(binding_name) = alias_binding_name(expression)? {
        let resolved = match resolve_named_binding(binding_name, context, locals, stack) {
            Ok(resolved) => resolved,
            Err(error) if error.starts_with("unknown top-level binding") => return Ok(()),
            Err(error) => return Err(error),
        };
        let result = collect_linked_text_input_plans(
            resolved,
            context,
            stack,
            locals,
            passed,
            current_binding,
            plans,
        );
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
                return collect_linked_text_input_plans(
                    from,
                    context,
                    stack,
                    locals,
                    passed,
                    Some(target_path.as_str()),
                    plans,
                );
            }
            if let Some((function_name, arguments)) =
                function_invocation_target(to, context, locals, passed, stack)?
            {
                return with_invoked_function_scope(
                    function_name,
                    arguments,
                    Some(from.as_ref()),
                    context,
                    stack,
                    locals,
                    passed,
                    |body, context, stack, locals, passed| {
                        collect_linked_text_input_plans(
                            body,
                            context,
                            stack,
                            locals,
                            passed,
                            current_binding,
                            plans,
                        )
                    },
                );
            }
            collect_linked_text_input_plans(
                from,
                context,
                stack,
                locals,
                passed,
                current_binding,
                plans,
            )?;
            collect_linked_text_input_plans(
                to,
                context,
                stack,
                locals,
                passed,
                current_binding,
                plans,
            )?;
        }
        StaticExpression::Block { variables, output } => {
            let scope = eager_block_scope(variables, context, stack, locals, passed);
            locals.push(scope);
            let result = collect_linked_text_input_plans(
                output,
                context,
                stack,
                locals,
                passed,
                current_binding,
                plans,
            );
            locals.pop();
            result?;
        }
        StaticExpression::FunctionCall { path, arguments }
            if resolved_function_name_for_path_in_stack(path, context, stack).is_some() =>
        {
            with_invoked_function_scope(
                resolved_function_name_for_path_in_stack(path, context, stack)
                    .expect("guard ensured function resolution"),
                arguments,
                None,
                context,
                stack,
                locals,
                passed,
                |body, context, stack, locals, passed| {
                    collect_linked_text_input_plans(
                        body,
                        context,
                        stack,
                        locals,
                        passed,
                        current_binding,
                        plans,
                    )
                },
            )?;
        }
        StaticExpression::FunctionCall { path, arguments }
            if path_matches(path, &["Document", "new"])
                || path_matches(path, &["Scene", "new"]) =>
        {
            if let Some(root) = find_named_argument(arguments, "root") {
                collect_linked_text_input_plans(
                    root,
                    context,
                    stack,
                    locals,
                    passed,
                    current_binding,
                    plans,
                )?;
            }
        }
        StaticExpression::FunctionCall { path, arguments }
            if path_matches_element(path, "text_input") =>
        {
            if let Some(binding) = current_binding {
                if let Some(plan) =
                    linked_text_input_plan(binding, arguments, context, stack, locals, passed)?
                {
                    plans.push(plan);
                }
            }
        }
        StaticExpression::FunctionCall { path, arguments }
            if path_matches_element(path, "stripe") =>
        {
            if let Some(items) = find_named_argument(arguments, "items") {
                collect_linked_text_input_plans(
                    items,
                    context,
                    stack,
                    locals,
                    passed,
                    current_binding,
                    plans,
                )?;
            }
        }
        StaticExpression::FunctionCall { path, arguments }
            if path_matches_element(path, "container")
                || path_matches_element(path, "block") =>
        {
            if let Some(child) = find_named_argument(arguments, "child") {
                collect_linked_text_input_plans(
                    child,
                    context,
                    stack,
                    locals,
                    passed,
                    current_binding,
                    plans,
                )?;
            }
        }
        StaticExpression::FunctionCall { path, arguments }
            if path_matches_element(path, "paragraph") =>
        {
            if let Some(contents) = find_named_argument(arguments, "contents") {
                collect_linked_text_input_plans(
                    contents,
                    context,
                    stack,
                    locals,
                    passed,
                    current_binding,
                    plans,
                )?;
            }
        }
        StaticExpression::FunctionCall { path, arguments }
            if path_matches_element(path, "label")
                || path_matches_element(path, "button")
                || path_matches_element(path, "link")
                || path_matches_element(path, "checkbox")
                || path_matches_element(path, "text") =>
        {
            if let Some(label) = find_named_argument(arguments, "label")
                .or_else(|| find_named_argument(arguments, "icon"))
            {
                collect_linked_text_input_plans(
                    label,
                    context,
                    stack,
                    locals,
                    passed,
                    current_binding,
                    plans,
                )?;
            }
        }
        StaticExpression::FunctionCall { path, arguments }
            if path_matches(path, &["Element", "svg"]) =>
        {
            if let Some(children) = find_named_argument(arguments, "children") {
                collect_linked_text_input_plans(
                    children,
                    context,
                    stack,
                    locals,
                    passed,
                    current_binding,
                    plans,
                )?;
            }
        }
        StaticExpression::List { items } => {
            for item in items {
                collect_linked_text_input_plans(
                    item,
                    context,
                    stack,
                    locals,
                    passed,
                    current_binding,
                    plans,
                )?;
            }
        }
        StaticExpression::When { arms } | StaticExpression::While { arms } => {
            for arm in arms {
                collect_linked_text_input_plans(
                    &arm.body,
                    context,
                    stack,
                    locals,
                    passed,
                    current_binding,
                    plans,
                )?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn linked_text_input_plan<'a>(
    binding: &str,
    arguments: &'a [static_expression::Spanned<StaticArgument>],
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<Option<LinkedTextInputPlan>, String> {
    let Some(text) = find_named_argument(arguments, "text") else {
        return Ok(None);
    };
    let text_binding = format!("{binding}.text");
    if let Some((initial_value, event_updates)) =
        latest_text_spec_for_expression(text, &context.path_bindings, &text_binding)?
    {
        return Ok(Some(LinkedTextInputPlan {
            binding: text_binding,
            initial_value,
            event_updates,
        }));
    }
    if let Some((initial_value, event_updates)) =
        partial_latest_text_spec_for_expression(text, &context.path_bindings, &text_binding)?
    {
        return Ok(Some(LinkedTextInputPlan {
            binding: text_binding,
            initial_value,
            event_updates,
        }));
    }
    if let Some((initial_value, event_updates)) =
        hold_text_spec_for_expression(text, &context.path_bindings, &text_binding)?
    {
        return Ok(Some(LinkedTextInputPlan {
            binding: text_binding,
            initial_value,
            event_updates,
        }));
    }
    let initial_value = lower_text_input_initial_value(text, context, stack, locals, passed)?;
    Ok(Some(LinkedTextInputPlan {
        binding: text_binding,
        initial_value,
        event_updates: Vec::new(),
    }))
}

fn partial_latest_text_spec_for_expression(
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
        if let Some(update) = text_event_update(input, path_bindings, binding_path)? {
            updates.push(update);
        }
    }

    if updates.is_empty() {
        return Ok(None);
    }

    Ok(Some((initial_value.unwrap_or_default(), updates)))
}

fn augment_dependent_object_list_updates_from_item_bindings(
    context: &mut LowerContext<'_>,
) -> Result<(), String> {
    let dependent_updates = context
        .path_bindings
        .iter()
        .filter_map(|(binding_name, expression)| {
            detect_dependent_bound_object_list_updates(
                expression,
                &context.path_bindings,
                &context.functions,
                binding_name,
            )
            .transpose()
        })
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();

    if dependent_updates.is_empty() {
        return Ok(());
    }

    for specs in context.object_list_plan.item_actions.values_mut() {
        for spec in specs {
            let ObjectItemActionKind::UpdateBindings {
                scalar_updates,
                text_updates,
                object_list_updates,
                payload_filter,
            } = &mut spec.action
            else {
                continue;
            };

            let mut updated_bindings = scalar_updates
                .iter()
                .map(|update| match update {
                    ItemScalarUpdate::SetStatic { binding, .. }
                    | ItemScalarUpdate::SetFromField { binding, .. } => binding.clone(),
                })
                .collect::<BTreeSet<_>>();
            updated_bindings.extend(text_updates.iter().map(|update| match update {
                ItemTextUpdate::SetStatic { binding, .. }
                | ItemTextUpdate::SetFromField { binding, .. }
                | ItemTextUpdate::SetFromPayload { binding }
                | ItemTextUpdate::SetFromValueSource { binding, .. }
                | ItemTextUpdate::SetFromInputSource { binding, .. } => binding.clone(),
            }));
            let mut pending_bindings = updated_bindings.iter().cloned().collect::<Vec<_>>();
            while let Some(current) = pending_bindings.pop() {
                for targets in [
                    context.scalar_mirrors.get(&current),
                    context.text_mirrors.get(&current),
                ]
                .into_iter()
                .flatten()
                {
                    for target in targets {
                        if updated_bindings.insert(target.clone()) {
                            pending_bindings.push(target.clone());
                        }
                    }
                }
            }

            for update in &dependent_updates {
                let required = required_bindings_for_append_bound_object(update);
                if required.is_empty()
                    || !required
                        .iter()
                        .all(|binding| updated_bindings.contains(binding))
                {
                    continue;
                }
                let filtered_update =
                    with_object_list_update_payload_filter(update.clone(), payload_filter.clone());
                if !object_list_updates.contains(&filtered_update) {
                    object_list_updates.push(filtered_update);
                }
            }
        }
    }

    Ok(())
}

fn detect_dependent_bound_object_list_updates<'a>(
    expression: &'a StaticSpannedExpression,
    path_bindings: &BTreeMap<String, &'a StaticSpannedExpression>,
    functions: &BTreeMap<String, FunctionSpec<'a>>,
    binding_path: &str,
) -> Result<Option<Vec<ObjectListUpdate>>, String> {
    let StaticExpression::Pipe { to, .. } = &expression.node else {
        return Ok(None);
    };
    let StaticExpression::Hold { body, .. } = &to.node else {
        return Ok(None);
    };

    let mut updates = Vec::new();
    let inputs = match &body.node {
        StaticExpression::Pipe { .. } => std::slice::from_ref(body.as_ref()),
        StaticExpression::Latest { inputs } => inputs.as_slice(),
        _ => return Ok(None),
    };

    for input in inputs {
        let StaticExpression::Pipe {
            to: trigger_then, ..
        } = &input.node
        else {
            return Ok(None);
        };
        let StaticExpression::Then { body } = &trigger_then.node else {
            return Ok(None);
        };
        collect_bound_object_append_updates(body, path_bindings, functions, binding_path, &mut updates)?;
    }

    Ok((!updates.is_empty()).then_some(updates))
}

fn collect_bound_object_append_updates<'a>(
    expression: &'a StaticSpannedExpression,
    path_bindings: &BTreeMap<String, &'a StaticSpannedExpression>,
    functions: &BTreeMap<String, FunctionSpec<'a>>,
    binding_path: &str,
    updates: &mut Vec<ObjectListUpdate>,
) -> Result<(), String> {
    if let Some(update) =
        detect_bound_object_append_update(expression, path_bindings, functions, binding_path)?
    {
        if !updates.contains(&update) {
            updates.push(update);
        }
    }

    match &expression.node {
        StaticExpression::Pipe { from, to } => {
            collect_bound_object_append_updates(from, path_bindings, functions, binding_path, updates)?;
            collect_bound_object_append_updates(to, path_bindings, functions, binding_path, updates)?;
        }
        StaticExpression::Then { body } => {
            collect_bound_object_append_updates(body, path_bindings, functions, binding_path, updates)?;
        }
        StaticExpression::Block { variables, output } => {
            for variable in variables {
                collect_bound_object_append_updates(
                    &variable.node.value,
                    path_bindings,
                    functions,
                    binding_path,
                    updates,
                )?;
            }
            collect_bound_object_append_updates(output, path_bindings, functions, binding_path, updates)?;
        }
        StaticExpression::When { arms } | StaticExpression::While { arms } => {
            for arm in arms {
                collect_bound_object_append_updates(
                    &arm.body,
                    path_bindings,
                    functions,
                    binding_path,
                    updates,
                )?;
            }
        }
        StaticExpression::Latest { inputs } | StaticExpression::List { items: inputs } => {
            for input in inputs {
                collect_bound_object_append_updates(
                    input,
                    path_bindings,
                    functions,
                    binding_path,
                    updates,
                )?;
            }
        }
        StaticExpression::FunctionCall { path, arguments }
            if resolved_function_name_in(path, functions).is_some() =>
        {
            let Some(function_name) = resolved_function_name_in(path, functions) else {
                return Ok(());
            };
            let Some(function) = functions.get(function_name) else {
                return Ok(());
            };
            collect_bound_object_append_updates(
                function.body,
                path_bindings,
                functions,
                binding_path,
                updates,
            )?;
            for argument in arguments {
                if let Some(value) = argument.node.value.as_ref() {
                    collect_bound_object_append_updates(
                        value,
                        path_bindings,
                        functions,
                        binding_path,
                        updates,
                    )?;
                }
            }
        }
        StaticExpression::FunctionCall { arguments, .. } => {
            for argument in arguments {
                if let Some(value) = argument.node.value.as_ref() {
                    collect_bound_object_append_updates(
                        value,
                        path_bindings,
                        functions,
                        binding_path,
                        updates,
                    )?;
                }
            }
        }
        StaticExpression::Object(object) => {
            for variable in &object.variables {
                collect_bound_object_append_updates(
                    &variable.node.value,
                    path_bindings,
                    functions,
                    binding_path,
                    updates,
                )?;
            }
        }
        _ => {}
    }

    Ok(())
}

fn required_bindings_for_append_bound_object(update: &ObjectListUpdate) -> BTreeSet<String> {
    let ObjectListUpdate::AppendBoundObject {
        scalar_bindings,
        text_bindings,
        ..
    } = update
    else {
        return BTreeSet::new();
    };

    scalar_bindings
        .values()
        .chain(text_bindings.values())
        .cloned()
        .collect()
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
            let scope = object
                .variables
                .iter()
                .filter_map(|variable| {
                    let name = variable.node.name.as_str();
                    (!name.is_empty()).then_some((
                        name.to_string(),
                        LocalBinding {
                            expr: Some(&variable.node.value),
                            object_base: None,
                        },
                    ))
                })
                .collect::<BTreeMap<_, _>>();
            locals.push(scope);
            for variable in &object.variables {
                if variable.node.name.is_empty() {
                    continue;
                }
                let field_name = variable.node.name.as_str().to_string();
                if let Some(kind) =
                    infer_object_field_kind(&variable.node.value, context, stack, locals, passed)?
                {
                    kinds.insert(field_name, kind);
                }
            }
            locals.pop();
        }
        StaticExpression::Pipe { from, to }
            if matches!(&to.node, StaticExpression::Hold { .. }) =>
        {
            return detect_initial_object_field_kinds(from, context, stack, locals, passed);
        }
        StaticExpression::Pipe { from, to } => {
            if let StaticExpression::Then { body } = &to.node {
                if let Some(source_binding_name) = alias_binding_name(from)? {
                    let mut scope = BTreeMap::new();
                    scope.insert(
                        source_binding_name.to_string(),
                        LocalBinding {
                            expr: None,
                            object_base: Some("__item__".to_string()),
                        },
                    );
                    locals.push(scope);
                    let next =
                        detect_initial_object_field_kinds(body, context, stack, locals, passed)?;
                    locals.pop();
                    kinds.extend(next);
                    return Ok(kinds);
                }
                let next = detect_initial_object_field_kinds(body, context, stack, locals, passed)?;
                kinds.extend(next);
                return Ok(kinds);
            }
            if let StaticExpression::When { arms } = &to.node {
                for arm in arms {
                    let next = detect_initial_object_field_kinds(
                        &arm.body, context, stack, locals, passed,
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
            if resolved_function_name_for_path_in_stack(path, context, stack).is_some() =>
        {
            with_invoked_function_scope(
                resolved_function_name_for_path_in_stack(path, context, stack)
                    .expect("guard ensured function resolution"),
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

fn infer_object_field_kind<'a>(
    expression: &'a StaticSpannedExpression,
    context: &'a LowerContext<'a>,
    stack: &mut Vec<String>,
    locals: &mut LocalScopes<'a>,
    passed: &mut PassedScopes,
) -> Result<Option<ObjectFieldKind>, String> {
    let expression = resolve_alias(expression, context, locals, passed, stack)?;
    if extract_integer_literal_opt(expression)?.is_some()
        || extract_bool_literal_opt(expression)?.is_some()
    {
        return Ok(Some(ObjectFieldKind::Scalar));
    }
    if matches!(
        &expression.node,
        StaticExpression::Literal(static_expression::Literal::Tag(tag)) if tag.as_str() == "None"
    ) {
        return Ok(Some(ObjectFieldKind::Scalar));
    }
    if extract_static_text_value(expression)?.is_some()
        || is_event_text_expression(expression, context, locals, passed)?
        || expression_path(expression, context, locals, passed)?
            .as_deref()
            .is_some_and(|path| path.ends_with(".text"))
    {
        return Ok(Some(ObjectFieldKind::Text));
    }
    if placeholder_object_field(expression, context, locals, passed)?.is_some()
        || expression_path(expression, context, locals, passed)?
            .as_deref()
            .is_some_and(|path| {
                path.ends_with(".row") || path.ends_with(".column") || path.ends_with(".id")
            })
    {
        return Ok(Some(ObjectFieldKind::Scalar));
    }
    match &expression.node {
        StaticExpression::Pipe { from, to }
            if matches!(&to.node, StaticExpression::Hold { .. }) =>
        {
            return infer_object_field_kind(from, context, stack, locals, passed);
        }
        StaticExpression::Pipe { from, to } => {
            if let StaticExpression::Then { body } = &to.node {
                return infer_object_field_kind(body, context, stack, locals, passed);
            }
            if let StaticExpression::When { arms } | StaticExpression::While { arms } = &to.node {
                for arm in arms {
                    if let Some(kind) =
                        infer_object_field_kind(&arm.body, context, stack, locals, passed)?
                    {
                        return Ok(Some(kind));
                    }
                }
                return Ok(None);
            }
            if let StaticExpression::FunctionCall { path, arguments } = &to.node {
                if path_matches(path, &["List", "latest"]) && arguments.is_empty() {
                    return infer_object_field_kind(from, context, stack, locals, passed);
                }
                if path_matches(path, &["List", "map"]) {
                    let Some(mapper_name) = find_positional_parameter_name(arguments) else {
                        return Ok(None);
                    };
                    let Some(new) = find_named_argument(arguments, "new") else {
                        return Ok(None);
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
                    let inferred = infer_object_field_kind(new, context, stack, locals, passed);
                    locals.pop();
                    return inferred;
                }
            }
            return Ok(None);
        }
        StaticExpression::Latest { inputs } => {
            for input in inputs {
                if let Some(kind) = infer_object_field_kind(input, context, stack, locals, passed)?
                {
                    return Ok(Some(kind));
                }
            }
            return Ok(None);
        }
        StaticExpression::FunctionCall { path, arguments }
            if resolved_function_name_for_path_in_stack(path, context, stack).is_some() =>
        {
            return with_invoked_function_scope(
                resolved_function_name_for_path_in_stack(path, context, stack)
                    .expect("guard ensured function resolution"),
                arguments,
                None,
                context,
                stack,
                locals,
                passed,
                |body, context, stack, locals, passed| {
                    infer_object_field_kind(body, context, stack, locals, passed)
                },
            );
        }
        _ => {}
    }
    Ok(None)
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
                    output.entry(list_binding.to_string()).or_default().push(
                        ObjectItemActionSpec {
                            source_binding_suffix: source_suffix.to_string(),
                            kind,
                            action: ObjectItemActionKind::UpdateBindings {
                                scalar_updates: vec![ItemScalarUpdate::SetStatic {
                                    binding: target_binding.to_string(),
                                    value: i64::from(value),
                                }],
                                text_updates: Vec::new(),
                                object_list_updates: Vec::new(),
                                payload_filter,
                            },
                        },
                    );
                }
                return Ok(output);
            }
        }
        StaticExpression::FunctionCall { path, arguments }
            if resolved_function_name_for_path_in_stack(path, context, stack).is_some() =>
        {
            let next = with_invoked_function_scope(
                resolved_function_name_for_path_in_stack(path, context, stack)
                    .expect("guard ensured function resolution"),
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
        StaticExpression::Then { body } => {
            Ok(extract_bool_literal_opt(body)?.map(|value| (value, None)))
        }
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
            let scope = object
                .variables
                .iter()
                .filter_map(|variable| {
                    let name = variable.node.name.as_str();
                    let binding = format!("{target_base}.{name}");
                    (!name.is_empty()).then_some((
                        name.to_string(),
                        LocalBinding {
                            expr: Some(&variable.node.value),
                            object_base: Some(binding),
                        },
                    ))
                })
                .collect::<BTreeMap<_, _>>();
            locals.push(scope);
            for variable in &object.variables {
                if variable.node.name.is_empty() {
                    continue;
                }
                let field_name = variable.node.name.as_str();
                let binding = format!("{target_base}.{field_name}");
                if let Some(kind) = field_kinds.get(field_name) {
                    match kind {
                        ObjectFieldKind::Scalar => {
                            if let Some(value) = extract_integer_literal_opt(&variable.node.value)?
                                .or(extract_bool_literal_opt(&variable.node.value)?.map(i64::from))
                            {
                                plan.scalar_initials.insert(binding.clone(), value);
                            }
                        }
                        ObjectFieldKind::Text => {
                            if let Some(value) = extract_static_text_value(&variable.node.value)? {
                                plan.text_initials.insert(binding.clone(), value);
                            }
                        }
                    }
                }
                let next = detect_top_level_object_field_plan(
                    &binding,
                    &variable.node.value,
                    context,
                    field_kinds,
                    stack,
                    locals,
                    passed,
                    current_object_list_binding,
                )?;
                merge_top_level_object_field_plan(&mut plan, next);
            }
            locals.pop();
        }
        StaticExpression::Pipe { from, to }
            if matches!(&to.node, StaticExpression::Hold { .. }) =>
        {
            let initial = detect_top_level_object_field_plan(
                target_base,
                from,
                context,
                field_kinds,
                stack,
                locals,
                passed,
                current_object_list_binding,
            )?;
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
            if matches!(&to.node, StaticExpression::LinkSetter { .. }) {
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
            if let Some(source_binding_name) = alias_binding_name(from)? {
                let resolved = resolve_named_binding(source_binding_name, context, locals, stack)?;
                let rewritten = match &to.node {
                    StaticExpression::Then { body } => {
                        Some(rewrite_top_level_object_field_plan_from_body(
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
                        )?)
                    }
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
                let source_binding_name = alias_binding_name(from)?;
                if let Some(action) = object_literal_item_update_action(
                    target_base,
                    to,
                    source.as_str(),
                    source_binding_name,
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
                if let (Some(list_binding), Some(kind_hint)) = (
                    current_object_list_binding,
                    target_base
                        .rsplit('.')
                        .next()
                        .and_then(|field_name| field_kinds.get(field_name)),
                ) {
                    if let Some(action) = primitive_item_update_action(
                        target_base,
                        to,
                        source.as_str(),
                        source_binding_name,
                        kind_hint,
                        context,
                        locals,
                        passed,
                    )? {
                        plan.item_actions
                            .entry(list_binding.to_string())
                            .or_default()
                            .push(action);
                        return Ok(plan);
                    }
                }
            }
            if current_object_list_binding.is_none() {
                if let Some(source) = detect_top_level_event_source(from, context, locals, passed)?
                {
                    let source_binding_name = alias_binding_name(from)?;
                    if let Some(action) = object_literal_item_update_action(
                        target_base,
                        to,
                        &format!("{}:{}", source.0, source.1),
                        source_binding_name,
                        field_kinds,
                        context,
                        locals,
                        passed,
                    )? {
                        merge_top_level_object_event_updates(&mut plan, source, action)?;
                        return Ok(plan);
                    }
                }
            }
        }
        StaticExpression::FunctionCall { path, arguments }
            if resolved_function_name_for_path_in_stack(path, context, stack).is_some() =>
        {
            let next = with_invoked_function_scope(
                resolved_function_name_for_path_in_stack(path, context, stack)
                    .expect("guard ensured function resolution"),
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
        scalar_event_updates: source_plan.scalar_event_updates,
        text_event_updates: source_plan.text_event_updates,
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
    let action_event_name = ui_event_name(&action.kind);
    let mut scoped_locals = locals.clone();
    scoped_locals.push(BTreeMap::from([(
        source_binding_name.to_string(),
        LocalBinding {
            expr: None,
            object_base: Some("__item__".to_string()),
        },
    )]));
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
                    placeholder_object_field(&variable.node.value, context, &scoped_locals, passed)?
                {
                    scalar_updates.push(ItemScalarUpdate::SetFromField { binding, field });
                    continue;
                }
            }
            ObjectFieldKind::Text => {
                if let Some(event_name) = action_event_name {
                    if let Some(update) = item_text_update_from_expression(
                        binding,
                        &variable.node.value,
                        context,
                        &scoped_locals,
                        passed,
                        &action.source_binding_suffix,
                        event_name,
                    )? {
                        text_updates.push(update);
                        continue;
                    }
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
            object_list_updates: Vec::new(),
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
    for (trigger, updates) in next.scalar_event_updates {
        into.scalar_event_updates
            .entry(trigger)
            .or_default()
            .extend(updates);
    }
    for (trigger, updates) in next.text_event_updates {
        into.text_event_updates
            .entry(trigger)
            .or_default()
            .extend(updates);
    }
    for (binding, actions) in next.item_actions {
        into.item_actions
            .entry(binding)
            .or_default()
            .extend(actions);
    }
}

fn merge_top_level_object_event_updates(
    plan: &mut TopLevelObjectFieldPlan,
    source: (String, String),
    action: ObjectItemActionSpec,
) -> Result<(), String> {
    let ObjectItemActionKind::UpdateBindings {
        scalar_updates,
        text_updates,
        payload_filter,
        ..
    } = action.action
    else {
        return Ok(());
    };
    let trigger = source;
    for update in scalar_updates {
        match update {
            ItemScalarUpdate::SetStatic { binding, value } => {
                plan.scalar_event_updates
                    .entry(trigger.clone())
                    .or_default()
                    .push(payload_filter.clone().map_or(
                        ScalarUpdate::Set {
                            binding: binding.clone(),
                            value,
                        },
                        |payload_filter| ScalarUpdate::SetFiltered {
                            binding,
                            value,
                            payload_filter,
                        },
                    ));
            }
            ItemScalarUpdate::SetFromField { .. } => {}
        }
    }
    for update in text_updates {
        let converted = match update {
            ItemTextUpdate::SetStatic { binding, value } => TextUpdate::SetStatic {
                binding,
                value,
                payload_filter: payload_filter.clone(),
            },
            ItemTextUpdate::SetFromField { .. } => continue,
            ItemTextUpdate::SetFromPayload { binding } => TextUpdate::SetFromPayload { binding },
            ItemTextUpdate::SetFromInputSource {
                binding,
                source_suffix,
            } => TextUpdate::SetFromInput {
                binding,
                source_binding: source_suffix,
                payload_filter: payload_filter.clone(),
            },
            ItemTextUpdate::SetFromValueSource { binding, value } => {
                TextUpdate::SetFromValueSource {
                    binding,
                    value,
                    payload_filter: payload_filter.clone(),
                }
            }
        };
        plan.text_event_updates
            .entry(trigger.clone())
            .or_default()
            .push(converted);
    }
    Ok(())
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
    if event_index == 1
        && lookup_local_binding_expr(parts[0].as_str(), locals).is_some()
        && lookup_local_object_base(parts[0].as_str(), locals).is_some()
    {
        return Ok(Some(format!(
            "{}:{}",
            parts[0].as_str(),
            parts[event_index + 1].as_str()
        )));
    }
    let path = canonical_without_passed_path(&parts[..event_index], context, locals, passed)?;
    let Some(suffix) = path.strip_prefix("__item__.") else {
        return Ok(None);
    };
    Ok(Some(format!(
        "{suffix}:{}",
        parts[event_index + 1].as_str()
    )))
}

fn detect_top_level_event_source(
    expression: &StaticSpannedExpression,
    context: &LowerContext<'_>,
    locals: &LocalScopes<'_>,
    passed: &PassedScopes,
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
        let index = parts.len() - 2;
        if parts[index].as_str() != "event" {
            return Ok(None);
        }
        index
    };
    let path = canonical_without_passed_path(&parts[..event_index], context, locals, passed)?;
    Ok(Some((path, parts[event_index + 1].as_str().to_string())))
}

fn item_text_update_from_expression<'a>(
    binding: String,
    expression: &'a StaticSpannedExpression,
    context: &'a LowerContext<'a>,
    locals: &LocalScopes<'a>,
    passed: &PassedScopes,
    source_suffix: &str,
    event_name: &str,
) -> Result<Option<ItemTextUpdate>, String> {
    if let Some(value) = extract_static_text_value(expression)? {
        return Ok(Some(ItemTextUpdate::SetStatic { binding, value }));
    }
    if item_event_text_source(
        expression,
        context,
        locals,
        passed,
        source_suffix,
        event_name,
    )? {
        return Ok(Some(match event_name {
            "key_down" => ItemTextUpdate::SetFromInputSource {
                binding,
                source_suffix: source_suffix.to_string(),
            },
            _ => ItemTextUpdate::SetFromPayload { binding },
        }));
    }
    if let Some(field) = placeholder_object_field(expression, context, locals, passed)? {
        return Ok(Some(ItemTextUpdate::SetFromField { binding, field }));
    }
    if let Some(field) = equivalent_local_object_text_field(expression, context, locals, passed)? {
        return Ok(Some(ItemTextUpdate::SetFromField { binding, field }));
    }
    let mut stack = Vec::new();
    let mut scoped_locals = locals.clone();
    let mut scoped_passed = passed.clone();
    let Ok(value) = lower_text_input_value_body(
        expression,
        context,
        &mut stack,
        &mut scoped_locals,
        &mut scoped_passed,
    ) else {
        return Ok(None);
    };
    Ok(Some(ItemTextUpdate::SetFromValueSource { binding, value }))
}

fn equivalent_local_object_text_field(
    expression: &StaticSpannedExpression,
    context: &LowerContext<'_>,
    locals: &LocalScopes<'_>,
    passed: &PassedScopes,
) -> Result<Option<String>, String> {
    let resolved_expression = resolve_alias(expression, context, locals, passed, &mut Vec::new())?;
    let target = describe_expression_detailed(resolved_expression);
    for scope in locals.iter().rev() {
        for (name, binding) in scope {
            if binding.object_base.as_deref() != Some("__item__") {
                continue;
            }
            let Some(candidate_expr) = binding.expr else {
                continue;
            };
            let Some(field) = local_object_field_name(name, context, locals, passed)? else {
                continue;
            };
            let resolved_candidate =
                resolve_alias(candidate_expr, context, locals, passed, &mut Vec::new())?;
            if describe_expression_detailed(resolved_candidate) == target {
                return Ok(Some(field));
            }
        }
    }
    Ok(None)
}

fn object_literal_item_update_action(
    target_base: &str,
    expression: &StaticSpannedExpression,
    source: &str,
    source_binding_name: Option<&str>,
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
    let mut scoped_locals = locals.clone();
    if let Some(source_binding_name) = source_binding_name {
        scoped_locals.push(BTreeMap::from([(
            source_binding_name.to_string(),
            LocalBinding {
                expr: None,
                object_base: Some("__item__".to_string()),
            },
        )]));
    }
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
                    placeholder_object_field(&variable.node.value, context, &scoped_locals, passed)?
                {
                    scalar_updates.push(ItemScalarUpdate::SetFromField { binding, field });
                }
            }
            ObjectFieldKind::Text => {
                if let Some(update) = item_text_update_from_expression(
                    binding,
                    &variable.node.value,
                    context,
                    &scoped_locals,
                    passed,
                    source_suffix,
                    event_name,
                )? {
                    text_updates.push(update);
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
            object_list_updates: Vec::new(),
            payload_filter,
        },
    }))
}

fn primitive_item_update_action(
    target_binding: &str,
    expression: &StaticSpannedExpression,
    source: &str,
    source_binding_name: Option<&str>,
    kind_hint: &ObjectFieldKind,
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
    let mut scoped_locals = locals.clone();
    if let Some(source_binding_name) = source_binding_name {
        scoped_locals.push(BTreeMap::from([(
            source_binding_name.to_string(),
            LocalBinding {
                expr: None,
                object_base: Some("__item__".to_string()),
            },
        )]));
    }
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

    match kind_hint {
        ObjectFieldKind::Scalar => {
            if let Some(value) = extract_integer_literal_opt(body)?
                .or(extract_bool_literal_opt(body)?.map(i64::from))
            {
                return Ok(Some(ObjectItemActionSpec {
                    source_binding_suffix: source_suffix.to_string(),
                    kind,
                    action: ObjectItemActionKind::UpdateBindings {
                        scalar_updates: vec![ItemScalarUpdate::SetStatic {
                            binding: target_binding.to_string(),
                            value,
                        }],
                        text_updates: Vec::new(),
                        object_list_updates: Vec::new(),
                        payload_filter,
                    },
                }));
            }
            if let Some(field) = placeholder_object_field(body, context, &scoped_locals, passed)? {
                return Ok(Some(ObjectItemActionSpec {
                    source_binding_suffix: source_suffix.to_string(),
                    kind,
                    action: ObjectItemActionKind::UpdateBindings {
                        scalar_updates: vec![ItemScalarUpdate::SetFromField {
                            binding: target_binding.to_string(),
                            field,
                        }],
                        text_updates: Vec::new(),
                        object_list_updates: Vec::new(),
                        payload_filter,
                    },
                }));
            }
        }
        ObjectFieldKind::Text => {
            if let Some(update) = item_text_update_from_expression(
                target_binding.to_string(),
                body,
                context,
                &scoped_locals,
                passed,
                source_suffix,
                event_name,
            )? {
                return Ok(Some(ObjectItemActionSpec {
                    source_binding_suffix: source_suffix.to_string(),
                    kind,
                    action: ObjectItemActionKind::UpdateBindings {
                        scalar_updates: Vec::new(),
                        text_updates: vec![update],
                        object_list_updates: Vec::new(),
                        payload_filter,
                    },
                }));
            }
        }
    }

    Ok(None)
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
    if event_index == 1
        && lookup_local_binding_expr(parts[0].as_str(), locals).is_some()
        && lookup_local_object_base(parts[0].as_str(), locals).is_some()
    {
        return Ok(parts[0].as_str() == expected_suffix);
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
    let specialized_expression = specialize_static_object_item_expression(expression, functions)?;
    let resolved_expression = specialized_expression
        .as_ref()
        .map_or(expression, |value| value);
    let object = resolve_object(resolved_expression)
        .or_else(|| {
            resolve_static_object_runtime_expression(expression, functions)
                .ok()
                .flatten()
        })
        .ok_or_else(|| "expected static object item".to_string())?;
    let mut item = ObjectListItem {
        id,
        title: String::new(),
        completed: false,
        text_fields: BTreeMap::new(),
        bool_fields: BTreeMap::new(),
        scalar_fields: BTreeMap::new(),
        object_lists: BTreeMap::new(),
        nested_item_actions: BTreeMap::new(),
    };
    for variable in &object.variables {
        let name = variable.node.name.as_str();
        if name.is_empty() {
            continue;
        }
        let value = find_object_field(object, name)
            .or_else(|| resolve_static_object_field_expression(expression, functions, name))
            .unwrap_or(&variable.node.value);
        if name == "title" {
            let initial_title =
                resolve_initial_static_object_text_field(resolved_expression, functions, "title")?
                    .or_else(|| {
                        resolve_initial_static_object_text_field(value, functions, "title")
                            .ok()
                            .flatten()
                    })
                    .or_else(|| static_text_item(value).ok());
            if let Some(title) = initial_title {
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
        let initial_text =
            resolve_initial_static_object_text_field(resolved_expression, functions, name)?
                .or_else(|| {
                    resolve_initial_static_object_text_field(value, functions, name)
                        .ok()
                        .flatten()
                })
                .or_else(|| static_text_item(value).ok());
        if let Some(value) = initial_text {
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
            if resolved_function_name_in(path, functions).is_some() =>
        {
            let function_name = resolved_function_name_in(path, functions)?;
            let function = functions.get(function_name)?;
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
    scalar_plan: &ScalarPlan,
    text_plan: &TextPlan,
) -> Result<
    Option<(
        Vec<ObjectListItem>,
        ObjectListEventUpdates,
        Vec<ObjectItemActionSpec>,
        PendingGlobalObjectUpdates,
    )>,
    String,
> {
    if let Some(path) = canonical_reference_path(expression, path_bindings, binding_path)? {
        if let Some(source_expression) = path_bindings.get(&path).copied() {
            return detect_object_list_pipeline(
                source_expression,
                path_bindings,
                functions,
                binding_path,
                scalar_plan,
                text_plan,
            );
        }
    }
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
            let (initial_items, mut updates, mut item_actions, mut global_actions) =
                if let Some(detected) = detect_object_list_pipeline(
                    from,
                    path_bindings,
                    functions,
                    binding_path,
                    scalar_plan,
                    text_plan,
                )? {
                    detected
                } else if is_empty_static_list(from) {
                    (Vec::new(), Vec::new(), Vec::new(), Vec::new())
                } else {
                    return Ok(None);
                };
            if let StaticExpression::Hold { state_param, body } = &to.node {
                let hold_updates = detect_object_list_hold_updates(
                    body,
                    state_param.as_str(),
                    path_bindings,
                    binding_path,
                    functions,
                    scalar_plan,
                    text_plan,
                )?;
                if hold_updates.is_empty() {
                    return Ok(None);
                }
                updates.extend(hold_updates);
                return Ok(Some((initial_items, updates, item_actions, global_actions)));
            }
            let StaticExpression::FunctionCall { path, arguments } = &to.node else {
                return Ok(None);
            };
            if path_matches(path, &["List", "append"]) {
                let mut append_item_actions = Vec::new();
                let mut append_global_actions = Vec::new();
                let ((trigger_binding, event_name), update) = if let Some(item) =
                    find_named_argument(arguments, "item")
                {
                    if initial_items.is_empty()
                        && updates.is_empty()
                        && item_actions.is_empty()
                        && global_actions.is_empty()
                        && detect_text_list_append_update(item, path_bindings, binding_path).is_ok()
                    {
                        return Ok(None);
                    }
                    if let Some((actions, globals)) = detect_append_item_runtime_specs(
                        item,
                        path_bindings,
                        binding_path,
                        functions,
                    )?
                    {
                        append_item_actions = actions;
                        append_global_actions = globals;
                    }
                    detect_object_list_append_update(item, path_bindings, binding_path, functions)?
                } else if let Some(on) = find_named_argument(arguments, "on") {
                    detect_object_list_append_on_update(on, path_bindings, binding_path)?
                } else {
                    return Err("List/append requires `item` or `on`".to_string());
                };
                extend_unique_object_item_actions(&mut item_actions, append_item_actions);
                extend_unique_pending_global_actions(&mut global_actions, append_global_actions);
                updates.push(((trigger_binding, event_name), update));
                return Ok(Some((initial_items, updates, item_actions, global_actions)));
            }
            if path_matches(path, &["List", "retain"]) {
                let item_name = find_positional_parameter_name(arguments)
                    .ok_or_else(|| "List/retain requires an item parameter name".to_string())?;
                let condition = find_named_argument(arguments, "if")
                    .ok_or_else(|| "List/retain requires `if`".to_string())?;
                let Some((retain_item_actions, retain_updates)) =
                    detect_dynamic_object_retain_actions(
                        condition,
                        item_name,
                        path_bindings,
                        binding_path,
                    )?
                else {
                    return Ok(None);
                };
                item_actions.extend(retain_item_actions);
                updates.extend(retain_updates);
                return Ok(Some((initial_items, updates, item_actions, global_actions)));
            }
            if path_matches(path, &["List", "remove_last"]) {
                let on = find_named_argument(arguments, "on")
                    .ok_or_else(|| "List/remove_last requires `on`".to_string())?;
                let ((trigger_binding, event_name), update) =
                    detect_object_list_remove_last_update(on, path_bindings, binding_path)?;
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

fn detect_append_item_runtime_specs<'a>(
    expression: &'a StaticSpannedExpression,
    path_bindings: &BTreeMap<String, &'a StaticSpannedExpression>,
    binding_path: &str,
    functions: &BTreeMap<String, FunctionSpec<'a>>,
) -> Result<Option<(Vec<ObjectItemActionSpec>, PendingGlobalObjectUpdates)>, String> {
    let resolved_expression = if let Some(path) =
        canonical_reference_path(expression, path_bindings, binding_path)?
    {
        path_bindings.get(&path).copied().unwrap_or(expression)
    } else {
        expression
    };
    let candidate = match &resolved_expression.node {
        StaticExpression::Pipe { to, .. } => match &to.node {
            StaticExpression::Then { body } => body.as_ref(),
            _ => to.as_ref(),
        },
        _ => resolved_expression,
    };
    let Some((_, item_actions, global_actions)) =
        detect_dynamic_object_list_item(candidate, functions, 0)?
    else {
        return Ok(None);
    };
    Ok(Some((item_actions, global_actions)))
}

fn extend_unique_object_item_actions(
    into: &mut Vec<ObjectItemActionSpec>,
    next: Vec<ObjectItemActionSpec>,
) {
    for action in next {
        if !into.contains(&action) {
            into.push(action);
        }
    }
}

fn extend_unique_pending_global_actions(
    into: &mut PendingGlobalObjectUpdates,
    next: PendingGlobalObjectUpdates,
) {
    for action in next {
        if !into.contains(&action) {
            into.push(action);
        }
    }
}

fn detect_object_list_hold_updates<'a>(
    expression: &'a StaticSpannedExpression,
    state_param: &str,
    path_bindings: &BTreeMap<String, &'a StaticSpannedExpression>,
    binding_path: &str,
    functions: &BTreeMap<String, FunctionSpec<'a>>,
    scalar_plan: &ScalarPlan,
    text_plan: &TextPlan,
) -> Result<ObjectListEventUpdates, String> {
    let mut updates = Vec::new();
    match &expression.node {
        StaticExpression::Pipe {
            from: trigger_source,
            to: trigger_then,
        } => {
            let Some(update) = detect_object_list_hold_update(
                trigger_source,
                trigger_then,
                state_param,
                path_bindings,
                binding_path,
                functions,
                scalar_plan,
                text_plan,
            )?
            else {
                return Ok(Vec::new());
            };
            updates.extend(update);
        }
        StaticExpression::Latest { inputs } => {
            for input in inputs {
                let StaticExpression::Pipe {
                    from: trigger_source,
                    to: trigger_then,
                } = &input.node
                else {
                    return Ok(Vec::new());
                };
                let Some(update) = detect_object_list_hold_update(
                    trigger_source,
                    trigger_then,
                    state_param,
                    path_bindings,
                    binding_path,
                    functions,
                    scalar_plan,
                    text_plan,
                )?
                else {
                    return Ok(Vec::new());
                };
                updates.extend(update);
            }
        }
        _ => return Ok(Vec::new()),
    }
    Ok(updates)
}

fn detect_object_list_hold_update<'a>(
    trigger_source: &'a StaticSpannedExpression,
    trigger_then: &'a StaticSpannedExpression,
    state_param: &str,
    path_bindings: &BTreeMap<String, &'a StaticSpannedExpression>,
    binding_path: &str,
    functions: &BTreeMap<String, FunctionSpec<'a>>,
    scalar_plan: &ScalarPlan,
    text_plan: &TextPlan,
) -> Result<Option<Vec<((String, String), ObjectListUpdate)>>, String> {
    let StaticExpression::Then { body } = &trigger_then.node else {
        return Ok(None);
    };
    let StaticExpression::Pipe { from, to } = &body.node else {
        return Ok(None);
    };
    if !is_state_alias_reference(from, state_param) {
        return Ok(None);
    }
    let StaticExpression::FunctionCall { path, arguments } = &to.node else {
        return Ok(None);
    };
    if !path_matches(path, &["List", "append"]) {
        return Ok(None);
    }
    let Some(item) = find_named_argument(arguments, "item") else {
        return Ok(None);
    };
    let update = if let Some(update) =
        detect_bound_object_append_update(item, path_bindings, functions, binding_path)?
    {
        update
    } else if let Some(((..), update)) = detect_static_object_append_update(
        trigger_source,
        &StaticSpannedExpression {
            span: body.span,
            persistence: body.persistence.clone(),
            node: StaticExpression::Then {
                body: Box::new(item.clone()),
            },
        },
        path_bindings,
        binding_path,
        functions,
    )? {
        match update {
            ObjectListUpdate::AppendObject { item, .. } => ObjectListUpdate::AppendObject {
                binding: binding_path.to_string(),
                item,
            },
            other => other,
        }
    } else {
        return Ok(None);
    };
    if let Some((trigger_binding, event_name)) =
        canonical_event_source_path(trigger_source, path_bindings, binding_path)?
    {
        return Ok(Some(vec![((trigger_binding, event_name), update)]));
    }

    if alias_binding_name(trigger_source)?.is_none() {
        return Ok(None);
    }
    let trigger_specs = trigger_specs_for_bound_object_append(&update, scalar_plan, text_plan);
    if trigger_specs.is_empty() {
        return Ok(None);
    }

    Ok(Some(
        trigger_specs
            .into_iter()
            .map(|trigger| {
                let update =
                    with_object_list_update_payload_filter(update.clone(), trigger.payload_filter);
                ((trigger.trigger_binding, trigger.event_name), update)
            })
            .collect(),
    ))
}

fn is_state_alias_reference(expression: &StaticSpannedExpression, state_param: &str) -> bool {
    matches!(
        &expression.node,
        StaticExpression::Alias(static_expression::Alias::WithoutPassed { parts, .. })
            if parts.len() == 1 && parts[0].as_str() == state_param
    )
}

fn detect_bound_object_append_update(
    expression: &StaticSpannedExpression,
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    functions: &BTreeMap<String, FunctionSpec<'_>>,
    binding_path: &str,
) -> Result<Option<ObjectListUpdate>, String> {
    let resolved_expression = if let Some(reference_path) =
        canonical_reference_path(expression, path_bindings, binding_path)?
    {
        path_bindings
            .get(&reference_path)
            .copied()
            .unwrap_or(expression)
    } else {
        expression
    };
    let candidate_expression = match &resolved_expression.node {
        StaticExpression::Pipe { to, .. } => match &to.node {
            StaticExpression::Then { body } => body.as_ref(),
            _ => resolved_expression,
        },
        _ => resolved_expression,
    };
    let Some(object) = resolve_static_object_runtime_expression(candidate_expression, functions)?
    else {
        return Ok(None);
    };
    let mut scalar_bindings = BTreeMap::new();
    let mut text_bindings = BTreeMap::new();
    for variable in &object.variables {
        let name = variable.node.name.as_str();
        if name.is_empty() {
            continue;
        }
        let Some(value_expression) =
            resolve_static_object_field_expression(candidate_expression, functions, name)
        else {
            continue;
        };
        let Some(scoped_binding) =
            append_bound_field_binding(value_expression, path_bindings, binding_path)
        else {
            continue;
        };
        if name == "title" || scoped_binding.ends_with(".text") {
            text_bindings.insert(name.to_string(), scoped_binding);
        } else {
            scalar_bindings.insert(name.to_string(), scoped_binding);
        }
    }
    if scalar_bindings.is_empty() && text_bindings.is_empty() {
        return Ok(None);
    }
    Ok(Some(ObjectListUpdate::AppendBoundObject {
        binding: binding_path.to_string(),
        scalar_bindings,
        text_bindings,
        payload_filter: None,
    }))
}

fn append_bound_field_binding(
    expression: &StaticSpannedExpression,
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    binding_path: &str,
) -> Option<String> {
    match &expression.node {
        StaticExpression::Alias(static_expression::Alias::WithoutPassed { parts, .. }) => {
            if parts.len() < 2 {
                return None;
            }
            let field = parts.last()?.as_str();
            let base_binding =
                canonical_parts_path(&parts[..parts.len() - 1], path_bindings, binding_path);
            Some(format!("{base_binding}.{field}"))
        }
        StaticExpression::Pipe { from, to }
            if matches!(&to.node, StaticExpression::Hold { .. }) =>
        {
            append_bound_field_binding(from, path_bindings, binding_path)
        }
        _ => None,
    }
}

fn with_object_list_update_payload_filter(
    update: ObjectListUpdate,
    payload_filter: Option<String>,
) -> ObjectListUpdate {
    match update {
        ObjectListUpdate::AppendBoundObject {
            binding,
            scalar_bindings,
            text_bindings,
            ..
        } => ObjectListUpdate::AppendBoundObject {
            binding,
            scalar_bindings,
            text_bindings,
            payload_filter,
        },
        other => other,
    }
}

fn trigger_specs_for_bound_object_append(
    update: &ObjectListUpdate,
    scalar_plan: &ScalarPlan,
    text_plan: &TextPlan,
) -> BTreeSet<TriggerSpec> {
    let ObjectListUpdate::AppendBoundObject {
        scalar_bindings,
        text_bindings,
        ..
    } = update
    else {
        return BTreeSet::new();
    };

    let mut all_bindings = scalar_bindings.values().cloned().collect::<Vec<_>>();
    all_bindings.extend(text_bindings.values().cloned());

    let mut triggers = None::<BTreeSet<TriggerSpec>>;
    for binding in all_bindings {
        let next = trigger_specs_for_runtime_binding(&binding, scalar_plan, text_plan);
        if next.is_empty() {
            return BTreeSet::new();
        }
        triggers = Some(match triggers {
            Some(existing) => existing.intersection(&next).cloned().collect(),
            None => next,
        });
    }

    triggers.unwrap_or_default()
}

fn trigger_specs_for_runtime_binding(
    binding: &str,
    scalar_plan: &ScalarPlan,
    text_plan: &TextPlan,
) -> BTreeSet<TriggerSpec> {
    let mut specs = BTreeSet::new();

    for ((trigger_binding, event_name), updates) in &scalar_plan.event_updates {
        for update in updates {
            match update {
                ScalarUpdate::Set {
                    binding: target, ..
                } if target == binding => {
                    specs.insert(TriggerSpec {
                        trigger_binding: trigger_binding.clone(),
                        event_name: event_name.clone(),
                        payload_filter: None,
                    });
                }
                ScalarUpdate::SetFiltered {
                    binding: target,
                    payload_filter,
                    ..
                } if target == binding => {
                    specs.insert(TriggerSpec {
                        trigger_binding: trigger_binding.clone(),
                        event_name: event_name.clone(),
                        payload_filter: Some(payload_filter.clone()),
                    });
                }
                _ => {}
            }
        }
    }

    for ((trigger_binding, event_name), updates) in &text_plan.event_updates {
        for update in updates {
            match update {
                TextUpdate::SetStatic {
                    binding: target,
                    payload_filter,
                    ..
                }
                | TextUpdate::SetComputed {
                    binding: target,
                    payload_filter,
                    ..
                }
                | TextUpdate::SetComputedBranch {
                    binding: target,
                    payload_filter,
                    ..
                }
                | TextUpdate::SetFromValueSource {
                    binding: target,
                    payload_filter,
                    ..
                }
                | TextUpdate::SetFromInput {
                    binding: target,
                    payload_filter,
                    ..
                } if target == binding => {
                    specs.insert(TriggerSpec {
                        trigger_binding: trigger_binding.clone(),
                        event_name: event_name.clone(),
                        payload_filter: payload_filter.clone(),
                    });
                }
                TextUpdate::SetFromPayload { binding: target } if target == binding => {
                    specs.insert(TriggerSpec {
                        trigger_binding: trigger_binding.clone(),
                        event_name: event_name.clone(),
                        payload_filter: None,
                    });
                }
                _ => {}
            }
        }
    }

    specs
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
    let specialized_expression = specialize_static_object_item_expression(expression, functions)?;
    let resolved_expression = specialized_expression
        .as_ref()
        .map_or(expression, |value| value);
    let object = resolve_object(resolved_expression)
        .or_else(|| {
            resolve_static_object_runtime_expression(expression, functions)
                .ok()
                .flatten()
        })
        .ok_or_else(|| "expected object runtime item".to_string())?;
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
        title: resolve_initial_static_object_text_field(resolved_expression, functions, "title")?
            .unwrap_or_default(),
        completed: initial_completed,
        text_fields: BTreeMap::new(),
        bool_fields: initial_extra_bools,
        scalar_fields: BTreeMap::new(),
        object_lists: BTreeMap::new(),
        nested_item_actions: BTreeMap::new(),
    };
    for variable in &object.variables {
        let name = variable.node.name.as_str();
        if name.is_empty() || name == "completed" {
            continue;
        }
        let value = find_object_field(object, name)
            .or_else(|| resolve_static_object_field_expression(expression, functions, name))
            .unwrap_or(&variable.node.value);
        if name == "title" {
            let initial_title =
                resolve_initial_static_object_text_field(resolved_expression, functions, name)?
                    .or_else(|| {
                        resolve_initial_static_object_text_field(value, functions, name)
                            .ok()
                            .flatten()
                    })
                    .or_else(|| static_text_item(value).ok());
            if let Some(value) = initial_title {
                item.title = value;
                continue;
            }
        }
        if let Some((nested_items, nested_updates, nested_actions, nested_globals)) =
            detect_object_list_pipeline(
                value,
                &BTreeMap::new(),
                functions,
                &format!("__item__.{name}"),
                &ScalarPlan::default(),
                &TextPlan::default(),
            )?
        {
            if !nested_globals.is_empty() {
                return Ok(None);
            }
            item.object_lists.insert(name.to_string(), nested_items);
            if !nested_actions.is_empty() {
                item.nested_item_actions
                    .insert(name.to_string(), nested_actions);
            }
            for ((trigger_binding, event_name), update) in nested_updates {
                let Some(kind) = ui_event_kind_for_name(&event_name) else {
                    continue;
                };
                let action = match update {
                    ObjectListUpdate::AppendObject { item: appended, .. } => {
                        ObjectItemActionKind::UpdateNestedObjectLists {
                            updates: vec![NestedObjectListAction::AppendObject {
                                field: name.to_string(),
                                item: appended,
                            }],
                        }
                    }
                    _ => return Ok(None),
                };
                item_actions.push(ObjectItemActionSpec {
                    source_binding_suffix: trigger_binding,
                    kind,
                    action,
                });
            }
            continue;
        }
        if let Some(value) = extract_integer_literal_opt(value)? {
            item.scalar_fields.insert(name.to_string(), value);
            continue;
        }
        if let Some(value) = extract_bool_literal_opt(value)? {
            item.bool_fields.entry(name.to_string()).or_insert(value);
            continue;
        }
        let initial_text =
            resolve_initial_static_object_text_field(resolved_expression, functions, name)?
                .or_else(|| {
                    resolve_initial_static_object_text_field(value, functions, name)
                        .ok()
                        .flatten()
                })
                .or_else(|| static_text_item(value).ok());
        if let Some(value) = initial_text {
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

fn specialize_static_object_item_expression<'a>(
    expression: &'a StaticSpannedExpression,
    functions: &BTreeMap<String, FunctionSpec<'a>>,
) -> Result<Option<&'static StaticSpannedExpression>, String> {
    let StaticExpression::FunctionCall { path, arguments } = &expression.node else {
        return Ok(None);
    };
    let Some(function_name) = resolved_function_name_in(path, functions) else {
        return Ok(None);
    };
    let Some(function) = functions.get(function_name) else {
        return Ok(None);
    };
    if function.parameters.len() != arguments.len() {
        return Ok(None);
    }
    let bindings = function_argument_bindings(function, arguments)?;
    Ok(Some(specialize_static_expression(function.body, &bindings)))
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
    if !matches!(event_name.as_str(), "change" | "input") || payload_field != "text" {
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
            .map(extract_initial_static_text_value)
            .transpose()
            .map(|value| value.flatten());
    }
    if let StaticExpression::FunctionCall { path, arguments } = &expression.node {
        if let Some(function_name) = resolved_function_name_in(path, functions) {
            if let Some(function) = functions.get(function_name) {
                if let Some(object) = resolve_object(function.body) {
                    if let Some(field_expression) = find_object_field(object, field_name) {
                        return initial_static_text_with_arguments(field_expression, arguments);
                    }
                }
            }
        }
    }
    extract_initial_static_text_value(expression)
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
                .map(|argument| initial_static_text_with_arguments(argument, arguments))
                .transpose()
                .map(|value| value.flatten())
        }
        StaticExpression::Pipe { from, to } if matches!(to.node, StaticExpression::Hold { .. }) => {
            initial_static_text_with_arguments(from, arguments)
        }
        StaticExpression::Latest { inputs } => {
            for input in inputs {
                if let Some(value) = initial_static_text_with_arguments(input, arguments)? {
                    return Ok(Some(value));
                }
            }
            Ok(None)
        }
        _ => extract_initial_static_text_value(expression),
    }
}

fn extract_initial_static_text_value(
    expression: &StaticSpannedExpression,
) -> Result<Option<String>, String> {
    match &expression.node {
        StaticExpression::Pipe { from, to } if matches!(to.node, StaticExpression::Hold { .. }) => {
            extract_initial_static_text_value(from)
        }
        StaticExpression::Latest { inputs } => {
            for input in inputs {
                if let Some(value) = extract_initial_static_text_value(input)? {
                    return Ok(Some(value));
                }
            }
            Ok(None)
        }
        _ => extract_static_text_value(expression),
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
            let source_path = canonical_reference_path(expression, path_bindings, binding_path)?
                .ok_or_else(|| {
                    "runtime object list append subset requires `source |> item_factory()`"
                        .to_string()
                })?;
            let source_expression = path_bindings
                .get(&source_path)
                .copied()
                .ok_or_else(|| format!("unknown append source binding `{source_path}`"))?;
            let StaticExpression::Pipe { from, to } = &source_expression.node else {
                return Err(
                    "runtime object list append subset requires `source |> item_factory()`"
                        .to_string(),
                );
            };
            (from.as_ref(), to.as_ref())
        }
    };
    if let Some(update) = detect_static_object_append_update(
        source_expression,
        target_expression,
        path_bindings,
        binding_path,
        functions,
    )? {
        return Ok(update);
    }
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

fn detect_object_list_append_on_update(
    expression: &StaticSpannedExpression,
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    binding_path: &str,
) -> Result<((String, String), ObjectListUpdate), String> {
    let StaticExpression::Pipe { from, to } = &expression.node else {
        return Err(
            "runtime object list append subset requires `on: event |> WHEN { payload => object }`"
                .to_string(),
        );
    };
    let Some((trigger_binding, event_name)) =
        canonical_event_source_path(from, path_bindings, binding_path)?
    else {
        return Err("runtime object list append subset requires a named event source".to_string());
    };
    let scalar_payload_fields = payload_object_scalar_fields_from_when(to)?;
    Ok((
        (trigger_binding, event_name),
        ObjectListUpdate::AppendPayloadObject {
            binding: binding_path.to_string(),
            scalar_payload_fields,
        },
    ))
}

fn payload_object_scalar_fields_from_when(
    expression: &StaticSpannedExpression,
) -> Result<BTreeMap<String, String>, String> {
    let StaticExpression::When { arms } = &expression.node else {
        return Err(
            "runtime object list append subset requires `WHEN { payload => object }`".to_string(),
        );
    };
    let mut payload_fields = None;
    for arm in arms {
        match &arm.pattern {
            static_expression::Pattern::Alias { name } => {
                payload_fields = Some(payload_object_scalar_fields(&arm.body, name.as_str())?);
            }
            static_expression::Pattern::WildCard
                if matches!(arm.body.node, StaticExpression::Skip) => {}
            _ => {}
        }
    }
    payload_fields
        .ok_or_else(|| "runtime object list append subset requires a payload alias arm".to_string())
}

fn payload_object_scalar_fields(
    expression: &StaticSpannedExpression,
    alias_name: &str,
) -> Result<BTreeMap<String, String>, String> {
    let StaticExpression::Object(object) = &expression.node else {
        return Err(
            "runtime object list append subset requires an object payload body".to_string(),
        );
    };
    let mut payload_fields = BTreeMap::new();
    for variable in &object.variables {
        let name = variable.node.name.as_str();
        if name.is_empty() {
            continue;
        }
        let Some(payload_field) = payload_scalar_field_reference(&variable.node.value, alias_name)
        else {
            return Err(
                "runtime object list append subset only supports scalar fields from payload access"
                    .to_string(),
            );
        };
        payload_fields.insert(name.to_string(), payload_field);
    }
    if payload_fields.is_empty() {
        return Err(
            "runtime object list append subset requires at least one payload field".to_string(),
        );
    }
    Ok(payload_fields)
}

fn payload_scalar_field_reference(
    expression: &StaticSpannedExpression,
    alias_name: &str,
) -> Option<String> {
    let StaticExpression::Alias(static_expression::Alias::WithoutPassed { parts, .. }) =
        &expression.node
    else {
        return None;
    };
    if parts.len() == 2 && parts[0].as_str() == alias_name {
        return Some(parts[1].as_str().to_string());
    }
    None
}

fn detect_static_object_append_update<'a>(
    source_expression: &'a StaticSpannedExpression,
    target_expression: &'a StaticSpannedExpression,
    path_bindings: &BTreeMap<String, &'a StaticSpannedExpression>,
    binding_path: &str,
    functions: &BTreeMap<String, FunctionSpec<'a>>,
) -> Result<Option<((String, String), ObjectListUpdate)>, String> {
    let StaticExpression::Then { body } = &target_expression.node else {
        return Ok(None);
    };
    if let Some(update) =
        detect_bound_object_append_update(body, path_bindings, functions, binding_path)?
    {
        let Some((trigger_binding, event_name)) =
            canonical_event_source_path(source_expression, path_bindings, binding_path)?
        else {
            return Ok(None);
        };
        return Ok(Some(((trigger_binding, event_name), update)));
    }
    if !object_runtime_item_supported(body, functions) {
        return Ok(None);
    }
    let Some((trigger_binding, event_name)) =
        canonical_event_source_path(source_expression, path_bindings, binding_path)?
    else {
        return Ok(None);
    };
    Ok(Some((
        (trigger_binding, event_name),
        ObjectListUpdate::AppendObject {
            binding: binding_path.to_string(),
            item: build_static_object_list_item(body, functions, 0)?,
        },
    )))
}

fn object_append_target_supported<'a>(
    expression: &'a StaticSpannedExpression,
    functions: &BTreeMap<String, FunctionSpec<'a>>,
) -> bool {
    let StaticExpression::FunctionCall { path, .. } = &expression.node else {
        return false;
    };
    resolved_function_name_in(path, functions).is_some()
        && resolve_static_object_field_expression(expression, functions, "title").is_some()
        && resolve_static_object_field_expression(expression, functions, "completed").is_some()
}

fn object_runtime_item_supported<'a>(
    expression: &'a StaticSpannedExpression,
    functions: &BTreeMap<String, FunctionSpec<'a>>,
) -> bool {
    resolve_static_object_runtime_expression(expression, functions)
        .ok()
        .flatten()
        .is_some()
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

fn detect_dynamic_object_retain_actions(
    expression: &StaticSpannedExpression,
    item_name: &str,
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    binding_path: &str,
) -> Result<Option<(Vec<ObjectItemActionSpec>, ObjectListEventUpdates)>, String> {
    let StaticExpression::Latest { inputs } = &expression.node else {
        return Ok(None);
    };
    let mut saw_initial_keep = false;
    let mut item_actions = Vec::new();
    let mut updates = Vec::new();
    for input in inputs {
        if extract_bool_literal_opt(input)? == Some(true) {
            saw_initial_keep = true;
            continue;
        }
        let StaticExpression::Pipe {
            from: trigger_source,
            to: trigger_then,
        } = &input.node
        else {
            return Ok(None);
        };
        let StaticExpression::Then { body } = &trigger_then.node else {
            return Ok(None);
        };
        if let Some(action) =
            detect_dynamic_object_retain_remove_self_action(trigger_source, body, item_name)?
        {
            item_actions.push(action);
            continue;
        }
        if let Some(update) = detect_dynamic_object_retain_bulk_remove_update(
            trigger_source,
            body,
            item_name,
            path_bindings,
            binding_path,
        )? {
            updates.push(update);
            continue;
        }
        return Ok(None);
    }
    if !saw_initial_keep || (item_actions.is_empty() && updates.is_empty()) {
        return Ok(None);
    }
    Ok(Some((item_actions, updates)))
}

fn detect_dynamic_object_retain_remove_self_action(
    trigger_source: &StaticSpannedExpression,
    body: &StaticSpannedExpression,
    item_name: &str,
) -> Result<Option<ObjectItemActionSpec>, String> {
    if extract_bool_literal_opt(body)? != Some(false) {
        return Ok(None);
    }
    let Some(remove) = detect_static_object_remove_spec(trigger_source, item_name)? else {
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

fn detect_dynamic_object_retain_bulk_remove_update(
    trigger_source: &StaticSpannedExpression,
    body: &StaticSpannedExpression,
    item_name: &str,
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    binding_path: &str,
) -> Result<Option<((String, String), ObjectListUpdate)>, String> {
    let Some(filter) =
        retain_keep_false_filter(body, item_name, path_bindings, binding_path)?
    else {
        return Ok(None);
    };
    let Some((trigger_binding, event_name)) =
        canonical_event_source_path(trigger_source, path_bindings, binding_path)?
    else {
        return Ok(None);
    };
    Ok(Some((
        (trigger_binding, event_name),
        ObjectListUpdate::RemoveMatching {
            binding: binding_path.to_string(),
            filter,
        },
    )))
}

fn retain_keep_false_filter(
    expression: &StaticSpannedExpression,
    item_name: &str,
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    binding_path: &str,
) -> Result<Option<ObjectListFilter>, String> {
    let StaticExpression::Pipe { from, to } = &expression.node else {
        return Ok(None);
    };
    let StaticExpression::FunctionCall { path, arguments } = &to.node else {
        return Ok(None);
    };
    if !path_matches(path, &["Bool", "not"]) || !arguments.is_empty() {
        return Ok(None);
    }
    if let StaticExpression::Alias(static_expression::Alias::WithoutPassed { parts, .. }) =
        &from.node
    {
        if parts.len() == 2 && parts[0].as_str() == item_name && parts[1].as_str() == "completed"
        {
            return Ok(Some(ObjectListFilter::BoolFieldEquals {
                field: "completed".to_string(),
                value: true,
            }));
        }
    }
    item_id_scalar_filter(from, item_name, path_bindings, binding_path)
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
        (matches!(
            &arm.pattern,
            static_expression::Pattern::Literal(static_expression::Literal::Tag(tag))
                if tag.as_str() == "False"
        ) || matches!(&arm.pattern, static_expression::Pattern::WildCard))
            && matches!(&arm.body.node, StaticExpression::Skip)
    });
    if !has_true_remove || !has_false_skip {
        return Ok(None);
    }
    if let StaticExpression::Alias(static_expression::Alias::WithoutPassed { parts, .. }) =
        &condition_source.node
    {
        if parts.len() == 2 && parts[0].as_str() == item_name && parts[1].as_str() == "completed" {
            return Ok(Some((
                (trigger_binding, event_name),
                ObjectListUpdate::RemoveMatching {
                    binding: String::new(),
                    filter: ObjectListFilter::BoolFieldEquals {
                        field: "completed".to_string(),
                        value: true,
                    },
                },
            )));
        }
    }
    let Some(filter) =
        item_id_scalar_filter(condition_source, item_name, path_bindings, binding_path)?
    else {
        return Ok(None);
    };
    Ok(Some((
        (trigger_binding, event_name),
        ObjectListUpdate::RemoveMatching {
            binding: String::new(),
            filter,
        },
    )))
}

fn item_id_scalar_filter(
    expression: &StaticSpannedExpression,
    item_name: &str,
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    binding_path: &str,
) -> Result<Option<ObjectListFilter>, String> {
    let StaticExpression::Comparator(static_expression::Comparator::Equal {
        operand_a,
        operand_b,
    }) = &expression.node
    else {
        return Ok(None);
    };
    let item_id_operand = |operand: &StaticSpannedExpression| {
        matches!(
            &operand.node,
            StaticExpression::Alias(static_expression::Alias::WithoutPassed { parts, .. })
                if parts.len() == 2
                    && parts[0].as_str() == item_name
                    && parts[1].as_str() == "id"
        )
    };
    let binding = if item_id_operand(operand_a) {
        canonical_reference_path(operand_b, path_bindings, binding_path)?
    } else if item_id_operand(operand_b) {
        canonical_reference_path(operand_a, path_bindings, binding_path)?
    } else {
        None
    };
    Ok(binding.map(|binding| ObjectListFilter::ItemIdEqualsScalarBinding { binding }))
}

fn detect_object_list_remove_last_update(
    expression: &StaticSpannedExpression,
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    binding_path: &str,
) -> Result<((String, String), ObjectListUpdate), String> {
    let Some((trigger_binding, event_name)) =
        canonical_event_source_path(expression, path_bindings, binding_path)?
    else {
        return Err("List/remove_last requires a named event source".to_string());
    };
    Ok((
        (trigger_binding, event_name),
        ObjectListUpdate::RemoveLast {
            binding: binding_path.to_string(),
        },
    ))
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
    if let Some(path) = canonical_reference_path(expression, path_bindings, binding_path)? {
        if let Some(source_expression) = path_bindings.get(&path).copied() {
            return detect_text_list_pipeline(source_expression, path_bindings, &path);
        }
    }
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
                let Some(item) = find_named_argument(arguments, "item") else {
                    return Ok(None);
                };
                if is_clearly_non_text_list_append_item(item) {
                    return Ok(None);
                }
                let ((trigger_binding, event_name), update) =
                    detect_text_list_append_update(item, path_bindings, binding_path)?;
                updates.push(((trigger_binding, event_name), update));
                return Ok(Some((initial_items, updates)));
            }
            if path_matches(path, &["List", "remove_last"]) {
                return Ok(None);
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

fn is_clearly_non_text_list_append_item(expression: &StaticSpannedExpression) -> bool {
    match &expression.node {
        StaticExpression::Object(_) | StaticExpression::List { .. } => true,
        StaticExpression::FunctionCall { path, .. } => {
            !matches!(path.first().map(|part| part.as_str()), Some("Text"))
        }
        StaticExpression::Pipe { to, .. } => match &to.node {
            StaticExpression::FunctionCall { path, .. } => {
                !matches!(path.first().map(|part| part.as_str()), Some("Text"))
            }
            StaticExpression::Object(_) | StaticExpression::List { .. } => true,
            _ => false,
        },
        _ => false,
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

fn is_empty_static_list(expression: &StaticSpannedExpression) -> bool {
    matches!(&expression.node, StaticExpression::List { items } if items.is_empty())
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
    if parts.last().map(boon::parser::StrSlice::as_str) != Some("text") {
        return Ok(None);
    }
    if parts.len() >= 4 && parts[parts.len() - 3].as_str() == "event" {
        let binding = canonical_parts_path(&parts[..parts.len() - 3], path_bindings, binding_path);
        if path_bindings.contains_key(&binding) {
            return Ok(Some(binding));
        }
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

fn detect_tag_toggle_scalar_specs(
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
) -> Result<BTreeMap<String, TagToggleScalarSpec>, String> {
    let mut specs = BTreeMap::new();
    for (binding_name, expression) in path_bindings {
        if let Some(spec) =
            tag_toggle_scalar_spec_for_expression(expression, path_bindings, binding_name)?
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
    let mut dynamic_values = BTreeMap::new();
    for input in inputs {
        if let Some(value) = extract_scalar_literal_value_with_dynamic_tags(input, &mut dynamic_values)?
        {
            static_emissions.push(value);
            continue;
        }
        let Some(event_value) =
            latest_event_value(input, path_bindings, binding_path, &mut dynamic_values)?
        else {
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
    dynamic_values: &mut BTreeMap<String, i64>,
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
    let Some(value) = extract_scalar_literal_value_with_dynamic_tags(body, dynamic_values)? else {
        return Ok(None);
    };
    Ok(Some(EventValueSpec {
        trigger_binding,
        event_name,
        value,
    }))
}

fn tag_toggle_scalar_spec_for_expression(
    expression: &StaticSpannedExpression,
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    binding_path: &str,
) -> Result<Option<TagToggleScalarSpec>, String> {
    let StaticExpression::Pipe { from, to } = &expression.node else {
        return Ok(None);
    };
    let StaticExpression::Hold { state_param, body } = &to.node else {
        return Ok(None);
    };
    let Some(initial_tag) = extract_tag_like_name(from) else {
        return Ok(None);
    };
    let inputs = match &body.node {
        StaticExpression::Pipe { .. } => vec![body.as_ref()],
        StaticExpression::Latest { inputs } => inputs.iter().collect::<Vec<_>>(),
        _ => return Ok(None),
    };

    let mut events = Vec::new();
    let mut shared_mapping = None::<BTreeMap<String, i64>>;
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
        let Some(mapping) = hold_tag_toggle_scalar_values_from_body(body, state_param.as_str())?
        else {
            return Ok(None);
        };
        if let Some(existing) = &shared_mapping {
            if existing != &mapping {
                return Ok(None);
            }
        } else {
            shared_mapping = Some(mapping);
        }
        events.push((trigger_binding, event_name));
    }

    let Some(mapping) = shared_mapping else {
        return Ok(None);
    };
    let Some(initial_value) = mapping.get(initial_tag.as_str()).copied() else {
        return Ok(None);
    };
    Ok(Some(TagToggleScalarSpec {
        initial_value,
        events,
    }))
}

fn hold_tag_toggle_scalar_values_for_expression(
    expression: &StaticSpannedExpression,
) -> Result<Option<BTreeMap<String, i64>>, String> {
    let StaticExpression::Pipe { to, .. } = &expression.node else {
        return Ok(None);
    };
    let StaticExpression::Hold { state_param, body } = &to.node else {
        return Ok(None);
    };
    hold_tag_toggle_scalar_values_from_hold_body(body, state_param.as_str())
}

fn hold_tag_toggle_scalar_values_from_hold_body(
    expression: &StaticSpannedExpression,
    state_param: &str,
) -> Result<Option<BTreeMap<String, i64>>, String> {
    let inputs = match &expression.node {
        StaticExpression::Pipe { .. } => vec![expression],
        StaticExpression::Latest { inputs } => inputs.iter().collect::<Vec<_>>(),
        _ => return Ok(None),
    };

    let mut shared_mapping = None::<BTreeMap<String, i64>>;
    for input in inputs {
        let mapping = match &input.node {
            StaticExpression::Pipe { to, .. } => match &to.node {
                StaticExpression::Then { body } => {
                    hold_tag_toggle_scalar_values_from_body(body, state_param)?
                }
                _ => hold_tag_toggle_scalar_values_from_body(input, state_param)?,
            },
            _ => hold_tag_toggle_scalar_values_from_body(input, state_param)?,
        };
        let Some(mapping) = mapping else {
            return Ok(None);
        };
        if let Some(existing) = &shared_mapping {
            if existing != &mapping {
                return Ok(None);
            }
        } else {
            shared_mapping = Some(mapping);
        }
    }

    Ok(shared_mapping)
}

fn hold_tag_toggle_scalar_values_from_body(
    expression: &StaticSpannedExpression,
    state_param: &str,
) -> Result<Option<BTreeMap<String, i64>>, String> {
    let StaticExpression::Pipe { from, to } = &expression.node else {
        return Ok(None);
    };
    if !is_state_alias_reference(from, state_param) {
        return Ok(None);
    }
    let StaticExpression::When { arms } = &to.node else {
        return Ok(None);
    };
    let mut transitions = BTreeMap::new();
    for arm in arms {
        let from_name = match &arm.pattern {
            static_expression::Pattern::Literal(static_expression::Literal::Tag(tag))
            | static_expression::Pattern::Literal(static_expression::Literal::Text(tag)) => {
                tag.as_str().to_string()
            }
            _ => return Ok(None),
        };
        let Some(to_name) = extract_tag_like_name(&arm.body) else {
            return Ok(None);
        };
        transitions.insert(from_name, to_name);
    }
    if transitions.len() != 2 {
        return Ok(None);
    }
    let names = transitions.keys().cloned().collect::<Vec<_>>();
    let first = &names[0];
    let second = &names[1];
    if transitions.get(first) != Some(second) || transitions.get(second) != Some(first) {
        return Ok(None);
    }
    Ok(Some(BTreeMap::from([
        (first.clone(), 0),
        (second.clone(), 1),
    ])))
}

fn extract_tag_like_name(expression: &StaticSpannedExpression) -> Option<String> {
    match &expression.node {
        StaticExpression::Literal(static_expression::Literal::Tag(tag))
        | StaticExpression::Literal(static_expression::Literal::Text(tag)) => {
            Some(tag.as_str().to_string())
        }
        _ => None,
    }
}

fn extract_scalar_literal_value_with_dynamic_tags(
    expression: &StaticSpannedExpression,
    dynamic_values: &mut BTreeMap<String, i64>,
) -> Result<Option<i64>, String> {
    if let Some(value) = extract_scalar_literal_value_opt(expression)? {
        return Ok(Some(value));
    }
    let Some(name) = extract_tag_like_name(expression) else {
        return Ok(None);
    };
    if let Some(value) = dynamic_values.get(&name).copied() {
        return Ok(Some(value));
    }
    let value = 1024 + dynamic_values.len() as i64;
    dynamic_values.insert(name, value);
    Ok(Some(value))
}

fn extract_scalar_literal_value_opt(
    expression: &StaticSpannedExpression,
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
    Ok(None)
}

fn latest_tag_scalar_values_for_expression(
    expression: &StaticSpannedExpression,
) -> Result<Option<BTreeMap<String, i64>>, String> {
    let StaticExpression::Latest { inputs } = &expression.node else {
        return Ok(None);
    };
    if inputs.is_empty() {
        return Ok(None);
    }

    let mut values = BTreeMap::new();
    let mut saw_tag = false;
    for input in inputs {
        let candidate = match &input.node {
            StaticExpression::Pipe { to, .. } => match &to.node {
                StaticExpression::Then { body } => body.as_ref(),
                _ => input,
            },
            _ => input,
        };
        if let Some(name) = extract_tag_like_name(candidate) {
            let Some(value) = extract_scalar_literal_value_opt(candidate)? else {
                return Ok(None);
            };
            values.entry(name).or_insert(value);
            saw_tag = true;
            continue;
        }
        if extract_scalar_literal_value_opt(candidate)?.is_some() {
            continue;
        }
        return Ok(None);
    }

    if saw_tag { Ok(Some(values)) } else { Ok(None) }
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

fn hold_payload_scalar_spec_for_expression(
    expression: &StaticSpannedExpression,
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    binding_path: &str,
) -> Result<Option<HoldPayloadScalarSpec>, String> {
    let StaticExpression::Pipe { from, to } = &expression.node else {
        return Ok(None);
    };
    let StaticExpression::Hold { body, .. } = &to.node else {
        return Ok(None);
    };
    let Some(initial_value) = extract_integer_literal_opt(from)? else {
        return Ok(None);
    };
    let Some((trigger_binding, event_name, payload_field)) =
        canonical_event_payload_source_path(body, path_bindings, binding_path)?
    else {
        return Ok(None);
    };
    if payload_field != "value" {
        return Ok(None);
    }
    Ok(Some(HoldPayloadScalarSpec {
        initial_value,
        trigger_binding,
        event_name,
    }))
}

fn hold_alias_scalar_spec_for_expression(
    expression: &StaticSpannedExpression,
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    binding_path: &str,
) -> Result<Option<(i64, String)>, String> {
    let StaticExpression::Pipe { from, to } = &expression.node else {
        return Ok(None);
    };
    let StaticExpression::Hold { body, .. } = &to.node else {
        return Ok(None);
    };
    let Some(initial_value) =
        extract_integer_literal_opt(from)?.or(extract_bool_literal_opt(from)?.map(i64::from))
    else {
        return Ok(None);
    };
    let Some(source_binding) = canonical_reference_path(body, path_bindings, binding_path)? else {
        return Ok(None);
    };
    Ok((source_binding != binding_path).then_some((initial_value, source_binding)))
}

fn hold_alias_text_spec_for_expression(
    expression: &StaticSpannedExpression,
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    binding_path: &str,
) -> Result<Option<(String, String)>, String> {
    let StaticExpression::Pipe { from, to } = &expression.node else {
        return Ok(None);
    };
    let StaticExpression::Hold { body, .. } = &to.node else {
        return Ok(None);
    };
    let Some(initial_value) = extract_static_text_value(from)? else {
        return Ok(None);
    };
    let Some(source_binding) = canonical_reference_path(body, path_bindings, binding_path)? else {
        return Ok(None);
    };
    Ok((source_binding != binding_path).then_some((initial_value, source_binding)))
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
            other => Ok(Some(generic_tag_scalar_value(other))),
        },
        _ => Ok(None),
    }
}

fn generic_tag_scalar_value(tag: &str) -> i64 {
    let mut hash = 1_469_598_103_934_665_603_u64;
    for byte in tag.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(1_099_511_628_211);
    }
    1_000_000 + (hash & 0x3fff_ffff) as i64
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

fn route_text_spec_for_expression(
    expression: &StaticSpannedExpression,
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    binding_path: &str,
) -> Result<Option<(String, Vec<((String, String), TextUpdate)>)>, String> {
    if !is_router_route_expression(expression) {
        return Ok(None);
    }

    let mut updates = Vec::new();
    for (candidate_binding, candidate_expression) in path_bindings {
        if let Some(candidate_updates) = route_navigation_text_updates(
            candidate_expression,
            path_bindings,
            candidate_binding,
            binding_path,
        )? {
            updates.extend(candidate_updates);
        }
    }

    Ok(Some(("/".to_string(), updates)))
}

fn route_navigation_text_updates(
    expression: &StaticSpannedExpression,
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    binding_path: &str,
    target_binding: &str,
) -> Result<Option<Vec<((String, String), TextUpdate)>>, String> {
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

    let mut updates = Vec::new();
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
        updates.push((
            (trigger_binding, event_name),
            TextUpdate::SetStatic {
                binding: target_binding.to_string(),
                value: route,
                payload_filter: None,
            },
        ));
    }

    Ok(Some(updates))
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
        if matches!(payload_field.as_str(), "text" | "value") {
            let key_down_text = event_name == "key_down";
            return Ok(Some((
                (trigger_binding, event_name),
                if key_down_text {
                    TextUpdate::SetFromInput {
                        binding: binding_path.to_string(),
                        source_binding: binding_path.to_string(),
                        payload_filter: None,
                    }
                } else {
                    TextUpdate::SetFromPayload {
                        binding: binding_path.to_string(),
                    }
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
        if let Some(update) =
            text_update_from_body(trigger_then, path_bindings, binding_path, None)?
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
            let Some(update) = text_update_from_body(body, path_bindings, binding_path, None)?
            else {
                return Ok(None);
            };
            Ok(Some(((trigger_binding, event_name), update)))
        }
        StaticExpression::When { arms } => {
            for arm in arms {
                match &arm.pattern {
                    static_expression::Pattern::Alias { name } => {
                        if matches!(payload_field.as_str(), "text" | "value")
                            && matches!(
                                &arm.body.node,
                                StaticExpression::Alias(static_expression::Alias::WithoutPassed {
                                    parts,
                                    ..
                                }) if parts.len() == 1 && parts[0].as_str() == name.as_str()
                            )
                        {
                            let key_down_text = event_name == "key_down";
                            return Ok(Some((
                                (trigger_binding, event_name),
                                if key_down_text {
                                    TextUpdate::SetFromInput {
                                        binding: binding_path.to_string(),
                                        source_binding: binding_path.to_string(),
                                        payload_filter: None,
                                    }
                                } else {
                                    TextUpdate::SetFromPayload {
                                        binding: binding_path.to_string(),
                                    }
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
                        )?
                        else {
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
    if let StaticExpression::Then { body } = &expression.node {
        return text_update_from_body(body, path_bindings, binding_path, payload_filter);
    }
    if let Some((_trigger_binding, event_name, payload_field)) =
        canonical_event_payload_source_path(expression, path_bindings, binding_path)?
    {
        if matches!(payload_field.as_str(), "text" | "value") {
            return Ok(Some(if event_name == "key_down" {
                TextUpdate::SetFromInput {
                    binding: binding_path.to_string(),
                    source_binding: binding_path.to_string(),
                    payload_filter,
                }
            } else {
                TextUpdate::SetFromPayload {
                    binding: binding_path.to_string(),
                }
            }));
        }
    }
    if let Some(parts) =
        top_level_text_parts_for_expression(expression, path_bindings, binding_path)?
    {
        return Ok(Some(TextUpdate::SetComputed {
            binding: binding_path.to_string(),
            parts,
            payload_filter,
        }));
    }
    if let Some((condition_binding, truthy_parts, falsy_parts)) =
        top_level_bool_text_branch_update(expression, path_bindings, binding_path)?
    {
        return Ok(Some(TextUpdate::SetComputedBranch {
            binding: binding_path.to_string(),
            condition_binding,
            truthy_parts,
            falsy_parts,
            payload_filter,
        }));
    }
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

fn top_level_text_parts_for_expression(
    expression: &StaticSpannedExpression,
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    binding_path: &str,
) -> Result<Option<Vec<SemanticTextPart>>, String> {
    let StaticExpression::TextLiteral { parts, .. } = &expression.node else {
        return Ok(None);
    };
    let mut output = Vec::with_capacity(parts.len());
    for part in parts {
        match part {
            StaticTextPart::Text(text) => {
                output.push(SemanticTextPart::Static(text.as_str().to_string()))
            }
            StaticTextPart::Interpolation { var, .. } => {
                let binding =
                    top_level_interpolation_binding(var.as_str(), path_bindings, binding_path);
                let Some((binding, kind)) = binding else {
                    return Ok(None);
                };
                output.push(match kind {
                    TopLevelInterpolationKind::Text => SemanticTextPart::TextBinding(binding),
                    TopLevelInterpolationKind::Scalar => SemanticTextPart::ScalarBinding(binding),
                });
            }
        }
    }
    Ok(Some(output))
}

fn top_level_bool_text_branch_update(
    expression: &StaticSpannedExpression,
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    binding_path: &str,
) -> Result<Option<(String, Vec<SemanticTextPart>, Vec<SemanticTextPart>)>, String> {
    let StaticExpression::Pipe { from, to } = &expression.node else {
        return Ok(None);
    };
    let (StaticExpression::When { arms } | StaticExpression::While { arms }) = &to.node else {
        return Ok(None);
    };
    let Some(condition_binding) = canonical_reference_path(from, path_bindings, binding_path)?
    else {
        return Ok(None);
    };
    let mut truthy_parts = None;
    let mut falsy_parts = None;
    for arm in arms {
        match &arm.pattern {
            static_expression::Pattern::Literal(static_expression::Literal::Tag(tag))
                if tag.as_str() == "True" =>
            {
                truthy_parts =
                    top_level_text_parts_for_expression(&arm.body, path_bindings, binding_path)?;
            }
            static_expression::Pattern::Literal(static_expression::Literal::Tag(tag))
                if tag.as_str() == "False" =>
            {
                falsy_parts =
                    top_level_text_parts_for_expression(&arm.body, path_bindings, binding_path)?;
            }
            static_expression::Pattern::WildCard => {
                falsy_parts =
                    top_level_text_parts_for_expression(&arm.body, path_bindings, binding_path)?;
            }
            _ => {}
        }
    }
    Ok(truthy_parts
        .zip(falsy_parts)
        .map(|(truthy_parts, falsy_parts)| (condition_binding, truthy_parts, falsy_parts)))
}

#[derive(Clone, Copy)]
enum TopLevelInterpolationKind {
    Text,
    Scalar,
}

fn top_level_interpolation_binding(
    name: &str,
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    binding_path: &str,
) -> Option<(String, TopLevelInterpolationKind)> {
    let path = canonical_string_reference_path(name, path_bindings, binding_path)?;
    let expression = path_bindings.get(&path).copied()?;
    if hold_text_spec_for_expression(expression, path_bindings, &path)
        .ok()
        .flatten()
        .is_some()
        || latest_text_spec_for_expression(expression, path_bindings, &path)
            .ok()
            .flatten()
            .is_some()
    {
        return Some((path, TopLevelInterpolationKind::Text));
    }
    if latest_value_spec_for_expression(expression, path_bindings, &path)
        .ok()
        .flatten()
        .is_some()
        || derived_text_value_branch_spec(expression, path_bindings, &path)
            .ok()
            .flatten()
            .is_some()
        || selected_filter_spec_for_expression(expression, path_bindings, &path)
            .ok()
            .flatten()
            .is_some()
        || detect_top_level_bool_spec(expression, path_bindings, &path)
            .ok()
            .flatten()
            .is_some()
        || detect_timer_bindings(path_bindings)
            .ok()
            .and_then(|timer_bindings| {
                counter_spec_for_expression(expression, path_bindings, &path, &timer_bindings)
                    .ok()
                    .flatten()
            })
            .is_some()
    {
        return Some((path, TopLevelInterpolationKind::Scalar));
    }
    None
}

fn canonical_string_reference_path(
    name: &str,
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    binding_path: &str,
) -> Option<String> {
    if path_bindings.contains_key(name) {
        return Some(name.to_string());
    }
    binding_scope_base(binding_path)
        .map(|base| format!("{base}.{name}"))
        .filter(|candidate| path_bindings.contains_key(candidate))
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
    timer_bindings: &BTreeMap<String, u32>,
) -> Result<Option<CounterSpec>, String> {
    if let Some(spec) =
        counter_spec_for_hold_expression(expression, path_bindings, binding_path, timer_bindings)?
    {
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
        canonical_trigger_source_path(trigger_source, path_bindings, binding_path)?
    else {
        return Ok(None);
    };
    if event_name != "press" {
        return Ok(None);
    }
    let StaticExpression::Then { body } = &trigger_then.node else {
        return Ok(None);
    };
    let Some(update) = extract_delta_update_opt(body)? else {
        return Ok(None);
    };

    Ok(Some(CounterSpec {
        initial,
        events: vec![EventDeltaSpec {
            trigger_binding,
            event_name,
            update,
        }],
    }))
}

fn event_only_counter_events_for_expression(
    expression: &StaticSpannedExpression,
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    binding_path: &str,
    timer_bindings: &BTreeMap<String, u32>,
) -> Result<Option<Vec<EventDeltaSpec>>, String> {
    if let Some(events) = skip_wrapped_hold_counter_events(
        expression,
        path_bindings,
        binding_path,
        timer_bindings,
    )? {
        return Ok(Some(events));
    }
    direct_timer_sum_counter_events(expression, path_bindings, binding_path)
}

fn skip_wrapped_hold_counter_events(
    expression: &StaticSpannedExpression,
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    binding_path: &str,
    timer_bindings: &BTreeMap<String, u32>,
) -> Result<Option<Vec<EventDeltaSpec>>, String> {
    let StaticExpression::Pipe { from, to } = &expression.node else {
        return Ok(None);
    };
    let StaticExpression::FunctionCall { path, arguments } = &to.node else {
        return Ok(None);
    };
    if !path_matches(path, &["Stream", "skip"]) {
        return Ok(None);
    }
    let skip_count = find_named_argument(arguments, "count")
        .map(extract_integer_literal)
        .transpose()?
        .unwrap_or_default();
    if skip_count < 1 {
        return Ok(None);
    }
    let Some(spec) =
        counter_spec_for_hold_expression(from, path_bindings, binding_path, timer_bindings)?
    else {
        return Ok(None);
    };
    Ok(Some(spec.events))
}

fn direct_timer_sum_counter_events(
    expression: &StaticSpannedExpression,
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    binding_path: &str,
) -> Result<Option<Vec<EventDeltaSpec>>, String> {
    let StaticExpression::Pipe { from, to } = &expression.node else {
        return Ok(None);
    };
    let StaticExpression::FunctionCall { path, arguments } = &to.node else {
        return Ok(None);
    };
    if !path_matches(path, &["Math", "sum"]) || !arguments.is_empty() {
        return Ok(None);
    }
    let StaticExpression::Pipe {
        from: trigger_source,
        to: trigger_then,
    } = &from.node
    else {
        return Ok(None);
    };
    let StaticExpression::Then { body } = &trigger_then.node else {
        return Ok(None);
    };
    let trigger = if let Some((binding, event_name)) =
        canonical_trigger_source_path(trigger_source, path_bindings, binding_path)?
    {
        Some((binding, event_name))
    } else if timer_interval_millis_for_expression(trigger_source)?.is_some()
        && path_bindings.contains_key(SYNTHETIC_DOCUMENT_ROOT_TIMER_BINDING)
    {
        Some((SYNTHETIC_DOCUMENT_ROOT_TIMER_BINDING.to_string(), "tick".to_string()))
    } else {
        None
    };
    let Some((trigger_binding, event_name)) = trigger else {
        return Ok(None);
    };
    let Some(update) = extract_delta_update_opt(body)? else {
        return Ok(None);
    };
    Ok(Some(vec![EventDeltaSpec {
        trigger_binding,
        event_name,
        update,
    }]))
}

fn counter_spec_for_hold_expression(
    expression: &StaticSpannedExpression,
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    binding_path: &str,
    timer_bindings: &BTreeMap<String, u32>,
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
        timer_bindings,
    )
}

fn counter_spec_for_hold_body(
    initial: i64,
    state_param: &str,
    body: &StaticSpannedExpression,
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    binding_path: &str,
    _timer_bindings: &BTreeMap<String, u32>,
) -> Result<Option<CounterSpec>, String> {
    let mut events = Vec::new();
    match &body.node {
        StaticExpression::Pipe {
            from: trigger_source,
            to: trigger_then,
        } => {
            let Some((trigger_binding, event_name)) =
                canonical_trigger_source_path(trigger_source, path_bindings, binding_path)?
            else {
                return Ok(None);
            };
            let StaticExpression::Then { body } = &trigger_then.node else {
                return Ok(None);
            };
            let Some(update) = extract_counter_event_update_opt(body, Some(state_param))? else {
                return Ok(None);
            };
            events.push(EventDeltaSpec {
                trigger_binding,
                event_name,
                update,
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
                    canonical_trigger_source_path(trigger_source, path_bindings, binding_path)?
                else {
                    return Ok(None);
                };
                let StaticExpression::Then { body } = &trigger_then.node else {
                    return Ok(None);
                };
                let Some(update) = extract_counter_event_update_opt(body, Some(state_param))?
                else {
                    return Ok(None);
                };
                events.push(EventDeltaSpec {
                    trigger_binding,
                    event_name,
                    update,
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

fn canonical_trigger_source_path(
    expression: &StaticSpannedExpression,
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    binding_path: &str,
) -> Result<Option<(String, String)>, String> {
    if let Some(event_source) =
        canonical_event_source_path(expression, path_bindings, binding_path)?
    {
        return Ok(Some(event_source));
    }
    let Some(timer_binding) = canonical_reference_path(expression, path_bindings, binding_path)?
    else {
        return Ok(None);
    };
    if timer_interval_millis_for_binding(path_bindings, &timer_binding)?.is_some() {
        return Ok(Some((timer_binding, "tick".to_string())));
    }
    Ok(None)
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
    parts: &[boon::parser::StrSlice],
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
    binding_path: &str,
) -> String {
    if let Some((first, rest)) = parts.split_first() {
        if first.as_str() == "element" {
            if let Some(scope_base) = binding_scope_base(binding_path) {
                if rest.is_empty() {
                    return scope_base.to_string();
                }
                return format!(
                    "{scope_base}.{}",
                    rest.iter()
                        .map(boon::parser::StrSlice::as_str)
                        .collect::<Vec<_>>()
                        .join(".")
                );
            }
        }
    }
    let joined = parts
        .iter()
        .map(boon::parser::StrSlice::as_str)
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

fn extract_counter_event_update(
    expression: &StaticSpannedExpression,
    state_param: Option<&str>,
) -> Result<CounterEventUpdate, String> {
    match &expression.node {
        StaticExpression::Literal(static_expression::Literal::Number(_)) => Ok(
            CounterEventUpdate::Set(extract_integer_literal(expression)?),
        ),
        StaticExpression::ArithmeticOperator(static_expression::ArithmeticOperator::Add {
            operand_a,
            operand_b,
        }) => {
            if operand_matches_state(operand_a, state_param) {
                extract_delta_update(operand_b, false)
            } else if operand_matches_state(operand_b, state_param) {
                extract_delta_update(operand_a, false)
            } else {
                Err("HOLD counter subset expects `state + <integer>`".to_string())
            }
        }
        StaticExpression::ArithmeticOperator(static_expression::ArithmeticOperator::Subtract {
            operand_a,
            operand_b,
        }) => {
            if operand_matches_state(operand_a, state_param) {
                extract_delta_update(operand_b, true)
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

fn extract_delta_update(
    expression: &StaticSpannedExpression,
    negate: bool,
) -> Result<CounterEventUpdate, String> {
    match &expression.node {
        StaticExpression::Literal(static_expression::Literal::Number(number)) => {
            if number.fract() == 0.0 {
                let delta = if negate {
                    -(*number as i64)
                } else {
                    *number as i64
                };
                return Ok(CounterEventUpdate::Add(delta));
            }
            let tenths = (number * 10.0).round();
            if (tenths / 10.0 - number).abs() < 1e-9 {
                let tenths_delta = if negate {
                    -(tenths as i64)
                } else {
                    tenths as i64
                };
                return Ok(CounterEventUpdate::AddTenths(tenths_delta));
            }
            Err("counter subset requires integer or tenths numeric literals".to_string())
        }
        _ => Err("counter subset requires numeric literals".to_string()),
    }
}

fn extract_delta_update_opt(
    expression: &StaticSpannedExpression,
) -> Result<Option<CounterEventUpdate>, String> {
    match extract_delta_update(expression, false) {
        Ok(update) => Ok(Some(update)),
        Err(error)
            if error.starts_with("counter subset requires numeric literals")
                || error == "counter subset requires integer or tenths numeric literals" =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

fn extract_counter_event_update_opt(
    expression: &StaticSpannedExpression,
    state_param: Option<&str>,
) -> Result<Option<CounterEventUpdate>, String> {
    match extract_counter_event_update(expression, state_param) {
        Ok(delta) => Ok(Some(delta)),
        Err(error)
            if error.starts_with("counter subset requires numeric literals")
                || error == "counter subset requires integer numeric literals"
                || error == "counter subset requires integer or tenths numeric literals"
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
        && context.scalar_plan.derived_scalars.is_empty()
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
            scalar_mirrors: context.scalar_mirrors.clone(),
            text_mirrors: context.text_mirrors.clone(),
            route_bindings: detect_route_bindings(&context.path_bindings),
            derived_scalars: context.scalar_plan.derived_scalars.clone(),
        })
    }
}

fn detect_route_bindings(
    path_bindings: &BTreeMap<String, &StaticSpannedExpression>,
) -> Vec<String> {
    path_bindings
        .iter()
        .filter_map(|(binding, expression)| {
            is_router_route_expression(expression).then_some(binding.clone())
        })
        .collect()
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
            if resolved_function_name_for_path_in_stack(path, context, stack).is_some() =>
        {
            let function_name = resolved_function_name_for_path_in_stack(path, context, stack)
                .expect("guard ensured function resolution");
            let function = &context.functions[function_name];
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
                    let keep = eval_static_bool(condition, context, stack, locals, passed).map_err(
                        |error| {
                            format!(
                                "static List/retain predicate `{}` failed: {error}",
                                describe_expression_detailed(condition)
                            )
                        },
                    );
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
            } else if path_matches(path, &["List", "append"]) {
                let items = resolve_static_list_items(from, context, stack, locals, passed)?;
                let Some(item) = find_named_argument(arguments, "item") else {
                    return Ok(items);
                };
                match resolve_alias(item, context, locals, passed, stack) {
                    Ok(resolved_item) => {
                        let mut appended = items;
                        match &resolved_item.node {
                            StaticExpression::Object(_)
                            | StaticExpression::TextLiteral { .. }
                            | StaticExpression::List { .. }
                            | StaticExpression::Literal(_) => appended.push(resolved_item),
                            _ => {}
                        }
                        Ok(appended)
                    }
                    Err(_) => Ok(items),
                }
            } else if path_matches(path, &["List", "remove"]) {
                resolve_static_list_items(from, context, stack, locals, passed)
            } else if path_matches(path, &["List", "remove_last"]) {
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

fn synthetic_alias_bound_block_expression(
    name: &boon::parser::StrSlice,
    value: &StaticSpannedExpression,
    variables: &[static_expression::Spanned<static_expression::Variable>],
    output: &StaticSpannedExpression,
    template: &StaticSpannedExpression,
) -> &'static StaticSpannedExpression {
    let mut block_variables = Vec::with_capacity(variables.len() + 1);
    block_variables.push(static_expression::Spanned {
        span: value.span,
        persistence: value.persistence,
        node: static_expression::Variable {
            name: name.clone(),
            is_referenced: true,
            value: value.clone(),
            value_changed: false,
        },
    });
    block_variables.extend(variables.iter().cloned());
    Box::leak(Box::new(static_expression::Spanned {
        span: template.span,
        persistence: template.persistence,
        node: StaticExpression::Block {
            variables: block_variables,
            output: Box::new(output.clone()),
        },
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

fn path_matches(path: &[boon::parser::StrSlice], expected: &[&str]) -> bool {
    path.len() == expected.len()
        && path
            .iter()
            .zip(expected)
            .all(|(segment, expected)| segment.as_str() == *expected)
}

fn path_matches_element(path: &[boon::parser::StrSlice], kind: &str) -> bool {
    path_matches(path, &["Element", kind]) || path_matches(path, &["Scene", "Element", kind])
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
        StaticExpression::Comparator(comparator) => {
            let (left, op, right) = match comparator {
                static_expression::Comparator::Equal {
                    operand_a,
                    operand_b,
                } => (operand_a.as_ref(), "==", operand_b.as_ref()),
                static_expression::Comparator::NotEqual {
                    operand_a,
                    operand_b,
                } => (operand_a.as_ref(), "!=", operand_b.as_ref()),
                static_expression::Comparator::Greater {
                    operand_a,
                    operand_b,
                } => (operand_a.as_ref(), ">", operand_b.as_ref()),
                static_expression::Comparator::GreaterOrEqual {
                    operand_a,
                    operand_b,
                } => (operand_a.as_ref(), ">=", operand_b.as_ref()),
                static_expression::Comparator::Less {
                    operand_a,
                    operand_b,
                } => (operand_a.as_ref(), "<", operand_b.as_ref()),
                static_expression::Comparator::LessOrEqual {
                    operand_a,
                    operand_b,
                } => (operand_a.as_ref(), "<=", operand_b.as_ref()),
            };
            format!(
                "comparator({} {} {})",
                describe_expression_detailed(left),
                op,
                describe_expression_detailed(right)
            )
        }
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

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::{
        LocalBinding, LowerContext, PassedScope, RuntimeObjectListRef, StaticExpression,
        StaticSpannedExpression, active_object_scope,
        augment_dependent_object_list_updates_from_item_bindings, canonical_expression_path,
        augment_document_link_forwarder_item_runtime, augment_hold_alias_runtime,
        augment_linked_text_input_runtime, augment_object_alias_field_runtime,
        augment_top_level_bool_item_runtime, augment_top_level_object_field_runtime,
        bool_ui_branch_arms, conditional_alias_arm, describe_expression_detailed,
        detect_item_event_source, detect_list_plan, detect_object_list_plan, detect_scalar_plan,
        detect_static_object_list_plan, detect_text_plan, detect_timer_bindings,
        eager_block_scope, find_document_expression, find_named_argument,
        find_object_field, find_positional_parameter_name, flatten_binding_paths,
        infer_argument_object_base,
        initial_scalar_value_in_context, invocation_marker, latest_value_spec_for_expression,
        linked_text_input_plan, lower_bool_condition_from_nodes, lower_dynamic_list_items,
        lower_scalar_value_when_node, lower_text_value, lower_to_semantic, lower_ui_node,
        object_derived_scalar_operand_in_context, parse_static_expressions,
        path_matches,
        passed_scope_for_expression, resolve_alias, resolve_scalar_reference,
        resolve_static_object_runtime_expression,
        select_when_arm_body,
        scalar_binding_has_runtime_state, scalar_branch_all_values_for_source,
        scalar_branch_pattern_value_for_source,
        seed_missing_scalar_initial_values,
        scalar_compare_branch_operands,
        synthetic_document_root_expression, synthetic_document_root_timer_expression,
        runtime_object_list_binding_path, runtime_object_list_filter, runtime_object_list_ref,
        runtime_text_binding_path, top_level_bindings, top_level_functions,
        trigger_specs_for_runtime_binding, try_lower_to_semantic, with_invoked_function_scope,
        SYNTHETIC_DOCUMENT_ROOT_BINDING, SYNTHETIC_DOCUMENT_ROOT_TIMER_BINDING,
    };
    use boon::parser::static_expression::Comparator as StaticComparator;
    use crate::semantic_ir::{
        DerivedScalarOperand, IntCompareOp, ItemScalarUpdate, ObjectDerivedScalarOperand,
        ObjectItemActionKind, ObjectListFilter, ObjectListUpdate, RuntimeModel, ScalarUpdate,
        SemanticAction, SemanticInputValue, SemanticNode, SemanticTextPart, TextListFilter,
        TextUpdate,
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
    fn try_lower_to_semantic_returns_error_for_invalid_document_pipe_form() {
        let error = try_lower_to_semantic("document: Document/new(root: root)", None, false)
            .expect_err("missing root binding should fail");
        assert!(
            error.contains("reference error") && error.contains("root"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn try_lower_to_semantic_supports_official_7guis_sources() {
        let examples = [
            (
                "counter",
                include_str!(
                    "../../../playground/frontend/src/examples/counter/counter.bn"
                ),
            ),
            (
                "temperature_converter",
                include_str!(
                    "../../../playground/frontend/src/examples/temperature_converter/temperature_converter.bn"
                ),
            ),
            (
                "flight_booker",
                include_str!(
                    "../../../playground/frontend/src/examples/flight_booker/flight_booker.bn"
                ),
            ),
            (
                "timer",
                include_str!("../../../playground/frontend/src/examples/timer/timer.bn"),
            ),
            (
                "crud",
                include_str!("../../../playground/frontend/src/examples/crud/crud.bn"),
            ),
            (
                "circle_drawer",
                include_str!(
                    "../../../playground/frontend/src/examples/circle_drawer/circle_drawer.bn"
                ),
            ),
            (
                "cells",
                include_str!("../../../playground/frontend/src/examples/cells/cells.bn"),
            ),
        ];

        for (name, source) in examples {
            if let Err(error) = try_lower_to_semantic(source, None, false) {
                panic!("7GUIs example `{name}` should lower for Wasm: {error}");
            }
        }
    }

    #[test]
    fn timer_progress_percent_has_initial_scalar_value() {
        let source =
            include_str!("../../../playground/frontend/src/examples/timer/timer.bn");
        let expressions = parse_static_expressions(source).expect("timer should parse");
        let bindings = top_level_bindings(&expressions);
        let functions = top_level_functions(&expressions, None);
        let path_bindings = flatten_binding_paths(&bindings);
        let mut scalar_plan = detect_scalar_plan(
            &path_bindings,
            &functions,
            &detect_timer_bindings(&path_bindings).expect("timer bindings should build"),
        )
        .expect("scalar plan should build");
        let static_object_lists =
            detect_static_object_list_plan(&path_bindings, &functions, &mut scalar_plan)
                .expect("static object list plan should build");
        let text_plan = detect_text_plan(&path_bindings).expect("text plan should build");
        let object_list_plan =
            detect_object_list_plan(&path_bindings, &functions, &scalar_plan, &text_plan)
                .expect("object list plan should build");
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
            .expect("top-level object field runtime should build");
        augment_linked_text_input_runtime(&mut context)
            .expect("linked text input runtime should build");
        augment_top_level_bool_item_runtime(&mut context)
            .expect("top-level bool item runtime should build");

        let mut stack = Vec::new();
        let mut locals = Vec::new();
        let mut passed = Vec::new();
        for binding in [
            "store.max_duration",
            "store.raw_elapsed",
            "store.elapsed",
            "store.elapsed_tenths",
            "store.elapsed_display",
            "store.progress_percent",
        ] {
            stack.clear();
            locals.clear();
            passed.clear();
            let expression = context
                .path_bindings
                .get(binding)
                .copied()
                .unwrap_or_else(|| panic!("{binding} binding should exist"));
            let value = initial_scalar_value_in_context(
                expression,
                &context,
                &mut stack,
                &mut locals,
                &mut passed,
            )
            .unwrap_or_else(|error| panic!("{binding} initial scalar error: {error}"));
            println!("{binding} = {value:?}");
        }

        stack.clear();
        locals.clear();
        passed.clear();
        let expression = context
            .path_bindings
            .get("store.progress_percent")
            .copied()
            .expect("progress_percent binding should exist");
        let value = initial_scalar_value_in_context(
            expression,
            &context,
            &mut stack,
            &mut locals,
            &mut passed,
        )
        .expect("initial scalar value should evaluate");
        assert_eq!(value, Some(0));
    }

    #[test]
    fn timer_duration_row_pass_object_resolves_store_binding() {
        let source =
            include_str!("../../../playground/frontend/src/examples/timer/timer.bn");
        let expressions = parse_static_expressions(source).expect("timer should parse");
        let bindings = top_level_bindings(&expressions);
        let functions = top_level_functions(&expressions, None);
        let path_bindings = flatten_binding_paths(&bindings);
        let mut scalar_plan = detect_scalar_plan(
            &path_bindings,
            &functions,
            &detect_timer_bindings(&path_bindings).expect("timer bindings should build"),
        )
        .expect("scalar plan should build");
        let static_object_lists =
            detect_static_object_list_plan(&path_bindings, &functions, &mut scalar_plan)
                .expect("static object list plan should build");
        let text_plan = detect_text_plan(&path_bindings).expect("text plan should build");
        let object_list_plan =
            detect_object_list_plan(&path_bindings, &functions, &scalar_plan, &text_plan)
                .expect("object list plan should build");
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
            .expect("top-level object field runtime should build");
        augment_linked_text_input_runtime(&mut context)
            .expect("linked text input runtime should build");
        augment_top_level_bool_item_runtime(&mut context)
            .expect("top-level bool item runtime should build");

        let mut stack = Vec::new();
        let mut locals = Vec::new();
        let mut passed = vec![PassedScope::Bindings(BTreeMap::from([(
            "store".to_string(),
            "store".to_string(),
        )]))];

        let result = with_invoked_function_scope(
            "root_element",
            &[],
            None,
            &context,
            &mut stack,
            &mut locals,
            &mut passed,
            |body, context, stack, locals, passed| {
                let StaticExpression::Block { output, .. } = &body.node else {
                    panic!("root_element should be a BLOCK");
                };
                let StaticExpression::FunctionCall { path, arguments } = &output.node else {
                    panic!("root_element output should be Element/stripe");
                };
                assert_eq!(
                    path.iter().map(|part| part.as_str()).collect::<Vec<_>>(),
                    vec!["Element", "stripe"]
                );
                let items =
                    find_named_argument(arguments, "items").expect("stripe items should exist");
                let StaticExpression::List { items } = &items.node else {
                    panic!("stripe items should be a LIST");
                };
                let duration_row_call = items
                    .iter()
                    .find(|item| {
                        matches!(
                            &item.node,
                            StaticExpression::FunctionCall { path, .. }
                                if path.iter().map(|part| part.as_str()).collect::<Vec<_>>()
                                    == vec!["duration_row"]
                        )
                    })
                    .expect("duration_row call should exist");
                let StaticExpression::FunctionCall { arguments, .. } = &duration_row_call.node
                else {
                    unreachable!();
                };
                let pass_argument =
                    find_named_argument(arguments, "PASS").expect("duration_row PASS should exist");
                passed_scope_for_expression(pass_argument, context, locals, passed, stack)
            },
        );

        let scope = result.expect("duration_row PASS should resolve");
        assert_eq!(
            scope,
            PassedScope::Bindings(BTreeMap::from([
                ("max_duration".to_string(), "store.max_duration".to_string()),
                ("store".to_string(), "store".to_string()),
            ]))
        );
    }

    #[test]
    fn timer_gauge_row_lowering_restores_outer_pass_scope() {
        let source =
            include_str!("../../../playground/frontend/src/examples/timer/timer.bn");
        let expressions = parse_static_expressions(source).expect("timer should parse");
        let bindings = top_level_bindings(&expressions);
        let functions = top_level_functions(&expressions, None);
        let path_bindings = flatten_binding_paths(&bindings);
        let mut scalar_plan = detect_scalar_plan(
            &path_bindings,
            &functions,
            &detect_timer_bindings(&path_bindings).expect("timer bindings should build"),
        )
        .expect("scalar plan should build");
        let static_object_lists =
            detect_static_object_list_plan(&path_bindings, &functions, &mut scalar_plan)
                .expect("static object list plan should build");
        let text_plan = detect_text_plan(&path_bindings).expect("text plan should build");
        let object_list_plan =
            detect_object_list_plan(&path_bindings, &functions, &scalar_plan, &text_plan)
                .expect("object list plan should build");
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
            .expect("top-level object field runtime should build");
        augment_linked_text_input_runtime(&mut context)
            .expect("linked text input runtime should build");
        augment_top_level_bool_item_runtime(&mut context)
            .expect("top-level bool item runtime should build");

        let mut stack = Vec::new();
        let mut locals = Vec::new();
        let mut passed = vec![PassedScope::Bindings(BTreeMap::from([(
            "store".to_string(),
            "store".to_string(),
        )]))];

        with_invoked_function_scope(
            "root_element",
            &[],
            None,
            &context,
            &mut stack,
            &mut locals,
            &mut passed,
            |body, context, stack, locals, passed| {
                let StaticExpression::Block { output, .. } = &body.node else {
                    panic!("root_element should be a BLOCK");
                };
                let StaticExpression::FunctionCall { arguments, .. } = &output.node else {
                    panic!("root_element output should be Element/stripe");
                };
                let items =
                    find_named_argument(arguments, "items").expect("stripe items should exist");
                let StaticExpression::List { items } = &items.node else {
                    panic!("stripe items should be a LIST");
                };
                let gauge_row = items
                    .iter()
                    .find(|item| {
                        matches!(
                            &item.node,
                            StaticExpression::FunctionCall { path, .. }
                                if path.iter().map(|part| part.as_str()).collect::<Vec<_>>()
                                    == vec!["gauge_row"]
                        )
                    })
                    .expect("gauge_row should exist");
                lower_ui_node(gauge_row, context, stack, locals, passed, None)
                    .expect("gauge_row should lower");
                assert_eq!(
                    passed.last(),
                    Some(&PassedScope::Bindings(BTreeMap::from([(
                        "store".to_string(),
                        "store".to_string(),
                    )]))),
                    "gauge_row lowering should restore outer PASS scope"
                );
                Ok(())
            },
        )
        .expect("root_element should lower gauge_row");
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
                "../../../playground/frontend/src/examples/list_object_state/list_object_state.bn"
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
            include_str!("../../../playground/frontend/src/examples/checkbox_test.bn"),
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
        assert_eq!(
            model.text_values.get("store.value").map(String::as_str),
            Some("")
        );

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
                "../../../playground/frontend/src/examples/shopping_list/shopping_list.bn"
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
                "../../../playground/frontend/src/examples/list_retain_count/list_retain_count.bn"
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

        let functions = super::top_level_functions(&expressions, None);
        let plan = detect_scalar_plan(
            &path_bindings,
            &functions,
            &detect_timer_bindings(&path_bindings).expect("timer bindings should build"),
        )
        .expect("scalar plan should build");

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
                == crate::semantic_ir::SemanticFactKind::Focused
                && binding.binding == "__element__.focused"
        }));
    }

    #[test]
    fn lower_to_semantic_todo_mvc_real_file_smoke() {
        let program = lower_to_semantic(
            include_str!("../../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn"),
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
    fn todo_mvc_physical_panel_footer_body_lowers_in_real_context() {
        let context = todo_mvc_physical_test_context();
        let mut passed = vec![PassedScope::Bindings(BTreeMap::from([
            ("store".to_string(), "store".to_string()),
            ("theme_options".to_string(), "theme_options".to_string()),
        ]))];

        let node = with_invoked_function_scope(
            "panel_footer",
            &[],
            None,
            &context,
            &mut Vec::new(),
            &mut Vec::new(),
            &mut passed,
            |body, context, stack, locals, passed| {
                lower_ui_node(body, context, stack, locals, passed, None)
            },
        )
        .expect("panel_footer should lower in the real todo_mvc_physical context");

        assert!(semantic_tree_contains_object_list_count_branch(&node, "store.todos"));
    }

    #[test]
    fn todo_mvc_physical_store_todos_pipeline_detects_append_and_retain_actions() {
        let context = todo_mvc_physical_test_context();
        let store_todos = context
            .path_bindings
            .get("store.todos")
            .copied()
            .expect("store.todos binding should exist");
        let StaticExpression::Pipe {
            from: append_expression,
            to: retain_expression,
        } = &store_todos.node
        else {
            panic!("store.todos should be an append -> retain pipe");
        };

        let append_detected = super::detect_object_list_pipeline(
            append_expression,
            &context.path_bindings,
            &context.functions,
            "store.todos",
            &context.scalar_plan,
            &context.text_plan,
        )
        .expect("append half should inspect");
        assert!(
            append_detected.is_some(),
            "store.todos append half should detect, expr={}",
            describe_expression_detailed(append_expression),
        );

        let StaticExpression::FunctionCall { arguments, .. } = &retain_expression.node else {
            panic!("store.todos tail should be List/retain");
        };
        let item_name = super::find_positional_parameter_name(arguments)
            .expect("List/retain should define item parameter");
        let condition =
            find_named_argument(arguments, "if").expect("List/retain should define `if`");
        let retain_detected = super::detect_dynamic_object_retain_actions(
            condition,
            item_name,
            &context.path_bindings,
            "store.todos",
        )
        .expect("retain half should inspect");
        assert!(
            retain_detected.is_some(),
            "store.todos retain half should detect, condition={}",
            describe_expression_detailed(condition),
        );
    }

    #[test]
    fn todo_mvc_physical_store_todos_exposes_checkbox_item_action() {
        let program = try_lower_to_semantic(
            include_str!(
                "../../../playground/frontend/src/examples/todo_mvc_physical/RUN.bn"
            ),
            None,
            false,
        )
        .expect("todo_mvc_physical should lower for Wasm");

        assert!(
            object_list_has_item_action(
                &program.root,
                "store.todos",
                "todo_elements.todo_checkbox",
                &boon_scene::UiEventKind::Click,
            ),
            "store.todos actions were {:?}",
            object_list_action_summaries(&program.root, "store.todos"),
        );
    }

    #[test]
    fn todo_mvc_physical_completed_bool_spec_detects_checkbox_and_toggle_all() {
        let context = todo_mvc_physical_test_context();
        let new_todo = context
            .functions
            .get("new_todo")
            .expect("new_todo function should exist");
        let completed = super::resolve_static_object_field_expression(
            new_todo.body,
            &context.functions,
            "completed",
        )
        .expect("new_todo.completed should exist");
        let spec = super::detect_local_bool_spec(completed)
            .expect("completed bool spec should inspect")
            .expect("completed should lower as a bool spec");

        assert!(!spec.initial);
        assert!(
            spec.events.iter().any(|event| {
                event.trigger_binding == "todo_elements.todo_checkbox"
                    && event.event_name == "click"
            }),
            "completed events were {:?}",
            spec.events,
        );
        assert!(
            spec.events.iter().any(|event| {
                event.trigger_binding == "store.elements.toggle_all_checkbox"
                    && event.event_name == "click"
            }),
            "completed events were {:?}",
            spec.events,
        );
    }

    #[test]
    fn todo_mvc_physical_selected_filter_spec_detects_filter_button_events() {
        let context = todo_mvc_physical_test_context();
        let selected_filter = context
            .path_bindings
            .get("store.selected_filter")
            .copied()
            .expect("store.selected_filter binding should exist");
        let spec = super::selected_filter_spec_for_expression(
            selected_filter,
            &context.path_bindings,
            "store.selected_filter",
        )
        .expect("selected_filter should inspect")
        .expect("selected_filter should lower as a selected-filter spec");

        assert_eq!(spec.initial_value, 0);
        assert!(
            spec.event_values.iter().any(|event| {
                event.trigger_binding == "store.elements.filter_buttons.all"
                    && event.event_name == "press"
                    && event.value == 0
            }),
            "selected_filter events were {:?}",
            spec.event_values,
        );
        assert!(
            spec.event_values.iter().any(|event| {
                event.trigger_binding == "store.elements.filter_buttons.active"
                    && event.event_name == "press"
                    && event.value == 1
            }),
            "selected_filter events were {:?}",
            spec.event_values,
        );
        assert!(
            spec.event_values.iter().any(|event| {
                event.trigger_binding == "store.elements.filter_buttons.completed"
                    && event.event_name == "press"
                    && event.value == 2
            }),
            "selected_filter events were {:?}",
            spec.event_values,
        );
    }

    #[test]
    fn todo_mvc_physical_todos_element_lowers_as_selected_filter_runtime_object_list() {
        let context = todo_mvc_physical_test_context();

        let todos_element = context
            .functions
            .get("todos_element")
            .expect("todos_element function should exist")
            .body;
        let StaticExpression::FunctionCall { arguments, .. } = &todos_element.node else {
            panic!("todos_element should lower from Scene/Element/stripe");
        };
        let outer_items = find_named_argument(arguments, "items")
            .expect("todos_element stripe should define items");
        let StaticExpression::List { items } = &outer_items.node else {
            panic!("todos_element outer items should be a LIST");
        };
        let inner_column = items
            .get(1)
            .expect("todos_element should have the list column as its second item");
        let StaticExpression::FunctionCall {
            arguments: inner_arguments,
            ..
        } = &inner_column.node
        else {
            panic!("todos_element second item should be a Scene/Element/stripe");
        };
        let items = find_named_argument(inner_arguments, "items")
            .expect("todos_element inner stripe should define items");
        let StaticExpression::Pipe { from, .. } = &items.node else {
            panic!("todos_element inner items should be a retain/map pipe");
        };
        let StaticExpression::Pipe {
            from: retain_from,
            to: retain_to,
        } = &from.node
        else {
            panic!("todos_element inner source should be a retain pipe");
        };
        let StaticExpression::FunctionCall {
            arguments: retain_arguments,
            ..
        } = &retain_to.node
        else {
            panic!("todos_element retain should be a function call");
        };
        let retain_condition = find_named_argument(retain_arguments, "if")
            .expect("todos_element retain should define if");

        let mut locals = Vec::new();
        let mut passed = vec![PassedScope::Bindings(BTreeMap::from([
            ("store".to_string(), "store".to_string()),
            ("theme_options".to_string(), "theme_options".to_string()),
        ]))];

        let lowered =
            lower_dynamic_list_items(items, &context, &mut Vec::new(), &mut locals, &mut passed)
                .expect("todos_element items should lower")
                .expect("todos_element items should use a runtime list");

        assert!(
            matches!(
                lowered.as_slice(),
                [SemanticNode::ObjectList {
                    binding,
                    filter: Some(ObjectListFilter::SelectedCompletedByScalar {
                        binding: filter_binding,
                    }),
                    ..
                }] if binding == "store.todos" && filter_binding == "store.selected_filter"
            ),
            "unexpected lowered todos list: {lowered:?}; condition={}; retain_from={}",
            describe_expression_detailed(retain_condition),
            describe_expression_detailed(retain_from),
        );
    }

    #[test]
    fn todo_mvc_physical_panel_footer_third_item_lowers_in_real_context() {
        let context = todo_mvc_physical_test_context();
        let mut passed = vec![PassedScope::Bindings(BTreeMap::from([
            ("store".to_string(), "store".to_string()),
            ("theme_options".to_string(), "theme_options".to_string()),
        ]))];

        let node = with_invoked_function_scope(
            "panel_footer",
            &[],
            None,
            &context,
            &mut Vec::new(),
            &mut Vec::new(),
            &mut passed,
            |body, context, stack, locals, passed| {
                let StaticExpression::FunctionCall { arguments, .. } = &body.node else {
                    panic!("panel_footer should lower from a Scene/Element/stripe call");
                };
                let items = find_named_argument(arguments, "items")
                    .expect("panel_footer stripe should define items");
                let StaticExpression::List { items } = &items.node else {
                    panic!("panel_footer items should be a LIST");
                };
                let third_item = items.get(2).expect("panel_footer should have a third item");
                lower_ui_node(third_item, context, stack, locals, passed, None)
            },
        )
        .expect("panel_footer third item should lower in the real todo_mvc_physical context");

        assert!(semantic_tree_contains_object_list_count_branch(&node, "store.todos"));
    }

    #[test]
    fn todo_mvc_physical_panel_footer_third_item_child_lowers_in_real_context() {
        let context = todo_mvc_physical_test_context();
        let mut passed = vec![PassedScope::Bindings(BTreeMap::from([
            ("store".to_string(), "store".to_string()),
            ("theme_options".to_string(), "theme_options".to_string()),
        ]))];

        let node = with_invoked_function_scope(
            "panel_footer",
            &[],
            None,
            &context,
            &mut Vec::new(),
            &mut Vec::new(),
            &mut passed,
            |body, context, stack, locals, passed| {
                let StaticExpression::FunctionCall { arguments, .. } = &body.node else {
                    panic!("panel_footer should lower from a Scene/Element/stripe call");
                };
                let items = find_named_argument(arguments, "items")
                    .expect("panel_footer stripe should define items");
                let StaticExpression::List { items } = &items.node else {
                    panic!("panel_footer items should be a LIST");
                };
                let third_item = items.get(2).expect("panel_footer should have a third item");
                let StaticExpression::FunctionCall { arguments, .. } = &third_item.node else {
                    panic!("panel_footer third item should be footer_section(...)");
                };
                let child = find_named_argument(arguments, "child")
                    .expect("third footer_section should define child");
                lower_ui_node(child, context, stack, locals, passed, None)
            },
        )
        .expect("panel_footer third item child should lower in the real todo_mvc_physical context");

        assert!(semantic_tree_contains_object_list_count_branch(&node, "store.todos"));
    }

    #[test]
    fn todo_mvc_physical_panel_footer_any_condition_lowers_to_list_count_compare() {
        let context = todo_mvc_physical_test_context();
        let mut passed = vec![PassedScope::Bindings(BTreeMap::from([
            ("store".to_string(), "store".to_string()),
            ("theme_options".to_string(), "theme_options".to_string()),
        ]))];

        with_invoked_function_scope(
            "panel_footer",
            &[],
            None,
            &context,
            &mut Vec::new(),
            &mut Vec::new(),
            &mut passed,
            |body, context, stack, locals, passed| {
                let StaticExpression::FunctionCall { arguments, .. } = &body.node else {
                    panic!("panel_footer should lower from a Scene/Element/stripe call");
                };
                let items = find_named_argument(arguments, "items")
                    .expect("panel_footer stripe should define items");
                let StaticExpression::List { items } = &items.node else {
                    panic!("panel_footer items should be a LIST");
                };
                let third_item = items.get(2).expect("panel_footer should have a third item");
                let StaticExpression::FunctionCall { arguments, .. } = &third_item.node else {
                    panic!("panel_footer third item should be footer_section(...)");
                };
                let child = find_named_argument(arguments, "child")
                    .expect("third footer_section should define child");
                let StaticExpression::Pipe { from, to } = &child.node else {
                    panic!("third child should be a conditional pipe");
                };
                let StaticExpression::While { arms } = &to.node else {
                    panic!("third child should use WHILE");
                };
                let (truthy, falsy) = bool_ui_branch_arms(arms)
                    .expect("bool_ui_branch_arms should inspect")
                    .expect("third child should be a bool branch");
                assert!(matches!(
                    truthy.node,
                    StaticExpression::Pipe { .. }
                ));
                assert!(matches!(
                    falsy.node,
                    StaticExpression::Literal(boon::parser::static_expression::Literal::Tag(_))
                ));

                let binding = runtime_object_list_binding_path(
                    from,
                    context,
                    locals,
                    passed,
                    &mut Vec::new(),
                )
                .expect("binding inspection should succeed");
                assert_eq!(binding.as_deref(), Some("store.todos"));

                let compare = scalar_compare_branch_operands(
                    from,
                    context,
                    locals,
                    passed,
                    stack,
                )
                .expect("scalar compare inspection should succeed");
                assert!(compare.is_some(), "List/any condition should lower to a compare");
                Ok(())
            },
        )
        .expect("panel_footer any condition should inspect in the real todo_mvc_physical context");
    }

    #[test]
    fn todo_mvc_physical_theme_mode_detects_runtime_scalar_toggle() {
        let context = todo_mvc_physical_test_context();

        assert_eq!(context.scalar_plan.initial_values.get("theme_options.mode"), Some(&1));
        assert!(
            context
                .scalar_plan
                .event_updates
                .get(&(
                    "store.elements.theme_switcher.mode_toggle".to_string(),
                    "press".to_string(),
                ))
                .is_some_and(|updates| updates.iter().any(|update| matches!(
                    update,
                    ScalarUpdate::ToggleBool { binding } if binding == "theme_options.mode"
                ))),
            "expected theme toggle press to update theme_options.mode, got {:?}",
            context.scalar_plan.event_updates
        );
    }

    #[test]
    fn todo_mvc_physical_theme_name_detects_runtime_scalar_latest() {
        let context = todo_mvc_physical_test_context();
        let professional = super::generic_tag_scalar_value("Professional");
        let glassmorphism = super::generic_tag_scalar_value("Glassmorphism");
        let neobrutalism = super::generic_tag_scalar_value("Neobrutalism");
        let neumorphism = super::generic_tag_scalar_value("Neumorphism");

        assert_eq!(
            context.scalar_plan.initial_values.get("theme_options.name"),
            Some(&professional)
        );
        for theme in [
            ("store.elements.theme_switcher.professional", professional),
            ("store.elements.theme_switcher.glassmorphism", glassmorphism),
            ("store.elements.theme_switcher.neobrutalism", neobrutalism),
            ("store.elements.theme_switcher.neumorphism", neumorphism),
        ] {
            assert!(
                context
                    .scalar_plan
                    .event_updates
                    .get(&(theme.0.to_string(), "press".to_string()))
                    .is_some_and(|updates| updates.iter().any(|update| matches!(
                        update,
                        ScalarUpdate::Set { binding, value }
                            if binding == "theme_options.name" && *value == theme.1
                    ))),
                "expected {} press to update theme_options.name to {}, got {:?}",
                theme.0,
                theme.1,
                context.scalar_plan.event_updates
            );
        }
    }

    #[test]
    fn todo_mvc_physical_theme_get_selects_professional_arm() {
        let context = todo_mvc_physical_test_context();
        let expressions: &'static [StaticSpannedExpression] = Box::leak(
            parse_static_expressions("value: Theme/get(from: Sizing, of: TouchTarget)")
                .expect("theme get source should parse")
                .into_boxed_slice(),
        );
        let bindings = top_level_bindings(expressions);
        let expression = bindings
            .get("value")
            .copied()
            .expect("value binding should exist");
        let StaticExpression::FunctionCall { arguments, .. } = &expression.node else {
            panic!("value should be a Theme/get function call");
        };
        let mut locals = Vec::new();
        let mut passed = vec![PassedScope::Bindings(BTreeMap::from([(
            "theme_options".to_string(),
            "theme_options".to_string(),
        )]))];

        with_invoked_function_scope(
            "Theme/get",
            arguments,
            None,
            &context,
            &mut Vec::new(),
            &mut locals,
            &mut passed,
            |body, context, stack, locals, passed| {
                let StaticExpression::Pipe { from, to } = &body.node else {
                    panic!("Theme/get should lower from a PASSED.theme_options.name |> WHEN");
                };
                let StaticExpression::When { arms } = &to.node else {
                    panic!("Theme/get body should use WHEN");
                };
                let selected = select_when_arm_body(from, arms, context, stack, locals, passed)?
                    .expect("Theme/get should select a theme arm");
                let StaticExpression::FunctionCall { path, .. } = &selected.node else {
                    panic!("selected Theme/get arm should call a concrete theme module");
                };
                assert_eq!(
                    path.iter().map(|part| part.as_str()).collect::<Vec<_>>(),
                    vec!["Professional", "get"]
                );
                Ok(())
            },
        )
        .expect("Theme/get should select the Professional arm in the real physical context");
    }

    #[test]
    fn todo_mvc_physical_mode_toggle_button_text_lowers_as_scalar_compare_branch() {
        let context = todo_mvc_physical_test_context();
        let mut passed = vec![PassedScope::Bindings(BTreeMap::from([
            ("store".to_string(), "store".to_string()),
            ("theme_options".to_string(), "theme_options".to_string()),
        ]))];

        let node = with_invoked_function_scope(
            "mode_toggle_button",
            &[],
            None,
            &context,
            &mut Vec::new(),
            &mut Vec::new(),
            &mut passed,
            |body, context, stack, locals, passed| {
                let StaticExpression::FunctionCall { arguments, .. } = &body.node else {
                    panic!("mode_toggle_button should lower from a Scene/Element/button call");
                };
                let label = find_named_argument(arguments, "label")
                    .expect("mode_toggle_button should define a label");
                let StaticExpression::FunctionCall {
                    arguments: label_arguments,
                    ..
                } = &label.node
                else {
                    panic!("mode_toggle_button label should be Scene/Element/text");
                };
                let text = find_named_argument(label_arguments, "text")
                    .expect("mode_toggle_button text element should define text");
                let StaticExpression::Pipe { from, to } = &text.node else {
                    panic!("mode_toggle_button text should be a pipe");
                };
                let StaticExpression::When { arms } = &to.node else {
                    panic!("mode_toggle_button text should use WHEN");
                };
                let resolved = resolve_scalar_reference(
                    from,
                    context,
                    locals,
                    passed,
                    &mut Vec::new(),
                )
                .expect("theme mode scalar should resolve");
                assert_eq!(
                    resolved.as_ref().map(|(binding, _)| binding.as_str()),
                    Some("theme_options.mode")
                );
                let source_expression = context
                    .path_bindings
                    .get("theme_options.mode")
                    .copied()
                    .expect("theme_options.mode should exist in path bindings");
                assert!(
                    scalar_binding_has_runtime_state("theme_options.mode", context),
                    "theme_options.mode should have runtime scalar state"
                );
                assert_eq!(
                    scalar_branch_pattern_value_for_source(&arms[0].pattern, Some(source_expression))
                        .expect("first arm should inspect"),
                    Some(1)
                );
                assert_eq!(
                    scalar_branch_pattern_value_for_source(&arms[1].pattern, Some(source_expression))
                        .expect("second arm should inspect"),
                    Some(0)
                );
                assert_eq!(
                    scalar_branch_all_values_for_source(source_expression)
                        .expect("theme mode source should inspect"),
                    Some(BTreeSet::from([0, 1]))
                );
                assert!(
                    lower_scalar_value_when_node(from, arms, context, stack, locals, passed)?
                        .is_some(),
                    "mode_toggle_button text should produce a scalar compare branch before the outer lower_ui_node path",
                );
                lower_ui_node(text, context, stack, locals, passed, None)
            },
        )
        .expect("mode_toggle_button text should lower in the real todo_mvc_physical context");

        assert!(
            semantic_tree_contains_scalar_compare_binding(&node, "theme_options.mode"),
            "expected mode toggle button text to lower as a scalar compare over theme_options.mode, got {node:?}",
        );
    }

    #[test]
    fn todo_mvc_physical_theme_sizing_resolves_touch_target_style_number() {
        let context = todo_mvc_physical_test_context();
        let expressions: &'static [StaticSpannedExpression] = Box::leak(
            parse_static_expressions("value: Theme/sizing(of: TouchTarget)")
                .expect("theme sizing source should parse")
                .into_boxed_slice(),
        );
        let bindings = top_level_bindings(expressions);
        let expression = bindings
            .get("value")
            .copied()
            .expect("value binding should exist");
        let mut locals = Vec::new();
        let mut passed = vec![PassedScope::Bindings(BTreeMap::from([
            ("store".to_string(), "store".to_string()),
            ("theme_options".to_string(), "theme_options".to_string()),
        ]))];

        assert_eq!(
            super::resolved_style_number(
                expression,
                &context,
                &mut Vec::new(),
                &mut locals,
                &mut passed,
            )
            .expect("style number should resolve"),
            Some(40.0)
        );
    }

    #[test]
    fn todo_mvc_physical_professional_sizing_resolves_touch_target_style_number() {
        let context = todo_mvc_physical_test_context();
        let expressions: &'static [StaticSpannedExpression] = Box::leak(
            parse_static_expressions("value: Professional/sizing(of: TouchTarget)")
                .expect("professional sizing source should parse")
                .into_boxed_slice(),
        );
        let bindings = top_level_bindings(expressions);
        let expression = bindings
            .get("value")
            .copied()
            .expect("value binding should exist");
        let mut locals = Vec::new();
        let mut passed = vec![PassedScope::Bindings(BTreeMap::from([(
            "theme_options".to_string(),
            "theme_options".to_string(),
        )]))];

        assert_eq!(
            super::resolved_style_number(
                expression,
                &context,
                &mut Vec::new(),
                &mut locals,
                &mut passed,
            )
            .expect("style number should resolve"),
            Some(40.0)
        );
    }

    #[test]
    fn todo_mvc_physical_theme_corners_resolves_pill_radius() {
        let context = todo_mvc_physical_test_context();
        let expressions: &'static [StaticSpannedExpression] = Box::leak(
            parse_static_expressions("value: Theme/corners(of: Pill)")
                .expect("theme corners source should parse")
                .into_boxed_slice(),
        );
        let bindings = top_level_bindings(expressions);
        let expression = bindings
            .get("value")
            .copied()
            .expect("value binding should exist");
        let mut locals = Vec::new();
        let mut passed = vec![PassedScope::Bindings(BTreeMap::from([
            ("store".to_string(), "store".to_string()),
            ("theme_options".to_string(), "theme_options".to_string()),
        ]))];

        assert_eq!(
            super::static_border_radius_css(
                expression,
                &context,
                &mut Vec::new(),
                &mut locals,
                &mut passed,
            )
            .expect("border radius should resolve"),
            Some("9999px".to_string())
        );
    }

    #[test]
    fn todo_mvc_physical_professional_corners_resolves_pill_radius() {
        let context = todo_mvc_physical_test_context();
        let expressions: &'static [StaticSpannedExpression] = Box::leak(
            parse_static_expressions("value: Professional/corners(of: Pill)")
                .expect("professional corners source should parse")
                .into_boxed_slice(),
        );
        let bindings = top_level_bindings(expressions);
        let expression = bindings
            .get("value")
            .copied()
            .expect("value binding should exist");
        let mut locals = Vec::new();
        let mut passed = vec![PassedScope::Bindings(BTreeMap::from([(
            "theme_options".to_string(),
            "theme_options".to_string(),
        )]))];

        assert_eq!(
            super::static_border_radius_css(
                expression,
                &context,
                &mut Vec::new(),
                &mut locals,
                &mut passed,
            )
            .expect("border radius should resolve"),
            Some("9999px".to_string())
        );
    }

    #[test]
    fn try_lower_to_semantic_todo_mvc_physical_real_file_smoke() {
        if let Err(error) = try_lower_to_semantic(
            include_str!(
                "../../../playground/frontend/src/examples/todo_mvc_physical/RUN.bn"
            ),
            None,
            false,
        ) {
            panic!("todo_mvc_physical should lower for Wasm: {error}");
        }
    }

    #[test]
    fn lower_to_semantic_supports_object_list_any_while_branch() {
        let program = lower_to_semantic(
            r#"
store: [
    todos: LIST {
        [completed: True]
        [completed: False]
    }
]

document: Document/new(root:
    store.todos
        |> List/any(item, if: item.completed)
        |> WHILE {
            True => TEXT { Has completed }
            False => NoElement
        }
)
"#,
            None,
            false,
        );

        assert!(matches!(program.root, SemanticNode::ScalarCompareBranch { .. }));
    }

    #[test]
    fn lower_to_semantic_supports_passed_object_list_any_while_branch() {
        let program = lower_to_semantic(
            r#"
store: [
    todos: LIST {
        [completed: True]
        [completed: False]
    }
]

FUNCTION footer() {
    PASSED.store.todos
        |> List/any(item, if: item.completed)
        |> WHILE {
            True => TEXT { Has completed }
            False => NoElement
        }
}

scene: Scene/new(root: footer(PASS: [store: store]))
"#,
            None,
            false,
        );

        assert!(matches!(program.root, SemanticNode::ScalarCompareBranch { .. }));
    }

    #[test]
    fn lower_to_semantic_supports_object_list_any_while_branch_inside_wrapped_static_items() {
        let program = lower_to_semantic(
            r#"
store: [
    elements: [remove_completed_button: LINK]
    todos: LIST {
        [completed: True]
        [completed: False]
    }
]

FUNCTION footer_section(child) {
    Scene/Element/block(
        element: []
        style: []
        child: child
    )
}

FUNCTION remove_completed_button() {
    Scene/Element/button(
        element: [event: [press: LINK]]
        style: []
        label: TEXT { Clear completed }
    )
}

FUNCTION footer() {
    Scene/Element/stripe(
        element: []
        direction: Row
        gap: 0
        style: []
        items: LIST {
            footer_section(child:
                PASSED.store.todos
                    |> List/any(item, if: item.completed)
                    |> WHILE {
                        True => remove_completed_button()
                            |> LINK { PASSED.store.elements.remove_completed_button }
                        False => NoElement
                    }
            )
        }
    )
}

scene: Scene/new(root: footer(PASS: [store: store]))
"#,
            None,
            false,
        );

        assert!(semantic_tree_contains_object_list_count_branch(&program.root, "store.todos"));
    }

    #[test]
    fn lower_to_semantic_supports_nested_object_list_branches_in_wrapped_static_items() {
        let program = lower_to_semantic(
            r#"
store: [
    elements: [remove_completed_button: LINK]
    todos: LIST {
        [completed: True]
        [completed: False]
    }
]

FUNCTION footer_section(child) {
    Scene/Element/block(
        element: []
        style: [width: Fill]
        child: child
    )
}

FUNCTION remove_completed_button() {
    Scene/Element/button(
        element: [event: [press: LINK]]
        style: []
        label: TEXT { Clear completed }
    )
}

FUNCTION active_items_count_text() {
    Scene/Element/text(
        element: []
        style: []
        text: TEXT { Left }
    )
}

FUNCTION filters_element() {
    Scene/Element/text(
        element: []
        style: []
        text: TEXT { Middle }
    )
}

FUNCTION panel_footer() {
    Scene/Element/stripe(
        element: []
        direction: Row
        gap: 0
        style: []
        items: LIST {
            footer_section(child: active_items_count_text())
            footer_section(child: filters_element())
            footer_section(child:
                PASSED.store.todos
                    |> List/any(item, if: item.completed)
                    |> WHILE {
                        True => remove_completed_button()
                            |> LINK { PASSED.store.elements.remove_completed_button }
                        False => NoElement
                    }
            )
        }
    )
}

FUNCTION panel() {
    PASSED.store.todos
        |> List/is_not_empty()
        |> WHILE {
            True => Scene/Element/stripe(
                element: []
                direction: Column
                gap: 0
                style: []
                items: LIST {
                    panel_footer()
                }
            )
            False => NoElement
        }
}

scene: Scene/new(root: panel(PASS: [store: store]))
"#,
            None,
            false,
        );

        assert!(semantic_tree_contains_object_list_count_branch(&program.root, "store.todos"));
    }

    #[test]
    fn lower_to_semantic_detects_nested_runtime_object_list_append_and_remove() {
        let program = lower_to_semantic(
            r#"
FUNCTION new_cell(title) {
    [title: title remove: LINK]
}

FUNCTION make_row() {
    [
        add: LINK
        cells:
            LIST {
                new_cell(title: TEXT { A })
                new_cell(title: TEXT { B })
            }
            |> List/append(item: add.event.press |> THEN { new_cell(title: TEXT { C }) })
            |> List/remove(item, on: item.remove.event.press)
    ]
}

store: [rows: LIST { make_row() }]

document: Document/new(root:
    Element/stripe(
        element: []
        direction: Column
        gap: 0
        style: []
        items:
            store.rows
            |> List/map(row, new:
                Element/stripe(
                    element: []
                    direction: Row
                    gap: 0
                    style: []
                    items: LIST {
                        Element/button(
                            element: [event: [press: LINK]]
                            style: []
                            label: TEXT { Add }
                        )
                        |> LINK { row.add }

                        Element/stripe(
                            element: []
                            direction: Row
                            gap: 0
                            style: []
                            items:
                                row.cells
                                |> List/map(cell, new:
                                    Element/stripe(
                                        element: []
                                        direction: Row
                                        gap: 0
                                        style: []
                                        items: LIST {
                                            Element/label(
                                                element: []
                                                style: []
                                                label: cell.title
                                            )
                                            Element/button(
                                                element: [event: [press: LINK]]
                                                style: []
                                                label: TEXT { x }
                                            )
                                            |> LINK { cell.remove }
                                        }
                                    )
                                )
                        )
                    }
                )
            )
    )
)
"#,
            None,
            false,
        );

        let RuntimeModel::State(model) = &program.runtime else {
            panic!("expected state runtime model for nested runtime object list test");
        };
        let rows = model
            .object_lists
            .get("store.rows")
            .expect("rows object list should exist");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].object_lists.get("cells").map(Vec::len), Some(2));

        let SemanticNode::Element { children, .. } = &program.root else {
            panic!("expected element root for nested runtime object list test");
        };
        assert!(matches!(
            &children[0],
            SemanticNode::ObjectList { binding, template, .. }
                if binding == "store.rows"
                    && semantic_tree_contains_object_list_binding(template, "__item__.cells")
        ));
    }

    #[test]
    fn lower_to_semantic_cells_real_file_smoke() {
        let program = super::parse_and_lower(
            include_str!("../../../playground/frontend/src/examples/cells/cells.bn"),
            None,
        )
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
        assert_eq!(
            model.object_lists.get("all_row_cells").map(Vec::len),
            Some(100)
        );
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
                "display_element",
                &boon_scene::UiEventKind::DoubleClick,
            ),
            "cells nested list should expose display DoubleClick item action"
        );
        assert!(
            object_list_has_item_action(
                &program.root,
                "__item__.cells",
                "editing_element",
                &boon_scene::UiEventKind::Input,
            ),
            "cells nested list should expose editing Input item action; nested actions: {:?}",
            object_list_action_summaries(&program.root, "__item__.cells")
        );
        assert!(
            object_list_has_item_action(
                &program.root,
                "__item__.cells",
                "editing_element",
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
    fn cells_overrides_hold_lowers_as_enter_keydown_object_list_update() {
        let context = cells_test_context("");
        let item_actions = context
            .object_list_plan
            .item_actions
            .iter()
            .filter_map(|(binding, actions)| {
                let matching = actions
                    .iter()
                    .filter(|action| {
                        matches!(
                            &action.action,
                            ObjectItemActionKind::UpdateBindings {
                                object_list_updates,
                                payload_filter: Some(filter),
                                ..
                            } if filter == "Enter"
                                && object_list_updates.iter().any(|update| matches!(
                                    update,
                                    ObjectListUpdate::AppendBoundObject {
                                        binding,
                                        scalar_bindings,
                                        text_bindings,
                                        payload_filter: Some(filter),
                                    } if binding == "overrides"
                                        && filter == "Enter"
                                        && scalar_bindings.get("row").map(String::as_str)
                                            == Some("edit_committed.row")
                                        && scalar_bindings.get("column").map(String::as_str)
                                            == Some("edit_committed.column")
                                        && text_bindings.get("text").map(String::as_str)
                                            == Some("edit_committed.text")
                                ))
                        )
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                (!matching.is_empty()).then_some((binding.clone(), matching))
            })
            .collect::<Vec<_>>();

        assert!(
            !item_actions.is_empty(),
            "expected Enter-filtered item action carrying overrides append, got {:?}",
            item_actions
        );
    }

    #[test]
    fn cells_edit_committed_text_lowers_as_keydown_input_source() {
        let context = cells_test_context("");
        let matching = context
            .object_list_plan
            .item_actions
            .iter()
            .filter_map(|(binding, actions)| {
                let matching = actions
                    .iter()
                    .filter(|action| {
                        action.source_binding_suffix == "editing_element"
                            && action.kind == boon_scene::UiEventKind::KeyDown
                            && matches!(
                                &action.action,
                                ObjectItemActionKind::UpdateBindings { text_updates, .. }
                                    if text_updates.iter().any(|update| matches!(
                                        update,
                                        crate::semantic_ir::ItemTextUpdate::SetFromInputSource {
                                            binding,
                                            source_suffix
                                        } if binding == "event_ports.edit_committed.text"
                                            && source_suffix == "editing_element"
                                    ))
                            )
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                (!matching.is_empty()).then_some((binding.clone(), matching))
            })
            .collect::<Vec<_>>();

        assert!(
            !matching.is_empty(),
            "expected cells keydown item action to preserve edit_committed.text as SetFromInputSource, got {:?}",
            context.object_list_plan.item_actions
        );
    }

    #[test]
    fn cells_edit_committed_alias_fields_mirror_event_ports_fields() {
        let context = cells_test_context("");

        assert!(
            context
                .scalar_mirrors
                .get("event_ports.edit_committed.row")
                .is_some_and(|targets| targets.iter().any(|target| target == "edit_committed.row")),
            "expected scalar mirror event_ports.edit_committed.row -> edit_committed.row, got {:?}",
            context.scalar_mirrors
        );
        assert!(
            context
                .scalar_mirrors
                .get("event_ports.edit_committed.column")
                .is_some_and(|targets| targets.iter().any(|target| target == "edit_committed.column")),
            "expected scalar mirror event_ports.edit_committed.column -> edit_committed.column, got {:?}",
            context.scalar_mirrors
        );
        assert!(
            context
                .text_mirrors
                .get("event_ports.edit_committed.text")
                .is_some_and(|targets| targets.iter().any(|target| target == "edit_committed.text")),
            "expected text mirror event_ports.edit_committed.text -> edit_committed.text, got {:?}",
            context.text_mirrors
        );
    }

    #[test]
    fn cells_double_click_updates_event_ports_from_item_fields() {
        let context = cells_test_context("");
        let actions = context
            .object_list_plan
            .item_actions
            .get("__item__.cells")
            .expect("cells nested list should expose runtime item actions");

        assert!(
            actions.iter().any(|action| {
                action.source_binding_suffix == "display_element"
                    && action.kind == boon_scene::UiEventKind::DoubleClick
                    && matches!(
                        &action.action,
                        ObjectItemActionKind::UpdateBindings {
                            scalar_updates,
                            ..
                        } if scalar_updates.iter().any(|update| matches!(
                            update,
                            ItemScalarUpdate::SetFromField { binding, field }
                                if binding == "event_ports.edit_started_row" && field == "row"
                        )) && scalar_updates.iter().any(|update| matches!(
                            update,
                            ItemScalarUpdate::SetFromField { binding, field }
                                if binding == "event_ports.edit_started_column" && field == "column"
                        ))
                    )
            }),
            "expected cells double-click item action to set event_ports.edit_started_row/column from item fields, got {:?}",
            actions
        );
    }

    #[test]
    fn fibonacci_document_lowers_logged_skipped_current_field_access() {
        let source =
            include_str!("../../../playground/frontend/src/examples/fibonacci/fibonacci.bn");

        let expressions = parse_static_expressions(source).expect("fibonacci should parse");
        let bindings = top_level_bindings(&expressions);
        let functions = top_level_functions(&expressions, None);
        let mut path_bindings = flatten_binding_paths(&bindings);
        let bindings_for_document = bindings.clone();
        let document = find_document_expression(&expressions, &bindings_for_document)
            .expect("document should resolve");
        if let Some(root_expression) = synthetic_document_root_expression(document) {
            path_bindings.insert(SYNTHETIC_DOCUMENT_ROOT_BINDING.to_string(), root_expression);
        }
        if let Some(timer_expression) = synthetic_document_root_timer_expression(document) {
            path_bindings.insert(
                SYNTHETIC_DOCUMENT_ROOT_TIMER_BINDING.to_string(),
                timer_expression,
            );
        }
        let timer_bindings = detect_timer_bindings(&path_bindings).expect("timer bindings should build");
        let mut scalar_plan =
            detect_scalar_plan(&path_bindings, &functions, &timer_bindings).expect("scalar plan should build");
        let static_object_lists = detect_static_object_list_plan(&path_bindings, &functions, &mut scalar_plan)
            .expect("static object list plan should build");
        let text_plan = detect_text_plan(&path_bindings).expect("text plan should build");
        let object_list_plan =
            detect_object_list_plan(&path_bindings, &functions, &scalar_plan, &text_plan)
                .expect("object list plan should build");
        let mut context = LowerContext {
            text_plan,
            list_plan: detect_list_plan(&path_bindings).expect("list plan should build"),
            object_list_plan,
            scalar_plan,
            static_object_lists,
            timer_bindings,
            bindings,
            path_bindings,
            functions,
            ..LowerContext::default()
        };
        augment_top_level_object_field_runtime(&mut context)
            .expect("top-level object field runtime should build");
        augment_hold_alias_runtime(&mut context).expect("hold alias runtime should build");
        augment_document_link_forwarder_item_runtime(&mut context)
            .expect("document link forwarders should build");
        augment_object_alias_field_runtime(&mut context)
            .expect("object alias field runtime should build");
        augment_linked_text_input_runtime(&mut context)
            .expect("linked text input runtime should build");
        augment_top_level_bool_item_runtime(&mut context)
            .expect("top-level bool item runtime should build");
        augment_dependent_object_list_updates_from_item_bindings(&mut context)
            .expect("dependent object list runtime should build");
        seed_missing_scalar_initial_values(&mut context)
            .expect("missing scalar initials should seed");

        let result_expression = context
            .path_bindings
            .get("result")
            .copied()
            .expect("result binding should exist");
        let result_value = initial_scalar_value_in_context(
            result_expression,
            &context,
            &mut Vec::new(),
            &mut Vec::new(),
            &mut Vec::new(),
        )
        .expect("result should evaluate");
        assert_eq!(result_value, Some(55));

        let program = lower_to_semantic(source, None, false);

        let rendered = match &program.root {
            SemanticNode::Text(text) => text.clone(),
            SemanticNode::TextTemplate { value, .. } => value.clone(),
            other => panic!("expected text root, got {other:?}"),
        };

        assert_eq!(rendered, "10. Fibonacci number is 55");
    }

    #[test]
    fn cells_template_compares_editing_cell_against_item_coordinates() {
        let program = super::parse_and_lower(
            include_str!("../../../playground/frontend/src/examples/cells/cells.bn"),
            None,
        )
        .expect("cells should lower");

        assert!(
            semantic_tree_contains_cells_editing_compare(&program.root),
            "expected cells template to retain a dynamic editing_cell row/column compare, got {:?}",
            program.root
        );
    }

    #[test]
    fn cells_editing_column_compare_lowers_as_object_scalar_branch() {
        let context = cells_test_context("");
        let function = context
            .functions
            .get("is_editing_cell")
            .expect("is_editing_cell function should exist");
        let StaticExpression::Pipe { to, .. } = &function.body.node else {
            panic!("is_editing_cell should lower from a row comparator WHEN");
        };
        let StaticExpression::When { arms } = &to.node else {
            panic!("is_editing_cell should use WHEN");
        };
        let truthy_body = &arms[0].body;
        let StaticExpression::Comparator(_) = &truthy_body.node else {
            panic!("truthy body should be the column comparator");
        };

        let mut locals = vec![BTreeMap::from([(
            "cell".to_string(),
            LocalBinding {
                expr: None,
                object_base: Some("__item__".to_string()),
            },
        )])];
        let mut passed = Vec::new();
        let lowered = lower_bool_condition_from_nodes(
            truthy_body,
            SemanticNode::text("editing"),
            SemanticNode::text("display"),
            &context,
            &mut Vec::new(),
            &mut locals,
            &mut passed,
        )
        .expect("column comparator should inspect");

        assert!(
            matches!(lowered, Some(SemanticNode::ObjectScalarCompareBranch { .. })),
            "expected column comparator to lower as object scalar compare branch, got {lowered:?}"
        );
    }

    #[test]
    fn cells_make_cell_element_output_retains_dynamic_editing_branch() {
        let context = cells_test_context("");
        let function = context
            .functions
            .get("make_cell_element")
            .expect("make_cell_element function should exist");
        let StaticExpression::Block { variables, output } = &function.body.node else {
            panic!("make_cell_element should lower from a block");
        };

        let mut locals = vec![BTreeMap::from([(
            "cell".to_string(),
            LocalBinding {
                expr: None,
                object_base: Some("__item__".to_string()),
            },
        )])];
        let mut scope = BTreeMap::new();
        let passed = &mut Vec::new();
        for variable in variables {
            let object_base =
                infer_argument_object_base(&variable.node.value, &context, &locals, passed)
                    .or_else(|| active_object_scope(&locals));
            scope.insert(
                variable.node.name.as_str().to_string(),
                LocalBinding {
                    expr: Some(&variable.node.value),
                    object_base,
                },
            );
        }
        locals.push(scope);

        let lowered = lower_ui_node(
            output,
            &context,
            &mut Vec::new(),
            &mut locals,
            passed,
            None,
        )
        .expect("make_cell_element output should lower");

        assert!(
            semantic_tree_contains_cells_editing_compare(&lowered),
            "expected make_cell_element output to retain a dynamic editing compare, got {lowered:?}"
        );
    }

    #[test]
    fn cells_make_cell_element_column_compare_operands_resolve_in_block_scope() {
        let context = cells_test_context("");
        let function = context
            .functions
            .get("is_editing_cell")
            .expect("is_editing_cell function should exist");
        let StaticExpression::Pipe { to, .. } = &function.body.node else {
            panic!("is_editing_cell should lower from a row comparator WHEN");
        };
        let StaticExpression::When { arms } = &to.node else {
            panic!("is_editing_cell should use WHEN");
        };
        let truthy_body = &arms[0].body;
        let StaticExpression::Comparator(StaticComparator::Equal {
            operand_a,
            operand_b,
        }) = &truthy_body.node
        else {
            panic!("truthy body should be the column comparator");
        };

        let make_cell = context
            .functions
            .get("make_cell_element")
            .expect("make_cell_element function should exist");
        let StaticExpression::Block { variables, .. } = &make_cell.body.node else {
            panic!("make_cell_element should lower from a block");
        };

        let mut locals = vec![BTreeMap::from([(
            "cell".to_string(),
            LocalBinding {
                expr: None,
                object_base: Some("__item__".to_string()),
            },
        )])];
        let passed = &mut Vec::new();
        let mut scope = BTreeMap::new();
        for variable in variables {
            let object_base =
                infer_argument_object_base(&variable.node.value, &context, &locals, passed)
                    .or_else(|| active_object_scope(&locals));
            scope.insert(
                variable.node.name.as_str().to_string(),
                LocalBinding {
                    expr: Some(&variable.node.value),
                    object_base,
                },
            );
        }
        locals.push(scope);

        let left = object_derived_scalar_operand_in_context(
            operand_a,
            &context,
            &locals,
            passed,
            &mut Vec::new(),
        )
        .expect("left operand should inspect");
        let right = object_derived_scalar_operand_in_context(
            operand_b,
            &context,
            &locals,
            passed,
            &mut Vec::new(),
        )
        .expect("right operand should inspect");

        assert_eq!(
            left,
            Some(ObjectDerivedScalarOperand::Binding(
                "editing_cell.column".to_string()
            ))
        );
        assert_eq!(
            right,
            Some(ObjectDerivedScalarOperand::Field("column".to_string()))
        );
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
        let locals = vec![
            variables
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
                .collect(),
        ];
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
        let mut locals = vec![
            variables
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
                .collect(),
        ];
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
            include_str!("../../../playground/frontend/src/examples/cells/cells.bn")
        );
        let leaked = Box::leak(source.into_boxed_str());
        let expressions: &'static [StaticSpannedExpression] = Box::leak(
            parse_static_expressions(leaked)
                .expect("cells source should parse")
                .into_boxed_slice(),
        );
        let bindings = top_level_bindings(expressions);
        let functions = top_level_functions(expressions, None);
        let path_bindings = flatten_binding_paths(&bindings);
        let mut scalar_plan = detect_scalar_plan(
            &path_bindings,
            &functions,
            &detect_timer_bindings(&path_bindings).expect("timer bindings should build"),
        )
        .expect("scalar plan should build");
        let static_object_lists =
            detect_static_object_list_plan(&path_bindings, &functions, &mut scalar_plan)
                .expect("static object list plan should build");
        let text_plan = detect_text_plan(&path_bindings).expect("text plan should build");
        let object_list_plan =
            detect_object_list_plan(&path_bindings, &functions, &scalar_plan, &text_plan)
                .expect("object list plan should build");
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
        augment_document_link_forwarder_item_runtime(&mut context)
            .expect("document link forwarder runtime should augment");
        augment_object_alias_field_runtime(&mut context)
            .expect("object alias field runtime should augment");
        augment_linked_text_input_runtime(&mut context)
            .expect("linked text input runtime should augment");
        augment_top_level_bool_item_runtime(&mut context)
            .expect("top-level bool runtime should augment");
        augment_dependent_object_list_updates_from_item_bindings(&mut context)
            .expect("dependent object list updates should augment");
        context
    }

    fn crud_test_context() -> LowerContext<'static> {
        let source =
            include_str!("../../../playground/frontend/src/examples/crud/crud.bn");
        let expressions: &'static [StaticSpannedExpression] = Box::leak(
            parse_static_expressions(source)
                .expect("crud source should parse")
                .into_boxed_slice(),
        );
        let bindings = top_level_bindings(expressions);
        let functions = top_level_functions(expressions, None);
        let path_bindings = flatten_binding_paths(&bindings);
        let mut scalar_plan = detect_scalar_plan(
            &path_bindings,
            &functions,
            &detect_timer_bindings(&path_bindings).expect("timer bindings should build"),
        )
        .expect("scalar plan should build");
        let static_object_lists =
            detect_static_object_list_plan(&path_bindings, &functions, &mut scalar_plan)
                .expect("static object list plan should build");
        let text_plan = detect_text_plan(&path_bindings).expect("text plan should build");
        let object_list_plan =
            detect_object_list_plan(&path_bindings, &functions, &scalar_plan, &text_plan)
                .expect("object list plan should build");
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
        augment_document_link_forwarder_item_runtime(&mut context)
            .expect("document link forwarder runtime should augment");
        augment_object_alias_field_runtime(&mut context)
            .expect("object alias field runtime should augment");
        augment_linked_text_input_runtime(&mut context)
            .expect("linked text input runtime should augment");
        augment_top_level_bool_item_runtime(&mut context)
            .expect("top-level bool runtime should augment");
        augment_dependent_object_list_updates_from_item_bindings(&mut context)
            .expect("dependent object list updates should augment");
        context
    }

    #[test]
    fn crud_initial_new_person_item_resolves_as_runtime_object() {
        let source =
            include_str!("../../../playground/frontend/src/examples/crud/crud.bn");
        let expressions: &'static [StaticSpannedExpression] = Box::leak(
            parse_static_expressions(source)
                .expect("crud source should parse")
                .into_boxed_slice(),
        );
        let bindings = top_level_bindings(expressions);
        let functions = top_level_functions(expressions, None);
        let path_bindings = flatten_binding_paths(&bindings);
        let people = path_bindings
            .get("store.people")
            .copied()
            .expect("store.people binding should exist");
        let mut source_expr = people;
        while let StaticExpression::Pipe { from, .. } = &source_expr.node {
            source_expr = from;
        }
        let StaticExpression::List { items } = &source_expr.node else {
            panic!("store.people source should be a list");
        };
        let first_item = items.first().expect("store.people should have an initial item");
        assert!(
            resolve_static_object_runtime_expression(first_item, &functions)
                .expect("new_person should inspect")
                .is_some(),
            "expected first new_person(...) item to resolve as a runtime object, got {}",
            describe_expression_detailed(first_item)
        );
    }

    #[test]
    fn crud_append_new_person_body_resolves_as_runtime_object() {
        let source =
            include_str!("../../../playground/frontend/src/examples/crud/crud.bn");
        let expressions: &'static [StaticSpannedExpression] = Box::leak(
            parse_static_expressions(source)
                .expect("crud source should parse")
                .into_boxed_slice(),
        );
        let bindings = top_level_bindings(expressions);
        let functions = top_level_functions(expressions, None);
        let path_bindings = flatten_binding_paths(&bindings);
        let person_to_add = path_bindings
            .get("store.person_to_add")
            .copied()
            .expect("store.person_to_add binding should exist");
        let StaticExpression::Pipe { to, .. } = &person_to_add.node else {
            panic!("store.person_to_add should be a then-pipe");
        };
        let StaticExpression::Then { body } = &to.node else {
            panic!("store.person_to_add should end in THEN");
        };
        assert!(
            resolve_static_object_runtime_expression(body, &functions)
                .expect("append body should inspect")
                .is_some(),
            "expected appended new_person(...) body to resolve as a runtime object, got {}",
            describe_expression_detailed(body)
        );
    }

    #[test]
    fn crud_selected_id_map_source_resolves_store_people_runtime_list_ref() {
        let context = crud_test_context();
        let store = context
            .bindings
            .get("store")
            .copied()
            .expect("store binding should exist");
        let StaticExpression::Object(store_object) = &store.node else {
            panic!("store binding should be an object");
        };
        let selected_id = find_object_field(store_object, "selected_id")
            .expect("store.selected_id field should exist");
        let StaticExpression::Pipe { to, .. } = &selected_id.node else {
            panic!("store.selected_id should be a hold pipe");
        };
        let StaticExpression::Hold { body, .. } = &to.node else {
            panic!("store.selected_id should end in HOLD");
        };
        let StaticExpression::Latest { inputs } = &body.node else {
            panic!("store.selected_id HOLD body should be LATEST");
        };
        let map_latest = inputs
            .iter()
            .find(|input| {
                matches!(
                    &input.node,
                    StaticExpression::Pipe {
                        to,
                        ..
                    } if matches!(
                        &to.node,
                        StaticExpression::FunctionCall { path, arguments }
                            if path_matches(path, &["List", "latest"]) && arguments.is_empty()
                    )
                )
            })
            .expect("selected_id should include map/latest input");
        let StaticExpression::Pipe { from, to } = &map_latest.node else {
            panic!("selected_id map input should be a pipe");
        };
        let StaticExpression::FunctionCall { path, arguments } = &to.node else {
            panic!("selected_id map input should end in List/latest");
        };
        assert!(
            path_matches(path, &["List", "latest"]) && arguments.is_empty(),
            "selected_id map input should end in List/latest"
        );
        let StaticExpression::Pipe {
            from: map_source,
            to: map_to,
        } = &from.node
        else {
            panic!("selected_id latest source should be a map pipe");
        };
        let StaticExpression::FunctionCall {
            path: map_path,
            arguments: map_arguments,
        } = &map_to.node
        else {
            panic!("selected_id latest source should end in List/map");
        };
        assert!(
            path_matches(map_path, &["List", "map"]),
            "selected_id latest source should be List/map"
        );
        let mapper_name = find_positional_parameter_name(map_arguments)
            .expect("selected_id List/map should define item parameter");
        let mut locals = vec![store_object
            .variables
            .iter()
            .filter_map(|variable| {
                let name = variable.node.name.as_str();
                (!name.is_empty()).then_some((
                    name.to_string(),
                    LocalBinding {
                        expr: Some(&variable.node.value),
                        object_base: Some(format!("store.{name}")),
                    },
                ))
            })
            .collect::<BTreeMap<_, _>>()];
        let mut passed = Vec::new();
        let list_ref = runtime_object_list_ref(
            map_source,
            &context,
            &locals,
            &passed,
            &mut Vec::new(),
        )
        .expect("selected_id map source should inspect")
        .expect("selected_id map source should resolve to a runtime object list");
        assert_eq!(list_ref.binding, "store.people");
        locals.push(BTreeMap::from([(
            mapper_name.to_string(),
            LocalBinding {
                expr: None,
                object_base: Some("__item__".to_string()),
            },
        )]));
        let new_expr = find_named_argument(map_arguments, "new")
            .expect("selected_id List/map should define new");
        let StaticExpression::Pipe {
            from: trigger_source,
            ..
        } = &new_expr.node
        else {
            panic!("selected_id mapped action should be a then-pipe");
        };
        assert_eq!(
            detect_item_event_source(trigger_source, &context, &locals, &passed)
                .expect("trigger source should inspect")
                .as_deref(),
            Some("person_elements.row:press")
        );
    }

    fn todo_mvc_test_context() -> LowerContext<'static> {
        let source =
            include_str!("../../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn");
        let expressions: &'static [StaticSpannedExpression] = Box::leak(
            parse_static_expressions(source)
                .expect("todo_mvc source should parse")
                .into_boxed_slice(),
        );
        let bindings = top_level_bindings(expressions);
        let functions = top_level_functions(expressions, None);
        let path_bindings = flatten_binding_paths(&bindings);
        let mut scalar_plan = detect_scalar_plan(
            &path_bindings,
            &functions,
            &detect_timer_bindings(&path_bindings).expect("timer bindings should build"),
        )
        .expect("scalar plan should build");
        let static_object_lists =
            detect_static_object_list_plan(&path_bindings, &functions, &mut scalar_plan)
                .expect("static object list plan should build");
        let text_plan = detect_text_plan(&path_bindings).expect("text plan should build");
        let object_list_plan =
            detect_object_list_plan(&path_bindings, &functions, &scalar_plan, &text_plan)
                .expect("object list plan should build");
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
        augment_document_link_forwarder_item_runtime(&mut context)
            .expect("document link forwarder runtime should augment");
        augment_object_alias_field_runtime(&mut context)
            .expect("object alias field runtime should augment");
        augment_linked_text_input_runtime(&mut context)
            .expect("linked text input runtime should augment");
        augment_top_level_bool_item_runtime(&mut context)
            .expect("top-level bool runtime should augment");
        augment_dependent_object_list_updates_from_item_bindings(&mut context)
            .expect("dependent object list updates should augment");
        context
    }

    fn todo_mvc_physical_test_context() -> LowerContext<'static> {
        let source = include_str!(
            "../../../playground/frontend/src/examples/todo_mvc_physical/RUN.bn"
        );
        let external_functions = todo_mvc_physical_external_functions();
        let expressions: &'static [StaticSpannedExpression] = Box::leak(
            parse_static_expressions(source)
                .expect("todo_mvc_physical source should parse")
                .into_boxed_slice(),
        );
        let bindings = top_level_bindings(expressions);
        let functions = top_level_functions(expressions, Some(external_functions));
        let path_bindings = flatten_binding_paths(&bindings);
        let mut scalar_plan = detect_scalar_plan(
            &path_bindings,
            &functions,
            &detect_timer_bindings(&path_bindings).expect("timer bindings should build"),
        )
        .expect("scalar plan should build");
        let static_object_lists =
            detect_static_object_list_plan(&path_bindings, &functions, &mut scalar_plan)
                .expect("static object list plan should build");
        let text_plan = detect_text_plan(&path_bindings).expect("text plan should build");
        let object_list_plan =
            detect_object_list_plan(&path_bindings, &functions, &scalar_plan, &text_plan)
                .expect("object list plan should build");
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
        augment_document_link_forwarder_item_runtime(&mut context)
            .expect("document link forwarder runtime should augment");
        augment_object_alias_field_runtime(&mut context)
            .expect("object alias field runtime should augment");
        augment_linked_text_input_runtime(&mut context)
            .expect("linked text input runtime should augment");
        augment_top_level_bool_item_runtime(&mut context)
            .expect("top-level bool runtime should augment");
        augment_dependent_object_list_updates_from_item_bindings(&mut context)
            .expect("dependent object list updates should augment");
        context
    }

    fn todo_mvc_physical_external_functions() -> &'static [super::ExternalFunction] {
        let mut functions = Vec::new();
        functions.extend(module_external_functions(
            "Assets",
            include_str!(
                "../../../playground/frontend/src/examples/todo_mvc_physical/Generated/Assets.bn"
            ),
        ));
        functions.extend(module_external_functions(
            "Theme",
            include_str!(
                "../../../playground/frontend/src/examples/todo_mvc_physical/Theme/Theme.bn"
            ),
        ));
        functions.extend(module_external_functions(
            "Professional",
            include_str!(
                "../../../playground/frontend/src/examples/todo_mvc_physical/Theme/Professional.bn"
            ),
        ));
        functions.extend(module_external_functions(
            "Glassmorphism",
            include_str!(
                "../../../playground/frontend/src/examples/todo_mvc_physical/Theme/Glassmorphism.bn"
            ),
        ));
        functions.extend(module_external_functions(
            "Neobrutalism",
            include_str!(
                "../../../playground/frontend/src/examples/todo_mvc_physical/Theme/Neobrutalism.bn"
            ),
        ));
        functions.extend(module_external_functions(
            "Neumorphism",
            include_str!(
                "../../../playground/frontend/src/examples/todo_mvc_physical/Theme/Neumorphism.bn"
            ),
        ));
        Box::leak(functions.into_boxed_slice())
    }

    fn module_external_functions(
        module_name: &str,
        source: &'static str,
    ) -> Vec<super::ExternalFunction> {
        let expressions = parse_static_expressions(source).expect("module source should parse");
        expressions
            .into_iter()
            .filter_map(|expression| match expression.node {
                StaticExpression::Function {
                    name,
                    parameters,
                    body,
                } => Some((
                    format!("{module_name}/{}", name.as_str()),
                    parameters
                        .into_iter()
                        .map(|parameter| parameter.node.as_str().to_string())
                        .collect(),
                    *body,
                    Some(module_name.to_string()),
                )),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn crud_people_list_lowers_as_filtered_runtime_object_list() {
        let context = crud_test_context();
        let initial_people = context
            .object_list_plan
            .initial_values
            .get("store.people")
            .expect("store.people should be a runtime object list");
        assert_eq!(
            initial_people.len(),
            3,
            "store.people should seed three rows"
        );
        assert_eq!(
            initial_people[0]
                .text_fields
                .get("name")
                .map(String::as_str),
            Some("Hans")
        );
        assert_eq!(
            initial_people[0]
                .text_fields
                .get("surname")
                .map(String::as_str),
            Some("Emil")
        );

        let people_list = context
            .functions
            .get("people_list")
            .expect("people_list function should exist")
            .body;
        let StaticExpression::Block { variables, output } = &people_list.node else {
            panic!("people_list should lower from a block");
        };
        let StaticExpression::FunctionCall { arguments, .. } = &output.node else {
            panic!("people_list block output should be Element/stripe");
        };
        let items = find_named_argument(arguments, "items")
            .expect("people_list stripe should define items");
        let StaticExpression::Pipe { from, .. } = &items.node else {
            panic!("people_list items should be a retain/map pipe");
        };

        let mut locals = vec![eager_block_scope(
            variables,
            &context,
            &mut Vec::new(),
            &Vec::new(),
            &vec![PassedScope::Bindings(BTreeMap::from([(
                "store".to_string(),
                "store".to_string(),
            )]))],
        )];
        let mut passed = vec![PassedScope::Bindings(BTreeMap::from([(
            "store".to_string(),
            "store".to_string(),
        )]))];
        let StaticExpression::Pipe {
            from: retain_from,
            to: retain_to,
        } = &from.node
        else {
            panic!("people_list source should be a retain pipe");
        };
        let binding_path = runtime_object_list_binding_path(
            retain_from,
            &context,
            &locals,
            &passed,
            &mut Vec::new(),
        )
        .expect("retain base binding should inspect");
        assert_eq!(binding_path.as_deref(), Some("store.people"));
        let StaticExpression::FunctionCall {
            arguments: retain_arguments,
            ..
        } = &retain_to.node
        else {
            panic!("retain source should end in List/retain");
        };
        let retain_condition =
            find_named_argument(retain_arguments, "if").expect("retain should define if");
        let StaticExpression::Pipe {
            to: retain_filter_to,
            ..
        } = &retain_condition.node
        else {
            panic!("retain condition should be a Text/starts_with pipe");
        };
        let StaticExpression::FunctionCall {
            arguments: filter_arguments,
            ..
        } = &retain_filter_to.node
        else {
            panic!("retain condition should call Text/starts_with");
        };
        let filter_prefix = find_named_argument(filter_arguments, "prefix")
            .expect("retain filter should define prefix");
        let prefix_binding = runtime_text_binding_path(filter_prefix, &context, &locals, &passed)
            .expect("retain prefix should inspect");
        assert_eq!(
            prefix_binding.as_deref(),
            Some("store.elements.filter_input.text")
        );
        let filter_input_triggers = trigger_specs_for_runtime_binding(
            "store.elements.filter_input.text",
            &context.scalar_plan,
            &context.text_plan,
        );
        assert!(
            filter_input_triggers
                .iter()
                .any(|spec| spec.trigger_binding == "store.elements.filter_input"
                    && spec.event_name == "change"),
            "filter input should expose runtime change trigger, got {filter_input_triggers:?}"
        );
        let filter =
            runtime_object_list_filter(retain_condition, "item", &context, &locals, &passed)
                .expect("retain filter should inspect");
        assert!(
            matches!(
                filter,
                Some(ObjectListFilter::TextFieldStartsWithTextBinding {
                    ref field,
                    binding: ref filter_binding,
                }) if field == "surname" && filter_binding == "store.elements.filter_input.text"
            ),
            "unexpected retain filter: {filter:?}"
        );
        let list_ref = runtime_object_list_ref(from, &context, &locals, &passed, &mut Vec::new())
            .expect("retain source should inspect");
        assert!(
            matches!(
                list_ref,
                Some(RuntimeObjectListRef {
                    ref binding,
                    filter: Some(ObjectListFilter::TextFieldStartsWithTextBinding {
                        ref field,
                        binding: ref filter_binding,
                    }),
                }) if binding == "store.people"
                    && field == "surname"
                    && filter_binding == "store.elements.filter_input.text"
            ),
            "unexpected runtime object list ref: {list_ref:?}"
        );

        let lowered =
            lower_dynamic_list_items(items, &context, &mut Vec::new(), &mut locals, &mut passed)
                .expect("people_list items should lower")
                .expect("people_list items should use a runtime list");

        assert!(matches!(
            lowered.as_slice(),
            [SemanticNode::ObjectList {
                binding,
                filter: Some(ObjectListFilter::TextFieldStartsWithTextBinding {
                    field,
                    binding: filter_binding,
                }),
                ..
            }] if binding == "store.people"
                && field == "surname"
                && filter_binding == "store.elements.filter_input.text"
        ));

        let [SemanticNode::ObjectList { template, .. }] = lowered.as_slice() else {
            panic!("people_list should lower to a single ObjectList node");
        };
        assert!(
            semantic_tree_contains_object_text_binding(template, "surname"),
            "people_list template should render surname text binding"
        );
        assert!(
            semantic_tree_contains_object_text_binding(template, "name"),
            "people_list template should render name text binding"
        );
    }

    #[test]
    fn todo_mvc_todos_element_lowers_as_selected_filter_runtime_object_list() {
        let context = todo_mvc_test_context();

        let todos_element = context
            .functions
            .get("todos_element")
            .expect("todos_element function should exist")
            .body;
        let StaticExpression::FunctionCall { arguments, .. } = &todos_element.node else {
            panic!("todos_element should lower from Element/stripe");
        };
        let items = find_named_argument(arguments, "items")
            .expect("todos_element stripe should define items");
        let StaticExpression::Pipe { from, .. } = &items.node else {
            panic!("todos_element items should be a retain/map pipe");
        };

        let mut locals = Vec::new();
        let mut passed = vec![PassedScope::Bindings(BTreeMap::from([(
            "store".to_string(),
            "store".to_string(),
        )]))];

        let StaticExpression::Pipe {
            from: retain_from,
            to: retain_to,
        } = &from.node
        else {
            panic!("todos_element source should be a retain pipe");
        };
        let binding_path = runtime_object_list_binding_path(
            retain_from,
            &context,
            &locals,
            &passed,
            &mut Vec::new(),
        )
        .expect("retain base binding should inspect");
        let direct_path =
            canonical_expression_path(retain_from, &context, &locals, &passed, &mut Vec::new());
        let mut passed_clone = passed.clone();
        let resolved_retain_from =
            resolve_alias(retain_from, &context, &locals, &mut passed_clone, &mut Vec::new())
                .expect("retain source should resolve");
        let resolved_path = canonical_expression_path(
            resolved_retain_from,
            &context,
            &locals,
            &passed,
            &mut Vec::new(),
        );
        assert_eq!(
            binding_path.as_deref(),
            Some("store.todos"),
            "retain_from={} direct_path={direct_path:?} resolved={} resolved_path={resolved_path:?} object_lists={:?}",
            describe_expression_detailed(retain_from),
            describe_expression_detailed(resolved_retain_from),
            context.object_list_plan.initial_values.keys().collect::<Vec<_>>(),
        );

        let StaticExpression::FunctionCall {
            arguments: retain_arguments,
            ..
        } = &retain_to.node
        else {
            panic!("todos_element source should end in List/retain");
        };
        let retain_condition =
            find_named_argument(retain_arguments, "if").expect("retain should define if");
        let filter =
            runtime_object_list_filter(retain_condition, "item", &context, &locals, &passed)
                .expect("retain filter should inspect");
        assert!(
            matches!(
                filter,
                Some(ObjectListFilter::SelectedCompletedByScalar { ref binding })
                    if binding == "store.selected_filter"
            ),
            "unexpected retain filter: {filter:?}"
        );

        let list_ref = runtime_object_list_ref(from, &context, &locals, &passed, &mut Vec::new())
            .expect("retain source should inspect");
        assert!(
            matches!(
                list_ref,
                Some(RuntimeObjectListRef {
                    ref binding,
                    filter: Some(ObjectListFilter::SelectedCompletedByScalar {
                        binding: ref filter_binding,
                    }),
                }) if binding == "store.todos"
                    && filter_binding == "store.selected_filter"
            ),
            "unexpected runtime object list ref: {list_ref:?}"
        );

        let lowered =
            lower_dynamic_list_items(items, &context, &mut Vec::new(), &mut locals, &mut passed)
                .expect("todos_element items should lower")
                .expect("todos_element items should use a runtime list");

        assert!(matches!(
            lowered.as_slice(),
            [SemanticNode::ObjectList {
                binding,
                filter: Some(ObjectListFilter::SelectedCompletedByScalar {
                    binding: filter_binding,
                }),
                ..
            }] if binding == "store.todos"
                && filter_binding == "store.selected_filter"
        ));
    }

    #[test]
    fn crud_filter_input_linked_text_plan_exposes_change_trigger() {
        let context = crud_test_context();
        let filter_input = context
            .functions
            .get("filter_input")
            .expect("filter_input function should exist")
            .body;
        let StaticExpression::FunctionCall { arguments, .. } = &filter_input.node else {
            panic!("filter_input should lower from Element/text_input");
        };

        let plan = linked_text_input_plan(
            "store.elements.filter_input",
            arguments,
            &context,
            &mut Vec::new(),
            &mut Vec::new(),
            &mut Vec::new(),
        )
        .expect("linked text input plan should inspect")
        .expect("linked text input plan should exist");

        assert!(
            plan.event_updates
                .iter()
                .any(|((trigger_binding, event_name), update)| {
                    trigger_binding == "store.elements.filter_input"
                        && event_name == "change"
                        && matches!(
                            update,
                            TextUpdate::SetFromPayload { binding }
                                if binding == "store.elements.filter_input.text"
                        )
                }),
            "unexpected linked text input plan: {plan:?}"
        );
    }

    #[test]
    fn crud_people_list_row_click_updates_selected_id_from_item_id() {
        let context = crud_test_context();
        let actions = context
            .object_list_plan
            .item_actions
            .get("store.people")
            .expect("store.people should expose runtime item actions");

        assert!(
            actions.iter().any(|action| {
                action.source_binding_suffix == "person_elements.row"
                    && action.kind == boon_scene::UiEventKind::Click
                    && matches!(
                        &action.action,
                        ObjectItemActionKind::UpdateBindings {
                            scalar_updates,
                            ..
                        } if scalar_updates.iter().any(|update| matches!(
                            update,
                            ItemScalarUpdate::SetFromField { binding, field }
                                if binding == "store.selected_id" && field == "id"
                        ))
                    )
            }),
            "expected store.people row click action to set store.selected_id from item.id, got {:?}",
            actions
        );
    }

    #[test]
    fn crud_person_row_is_selected_condition_lowers_to_object_compare_branch() {
        let context = crud_test_context();
        let person_row = context
            .functions
            .get("person_row")
            .expect("person_row function should exist")
            .body;
        let StaticExpression::Block { variables, .. } = &person_row.node else {
            panic!("person_row should lower from a block");
        };
        let is_selected = variables
            .iter()
            .find(|variable| variable.node.name.as_str() == "is_selected")
            .map(|variable| &variable.node.value)
            .expect("person_row block should define is_selected");
        let mut locals = vec![BTreeMap::from([(
            "person".to_string(),
            LocalBinding {
                expr: None,
                object_base: Some("__item__".to_string()),
            },
        )])];
        let mut passed = vec![PassedScope::Bindings(BTreeMap::from([(
            "store".to_string(),
            "store".to_string(),
        )]))];

        let lowered = lower_bool_condition_from_nodes(
            is_selected,
            SemanticNode::text("selected"),
            SemanticNode::text("plain"),
            &context,
            &mut Vec::new(),
            &mut locals,
            &mut passed,
        )
        .expect("is_selected condition should lower");

        let resolved = resolve_alias(is_selected, &context, &locals, &passed, &mut Vec::new())
            .expect("is_selected should resolve");
        let nested = match &resolved.node {
            StaticExpression::Pipe { to, .. } => match &to.node {
                StaticExpression::When { arms } | StaticExpression::While { arms } => {
                    conditional_alias_arm(arms).map(|(_, body)| body)
                }
                _ => None,
            },
            _ => None,
        };
        let nested_lowered = match (&resolved.node, nested) {
            (StaticExpression::Pipe { from, .. }, Some(body)) => {
                let mut nested_locals = locals.clone();
                nested_locals.push(BTreeMap::from([(
                    "selected".to_string(),
                    LocalBinding {
                        expr: Some(from),
                        object_base: infer_argument_object_base(from, &context, &locals, &passed),
                    },
                )]));
                lower_bool_condition_from_nodes(
                    body,
                    SemanticNode::text("selected"),
                    SemanticNode::text("plain"),
                    &context,
                    &mut Vec::new(),
                    &mut nested_locals,
                    &mut passed,
                )
                .expect("nested is_selected body should lower")
            }
            _ => None,
        };
        let (compare_left, compare_right) = match nested {
            Some(StaticSpannedExpression {
                node: StaticExpression::Pipe { from, .. },
                ..
            }) => match &from.node {
                StaticExpression::Comparator(StaticComparator::Equal {
                    operand_a,
                    operand_b,
                })
                | StaticExpression::Comparator(StaticComparator::NotEqual {
                    operand_a,
                    operand_b,
                })
                | StaticExpression::Comparator(StaticComparator::Greater {
                    operand_a,
                    operand_b,
                })
                | StaticExpression::Comparator(StaticComparator::GreaterOrEqual {
                    operand_a,
                    operand_b,
                })
                | StaticExpression::Comparator(StaticComparator::Less {
                    operand_a,
                    operand_b,
                })
                | StaticExpression::Comparator(StaticComparator::LessOrEqual {
                    operand_a,
                    operand_b,
                }) => {
                    let mut nested_locals = locals.clone();
                    if let StaticExpression::Pipe {
                        from: selected_source,
                        ..
                    } = &resolved.node
                    {
                        nested_locals.push(BTreeMap::from([(
                            "selected".to_string(),
                            LocalBinding {
                                expr: Some(selected_source),
                                object_base: infer_argument_object_base(
                                    selected_source,
                                    &context,
                                    &locals,
                                    &passed,
                                ),
                            },
                        )]));
                    }
                    (
                        object_derived_scalar_operand_in_context(
                            operand_a,
                            &context,
                            &nested_locals,
                            &passed,
                            &mut Vec::new(),
                        )
                        .expect("left compare operand should inspect"),
                        object_derived_scalar_operand_in_context(
                            operand_b,
                            &context,
                            &nested_locals,
                            &passed,
                            &mut Vec::new(),
                        )
                        .expect("right compare operand should inspect"),
                    )
                }
                _ => (None, None),
            },
            _ => (None, None),
        };

        assert!(
            lowered
                .as_ref()
                .is_some_and(semantic_tree_contains_object_selected_id_compare),
            "expected is_selected condition to retain a store.selected_id == item.id branch, got {lowered:?}; nested_lowered={nested_lowered:?}; compare_left={compare_left:?}; compare_right={compare_right:?}; expr={}; resolved={}; nested={}",
            describe_expression_detailed(is_selected),
            describe_expression_detailed(resolved),
            nested
                .map(describe_expression_detailed)
                .unwrap_or_else(|| "<none>".to_string())
        );
    }

    #[test]
    fn crud_person_row_label_expression_lowers_to_object_compare_branch() {
        let context = crud_test_context();
        let person_row = context
            .functions
            .get("person_row")
            .expect("person_row function should exist")
            .body;
        let StaticExpression::Block { variables, .. } = &person_row.node else {
            panic!("person_row should lower from a block");
        };
        let label = variables
            .iter()
            .find(|variable| variable.node.name.as_str() == "label")
            .map(|variable| &variable.node.value)
            .expect("person_row block should define label");
        let mut locals = vec![BTreeMap::from([(
            "person".to_string(),
            LocalBinding {
                expr: None,
                object_base: Some("__item__".to_string()),
            },
        )])];
        let mut passed = vec![PassedScope::Bindings(BTreeMap::from([(
            "store".to_string(),
            "store".to_string(),
        )]))];
        let mut scope = BTreeMap::new();
        for variable in variables {
            let object_base =
                infer_argument_object_base(&variable.node.value, &context, &locals, &passed)
                    .or_else(|| active_object_scope(&locals));
            scope.insert(
                variable.node.name.as_str().to_string(),
                LocalBinding {
                    expr: Some(&variable.node.value),
                    object_base,
                },
            );
        }
        locals.push(scope);

        let lowered = lower_ui_node(
            label,
            &context,
            &mut Vec::new(),
            &mut locals,
            &mut passed,
            None,
        )
        .expect("label expression should lower");
        locals.pop();

        assert!(
            semantic_tree_contains_object_selected_id_compare(&lowered),
            "expected label expression to retain a store.selected_id == item.id branch, got {lowered:?}"
        );
    }

    #[test]
    fn crud_people_list_template_compares_selected_id_against_item_id() {
        let program = lower_to_semantic(
            include_str!("../../../playground/frontend/src/examples/crud/crud.bn"),
            None,
            false,
        );

        assert!(
            semantic_tree_contains_object_selected_id_compare(&program.root),
            "expected crud template to retain a dynamic store.selected_id == item.id branch"
        );
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
            "../../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn"
        ))
        .expect("todo_mvc should parse");
        let bindings = top_level_bindings(&expressions);
        let functions = super::top_level_functions(&expressions, None);
        let path_bindings = super::flatten_binding_paths(&bindings);

        let plan = detect_scalar_plan(
            &path_bindings,
            &functions,
            &detect_timer_bindings(&path_bindings).expect("timer bindings should build"),
        )
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
                binding == expected
                    || semantic_tree_contains_object_list_binding(template, expected)
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
            SemanticNode::Element { children, .. } | SemanticNode::Fragment(children) => {
                children.iter().any(|child| {
                    semantic_tree_contains_object_text_field_branch(child, expected_field)
                })
            }
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
            SemanticNode::Element {
                children,
                input_value,
                ..
            } => {
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
            SemanticInputValue::Node(node) => {
                semantic_tree_contains_object_text_binding(node, expected_field)
            }
            SemanticInputValue::ParsedTextBindingBranch { number, nan, .. } => {
                input_value_contains_object_text_binding(number, expected_field)
                    || input_value_contains_object_text_binding(nan, expected_field)
            }
            SemanticInputValue::TextValueBranch { truthy, falsy, .. }
            | SemanticInputValue::TextBindingBranch { truthy, falsy, .. }
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
                        item_actions.iter().map(|action| {
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
            SemanticNode::Element { children, .. } | SemanticNode::Fragment(children) => {
                children.iter().any(|child| {
                    object_list_has_item_action(
                        child,
                        expected_binding,
                        expected_suffix,
                        expected_kind,
                    )
                })
            }
            SemanticNode::BoolBranch { truthy, falsy, .. }
            | SemanticNode::ScalarCompareBranch { truthy, falsy, .. }
            | SemanticNode::ObjectScalarCompareBranch { truthy, falsy, .. }
            | SemanticNode::ObjectBoolFieldBranch { truthy, falsy, .. }
            | SemanticNode::ObjectTextFieldBranch { truthy, falsy, .. }
            | SemanticNode::TextBindingBranch { truthy, falsy, .. }
            | SemanticNode::ListEmptyBranch { truthy, falsy, .. } => {
                object_list_has_item_action(
                    truthy,
                    expected_binding,
                    expected_suffix,
                    expected_kind,
                ) || object_list_has_item_action(
                    falsy,
                    expected_binding,
                    expected_suffix,
                    expected_kind,
                )
            }
            _ => false,
        }
    }

    fn semantic_tree_contains_object_selected_id_compare(node: &SemanticNode) -> bool {
        match node {
            SemanticNode::ObjectScalarCompareBranch {
                left,
                op: IntCompareOp::Equal,
                right,
                truthy,
                falsy,
            } => {
                matches!(
                    (left, right),
                    (
                        ObjectDerivedScalarOperand::Binding(binding),
                        ObjectDerivedScalarOperand::Field(field),
                    ) if binding == "store.selected_id" && field == "id"
                ) || matches!(
                    (left, right),
                    (
                        ObjectDerivedScalarOperand::Field(field),
                        ObjectDerivedScalarOperand::Binding(binding),
                    ) if binding == "store.selected_id" && field == "id"
                ) || semantic_tree_contains_object_selected_id_compare(truthy)
                    || semantic_tree_contains_object_selected_id_compare(falsy)
            }
            SemanticNode::Element { children, .. } | SemanticNode::Fragment(children) => children
                .iter()
                .any(semantic_tree_contains_object_selected_id_compare),
            SemanticNode::BoolBranch { truthy, falsy, .. }
            | SemanticNode::ScalarCompareBranch { truthy, falsy, .. }
            | SemanticNode::ObjectBoolFieldBranch { truthy, falsy, .. }
            | SemanticNode::ObjectTextFieldBranch { truthy, falsy, .. }
            | SemanticNode::TextBindingBranch { truthy, falsy, .. }
            | SemanticNode::ListEmptyBranch { truthy, falsy, .. } => {
                semantic_tree_contains_object_selected_id_compare(truthy)
                    || semantic_tree_contains_object_selected_id_compare(falsy)
            }
            SemanticNode::ObjectList { template, .. } => {
                semantic_tree_contains_object_selected_id_compare(template)
            }
            _ => false,
        }
    }

    fn semantic_tree_contains_cells_editing_compare(node: &SemanticNode) -> bool {
        match node {
            SemanticNode::ObjectScalarCompareBranch {
                left,
                op: IntCompareOp::Equal,
                right,
                truthy,
                falsy,
            } => {
                matches!(
                    (left, right),
                    (
                        ObjectDerivedScalarOperand::Binding(binding),
                        ObjectDerivedScalarOperand::Field(field),
                    ) if (binding == "editing_cell.row" && field == "row")
                        || (binding == "editing_cell.column" && field == "column")
                ) || matches!(
                    (left, right),
                    (
                        ObjectDerivedScalarOperand::Field(field),
                        ObjectDerivedScalarOperand::Binding(binding),
                    ) if (binding == "editing_cell.row" && field == "row")
                        || (binding == "editing_cell.column" && field == "column")
                ) || semantic_tree_contains_cells_editing_compare(truthy)
                    || semantic_tree_contains_cells_editing_compare(falsy)
            }
            SemanticNode::Element { children, .. } | SemanticNode::Fragment(children) => children
                .iter()
                .any(semantic_tree_contains_cells_editing_compare),
            SemanticNode::BoolBranch { truthy, falsy, .. }
            | SemanticNode::ScalarCompareBranch { truthy, falsy, .. }
            | SemanticNode::ObjectBoolFieldBranch { truthy, falsy, .. }
            | SemanticNode::ObjectTextFieldBranch { truthy, falsy, .. }
            | SemanticNode::TextBindingBranch { truthy, falsy, .. }
            | SemanticNode::ListEmptyBranch { truthy, falsy, .. } => {
                semantic_tree_contains_cells_editing_compare(truthy)
                    || semantic_tree_contains_cells_editing_compare(falsy)
            }
            SemanticNode::ObjectList { template, .. } => {
                semantic_tree_contains_cells_editing_compare(template)
            }
            SemanticNode::Keyed { node, .. } => {
                semantic_tree_contains_cells_editing_compare(node)
            }
            _ => false,
        }
    }

    fn semantic_tree_contains_scalar_compare_binding(node: &SemanticNode, expected: &str) -> bool {
        match node {
            SemanticNode::ScalarCompareBranch {
                left,
                right,
                truthy,
                falsy,
                ..
            } => {
                matches!(left, DerivedScalarOperand::Binding(binding) if binding == expected)
                    || matches!(right, DerivedScalarOperand::Binding(binding) if binding == expected)
                    || semantic_tree_contains_scalar_compare_binding(truthy, expected)
                    || semantic_tree_contains_scalar_compare_binding(falsy, expected)
            }
            SemanticNode::Element { children, .. } | SemanticNode::Fragment(children) => children
                .iter()
                .any(|child| semantic_tree_contains_scalar_compare_binding(child, expected)),
            SemanticNode::BoolBranch { truthy, falsy, .. }
            | SemanticNode::ObjectScalarCompareBranch { truthy, falsy, .. }
            | SemanticNode::ObjectBoolFieldBranch { truthy, falsy, .. }
            | SemanticNode::ObjectTextFieldBranch { truthy, falsy, .. }
            | SemanticNode::TextBindingBranch { truthy, falsy, .. }
            | SemanticNode::ListEmptyBranch { truthy, falsy, .. } => {
                semantic_tree_contains_scalar_compare_binding(truthy, expected)
                    || semantic_tree_contains_scalar_compare_binding(falsy, expected)
            }
            SemanticNode::ObjectList { template, .. } => {
                semantic_tree_contains_scalar_compare_binding(template, expected)
            }
            SemanticNode::Keyed { node, .. } => {
                semantic_tree_contains_scalar_compare_binding(node, expected)
            }
            _ => false,
        }
    }

    fn semantic_tree_contains_object_list_count_branch(
        node: &SemanticNode,
        expected: &str,
    ) -> bool {
        match node {
            SemanticNode::ScalarCompareBranch {
                left,
                right,
                truthy,
                falsy,
                ..
            } => {
                matches!(left, DerivedScalarOperand::ObjectListCount { binding, .. } if binding == expected)
                    || matches!(right, DerivedScalarOperand::ObjectListCount { binding, .. } if binding == expected)
                    || semantic_tree_contains_object_list_count_branch(truthy, expected)
                    || semantic_tree_contains_object_list_count_branch(falsy, expected)
            }
            SemanticNode::Element { children, .. } | SemanticNode::Fragment(children) => children
                .iter()
                .any(|child| semantic_tree_contains_object_list_count_branch(child, expected)),
            SemanticNode::BoolBranch { truthy, falsy, .. }
            | SemanticNode::ObjectScalarCompareBranch { truthy, falsy, .. }
            | SemanticNode::ObjectBoolFieldBranch { truthy, falsy, .. }
            | SemanticNode::ObjectTextFieldBranch { truthy, falsy, .. }
            | SemanticNode::TextBindingBranch { truthy, falsy, .. }
            | SemanticNode::ListEmptyBranch { truthy, falsy, .. } => {
                semantic_tree_contains_object_list_count_branch(truthy, expected)
                    || semantic_tree_contains_object_list_count_branch(falsy, expected)
            }
            SemanticNode::ObjectList { template, .. } => {
                semantic_tree_contains_object_list_count_branch(template, expected)
            }
            SemanticNode::Keyed { node, .. } => {
                semantic_tree_contains_object_list_count_branch(node, expected)
            }
            _ => false,
        }
    }

    fn semantic_tree_contains_object_scalar_compare_binding(
        node: &SemanticNode,
        expected: &str,
    ) -> bool {
        match node {
            SemanticNode::ObjectScalarCompareBranch {
                left,
                right,
                truthy,
                falsy,
                ..
            } => {
                matches!(left, ObjectDerivedScalarOperand::Binding(binding) if binding == expected)
                    || matches!(right, ObjectDerivedScalarOperand::Binding(binding) if binding == expected)
                    || semantic_tree_contains_object_scalar_compare_binding(truthy, expected)
                    || semantic_tree_contains_object_scalar_compare_binding(falsy, expected)
            }
            SemanticNode::Element { children, .. } | SemanticNode::Fragment(children) => children
                .iter()
                .any(|child| semantic_tree_contains_object_scalar_compare_binding(child, expected)),
            SemanticNode::BoolBranch { truthy, falsy, .. }
            | SemanticNode::ScalarCompareBranch { truthy, falsy, .. }
            | SemanticNode::ObjectBoolFieldBranch { truthy, falsy, .. }
            | SemanticNode::ObjectTextFieldBranch { truthy, falsy, .. }
            | SemanticNode::TextBindingBranch { truthy, falsy, .. }
            | SemanticNode::ListEmptyBranch { truthy, falsy, .. } => {
                semantic_tree_contains_object_scalar_compare_binding(truthy, expected)
                    || semantic_tree_contains_object_scalar_compare_binding(falsy, expected)
            }
            SemanticNode::ObjectList { template, .. } => {
                semantic_tree_contains_object_scalar_compare_binding(template, expected)
            }
            SemanticNode::Keyed { node, .. } => {
                semantic_tree_contains_object_scalar_compare_binding(node, expected)
            }
            _ => false,
        }
    }

    #[test]
    fn lower_to_semantic_todo_mvc_footer_uses_scalar_compare_for_completed_count() {
        let program = lower_to_semantic(
            include_str!("../../../playground/frontend/src/examples/todo_mvc/todo_mvc.bn"),
            None,
            false,
        );
        let has_scalar = semantic_tree_contains_scalar_compare_binding(
            &program.root,
            "store.completed_todos_count",
        );
        let has_object = semantic_tree_contains_object_scalar_compare_binding(
            &program.root,
            "store.completed_todos_count",
        );

        assert!(
            has_scalar,
            "expected todo_mvc root to contain a scalar compare branch over store.completed_todos_count (object={has_object})",
        );
        assert!(
            !has_object,
            "todo_mvc should not lower store.completed_todos_count footer visibility as an object scalar compare branch",
        );
    }
}
