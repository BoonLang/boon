// @TODO remove
#![allow(dead_code)]

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::pin::pin;
use std::rc::Rc;
use std::sync::Arc;

use crate::parser;
use crate::parser::SourceCode;
use crate::parser::static_expression;
use super::evaluator::evaluate_static_expression;

use ulid::Ulid;

use zoon::IntoCowStr;
use zoon::future;
use zoon::futures_channel::{mpsc, oneshot};
use zoon::futures_util::select;
use zoon::futures_util::stream::{self, Stream, StreamExt};
use zoon::{Deserialize, DeserializeOwned, Serialize, serde, serde_json};
use zoon::{Task, TaskHandle};
use zoon::{WebStorage, local_storage};
use zoon::{eprintln, println};

const LOG_DROPS_AND_LOOP_ENDS: bool = false;

// --- constant ---

pub fn constant<T>(item: T) -> impl Stream<Item = T> {
    stream::once(future::ready(item)).chain(stream::once(future::pending()))
}

// --- VirtualFilesystem ---

/// In-memory filesystem for WASM environment
/// Stores files as path -> content mappings
#[derive(Clone, Default)]
pub struct VirtualFilesystem {
    files: Arc<std::cell::RefCell<HashMap<String, String>>>,
}

impl VirtualFilesystem {
    pub fn new() -> Self {
        Self {
            files: Arc::new(std::cell::RefCell::new(HashMap::new())),
        }
    }

    /// Create a VirtualFilesystem pre-populated with files
    pub fn with_files(files: HashMap<String, String>) -> Self {
        Self {
            files: Arc::new(std::cell::RefCell::new(files)),
        }
    }

    /// Read text content from a file
    pub fn read_text(&self, path: &str) -> Option<String> {
        let normalized = Self::normalize_path(path);
        self.files.borrow().get(&normalized).cloned()
    }

    /// Write text content to a file
    pub fn write_text(&self, path: &str, content: String) {
        let normalized = Self::normalize_path(path);
        self.files.borrow_mut().insert(normalized, content);
    }

    /// List entries in a directory
    pub fn list_directory(&self, path: &str) -> Vec<String> {
        let normalized = Self::normalize_path(path);
        let prefix = if normalized.is_empty() || normalized == "/" {
            String::new()
        } else if normalized.ends_with('/') {
            normalized.clone()
        } else {
            format!("{}/", normalized)
        };

        let files = self.files.borrow();
        let mut entries: Vec<String> = files
            .keys()
            .filter_map(|file_path| {
                if prefix.is_empty() {
                    // Root directory - get first path component
                    file_path.split('/').next().map(|s| s.to_string())
                } else if file_path.starts_with(&prefix) {
                    // Get the next path component after the prefix
                    let remainder = &file_path[prefix.len()..];
                    remainder.split('/').next().map(|s| s.to_string())
                } else {
                    None
                }
            })
            .collect();

        // Remove duplicates and sort
        entries.sort();
        entries.dedup();
        entries
    }

    /// Check if a file exists
    pub fn exists(&self, path: &str) -> bool {
        let normalized = Self::normalize_path(path);
        self.files.borrow().contains_key(&normalized)
    }

    /// Delete a file
    pub fn delete(&self, path: &str) -> bool {
        let normalized = Self::normalize_path(path);
        self.files.borrow_mut().remove(&normalized).is_some()
    }

    /// Normalize path by removing leading/trailing slashes and "./" prefixes
    fn normalize_path(path: &str) -> String {
        let path = path.trim();
        let path = path.strip_prefix("./").unwrap_or(path);
        let path = path.strip_prefix('/').unwrap_or(path);
        let path = path.strip_suffix('/').unwrap_or(path);
        path.to_string()
    }
}

// --- ConstructContext ---

#[derive(Clone)]
pub struct ConstructContext {
    pub construct_storage: Arc<ConstructStorage>,
    pub virtual_fs: VirtualFilesystem,
}

// --- ConstructStorage ---

pub struct ConstructStorage {
    state_inserter_sender: mpsc::UnboundedSender<(
        parser::PersistenceId,
        serde_json::Value,
        oneshot::Sender<()>,
    )>,
    state_getter_sender: mpsc::UnboundedSender<(
        parser::PersistenceId,
        oneshot::Sender<Option<serde_json::Value>>,
    )>,
    loop_task: TaskHandle,
}

// @TODO Replace LocalStorage with IndexedDB
// - https://crates.io/crates/indexed_db
// - https://developer.mozilla.org/en-US/docs/Web/API/IndexedDB_API
// - https://blog.openreplay.com/the-ultimate-guide-to-browser-side-storage/
impl ConstructStorage {
    pub fn new(states_local_storage_key: impl Into<Cow<'static, str>>) -> Self {
        let states_local_storage_key = states_local_storage_key.into();
        let (state_inserter_sender, mut state_inserter_receiver) = mpsc::unbounded();
        let (state_getter_sender, mut state_getter_receiver) = mpsc::unbounded();
        Self {
            state_inserter_sender,
            state_getter_sender,
            loop_task: Task::start_droppable(async move {
                let mut states = match local_storage().get(&states_local_storage_key) {
                    None => BTreeMap::<String, serde_json::Value>::new(),
                    Some(Ok(states)) => states,
                    Some(Err(error)) => panic!("Failed to deserialize states: {error:#}"),
                };
                loop {
                    select! {
                        (persistence_id, json_value, confirmation_sender) = state_inserter_receiver.select_next_some() => {
                            // @TODO remove `.to_string()` call when LocalStorage is replaced with IndexedDB (?)
                            states.insert(persistence_id.to_string(), json_value);
                            if let Err(error) = local_storage().insert(&states_local_storage_key, &states) {
                                eprintln!("Failed to save states: {error:#}");
                            }
                            if confirmation_sender.send(()).is_err() {
                                eprintln!("Failed to send save confirmation from construct storage");
                            }
                        },
                        (persistence_id, state_sender) = state_getter_receiver.select_next_some() => {
                            // @TODO Cheaper cloning? Replace get with remove?
                            let state = states.get(&persistence_id.to_string()).cloned();
                            if state_sender.send(state).is_err() {
                                eprintln!("Failed to send state from construct storage");
                            }
                        }
                    }
                }
            }),
        }
    }

    pub async fn save_state<T: Serialize>(&self, persistence_id: parser::PersistenceId, state: &T) {
        let json_value = match serde_json::to_value(state) {
            Ok(json_value) => json_value,
            Err(error) => {
                eprintln!("Failed to save state: {error:#}");
                return;
            }
        };
        let (confirmation_sender, confirmation_receiver) = oneshot::channel::<()>();
        if let Err(error) = self.state_inserter_sender.unbounded_send((
            persistence_id,
            json_value,
            confirmation_sender,
        )) {
            eprintln!("Failed to save state: {error:#}")
        }
        confirmation_receiver
            .await
            .expect("Failed to get confirmation from ConstructStorage")
    }

    // @TODO is &self enough?
    pub async fn load_state<T: DeserializeOwned>(
        self: Arc<Self>,
        persistence_id: parser::PersistenceId,
    ) -> Option<T> {
        let (state_sender, state_receiver) = oneshot::channel::<Option<serde_json::Value>>();
        if let Err(error) = self
            .state_getter_sender
            .unbounded_send((persistence_id, state_sender))
        {
            eprintln!("Failed to load state: {error:#}")
        }
        let json_value = state_receiver
            .await
            .expect("Failed to get state from ConstructStorage")?;
        match serde_json::from_value(json_value) {
            Ok(state) => Some(state),
            Err(error) => {
                panic!("Failed to load state: {error:#}");
            }
        }
    }
}

// --- ActorContext ---

#[derive(Default, Clone)]
pub struct ActorContext {
    pub output_valve_signal: Option<Arc<ActorOutputValveSignal>>,
    /// The piped value from `|>` operator.
    /// Set when evaluating `x |> expr` - the `x` becomes `piped` for `expr`.
    /// Used by function calls to prepend as first argument.
    /// Also used by THEN/WHEN/WHILE/LinkSetter to process the piped stream.
    pub piped: Option<Arc<ValueActor>>,
    /// The PASSED context - implicit context passed through function calls.
    /// Set when calling a function with `PASS: something` argument.
    /// Accessible inside the function via `PASSED` or `PASSED.field`.
    /// Propagates automatically through nested function calls.
    pub passed: Option<Arc<ValueActor>>,
    /// Function parameter bindings - maps parameter names to their values.
    /// Set when calling a user-defined function.
    /// e.g., `fn(param: x)` binds "param" -> x's ValueActor
    pub parameters: HashMap<String, Arc<ValueActor>>,
}

// --- ActorOutputValveSignal ---

pub struct ActorOutputValveSignal {
    impulse_sender_sender: mpsc::UnboundedSender<mpsc::UnboundedSender<()>>,
    loop_task: TaskHandle,
}

impl ActorOutputValveSignal {
    pub fn new(impulse_stream: impl Stream<Item = ()> + 'static) -> Self {
        let (impulse_sender_sender, mut impulse_sender_receiver) =
            mpsc::unbounded::<mpsc::UnboundedSender<()>>();
        Self {
            impulse_sender_sender,
            loop_task: Task::start_droppable(async move {
                let mut impulse_stream = pin!(impulse_stream.fuse());
                let mut impulse_senders = Vec::<mpsc::UnboundedSender<()>>::new();
                loop {
                    select! {
                        impulse = impulse_stream.next() => {
                            if impulse.is_none() { break };
                            impulse_senders.retain(|impulse_sender| {
                                if let Err(error) = impulse_sender.unbounded_send(()) {
                                    false
                                } else {
                                    true
                                }
                            });
                        }
                        impulse_sender = impulse_sender_receiver.select_next_some() => {
                            impulse_senders.push(impulse_sender);
                        }
                    }
                }
            }),
        }
    }

    pub fn subscribe(&self) -> impl Stream<Item = ()> {
        let (impulse_sender, impulse_receiver) = mpsc::unbounded();
        if let Err(error) = self.impulse_sender_sender.unbounded_send(impulse_sender) {
            eprintln!("Failed to subscribe to actor output valve signal: {error:#}");
        }
        impulse_receiver
    }
}

// --- ConstructInfo ---

pub struct ConstructInfo {
    id: ConstructId,
    // @TODO remove Option in the future once Persistence is created also inside API functions?
    persistence: Option<parser::Persistence>,
    description: Cow<'static, str>,
}

impl ConstructInfo {
    pub fn new(
        id: impl Into<ConstructId>,
        persistence: Option<parser::Persistence>,
        description: impl Into<Cow<'static, str>>,
    ) -> Self {
        Self {
            id: id.into(),
            persistence,
            description: description.into(),
        }
    }

    pub fn complete(self, r#type: ConstructType) -> ConstructInfoComplete {
        ConstructInfoComplete {
            r#type,
            id: self.id,
            persistence: self.persistence,
            description: self.description,
        }
    }
}

// --- ConstructInfoComplete ---

#[derive(Clone)]
pub struct ConstructInfoComplete {
    r#type: ConstructType,
    id: ConstructId,
    persistence: Option<parser::Persistence>,
    description: Cow<'static, str>,
}

impl ConstructInfoComplete {
    pub fn id(&self) -> ConstructId {
        self.id.clone()
    }
}

impl std::fmt::Display for ConstructInfoComplete {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "({:?} {:?} '{}')",
            self.r#type, self.id.ids, self.description
        )
    }
}

// --- ConstructType ---

#[derive(Debug, Clone, Copy)]
pub enum ConstructType {
    Variable,
    LinkVariable,
    VariableOrArgumentReference,
    FunctionCall,
    LatestCombinator,
    ThenCombinator,
    ValueActor,
    Object,
    TaggedObject,
    Text,
    Tag,
    Number,
    List,
}

// --- ConstructId ---

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(crate = "serde")]
pub struct ConstructId {
    ids: Arc<Vec<Cow<'static, str>>>,
}

impl ConstructId {
    pub fn new(id: impl IntoCowStr<'static>) -> Self {
        Self {
            ids: Arc::new(vec![id.into_cow_str()]),
        }
    }

    pub fn with_child_id(&self, child: impl IntoCowStr<'static>) -> Self {
        let mut ids = Vec::clone(&self.ids);
        ids.push(child.into_cow_str());
        Self { ids: Arc::new(ids) }
    }
}

impl<T: IntoCowStr<'static>> From<T> for ConstructId {
    fn from(id: T) -> Self {
        ConstructId::new(id)
    }
}

// --- Variable ---

pub struct Variable {
    construct_info: ConstructInfoComplete,
    name: Cow<'static, str>,
    value_actor: Arc<ValueActor>,
    link_value_sender: Option<mpsc::UnboundedSender<Value>>,
}

impl Variable {
    pub fn new(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        name: impl Into<Cow<'static, str>>,
        value_actor: Arc<ValueActor>,
    ) -> Self {
        Self {
            construct_info: construct_info.complete(ConstructType::Variable),
            name: name.into(),
            value_actor,
            link_value_sender: None,
        }
    }

    pub fn new_arc(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        name: impl Into<Cow<'static, str>>,
        value_actor: Arc<ValueActor>,
    ) -> Arc<Self> {
        Arc::new(Self::new(
            construct_info,
            construct_context,
            name,
            value_actor,
        ))
    }

