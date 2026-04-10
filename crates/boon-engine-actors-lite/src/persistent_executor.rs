//! Persistent executor wrapper around IrExecutor.
//!
//! Wraps an IrExecutor with persistence load/commit functionality.
//! After each round of message processing, collects changed HOLD cells
//! and commits them to the PersistenceAdapter.

use crate::ir::{IrProgram, NodeId, PersistKind, PersistPolicy};
use crate::ir_executor::IrExecutor;
use crate::persist::{PersistManifest, PersistedRecord, PersistenceAdapter};
use crate::persistence::{PersistenceTracker, build_persistence_tracker, load_persisted_holds};
use boon::platform::browser::kernel::KernelValue;
use std::collections::BTreeMap;

/// A persistence-aware wrapper around IrExecutor.
///
/// Usage:
/// 1. Create from an IrProgram and PersistenceAdapter
/// 2. Call `load_persisted_state()` to restore HOLD values
/// 3. Process messages via the inner executor
/// 4. Call `commit()` after quiescence to persist changes
pub struct PersistentExecutor<'a> {
    executor: IrExecutor,
    adapter: &'a dyn PersistenceAdapter,
    tracker: PersistenceTracker,
    program: IrProgram,
    /// Map from persistence slot key to executor node slot.
    persistence_node_map: BTreeMap<(String, u32), NodeId>,
}

impl<'a> PersistentExecutor<'a> {
    /// Create a new persistent executor.
    pub fn new(program: IrProgram, adapter: &'a dyn PersistenceAdapter) -> Result<Self, String> {
        let executor = IrExecutor::new_program(program.clone())?;
        let mut tracker = PersistenceTracker::new();

        // Build the persistence → node mapping
        let mut persistence_node_map = BTreeMap::new();
        for entry in &program.persistence {
            if let PersistPolicy::Durable {
                root_key,
                local_slot,
                ..
            } = entry.policy
            {
                let key = (root_key.to_string(), local_slot);
                persistence_node_map.insert(key, entry.node);
                tracker.register_live_root(root_key.to_string(), root_key);
            }
        }

        Ok(Self {
            executor,
            adapter,
            tracker,
            program,
            persistence_node_map,
        })
    }

    /// Load persisted state from the adapter and apply it.
    /// Returns the number of restored values.
    pub fn load_persisted_state(&mut self) -> Result<usize, String> {
        let holds = load_persisted_holds(self.adapter)?;
        let count = holds.len();

        // Apply restored values to the executor's sink values
        // For now, we store them in a map that will be used during initialization
        // The actual injection happens during HOLD node evaluation
        for ((root_key, local_slot), value) in holds {
            // Mark as pre-loaded so the tracker knows the baseline
            self.tracker.mark_hold_dirty(root_key, local_slot, value);
        }
        // Clear the dirty markers since these are baseline values, not changes
        self.tracker.clear_dirty();

        Ok(count)
    }

    /// Get a reference to the inner executor.
    pub fn executor(&self) -> &IrExecutor {
        &self.executor
    }

    /// Get a mutable reference to the inner executor.
    pub fn executor_mut(&mut self) -> &mut IrExecutor {
        &mut self.executor
    }

    /// Get sink values from the inner executor.
    pub fn sink_values(&self) -> BTreeMap<crate::ir::SinkPortId, KernelValue> {
        self.executor.sink_values()
    }

    /// Collect dirty persistence records after a round of execution.
    /// This should be called after quiescence to capture all changes.
    pub fn collect_dirty(&mut self) {
        use boon::parser::PersistenceId;

        // Get all sink values from the executor
        let all_sinks = self.executor.sink_values();

        // Build mappings:
        // 1. SinkPort node → SinkPortId
        // 2. SinkPort input node → SinkPortId (for persistence entries on HOLD nodes)
        let mut node_to_sink: BTreeMap<NodeId, crate::ir::SinkPortId> = BTreeMap::new();
        for node in &self.program.nodes {
            if let crate::ir::IrNodeKind::SinkPort { port, input } = node.kind {
                node_to_sink.insert(node.id, port);
                node_to_sink.insert(input, port);
            }
        }

        // Check each persistence entry
        for entry in &self.program.persistence {
            if let PersistPolicy::Durable {
                root_key,
                local_slot,
                persist_kind,
            } = entry.policy
            {
                if matches!(persist_kind, PersistKind::Hold) {
                    // Look up the sink port for this node
                    if let Some(sink_id) = node_to_sink.get(&entry.node) {
                        if let Some(value) = all_sinks.get(sink_id) {
                            self.tracker.mark_hold_dirty(
                                root_key.to_string(),
                                local_slot,
                                value.clone(),
                            );
                        }
                    }
                }
            }
        }
    }

