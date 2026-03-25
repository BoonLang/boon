use crate::ids::ScopeId;
use boon::platform::browser::kernel::{ExprId, KernelValue};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FunctionId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CallSiteId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SourcePortId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MirrorCellId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SinkPortId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ViewSiteId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FunctionInstanceId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FunctionInstanceKey {
    pub function: FunctionId,
    pub call_site: CallSiteId,
    pub parent_scope: ScopeId,
    pub mapped_item_identity: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RetainedNodeKey {
    pub view_site: ViewSiteId,
    pub function_instance: Option<FunctionInstanceId>,
    pub mapped_item_identity: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IrNode {
    pub id: NodeId,
    pub source_expr: Option<ExprId>,
    pub kind: IrNodeKind,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IrFunctionTemplate {
    pub id: FunctionId,
    pub parameter_count: usize,
    pub output: NodeId,
    pub nodes: Vec<IrNode>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct IrProgram {
    pub nodes: Vec<IrNode>,
    pub functions: Vec<IrFunctionTemplate>,
}

impl From<Vec<IrNode>> for IrProgram {
    fn from(nodes: Vec<IrNode>) -> Self {
        Self {
            nodes,
            functions: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum IrNodeKind {
    Literal(KernelValue),
    Parameter {
        index: usize,
    },
    ObjectLiteral {
        fields: Vec<(String, NodeId)>,
    },
    FieldRead {
        object: NodeId,
        field: String,
    },
    Block {
        inputs: Vec<NodeId>,
    },
    Hold {
        seed: NodeId,
        updates: NodeId,
    },
    Then {
        source: NodeId,
        body: NodeId,
    },
    When {
        source: NodeId,
        arms: Vec<MatchArm>,
        fallback: NodeId,
    },
    While {
        source: NodeId,
        arms: Vec<MatchArm>,
        fallback: NodeId,
    },
    Latest {
        inputs: Vec<NodeId>,
    },
    Skip,
    LinkCell,
    LinkRead {
        cell: NodeId,
    },
    LinkBind {
        value: NodeId,
        target: NodeId,
    },
    Add {
        lhs: NodeId,
        rhs: NodeId,
    },
    Sub {
        lhs: NodeId,
        rhs: NodeId,
    },
    Mul {
        lhs: NodeId,
        rhs: NodeId,
    },
    Div {
        lhs: NodeId,
        rhs: NodeId,
    },
    Eq {
        lhs: NodeId,
        rhs: NodeId,
    },
    BoolNot {
        input: NodeId,
    },
    Ge {
        lhs: NodeId,
        rhs: NodeId,
    },
    MathSum {
        input: NodeId,
    },
    MathMin {
        lhs: NodeId,
        rhs: NodeId,
    },
    MathRound {
        input: NodeId,
    },
    TextToNumber {
        input: NodeId,
    },
    TextTrim {
        input: NodeId,
    },
    KeyDownKey {
        input: NodeId,
    },
    KeyDownText {
        input: NodeId,
    },
    TextJoin {
        inputs: Vec<NodeId>,
    },
    Call {
        function: FunctionId,
        call_site: CallSiteId,
        args: Vec<NodeId>,
    },
    ListLiteral {
        items: Vec<NodeId>,
    },
    ListRange {
        from: NodeId,
        to: NodeId,
    },
    ListMap {
        list: NodeId,
        function: FunctionId,
        call_site: CallSiteId,
    },
    ListAppend {
        list: NodeId,
        item: NodeId,
    },
    ListRemoveLast {
        list: NodeId,
        on: NodeId,
    },
    ListMapObjectBoolField {
        list: NodeId,
        field: String,
        value: NodeId,
    },
    ListMapToggleObjectBoolFieldByFieldEq {
        list: NodeId,
        match_field: String,
        match_value: NodeId,
        bool_field: String,
    },
    ListMapObjectFieldByFieldEq {
        list: NodeId,
        match_field: String,
        match_value: NodeId,
        update_field: String,
        update_value: NodeId,
    },
    ListAllObjectBoolField {
        list: NodeId,
        field: String,
    },
    ListRemove {
        list: NodeId,
        predicate: NodeId,
    },
    ListRetain {
        list: NodeId,
        predicate: NodeId,
    },
    ListRetainObjectBoolField {
        list: NodeId,
        field: String,
        keep_if: bool,
    },
    ListRemoveObjectByFieldEq {
        list: NodeId,
        field: String,
        value: NodeId,
    },
    ListCount {
        list: NodeId,
    },
    ListGet {
        list: NodeId,
        index: NodeId,
    },
    ListIsEmpty {
        list: NodeId,
    },
    ListSum {
        list: NodeId,
    },
    SourcePort(SourcePortId),
    MirrorCell(MirrorCellId),
    SinkPort {
        port: SinkPortId,
        input: NodeId,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct MatchArm {
    pub matcher: KernelValue,
    pub result: NodeId,
}
