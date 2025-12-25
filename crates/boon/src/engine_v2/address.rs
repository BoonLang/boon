use serde::{Deserialize, Serialize};

/// Parse-time stable identifier for AST nodes.
/// Survives whitespace/comment changes via structural hash.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SourceId {
    pub stable_id: u64,    // Structural hash
    pub parse_order: u32,  // For debugging and collision tiebreaking
}

impl Default for SourceId {
    fn default() -> Self {
        Self { stable_id: 0, parse_order: 0 }
    }
}

/// Runtime scope identifier - captures dynamic instantiation context.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ScopeId(pub u64);

impl ScopeId {
    pub const ROOT: Self = Self(0);

    pub fn child(&self, discriminator: u64) -> Self {
        Self(self.0.wrapping_mul(31).wrapping_add(discriminator))
    }
}

impl Default for ScopeId {
    fn default() -> Self {
        Self::ROOT
    }
}

/// Execution domain - where a node runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum Domain {
    #[default]
    Main,           // UI thread (browser main, or single-threaded mode)
    Worker(u8),     // WebWorker index
    Server,         // Backend (future: over WebSocket)
}

/// Port identifier for multi-input/output nodes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Port {
    Output,           // Default output
    Input(u8),        // Numbered input (for LATEST, etc.)
    Field(u32),       // Field ID (for Router/Object)
}

impl Port {
    /// Extract the input index from an Input port, panics for other variants.
    pub fn input_index(&self) -> usize {
        match self {
            Port::Input(i) => *i as usize,
            _ => panic!("input_index() called on non-Input port: {:?}", self),
        }
    }
}

impl Default for Port {
    fn default() -> Self {
        Self::Output
    }
}

/// Full address of a reactive node port.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct NodeAddress {
    pub domain: Domain,
    pub source_id: SourceId,
    pub scope_id: ScopeId,
    pub port: Port,
}

impl NodeAddress {
    pub fn new(source_id: SourceId, scope_id: ScopeId) -> Self {
        Self {
            domain: Domain::default(),
            source_id,
            scope_id,
            port: Port::Output,
        }
    }

    pub fn with_port(mut self, port: Port) -> Self {
        self.port = port;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_id_child_chain() {
        let root = ScopeId::ROOT;
        let child1 = root.child(1);
        let child2 = root.child(2);
        let grandchild = child1.child(1);

        assert_ne!(root, child1);
        assert_ne!(child1, child2);
        assert_ne!(child1, grandchild);
    }

    #[test]
    fn node_address_equality() {
        let addr1 = NodeAddress::new(
            SourceId { stable_id: 42, parse_order: 1 },
            ScopeId(100),
        );
        let addr2 = addr1.with_port(Port::Input(0));

        assert_ne!(addr1, addr2);
    }
}
