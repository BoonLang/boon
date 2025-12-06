use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::HashMap;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;

use chumsky::Parser as ChumskyParser;
use chumsky::input::{Input as ChumskyInput, Stream as ChumskyStream};
use ulid::Ulid;
use zoon::futures_util::stream;
use zoon::{Stream, StreamExt, println, eprintln};

/// Yields control to the executor, allowing other tasks to run.
/// This is a simple implementation that returns Pending once and schedules a wake.
async fn yield_once() {
    use std::task::Poll;
    let mut yielded = false;
    std::future::poll_fn(|cx| {
        if yielded {
            Poll::Ready(())
        } else {
            yielded = true;
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    }).await
}

use super::super::super::parser::{
    PersistenceId, SourceCode, Span, static_expression, lexer, parser, resolve_references, Token, Spanned,
};
use super::api;
use super::engine::*;

/// Registry for user-defined functions using static expressions.
/// No lifetime parameter - can be stored and used anywhere.
#[derive(Clone, Default)]
pub struct StaticFunctionRegistry {
    pub functions: Rc<RefCell<HashMap<String, StaticFunctionDefinition>>>,
}

/// A user-defined function definition using static expressions.
#[derive(Clone)]
pub struct StaticFunctionDefinition {
    pub parameters: Vec<String>,
    pub body: static_expression::Spanned<static_expression::Expression>,
}

/// Cached module data - contains functions and variables from a parsed module file.
#[derive(Clone)]
pub struct ModuleData {
    /// Functions defined in this module (name -> definition)
    pub functions: HashMap<String, StaticFunctionDefinition>,
    /// Variables defined in this module (name -> value expression)
    pub variables: HashMap<String, static_expression::Spanned<static_expression::Expression>>,
}

/// Module loader with caching for loading and parsing Boon modules.
/// Resolves module paths like "Theme" to file paths and caches parsed modules.
#[derive(Clone, Default)]
pub struct ModuleLoader {
    /// Cache of loaded modules (module_path -> ModuleData)
    cache: Rc<RefCell<HashMap<String, ModuleData>>>,
    /// Base directory for module resolution (e.g., the directory containing RUN.bn)
    base_dir: Rc<RefCell<String>>,
}

impl ModuleLoader {
    pub fn new(base_dir: impl Into<String>) -> Self {
        Self {
            cache: Rc::new(RefCell::new(HashMap::new())),
            base_dir: Rc::new(RefCell::new(base_dir.into())),
        }
    }

    /// Set the base directory for module resolution
    pub fn set_base_dir(&self, dir: impl Into<String>) {
        *self.base_dir.borrow_mut() = dir.into();
    }

    /// Get the base directory
    pub fn base_dir(&self) -> String {
        self.base_dir.borrow().clone()
    }

    /// Load a module by name (e.g., "Theme", "Professional", "Assets")
    /// Tries multiple resolution paths:
    /// 1. {base_dir}/{module_name}.bn
    /// 2. {base_dir}/{module_name}/{module_name}.bn
    /// 3. {base_dir}/Generated/{module_name}.bn (for generated files)
    pub fn load_module(
        &self,
        module_name: &str,
        virtual_fs: &VirtualFilesystem,
        current_dir: Option<&str>,
    ) -> Option<ModuleData> {
        // Check cache first
        if let Some(cached) = self.cache.borrow().get(module_name) {
            return Some(cached.clone());
        }

        let base_dir_binding = self.base_dir.borrow();
        let base = current_dir.unwrap_or(&base_dir_binding);

        // Helper to create path, avoiding leading slash when base is empty
        let make_path = |base: &str, rest: &str| {
            if base.is_empty() {
                rest.to_string()
            } else {
                format!("{}/{}", base, rest)
            }
        };

        // Try different resolution paths
        let paths_to_try = vec![
            make_path(base, &format!("{}.bn", module_name)),
            make_path(base, &format!("{}/{}.bn", module_name, module_name)),
            make_path(base, &format!("Generated/{}.bn", module_name)),
            // Also try from the module loader's base directory if current_dir is different
            make_path(&base_dir_binding, &format!("{}.bn", module_name)),
            make_path(&base_dir_binding, &format!("{}/{}.bn", module_name, module_name)),
            make_path(&base_dir_binding, &format!("Generated/{}.bn", module_name)),
        ];

        for path in paths_to_try {
            if let Some(source_code) = virtual_fs.read_text(&path) {
                println!("[ModuleLoader] Loading module '{}' from '{}'", module_name, path);
                if let Some(module_data) = self.parse_module(&path, &source_code) {
                    // Cache the module
                    self.cache.borrow_mut().insert(module_name.to_string(), module_data.clone());
                    return Some(module_data);
                }
            }
        }

        eprintln!("[ModuleLoader] Could not find module '{}' (tried from base '{}')", module_name, base);
        None
    }

    /// Parse module source code into ModuleData
    fn parse_module(&self, filename: &str, source_code: &str) -> Option<ModuleData> {
        // Lexer
        let (tokens, errors) = lexer().parse(source_code).into_output_errors();
        if !errors.is_empty() {
            eprintln!("[ModuleLoader] Lex errors in '{}': {:?}", filename, errors.len());
            return None;
        }
        let mut tokens = tokens?;
        tokens.retain(|spanned_token| !matches!(spanned_token.node, Token::Comment(_)));

        // Parser
        let (ast, errors) = parser()
            .parse(ChumskyStream::from_iter(tokens).map(
                Span::splat(source_code.len()),
                |Spanned { node, span, persistence: _ }| (node, span),
            ))
            .into_output_errors();
        if !errors.is_empty() {
            eprintln!("[ModuleLoader] Parse errors in '{}': {:?}", filename, errors.len());
            return None;
        }
        let ast = ast?;

        // Reference resolution
        let ast = match resolve_references(ast) {
            Ok(ast) => ast,
            Err(errors) => {
                eprintln!("[ModuleLoader] Reference errors in '{}': {:?}", filename, errors.len());
                return None;
            }
        };

        // Convert to static expressions
        let source_code_arc = SourceCode::new(source_code.to_string());
        let static_ast = static_expression::convert_expressions(source_code_arc, ast);

        // Extract functions and variables
        let mut functions = HashMap::new();
        let mut variables = HashMap::new();

        for expr in static_ast {
            match expr.node.clone() {
                static_expression::Expression::Variable(variable) => {
                    let name = variable.name.to_string();
                    let value_expr = variable.value;
                    variables.insert(name, value_expr);
                }
                static_expression::Expression::Function { name, parameters, body } => {
                    functions.insert(
                        name.to_string(),
                        StaticFunctionDefinition {
                            parameters: parameters.into_iter().map(|p| p.node.to_string()).collect(),
                            body: *body,
                        },
                    );
                }
                _ => {}
            }
        }

        Some(ModuleData { functions, variables })
    }

    /// Get a function from a module
    pub fn get_function(
        &self,
        module_name: &str,
        function_name: &str,
        virtual_fs: &VirtualFilesystem,
        current_dir: Option<&str>,
    ) -> Option<StaticFunctionDefinition> {
        let module = self.load_module(module_name, virtual_fs, current_dir)?;
        module.functions.get(function_name).cloned()
    }

    /// Get a variable from a module
    pub fn get_variable(
        &self,
        module_name: &str,
        variable_name: &str,
        virtual_fs: &VirtualFilesystem,
        current_dir: Option<&str>,
    ) -> Option<static_expression::Spanned<static_expression::Expression>> {
        let module = self.load_module(module_name, virtual_fs, current_dir)?;
        module.variables.get(variable_name).cloned()
    }
}

/// Main evaluation function - takes static expressions (owned, 'static, no lifetimes).
pub fn evaluate(
    source_code: SourceCode,
    expressions: Vec<static_expression::Spanned<static_expression::Expression>>,
    states_local_storage_key: impl Into<Cow<'static, str>>,
    virtual_fs: VirtualFilesystem,
) -> Result<(Arc<Object>, ConstructContext), String> {
    let function_registry = StaticFunctionRegistry::default();
    let module_loader = ModuleLoader::default();
    let (obj, ctx, _, _) = evaluate_with_registry(
        source_code,
        expressions,
        states_local_storage_key,
        virtual_fs,
        function_registry,
        module_loader,
    )?;
    Ok((obj, ctx))
}

/// Evaluation function that accepts and returns a function registry and module loader.
/// This enables sharing function definitions across multiple files.
pub fn evaluate_with_registry(
    source_code: SourceCode,
    expressions: Vec<static_expression::Spanned<static_expression::Expression>>,
    states_local_storage_key: impl Into<Cow<'static, str>>,
    virtual_fs: VirtualFilesystem,
    function_registry: StaticFunctionRegistry,
    module_loader: ModuleLoader,
) -> Result<(Arc<Object>, ConstructContext, StaticFunctionRegistry, ModuleLoader), String> {
    let construct_context = ConstructContext {
        construct_storage: Arc::new(ConstructStorage::new(states_local_storage_key)),
        virtual_fs,
    };
    let actor_context = ActorContext::default();
    let reference_connector = Arc::new(ReferenceConnector::new());
    let link_connector = Arc::new(LinkConnector::new());

    // First pass: collect function definitions and variables
    let mut variables = Vec::new();
    for expr in expressions {
        let static_expression::Spanned {
            span,
            node: expression,
            persistence,
        } = expr;
        match expression {
            static_expression::Expression::Variable(variable) => {
                variables.push(static_expression::Spanned {
                    span,
                    node: *variable,
                    persistence,
                });
            }
            static_expression::Expression::Function {
                name,
                parameters,
                body,
            } => {
                // Store function definition in registry
                function_registry.functions.borrow_mut().insert(
                    name.to_string(),
                    StaticFunctionDefinition {
                        parameters: parameters.into_iter().map(|p| p.node.to_string()).collect(),
                        body: *body,
                    },
                );
            }
            _ => {
                return Err(format!(
                    "Only variables or functions expected at top level (span: {span})"
                ));
            }
        }
    }

    // Second pass: evaluate variables
    let evaluated_variables: Result<Vec<_>, _> = variables
        .into_iter()
        .map(|variable| {
            static_spanned_variable_into_variable(
                variable,
                construct_context.clone(),
                actor_context.clone(),
                reference_connector.clone(),
                link_connector.clone(),
                function_registry.clone(),
                module_loader.clone(),
                source_code.clone(),
            )
        })
        .collect();

    let root_object = Object::new_arc(
        ConstructInfo::new("root", None, "root"),
        construct_context.clone(),
        evaluated_variables?,
    );
    Ok((root_object, construct_context, function_registry, module_loader))
}

/// Evaluates a static variable into a Variable.
fn static_spanned_variable_into_variable(
    variable: static_expression::Spanned<static_expression::Variable>,
    construct_context: ConstructContext,
    actor_context: ActorContext,
    reference_connector: Arc<ReferenceConnector>,
    link_connector: Arc<LinkConnector>,
    function_registry: StaticFunctionRegistry,
    module_loader: ModuleLoader,
    source_code: SourceCode,
) -> Result<Arc<Variable>, String> {
    let static_expression::Spanned {
        span,
        node: variable,
        persistence,
    } = variable;
    let static_expression::Variable {
        name,
        value,
        is_referenced,
    } = variable;

    let persistence_id = persistence.clone().ok_or("Failed to get Persistence")?.id;
    let name_string = name.to_string();

    let construct_info = ConstructInfo::new(
        format!("PersistenceId: {persistence_id}"),
        persistence,
        format!("{span}; {name_string}"),
    );

    let is_link = matches!(&value.node, static_expression::Expression::Link);

    let variable = if is_link {
        Variable::new_link_arc(construct_info, construct_context, name_string, actor_context, Some(persistence_id))
    } else {
        Variable::new_arc(
            construct_info,
            construct_context.clone(),
            name_string,
            static_spanned_expression_into_value_actor(
                value,
                construct_context,
                actor_context,
                reference_connector.clone(),
                link_connector.clone(),
                function_registry,
                module_loader,
                source_code,
            )?,
            Some(persistence_id),
        )
    };
    if is_referenced {
        reference_connector.register_referenceable(span, variable.value_actor());
    }
    // Register LINK variable senders with LinkConnector
    if is_link {
        if let Some(sender) = variable.link_value_sender() {
            link_connector.register_link(span, sender);
        }
    }
    Ok(variable)
}

/// Evaluates a static expression, returning a ValueActor.
///
/// This is used by ListBindingFunction to evaluate transform expressions
/// for each list item. The binding variable is passed via `actor_context.parameters`.
///
/// Note: User-defined function calls inside the expression will not work
/// (the function registry is empty). Built-in functions and operators work fine.
pub fn evaluate_static_expression(
    static_expr: &static_expression::Spanned<static_expression::Expression>,
    construct_context: ConstructContext,
    actor_context: ActorContext,
    reference_connector: Arc<ReferenceConnector>,
    link_connector: Arc<LinkConnector>,
    source_code: SourceCode,
) -> Result<Arc<ValueActor>, String> {
    static_spanned_expression_into_value_actor(
        static_expr.clone(),
        construct_context,
        actor_context,
        reference_connector,
        link_connector,
        StaticFunctionRegistry::default(),
        ModuleLoader::default(),
        source_code,
    )
}

/// Evaluates a static expression directly (no to_borrowed conversion).
/// This is the core static evaluator used for List binding functions.
fn static_spanned_expression_into_value_actor(
    expression: static_expression::Spanned<static_expression::Expression>,
    construct_context: ConstructContext,
    actor_context: ActorContext,
    reference_connector: Arc<ReferenceConnector>,
    link_connector: Arc<LinkConnector>,
    function_registry: StaticFunctionRegistry,
    module_loader: ModuleLoader,
    source_code: SourceCode,
) -> Result<Arc<ValueActor>, String> {
    let static_expression::Spanned {
        span,
        node: expression,
        persistence,
    } = expression;

    let persistence_info = persistence.clone().ok_or("Failed to get Persistence")?;
    let persistence_id = persistence_info.id;
    let idempotency_key = persistence_id;

    // NOTE: Actor reuse is disabled because it creates broken subscription graphs.
    // Reused actors keep OLD subscriptions to OLD actors, which fail when other
    // parts of the graph are recreated. The proper solution is STATE persistence
    // (saving/restoring values to localStorage), not actor instance reuse.
    //
    // TODO: Implement proper state persistence for stateful constructs like:
    // - LATEST with state (the accumulated value)
    // - Math/sum (the running total)
    // - User-defined stateful functions
    //
    // if persistence_info.status == PersistenceStatus::Unchanged {
    //     if let Some(existing_actor) = construct_context.previous_actors.get_actor(persistence_id) {
    //         return Ok(existing_actor);
    //     }
    // }

    let actor = match expression {
        static_expression::Expression::Variable(_) => {
            return Err("Failed to evaluate the variable in this context.".to_string());
        }
        static_expression::Expression::Literal(literal) => match literal {
            static_expression::Literal::Number(number) => Number::new_arc_value_actor(
                ConstructInfo::new(
                    format!("PersistenceId: {persistence_id}"),
                    persistence,
                    format!("{span}; Number {number}"),
                ),
                construct_context,
                idempotency_key,
                actor_context,
                number,
            ),
            static_expression::Literal::Tag(tag) => {
                let tag = tag.to_string();
                Tag::new_arc_value_actor(
                    ConstructInfo::new(
                        format!("PersistenceId: {persistence_id}"),
                        persistence,
                        format!("{span}; Tag {tag}"),
                    ),
                    construct_context,
                    idempotency_key,
                    actor_context,
                    tag,
                )
            }
        },
        static_expression::Expression::List { items } => {
            let evaluated_items: Result<Vec<_>, _> = items
                .into_iter()
                .map(|item| {
                    static_spanned_expression_into_value_actor(
                        item,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector.clone(),
                        link_connector.clone(),
                        function_registry.clone(),
                        module_loader.clone(),
                        source_code.clone(),
                    )
                })
                .collect();
            List::new_arc_value_actor_with_persistence(
                ConstructInfo::new(
                    format!("PersistenceId: {persistence_id}"),
                    persistence,
                    format!("{span}; LIST {{..}}"),
                ),
                construct_context,
                idempotency_key,
                actor_context,
                evaluated_items?,
            )
        }
        static_expression::Expression::Alias(alias) => {
            type BoxedFuture = Pin<Box<dyn std::future::Future<Output = Arc<ValueActor>>>>;

            let root_value_actor: BoxedFuture = match &alias {
                static_expression::Alias::WithPassed { extra_parts: _ } => {
                    match &actor_context.passed {
                        Some(passed) => {
                            let passed = passed.clone();
                            Box::pin(async move { passed })
                        }
                        None => {
                            return Err("PASSED is not available in this context".to_string());
                        }
                    }
                }
                static_expression::Alias::WithoutPassed { parts, referenced_span } => {
                    let first_part = parts.first().map(|s| s.to_string()).unwrap_or_default();
                    if let Some(param_actor) = actor_context.parameters.get(&first_part) {
                        let param_actor = param_actor.clone();
                        Box::pin(async move { param_actor })
                    } else if let Some(ref_span) = referenced_span {
                        Box::pin(reference_connector.referenceable(*ref_span))
                    } else if parts.len() >= 2 {
                        // Try module variable access: e.g., Assets/icon.checkbox_active
                        // parts[0] = module name (Assets), parts[1] = variable name (icon)
                        let module_name = &parts[0];
                        let var_name = parts[1].to_string();

                        if let Some(module_data) = module_loader.load_module(module_name, &construct_context.virtual_fs, None) {
                            if let Some(var_expr) = module_data.variables.get(&var_name).cloned() {
                                println!("[ModuleLoader] Found variable '{}' in module '{}'", var_name, module_name);

                                // Evaluate the module's variable expression
                                let var_actor = static_spanned_expression_into_value_actor(
                                    var_expr,
                                    construct_context.clone(),
                                    actor_context.clone(),
                                    reference_connector.clone(),
                                    link_connector.clone(),
                                    function_registry.clone(),
                                    module_loader.clone(),
                                    source_code.clone(),
                                )?;
                                Box::pin(async move { var_actor })
                            } else {
                                return Err(format!("Variable '{}' not found in module '{}'", var_name, module_name));
                            }
                        } else {
                            return Err(format!("Module '{}' not found for variable access", module_name));
                        }
                    } else {
                        return Err(format!("Failed to get aliased variable '{}'", first_part));
                    }
                }
            };

            VariableOrArgumentReference::new_arc_value_actor(
                ConstructInfo::new(
                    format!("PersistenceId: {persistence_id}"),
                    persistence,
                    format!("{span}; (alias)"),
                ),
                construct_context,
                actor_context,
                alias,
                root_value_actor,
            )
        }
        static_expression::Expression::ArithmeticOperator(op) => {
            let construct_info = ConstructInfo::new(
                format!("PersistenceId: {persistence_id}"),
                persistence,
                format!("{span}; ArithmeticOperator"),
            );
            match op {
                static_expression::ArithmeticOperator::Add { operand_a, operand_b } => {
                    let a = static_spanned_expression_into_value_actor(
                        *operand_a,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector.clone(),
                        link_connector.clone(),
                        function_registry.clone(),
                        module_loader.clone(),
                        source_code.clone(),
                    )?;
                    let b = static_spanned_expression_into_value_actor(
                        *operand_b,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector,
                        link_connector,
                        function_registry,
                        module_loader,
                        source_code,
                    )?;
                    ArithmeticCombinator::new_add(
                        construct_info,
                        construct_context,
                        actor_context,
                        a,
                        b,
                    )
                }
                static_expression::ArithmeticOperator::Subtract { operand_a, operand_b } => {
                    let a = static_spanned_expression_into_value_actor(
                        *operand_a,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector.clone(),
                        link_connector.clone(),
                        function_registry.clone(),
                        module_loader.clone(),
                        source_code.clone(),
                    )?;
                    let b = static_spanned_expression_into_value_actor(
                        *operand_b,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector,
                        link_connector,
                        function_registry,
                        module_loader,
                        source_code,
                    )?;
                    ArithmeticCombinator::new_subtract(
                        construct_info,
                        construct_context,
                        actor_context,
                        a,
                        b,
                    )
                }
                static_expression::ArithmeticOperator::Multiply { operand_a, operand_b } => {
                    let a = static_spanned_expression_into_value_actor(
                        *operand_a,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector.clone(),
                        link_connector.clone(),
                        function_registry.clone(),
                        module_loader.clone(),
                        source_code.clone(),
                    )?;
                    let b = static_spanned_expression_into_value_actor(
                        *operand_b,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector,
                        link_connector,
                        function_registry,
                        module_loader,
                        source_code,
                    )?;
                    ArithmeticCombinator::new_multiply(
                        construct_info,
                        construct_context,
                        actor_context,
                        a,
                        b,
                    )
                }
                static_expression::ArithmeticOperator::Divide { operand_a, operand_b } => {
                    let a = static_spanned_expression_into_value_actor(
                        *operand_a,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector.clone(),
                        link_connector.clone(),
                        function_registry.clone(),
                        module_loader.clone(),
                        source_code.clone(),
                    )?;
                    let b = static_spanned_expression_into_value_actor(
                        *operand_b,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector,
                        link_connector,
                        function_registry,
                        module_loader,
                        source_code,
                    )?;
                    ArithmeticCombinator::new_divide(
                        construct_info,
                        construct_context,
                        actor_context,
                        a,
                        b,
                    )
                }
                static_expression::ArithmeticOperator::Negate { operand } => {
                    let a = static_spanned_expression_into_value_actor(
                        *operand,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector,
                        link_connector,
                        function_registry,
                        module_loader,
                        source_code,
                    )?;
                    let neg_one = Number::new_arc_value_actor(
                        ConstructInfo::new("neg_one", None, "-1 constant"),
                        construct_context.clone(),
                        idempotency_key,
                        actor_context.clone(),
                        -1.0,
                    );
                    ArithmeticCombinator::new_multiply(
                        construct_info,
                        construct_context,
                        actor_context,
                        neg_one,
                        a,
                    )
                }
            }
        }
        static_expression::Expression::Comparator(cmp) => {
            let construct_info = ConstructInfo::new(
                format!("PersistenceId: {persistence_id}"),
                persistence,
                format!("{span}; Comparator"),
            );
            match cmp {
                static_expression::Comparator::Equal { operand_a, operand_b } => {
                    let a = static_spanned_expression_into_value_actor(
                        *operand_a,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector.clone(),
                        link_connector.clone(),
                        function_registry.clone(),
                        module_loader.clone(),
                        source_code.clone(),
                    )?;
                    let b = static_spanned_expression_into_value_actor(
                        *operand_b,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector,
                        link_connector,
                        function_registry,
                        module_loader,
                        source_code,
                    )?;
                    ComparatorCombinator::new_equal(
                        construct_info,
                        construct_context,
                        actor_context,
                        a,
                        b,
                    )
                }
                static_expression::Comparator::NotEqual { operand_a, operand_b } => {
                    let a = static_spanned_expression_into_value_actor(
                        *operand_a,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector.clone(),
                        link_connector.clone(),
                        function_registry.clone(),
                        module_loader.clone(),
                        source_code.clone(),
                    )?;
                    let b = static_spanned_expression_into_value_actor(
                        *operand_b,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector,
                        link_connector,
                        function_registry,
                        module_loader,
                        source_code,
                    )?;
                    ComparatorCombinator::new_not_equal(
                        construct_info,
                        construct_context,
                        actor_context,
                        a,
                        b,
                    )
                }
                static_expression::Comparator::Greater { operand_a, operand_b } => {
                    let a = static_spanned_expression_into_value_actor(
                        *operand_a,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector.clone(),
                        link_connector.clone(),
                        function_registry.clone(),
                        module_loader.clone(),
                        source_code.clone(),
                    )?;
                    let b = static_spanned_expression_into_value_actor(
                        *operand_b,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector,
                        link_connector,
                        function_registry,
                        module_loader,
                        source_code,
                    )?;
                    ComparatorCombinator::new_greater(
                        construct_info,
                        construct_context,
                        actor_context,
                        a,
                        b,
                    )
                }
                static_expression::Comparator::GreaterOrEqual { operand_a, operand_b } => {
                    let a = static_spanned_expression_into_value_actor(
                        *operand_a,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector.clone(),
                        link_connector.clone(),
                        function_registry.clone(),
                        module_loader.clone(),
                        source_code.clone(),
                    )?;
                    let b = static_spanned_expression_into_value_actor(
                        *operand_b,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector,
                        link_connector,
                        function_registry,
                        module_loader,
                        source_code,
                    )?;
                    ComparatorCombinator::new_greater_or_equal(
                        construct_info,
                        construct_context,
                        actor_context,
                        a,
                        b,
                    )
                }
                static_expression::Comparator::Less { operand_a, operand_b } => {
                    let a = static_spanned_expression_into_value_actor(
                        *operand_a,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector.clone(),
                        link_connector.clone(),
                        function_registry.clone(),
                        module_loader.clone(),
                        source_code.clone(),
                    )?;
                    let b = static_spanned_expression_into_value_actor(
                        *operand_b,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector,
                        link_connector,
                        function_registry,
                        module_loader,
                        source_code,
                    )?;
                    ComparatorCombinator::new_less(
                        construct_info,
                        construct_context,
                        actor_context,
                        a,
                        b,
                    )
                }
                static_expression::Comparator::LessOrEqual { operand_a, operand_b } => {
                    let a = static_spanned_expression_into_value_actor(
                        *operand_a,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector.clone(),
                        link_connector.clone(),
                        function_registry.clone(),
                        module_loader.clone(),
                        source_code.clone(),
                    )?;
                    let b = static_spanned_expression_into_value_actor(
                        *operand_b,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector,
                        link_connector,
                        function_registry,
                        module_loader,
                        source_code,
                    )?;
                    ComparatorCombinator::new_less_or_equal(
                        construct_info,
                        construct_context,
                        actor_context,
                        a,
                        b,
                    )
                }
            }
        }
        static_expression::Expression::FunctionCall { path, arguments } => {
            // Handle built-in function calls
            let path_strs: Vec<&str> = path.iter().map(|s| &**s).collect();

            // Special handling for List binding functions (map, retain, every, any, sort_by)
            // These need the unevaluated expression to evaluate per-item with bindings
            match path_strs.as_slice() {
                ["List", "map"] | ["List", "retain"] | ["List", "every"] | ["List", "any"] | ["List", "sort_by"] => {
                    let operation = match path_strs[1] {
                        "map" => ListBindingOperation::Map,
                        "retain" => ListBindingOperation::Retain,
                        "every" => ListBindingOperation::Every,
                        "any" => ListBindingOperation::Any,
                        "sort_by" => ListBindingOperation::SortBy,
                        _ => unreachable!(),
                    };

                    // For List binding functions:
                    // - First arg: binding name (e.g., "old", "item"), value is the list (passed)
                    // - Second arg: transform/predicate expression (e.g., "new: expr", "if: expr")
                    if arguments.len() < 2 {
                        return Err(format!("List/{} requires 2 arguments", path_strs[1]));
                    }

                    // Get binding name from first argument
                    let binding_name = arguments[0].node.name.clone();

                    // Get the list - either from first argument's value or from PASSED
                    let list_actor = if let Some(ref list_value) = arguments[0].node.value {
                        static_spanned_expression_into_value_actor(
                            list_value.clone(),
                            construct_context.clone(),
                            actor_context.clone(),
                            reference_connector.clone(),
                            link_connector.clone(),
                            function_registry.clone(),
                            module_loader.clone(),
                            source_code.clone(),
                        )?
                    } else if let Some(ref piped) = actor_context.piped {
                        piped.clone()
                    } else {
                        return Err(format!("List/{} requires a list argument", path_strs[1]));
                    };

                    // Get transform/predicate expression from second argument (NOT evaluated)
                    let transform_expr = arguments[1].node.value.clone()
                        .ok_or_else(|| format!("List/{} requires a transform expression", path_strs[1]))?;

                    let config = ListBindingConfig {
                        binding_name,
                        transform_expr,
                        operation,
                        reference_connector: reference_connector.clone(),
                        link_connector: link_connector.clone(),
                        source_code: source_code.clone(),
                    };

                    ListBindingFunction::new_arc_value_actor(
                        ConstructInfo::new(
                            format!("PersistenceId: {persistence_id}"),
                            persistence,
                            format!("{span}; List/{}(..)", path_strs[1]),
                        ),
                        construct_context,
                        actor_context,
                        list_actor,
                        config,
                    )
                }
                _ => {
                    // Check for user-defined function first (single-element path like "new_todo")
                    if path.len() == 1 {
                        let func_name = path[0].as_str();
                        let maybe_user_func = function_registry.functions.borrow().get(func_name).cloned();

                        if let Some(user_func) = maybe_user_func {
                            // User-defined function call
                            // Evaluate arguments and bind to parameters
                            let mut param_bindings: HashMap<String, Arc<ValueActor>> = HashMap::new();
                            let mut passed_context: Option<Arc<ValueActor>> = actor_context.passed.clone();

                            // If there's a piped value, bind it to the first parameter
                            if let Some(piped) = &actor_context.piped {
                                if let Some(first_param) = user_func.parameters.first() {
                                    param_bindings.insert(first_param.clone(), piped.clone());
                                }
                            }

                            // Process named arguments
                            for arg in &arguments {
                                // Check for PASS: argument
                                if arg.node.name.as_str() == "PASS" {
                                    if let Some(value) = &arg.node.value {
                                        let pass_actor = static_spanned_expression_into_value_actor(
                                            value.clone(),
                                            construct_context.clone(),
                                            actor_context.clone(),
                                            reference_connector.clone(),
                                            link_connector.clone(),
                                            function_registry.clone(),
                                            module_loader.clone(),
                                            source_code.clone(),
                                        )?;
                                        passed_context = Some(pass_actor);
                                    }
                                    continue;
                                }

                                // Bind named argument to parameter
                                let param_name = arg.node.name.to_string();
                                if let Some(value) = &arg.node.value {
                                    let actor = static_spanned_expression_into_value_actor(
                                        value.clone(),
                                        construct_context.clone(),
                                        actor_context.clone(),
                                        reference_connector.clone(),
                                        link_connector.clone(),
                                        function_registry.clone(),
                                        module_loader.clone(),
                                        source_code.clone(),
                                    )?;
                                    param_bindings.insert(param_name, actor);
                                }
                            }

                            // Create actor context with parameter bindings for the function body
                            let func_actor_context = ActorContext {
                                output_valve_signal: actor_context.output_valve_signal.clone(),
                                piped: None, // Clear piped - it was bound to first param
                                passed: passed_context,
                                parameters: param_bindings,
                                sequential_processing: actor_context.sequential_processing,
                                backpressure_permit: actor_context.backpressure_permit.clone(),
                            };

                            // Evaluate the function body with the new context
                            return static_spanned_expression_into_value_actor(
                                user_func.body,
                                construct_context,
                                func_actor_context,
                                reference_connector,
                                link_connector,
                                function_registry,
                                module_loader,
                                source_code,
                            );
                        }
                    }

                    // Check for module function call (path.len() >= 2, e.g., Theme/material)
                    // Built-in modules: Math, Text, List, Bool, Logic, Storage, Time, Object, Browser, Ui, Css, Selector
                    let builtin_modules = ["Math", "Text", "List", "Bool", "Logic", "Storage", "Time", "Object", "Browser", "Ui", "Css", "Selector", "Color", "Spring", "Page", "Attr", "Router", "Ulid", "Document", "Element", "Timer", "Log", "Build", "Scene", "Theme", "File", "Directory"];
                    if path.len() >= 2 && !builtin_modules.contains(&path[0].as_str()) {
                        let module_name = &path[0];
                        let func_name = &path[1];

                        // Try to load module and get the function
                        if let Some(module_data) = module_loader.load_module(module_name, &construct_context.virtual_fs, None) {
                            if let Some(user_func) = module_data.functions.get(func_name.as_str()) {
                                println!("[ModuleLoader] Found function '{}' in module '{}'", func_name, module_name);

                                // User-defined function from module - evaluate arguments and bind to parameters
                                let mut param_bindings: HashMap<String, Arc<ValueActor>> = HashMap::new();
                                let mut passed_context: Option<Arc<ValueActor>> = actor_context.passed.clone();

                                // If there's a piped value, bind it to the first parameter
                                if let Some(piped) = &actor_context.piped {
                                    if let Some(first_param) = user_func.parameters.first() {
                                        param_bindings.insert(first_param.clone(), piped.clone());
                                    }
                                }

                                // Process named arguments
                                for arg in &arguments {
                                    // Check for PASS: argument
                                    if arg.node.name.as_str() == "PASS" {
                                        if let Some(value) = &arg.node.value {
                                            let pass_actor = static_spanned_expression_into_value_actor(
                                                value.clone(),
                                                construct_context.clone(),
                                                actor_context.clone(),
                                                reference_connector.clone(),
                                                link_connector.clone(),
                                                function_registry.clone(),
                                                module_loader.clone(),
                                                source_code.clone(),
                                            )?;
                                            passed_context = Some(pass_actor);
                                        }
                                        continue;
                                    }

                                    // Bind named argument to parameter
                                    let param_name = arg.node.name.to_string();
                                    if let Some(value) = &arg.node.value {
                                        let actor = static_spanned_expression_into_value_actor(
                                            value.clone(),
                                            construct_context.clone(),
                                            actor_context.clone(),
                                            reference_connector.clone(),
                                            link_connector.clone(),
                                            function_registry.clone(),
                                            module_loader.clone(),
                                            source_code.clone(),
                                        )?;
                                        param_bindings.insert(param_name, actor);
                                    }
                                }

                                // Create actor context with parameter bindings for the function body
                                let func_actor_context = ActorContext {
                                    output_valve_signal: actor_context.output_valve_signal.clone(),
                                    piped: None, // Clear piped - it was bound to first param
                                    passed: passed_context,
                                    parameters: param_bindings,
                                    sequential_processing: actor_context.sequential_processing,
                                    backpressure_permit: actor_context.backpressure_permit.clone(),
                                };

                                // Evaluate the function body with the new context
                                return static_spanned_expression_into_value_actor(
                                    user_func.body.clone(),
                                    construct_context,
                                    func_actor_context,
                                    reference_connector,
                                    link_connector,
                                    function_registry,
                                    module_loader,
                                    source_code,
                                );
                            }
                        }
                    }

                    // Built-in function call - evaluate all arguments
                    let mut evaluated_args: Vec<Arc<ValueActor>> = Vec::new();
                    let mut passed_context: Option<Arc<ValueActor>> = actor_context.passed.clone();

                    // If there's a piped value, add it as the first argument
                    if let Some(piped) = &actor_context.piped {
                        evaluated_args.push(piped.clone());
                    }

                    for arg in &arguments {
                        // Check for PASS: argument - sets implicit context for nested calls
                        if arg.node.name.as_str() == "PASS" {
                            if let Some(value) = &arg.node.value {
                                let pass_actor = static_spanned_expression_into_value_actor(
                                    value.clone(),
                                    construct_context.clone(),
                                    actor_context.clone(),
                                    reference_connector.clone(),
                                    link_connector.clone(),
                                    function_registry.clone(),
                                    module_loader.clone(),
                                    source_code.clone(),
                                )?;
                                passed_context = Some(pass_actor);
                            }
                            continue; // Don't add PASS to positional arguments
                        }

                        if let Some(value) = &arg.node.value {
                            let actor = static_spanned_expression_into_value_actor(
                                value.clone(),
                                construct_context.clone(),
                                actor_context.clone(),
                                reference_connector.clone(),
                                link_connector.clone(),
                                function_registry.clone(),
                                module_loader.clone(),
                                source_code.clone(),
                            )?;
                            evaluated_args.push(actor);
                        }
                    }

                    // Create actor context with PASS context for the function call
                    let call_actor_context = ActorContext {
                        output_valve_signal: actor_context.output_valve_signal.clone(),
                        piped: None, // Clear piped - it was already added as first arg
                        passed: passed_context,
                        parameters: actor_context.parameters.clone(),
                        sequential_processing: actor_context.sequential_processing,
                        backpressure_permit: actor_context.backpressure_permit.clone(),
                    };

                    // Get function definition
                    let borrowed_path: Vec<&str> = path.iter().map(|s| &**s).collect();
                    let definition = static_function_call_path_to_definition(&borrowed_path, span)?;

                    FunctionCall::new_arc_value_actor(
                        ConstructInfo::new(
                            format!("PersistenceId: {persistence_id}"),
                            persistence,
                            format!("{span}; {}(..)", path_strs.join("/")),
                        ),
                        construct_context,
                        call_actor_context,
                        definition,
                        evaluated_args,
                    )
                }
            }
        }
        static_expression::Expression::Skip => {
            let construct_info = ConstructInfo::new(
                format!("PersistenceId: {persistence_id}"),
                persistence,
                format!("{span}; SKIP"),
            );
            ValueActor::new_arc(
                construct_info,
                actor_context,
                TypedStream::infinite(stream::empty()),
                Some(persistence_id),
            )
        }
        // Object expressions - [key: value, ...]
        static_expression::Expression::Object(object) => {
            let evaluated_variables: Result<Vec<Arc<Variable>>, String> = object.variables
                .into_iter()
                .map(|var| {
                    let var_name = var.node.name.to_string();
                    let var_span = var.span.clone();
                    let is_link = matches!(&var.node.value.node, static_expression::Expression::Link);

                    let variable = if is_link {
                        Variable::new_link_arc(
                            ConstructInfo::new(
                                format!("PersistenceId: {persistence_id}; var: {var_name}"),
                                None,
                                format!("{span}; Object variable {var_name} (LINK)"),
                            ),
                            construct_context.clone(),
                            var_name,
                            actor_context.clone(),
                            None,
                        )
                    } else {
                        let value_actor = static_spanned_expression_into_value_actor(
                            var.node.value,
                            construct_context.clone(),
                            actor_context.clone(),
                            reference_connector.clone(),
                            link_connector.clone(),
                            function_registry.clone(),
                            module_loader.clone(),
                            source_code.clone(),
                        )?;
                        Variable::new_arc(
                            ConstructInfo::new(
                                format!("PersistenceId: {persistence_id}; var: {var_name}"),
                                None,
                                format!("{span}; Object variable {var_name}"),
                            ),
                            construct_context.clone(),
                            var_name,
                            value_actor,
                            None,
                        )
                    };

                    // Register LINK variable senders with LinkConnector
                    if is_link {
                        if let Some(sender) = variable.link_value_sender() {
                            link_connector.register_link(var_span, sender);
                        }
                    }

                    Ok(variable)
                })
                .collect();
            Object::new_arc_value_actor(
                ConstructInfo::new(
                    format!("PersistenceId: {persistence_id}"),
                    persistence,
                    format!("{span}; Object [..]"),
                ),
                construct_context,
                idempotency_key,
                actor_context,
                evaluated_variables?,
            )
        }
        // TaggedObject expressions - Tag[key: value, ...]
        static_expression::Expression::TaggedObject { tag, object } => {
            let tag_string = tag.to_string();
            let evaluated_variables: Result<Vec<Arc<Variable>>, String> = object.variables
                .into_iter()
                .map(|var| {
                    let var_name = var.node.name.to_string();
                    let var_span = var.span.clone();
                    let is_link = matches!(&var.node.value.node, static_expression::Expression::Link);

                    let variable = if is_link {
                        Variable::new_link_arc(
                            ConstructInfo::new(
                                format!("PersistenceId: {persistence_id}; var: {var_name}"),
                                None,
                                format!("{span}; TaggedObject {tag_string} variable {var_name} (LINK)"),
                            ),
                            construct_context.clone(),
                            var_name,
                            actor_context.clone(),
                            None,
                        )
                    } else {
                        let value_actor = static_spanned_expression_into_value_actor(
                            var.node.value,
                            construct_context.clone(),
                            actor_context.clone(),
                            reference_connector.clone(),
                            link_connector.clone(),
                            function_registry.clone(),
                            module_loader.clone(),
                            source_code.clone(),
                        )?;
                        Variable::new_arc(
                            ConstructInfo::new(
                                format!("PersistenceId: {persistence_id}; var: {var_name}"),
                                None,
                                format!("{span}; TaggedObject {tag_string} variable {var_name}"),
                            ),
                            construct_context.clone(),
                            var_name,
                            value_actor,
                            None,
                        )
                    };

                    // Register LINK variable senders with LinkConnector
                    if is_link {
                        if let Some(sender) = variable.link_value_sender() {
                            link_connector.register_link(var_span, sender);
                        }
                    }

                    Ok(variable)
                })
                .collect();
            TaggedObject::new_arc_value_actor(
                ConstructInfo::new(
                    format!("PersistenceId: {persistence_id}"),
                    persistence,
                    format!("{span}; {tag_string}[..]"),
                ),
                construct_context,
                idempotency_key,
                actor_context,
                tag_string,
                evaluated_variables?,
            )
        }
        static_expression::Expression::Map { .. } => {
            return Err("Map expressions not yet supported in static context".to_string());
        }
        static_expression::Expression::Function { .. } => {
            return Err("Function definitions not supported in static context".to_string());
        }
        static_expression::Expression::LinkSetter { alias } => {
            // LinkSetter: `|> LINK { alias }` - sends piped value to the LINK variable
            // Get the referenced_span from the alias to look up the link sender
            let referenced_span = match &alias.node {
                static_expression::Alias::WithoutPassed { referenced_span, .. } => {
                    referenced_span.ok_or("LinkSetter alias has no referenced_span")?
                }
                static_expression::Alias::WithPassed { .. } => {
                    return Err("LinkSetter does not support PASSED alias".to_string());
                }
            };

            match &actor_context.piped {
                Some(piped) => {
                    let piped = piped.clone();
                    let link_connector_for_setter = link_connector.clone();

                    // Subscribe to the piped stream and send each value to the link sender
                    let sending_stream = piped.clone().subscribe().then(move |value| {
                        let link_connector_clone = link_connector_for_setter.clone();
                        async move {
                            // Get the link sender from the connector
                            let sender = link_connector_clone.link_sender(referenced_span).await;
                            // Send the value to the LINK variable
                            if sender.unbounded_send(value.clone()).is_err() {
                                eprintln!("Failed to send value to LINK variable");
                            }
                            value
                        }
                    });

                    ValueActor::new_arc(
                        ConstructInfo::new(
                            format!("PersistenceId: {persistence_id}"),
                            persistence,
                            format!("{span}; LinkSetter"),
                        ),
                        actor_context,
                        TypedStream::infinite(sending_stream),
                        Some(persistence_id),
                    )
                }
                None => {
                    return Err("LinkSetter requires a piped value".to_string());
                }
            }
        }
        static_expression::Expression::Link => {
            return Err("Link not yet supported in static context".to_string());
        }
        static_expression::Expression::Latest { inputs } => {
            // LATEST merges multiple streams and emits whenever any stream produces a value
            // Returns the most recent value from any of the input streams
            let evaluated_inputs: Result<Vec<Arc<ValueActor>>, String> = inputs
                .into_iter()
                .map(|input| {
                    static_spanned_expression_into_value_actor(
                        input,
                        construct_context.clone(),
                        actor_context.clone(),
                        reference_connector.clone(),
                        link_connector.clone(),
                        function_registry.clone(),
                        module_loader.clone(),
                        source_code.clone(),
                    )
                })
                .collect();
            let inputs = evaluated_inputs?;

            // Merge all input streams using select_all
            // subscribe() returns a Subscription that keeps the actor alive
            let merged_stream = stream::select_all(
                inputs.iter().map(|input| input.clone().subscribe())
            );

            ValueActor::new_arc(
                ConstructInfo::new(
                    format!("PersistenceId: {persistence_id}"),
                    persistence,
                    format!("{span}; LATEST {{..}}"),
                ),
                actor_context,
                TypedStream::infinite(merged_stream),
                Some(persistence_id),
            )
        }
        static_expression::Expression::Then { body } => {
            // THEN transforms the piped value using the body expression
            // It evaluates the body with piped set to each incoming value
            match &actor_context.piped {
                Some(piped) => {
                    let piped = piped.clone();
                    let construct_context_for_then = construct_context.clone();
                    let actor_context_for_then = actor_context.clone();
                    let reference_connector_for_then = reference_connector.clone();
                    let link_connector_for_then = link_connector.clone();
                    let function_registry_for_then = function_registry.clone();
                    let module_loader_for_then = module_loader.clone();
                    let source_code_for_then = source_code.clone();
                    let persistence_for_then = persistence.clone();
                    let span_for_then = span;

                    // Subscribe to the piped stream and for each value, evaluate the body
                    let body_for_closure = body;

                    // Check if we need sequential processing (inside HOLD).
                    // When sequential_processing is true, we use .then() which processes
                    // one value at a time, waiting for the body to complete before processing next.
                    // This prevents race conditions where parallel body evaluations all read stale state.
                    let sequential = actor_context_for_then.sequential_processing;

                    // Extract backpressure permit BEFORE creating eval_body closure
                    // (closure moves actor_context_for_then, so we must extract first)
                    let backpressure_permit = actor_context_for_then.backpressure_permit.clone();

                    // When inside HOLD (has backpressure), we need to materialize Object values
                    // to prevent circular dependencies from lazy ValueActors referencing state
                    let should_materialize = backpressure_permit.is_some();

                    // Helper closure for evaluating a single input value
                    // Returns Option<Value> - the body result (or None on error)
                    let eval_body = move |value: Value| {
                        let actor_context_clone = actor_context_for_then.clone();
                        let construct_context_clone = construct_context_for_then.clone();
                        let reference_connector_clone = reference_connector_for_then.clone();
                        let link_connector_clone = link_connector_for_then.clone();
                        let function_registry_clone = function_registry_for_then.clone();
                        let module_loader_clone = module_loader_for_then.clone();
                        let source_code_clone = source_code_for_then.clone();
                        let persistence_clone = persistence_for_then.clone();
                        let body_clone = body_for_closure.clone();

                        async move {
                            // Create a new value actor for this specific value
                            // Use constant() to keep the actor alive (emits once then stays pending)
                            // rather than stream::once() which ends after emitting.
                            let value_actor = ValueActor::new_arc(
                                ConstructInfo::new(
                                    format!("THEN input value"),
                                    None,
                                    format!("{span_for_then}; THEN input"),
                                ),
                                actor_context_clone.clone(),
                                constant(value),
                                None,
                            );

                            // Evaluate the body with PASSED set to this value
                            // Clone value_actor - we need one for the actor context and one to keep alive
                            let new_actor_context = ActorContext {
                                output_valve_signal: actor_context_clone.output_valve_signal.clone(),
                                piped: Some(value_actor.clone()),
                                passed: actor_context_clone.passed.clone(),
                                parameters: actor_context_clone.parameters.clone(),
                                // Propagate sequential_processing to nested THEN/WHEN inside body
                                sequential_processing: actor_context_clone.sequential_processing,
                                // Propagate backpressure_permit to nested constructs
                                backpressure_permit: actor_context_clone.backpressure_permit.clone(),
                            };

                            let body_expr = static_expression::Spanned {
                                span: body_clone.span,
                                persistence: persistence_clone,
                                node: body_clone.node.clone(),
                            };

                            // Clone construct_context for potential materialization use
                            let construct_context_for_materialize = construct_context_clone.clone();

                            match static_spanned_expression_into_value_actor(
                                body_expr,
                                construct_context_clone,
                                new_actor_context.clone(),
                                reference_connector_clone,
                                link_connector_clone,
                                function_registry_clone,
                                module_loader_clone,
                                source_code_clone,
                            ) {
                                Ok(result_actor) => {
                                    // Subscribe and get first value from body
                                    // Keep value_actor alive while we wait for the result
                                    let mut subscription = result_actor.subscribe();
                                    let _keep_alive = value_actor;
                                    if let Some(mut result_value) = subscription.next().await {
                                        // THEN SEMANTICS: Each input pulse produces a conceptually "new" value,
                                        // even if the body evaluates to the same content (e.g., constant `1`).
                                        // We assign fresh idempotency keys so downstream consumers (like Math/sum)
                                        // treat each pulse's output as unique rather than skipping "duplicates".
                                        result_value.set_idempotency_key(ValueIdempotencyKey::new());

                                        // When inside HOLD, materialize Object values to break circular
                                        // dependencies from lazy ValueActors that reference state
                                        if should_materialize {
                                            result_value = materialize_value(
                                                result_value,
                                                construct_context_for_materialize,
                                                new_actor_context.clone(),
                                            ).await;
                                        }

                                        Some(result_value)
                                    } else {
                                        None
                                    }
                                }
                                Err(_) => None,
                            }
                        }
                    };

                    // Create the flattened stream using either sequential or parallel processing
                    let flattened_stream: Pin<Box<dyn Stream<Item = Value>>> = if backpressure_permit.is_some() || sequential {
                        // Backpressure or sequential mode: process one at a time.
                        // When backpressure permit exists (inside HOLD), PULSES has already
                        // acquired the permit before emitting each value. THEN just processes
                        // sequentially, and HOLD releases permit after state update.
                        // This guarantees state is updated before next pulse arrives.
                        let stream = piped.clone().subscribe()
                            .then(eval_body)
                            .filter_map(|opt| async { opt });
                        Box::pin(stream)
                    } else {
                        // Parallel mode (default): use filter_map which spawns concurrent tasks.
                        // Each input value's body evaluation runs in parallel, which is faster
                        // but can cause race conditions when reading shared state (like HOLD).
                        //
                        // The original two-step approach (filter_map + flat_map) is kept for
                        // compatibility, even though eval_body now does both steps.
                        let stream = piped.clone().subscribe().filter_map(eval_body);
                        Box::pin(stream)
                    };

                    ValueActor::new_arc(
                        ConstructInfo::new(
                            format!("PersistenceId: {persistence_id}"),
                            persistence,
                            format!("{span}; THEN {{..}}"),
                        ),
                        actor_context,
                        TypedStream::infinite(flattened_stream),
                        Some(persistence_id),
                    )
                }
                None => {
                    return Err("THEN requires a piped value".to_string());
                }
            }
        }
        static_expression::Expression::When { arms } => {
            // WHEN performs pattern matching on the piped value
            // It tries each arm in order and evaluates the first matching arm's body
            match &actor_context.piped {
                Some(piped) => {
                    let piped = piped.clone();
                    let construct_context_for_when = construct_context.clone();
                    let actor_context_for_when = actor_context.clone();
                    let reference_connector_for_when = reference_connector.clone();
                    let link_connector_for_when = link_connector.clone();
                    let function_registry_for_when = function_registry.clone();
                    let module_loader_for_when = module_loader.clone();
                    let source_code_for_when = source_code.clone();
                    let persistence_for_when = persistence.clone();
                    let span_for_when = span;
                    let arms_for_closure = arms.clone();

                    // Extract these BEFORE creating eval_body closure
                    // (closure moves actor_context_for_when, so we must extract first)
                    let sequential = actor_context_for_when.sequential_processing;
                    let backpressure_permit = actor_context_for_when.backpressure_permit.clone();

                    // Helper closure for evaluating a single input value
                    // Returns Option<Value> - the body result (or None on error or no match)
                    let eval_body = move |value: Value| {
                        let actor_context_clone = actor_context_for_when.clone();
                        let construct_context_clone = construct_context_for_when.clone();
                        let reference_connector_clone = reference_connector_for_when.clone();
                        let link_connector_clone = link_connector_for_when.clone();
                        let function_registry_clone = function_registry_for_when.clone();
                        let module_loader_clone = module_loader_for_when.clone();
                        let source_code_clone = source_code_for_when.clone();
                        let persistence_clone = persistence_for_when.clone();
                        let arms_clone = arms_for_closure.clone();

                        async move {
                            // Try each arm in order
                            for arm in &arms_clone {
                                if let Some(bindings) = match_pattern(
                                    &arm.pattern,
                                    &value,
                                    &construct_context_clone,
                                    &actor_context_clone,
                                ) {
                                    // Pattern matched! Evaluate the body with bindings
                                    let mut params = actor_context_clone.parameters.clone();
                                    for (name, actor) in bindings {
                                        params.insert(name, actor);
                                    }

                                    let new_actor_context = ActorContext {
                                        output_valve_signal: actor_context_clone.output_valve_signal.clone(),
                                        piped: actor_context_clone.piped.clone(),
                                        passed: actor_context_clone.passed.clone(),
                                        parameters: params,
                                        // Propagate sequential_processing to nested THEN/WHEN inside body
                                        sequential_processing: actor_context_clone.sequential_processing,
                                        backpressure_permit: actor_context_clone.backpressure_permit.clone(),
                                    };

                                    // Create a spanned expression from the body
                                    let body_expr = static_expression::Spanned {
                                        span: span_for_when,
                                        persistence: persistence_clone,
                                        node: arm.body.clone(),
                                    };

                                    match static_spanned_expression_into_value_actor(
                                        body_expr,
                                        construct_context_clone,
                                        new_actor_context,
                                        reference_connector_clone,
                                        link_connector_clone,
                                        function_registry_clone,
                                        module_loader_clone,
                                        source_code_clone,
                                    ) {
                                        Ok(result_actor) => {
                                            // Get first value from body before returning
                                            let mut subscription = result_actor.subscribe();
                                            if let Some(mut result_value) = subscription.next().await {
                                                // WHEN SEMANTICS: Like THEN, each input pulse produces a "new" value.
                                                result_value.set_idempotency_key(ValueIdempotencyKey::new());
                                                return Some(result_value);
                                            }
                                            return None;
                                        }
                                        Err(_) => return None,
                                    }
                                }
                            }
                            // No arm matched
                            None
                        }
                    };

                    // Create the flattened stream using either sequential or parallel processing
                    let flattened_stream: Pin<Box<dyn Stream<Item = Value>>> = if let Some(permit) = backpressure_permit {
                        // Backpressure mode: HOLD controls pacing via permit.
                        // WHEN acquires permit before each body evaluation.
                        // HOLD releases permit after updating state.
                        // This guarantees state is updated before next body starts.
                        let stream = piped.clone().subscribe()
                            .then(move |value| {
                                let permit = permit.clone();
                                let eval = eval_body.clone();
                                async move {
                                    // Wait for permit - HOLD releases after state update
                                    permit.acquire().await;
                                    eval(value).await
                                }
                            })
                            .filter_map(|opt| async { opt });
                        Box::pin(stream)
                    } else if sequential {
                        // Sequential mode without backpressure (fallback).
                        // Uses .then() to process one at a time, but no synchronization with HOLD.
                        let stream = piped.clone().subscribe().then(eval_body).filter_map(|opt| async { opt });
                        Box::pin(stream)
                    } else {
                        // Parallel mode (default): use filter_map which spawns concurrent tasks.
                        let stream = piped.clone().subscribe().filter_map(eval_body);
                        Box::pin(stream)
                    };

                    ValueActor::new_arc(
                        ConstructInfo::new(
                            format!("PersistenceId: {persistence_id}"),
                            persistence,
                            format!("{span}; WHEN {{..}}"),
                        ),
                        actor_context,
                        TypedStream::infinite(flattened_stream),
                        Some(persistence_id),
                    )
                }
                None => {
                    return Err("WHEN requires a piped value".to_string());
                }
            }
        }
        static_expression::Expression::While { arms } => {
            // WHILE is similar to WHEN but used for conditional UI rendering
            // It performs pattern matching and evaluates the matching arm's body
            match &actor_context.piped {
                Some(piped) => {
                    let piped = piped.clone();
                    let construct_context_for_while = construct_context.clone();
                    let actor_context_for_while = actor_context.clone();
                    let reference_connector_for_while = reference_connector.clone();
                    let link_connector_for_while = link_connector.clone();
                    let function_registry_for_while = function_registry.clone();
                    let source_code_for_while = source_code.clone();
                    let module_loader_for_while = module_loader.clone();
                    let persistence_for_while = persistence.clone();
                    let span_for_while = span;
                    let arms_for_closure = arms.clone();

                    // For each value, try to match against arms and evaluate matching body
                    let mapped_stream = piped.clone().subscribe().filter_map(move |value| {
                        let actor_context_clone = actor_context_for_while.clone();
                        let construct_context_clone = construct_context_for_while.clone();
                        let reference_connector_clone = reference_connector_for_while.clone();
                        let link_connector_clone = link_connector_for_while.clone();
                        let function_registry_clone = function_registry_for_while.clone();
                        let source_code_clone = source_code_for_while.clone();
                        let module_loader_clone = module_loader_for_while.clone();
                        let persistence_clone = persistence_for_while.clone();
                        let arms_clone = arms_for_closure.clone();

                        async move {
                            // Try each arm in order
                            for arm in &arms_clone {
                                if let Some(bindings) = match_pattern(
                                    &arm.pattern,
                                    &value,
                                    &construct_context_clone,
                                    &actor_context_clone,
                                ) {
                                    // Pattern matched! Evaluate the body with bindings
                                    let mut params = actor_context_clone.parameters.clone();
                                    for (name, actor) in bindings {
                                        params.insert(name, actor);
                                    }

                                    let new_actor_context = ActorContext {
                                        output_valve_signal: actor_context_clone.output_valve_signal.clone(),
                                        piped: actor_context_clone.piped.clone(),
                                        passed: actor_context_clone.passed.clone(),
                                        parameters: params,
                                        // Propagate sequential_processing to nested constructs
                                        sequential_processing: actor_context_clone.sequential_processing,
                                        backpressure_permit: actor_context_clone.backpressure_permit.clone(),
                                    };

                                    // Create a spanned expression from the body
                                    let body_expr = static_expression::Spanned {
                                        span: span_for_while,
                                        persistence: persistence_clone,
                                        node: arm.body.clone(),
                                    };

                                    match static_spanned_expression_into_value_actor(
                                        body_expr,
                                        construct_context_clone,
                                        new_actor_context,
                                        reference_connector_clone,
                                        link_connector_clone,
                                        function_registry_clone,
                                        module_loader_clone,
                                        source_code_clone,
                                    ) {
                                        Ok(result_actor) => return Some(result_actor),
                                        Err(_) => return None,
                                    }
                                }
                            }
                            // No arm matched
                            None
                        }
                    });

                    // Flatten the stream of actors into a stream of values
                    // subscribe() returns Subscription which keeps the actor alive
                    // IMPORTANT: Use .take(1) because body results use constant() streams
                    // which never complete. Without take(1), flat_map blocks waiting for
                    // the inner stream to finish, preventing subsequent input values from
                    // being processed (causing interval/counter to only process first tick).
                    //
                    // WHILE: Like THEN/WHEN, each matching input produces a body evaluation.
                    // While WHILE has "let everything flow" semantics at a conceptual level,
                    // the current implementation evaluates the body per input pulse, so we
                    // need fresh idempotency keys to prevent downstream duplicate skipping.
                    let flattened_stream = mapped_stream.flat_map(|actor| {
                        actor.subscribe().take(1).map(|mut value| {
                            value.set_idempotency_key(ValueIdempotencyKey::new());
                            value
                        })
                    });

                    ValueActor::new_arc(
                        ConstructInfo::new(
                            format!("PersistenceId: {persistence_id}"),
                            persistence,
                            format!("{span}; WHILE {{..}}"),
                        ),
                        actor_context,
                        TypedStream::infinite(flattened_stream),
                        Some(persistence_id),
                    )
                }
                None => {
                    return Err("WHILE requires a piped value".to_string());
                }
            }
        }
        static_expression::Expression::Hold { state_param, body } => {
            // TODO: Add compiler check to reject expensive-to-copy types (LIST, MAP, BYTES, MEMORY) in HOLD.
            // HOLD replaces entire value on each update - variable-size types are a performance trap.
            // See docs/language/HOLD.md "Supported Types".

            // HOLD: `input |> HOLD state_param { body }`
            // The piped value sets/resets the state (not just initial - any emission).
            // The body can reference `state_param` to get the current state.
            // The body expression's result becomes the new state value.
            // CRITICAL: The state is NOT self-reactive - changes to state don't
            // trigger re-evaluation of body. Only external events trigger updates.
            //
            // Example with reset:
            // ```boon
            // counter: LATEST { 0, reset } |> HOLD counter {
            //     increment |> THEN { counter + 1 }
            // }
            // ```
            // Here, `counter` starts at 0. When `increment` fires, `counter + 1`
            // becomes new state. When `reset` emits, state resets to that value.

            let initial_actor = actor_context.piped.clone()
                .ok_or("HOLD requires a piped initial value")?;

            let state_param_string = state_param.to_string();
            let construct_context_for_state = construct_context.clone();
            let actor_context_for_state = actor_context.clone();
            let persistence_for_state = persistence.clone();
            let span_for_state = span;

            // Use a channel to hold current state value and broadcast updates
            let (state_sender, state_receiver) = zoon::futures_channel::mpsc::unbounded::<Value>();
            let state_sender = Rc::new(RefCell::new(state_sender));
            let state_sender_for_body = state_sender.clone();
            let state_sender_for_update = state_sender.clone();

            // Current state holder (starts with None, will be set when initial emits)
            let current_state: Rc<RefCell<Option<Value>>> = Rc::new(RefCell::new(None));
            let current_state_for_body = current_state.clone();
            let current_state_for_update = current_state.clone();

            // Create a ValueActor that provides the current state to the body
            // This is what the state_param references
            //
            // CRITICAL: state_actor's stream MUST first get the initial value directly
            // from initial_actor (using take(1)), then listen to state_receiver for updates.
            // This ensures the initial value is available BEFORE body evaluation starts.
            // Without this, there's a race condition:
            // 1. Body evaluates, creating PULSES/THEN actors
            // 2. PULSES emits, THEN evaluates, needs state.field
            // 3. But state_actor hasn't received initial value yet (it comes from combined_stream)
            // 4. Deadlock: body waits for state, state waits for combined_stream to run
            //
            // The fix: state_actor first subscribes to initial_actor directly, getting
            // the initial value immediately. Then it chains with state_receiver for:
            // - Body updates (from state_update_stream → state_sender_for_update)
            // - Reset values (from initial_stream → state_sender_for_body)
            let state_stream = initial_actor.clone().subscribe()
                .take(1)  // Get the first initial value directly
                .chain(state_receiver);  // Then listen for updates and resets
            let state_actor = ValueActor::new_arc(
                ConstructInfo::new(
                    format!("Hold state actor for {state_param_string}"),
                    None,
                    format!("{span}; HOLD state parameter"),
                ),
                actor_context.clone(),
                TypedStream::infinite(state_stream),
                None,
            );

            // Bind the state parameter in the context so body can reference it
            let mut body_parameters = actor_context.parameters.clone();
            body_parameters.insert(state_param_string.clone(), state_actor);

            // Create backpressure permit for synchronizing THEN with state updates.
            // Initial count = 1 allows first body evaluation to start.
            // HOLD releases permit after each state update, allowing next body to run.
            let backpressure_permit = BackpressurePermit::new(1);
            let permit_for_state_update = backpressure_permit.clone();

            let body_actor_context = ActorContext {
                output_valve_signal: actor_context.output_valve_signal.clone(),
                piped: None, // Clear piped - the body shouldn't re-use it
                passed: actor_context.passed.clone(),
                parameters: body_parameters,
                // Force sequential processing in HOLD body to ensure state consistency.
                // Without this, THEN/WHEN would spawn parallel body evaluations that all
                // read stale state (e.g., PULSES {3} |> THEN { counter + 1 } would read counter=0 three times).
                sequential_processing: true,
                // Pass permit to body - THEN will acquire before each evaluation
                backpressure_permit: Some(backpressure_permit),
            };

            // Evaluate the body with state parameter bound
            let body_result = static_spanned_expression_into_value_actor(
                *body,
                construct_context.clone(),
                body_actor_context,
                reference_connector.clone(),
                link_connector.clone(),
                function_registry.clone(),
                module_loader.clone(),
                source_code.clone(),
            )?;

            // When body produces new values, update the state
            // Note: We avoid self-reactivity by not triggering body re-evaluation
            // from state changes. Body only evaluates when its event sources fire.
            let body_subscription = body_result.subscribe();
            let state_update_stream = body_subscription.map(move |new_value| {
                // Update current state
                *current_state_for_update.borrow_mut() = Some(new_value.clone());
                // Send to state channel so body can see it on next event
                let _ = state_sender_for_update.borrow().unbounded_send(new_value.clone());
                // Release permit to allow THEN to process next input.
                // This guarantees state is updated before next body evaluation starts.
                permit_for_state_update.release();
                new_value
            });

            // When initial value emits, set up initial state
            let initial_stream = initial_actor.subscribe().map(move |initial| {
                // Set current state
                *current_state_for_body.borrow_mut() = Some(initial.clone());
                // Send initial state to the state channel
                let _ = state_sender_for_body.borrow().unbounded_send(initial.clone());
                initial
            });

            // Combine: input stream sets/resets state, body updates state
            // Use select to merge both streams - any emission from input resets state
            let combined_stream = stream::select(
                initial_stream, // Any emission from input resets the state
                state_update_stream
            );

            ValueActor::new_arc(
                ConstructInfo::new(
                    format!("PersistenceId: {persistence_id}"),
                    persistence,
                    format!("{span}; HOLD {state_param_string} {{..}}"),
                ),
                actor_context,
                TypedStream::infinite(combined_stream),
                Some(persistence_id),
            )
        }
        static_expression::Expression::Flush { value } => {
            // FLUSH for fail-fast error handling
            // `FLUSH { error_value }` creates a FLUSHED[value] wrapper that propagates transparently
            // The wrapper bypasses function processing and unwraps at boundaries
            // (variable bindings, function returns, BLOCK returns)
            //
            // From FLUSH.md:
            // - FLUSHED[value] propagates transparently through pipelines
            // - Functions check if input is FLUSHED, if so bypass processing
            // - Unwraps at boundaries (assignment, function return, BLOCK return)

            let error_actor = static_spanned_expression_into_value_actor(
                *value,
                construct_context.clone(),
                actor_context.clone(),
                reference_connector.clone(),
                link_connector.clone(),
                function_registry.clone(),
                module_loader.clone(),
                source_code.clone(),
            )?;

            // Wrap each emitted value in Value::Flushed
            let flushed_stream = error_actor.subscribe().map(|value| {
                value.into_flushed()
            });

            ValueActor::new_arc(
                ConstructInfo::new(
                    format!("PersistenceId: {persistence_id}"),
                    persistence,
                    format!("{span}; FLUSH {{..}}"),
                ),
                actor_context,
                TypedStream::infinite(flushed_stream),
                Some(persistence_id),
            )
        }
        static_expression::Expression::Pulses { count } => {
            // PULSES for iteration: `PULSES { count }` emits count values (0 to count-1)
            // Can be used with THEN for iteration: `PULSES { 10 } |> THEN { ... }`

            let count_actor = static_spanned_expression_into_value_actor(
                *count,
                construct_context.clone(),
                actor_context.clone(),
                reference_connector.clone(),
                link_connector.clone(),
                function_registry.clone(),
                module_loader.clone(),
                source_code.clone(),
            )?;

            let construct_context_for_pulses = construct_context.clone();

            // Get backpressure permit from HOLD context if available.
            // When inside HOLD, PULSES will acquire permit before each emission,
            // ensuring consumer (THEN) processes each value before next is emitted.
            let backpressure_permit = actor_context.backpressure_permit.clone();

            // When count changes, emit that many pulses
            // Clone count_actor before moving into closure - we need to keep it alive
            // Use stream::unfold instead of stream::iter to yield between emissions,
            // ensuring downstream subscribers have a chance to process each pulse
            let pulses_stream = count_actor.clone().subscribe().flat_map(move |count_value| {
                let n = match &count_value {
                    Value::Number(num, _) => num.number() as i64,
                    _ => 0,
                };

                let construct_context_inner = construct_context_for_pulses.clone();
                let permit_for_iteration = backpressure_permit.clone();

                // Use unfold to emit pulses one at a time with async yield points
                // When backpressure permit exists (inside HOLD), acquire it before
                // each emission to ensure THEN processes the value before next pulse.
                stream::unfold(0i64, move |i| {
                    let construct_context_for_iter = construct_context_inner.clone();
                    let permit = permit_for_iteration.clone();
                    async move {
                        if i >= n.max(0) {
                            return None;
                        }

                        // If backpressure permit exists, acquire before emitting.
                        // This ensures previous value was processed by consumer (THEN/HOLD).
                        // HOLD releases permit after state update, so next pulse can emit.
                        if let Some(ref permit) = permit {
                            permit.acquire().await;
                        } else {
                            // No backpressure: yield to allow downstream to process
                            yield_once().await;
                        }

                        let value = Value::Number(
                            Arc::new(Number::new(
                                ConstructInfo::new(
                                    format!("PULSES iteration {i}"),
                                    None,
                                    format!("PULSES iteration {i}"),
                                ),
                                construct_context_for_iter,
                                i as f64,
                            )),
                            ValueMetadata {
                                idempotency_key: Ulid::new(),
                            },
                        );
                        Some((value, i + 1))
                    }
                })
            });

            // Keep count_actor alive by passing it as an input dependency
            ValueActor::new_arc_with_inputs(
                ConstructInfo::new(
                    format!("PersistenceId: {persistence_id}"),
                    persistence,
                    format!("{span}; PULSES {{..}}"),
                ),
                actor_context,
                TypedStream::infinite(pulses_stream),
                Some(persistence_id),
                vec![count_actor],
            )
        }
        static_expression::Expression::Spread { value } => {
            // Spread operator: `...expression` - spreads object fields
            // Used in object literals: `[...base, override: new_value]`
            // For now, just evaluate the expression and return it
            // The actual spreading happens at object construction time

            static_spanned_expression_into_value_actor(
                *value,
                construct_context,
                actor_context,
                reference_connector,
                link_connector,
                function_registry,
                module_loader,
                source_code,
            )?
        }
        static_expression::Expression::Pipe { from, to } => {
            // Evaluate the 'from' expression to get the piped value
            let from_actor = static_spanned_expression_into_value_actor(
                *from,
                construct_context.clone(),
                actor_context.clone(),
                reference_connector.clone(),
                link_connector.clone(),
                function_registry.clone(),
                module_loader.clone(),
                source_code.clone(),
            )?;

            // Create new actor context with piped value set
            let new_actor_context = ActorContext {
                output_valve_signal: actor_context.output_valve_signal.clone(),
                piped: Some(from_actor),
                passed: actor_context.passed.clone(),
                parameters: actor_context.parameters.clone(),
                sequential_processing: actor_context.sequential_processing,
                backpressure_permit: actor_context.backpressure_permit.clone(),
            };

            // Evaluate the 'to' expression with the new actor context
            return static_spanned_expression_into_value_actor(
                *to,
                construct_context,
                new_actor_context,
                reference_connector,
                link_connector,
                function_registry,
                module_loader,
                source_code,
            );
        }
        static_expression::Expression::Block { variables, output } => {
            // BLOCK creates a scope with local variables
            // Variables are evaluated in order and added to parameters
            // The output expression is then evaluated with access to those variables

            // Start with current parameters
            let mut local_parameters = actor_context.parameters.clone();

            // Evaluate each variable and add to local scope
            for var in variables {
                let var_name = var.node.name.to_string();
                let value_actor = static_spanned_expression_into_value_actor(
                    var.node.value,
                    construct_context.clone(),
                    ActorContext {
                        output_valve_signal: actor_context.output_valve_signal.clone(),
                        piped: actor_context.piped.clone(),
                        passed: actor_context.passed.clone(),
                        parameters: local_parameters.clone(),
                        sequential_processing: actor_context.sequential_processing,
                        backpressure_permit: actor_context.backpressure_permit.clone(),
                    },
                    reference_connector.clone(),
                    link_connector.clone(),
                    function_registry.clone(),
                    module_loader.clone(),
                    source_code.clone(),
                )?;
                local_parameters.insert(var_name, value_actor);
            }

            // Evaluate the output expression with local variables in scope
            return static_spanned_expression_into_value_actor(
                *output,
                construct_context,
                ActorContext {
                    output_valve_signal: actor_context.output_valve_signal.clone(),
                    piped: actor_context.piped.clone(),
                    passed: actor_context.passed.clone(),
                    parameters: local_parameters,
                    sequential_processing: actor_context.sequential_processing,
                    backpressure_permit: actor_context.backpressure_permit.clone(),
                },
                reference_connector,
                link_connector,
                function_registry,
                module_loader,
                source_code,
            );
        }
        static_expression::Expression::TextLiteral { parts } => {
            // TextLiteral combines literal text with interpolated variables
            // e.g., TEXT { {count} item{maybe_s} left }

            // Collect all parts - literals as constant streams, interpolations as variable lookups
            let mut part_actors: Vec<(bool, Arc<ValueActor>)> = Vec::new();

            for part in &parts {
                match part {
                    static_expression::TextPart::Text(text) => {
                        // Literal text part - create a constant text value
                        let text_string = text.to_string();
                        let text_actor = Text::new_arc_value_actor(
                            ConstructInfo::new(
                                format!("TextLiteral part"),
                                None,
                                format!("{span}; TextLiteral text part"),
                            ),
                            construct_context.clone(),
                            idempotency_key,
                            actor_context.clone(),
                            text_string,
                        );
                        part_actors.push((true, text_actor));
                    }
                    static_expression::TextPart::Interpolation { var, referenced_span } => {
                        // Interpolation - look up the variable
                        let var_name = var.to_string();
                        if let Some(var_actor) = actor_context.parameters.get(&var_name) {
                            part_actors.push((false, var_actor.clone()));
                        } else if let Some(ref_span) = referenced_span {
                            // Use reference_connector to get the variable from outer scope
                            // Create a wrapper actor that resolves the reference asynchronously
                            let ref_connector = reference_connector.clone();
                            let ref_span_copy = *ref_span;
                            let value_stream = stream::once(ref_connector.referenceable(ref_span_copy))
                                .flat_map(|actor| actor.subscribe())
                                .boxed_local();
                            let ref_actor = Arc::new(ValueActor::new(
                                ConstructInfo::new(
                                    format!("TextInterpolation:{}", var_name),
                                    None,
                                    format!("{span}; TextInterpolation for '{}'", var_name),
                                ).complete(ConstructType::ValueActor),
                                actor_context.clone(),
                                TypedStream::infinite(value_stream),
                                None,
                            ));
                            part_actors.push((false, ref_actor));
                        } else {
                            return Err(format!("Variable '{}' not found for text interpolation", var_name));
                        }
                    }
                }
            }

            if part_actors.is_empty() {
                // Empty text literal
                Text::new_arc_value_actor(
                    ConstructInfo::new(
                        format!("PersistenceId: {persistence_id}"),
                        persistence,
                        format!("{span}; TextLiteral empty"),
                    ),
                    construct_context,
                    idempotency_key,
                    actor_context,
                    String::new(),
                )
            } else if part_actors.len() == 1 && part_actors[0].0 {
                // Single literal text part - return as-is
                part_actors.into_iter().next().unwrap().1
            } else {
                // Multiple parts or interpolations - combine with combineLatest-like behavior
                let actor_context_for_combine = actor_context.clone();
                let construct_context_for_combine = construct_context.clone();
                let span_for_combine = span;

                // Create combined stream using select_all on all part streams
                // Each time any part emits, we need to recombine
                let part_subscriptions: Vec<_> = part_actors
                    .iter()
                    .map(|(_, actor)| actor.clone().subscribe())
                    .collect();

                // For simplicity, use select_all and latest values approach
                let merged = stream::select_all(part_subscriptions.into_iter().enumerate().map(|(idx, s)| {
                    s.map(move |v| (idx, v))
                }));

                let part_count = part_actors.len();
                let combined_stream = merged.scan(
                    vec![None; part_count],
                    move |latest_values, (idx, value)| {
                        latest_values[idx] = Some(value);

                        // Check if all parts have values
                        if latest_values.iter().all(|v| v.is_some()) {
                            // Combine all text parts
                            let combined: String = latest_values
                                .iter()
                                .filter_map(|v| {
                                    v.as_ref().and_then(|val| {
                                        match val {
                                            Value::Text(text, _) => Some(text.text().to_string()),
                                            Value::Number(num, _) => Some(num.number().to_string()),
                                            Value::Tag(tag, _) => Some(tag.tag().to_string()),
                                            _ => None,
                                        }
                                    })
                                })
                                .collect();

                            std::future::ready(Some(Some(combined)))
                        } else {
                            std::future::ready(Some(None))
                        }
                    },
                )
                .filter_map(|opt| async move { opt });

                // Create a value actor for the combined text
                // We'll use flat_map to create each combined text value
                let flattened = combined_stream.flat_map(move |combined_text| {
                    let text_actor = Text::new_arc_value_actor(
                        ConstructInfo::new(
                            format!("TextLiteral combined"),
                            None,
                            format!("{span_for_combine}; TextLiteral combined"),
                        ),
                        construct_context_for_combine.clone(),
                        Ulid::new(),
                        actor_context_for_combine.clone(),
                        combined_text,
                    );
                    text_actor.subscribe()
                });

                ValueActor::new_arc(
                    ConstructInfo::new(
                        format!("PersistenceId: {persistence_id}"),
                        persistence,
                        format!("{span}; TextLiteral {{..}}"),
                    ),
                    actor_context,
                    TypedStream::infinite(flattened),
                    Some(persistence_id),
                )
            }
        }
        // Hardware types (parse-only for now - return error if used)
        static_expression::Expression::Bits { .. }
        | static_expression::Expression::Memory { .. }
        | static_expression::Expression::Bytes { .. } => {
            return Err("Hardware types (BITS, MEMORY, BYTES) are parse-only and cannot be evaluated yet".to_string());
        }
    };
    Ok(actor)
}

/// Get function definition for static function calls.
fn static_function_call_path_to_definition(
    path: &[&str],
    span: Span,
) -> Result<
    impl Fn(
        Arc<Vec<Arc<ValueActor>>>,
        ConstructId,
        PersistenceId,
        ConstructContext,
        ActorContext,
    ) -> Pin<Box<dyn Stream<Item = Value>>>
    + 'static,
    String,
> {
    let definition = match path {
        ["Math", "sum"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_math_sum(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["Text", "empty"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_text_empty(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["Text", "trim"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_text_trim(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["Text", "is_empty"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_text_is_empty(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["Text", "is_not_empty"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_text_is_not_empty(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["Bool", "not"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_bool_not(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["Bool", "toggle"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_bool_toggle(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["Bool", "or"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_bool_or(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["List", "count"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_list_count(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["List", "append"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_list_append(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["List", "latest"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_list_latest(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["List", "empty"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_list_empty(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["List", "not_empty"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_list_not_empty(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["Router", "route"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_router_route(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["Router", "go_to"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_router_go_to(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["Ulid", "generate"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_ulid_generate(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["Document", "new"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_document_new(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["Element", "stripe"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_element_stripe(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["Element", "button"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_element_button(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["Element", "text_input"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_element_text_input(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["Element", "checkbox"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_element_checkbox(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["Element", "label"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_element_label(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["Element", "paragraph"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_element_paragraph(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["Element", "link"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_element_link(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["Timer", "interval"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_timer_interval(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["Log", "info"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_log_info(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["Log", "error"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_log_error(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["Build", "succeed"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_build_succeed(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["Build", "fail"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_build_fail(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["Scene", "new"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_scene_new(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["Theme", "background_color"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_theme_background_color(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["Theme", "text_color"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_theme_text_color(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["Theme", "accent_color"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_theme_accent_color(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["File", "read_text"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_file_read_text(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["File", "write_text"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_file_write_text(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        ["Directory", "entries"] => |arguments, id, persistence_id, construct_context, actor_context| {
            api::function_directory_entries(arguments, id, persistence_id, construct_context, actor_context)
                .boxed_local()
        },
        _ => return Err(format!("Unknown function '{}(..)' in static context", path.join("/"))),
    };
    Ok(definition)
}

/// Match result containing bindings if match succeeded
type PatternBindings = HashMap<String, Arc<ValueActor>>;

/// Try to match a Value against a Pattern.
/// Returns Some(bindings) if match succeeds, None otherwise.
fn match_pattern(
    pattern: &static_expression::Pattern,
    value: &Value,
    _construct_context: &ConstructContext,
    actor_context: &ActorContext,
) -> Option<PatternBindings> {
    let mut bindings = HashMap::new();

    match pattern {
        static_expression::Pattern::WildCard => {
            // Wildcard matches everything
            Some(bindings)
        }
        static_expression::Pattern::Alias { name } => {
            // Bind the value to a new name
            let name_string = name.to_string();
            let value_actor = ValueActor::new_arc(
                ConstructInfo::new(
                    format!("pattern_binding_{name_string}"),
                    None,
                    format!("Pattern binding {name_string}"),
                ),
                actor_context.clone(),
                constant(value.clone()),
                None,
            );
            bindings.insert(name_string, value_actor);
            Some(bindings)
        }
        static_expression::Pattern::Literal(lit) => {
            // Match literal values
            match (lit, value) {
                (static_expression::Literal::Number(pattern_num), Value::Number(num, _)) => {
                    if (num.number() - pattern_num).abs() < f64::EPSILON {
                        Some(bindings)
                    } else {
                        None
                    }
                }
                (static_expression::Literal::Tag(pattern_tag), Value::Tag(tag, _)) => {
                    if tag.tag() == pattern_tag.as_ref() {
                        Some(bindings)
                    } else {
                        None
                    }
                }
                _ => None,
            }
        }
        static_expression::Pattern::TaggedObject { tag: pattern_tag, variables: pattern_vars } => {
            // Match tagged objects
            if let Value::TaggedObject(tagged_obj, _) = value {
                if tagged_obj.tag() == pattern_tag.as_ref() {
                    // Match each pattern variable against object variables
                    for pattern_var in pattern_vars {
                        let var_name = pattern_var.name.to_string();
                        // Find the variable in the object
                        if let Some(obj_var) = tagged_obj.variables().iter().find(|v| v.name() == var_name) {
                            if let Some(sub_pattern) = &pattern_var.value {
                                // TODO: Would need to get a value from obj_var.value_actor() to match
                                // For now, just bind the variable
                                bindings.insert(var_name, obj_var.value_actor());
                            } else {
                                // No sub-pattern, just bind the variable
                                bindings.insert(var_name, obj_var.value_actor());
                            }
                        } else {
                            return None; // Required variable not found
                        }
                    }
                    Some(bindings)
                } else {
                    None
                }
            } else {
                None
            }
        }
        static_expression::Pattern::Object { variables: pattern_vars } => {
            // Match objects
            if let Value::Object(obj, _) = value {
                for pattern_var in pattern_vars {
                    let var_name = pattern_var.name.to_string();
                    if let Some(obj_var) = obj.variables().iter().find(|v| v.name() == var_name) {
                        bindings.insert(var_name, obj_var.value_actor());
                    } else {
                        return None;
                    }
                }
                Some(bindings)
            } else {
                None
            }
        }
        static_expression::Pattern::List { items: _ } => {
            // TODO: Implement list pattern matching
            None
        }
        static_expression::Pattern::Map { entries: _ } => {
            // TODO: Implement map pattern matching
            None
        }
    }
}