    pub fn new_link_arc(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        name: impl Into<Cow<'static, str>>,
        actor_context: ActorContext,
    ) -> Arc<Self> {
        let ConstructInfo {
            id: actor_id,
            persistence,
            description: variable_description,
        } = construct_info;
        let construct_info = ConstructInfo::new(
            actor_id.with_child_id("wrapped Variable"),
            persistence,
            variable_description,
        );
        let actor_construct_info =
            ConstructInfo::new(actor_id, persistence, "Link variable value actor")
                .complete(ConstructType::ValueActor);
        let (link_value_sender, link_value_receiver) = mpsc::unbounded();
        let value_actor =
            ValueActor::new_internal(actor_construct_info, actor_context, link_value_receiver, ());
        Arc::new(Self {
            construct_info: construct_info.complete(ConstructType::LinkVariable),
            name: name.into(),
            value_actor: Arc::new(value_actor),
            link_value_sender: Some(link_value_sender),
        })
    }

    pub fn subscribe(&self) -> impl Stream<Item = Value> + use<> {
        self.value_actor.subscribe()
    }

    pub fn value_actor(&self) -> Arc<ValueActor> {
        self.value_actor.clone()
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn link_value_sender(&self) -> Option<mpsc::UnboundedSender<Value>> {
        self.link_value_sender.clone()
    }

    pub fn expect_link_value_sender(&self) -> mpsc::UnboundedSender<Value> {
        if let Some(link_value_sender) = self.link_value_sender.clone() {
            link_value_sender
        } else {
            panic!(
                "Failed to get expected link value sender from {}",
                self.construct_info
            );
        }
    }
}

impl Drop for Variable {
    fn drop(&mut self) {
        if LOG_DROPS_AND_LOOP_ENDS {
            println!("Dropped: {}", self.construct_info);
        }
    }
}

// --- VariableOrArgumentReference ---

pub struct VariableOrArgumentReference {}

impl VariableOrArgumentReference {
    pub fn new_arc_value_actor(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        actor_context: ActorContext,
        alias: static_expression::Alias,
        root_value_actor: impl Future<Output = Arc<ValueActor>> + 'static,
    ) -> Arc<ValueActor> {
        let construct_info = construct_info.complete(ConstructType::VariableOrArgumentReference);
        let mut skip_alias_parts = 0;
        let alias_parts = match alias {
            static_expression::Alias::WithoutPassed {
                parts,
                referenced_span: _,
            } => {
                skip_alias_parts = 1;
                parts
            }
            static_expression::Alias::WithPassed { extra_parts } => extra_parts,
        };
        let mut value_stream = stream::once(root_value_actor)
            .flat_map(|actor| actor.subscribe())
            .boxed_local();
        for alias_part in alias_parts.into_iter().skip(skip_alias_parts) {
            let alias_part = alias_part.to_string();
            value_stream = value_stream
                .flat_map(move |value| match value {
                    Value::Object(object, _) => object.expect_variable(&alias_part).subscribe(),
                    Value::TaggedObject(tagged_object, _) => {
                        tagged_object.expect_variable(&alias_part).subscribe()
                    }
                    other => panic!(
                        "Failed to get Object or TaggedObject to create VariableOrArgumentReference: The Value has a different type {}",
                        other.construct_info()
                    ),
                })
                .boxed_local();
        }
        Arc::new(ValueActor::new_internal(
            construct_info,
            actor_context,
            value_stream,
            (),
        ))
    }
}

// --- ReferenceConnector ---

pub struct ReferenceConnector {
    referenceable_inserter_sender: mpsc::UnboundedSender<(parser::Span, Arc<ValueActor>)>,
    referenceable_getter_sender:
        mpsc::UnboundedSender<(parser::Span, oneshot::Sender<Arc<ValueActor>>)>,
    loop_task: TaskHandle,
}

impl ReferenceConnector {
    pub fn new() -> Self {
        let (referenceable_inserter_sender, mut referenceable_inserter_receiver) =
            mpsc::unbounded();
        let (referenceable_getter_sender, mut referenceable_getter_receiver) = mpsc::unbounded();
        Self {
            referenceable_inserter_sender,
            referenceable_getter_sender,
            loop_task: Task::start_droppable(async move {
                let mut referenceables = HashMap::<parser::Span, Arc<ValueActor>>::new();
                let mut referenceable_senders =
                    HashMap::<parser::Span, Vec<oneshot::Sender<Arc<ValueActor>>>>::new();
                loop {
                    select! {
                        (span, actor) = referenceable_inserter_receiver.select_next_some() => {
                            if let Some(senders) = referenceable_senders.remove(&span) {
                                for sender in senders {
                                    if sender.send(actor.clone()).is_err() {
                                        eprintln!("Failed to send referenceable actor from reference connector");
                                    }
                                }
                            }
                            referenceables.insert(span, actor);
                        },
                        (span, referenceable_sender) = referenceable_getter_receiver.select_next_some() => {
                            if let Some(actor) = referenceables.get(&span) {
                                if referenceable_sender.send(actor.clone()).is_err() {
                                    eprintln!("Failed to send referenceable actor from reference connector");
                                }
                            } else {
                                referenceable_senders.entry(span).or_default().push(referenceable_sender);
                            }
                        }
                    }
                }
            }),
        }
    }

    pub fn register_referenceable(&self, span: parser::Span, actor: Arc<ValueActor>) {
        if let Err(error) = self
            .referenceable_inserter_sender
            .unbounded_send((span, actor))
        {
            eprintln!("Failed to register referenceable: {error:#}")
        }
    }

    // @TODO is &self enough?
    pub async fn referenceable(self: Arc<Self>, span: parser::Span) -> Arc<ValueActor> {
        let (referenceable_sender, referenceable_receiver) = oneshot::channel();
        if let Err(error) = self
            .referenceable_getter_sender
            .unbounded_send((span, referenceable_sender))
        {
            eprintln!("Failed to register referenceable: {error:#}")
        }
        referenceable_receiver
            .await
            .expect("Failed to get referenceable from ReferenceConnector")
    }
}

// --- LinkConnector ---

/// Connects LINK variables with their setters.
/// Similar to ReferenceConnector but stores mpsc senders for LINK variables.
pub struct LinkConnector {
    link_inserter_sender: mpsc::UnboundedSender<(parser::Span, mpsc::UnboundedSender<Value>)>,
    link_getter_sender:
        mpsc::UnboundedSender<(parser::Span, oneshot::Sender<mpsc::UnboundedSender<Value>>)>,
    loop_task: TaskHandle,
}

impl LinkConnector {
    pub fn new() -> Self {
        let (link_inserter_sender, mut link_inserter_receiver) = mpsc::unbounded();
        let (link_getter_sender, mut link_getter_receiver) = mpsc::unbounded();
        Self {
            link_inserter_sender,
            link_getter_sender,
            loop_task: Task::start_droppable(async move {
                let mut links = HashMap::<parser::Span, mpsc::UnboundedSender<Value>>::new();
                let mut link_senders =
                    HashMap::<parser::Span, Vec<oneshot::Sender<mpsc::UnboundedSender<Value>>>>::new();
                loop {
                    select! {
                        (span, sender) = link_inserter_receiver.select_next_some() => {
                            if let Some(senders) = link_senders.remove(&span) {
                                for link_sender in senders {
                                    if link_sender.send(sender.clone()).is_err() {
                                        eprintln!("Failed to send link sender from link connector");
                                    }
                                }
                            }
                            links.insert(span, sender);
                        },
                        (span, link_sender) = link_getter_receiver.select_next_some() => {
                            if let Some(sender) = links.get(&span) {
                                if link_sender.send(sender.clone()).is_err() {
                                    eprintln!("Failed to send link sender from link connector");
                                }
                            } else {
                                link_senders.entry(span).or_default().push(link_sender);
                            }
                        }
                    }
                }
            }),
        }
    }

    /// Register a LINK variable's sender with its span.
    pub fn register_link(&self, span: parser::Span, sender: mpsc::UnboundedSender<Value>) {
        if let Err(error) = self
            .link_inserter_sender
            .unbounded_send((span, sender))
        {
            eprintln!("Failed to register link: {error:#}")
        }
    }

    /// Get a LINK variable's sender by its span.
    pub async fn link_sender(self: Arc<Self>, span: parser::Span) -> mpsc::UnboundedSender<Value> {
        let (link_sender, link_receiver) = oneshot::channel();
        if let Err(error) = self
            .link_getter_sender
            .unbounded_send((span, link_sender))
        {
            eprintln!("Failed to get link sender: {error:#}")
        }
        link_receiver
            .await
            .expect("Failed to get link sender from LinkConnector")
    }
}

// --- FunctionCall ---

pub struct FunctionCall {}

impl FunctionCall {
    pub fn new_arc_value_actor<FR: Stream<Item = Value> + 'static>(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        actor_context: ActorContext,
        definition: impl Fn(
            Arc<Vec<Arc<ValueActor>>>,
            ConstructId,
            parser::PersistenceId,
            ConstructContext,
            ActorContext,
        ) -> FR
        + 'static,
        arguments: impl Into<Vec<Arc<ValueActor>>>,
    ) -> Arc<ValueActor> {
        use zoon::futures_util::stream::StreamExt;

        let construct_info = construct_info.complete(ConstructType::FunctionCall);
        let arguments = Arc::new(arguments.into());

        // FLUSHED bypass logic: If any argument emits a FLUSHED value,
        // bypass the function and emit that FLUSHED value immediately.
        // This implements fail-fast error handling per FLUSH.md specification.
        //
        // Implementation:
        // 1. Subscribe to all arguments and merge their streams
        // 2. If any value is FLUSHED, emit it and don't call the function for that cycle
        // 3. If all values are non-FLUSHED, proceed with normal function processing
        //
        // For simplicity, we use a hybrid approach:
        // - Call the function normally
        // - Wrap the result stream to also listen to arguments for FLUSHED values
        // - If any argument emits FLUSHED before/during function processing, bypass

        let value_stream = definition(
            arguments.clone(),
            construct_info.id(),
            construct_info
                .persistence
                .expect("Failed to get FunctionCall Persistence")
                .id,
            construct_context,
            actor_context.clone(),
        );

        // Create a stream that monitors arguments for FLUSHED values
        // and bypasses the function when FLUSHED is detected
        let arguments_for_flushed = arguments.clone();
        let flushed_bypass_stream = if arguments_for_flushed.is_empty() {
            // No arguments - no FLUSHED bypass needed
            zoon::futures_util::stream::empty().boxed_local()
        } else {
            // Subscribe to all arguments and filter for FLUSHED values only
            let flushed_streams: Vec<_> = arguments_for_flushed
                .iter()
                .map(|arg| arg.subscribe().filter(|v| {
                    let is_flushed = v.is_flushed();
                    std::future::ready(is_flushed)
                }))
                .collect();
            zoon::futures_util::stream::select_all(flushed_streams).boxed_local()
        };

        // Select between normal function output and FLUSHED bypass
        // FLUSHED values from arguments take priority
        let combined_stream = zoon::futures_util::stream::select(
            flushed_bypass_stream,
            value_stream.map(|v| {
                // If the function itself produces FLUSHED, pass it through
                v
            }),
        );

        Arc::new(ValueActor::new_internal(
            construct_info,
            actor_context,
            combined_stream,
            arguments,
        ))
    }
}

// --- LatestCombinator ---

pub struct LatestCombinator {}

impl LatestCombinator {
    pub fn new_arc_value_actor(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        actor_context: ActorContext,
        inputs: impl Into<Vec<Arc<ValueActor>>>,
    ) -> Arc<ValueActor> {
        #[derive(Default, Clone, Serialize, Deserialize)]
        #[serde(crate = "serde")]
        struct State {
            input_idempotency_keys: BTreeMap<usize, ValueIdempotencyKey>,
        }

        let construct_info = construct_info.complete(ConstructType::LatestCombinator);
        let inputs: Vec<Arc<ValueActor>> = inputs.into();
        let persistent_id = construct_info
            .persistence
            .expect("Failed to get Persistence in LatestCombinator")
            .id;
        let storage = construct_context.construct_storage.clone();

        let value_stream =
            stream::select_all(inputs.iter().enumerate().map(|(index, value_actor)| {
                value_actor.subscribe().map(move |value| (index, value))
            }))
            .scan(true, {
                let storage = storage.clone();
                move |first_run, (index, value)| {
                    let storage = storage.clone();
                    let previous_first_run = *first_run;
                    *first_run = false;
                    async move {
                        if previous_first_run {
                            Some((
                                storage.clone().load_state::<State>(persistent_id).await,
                                index,
                                value,
                            ))
                        } else {
                            Some((None, index, value))
                        }
                    }
                }
            })
            .scan(State::default(), move |state, (new_state, index, value)| {
                if let Some(new_state) = new_state {
                    *state = new_state;
                }
                let idempotency_key = value.idempotency_key();
                let skip_value = state.input_idempotency_keys.get(&index).is_some_and(
                    |previous_idempotency_key| *previous_idempotency_key == idempotency_key,
                );
                if !skip_value {
                    state.input_idempotency_keys.insert(index, idempotency_key);
                }
                // @TODO Refactor to get rid of the `clone` call. Use async closure?
                let state = state.clone();
                let storage = storage.clone();
                async move {
                    if skip_value {
                        Some(None)
                    } else {
                        storage.save_state(persistent_id, &state).await;
                        Some(Some(value))
                    }
                }
            })
            .filter_map(future::ready);

        Arc::new(ValueActor::new_internal(
            construct_info,
            actor_context,
            value_stream,
            inputs,
        ))
    }
}

