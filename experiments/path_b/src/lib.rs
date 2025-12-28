//! Path B: Re-evaluation + No Cloning (Instance VM)
//!
//! This prototype eliminates subgraph cloning entirely by
//! evaluating the same code in different scopes. State lives
//! in cells keyed by (ScopeId, ExprId).

pub mod cache;
pub mod cell;
pub mod diagnostics;
pub mod evaluator;
pub mod runtime;
pub mod scope;
pub mod slot;
pub mod tick;
pub mod value;

pub use runtime::Runtime;
pub use shared::test_harness::Value;

/// The Path B engine wrapper
pub struct Engine {
    runtime: Runtime,
}

impl Engine {
    pub fn new_from_program(program: &shared::ast::Program) -> Self {
        let mut runtime = Runtime::new(program.clone());
        runtime.tick(); // Initial evaluation
        Self { runtime }
    }

    /// Get the runtime for debugging
    pub fn runtime(&self) -> &Runtime {
        &self.runtime
    }
}

impl shared::test_harness::Engine for Engine {
    fn new(program: &shared::ast::Program) -> Self {
        Self::new_from_program(program)
    }

    fn inject(&mut self, path: &str, payload: Value) {
        self.runtime.inject(path, payload);
    }

    fn tick(&mut self) {
        self.runtime.tick();
    }

    fn read(&self, path: &str) -> Value {
        self.runtime.read(path)
    }
}