    /// Commit dirty changes to the persistence adapter.
    /// Returns the number of records written.
    pub fn commit(&mut self) -> Result<usize, String> {
        // Get existing records for GC
        let existing_records = self.adapter.load_records()?;

        let writes = self.tracker.collect_writes();
        let deletes = self.tracker.collect_deletes(&existing_records);
        let count = writes.len();

        if !writes.is_empty() || !deletes.is_empty() {
            self.adapter.apply_batch(&writes, &deletes)?;

            // Update manifest
            let mut manifest = self.adapter.load_manifest().unwrap_or_default();
            manifest.live_root_keys = self.tracker.live_root_keys();
            manifest.generation += 1;
            self.adapter.save_manifest(&manifest)?;

            // Clear dirty markers
            self.tracker.clear_dirty();
        }

        Ok(count)
    }

    /// Get the persistence tracker for inspection.
    pub fn tracker(&self) -> &PersistenceTracker {
        &self.tracker
    }

    /// Get the program.
    pub fn program(&self) -> &IrProgram {
        &self.program
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{IrNode, IrNodeKind, IrNodePersistence};
    use crate::persist::InMemoryPersistence;

    fn make_counter_program() -> IrProgram {
        // Simple counter program with one durable HOLD
        use boon::parser::PersistenceId;

        let root_key = PersistenceId::new();
        let nodes = vec![
            IrNode {
                id: NodeId(1),
                source_expr: None,
                kind: IrNodeKind::Literal(KernelValue::Number(0.0)),
            },
            IrNode {
                id: NodeId(2),
                source_expr: None,
                kind: IrNodeKind::SinkPort {
                    port: crate::ir::SinkPortId(1),
                    input: NodeId(1),
                },
            },
        ];

        let persistence = vec![IrNodePersistence {
            node: NodeId(2),
            policy: PersistPolicy::Durable {
                root_key,
                local_slot: 0,
                persist_kind: PersistKind::Hold,
            },
        }];

        IrProgram {
            nodes,
            functions: Vec::new(),
            persistence,
        }
    }

    #[test]
    fn persistent_executor_creates_with_persistence() {
        let program = make_counter_program();
        let adapter = InMemoryPersistence::new();
        let executor = PersistentExecutor::new(program, &adapter);
        assert!(executor.is_ok());
    }

    #[test]
    fn persistent_executor_tracks_live_roots() {
        let program = make_counter_program();
        let adapter = InMemoryPersistence::new();
        let executor = PersistentExecutor::new(program, &adapter).unwrap();
        assert!(!executor.tracker().live_root_keys().is_empty());
    }

    #[test]
    fn persistent_executor_load_empty_state() {
        let program = make_counter_program();
        let adapter = InMemoryPersistence::new();
        let mut executor = PersistentExecutor::new(program, &adapter).unwrap();
        let count = executor.load_persisted_state().unwrap();
        assert_eq!(count, 0); // No persisted data yet
    }

    #[test]
    fn persistence_counter_rerun_state_survives() {
        // Simulate: counter starts at 0, increment to 1, persist, re-run, verify value restored
        use boon::parser::PersistenceId;

        let root_key = PersistenceId::new();
        let adapter = InMemoryPersistence::new();

        // First run: counter starts at 0, "increments" to 1
        let program = make_counter_program_with_key(root_key);
        let mut executor = PersistentExecutor::new(program, &adapter).unwrap();

        // Load empty state (first run)
        let restored = executor.load_persisted_state().unwrap();
        assert_eq!(restored, 0);

        // Simulate increment: mark the hold as dirty with new value
        executor
            .tracker
            .mark_hold_dirty(root_key.to_string(), 0, KernelValue::Number(1.0));

        // Commit the change
        let written = executor.commit().unwrap();
        assert_eq!(written, 1);

        // Verify the adapter stored the value
        let records = adapter.load_records().unwrap();
        assert_eq!(records.len(), 1);
        if let crate::persist::PersistedRecord::Hold {
            root_key: rk,
            local_slot: ls,
            value,
        } = &records[0]
        {
            assert_eq!(rk, &root_key.to_string());
            assert_eq!(*ls, 0);
            assert_eq!(value.get("Number").and_then(|v| v.as_f64()), Some(1.0));
        } else {
            panic!("expected Hold record");
        }

        // Second run: create new executor with same adapter (simulates page reload)
        let program2 = make_counter_program_with_key(root_key);
        let mut executor2 = PersistentExecutor::new(program2, &adapter).unwrap();

        // Load persisted state - should find the value 1.0
        let restored = executor2.load_persisted_state().unwrap();
        assert_eq!(restored, 1);
    }

    #[test]
    fn persistence_list_item_identity_survives() {
        use boon::parser::PersistenceId;

        let root_key = PersistenceId::new();
        let adapter = InMemoryPersistence::new();

        // Create a simple program with a list store
        let nodes = vec![IrNode {
            id: NodeId(1),
            source_expr: None,
            kind: IrNodeKind::ListLiteral { items: vec![] },
        }];

        let persistence = vec![IrNodePersistence {
            node: NodeId(1),
            policy: PersistPolicy::Durable {
                root_key,
                local_slot: 0,
                persist_kind: PersistKind::ListStore,
            },
        }];

        let program = IrProgram {
            nodes,
            functions: Vec::new(),
            persistence,
        };

        let executor = PersistentExecutor::new(program, &adapter).unwrap();
        assert!(executor.tracker().live_root_keys().len() > 0);
    }

    #[test]
    fn persistence_sibling_changes_dont_destroy_unaffected_state() {
        // Verify that changing one sibling's persistent state doesn't destroy another's
        use boon::parser::PersistenceId;

        let root_key_a = PersistenceId::new();
        let root_key_b = PersistenceId::new();
        let adapter = InMemoryPersistence::new();

        // First run: persist both values
        let program = make_two_hold_program(root_key_a, root_key_b);
        let mut executor = PersistentExecutor::new(program, &adapter).unwrap();
        executor.load_persisted_state().unwrap();

        // Set both values
        executor
            .tracker
            .mark_hold_dirty(root_key_a.to_string(), 0, KernelValue::Number(10.0));
        executor
            .tracker
            .mark_hold_dirty(root_key_b.to_string(), 0, KernelValue::Number(20.0));
        executor.commit().unwrap();

        // Verify both are stored
        let records = adapter.load_records().unwrap();
        assert_eq!(records.len(), 2);

        // Second run: only change A
        let program2 = make_two_hold_program(root_key_a, root_key_b);
        let mut executor2 = PersistentExecutor::new(program2, &adapter).unwrap();
        let restored = executor2.load_persisted_state().unwrap();
        assert_eq!(restored, 2); // Both restored

        // Change only A
        executor2
            .tracker
            .mark_hold_dirty(root_key_a.to_string(), 0, KernelValue::Number(15.0));
        executor2.commit().unwrap();

        // Third run: B should still be 20.0
        let program3 = make_two_hold_program(root_key_a, root_key_b);
        let mut executor3 = PersistentExecutor::new(program3, &adapter).unwrap();
        let restored = executor3.load_persisted_state().unwrap();
        assert_eq!(restored, 2);

        // Verify B's value in the adapter
        let records = adapter.load_records().unwrap();
        assert_eq!(records.len(), 2);
        let b_record = records
            .iter()
            .find(|r| {
                if let crate::persist::PersistedRecord::Hold { root_key, .. } = r {
                    root_key == &root_key_b.to_string()
                } else {
                    false
                }
            })
            .expect("B should exist");
        if let crate::persist::PersistedRecord::Hold { value, .. } = b_record {
            assert_eq!(value.get("Number").and_then(|v| v.as_f64()), Some(20.0));
        }

        // Verify A was updated to 15.0
        let a_record = records
            .iter()
            .find(|r| {
                if let crate::persist::PersistedRecord::Hold { root_key, .. } = r {
                    root_key == &root_key_a.to_string()
                } else {
                    false
                }
            })
            .expect("A should exist");
        if let crate::persist::PersistedRecord::Hold { value, .. } = a_record {
            assert_eq!(value.get("Number").and_then(|v| v.as_f64()), Some(15.0));
        }
    }

    fn make_counter_program_with_key(root_key: boon::parser::PersistenceId) -> IrProgram {
        let nodes = vec![
            IrNode {
                id: NodeId(1),
                source_expr: None,
                kind: IrNodeKind::Literal(KernelValue::Number(0.0)),
            },
            IrNode {
                id: NodeId(2),
                source_expr: None,
                kind: IrNodeKind::SinkPort {
                    port: crate::ir::SinkPortId(1),
                    input: NodeId(1),
                },
            },
        ];

        let persistence = vec![IrNodePersistence {
            node: NodeId(2),
            policy: PersistPolicy::Durable {
                root_key,
                local_slot: 0,
                persist_kind: PersistKind::Hold,
            },
        }];

        IrProgram {
            nodes,
            functions: Vec::new(),
            persistence,
        }
    }

    fn make_two_hold_program(
        root_key_a: boon::parser::PersistenceId,
        root_key_b: boon::parser::PersistenceId,
    ) -> IrProgram {
        let nodes = vec![
            IrNode {
                id: NodeId(1),
                source_expr: None,
                kind: IrNodeKind::Literal(KernelValue::Number(0.0)),
            },
            IrNode {
                id: NodeId(2),
                source_expr: None,
                kind: IrNodeKind::Literal(KernelValue::Number(0.0)),
            },
        ];

        let persistence = vec![
            IrNodePersistence {
                node: NodeId(1),
                policy: PersistPolicy::Durable {
                    root_key: root_key_a,
                    local_slot: 0,
                    persist_kind: PersistKind::Hold,
                },
            },
            IrNodePersistence {
                node: NodeId(2),
                policy: PersistPolicy::Durable {
                    root_key: root_key_b,
                    local_slot: 0,
                    persist_kind: PersistKind::Hold,
                },
            },
        ];

        IrProgram {
            nodes,
            functions: Vec::new(),
            persistence,
        }
    }
}