// --- ThenCombinator ---

pub struct ThenCombinator {}

impl ThenCombinator {
    pub fn new_arc_value_actor(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        actor_context: ActorContext,
        observed: Arc<ValueActor>,
        impulse_sender: mpsc::UnboundedSender<()>,
        body: Arc<ValueActor>,
    ) -> Arc<ValueActor> {
        #[derive(Default, Copy, Clone, Serialize, Deserialize)]
        #[serde(crate = "serde")]
        struct State {
            observed_idempotency_key: Option<ValueIdempotencyKey>,
        }

        let construct_info = construct_info.complete(ConstructType::ThenCombinator);
        let persistent_id = construct_info
            .persistence
            .expect("Failed to get Persistence in ThenCombinator")
            .id;
        let storage = construct_context.construct_storage.clone();

        let send_impulse_task = Task::start_droppable(
            observed
                .subscribe()
                .scan(true, {
                    let storage = storage.clone();
                    move |first_run, value| {
                        let storage = storage.clone();
                        let previous_first_run = *first_run;
                        *first_run = false;
                        async move {
                            if previous_first_run {
                                Some((
                                    storage.clone().load_state::<State>(persistent_id).await,
                                    value,
                                ))
                            } else {
                                Some((None, value))
                            }
                        }
                    }
                })
                .scan(State::default(), move |state, (new_state, value)| {
                    if let Some(new_state) = new_state {
                        *state = new_state;
                    }
                    let idempotency_key = value.idempotency_key();
                    let skip_value = state
                        .observed_idempotency_key
                        .is_some_and(|key| key == idempotency_key);
                    if !skip_value {
                        state.observed_idempotency_key = Some(idempotency_key);
                    }
                    let state = *state;
                    let storage = storage.clone();
                    async move {
                        if skip_value {
                            Some(None)
                        } else {
                            storage.save_state(persistent_id, &state).await;
                            Some(Some(value))
                        }
                    }
                })
                .filter_map(future::ready)
                .for_each({
                    let construct_info = construct_info.clone();
                    move |_| {
                        if let Err(error) = impulse_sender.unbounded_send(()) {
                            eprintln!("Failed to send impulse in {construct_info}: {error:#}")
                        }
                        future::ready(())
                    }
                }),
        );
        let value_stream = body.subscribe().map(|mut value| {
            value.set_idempotency_key(ValueIdempotencyKey::new());
            value
        });
        Arc::new(ValueActor::new_internal(
            construct_info,
            actor_context,
            value_stream,
            (observed, send_impulse_task, body),
        ))
    }
}

// --- BinaryOperatorCombinator ---

/// Combines two value streams using a binary operation.
/// Used for comparators (==, <, >, etc.) and arithmetic (+, -, *, /).
pub struct BinaryOperatorCombinator {}

impl BinaryOperatorCombinator {
    /// Creates a ValueActor that combines two operands using the given operation.
    /// The operation receives both values and returns a new Value.
    pub fn new_arc_value_actor<F>(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        actor_context: ActorContext,
        operand_a: Arc<ValueActor>,
        operand_b: Arc<ValueActor>,
        operation: F,
    ) -> Arc<ValueActor>
    where
        F: Fn(Value, Value, ConstructContext, ValueIdempotencyKey) -> Value + 'static,
    {
        let construct_info = construct_info.complete(ConstructType::ValueActor);

        // Merge both operand streams, tracking which operand changed
        let value_stream = stream::select_all([
            operand_a.subscribe().map(|v| (0usize, v)).boxed_local(),
            operand_b.subscribe().map(|v| (1usize, v)).boxed_local(),
        ])
        .scan(
            (None::<Value>, None::<Value>),
            move |(latest_a, latest_b), (index, value)| {
                match index {
                    0 => *latest_a = Some(value),
                    1 => *latest_b = Some(value),
                    _ => unreachable!(),
                }
                let result = match (latest_a.clone(), latest_b.clone()) {
                    (Some(a), Some(b)) => Some((a, b)),
                    _ => None,
                };
                future::ready(Some(result))
            },
        )
        .filter_map(future::ready)
        .map({
            let construct_context = construct_context.clone();
            move |(a, b)| {
                let idempotency_key = ValueIdempotencyKey::new();
                operation(a, b, construct_context.clone(), idempotency_key)
            }
        });

        Arc::new(ValueActor::new_internal(
            construct_info,
            actor_context,
            value_stream,
            (operand_a, operand_b),
        ))
    }
}

// --- ComparatorCombinator ---

/// Helper for creating comparison combinators.
pub struct ComparatorCombinator {}

impl ComparatorCombinator {
    pub fn new_equal(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        actor_context: ActorContext,
        operand_a: Arc<ValueActor>,
        operand_b: Arc<ValueActor>,
    ) -> Arc<ValueActor> {
        BinaryOperatorCombinator::new_arc_value_actor(
            construct_info,
            construct_context.clone(),
            actor_context,
            operand_a,
            operand_b,
            |a, b, ctx, key| {
                let result = values_equal(&a, &b);
                Tag::new_value(
                    ConstructInfo::new("comparator_result", None, "== result"),
                    ctx,
                    key,
                    if result { "True" } else { "False" },
                )
            },
        )
    }

    pub fn new_not_equal(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        actor_context: ActorContext,
        operand_a: Arc<ValueActor>,
        operand_b: Arc<ValueActor>,
    ) -> Arc<ValueActor> {
        BinaryOperatorCombinator::new_arc_value_actor(
            construct_info,
            construct_context.clone(),
            actor_context,
            operand_a,
            operand_b,
            |a, b, ctx, key| {
                let result = !values_equal(&a, &b);
                Tag::new_value(
                    ConstructInfo::new("comparator_result", None, "=/= result"),
                    ctx,
                    key,
                    if result { "True" } else { "False" },
                )
            },
        )
    }

    pub fn new_greater(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        actor_context: ActorContext,
        operand_a: Arc<ValueActor>,
        operand_b: Arc<ValueActor>,
    ) -> Arc<ValueActor> {
        BinaryOperatorCombinator::new_arc_value_actor(
            construct_info,
            construct_context.clone(),
            actor_context,
            operand_a,
            operand_b,
            |a, b, ctx, key| {
                let result = compare_values(&a, &b).map(|o| o.is_gt()).unwrap_or(false);
                Tag::new_value(
                    ConstructInfo::new("comparator_result", None, "> result"),
                    ctx,
                    key,
                    if result { "True" } else { "False" },
                )
            },
        )
    }

    pub fn new_greater_or_equal(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        actor_context: ActorContext,
        operand_a: Arc<ValueActor>,
        operand_b: Arc<ValueActor>,
    ) -> Arc<ValueActor> {
        BinaryOperatorCombinator::new_arc_value_actor(
            construct_info,
            construct_context.clone(),
            actor_context,
            operand_a,
            operand_b,
            |a, b, ctx, key| {
                let result = compare_values(&a, &b).map(|o| o.is_ge()).unwrap_or(false);
                Tag::new_value(
                    ConstructInfo::new("comparator_result", None, ">= result"),
                    ctx,
                    key,
                    if result { "True" } else { "False" },
                )
            },
        )
    }

    pub fn new_less(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        actor_context: ActorContext,
        operand_a: Arc<ValueActor>,
        operand_b: Arc<ValueActor>,
    ) -> Arc<ValueActor> {
        BinaryOperatorCombinator::new_arc_value_actor(
            construct_info,
            construct_context.clone(),
            actor_context,
            operand_a,
            operand_b,
            |a, b, ctx, key| {
                let result = compare_values(&a, &b).map(|o| o.is_lt()).unwrap_or(false);
                Tag::new_value(
                    ConstructInfo::new("comparator_result", None, "< result"),
                    ctx,
                    key,
                    if result { "True" } else { "False" },
                )
            },
        )
    }

    pub fn new_less_or_equal(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        actor_context: ActorContext,
        operand_a: Arc<ValueActor>,
        operand_b: Arc<ValueActor>,
    ) -> Arc<ValueActor> {
        BinaryOperatorCombinator::new_arc_value_actor(
            construct_info,
            construct_context.clone(),
            actor_context,
            operand_a,
            operand_b,
            |a, b, ctx, key| {
                let result = compare_values(&a, &b).map(|o| o.is_le()).unwrap_or(false);
                Tag::new_value(
                    ConstructInfo::new("comparator_result", None, "<= result"),
                    ctx,
                    key,
                    if result { "True" } else { "False" },
                )
            },
        )
    }
}

/// Compare two Values for equality.
fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(n1, _), Value::Number(n2, _)) => n1.number() == n2.number(),
        (Value::Text(t1, _), Value::Text(t2, _)) => t1.text() == t2.text(),
        (Value::Tag(tag1, _), Value::Tag(tag2, _)) => tag1.tag() == tag2.tag(),
        _ => false, // Different types are not equal
    }
}

/// Compare two Values for ordering. Returns None if types are incompatible.
fn compare_values(a: &Value, b: &Value) -> Option<std::cmp::Ordering> {
    match (a, b) {
        (Value::Number(n1, _), Value::Number(n2, _)) => n1.number().partial_cmp(&n2.number()),
        (Value::Text(t1, _), Value::Text(t2, _)) => Some(t1.text().cmp(t2.text())),
        _ => None,
    }
}

// --- ArithmeticCombinator ---

/// Helper for creating arithmetic combinators.
pub struct ArithmeticCombinator {}

impl ArithmeticCombinator {
    pub fn new_add(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        actor_context: ActorContext,
        operand_a: Arc<ValueActor>,
        operand_b: Arc<ValueActor>,
    ) -> Arc<ValueActor> {
        BinaryOperatorCombinator::new_arc_value_actor(
            construct_info,
            construct_context.clone(),
            actor_context,
            operand_a,
            operand_b,
            |a, b, ctx, key| {
                let result = get_number(&a) + get_number(&b);
                Number::new_value(
                    ConstructInfo::new("arithmetic_result", None, "+ result"),
                    ctx,
                    key,
                    result,
                )
            },
        )
    }

    pub fn new_subtract(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        actor_context: ActorContext,
        operand_a: Arc<ValueActor>,
        operand_b: Arc<ValueActor>,
    ) -> Arc<ValueActor> {
        BinaryOperatorCombinator::new_arc_value_actor(
            construct_info,
            construct_context.clone(),
            actor_context,
            operand_a,
            operand_b,
            |a, b, ctx, key| {
                let result = get_number(&a) - get_number(&b);
                Number::new_value(
                    ConstructInfo::new("arithmetic_result", None, "- result"),
                    ctx,
                    key,
                    result,
                )
            },
        )
    }

    pub fn new_multiply(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        actor_context: ActorContext,
        operand_a: Arc<ValueActor>,
        operand_b: Arc<ValueActor>,
    ) -> Arc<ValueActor> {
        BinaryOperatorCombinator::new_arc_value_actor(
            construct_info,
            construct_context.clone(),
            actor_context,
            operand_a,
            operand_b,
            |a, b, ctx, key| {
                let result = get_number(&a) * get_number(&b);
                Number::new_value(
                    ConstructInfo::new("arithmetic_result", None, "* result"),
                    ctx,
                    key,
                    result,
                )
            },
        )
    }

    pub fn new_divide(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        actor_context: ActorContext,
        operand_a: Arc<ValueActor>,
        operand_b: Arc<ValueActor>,
    ) -> Arc<ValueActor> {
        BinaryOperatorCombinator::new_arc_value_actor(
            construct_info,
            construct_context.clone(),
            actor_context,
            operand_a,
            operand_b,
            |a, b, ctx, key| {
                let result = get_number(&a) / get_number(&b);
                Number::new_value(
                    ConstructInfo::new("arithmetic_result", None, "/ result"),
                    ctx,
                    key,
                    result,
                )
            },
        )
    }
}

/// Extract a number from a Value, panicking if not a Number.
fn get_number(value: &Value) -> f64 {
    match value {
        Value::Number(n, _) => n.number(),
        other => panic!(
            "Expected Number for arithmetic operation, got {}",
            other.construct_info()
        ),
    }
}

// --- WhenCombinator ---

/// Pattern matching combinator for WHEN expressions.
/// Matches an input value against patterns and returns the first matching arm's result.
pub struct WhenCombinator {}

/// A compiled arm for WHEN matching.
pub struct CompiledArm {
    pub matcher: Box<dyn Fn(&Value) -> bool + Send + Sync>,
    pub body: Arc<ValueActor>,
}

impl WhenCombinator {
    /// Creates a ValueActor for WHEN pattern matching.
    /// The arms are tried in order; first matching pattern's body is returned.
    pub fn new_arc_value_actor(
        construct_info: ConstructInfo,
        _construct_context: ConstructContext,
        actor_context: ActorContext,
        input: Arc<ValueActor>,
        arms: Vec<CompiledArm>,
    ) -> Arc<ValueActor> {
        let construct_info = construct_info.complete(ConstructType::ValueActor);
        let arms = Arc::new(arms);

        // For each input value, find the matching arm and emit its body's value
        let value_stream = input
            .subscribe()
            .flat_map({
                let arms = arms.clone();
                move |input_value| {
                    // Find the first matching arm
                    let matched_arm = arms
                        .iter()
                        .find(|arm| (arm.matcher)(&input_value));

                    if let Some(arm) = matched_arm {
                        // Subscribe to the matching arm's body
                        arm.body.subscribe().boxed_local()
                    } else {
                        // No match - this shouldn't happen if we have a wildcard default
                        // Return an empty stream
                        stream::empty().boxed_local()
                    }
                }
            });

        Arc::new(ValueActor::new_internal(
            construct_info,
            actor_context,
            value_stream,
            (input, arms),
        ))
    }
}

