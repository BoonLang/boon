//! Core types for the DD v2 engine.
//! No DD, Zoon, or browser dependencies.

use std::sync::Arc;

use indexmap::IndexMap;

use super::value::Value;

/// Identifies a variable / collection in the DD dataflow graph.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct VarId(pub Arc<str>);

impl VarId {
    pub fn new(name: impl Into<Arc<str>>) -> Self {
        VarId(name.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for VarId {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Identifies an external input source (LINK events, timers, browser state).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct InputId(pub usize);

/// Identifies a LINK event binding path (e.g., "increment_button.event.press").
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct LinkId(pub Arc<str>);

impl LinkId {
    pub fn new(path: impl Into<Arc<str>>) -> Self {
        LinkId(path.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Key for list elements in DD collections.
///
/// Lists are DD collections of `(ListKey, Value)` pairs.
/// ListKey provides stable identity for incremental updates.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
pub struct ListKey(pub Arc<str>);

impl ListKey {
    pub fn new(key: impl Into<Arc<str>>) -> Self {
        ListKey(key.into())
    }

    pub fn from_index(index: usize) -> Self {
        ListKey(Arc::from(format!("{index}")))
    }
}

impl std::fmt::Display for ListKey {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

// ===========================================================================
// KeyedDiff — incremental diffs from DD keyed collections
// ===========================================================================

/// A keyed diff from a DD collection.
///
/// Represents an O(1) incremental change to a keyed list.
/// Produced by inspect callbacks on keyed DD collections.
#[derive(Clone, Debug)]
pub enum KeyedDiff {
    /// Insert or update an item with the given key.
    Upsert { key: ListKey, value: Value },
    /// Remove the item with the given key.
    Remove { key: ListKey },
}

/// Specification for streaming keyed diffs to the bridge and persistence.
///
/// When set on a DataflowGraph, the runtime wires inspect callbacks on the
/// keyed collections to produce O(1) per-item diffs instead of monolithic
/// assembled list values.
pub struct KeyedListOutput {
    /// Keyed collection of display-ready element Values (post-retain, post-map).
    pub display_var: VarId,
    /// Keyed collection of raw data for persistence (pre-display-transform).
    pub persistence_var: VarId,
    /// Storage key for localStorage (if persistence is enabled).
    pub storage_key: Option<String>,
    /// Hold name for persistence identification.
    pub hold_name: Option<String>,
    /// Element tag of the Stripe that displays keyed items (e.g., "Ul").
    /// Used by the bridge to identify which Stripe receives keyed diffs.
    pub element_tag: Option<String>,
}

// ===========================================================================
// DataflowGraph — the compiled representation of a reactive Boon program
// ===========================================================================

/// A complete DD dataflow specification compiled from Boon source.
///
/// Contains all input specifications, collection definitions in topological
/// order, and the root document output variable.
pub struct DataflowGraph {
    /// External input sources (LINK events, timers, router).
    pub inputs: Vec<InputSpec>,
    /// Collection definitions in topological order.
    /// Each entry maps a VarId to its CollectionSpec.
    pub collections: IndexMap<VarId, CollectionSpec>,
    /// The VarId of the root document output.
    pub document: VarId,
    /// Storage key for localStorage persistence (e.g., "counter_hold").
    /// When set, HOLD state changes are persisted and restored on re-run.
    pub storage_key: Option<String>,
    /// Keyed list output specification for O(1) per-item streaming.
    /// When set, items flow individually from DD to bridge/persistence,
    /// bypassing AssembleList entirely.
    pub keyed_list_output: Option<KeyedListOutput>,
}

/// Specification for an external input source.
#[derive(Clone, Debug)]
pub struct InputSpec {
    pub id: InputId,
    pub kind: InputKind,
    /// LINK path for link-based events (e.g., "increment_button.event.press").
    /// For timers, this is the variable name (e.g., "tick").
    pub link_path: Option<String>,
    /// For Timer inputs: the interval duration in seconds.
    pub timer_interval_secs: Option<f64>,
}

/// Kind of external input.
#[derive(Clone, Debug, PartialEq)]
pub enum InputKind {
    LinkPress,
    LinkClick,
    KeyDown,
    TextChange,
    Blur,
    Focus,
    DoubleClick,
    HoverChange,
    Timer,
    Router,
}

/// Closure types for DD operators.
/// These are `Arc`-wrapped so they can be shared across DD operator closures.
pub type TransformFn = Arc<dyn Fn(&Value) -> Value + 'static>;
pub type CombineFn = Arc<dyn Fn(&Value, &Value) -> Value + 'static>;
pub type FlatMapFn = Arc<dyn Fn(Value) -> Option<Value> + 'static>;
pub type HoldTransformFn = Arc<dyn Fn(&Value, &Value) -> Value + 'static>;
pub type ClassifyFn = Arc<dyn Fn(&Value) -> Option<(ListKey, Value)> + 'static>;
pub type BroadcastHandlerFn = Arc<
    dyn Fn(
            &std::collections::HashMap<ListKey, Value>,
            &Value,
        ) -> Vec<(ListKey, Option<Value>)>
        + 'static,
>;

/// Specification of a single collection in the dataflow.
///
/// Each variant corresponds to a DD operator or input source.
/// The compiler emits these, and `runtime::materialize()` turns them
/// into live DD collections.
pub enum CollectionSpec {
    /// Constant value — a single-element collection.
    Literal(Value),

    /// Constant keyed list — a multi-element keyed collection.
    LiteralList(Vec<(ListKey, Value)>),

    /// External input source.
    Input(InputId),

    /// LATEST: concat multiple sources, keep only the most recently changed.
    HoldLatest(Vec<VarId>),

    /// HOLD state: stateful accumulator.
    /// initial + events → new state via transform(old_state, event).
    HoldState {
        initial: VarId,
        events: VarId,
        initial_value: Value,
        transform: HoldTransformFn,
    },

    /// THEN: event-triggered map (positive diffs only).
    Then {
        source: VarId,
        body: TransformFn,
    },

    /// Pure transform on a single source.
    Map {
        source: VarId,
        f: TransformFn,
    },

    /// Pattern matching (WHEN): 0 or 1 output per input.
    FlatMap {
        source: VarId,
        f: FlatMapFn,
    },

    /// Reactive join of two scalar collections.
    /// Used for WHILE, reactive TEXT, reactive arithmetic.
    Join {
        left: VarId,
        right: VarId,
        combine: CombineFn,
    },

    /// Concatenate multiple collections.
    Concat(Vec<VarId>),

    /// List element count (scalar output).
    ListCount(VarId),

    /// List retain with static predicate.
    ListRetain {
        source: VarId,
        predicate: Arc<dyn Fn(&Value) -> bool + 'static>,
    },

    /// List retain with reactive predicate (join list × filter state).
    ListRetainReactive {
        list: VarId,
        filter_state: VarId,
        predicate: Arc<dyn Fn(&Value, &Value) -> bool + 'static>,
    },

    /// Transform each list item.
    ListMap {
        source: VarId,
        f: TransformFn,
    },

    /// Transform each list item with access to the item's key.
    /// Used for injecting per-item link paths (key → link path).
    ListMapWithKey {
        source: VarId,
        f: Arc<dyn Fn(&ListKey, &Value) -> Value + 'static>,
    },

    /// Append items to a list (concat).
    ListAppend {
        list: VarId,
        new_items: VarId,
    },

    /// Remove items from a list by key.
    ListRemove {
        list: VarId,
        remove_keys: VarId,
    },

    /// Per-item stateful accumulator for keyed list elements.
    KeyedHoldState {
        initial: VarId,
        events: VarId,
        transform: HoldTransformFn,
        /// Optional scalar broadcast events (toggle_all, remove_completed).
        broadcasts: Option<VarId>,
        /// Handler called with (all_items, broadcast_event) → per-item updates.
        broadcast_handler: Option<BroadcastHandlerFn>,
    },

    /// Scalar event → keyed pairs (for wildcard event demuxing).
    MapToKeyed {
        source: VarId,
        classify: ClassifyFn,
    },

    /// Scalar trigger → new keyed item with auto-incrementing key.
    AppendNewKeyed {
        source: VarId,
        f: TransformFn,
        initial_counter: usize,
    },

    /// Keyed → scalar list Value::Tagged("List", BTreeMap).
    AssembleList(VarId),

    /// Concat multiple keyed collections.
    KeyedConcat(Vec<VarId>),

    /// Skip first N positive diffs from a collection.
    Skip {
        source: VarId,
        count: usize,
    },

    /// Side effect (e.g., localStorage persistence).
    SideEffect {
        source: VarId,
        effect: SideEffectKind,
    },
}

/// Kinds of side effects.
#[derive(Clone, Debug)]
pub enum SideEffectKind {
    PersistHold { key: String, hold_name: String },
    RouterGoTo,
}
