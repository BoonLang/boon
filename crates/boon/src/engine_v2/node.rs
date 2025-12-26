use std::collections::HashMap;
use std::sync::Arc;
use super::arena::SlotId;
use super::message::{Payload, FieldId, ItemKey};
use super::address::SourceId;

/// The kind of reactive node and its kind-specific state.
#[derive(Debug, Clone)]
pub enum NodeKind {
    /// Constant value producer (tied signal)
    Producer { value: Option<Payload> },
    /// Named wire (variable forwarding)
    Wire { source: Option<SlotId> },
    /// Object demultiplexer - routes to field slots
    Router { fields: HashMap<FieldId, SlotId> },

    // Phase 4: Combinators

    /// Multi-input combiner (LATEST) - emits when any input changes
    Combiner {
        inputs: Vec<SlotId>,
        last_values: Vec<Option<Payload>>,
    },
    /// State holder (HOLD) - D flip-flop equivalent
    Register {
        stored_value: Option<Payload>,
        body_input: Option<SlotId>,
        /// Initial value input - first emission sets stored_value if Unit
        initial_input: Option<SlotId>,
        /// Whether initial value has been received
        initial_received: bool,
    },
    /// Combinational logic (THEN) - transforms input on arrival
    Transformer {
        input: Option<SlotId>,
        body_slot: Option<SlotId>,  // Output slot of body subgraph
    },
    /// Pattern decoder (WHEN) - matches patterns and routes (one-shot copy)
    PatternMux {
        input: Option<SlotId>,
        /// Currently matched arm index (for forwarding body updates)
        current_arm: Option<usize>,
        /// (pattern, body_slot) pairs
        arms: Vec<(RuntimePattern, SlotId)>,
    },
    /// Tri-state buffer (WHILE) - continuous while pattern matches
    SwitchedWire {
        input: Option<SlotId>,
        current_arm: Option<usize>,
        /// (pattern, body_slot) pairs
        arms: Vec<(RuntimePattern, SlotId)>,
    },

    // Phase 5: Lists

    /// Dynamic wire collection (List) - address decoder
    Bus {
        items: Vec<(ItemKey, SlotId)>,
        alloc_site: AllocSite,
    },

    /// List appender - reactively appends items to a Bus when input emits
    ListAppender {
        /// Target Bus slot
        bus_slot: SlotId,
        /// Input source for new items
        input: Option<SlotId>,
    },

    /// Filtered view of a Bus - stores per-item visibility conditions
    FilteredView {
        /// Source Bus slot
        source_bus: SlotId,
        /// Per-item visibility conditions: item_slot -> condition_slot
        conditions: HashMap<SlotId, SlotId>,
    },

    /// Reactive list mapper - transforms items when source Bus changes
    ListMapper {
        /// Source Bus to watch for new items
        source_bus: SlotId,
        /// Output Bus where transformed items are added
        output_bus: SlotId,
        /// Template input wire - set source to item before cloning
        template_input: SlotId,
        /// Template output slot - the root of the transform subgraph
        template_output: SlotId,
        /// Already-mapped items: source_item_slot -> mapped_item_slot
        mapped_items: HashMap<SlotId, SlotId>,
    },

    // Phase 6: Timer & Effects

    /// Timer node - emits pulses at intervals
    Timer {
        /// Interval in milliseconds
        interval_ms: f64,
        /// Next scheduled tick
        next_tick: u64,
        /// Whether the timer is active
        active: bool,
    },

    /// Pulses node (Stream/pulses) - emits sequential values 0, 1, 2, ..., N-1
    /// Sequential emission allows HOLD body to see updated state between each pulse
    Pulses {
        /// Total number of pulses to emit
        total: u32,
        /// Current pulse index (0-based)
        current: u32,
        /// Whether pulses have started emitting
        started: bool,
    },

    /// Skip node (Stream/skip) - skips first N values from source, then passes through
    Skip {
        /// Source slot to read from
        source: SlotId,
        /// Number of values to skip
        count: u32,
        /// Number of values already skipped
        skipped: u32,
    },

    /// Accumulator node (Math/sum) - sums incoming values
    Accumulator {
        /// Current sum
        sum: f64,
    },

    /// Arithmetic operation node - combines two inputs with an operator
    Arithmetic {
        op: ArithmeticOp,
        left: Option<SlotId>,
        right: Option<SlotId>,
        left_value: Option<f64>,
        right_value: Option<f64>,
    },

    /// Comparison operation node - compares two inputs and produces Bool
    Comparison {
        op: ComparisonOp,
        left: Option<SlotId>,
        right: Option<SlotId>,
        left_value: Option<Payload>,
        right_value: Option<Payload>,
    },

    /// Effect node - executes side effects at tick end
    Effect {
        effect_type: EffectType,
        input: Option<SlotId>,
    },

    /// IO Pad for LINK event binding
    IOPad {
        element_slot: Option<SlotId>,
        event_type: String,
        connected: bool,
    },