/// Create a matcher function for a pattern.
pub fn pattern_to_matcher(pattern: &crate::parser::Pattern) -> Box<dyn Fn(&Value) -> bool + Send + Sync> {
    match pattern {
        crate::parser::Pattern::WildCard => {
            Box::new(|_| true)
        }
        crate::parser::Pattern::Literal(lit) => {
            match lit {
                crate::parser::Literal::Number(n) => {
                    let n = *n;
                    Box::new(move |v| {
                        matches!(v, Value::Number(num, _) if num.number() == n)
                    })
                }
                crate::parser::Literal::Text(t) => {
                    let t = t.to_string();
                    Box::new(move |v| {
                        matches!(v, Value::Text(text, _) if text.text() == t)
                    })
                }
                crate::parser::Literal::Tag(tag) => {
                    let tag = tag.to_string();
                    Box::new(move |v| {
                        match v {
                            Value::Tag(t, _) => t.tag() == tag,
                            Value::TaggedObject(to, _) => to.tag == tag,
                            _ => false,
                        }
                    })
                }
            }
        }
        crate::parser::Pattern::Alias { name: _ } => {
            // Alias just binds the value, so it always matches
            // (Variable binding will be handled separately)
            Box::new(|_| true)
        }
        crate::parser::Pattern::TaggedObject { tag, variables: _ } => {
            let tag = tag.to_string();
            Box::new(move |v| {
                matches!(v, Value::TaggedObject(to, _) if to.tag == tag)
            })
        }
        crate::parser::Pattern::Object { variables: _ } => {
            // Object pattern matches any object
            Box::new(|v| matches!(v, Value::Object(_, _)))
        }
        crate::parser::Pattern::List { items: _ } => {
            // List pattern matches any list (detailed matching would check items)
            Box::new(|v| matches!(v, Value::List(_, _)))
        }
        crate::parser::Pattern::Map { entries: _ } => {
            // Map pattern - not fully supported yet
            Box::new(|_| false)
        }
    }
}

// --- ValueActor ---

pub struct ValueActor {
    construct_info: Arc<ConstructInfoComplete>,
    loop_task: TaskHandle,
    value_sender_sender: mpsc::UnboundedSender<mpsc::UnboundedSender<Value>>,
}

impl ValueActor {
    pub fn new(
        construct_info: ConstructInfo,
        actor_context: ActorContext,
        value_stream: impl Stream<Item = Value> + 'static,
    ) -> Self {
        let construct_info = construct_info.complete(ConstructType::ValueActor);
        Self::new_internal(construct_info, actor_context, value_stream, ())
    }

    fn new_internal<EOD: 'static>(
        construct_info: ConstructInfoComplete,
        actor_context: ActorContext,
        value_stream: impl Stream<Item = Value> + 'static,
        extra_owned_data: EOD,
    ) -> Self {
        let construct_info = Arc::new(construct_info);
        let (value_sender_sender, mut value_sender_receiver) =
            mpsc::unbounded::<mpsc::UnboundedSender<Value>>();
        let loop_task = Task::start_droppable({
            let construct_info = construct_info.clone();
            let output_valve_signal = actor_context.output_valve_signal;
            async move {
                let output_valve_signal = output_valve_signal;
                let mut output_valve_impulse_stream =
                    if let Some(output_valve_signal) = &output_valve_signal {
                        output_valve_signal.subscribe().left_stream()
                    } else {
                        stream::pending().right_stream()
                    }
                    .fuse();
                let mut value_stream = pin!(value_stream.fuse());
                let mut value = None;
                let mut value_senders = Vec::<mpsc::UnboundedSender<Value>>::new();
                loop {
                    select! {
                        new_value = value_stream.next() => {
                            let Some(new_value) = new_value else { break };
                            if output_valve_signal.is_none() {
                                value_senders.retain(|value_sender| {
                                    if let Err(error) = value_sender.unbounded_send(new_value.clone()) {
                                        eprintln!("Failed to send new {construct_info} value to subscriber: {error:#}");
                                        false
                                    } else {
                                        true
                                    }
                                });
                            }
                            value = Some(new_value);
                        }
                        value_sender = value_sender_receiver.select_next_some() => {
                            if output_valve_signal.is_none() {
                                if let Some(value) = value.as_ref() {
                                    if let Err(error) = value_sender.unbounded_send(value.clone()) {
                                        eprintln!("Failed to send {construct_info} value to subscriber: {error:#}");
                                    } else {
                                        value_senders.push(value_sender);
                                    }
                                } else {
                                    value_senders.push(value_sender);
                                }
                            } else {
                                value_senders.push(value_sender);
                            }
                        }
                        impulse = output_valve_impulse_stream.next() => {
                            if impulse.is_none() {
                                break
                            }
                            if let Some(value) = value.as_ref() {
                                value_senders.retain(|value_sender| {
                                    if let Err(error) = value_sender.unbounded_send(value.clone()) {
                                        eprintln!("Failed to send {construct_info} value to subscriber on impulse: {error:#}");
                                        false
                                    } else {
                                        true
                                    }
                                });
                            }
                        }
                    }
                }
                if LOG_DROPS_AND_LOOP_ENDS {
                    println!("Loop ended {construct_info}");
                }
                drop(extra_owned_data);
            }
        });
        Self {
            construct_info,
            loop_task,
            value_sender_sender,
        }
    }

    pub fn new_arc(
        construct_info: ConstructInfo,
        actor_context: ActorContext,
        value_stream: impl Stream<Item = Value> + 'static,
    ) -> Arc<Self> {
        Arc::new(Self::new(construct_info, actor_context, value_stream))
    }

    pub fn subscribe(&self) -> impl Stream<Item = Value> + use<> {
        let (value_sender, value_receiver) = mpsc::unbounded();
        if let Err(error) = self.value_sender_sender.unbounded_send(value_sender) {
            eprintln!("Failed to subscribe to {}: {error:#}", self.construct_info);
        }
        value_receiver
    }
}

impl Drop for ValueActor {
    fn drop(&mut self) {
        if LOG_DROPS_AND_LOOP_ENDS {
            println!("Dropped: {}", self.construct_info);
        }
    }
}

// --- ValueIdempotencyKey ---

pub type ValueIdempotencyKey = Ulid;

// --- ValueMetadata ---

#[derive(Clone, Copy)]
pub struct ValueMetadata {
    pub idempotency_key: ValueIdempotencyKey,
}

// --- Value ---

#[derive(Clone)]
pub enum Value {
    Object(Arc<Object>, ValueMetadata),
    TaggedObject(Arc<TaggedObject>, ValueMetadata),
    Text(Arc<Text>, ValueMetadata),
    Tag(Arc<Tag>, ValueMetadata),
    Number(Arc<Number>, ValueMetadata),
    List(Arc<List>, ValueMetadata),
    /// FLUSHED[value] - internal wrapper for fail-fast error handling
    /// Created by FLUSH { value }, propagates transparently through pipelines,
    /// and unwraps at boundaries (variable bindings, function returns, BLOCK returns)
    Flushed(Box<Value>, ValueMetadata),
}

impl Value {
    pub fn construct_info(&self) -> &ConstructInfoComplete {
        match self {
            Self::Object(object, _) => &object.construct_info,
            Self::TaggedObject(tagged_object, _) => &tagged_object.construct_info,
            Self::Text(text, _) => &text.construct_info,
            Self::Tag(tag, _) => &tag.construct_info,
            Self::Number(number, _) => &number.construct_info,
            Self::List(list, _) => &list.construct_info,
            Self::Flushed(inner, _) => inner.construct_info(),
        }
    }

    pub fn metadata(&self) -> ValueMetadata {
        match self {
            Self::Object(_, metadata) => *metadata,
            Self::TaggedObject(_, metadata) => *metadata,
            Self::Text(_, metadata) => *metadata,
            Self::Tag(_, metadata) => *metadata,
            Self::Number(_, metadata) => *metadata,
            Self::List(_, metadata) => *metadata,
            Self::Flushed(_, metadata) => *metadata,
        }
    }
    pub fn metadata_mut(&mut self) -> &mut ValueMetadata {
        match self {
            Self::Object(_, metadata) => metadata,
            Self::TaggedObject(_, metadata) => metadata,
            Self::Text(_, metadata) => metadata,
            Self::Tag(_, metadata) => metadata,
            Self::Number(_, metadata) => metadata,
            Self::List(_, metadata) => metadata,
            Self::Flushed(_, metadata) => metadata,
        }
    }

    /// Check if this value is a FLUSHED wrapper
    pub fn is_flushed(&self) -> bool {
        matches!(self, Self::Flushed(_, _))
    }

    /// Unwrap FLUSHED to get the inner value (for boundary unwrapping)
    /// Returns self unchanged if not FLUSHED
    pub fn unwrap_flushed(self) -> Value {
        match self {
            Self::Flushed(inner, _) => *inner,
            other => other,
        }
    }

    /// Create a FLUSHED wrapper around this value
    pub fn into_flushed(self) -> Value {
        let metadata = ValueMetadata {
            idempotency_key: Ulid::new(),
        };
        Value::Flushed(Box::new(self), metadata)
    }

    pub fn idempotency_key(&self) -> ValueIdempotencyKey {
        self.metadata().idempotency_key
    }

    pub fn set_idempotency_key(&mut self, key: ValueIdempotencyKey) {
        self.metadata_mut().idempotency_key = key;
    }

    pub fn expect_object(self) -> Arc<Object> {
        let Self::Object(object, _) = self else {
            panic!(
                "Failed to get expected Object: The Value has a different type {}",
                self.construct_info()
            )
        };
        object
    }

    pub fn expect_tagged_object(self, tag: &str) -> Arc<TaggedObject> {
        let Self::TaggedObject(tagged_object, _) = self else {
            panic!("Failed to get expected TaggedObject: The Value has a different type")
        };
        let found_tag = &tagged_object.tag;
        if found_tag != tag {
            panic!(
                "Failed to get expected TaggedObject: Expected tag: '{tag}', found tag: '{found_tag}'"
            )
        }
        tagged_object
    }

    pub fn expect_text(self) -> Arc<Text> {
        let Self::Text(text, _) = self else {
            panic!("Failed to get expected Text: The Value has a different type")
        };
        text
    }

    pub fn expect_tag(self) -> Arc<Tag> {
        let Self::Tag(tag, _) = self else {
            panic!("Failed to get expected Tag: The Value has a different type")
        };
        tag
    }

    pub fn expect_number(self) -> Arc<Number> {
        let Self::Number(number, _) = self else {
            panic!("Failed to get expected Number: The Value has a different type")
        };
        number
    }

    pub fn expect_list(self) -> Arc<List> {
        let Self::List(list, _) = self else {
            panic!("Failed to get expected List: The Value has a different type")
        };
        list
    }

    /// Serializes this Value to a JSON representation.
    /// This is an async function because it needs to subscribe to streaming values.
    pub async fn to_json(&self) -> serde_json::Value {
        match self {
            Value::Text(text, _) => {
                serde_json::Value::String(text.text().to_string())
            }
            Value::Tag(tag, _) => {
                let mut obj = serde_json::Map::new();
                obj.insert("_tag".to_string(), serde_json::Value::String(tag.tag().to_string()));
                serde_json::Value::Object(obj)
            }
            Value::Number(number, _) => {
                serde_json::json!(number.number())
            }
            Value::Object(object, _) => {
                let mut obj = serde_json::Map::new();
                for variable in object.variables() {
                    let value = variable.value_actor().subscribe().next().await;
                    if let Some(value) = value {
                        let json_value = Box::pin(value.to_json()).await;
                        obj.insert(variable.name().to_string(), json_value);
                    }
                }
                serde_json::Value::Object(obj)
            }
            Value::TaggedObject(tagged_object, _) => {
                let mut obj = serde_json::Map::new();
                obj.insert("_tag".to_string(), serde_json::Value::String(tagged_object.tag().to_string()));
                for variable in tagged_object.variables() {
                    let value = variable.value_actor().subscribe().next().await;
                    if let Some(value) = value {
                        let json_value = Box::pin(value.to_json()).await;
                        obj.insert(variable.name().to_string(), json_value);
                    }
                }
                serde_json::Value::Object(obj)
            }
            Value::List(list, _) => {
                let first_change = list.subscribe().next().await;
                if let Some(ListChange::Replace { items }) = first_change {
                    let mut json_items = Vec::new();
                    for item in items {
                        let value = item.subscribe().next().await;
                        if let Some(value) = value {
                            let json_value = Box::pin(value.to_json()).await;
                            json_items.push(json_value);
                        }
                    }
                    serde_json::Value::Array(json_items)
                } else {
                    serde_json::Value::Array(Vec::new())
                }
            }
            Value::Flushed(inner, _) => {
                // Serialize FLUSHED values with a wrapper to preserve the flushed state
                let mut obj = serde_json::Map::new();
                obj.insert("_flushed".to_string(), serde_json::Value::Bool(true));
                obj.insert("value".to_string(), Box::pin(inner.to_json()).await);
                serde_json::Value::Object(obj)
            }
        }
    }

