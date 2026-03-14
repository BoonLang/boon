use std::collections::BTreeMap;

use boon_scene::UiEventKind;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticProgram {
    pub root: SemanticNode,
    pub runtime: RuntimeModel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SemanticNode {
    Fragment(Vec<SemanticNode>),
    Keyed {
        key: u64,
        node: Box<SemanticNode>,
    },
    Element {
        tag: String,
        text: Option<String>,
        properties: Vec<(String, String)>,
        input_value: Option<SemanticInputValue>,
        style_fragments: Vec<SemanticStyleFragment>,
        event_bindings: Vec<SemanticEventBinding>,
        fact_bindings: Vec<SemanticFactBinding>,
        children: Vec<SemanticNode>,
    },
    Text(String),
    TextTemplate {
        parts: Vec<SemanticTextPart>,
        value: String,
    },
    TextBindingBranch {
        binding: String,
        invert: bool,
        truthy: Box<SemanticNode>,
        falsy: Box<SemanticNode>,
    },
    BoolBranch {
        binding: String,
        truthy: Box<SemanticNode>,
        falsy: Box<SemanticNode>,
    },
    ScalarCompareBranch {
        left: DerivedScalarOperand,
        op: IntCompareOp,
        right: DerivedScalarOperand,
        truthy: Box<SemanticNode>,
        falsy: Box<SemanticNode>,
    },
    ObjectScalarCompareBranch {
        left: ObjectDerivedScalarOperand,
        op: IntCompareOp,
        right: ObjectDerivedScalarOperand,
        truthy: Box<SemanticNode>,
        falsy: Box<SemanticNode>,
    },
    ObjectBoolFieldBranch {
        field: String,
        truthy: Box<SemanticNode>,
        falsy: Box<SemanticNode>,
    },
    ObjectTextFieldBranch {
        field: String,
        invert: bool,
        truthy: Box<SemanticNode>,
        falsy: Box<SemanticNode>,
    },
    ListEmptyBranch {
        binding: String,
        object_items: bool,
        invert: bool,
        truthy: Box<SemanticNode>,
        falsy: Box<SemanticNode>,
    },
    ScalarValue {
        binding: String,
        value: i64,
    },
    ObjectFieldValue {
        field: String,
    },
    TextList {
        binding: String,
        values: Vec<String>,
        filter: Option<TextListFilter>,
        template: TextListTemplate,
    },
    ObjectList {
        binding: String,
        filter: Option<ObjectListFilter>,
        item_actions: Vec<ObjectItemActionSpec>,
        template: Box<SemanticNode>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SemanticTextPart {
    Static(String),
    TextBinding(String),
    ScalarBinding(String),
    ObjectFieldBinding(String),
    ListCountBinding(String),
    ObjectListCountBinding(String),
    FilteredListCountBinding {
        binding: String,
        filter: TextListFilter,
    },
    FilteredObjectListCountBinding {
        binding: String,
        filter: ObjectListFilter,
    },
    BoolBindingText {
        binding: String,
        true_text: String,
        false_text: String,
    },
    ObjectBoolFieldText {
        field: String,
        true_text: String,
        false_text: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SemanticInputValue {
    Static(String),
    TextParts {
        parts: Vec<SemanticTextPart>,
        value: String,
    },
    TextBindingBranch {
        binding: String,
        invert: bool,
        truthy: Box<SemanticInputValue>,
        falsy: Box<SemanticInputValue>,
    },
    ObjectTextFieldBranch {
        field: String,
        invert: bool,
        truthy: Box<SemanticInputValue>,
        falsy: Box<SemanticInputValue>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextListFilter {
    IntCompare { op: IntCompareOp, value: i64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntCompareOp {
    Equal,
    NotEqual,
    Greater,
    GreaterOrEqual,
    Less,
    LessOrEqual,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextListTemplate {
    pub tag: String,
    pub properties: Vec<(String, String)>,
    pub prefix: String,
    pub suffix: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObjectListFilter {
    BoolFieldEquals { field: String, value: bool },
    SelectedCompletedByScalar { binding: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectListItem {
    pub id: u64,
    pub title: String,
    pub completed: bool,
    pub text_fields: BTreeMap<String, String>,
    pub bool_fields: BTreeMap<String, bool>,
    pub scalar_fields: BTreeMap<String, i64>,
    pub object_lists: BTreeMap<String, Vec<ObjectListItem>>,
    pub nested_item_actions: BTreeMap<String, Vec<ObjectItemActionSpec>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectItemActionSpec {
    pub source_binding_suffix: String,
    pub kind: UiEventKind,
    pub action: ObjectItemActionKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObjectItemActionKind {
    ToggleBoolField {
        field: String,
    },
    UpdateNestedObjectLists {
        updates: Vec<NestedObjectListAction>,
    },
    SetBoolField {
        field: String,
        value: bool,
        payload_filter: Option<String>,
    },
    SetTitle {
        trim: bool,
        reject_empty: bool,
        payload_filter: Option<String>,
    },
    UpdateBindings {
        scalar_updates: Vec<ItemScalarUpdate>,
        text_updates: Vec<ItemTextUpdate>,
        payload_filter: Option<String>,
    },
    RemoveSelf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NestedObjectListAction {
    AppendObject { field: String, item: ObjectListItem },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemScalarUpdate {
    SetStatic { binding: String, value: i64 },
    SetFromField { binding: String, field: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemTextUpdate {
    SetStatic {
        binding: String,
        value: String,
    },
    SetFromField {
        binding: String,
        field: String,
    },
    SetFromPayload {
        binding: String,
    },
    SetFromInputSource {
        binding: String,
        source_suffix: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticEventBinding {
    pub kind: UiEventKind,
    pub source_binding: Option<String>,
    pub action: Option<SemanticAction>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SemanticFactKind {
    Hovered,
    Focused,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticFactBinding {
    pub kind: SemanticFactKind,
    pub binding: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SemanticStyleFragment {
    Static(Option<String>),
    BoolBinding {
        binding: String,
        truthy: Box<SemanticStyleFragment>,
        falsy: Box<SemanticStyleFragment>,
    },
    ScalarCompare {
        left: DerivedScalarOperand,
        op: IntCompareOp,
        right: DerivedScalarOperand,
        truthy: Box<SemanticStyleFragment>,
        falsy: Box<SemanticStyleFragment>,
    },
    ObjectBoolField {
        field: String,
        truthy: Box<SemanticStyleFragment>,
        falsy: Box<SemanticStyleFragment>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SemanticAction {
    UpdateScalars {
        updates: Vec<ScalarUpdate>,
    },
    UpdateTexts {
        updates: Vec<TextUpdate>,
    },
    UpdateTextLists {
        updates: Vec<TextListUpdate>,
    },
    UpdateObjectLists {
        updates: Vec<ObjectListUpdate>,
    },
    UpdateNestedObjectLists {
        parent_binding: String,
        parent_item_id: u64,
        updates: Vec<NestedObjectListUpdate>,
    },
    Batch {
        actions: Vec<SemanticAction>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeModel {
    Static,
    Scalars(ScalarRuntimeModel),
    State(StateRuntimeModel),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScalarRuntimeModel {
    pub values: BTreeMap<String, i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DerivedScalarOperand {
    Binding(String),
    Literal(i64),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObjectDerivedScalarOperand {
    Binding(String),
    Field(String),
    Literal(i64),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DerivedArithmeticOp {
    Add,
    Subtract,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DerivedScalarSpec {
    TextListCount {
        binding: String,
        filter: Option<TextListFilter>,
        target: String,
    },
    ObjectListCount {
        binding: String,
        filter: Option<ObjectListFilter>,
        target: String,
    },
    Arithmetic {
        target: String,
        op: DerivedArithmeticOp,
        left: DerivedScalarOperand,
        right: DerivedScalarOperand,
    },
    Comparison {
        target: String,
        op: IntCompareOp,
        left: DerivedScalarOperand,
        right: DerivedScalarOperand,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StateRuntimeModel {
    pub scalar_values: BTreeMap<String, i64>,
    pub text_values: BTreeMap<String, String>,
    pub text_lists: BTreeMap<String, Vec<String>>,
    pub object_lists: BTreeMap<String, Vec<ObjectListItem>>,
    pub input_texts: BTreeMap<String, String>,
    pub derived_scalars: Vec<DerivedScalarSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScalarUpdate {
    Set {
        binding: String,
        value: i64,
    },
    SetFiltered {
        binding: String,
        value: i64,
        payload_filter: String,
    },
    Add {
        binding: String,
        delta: i64,
    },
    ToggleBool {
        binding: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextUpdate {
    SetStatic {
        binding: String,
        value: String,
        payload_filter: Option<String>,
    },
    SetFromInput {
        binding: String,
        source_binding: String,
        payload_filter: Option<String>,
    },
    SetFromPayload {
        binding: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextListUpdate {
    AppendDraftText {
        binding: String,
        source_binding: String,
        key: Option<String>,
        trim: bool,
        reject_empty: bool,
        clear_draft: bool,
    },
    Clear {
        binding: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObjectListUpdate {
    AppendObject {
        binding: String,
        item: ObjectListItem,
    },
    AppendDraftObject {
        binding: String,
        source_binding: String,
        key: Option<String>,
        trim: bool,
        reject_empty: bool,
        clear_draft: bool,
        bool_fields: BTreeMap<String, bool>,
    },
    ToggleBoolField {
        binding: String,
        item_id: u64,
        field: String,
    },
    SetBoolField {
        binding: String,
        item_id: u64,
        field: String,
        value: bool,
        payload_filter: Option<String>,
    },
    SetTitle {
        binding: String,
        item_id: u64,
        trim: bool,
        reject_empty: bool,
        payload_filter: Option<String>,
    },
    ToggleAllBoolField {
        binding: String,
        field: String,
    },
    RemoveItem {
        binding: String,
        item_id: u64,
    },
    RemoveMatching {
        binding: String,
        filter: ObjectListFilter,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NestedObjectListUpdate {
    AppendObject { field: String, item: ObjectListItem },
    RemoveItem { field: String, item_id: u64 },
}

impl SemanticNode {
    #[must_use]
    pub fn keyed(key: u64, node: SemanticNode) -> Self {
        Self::Keyed {
            key,
            node: Box::new(node),
        }
    }

    #[must_use]
    pub fn element(
        tag: impl Into<String>,
        text: Option<String>,
        properties: Vec<(String, String)>,
        event_bindings: Vec<SemanticEventBinding>,
        children: Vec<SemanticNode>,
    ) -> Self {
        Self::Element {
            tag: tag.into(),
            text,
            properties,
            input_value: None,
            style_fragments: Vec::new(),
            event_bindings,
            fact_bindings: Vec::new(),
            children,
        }
    }

    #[must_use]
    pub fn element_with_facts(
        tag: impl Into<String>,
        text: Option<String>,
        properties: Vec<(String, String)>,
        event_bindings: Vec<SemanticEventBinding>,
        fact_bindings: Vec<SemanticFactBinding>,
        children: Vec<SemanticNode>,
    ) -> Self {
        Self::Element {
            tag: tag.into(),
            text,
            properties,
            input_value: None,
            style_fragments: Vec::new(),
            event_bindings,
            fact_bindings,
            children,
        }
    }

    #[must_use]
    pub fn element_with_facts_and_styles(
        tag: impl Into<String>,
        text: Option<String>,
        properties: Vec<(String, String)>,
        style_fragments: Vec<SemanticStyleFragment>,
        event_bindings: Vec<SemanticEventBinding>,
        fact_bindings: Vec<SemanticFactBinding>,
        children: Vec<SemanticNode>,
    ) -> Self {
        Self::Element {
            tag: tag.into(),
            text,
            properties,
            input_value: None,
            style_fragments,
            event_bindings,
            fact_bindings,
            children,
        }
    }

    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text(text.into())
    }

    #[must_use]
    pub fn text_template(parts: Vec<SemanticTextPart>, value: impl Into<String>) -> Self {
        Self::TextTemplate {
            parts,
            value: value.into(),
        }
    }

    #[must_use]
    pub fn bool_branch(
        binding: impl Into<String>,
        truthy: SemanticNode,
        falsy: SemanticNode,
    ) -> Self {
        Self::BoolBranch {
            binding: binding.into(),
            truthy: Box::new(truthy),
            falsy: Box::new(falsy),
        }
    }

    #[must_use]
    pub fn text_binding_branch(
        binding: impl Into<String>,
        invert: bool,
        truthy: SemanticNode,
        falsy: SemanticNode,
    ) -> Self {
        Self::TextBindingBranch {
            binding: binding.into(),
            invert,
            truthy: Box::new(truthy),
            falsy: Box::new(falsy),
        }
    }

    #[must_use]
    pub fn scalar_compare_branch(
        left: DerivedScalarOperand,
        op: IntCompareOp,
        right: DerivedScalarOperand,
        truthy: SemanticNode,
        falsy: SemanticNode,
    ) -> Self {
        Self::ScalarCompareBranch {
            left,
            op,
            right,
            truthy: Box::new(truthy),
            falsy: Box::new(falsy),
        }
    }

    #[must_use]
    pub fn object_scalar_compare_branch(
        left: ObjectDerivedScalarOperand,
        op: IntCompareOp,
        right: ObjectDerivedScalarOperand,
        truthy: SemanticNode,
        falsy: SemanticNode,
    ) -> Self {
        Self::ObjectScalarCompareBranch {
            left,
            op,
            right,
            truthy: Box::new(truthy),
            falsy: Box::new(falsy),
        }
    }

    #[must_use]
    pub fn list_empty_branch(
        binding: impl Into<String>,
        object_items: bool,
        invert: bool,
        truthy: SemanticNode,
        falsy: SemanticNode,
    ) -> Self {
        Self::ListEmptyBranch {
            binding: binding.into(),
            object_items,
            invert,
            truthy: Box::new(truthy),
            falsy: Box::new(falsy),
        }
    }

    #[must_use]
    pub fn object_bool_field_branch(
        field: impl Into<String>,
        truthy: SemanticNode,
        falsy: SemanticNode,
    ) -> Self {
        Self::ObjectBoolFieldBranch {
            field: field.into(),
            truthy: Box::new(truthy),
            falsy: Box::new(falsy),
        }
    }

    #[must_use]
    pub fn object_text_field_branch(
        field: impl Into<String>,
        invert: bool,
        truthy: SemanticNode,
        falsy: SemanticNode,
    ) -> Self {
        Self::ObjectTextFieldBranch {
            field: field.into(),
            invert,
            truthy: Box::new(truthy),
            falsy: Box::new(falsy),
        }
    }

    #[must_use]
    pub fn text_list(
        binding: impl Into<String>,
        values: Vec<String>,
        filter: Option<TextListFilter>,
        template: TextListTemplate,
    ) -> Self {
        Self::TextList {
            binding: binding.into(),
            values,
            filter,
            template,
        }
    }

    #[must_use]
    pub fn object_list(
        binding: impl Into<String>,
        filter: Option<ObjectListFilter>,
        item_actions: Vec<ObjectItemActionSpec>,
        template: SemanticNode,
    ) -> Self {
        Self::ObjectList {
            binding: binding.into(),
            filter,
            item_actions,
            template: Box::new(template),
        }
    }
}

#[must_use]
pub fn bootstrap_runtime_scaffold(
    source_len: usize,
    external_functions: usize,
    persistence_enabled: bool,
    event_batches: usize,
    fact_batches: usize,
    last_event: Option<&str>,
    last_fact: Option<&str>,
    draft_text: &str,
    focused: bool,
) -> SemanticProgram {
    SemanticProgram {
        root: SemanticNode::element(
            "section",
            Some("Wasm runtime scaffold".to_string()),
            Vec::new(),
            Vec::new(),
            vec![
                SemanticNode::element(
                    "button",
                    Some("Dispatch click batch".to_string()),
                    Vec::new(),
                    vec![SemanticEventBinding {
                        kind: UiEventKind::Click,
                        source_binding: None,
                        action: None,
                    }],
                    Vec::new(),
                ),
                SemanticNode::element(
                    "input",
                    None,
                    vec![
                        ("type".to_string(), "text".to_string()),
                        (
                            "placeholder".to_string(),
                            "Type to send event + fact batches".to_string(),
                        ),
                        ("value".to_string(), draft_text.to_string()),
                    ],
                    vec![
                        SemanticEventBinding {
                            kind: UiEventKind::Focus,
                            source_binding: None,
                            action: None,
                        },
                        SemanticEventBinding {
                            kind: UiEventKind::Blur,
                            source_binding: None,
                            action: None,
                        },
                        SemanticEventBinding {
                            kind: UiEventKind::Input,
                            source_binding: None,
                            action: None,
                        },
                    ],
                    Vec::new(),
                ),
                SemanticNode::text(format!("Source bytes: {source_len}")),
                SemanticNode::text(format!("External functions: {external_functions}")),
                SemanticNode::text(format!(
                    "Persistence: {}",
                    if persistence_enabled {
                        "enabled"
                    } else {
                        "disabled"
                    }
                )),
                SemanticNode::text(format!("Event batches: {event_batches}")),
                SemanticNode::text(format!("Fact batches: {fact_batches}")),
                SemanticNode::text(format!("Last event: {}", last_event.unwrap_or("none"))),
                SemanticNode::text(format!("Last fact: {}", last_fact.unwrap_or("none"))),
                SemanticNode::text(format!("Focused: {}", if focused { "yes" } else { "no" })),
            ],
        ),
        runtime: RuntimeModel::Static,
    }
}