    /// Field extractor - extracts a field from a Router payload at runtime
    Extractor {
        source: Option<SlotId>,
        field: FieldId,
        /// Field slot we've subscribed to (for reactive updates)
        subscribed_field: Option<SlotId>,
    },

    // Phase 7d: TextTemplate

    /// Text template with reactive interpolations (TEXT { ... {var} ... })
    TextTemplate {
        /// Template string with placeholders: "Count: {0}, Total: {1}"
        template: String,
        /// Dependencies: SlotIds referenced in interpolations (collected at compile-time)
        dependencies: Vec<SlotId>,
        /// Cached rendered string (updated when any dependency changes)
        cached: Option<Arc<str>>,
    },

    /// List count node - counts items in a Bus at runtime
    ListCount {
        /// Source slot (Wire or Bus)
        source: Option<SlotId>,
    },

    /// List is_empty check - returns Bool
    ListIsEmpty {
        /// Source slot (Wire or Bus)
        source: Option<SlotId>,
    },

    /// Boolean NOT operation - reactive negation
    BoolNot {
        /// Source slot (input boolean)
        source: Option<SlotId>,
        /// Cached result
        cached: Option<bool>,
    },

    /// Text trim operation - reactive whitespace trimming
    TextTrim {
        /// Source slot (input text)
        source: Option<SlotId>,
    },

    /// Text is_not_empty check - returns Bool
    TextIsNotEmpty {
        /// Source slot (input text)
        source: Option<SlotId>,
    },

    /// Test probe - stores last received value for assertions (test-only)
    #[cfg(test)]
    Probe { last: Option<Payload> },
}

/// Allocation site for list items - generates stable ItemKeys.
#[derive(Debug, Clone)]
pub struct AllocSite {
    pub site_source_id: SourceId,
    pub next_instance: u64,
}

impl AllocSite {
    pub fn new(source_id: SourceId) -> Self {
        Self {
            site_source_id: source_id,
            next_instance: 0,
        }
    }

    pub fn allocate(&mut self) -> ItemKey {
        let id = self.next_instance;
        self.next_instance += 1;
        id
    }
}

/// Types of effects that can be executed.
#[derive(Debug, Clone)]
pub enum EffectType {
    LogInfo,
    LogWarn,
    LogError,
    RouterGoTo,
}

/// Arithmetic operation types.
#[derive(Debug, Clone, Copy)]
pub enum ArithmeticOp {
    Add,
    Subtract,
    Multiply,
    Divide,
    Negate,
}

/// Comparison operation types.
#[derive(Debug, Clone, Copy)]
pub enum ComparisonOp {
    Equal,
    NotEqual,
    Greater,
    GreaterOrEqual,
    Less,
    LessOrEqual,
}

/// Runtime pattern for matching against Payload values.
#[derive(Debug, Clone)]
pub enum RuntimePattern {
    /// Match exact value
    Literal(Payload),
    /// Match anything (__ wildcard)
    Wildcard,
    /// Capture value to a binding name (for use in body)
    Binding(String),
    /// Match list with element patterns
    List(Vec<RuntimePattern>),
    /// Match object with field patterns (field_id -> pattern)
    Object(Vec<(FieldId, RuntimePattern)>),
    /// Match tagged object by tag ID (interned)
    Tag(u32),
}

impl RuntimePattern {
    /// Check if this pattern matches the given payload.
    pub fn matches(&self, payload: &Payload) -> bool {
        match (self, payload) {
            // Wildcard matches anything
            (RuntimePattern::Wildcard, _) => true,
            // Binding captures anything
            (RuntimePattern::Binding(_), _) => true,
            // Literal must match exactly
            (RuntimePattern::Literal(pat_val), val) => pat_val == val,
            // Tag matches Payload::Tag by ID
            (RuntimePattern::Tag(pat_tag_id), Payload::Tag(val_tag_id)) => {
                pat_tag_id == val_tag_id
            }
            // Tag also matches TaggedObject by tag field
            (RuntimePattern::Tag(pat_tag_id), Payload::TaggedObject { tag, .. }) => {
                *pat_tag_id == *tag
            }
            // Bool matches True/False literals - we use well-known IDs
            // True is typically interned first, False second
            // But safer: Payload::Bool also has Literal matching below
            (RuntimePattern::Literal(Payload::Bool(pat_b)), Payload::Bool(val_b)) => {
                pat_b == val_b
            }
            // List patterns match ListHandle (but need arena access to check elements - simplified)
            (RuntimePattern::List(_), Payload::ListHandle(_)) => {
                // TODO: Would need arena access to check list elements
                true // Simplified: assume list patterns match list handles
            }
            // Object patterns match ObjectHandle (simplified)
            (RuntimePattern::Object(_), Payload::ObjectHandle(_)) => {
                // TODO: Would need arena access to check object fields
                true // Simplified: assume object patterns match object handles
            }
            _ => false,
        }
    }
}