    /// Deserializes a JSON value into a Value (not wrapped in ValueActor).
    /// This is used internally by `value_actor_from_json`.
    pub fn from_json(
        json: &serde_json::Value,
        construct_id: ConstructId,
        construct_context: ConstructContext,
        idempotency_key: ValueIdempotencyKey,
        actor_context: ActorContext,
    ) -> Value {
        match json {
            serde_json::Value::String(s) => {
                let construct_info = ConstructInfo::new(
                    construct_id,
                    None,
                    "Text from JSON",
                );
                Text::new_value(construct_info, construct_context, idempotency_key, s.clone())
            }
            serde_json::Value::Number(n) => {
                let construct_info = ConstructInfo::new(
                    construct_id,
                    None,
                    "Number from JSON",
                );
                let number = n.as_f64().unwrap_or(0.0);
                Number::new_value(construct_info, construct_context, idempotency_key, number)
            }
            serde_json::Value::Object(obj) => {
                if let Some(serde_json::Value::String(tag)) = obj.get("_tag") {
                    // TaggedObject or Tag
                    let other_fields: Vec<_> = obj.iter()
                        .filter(|(k, _)| *k != "_tag")
                        .collect();

                    if other_fields.is_empty() {
                        // Just a Tag
                        let construct_info = ConstructInfo::new(
                            construct_id,
                            None,
                            "Tag from JSON",
                        );
                        Tag::new_value(construct_info, construct_context, idempotency_key, tag.clone())
                    } else {
                        // TaggedObject
                        let construct_info = ConstructInfo::new(
                            construct_id.clone(),
                            None,
                            "TaggedObject from JSON",
                        );
                        let variables: Vec<Arc<Variable>> = other_fields.iter()
                            .enumerate()
                            .map(|(i, (name, value))| {
                                let var_construct_info = ConstructInfo::new(
                                    construct_id.with_child_id(format!("var_{name}")),
                                    None,
                                    "Variable from JSON",
                                );
                                let value_actor = value_actor_from_json(
                                    value,
                                    construct_id.with_child_id(format!("value_{name}")),
                                    construct_context.clone(),
                                    Ulid::new(),
                                    actor_context.clone(),
                                );
                                Variable::new_arc(
                                    var_construct_info,
                                    construct_context.clone(),
                                    (*name).clone(),
                                    value_actor,
                                )
                            })
                            .collect();
                        TaggedObject::new_value(
                            construct_info,
                            construct_context,
                            idempotency_key,
                            tag.clone(),
                            variables,
                        )
                    }
                } else {
                    // Regular Object
                    let construct_info = ConstructInfo::new(
                        construct_id.clone(),
                        None,
                        "Object from JSON",
                    );
                    let variables: Vec<Arc<Variable>> = obj.iter()
                        .map(|(name, value)| {
                            let var_construct_info = ConstructInfo::new(
                                construct_id.with_child_id(format!("var_{name}")),
                                None,
                                "Variable from JSON",
                            );
                            let value_actor = value_actor_from_json(
                                value,
                                construct_id.with_child_id(format!("value_{name}")),
                                construct_context.clone(),
                                Ulid::new(),
                                actor_context.clone(),
                            );
                            Variable::new_arc(
                                var_construct_info,
                                construct_context.clone(),
                                name.clone(),
                                value_actor,
                            )
                        })
                        .collect();
                    Object::new_value(construct_info, construct_context, idempotency_key, variables)
                }
            }
            serde_json::Value::Array(arr) => {
                let construct_info = ConstructInfo::new(
                    construct_id.clone(),
                    None,
                    "List from JSON",
                );
                let items: Vec<Arc<ValueActor>> = arr.iter()
                    .enumerate()
                    .map(|(i, item)| {
                        value_actor_from_json(
                            item,
                            construct_id.with_child_id(format!("item_{i}")),
                            construct_context.clone(),
                            Ulid::new(),
                            actor_context.clone(),
                        )
                    })
                    .collect();
                List::new_value(construct_info, construct_context, idempotency_key, actor_context, items)
            }
            serde_json::Value::Bool(b) => {
                // Represent booleans as tags
                let construct_info = ConstructInfo::new(
                    construct_id,
                    None,
                    "Tag from JSON bool",
                );
                let tag = if *b { "True" } else { "False" };
                Tag::new_value(construct_info, construct_context, idempotency_key, tag)
            }
            serde_json::Value::Null => {
                // Represent null as a tag
                let construct_info = ConstructInfo::new(
                    construct_id,
                    None,
                    "Tag from JSON null",
                );
                Tag::new_value(construct_info, construct_context, idempotency_key, "None")
            }
        }
    }
}

/// Creates a ValueActor from a JSON value.
pub fn value_actor_from_json(
    json: &serde_json::Value,
    construct_id: ConstructId,
    construct_context: ConstructContext,
    idempotency_key: ValueIdempotencyKey,
    actor_context: ActorContext,
) -> Arc<ValueActor> {
    let value = Value::from_json(
        json,
        construct_id.clone(),
        construct_context,
        idempotency_key,
        actor_context.clone(),
    );
    let actor_construct_info = ConstructInfo::new(
        construct_id.with_child_id("value_actor"),
        None,
        "ValueActor from JSON",
    ).complete(ConstructType::ValueActor);
    Arc::new(ValueActor::new_internal(
        actor_construct_info,
        actor_context,
        constant(value),
        (),
    ))
}

// --- Object ---

pub struct Object {
    construct_info: ConstructInfoComplete,
    variables: Vec<Arc<Variable>>,
}

impl Object {
    pub fn new(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        variables: impl Into<Vec<Arc<Variable>>>,
    ) -> Self {
        Self {
            construct_info: construct_info.complete(ConstructType::Object),
            variables: variables.into(),
        }
    }

    pub fn new_arc(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        variables: impl Into<Vec<Arc<Variable>>>,
    ) -> Arc<Self> {
        Arc::new(Self::new(construct_info, construct_context, variables))
    }

    pub fn new_value(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        idempotency_key: ValueIdempotencyKey,
        variables: impl Into<Vec<Arc<Variable>>>,
    ) -> Value {
        Value::Object(
            Self::new_arc(construct_info, construct_context, variables),
            ValueMetadata { idempotency_key },
        )
    }

    pub fn new_constant(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        idempotency_key: ValueIdempotencyKey,
        variables: impl Into<Vec<Arc<Variable>>>,
    ) -> impl Stream<Item = Value> {
        constant(Self::new_value(
            construct_info,
            construct_context,
            idempotency_key,
            variables,
        ))
    }

    pub fn new_arc_value_actor(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        idempotency_key: ValueIdempotencyKey,
        actor_context: ActorContext,
        variables: impl Into<Vec<Arc<Variable>>>,
    ) -> Arc<ValueActor> {
        let ConstructInfo {
            id: actor_id,
            persistence,
            description: object_description,
        } = construct_info;
        let construct_info = ConstructInfo::new(
            actor_id.with_child_id("wrapped Object"),
            persistence,
            object_description,
        );
        let actor_construct_info =
            ConstructInfo::new(actor_id, persistence, "Constant object wrapper")
                .complete(ConstructType::ValueActor);
        let value_stream = Self::new_constant(
            construct_info,
            construct_context,
            idempotency_key,
            variables.into(),
        );
        Arc::new(ValueActor::new_internal(
            actor_construct_info,
            actor_context,
            value_stream,
            (),
        ))
    }

    pub fn variable(&self, name: &str) -> Option<Arc<Variable>> {
        self.variables
            .iter()
            .position(|variable| variable.name == name)
            .map(|index| self.variables[index].clone())
    }

    pub fn expect_variable(&self, name: &str) -> Arc<Variable> {
        self.variable(name).unwrap_or_else(|| {
            panic!(
                "Failed to get expected Variable '{name}' from {}",
                self.construct_info
            )
        })
    }

    pub fn variables(&self) -> &[Arc<Variable>] {
        &self.variables
    }
}

impl Drop for Object {
    fn drop(&mut self) {
        if LOG_DROPS_AND_LOOP_ENDS {
            println!("Dropped: {}", self.construct_info);
        }
    }
}

// --- TaggedObject ---

pub struct TaggedObject {
    construct_info: ConstructInfoComplete,
    tag: Cow<'static, str>,
    variables: Vec<Arc<Variable>>,
}

impl TaggedObject {
    pub fn new(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        tag: impl Into<Cow<'static, str>>,
        variables: impl Into<Vec<Arc<Variable>>>,
    ) -> Self {
        Self {
            construct_info: construct_info.complete(ConstructType::TaggedObject),
            tag: tag.into(),
            variables: variables.into(),
        }
    }

    pub fn new_arc(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        tag: impl Into<Cow<'static, str>>,
        variables: impl Into<Vec<Arc<Variable>>>,
    ) -> Arc<Self> {
        Arc::new(Self::new(construct_info, construct_context, tag, variables))
    }

    pub fn new_value(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        idempotency_key: ValueIdempotencyKey,
        tag: impl Into<Cow<'static, str>>,
        variables: impl Into<Vec<Arc<Variable>>>,
    ) -> Value {
        Value::TaggedObject(
            Self::new_arc(construct_info, construct_context, tag, variables),
            ValueMetadata { idempotency_key },
        )
    }

    pub fn new_constant(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        idempotency_key: ValueIdempotencyKey,
        tag: impl Into<Cow<'static, str>>,
        variables: impl Into<Vec<Arc<Variable>>>,
    ) -> impl Stream<Item = Value> {
        constant(Self::new_value(
            construct_info,
            construct_context,
            idempotency_key,
            tag,
            variables,
        ))
    }

    pub fn new_arc_value_actor(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        idempotency_key: ValueIdempotencyKey,
        actor_context: ActorContext,
        tag: impl Into<Cow<'static, str>>,
        variables: impl Into<Vec<Arc<Variable>>>,
    ) -> Arc<ValueActor> {
        let ConstructInfo {
            id: actor_id,
            persistence,
            description: tagged_object_description,
        } = construct_info;
        let construct_info = ConstructInfo::new(
            actor_id.with_child_id("wrapped TaggedObject"),
            persistence,
            tagged_object_description,
        );
        let actor_construct_info =
            ConstructInfo::new(actor_id, persistence, "Tagged object wrapper")
                .complete(ConstructType::ValueActor);
        let value_stream = Self::new_constant(
            construct_info,
            construct_context,
            idempotency_key,
            tag.into(),
            variables.into(),
        );
        Arc::new(ValueActor::new_internal(
            actor_construct_info,
            actor_context,
            value_stream,
            (),
        ))
    }

    pub fn variable(&self, name: &str) -> Option<Arc<Variable>> {
        self.variables
            .iter()
            .position(|variable| variable.name == name)
            .map(|index| self.variables[index].clone())
    }

    pub fn expect_variable(&self, name: &str) -> Arc<Variable> {
        self.variable(name).unwrap_or_else(|| {
            panic!(
                "Failed to get expected Variable '{name}' from {}",
                self.construct_info
            )
        })
    }

    pub fn tag(&self) -> &str {
        &self.tag
    }

    pub fn variables(&self) -> &[Arc<Variable>] {
        &self.variables
    }
}

impl Drop for TaggedObject {
    fn drop(&mut self) {
        if LOG_DROPS_AND_LOOP_ENDS {
            println!("Dropped: {}", self.construct_info);
        }
    }
}

// --- Text ---

pub struct Text {
    construct_info: ConstructInfoComplete,
    text: Cow<'static, str>,
}

impl Text {
    pub fn new(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        text: impl Into<Cow<'static, str>>,
    ) -> Self {
        Self {
            construct_info: construct_info.complete(ConstructType::Text),
            text: text.into(),
        }
    }

    pub fn new_arc(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        text: impl Into<Cow<'static, str>>,
    ) -> Arc<Self> {
        Arc::new(Self::new(construct_info, construct_context, text))
    }

    pub fn new_value(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        idempotency_key: ValueIdempotencyKey,
        text: impl Into<Cow<'static, str>>,
    ) -> Value {
        Value::Text(
            Self::new_arc(construct_info, construct_context, text),
            ValueMetadata { idempotency_key },
        )
    }

    pub fn new_constant(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        idempotency_key: ValueIdempotencyKey,
        text: impl Into<Cow<'static, str>>,
    ) -> impl Stream<Item = Value> {
        constant(Self::new_value(
            construct_info,
            construct_context,
            idempotency_key,
            text,
        ))
    }

    pub fn new_arc_value_actor(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        idempotency_key: ValueIdempotencyKey,
        actor_context: ActorContext,
        text: impl Into<Cow<'static, str>>,
    ) -> Arc<ValueActor> {
        let ConstructInfo {
            id: actor_id,
            persistence,
            description: text_description,
        } = construct_info;
        let construct_info = ConstructInfo::new(
            actor_id.with_child_id("wrapped Text"),
            persistence,
            text_description,
        );
        let actor_construct_info =
            ConstructInfo::new(actor_id, persistence, "Constant text wrapper")
                .complete(ConstructType::ValueActor);
        let value_stream = Self::new_constant(
            construct_info,
            construct_context,
            idempotency_key,
            text.into(),
        );
        Arc::new(ValueActor::new_internal(
            actor_construct_info,
            actor_context,
            value_stream,
            (),
        ))
    }

