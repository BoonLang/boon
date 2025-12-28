//! Slot key for Path B engine.
//!
//! SlotKey = (ScopeId, ExprId) uniquely identifies a value
//! in the reactive graph.

use crate::scope::ScopeId;
use shared::ast::ExprId;

/// Universal address for values/cells
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SlotKey {
    /// The scope this value belongs to
    pub scope: ScopeId,
    /// The expression that produces this value
    pub expr: ExprId,
}

impl SlotKey {
    pub fn new(scope: ScopeId, expr: ExprId) -> Self {
        Self { scope, expr }
    }

    /// Create a root-scope slot key
    pub fn root(expr: ExprId) -> Self {
        Self {
            scope: ScopeId::root(),
            expr,
        }
    }

    /// Create a child slot key with a new expression in the same scope
    pub fn with_expr(&self, expr: ExprId) -> Self {
        Self {
            scope: self.scope.clone(),
            expr,
        }
    }

    /// Create a child slot key with a new scope
    pub fn with_scope(&self, scope: ScopeId) -> Self {
        Self {
            scope,
            expr: self.expr,
        }
    }
}