    pub fn text(&self) -> &str {
        &self.text
    }
}

impl Drop for Text {
    fn drop(&mut self) {
        if LOG_DROPS_AND_LOOP_ENDS {
            println!("Dropped: {}", self.construct_info);
        }
    }
}

// --- Tag ---

pub struct Tag {
    construct_info: ConstructInfoComplete,
    tag: Cow<'static, str>,
}

impl Tag {
    pub fn new(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        tag: impl Into<Cow<'static, str>>,
    ) -> Self {
        Self {
            construct_info: construct_info.complete(ConstructType::Tag),
            tag: tag.into(),
        }
    }

    pub fn new_arc(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        tag: impl Into<Cow<'static, str>>,
    ) -> Arc<Self> {
        Arc::new(Self::new(construct_info, construct_context, tag))
    }

    pub fn new_value(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        idempotency_key: ValueIdempotencyKey,
        tag: impl Into<Cow<'static, str>>,
    ) -> Value {
        Value::Tag(
            Self::new_arc(construct_info, construct_context, tag),
            ValueMetadata { idempotency_key },
        )
    }

    pub fn new_constant(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        idempotency_key: ValueIdempotencyKey,
        tag: impl Into<Cow<'static, str>>,
    ) -> impl Stream<Item = Value> {
        constant(Self::new_value(
            construct_info,
            construct_context,
            idempotency_key,
            tag,
        ))
    }

    pub fn new_arc_value_actor(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        idempotency_key: ValueIdempotencyKey,
        actor_context: ActorContext,
        tag: impl Into<Cow<'static, str>>,
    ) -> Arc<ValueActor> {
        let ConstructInfo {
            id: actor_id,
            persistence,
            description: tag_description,
        } = construct_info;
        let construct_info = ConstructInfo::new(
            actor_id.with_child_id("wrapped Tag"),
            persistence,
            tag_description,
        );
        let actor_construct_info =
            ConstructInfo::new(actor_id, persistence, "Constant tag wrapper")
                .complete(ConstructType::ValueActor);
        let value_stream = Self::new_constant(
            construct_info,
            construct_context,
            idempotency_key,
            tag.into(),
        );
        Arc::new(ValueActor::new_internal(
            actor_construct_info,
            actor_context,
            value_stream,
            (),
        ))
    }

    pub fn tag(&self) -> &str {
        &self.tag
    }
}

impl Drop for Tag {
    fn drop(&mut self) {
        if LOG_DROPS_AND_LOOP_ENDS {
            println!("Dropped: {}", self.construct_info);
        }
    }
}

// --- Number ---

pub struct Number {
    construct_info: ConstructInfoComplete,
    number: f64,
}

impl Number {
    pub fn new(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        number: impl Into<f64>,
    ) -> Self {
        Self {
            construct_info: construct_info.complete(ConstructType::Number),
            number: number.into(),
        }
    }

    pub fn new_arc(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        number: impl Into<f64>,
    ) -> Arc<Self> {
        Arc::new(Self::new(construct_info, construct_context, number))
    }

    pub fn new_value(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        idempotency_key: ValueIdempotencyKey,
        number: impl Into<f64>,
    ) -> Value {
        Value::Number(
            Self::new_arc(construct_info, construct_context, number),
            ValueMetadata { idempotency_key },
        )
    }

    pub fn new_constant(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        idempotency_key: ValueIdempotencyKey,
        number: impl Into<f64>,
    ) -> impl Stream<Item = Value> {
        constant(Self::new_value(
            construct_info,
            construct_context,
            idempotency_key,
            number,
        ))
    }

    pub fn new_arc_value_actor(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        idempotency_key: ValueIdempotencyKey,
        actor_context: ActorContext,
        number: impl Into<f64>,
    ) -> Arc<ValueActor> {
        let ConstructInfo {
            id: actor_id,
            persistence,
            description: number_description,
        } = construct_info;
        let construct_info = ConstructInfo::new(
            actor_id.with_child_id("wrapped Number"),
            persistence,
            number_description,
        );
        let actor_construct_info =
            ConstructInfo::new(actor_id, persistence, "Constant number wrapper)")
                .complete(ConstructType::ValueActor);
        let value_stream = Self::new_constant(
            construct_info,
            construct_context,
            idempotency_key,
            number.into(),
        );
        Arc::new(ValueActor::new_internal(
            actor_construct_info,
            actor_context,
            value_stream,
            (),
        ))
    }

    pub fn number(&self) -> f64 {
        self.number
    }
}

impl Drop for Number {
    fn drop(&mut self) {
        if LOG_DROPS_AND_LOOP_ENDS {
            println!("Dropped: {}", self.construct_info);
        }
    }
}

// --- List ---

pub struct List {
    construct_info: Arc<ConstructInfoComplete>,
    loop_task: TaskHandle,
    change_sender_sender: mpsc::UnboundedSender<mpsc::UnboundedSender<ListChange>>,
}

impl List {
    pub fn new(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        actor_context: ActorContext,
        items: impl Into<Vec<Arc<ValueActor>>>,
    ) -> Self {
        let change_stream = constant(ListChange::Replace {
            items: items.into(),
        });
        Self::new_with_change_stream(construct_info, actor_context, change_stream, ())
    }

    pub fn new_with_change_stream<EOD: 'static>(
        construct_info: ConstructInfo,
        actor_context: ActorContext,
        change_stream: impl Stream<Item = ListChange> + 'static,
        extra_owned_data: EOD,
    ) -> Self {
        let construct_info = Arc::new(construct_info.complete(ConstructType::List));
        let (change_sender_sender, mut change_sender_receiver) =
            mpsc::unbounded::<mpsc::UnboundedSender<ListChange>>();
        let loop_task = Task::start_droppable({
            let construct_info = construct_info.clone();
            let output_valve_signal = actor_context.output_valve_signal;
            async move {
                let output_valve_signal = output_valve_signal;
                let mut output_valve_impulse_stream =
                    if let Some(output_valve_signal) = &output_valve_signal {
                        output_valve_signal.subscribe().left_stream()
                    } else {
                        stream::pending().right_stream()
                    }
                    .fuse();
                let mut change_stream = pin!(change_stream.fuse());
                let mut change_senders = Vec::<mpsc::UnboundedSender<ListChange>>::new();
                let mut list = None;
                loop {
                    select! {
                        change = change_stream.next() => {
                            let Some(change) = change else { break };
                            if output_valve_signal.is_none() {
                                change_senders.retain(|change_sender| {
                                    if let Err(error) = change_sender.unbounded_send(change.clone()) {
                                        eprintln!("Failed to send new {construct_info} change to subscriber: {error:#}");
                                        false
                                    } else {
                                        true
                                    }
                                });
                            }
                            if let Some(list) = &mut list {
                                change.clone().apply_to_vec(list);
                            } else {
                                if let ListChange::Replace { items } = &change {
                                    list = Some(items.clone());
                                } else {
                                    panic!("Failed to initialize {construct_info}: The first change has to be 'ListChange::Replace'")
                                }
                            }
                        }
                        change_sender = change_sender_receiver.select_next_some() => {
                            if output_valve_signal.is_none() {
                                if let Some(list) = list.as_ref() {
                                    let first_change_to_send = ListChange::Replace { items: list.clone() };
                                    if let Err(error) = change_sender.unbounded_send(first_change_to_send) {
                                        eprintln!("Failed to send {construct_info} change to subscriber: {error:#}");
                                    } else {
                                        change_senders.push(change_sender);
                                    }
                                } else {
                                    change_senders.push(change_sender);
                                }
                            } else {
                                change_senders.push(change_sender);
                            }
                        }
                        impulse = output_valve_impulse_stream.next() => {
                            if impulse.is_none() {
                                break
                            }
                            if let Some(list) = list.as_ref() {
                                change_senders.retain(|change_sender| {
                                    let change_to_send = ListChange::Replace { items: list.clone() };
                                    if let Err(error) = change_sender.unbounded_send(change_to_send) {
                                        eprintln!("Failed to send {construct_info} change to subscriber on impulse: {error:#}");
                                        false
                                    } else {
                                        true
                                    }
                                });
                            }
                        }
                    }
                }
                if LOG_DROPS_AND_LOOP_ENDS {
                    println!("Loop ended {construct_info}");
                }
                drop(extra_owned_data);
            }
        });
        Self {
            construct_info,
            loop_task,
            change_sender_sender,
        }
    }

    pub fn new_arc(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        actor_context: ActorContext,
        items: impl Into<Vec<Arc<ValueActor>>>,
    ) -> Arc<Self> {
        Arc::new(Self::new(
            construct_info,
            construct_context,
            actor_context,
            items,
        ))
    }

    pub fn new_value(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        idempotency_key: ValueIdempotencyKey,
        actor_context: ActorContext,
        items: impl Into<Vec<Arc<ValueActor>>>,
    ) -> Value {
        Value::List(
            Self::new_arc(construct_info, construct_context, actor_context, items),
            ValueMetadata { idempotency_key },
        )
    }

    pub fn new_constant(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        idempotency_key: ValueIdempotencyKey,
        actor_context: ActorContext,
        items: impl Into<Vec<Arc<ValueActor>>>,
    ) -> impl Stream<Item = Value> {
        constant(Self::new_value(
            construct_info,
            construct_context,
            idempotency_key,
            actor_context,
            items,
        ))
    }

    pub fn new_arc_value_actor(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        idempotency_key: ValueIdempotencyKey,
        actor_context: ActorContext,
        items: impl Into<Vec<Arc<ValueActor>>>,
    ) -> Arc<ValueActor> {
        let ConstructInfo {
            id: actor_id,
            persistence,
            description: list_description,
        } = construct_info;
        let construct_info = ConstructInfo::new(
            actor_id.with_child_id("wrapped List"),
            persistence,
            list_description,
        );
        let actor_construct_info =
            ConstructInfo::new(actor_id, persistence, "Constant list wrapper")
                .complete(ConstructType::ValueActor);
        let value_stream = Self::new_constant(
            construct_info,
            construct_context,
            idempotency_key,
            actor_context.clone(),
            items.into(),
        );
        Arc::new(ValueActor::new_internal(
            actor_construct_info,
            actor_context,
            value_stream,
            (),
        ))
    }

    pub fn subscribe(&self) -> impl Stream<Item = ListChange> + use<> {
        let (change_sender, change_receiver) = mpsc::unbounded();
        if let Err(error) = self.change_sender_sender.unbounded_send(change_sender) {
            eprintln!("Failed to subscribe to {}: {error:#}", self.construct_info);
        }
        change_receiver
    }

    /// Creates a List with persistence support.
    /// - If saved data exists, it's loaded and used as initial items (code items are ignored)
    /// - On any change, the current list state is saved to storage
    pub fn new_arc_value_actor_with_persistence(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        idempotency_key: ValueIdempotencyKey,
        actor_context: ActorContext,
        code_items: impl Into<Vec<Arc<ValueActor>>>,
    ) -> Arc<ValueActor> {
        let code_items = code_items.into();
        let persistence = construct_info.persistence;

        // If no persistence, just use the regular constructor
        let Some(persistence_data) = persistence else {
            return Self::new_arc_value_actor(
                construct_info,
                construct_context,
                idempotency_key,
                actor_context,
                code_items,
            );
        };

        let persistence_id = persistence_data.id;
        let construct_storage = construct_context.construct_storage.clone();

        let ConstructInfo {
            id: actor_id,
            persistence: _,
            description: list_description,
        } = construct_info;

        // Create a stream that:
        // 1. First emits loaded items from storage (or code items if nothing saved)
        // 2. Then wraps further changes to save them
        let construct_context_for_load = construct_context.clone();
        let actor_context_for_load = actor_context.clone();
        let actor_id_for_load = actor_id.clone();

        let value_stream = stream::once(async move {
            // Try to load from storage
            let loaded_items: Option<Vec<serde_json::Value>> = construct_storage
                .clone()
                .load_state(persistence_id)
                .await;

            let initial_items = if let Some(json_items) = loaded_items {
                // Deserialize items from JSON
                json_items
                    .iter()
                    .enumerate()
                    .map(|(i, json)| {
                        value_actor_from_json(
                            json,
                            actor_id_for_load.with_child_id(format!("loaded_item_{i}")),
                            construct_context_for_load.clone(),
                            Ulid::new(),
                            actor_context_for_load.clone(),
                        )
                    })
                    .collect()
            } else {
                // Use code-defined items
                code_items
            };

            // Create the inner list
            let inner_construct_info = ConstructInfo::new(
                actor_id_for_load.with_child_id("persistent_list"),
                Some(persistence_data),
                "Persistent List",
            );
            let list = List::new_arc(
                inner_construct_info,
                construct_context_for_load.clone(),
                actor_context_for_load.clone(),
                initial_items,
            );

            // Start a background task to save changes
            let list_for_save = list.clone();
            let construct_storage_for_save = construct_storage;
            Task::start(async move {
                let mut change_stream = pin!(list_for_save.subscribe());
                while let Some(change) = change_stream.next().await {
                    // After any change, serialize and save the current list
                    if let ListChange::Replace { ref items } = change {
                        let mut json_items = Vec::new();
                        for item in items {
                            if let Some(value) = item.subscribe().next().await {
                                json_items.push(value.to_json().await);
                            }
                        }
                        construct_storage_for_save.save_state(persistence_id, &json_items).await;
                    } else {
                        // For incremental changes, we need to get the full list and save it
                        // This is done by getting the next Replace event after the change is applied
                        // But for simplicity, let's re-subscribe to get the current state
                        if let Some(ListChange::Replace { items }) = list_for_save.subscribe().next().await {
                            let mut json_items = Vec::new();
                            for item in &items {
                                if let Some(value) = item.subscribe().next().await {
                                    json_items.push(value.to_json().await);
                                }
                            }
                            construct_storage_for_save.save_state(persistence_id, &json_items).await;
                        }
                    }
                }
            });

            Value::List(list, ValueMetadata { idempotency_key })
        }).chain(stream::pending());

        let actor_construct_info = ConstructInfo::new(
            actor_id,
            Some(persistence_data),
            "Persistent list wrapper",
        ).complete(ConstructType::ValueActor);

        Arc::new(ValueActor::new_internal(
            actor_construct_info,
            actor_context,
            value_stream,
            (),
        ))
    }
}

impl Drop for List {
    fn drop(&mut self) {
        if LOG_DROPS_AND_LOOP_ENDS {
            println!("Dropped: {}", self.construct_info);
        }
    }
}

#[derive(Clone)]
pub enum ListChange {
    Replace { items: Vec<Arc<ValueActor>> },
    InsertAt { index: usize, item: Arc<ValueActor> },
    UpdateAt { index: usize, item: Arc<ValueActor> },
    RemoveAt { index: usize },
    Move { old_index: usize, new_index: usize },
    Push { item: Arc<ValueActor> },
    Pop,
    Clear,
}

impl ListChange {
    pub fn apply_to_vec(self, vec: &mut Vec<Arc<ValueActor>>) {
        match self {
            Self::Replace { items } => {
                *vec = items;
            }
            Self::InsertAt { index, item } => {
                vec.insert(index, item);
            }
            Self::UpdateAt { index, item } => {
                vec[index] = item;
            }
            Self::Push { item } => {
                vec.push(item);
            }
            Self::RemoveAt { index } => {
                vec.remove(index);
            }
            Self::Move {
                old_index,
                new_index,
            } => {
                let item = vec.remove(old_index);
                vec.insert(new_index, item);
            }
            Self::Pop => {
                vec.pop().unwrap();
            }
            Self::Clear => {
                vec.clear();
            }
        }
    }
}

// --- ListBindingFunction ---

use crate::parser::static_expression::{Expression as StaticExpression, Spanned as StaticSpanned};
use crate::parser::StrSlice;

/// Handles List binding functions (map, retain, every, any) that need to
/// evaluate an expression for each list item.
///
/// Uses StaticExpression which is 'static (via StrSlice into Arc<String> source)
/// and can be:
/// - Stored in async contexts without lifetime issues
/// - Sent to WebWorkers for parallel processing
/// - Cloned cheaply (just Arc increment + offset copy)
/// - Serialized for distributed evaluation
pub struct ListBindingFunction;

/// Configuration for a list binding operation.
#[derive(Clone)]
pub struct ListBindingConfig {
    /// The variable name that will be bound to each list item
    pub binding_name: StrSlice,
    /// The expression to evaluate for each item (with binding_name in scope)
    pub transform_expr: StaticSpanned<StaticExpression>,
    /// The type of list operation
    pub operation: ListBindingOperation,
    /// Reference connector for looking up scope-resolved references
    pub reference_connector: Arc<ReferenceConnector>,
    /// Link connector for connecting LINK variables with their setters
    pub link_connector: Arc<LinkConnector>,
    /// Source code for creating borrowed expressions
    pub source_code: SourceCode,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ListBindingOperation {
    Map,
    Retain,
    Every,
    Any,
    SortBy,
}

/// A sortable key extracted from a Value for use in List/sort_by.
/// Supports comparison of Numbers, Text, and Tags.
#[derive(Clone, Debug)]
pub enum SortKey {
    Number(f64),
    Text(String),
    Tag(String),
    /// Fallback for unsupported types - sorts last
    Unsupported,
}

impl SortKey {
    /// Extract a sortable key from a Value
    pub fn from_value(value: &Value) -> Self {
        match value {
            Value::Number(num, _) => SortKey::Number(num.number()),
            Value::Text(text, _) => SortKey::Text(text.text().to_string()),
            Value::Tag(tag, _) => SortKey::Tag(tag.tag().to_string()),
            Value::Flushed(inner, _) => SortKey::from_value(inner),
            _ => SortKey::Unsupported,
        }
    }
}

impl PartialEq for SortKey {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (SortKey::Number(a), SortKey::Number(b)) => {
                // Handle NaN properly
                if a.is_nan() && b.is_nan() { true }
                else { a == b }
            }
            (SortKey::Text(a), SortKey::Text(b)) => a == b,
            (SortKey::Tag(a), SortKey::Tag(b)) => a == b,
            (SortKey::Unsupported, SortKey::Unsupported) => true,
            _ => false,
        }
    }
}

impl Eq for SortKey {}

impl PartialOrd for SortKey {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SortKey {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        match (self, other) {
            (SortKey::Number(a), SortKey::Number(b)) => {
                // Handle NaN: NaN sorts last
                match (a.is_nan(), b.is_nan()) {
                    (true, true) => Ordering::Equal,
                    (true, false) => Ordering::Greater,
                    (false, true) => Ordering::Less,
                    (false, false) => a.partial_cmp(b).unwrap_or(Ordering::Equal),
                }
            }
            (SortKey::Text(a), SortKey::Text(b)) => a.cmp(b),
            (SortKey::Tag(a), SortKey::Tag(b)) => a.cmp(b),
            // Different types sort by type priority: Number < Text < Tag < Unsupported
            (SortKey::Number(_), _) => Ordering::Less,
            (_, SortKey::Number(_)) => Ordering::Greater,
            (SortKey::Text(_), _) => Ordering::Less,
            (_, SortKey::Text(_)) => Ordering::Greater,
            (SortKey::Tag(_), _) => Ordering::Less,
            (_, SortKey::Tag(_)) => Ordering::Greater,
            (SortKey::Unsupported, SortKey::Unsupported) => Ordering::Equal,
        }
    }
}

impl ListBindingFunction {
    /// Creates a new ValueActor for a List binding function.
    ///
    /// For List/map(old, new: expr):
    /// - Subscribes to the source list
    /// - For each item, evaluates transform_expr with 'old' bound to the item
    /// - Produces the transformed list
    ///
    /// The StaticExpression is 'static, so it can be used in async handlers
    /// and potentially sent to WebWorkers for parallel processing.
    pub fn new_arc_value_actor(
        construct_info: ConstructInfo,
        construct_context: ConstructContext,
        actor_context: ActorContext,
        source_list_actor: Arc<ValueActor>,
        config: ListBindingConfig,
    ) -> Arc<ValueActor> {
        let construct_info = construct_info.complete(ConstructType::FunctionCall);
        let config = Rc::new(config);

        match config.operation {
            ListBindingOperation::Map => {
                Self::create_map_actor(
                    construct_info,
                    construct_context,
                    actor_context,
                    source_list_actor,
                    config,
                )
            }
            ListBindingOperation::Retain => {
                Self::create_retain_actor(
                    construct_info,
                    construct_context,
                    actor_context,
                    source_list_actor,
                    config,
                )
            }
            ListBindingOperation::Every => {
                Self::create_every_any_actor(
                    construct_info,
                    construct_context,
                    actor_context,
                    source_list_actor,
                    config,
                    true, // is_every
                )
            }
            ListBindingOperation::Any => {
                Self::create_every_any_actor(
                    construct_info,
                    construct_context,
                    actor_context,
                    source_list_actor,
                    config,
                    false, // is_every (false = any)
                )
            }
            ListBindingOperation::SortBy => {
                Self::create_sort_by_actor(
                    construct_info,
                    construct_context,
                    actor_context,
                    source_list_actor,
                    config,
                )
            }
        }
    }

    /// Creates a map actor that transforms each list item.
    fn create_map_actor(
        construct_info: ConstructInfoComplete,
        construct_context: ConstructContext,
        actor_context: ActorContext,
        source_list_actor: Arc<ValueActor>,
        config: Rc<ListBindingConfig>,
    ) -> Arc<ValueActor> {
        let config_for_stream = config.clone();
        let construct_context_for_stream = construct_context.clone();
        let actor_context_for_stream = actor_context.clone();

        let change_stream = source_list_actor.subscribe().filter_map(|value| {
            future::ready(match value {
                Value::List(list, _) => Some(list),
                _ => None,
            })
        }).flat_map(move |list| {
            let config = config_for_stream.clone();
            let construct_context = construct_context_for_stream.clone();
            let actor_context = actor_context_for_stream.clone();

            list.subscribe().map(move |change| {
                Self::transform_list_change_for_map(
                    change,
                    &config,
                    construct_context.clone(),
                    actor_context.clone(),
                )
            })
        });

        let list = List::new_with_change_stream(
            ConstructInfo::new(
                construct_info.id.clone().with_child_id(0),
                None,
                "List/map result",
            ),
            actor_context.clone(),
            change_stream,
            source_list_actor.clone(),
        );

        Arc::new(ValueActor::new_internal(
            construct_info,
            actor_context,
            constant(Value::List(
                Arc::new(list),
                ValueMetadata { idempotency_key: ValueIdempotencyKey::new() },
            )),
            vec![source_list_actor],
        ))
    }

    /// Creates a retain actor that filters list items based on predicate.
    /// When any item's predicate changes, emits an updated filtered list.
    fn create_retain_actor(
        construct_info: ConstructInfoComplete,
        construct_context: ConstructContext,
        actor_context: ActorContext,
        source_list_actor: Arc<ValueActor>,
        config: Rc<ListBindingConfig>,
    ) -> Arc<ValueActor> {
        let construct_info_id = construct_info.id.clone();

        // Clone for use after the flat_map chain
        let actor_context_for_list = actor_context.clone();
        let actor_context_for_result = actor_context.clone();

        // Create a stream that:
        // 1. Subscribes to source list changes
        // 2. For each item, evaluates predicate and subscribes to its changes
        // 3. When list or any predicate changes, emits filtered Replace
        let value_stream = source_list_actor.subscribe().filter_map(|value| {
            future::ready(match value {
                Value::List(list, _) => Some(list),
                _ => None,
            })
        }).flat_map(move |list| {
            let config = config.clone();
            let construct_context = construct_context.clone();
            let actor_context = actor_context.clone();
            let construct_info_id = construct_info_id.clone();

            // Clone for the second flat_map
            let construct_info_id_inner = construct_info_id.clone();

            // Track items and their predicates
            list.subscribe().scan(
                Vec::<(Arc<ValueActor>, Arc<ValueActor>)>::new(), // (item, predicate)
                move |item_predicates, change| {
                    let config = config.clone();
                    let construct_context = construct_context.clone();
                    let actor_context = actor_context.clone();

                    // Apply change and update predicate actors
                    match &change {
                        ListChange::Replace { items } => {
                            *item_predicates = items.iter().map(|item| {
                                let predicate = Self::transform_item(
                                    item.clone(),
                                    &config,
                                    construct_context.clone(),
                                    actor_context.clone(),
                                );
                                (item.clone(), predicate)
                            }).collect();
                        }
                        ListChange::Push { item } => {
                            let predicate = Self::transform_item(
                                item.clone(),
                                &config,
                                construct_context.clone(),
                                actor_context.clone(),
                            );
                            item_predicates.push((item.clone(), predicate));
                        }
                        ListChange::InsertAt { index, item } => {
                            let predicate = Self::transform_item(
                                item.clone(),
                                &config,
                                construct_context.clone(),
                                actor_context.clone(),
                            );
                            if *index <= item_predicates.len() {
                                item_predicates.insert(*index, (item.clone(), predicate));
                            }
                        }
                        ListChange::RemoveAt { index } => {
                            if *index < item_predicates.len() {
                                item_predicates.remove(*index);
                            }
                        }
                        ListChange::Clear => {
                            item_predicates.clear();
                        }
                        ListChange::Pop => {
                            item_predicates.pop();
                        }
                        _ => {}
                    }

                    future::ready(Some(item_predicates.clone()))
                }
            ).flat_map(move |item_predicates| {
                let construct_info_id = construct_info_id_inner.clone();

                if item_predicates.is_empty() {
                    // Empty list - emit empty Replace
                    return stream::once(future::ready(ListChange::Replace { items: vec![] })).boxed_local();
                }

                // Subscribe to all predicates and emit filtered list when any changes
                let predicate_streams: Vec<_> = item_predicates.iter().enumerate().map(|(idx, (item, pred))| {
                    let item = item.clone();
                    pred.subscribe().map(move |value| (idx, item.clone(), value))
                }).collect();

                stream::select_all(predicate_streams)
                    .scan(
                        item_predicates.iter().map(|(item, _)| (item.clone(), None::<bool>)).collect::<Vec<_>>(),
                        move |states, (idx, item, value)| {
                            // Update the predicate result for this item
                            let is_true = match &value {
                                Value::Tag(tag, _) => tag.tag() == "True",
                                _ => false,
                            };
                            if idx < states.len() {
                                states[idx] = (item, Some(is_true));
                            }

                            // If all items have predicate results, emit filtered list
                            let all_evaluated = states.iter().all(|(_, result)| result.is_some());
                            if all_evaluated {
                                let filtered: Vec<Arc<ValueActor>> = states.iter()
                                    .filter_map(|(item, result)| {
                                        if result == &Some(true) {
                                            Some(item.clone())
                                        } else {
                                            None
                                        }
                                    })
                                    .collect();
                                future::ready(Some(Some(ListChange::Replace { items: filtered })))
                            } else {
                                future::ready(Some(None))
                            }
                        }
                    )
                    .filter_map(future::ready)
                    .boxed_local()
            })
        });

        let list = List::new_with_change_stream(
            ConstructInfo::new(
                construct_info.id.clone().with_child_id(0),
                None,
                "List/retain result",
            ),
            actor_context_for_list,
            value_stream,
            source_list_actor.clone(),
        );

        Arc::new(ValueActor::new_internal(
            construct_info,
            actor_context_for_result,
            constant(Value::List(
                Arc::new(list),
                ValueMetadata { idempotency_key: ValueIdempotencyKey::new() },
            )),
            vec![source_list_actor],
        ))
    }

    /// Creates an every/any actor that produces True/False based on predicates.
    fn create_every_any_actor(
        construct_info: ConstructInfoComplete,
        construct_context: ConstructContext,
        actor_context: ActorContext,
        source_list_actor: Arc<ValueActor>,
        config: Rc<ListBindingConfig>,
        is_every: bool, // true = every, false = any
    ) -> Arc<ValueActor> {
        let construct_info_id = construct_info.id.clone();

        // Clone for use after the flat_map chain
        let actor_context_for_result = actor_context.clone();

        let value_stream = source_list_actor.subscribe().filter_map(|value| {
            future::ready(match value {
                Value::List(list, _) => Some(list),
                _ => None,
            })
        }).flat_map(move |list| {
            let config = config.clone();
            let construct_context = construct_context.clone();
            let actor_context = actor_context.clone();
            let construct_info_id = construct_info_id.clone();

            // Clone for the second flat_map
            let construct_info_id_inner = construct_info_id.clone();
            let construct_context_inner = construct_context.clone();

            list.subscribe().scan(
                Vec::<(Arc<ValueActor>, Arc<ValueActor>)>::new(),
                move |item_predicates, change| {
                    let config = config.clone();
                    let construct_context = construct_context.clone();
                    let actor_context = actor_context.clone();

                    match &change {
                        ListChange::Replace { items } => {
                            *item_predicates = items.iter().map(|item| {
                                let predicate = Self::transform_item(
                                    item.clone(),
                                    &config,
                                    construct_context.clone(),
                                    actor_context.clone(),
                                );
                                (item.clone(), predicate)
                            }).collect();
                        }
                        ListChange::Push { item } => {
                            let predicate = Self::transform_item(
                                item.clone(),
                                &config,
                                construct_context.clone(),
                                actor_context.clone(),
                            );
                            item_predicates.push((item.clone(), predicate));
                        }
                        ListChange::Clear => {
                            item_predicates.clear();
                        }
                        ListChange::Pop => {
                            item_predicates.pop();
                        }
                        ListChange::RemoveAt { index } => {
                            if *index < item_predicates.len() {
                                item_predicates.remove(*index);
                            }
                        }
                        _ => {}
                    }

                    future::ready(Some(item_predicates.clone()))
                }
            ).flat_map(move |item_predicates| {
                let construct_info_id = construct_info_id_inner.clone();
                let construct_context = construct_context_inner.clone();

                if item_predicates.is_empty() {
                    // Empty list: every([]) = True, any([]) = False
                    let result = if is_every { "True" } else { "False" };
                    return stream::once(future::ready(Tag::new_value(
                        ConstructInfo::new(
                            construct_info_id.clone().with_child_id(0),
                            None,
                            if is_every { "List/every result" } else { "List/any result" },
                        ),
                        construct_context,
                        ValueIdempotencyKey::new(),
                        result.to_string(),
                    ))).boxed_local();
                }

                // Clone for the map closure
                let construct_info_id_map = construct_info_id.clone();
                let construct_context_map = construct_context.clone();

                let predicate_streams: Vec<_> = item_predicates.iter().enumerate().map(|(idx, (_, pred))| {
                    pred.subscribe().map(move |value| (idx, value))
                }).collect();

                stream::select_all(predicate_streams)
                    .scan(
                        vec![None::<bool>; item_predicates.len()],
                        move |states, (idx, value)| {
                            let is_true = match &value {
                                Value::Tag(tag, _) => tag.tag() == "True",
                                _ => false,
                            };
                            if idx < states.len() {
                                states[idx] = Some(is_true);
                            }

                            let all_evaluated = states.iter().all(|r| r.is_some());
                            if all_evaluated {
                                let result = if is_every {
                                    states.iter().all(|r| r == &Some(true))
                                } else {
                                    states.iter().any(|r| r == &Some(true))
                                };
                                future::ready(Some(Some(result)))
                            } else {
                                future::ready(Some(None))
                            }
                        }
                    )
                    .filter_map(future::ready)
                    .map(move |result| {
                        let tag = if result { "True" } else { "False" };
                        Tag::new_value(
                            ConstructInfo::new(
                                construct_info_id_map.clone().with_child_id(0),
                                None,
                                if is_every { "List/every result" } else { "List/any result" },
                            ),
                            construct_context_map.clone(),
                            ValueIdempotencyKey::new(),
                            tag.to_string(),
                        )
                    })
                    .boxed_local()
            })
        });

        Arc::new(ValueActor::new_internal(
            construct_info,
            actor_context_for_result,
            value_stream,
            vec![source_list_actor],
        ))
    }

    /// Creates a sort_by actor that sorts list items based on a key expression.
    /// When any item's key changes, emits an updated sorted list.
    fn create_sort_by_actor(
        construct_info: ConstructInfoComplete,
        construct_context: ConstructContext,
        actor_context: ActorContext,
        source_list_actor: Arc<ValueActor>,
        config: Rc<ListBindingConfig>,
    ) -> Arc<ValueActor> {
        let construct_info_id = construct_info.id.clone();

        // Clone for use after the flat_map chain
        let actor_context_for_list = actor_context.clone();
        let actor_context_for_result = actor_context.clone();

        // Create a stream that:
        // 1. Subscribes to source list changes
        // 2. For each item, evaluates key expression and subscribes to its changes
        // 3. When list or any key changes, emits sorted Replace
        let value_stream = source_list_actor.subscribe().filter_map(|value| {
            future::ready(match value {
                Value::List(list, _) => Some(list),
                _ => None,
            })
        }).flat_map(move |list| {
            let config = config.clone();
            let construct_context = construct_context.clone();
            let actor_context = actor_context.clone();
            let construct_info_id = construct_info_id.clone();

            // Clone for the second flat_map
            let construct_info_id_inner = construct_info_id.clone();

            // Track items and their keys
            list.subscribe().scan(
                Vec::<(Arc<ValueActor>, Arc<ValueActor>)>::new(), // (item, key_actor)
                move |item_keys, change| {
                    let config = config.clone();
                    let construct_context = construct_context.clone();
                    let actor_context = actor_context.clone();

                    // Apply change and update key actors
                    match &change {
                        ListChange::Replace { items } => {
                            *item_keys = items.iter().map(|item| {
                                let key_actor = Self::transform_item(
                                    item.clone(),
                                    &config,
                                    construct_context.clone(),
                                    actor_context.clone(),
                                );
                                (item.clone(), key_actor)
                            }).collect();
                        }
                        ListChange::Push { item } => {
                            let key_actor = Self::transform_item(
                                item.clone(),
                                &config,
                                construct_context.clone(),
                                actor_context.clone(),
                            );
                            item_keys.push((item.clone(), key_actor));
                        }
                        ListChange::InsertAt { index, item } => {
                            let key_actor = Self::transform_item(
                                item.clone(),
                                &config,
                                construct_context.clone(),
                                actor_context.clone(),
                            );
                            if *index <= item_keys.len() {
                                item_keys.insert(*index, (item.clone(), key_actor));
                            }
                        }
                        ListChange::RemoveAt { index } => {
                            if *index < item_keys.len() {
                                item_keys.remove(*index);
                            }
                        }
                        ListChange::Clear => {
                            item_keys.clear();
                        }
                        ListChange::Pop => {
                            item_keys.pop();
                        }
                        _ => {}
                    }

                    future::ready(Some(item_keys.clone()))
                }
            ).flat_map(move |item_keys| {
                let construct_info_id = construct_info_id_inner.clone();

                if item_keys.is_empty() {
                    // Empty list - emit empty Replace
                    return stream::once(future::ready(ListChange::Replace { items: vec![] })).boxed_local();
                }

                // Subscribe to all keys and emit sorted list when any changes
                let key_streams: Vec<_> = item_keys.iter().enumerate().map(|(idx, (item, key_actor))| {
                    let item = item.clone();
                    key_actor.subscribe().map(move |value| (idx, item.clone(), value))
                }).collect();

                stream::select_all(key_streams)
                    .scan(
                        item_keys.iter().map(|(item, _)| (item.clone(), None::<SortKey>)).collect::<Vec<_>>(),
                        move |states, (idx, item, value)| {
                            // Extract sortable key from value
                            let sort_key = SortKey::from_value(&value);
                            if idx < states.len() {
                                states[idx] = (item, Some(sort_key));
                            }

                            // If all items have key results, emit sorted list
                            let all_evaluated = states.iter().all(|(_, result)| result.is_some());
                            if all_evaluated {
                                // Sort by key, preserving original order for equal keys (stable sort)
                                let mut indexed_items: Vec<_> = states.iter().enumerate()
                                    .map(|(orig_idx, (item, key))| (orig_idx, item.clone(), key.clone().unwrap()))
                                    .collect();
                                indexed_items.sort_by(|(orig_a, _, key_a), (orig_b, _, key_b)| {
                                    match key_a.cmp(key_b) {
                                        std::cmp::Ordering::Equal => orig_a.cmp(orig_b), // stable sort
                                        other => other,
                                    }
                                });
                                let sorted: Vec<Arc<ValueActor>> = indexed_items.into_iter()
                                    .map(|(_, item, _)| item)
                                    .collect();
                                future::ready(Some(Some(ListChange::Replace { items: sorted })))
                            } else {
                                future::ready(Some(None))
                            }
                        }
                    )
                    .filter_map(future::ready)
                    .boxed_local()
            })
        });

        let list = List::new_with_change_stream(
            ConstructInfo::new(
                construct_info.id.clone().with_child_id(0),
                None,
                "List/sort_by result",
            ),
            actor_context_for_list,
            value_stream,
            source_list_actor.clone(),
        );

        Arc::new(ValueActor::new_internal(
            construct_info,
            actor_context_for_result,
            constant(Value::List(
                Arc::new(list),
                ValueMetadata { idempotency_key: ValueIdempotencyKey::new() },
            )),
            vec![source_list_actor],
        ))
    }

    /// Transform a single list item using the config's transform expression.
    fn transform_item(
        item_actor: Arc<ValueActor>,
        config: &ListBindingConfig,
        construct_context: ConstructContext,
        actor_context: ActorContext,
    ) -> Arc<ValueActor> {
        // Create a new ActorContext with the binding variable set
        let binding_name = config.binding_name.to_string();
        let mut new_params = actor_context.parameters.clone();
        new_params.insert(binding_name, item_actor.clone());

        let new_actor_context = ActorContext {
            parameters: new_params,
            ..actor_context
        };

        // Evaluate the transform expression with the binding in scope
        match evaluate_static_expression(
            &config.transform_expr,
            construct_context,
            new_actor_context,
            config.reference_connector.clone(),
            config.link_connector.clone(),
            config.source_code.clone(),
        ) {
            Ok(result_actor) => result_actor,
            Err(e) => {
                eprintln!("Error evaluating transform expression: {e}");
                // Return the original item as fallback
                item_actor
            }
        }
    }

    /// Transform a ListChange by applying the transform expression to affected items.
    /// Only used for map operation.
    fn transform_list_change_for_map(
        change: ListChange,
        config: &ListBindingConfig,
        construct_context: ConstructContext,
        actor_context: ActorContext,
    ) -> ListChange {
        match change {
            ListChange::Replace { items } => {
                let transformed_items: Vec<Arc<ValueActor>> = items
                    .into_iter()
                    .map(|item| {
                        Self::transform_item(
                            item,
                            config,
                            construct_context.clone(),
                            actor_context.clone(),
                        )
                    })
                    .collect();
                ListChange::Replace { items: transformed_items }
            }
            ListChange::InsertAt { index, item } => {
                let transformed_item = Self::transform_item(
                    item,
                    config,
                    construct_context,
                    actor_context,
                );
                ListChange::InsertAt { index, item: transformed_item }
            }
            ListChange::UpdateAt { index, item } => {
                let transformed_item = Self::transform_item(
                    item,
                    config,
                    construct_context,
                    actor_context,
                );
                ListChange::UpdateAt { index, item: transformed_item }
            }
            ListChange::Push { item } => {
                let transformed_item = Self::transform_item(
                    item,
                    config,
                    construct_context,
                    actor_context,
                );
                ListChange::Push { item: transformed_item }
            }
            // These operations don't involve new items, pass through unchanged
            ListChange::RemoveAt { index } => ListChange::RemoveAt { index },
            ListChange::Move { old_index, new_index } => ListChange::Move { old_index, new_index },
            ListChange::Pop => ListChange::Pop,
            ListChange::Clear => ListChange::Clear,
        }
    }
}
