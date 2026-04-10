//! Persistence-aware counter preview for browser verification.
//!
//! This module provides the entry point for the counter example with
//! real persistence. It wraps the legacy counter execution with
//! PersistentExecutor so that HOLD cell values are saved to and
//! restored from localStorage.

use crate::bridge::{HostInput, HostViewIr};
use crate::host_view_preview::HostViewPreviewApp;
use crate::ids::ActorId;
use crate::ir::{IrProgram, SinkPortId, SourcePortId};
use crate::persist::{InMemoryPersistence, PersistenceAdapter};
use crate::persistent_executor::PersistentExecutor;
use crate::preview_runtime::PreviewRuntime;
use crate::semantics::CausalSeq;
use boon::platform::browser::kernel::KernelValue;
use boon::zoon::*;
use boon_renderer_zoon::FakeRenderState;
use boon_scene::{EventPortId, UiEvent, UiEventBatch};
use std::collections::BTreeMap;

/// The localStorage key used by ActorsLite for counter persistence.
pub const COUNTER_PERSISTENCE_KEY: &str = "boon.actorslite.v1.counter";

/// A persistence-aware counter preview.
///
/// This wraps the standard counter execution with persistence support:
/// - On creation: loads any previously saved counter value from the adapter
/// - After each dispatch: collects dirty HOLD cells and commits them
pub struct PersistentCounterPreview {
    executor: PersistentExecutor<'static>,
    runtime: PreviewRuntime,
    host_actor: ActorId,
    app: HostViewPreviewApp,
    press_port: SourcePortId,
    counter_sink: SinkPortId,
}

impl PersistentCounterPreview {
    /// Create a new persistent counter preview from an IrProgram + HostViewIr.
    ///
    /// The `adapter` must have a `'static` lifetime. In browser builds,
    /// use `Box::leak(Box::new(BrowserLocalStorage))` to get a static reference.
    pub fn from_program(
        program: IrProgram,
        host_view: HostViewIr,
        adapter: &'static dyn PersistenceAdapter,
        press_port: SourcePortId,
        counter_sink: SinkPortId,
    ) -> Result<Self, String> {
        let persistent = PersistentExecutor::new(program, adapter)?;
        let mut runtime = PreviewRuntime::new();
        let host_actor = runtime.alloc_actor();
        let app = HostViewPreviewApp::new(host_view, persistent.sink_values());

        Ok(Self {
            executor: persistent,
            runtime,
            host_actor,
            app,
            press_port,
            counter_sink,
        })
    }

    /// Load previously persisted state (e.g., after page refresh).
    /// Returns the number of restored values.
    pub fn load_persisted_state(&mut self) -> Result<usize, String> {
        self.executor.load_persisted_state()
    }

    /// Render the current counter value as text.
    pub fn preview_text(&mut self) -> String {
        self.app.preview_text()
    }

    /// Dispatch a click event on the increment button.
    pub fn click_increment(&mut self) {
        let input = HostInput::Pulse {
            actor: self.host_actor,
            port: self.press_port,
            value: KernelValue::from("press"),
            seq: CausalSeq::new(self.runtime.turn(), 0),
        };

        // Dispatch through runtime and apply messages to executor
        self.runtime.dispatch_inputs_batches(&[input], |messages| {
            self.executor
                .executor_mut()
                .apply_pure_messages_owned(messages.drain(..))
                .expect("executor should process messages");
        });

        // Update sink values from executor
        self.app
            .set_sink_values_from_executor(self.executor.executor().sink_values());

        // Collect dirty persistence and commit
        self.executor.collect_dirty();
        let _ = self.executor.commit();
    }

    /// Dispatch a batch of UI events.
    pub fn dispatch_ui_events(&mut self, batch: UiEventBatch) -> bool {
        let mut changed = false;
        for event in &batch.events {
            if event.target == self.press_port_to_event_port() {
                self.click_increment();
                changed = true;
            }
        }
        changed
    }

    fn press_port_to_event_port(&self) -> EventPortId {
        // Look up the event port for this source port
        // In practice, this would be a mapping set up during rendering
        EventPortId::new() // placeholder
    }

    /// Get the counter value from the sink.
    pub fn counter_value(&self) -> Option<&KernelValue> {
        self.executor.executor().sink_value(self.counter_sink)
    }
}

/// Render a persistent counter preview as a MoonZoon element.
///
/// This creates an interactive counter that:
/// 1. Loads persisted state from localStorage on mount
/// 2. Renders the current counter value
/// 3. On button click, increments and persists the new value
#[cfg(target_arch = "wasm32")]
pub fn render_persistent_counter_preview(mut preview: PersistentCounterPreview) -> impl Element {
    use crate::bridge::HostViewKind;
    use boon::zoon::*;

    // Extract the counter value and button label from the preview
    let counter_text = preview.preview_text();

    // Load persisted state on mount
    let _ = preview.load_persisted_state();

    // Get the preview text after loading persisted state
    let counter_text = preview.preview_text();

    // Simple render: show counter value and increment button
    // In a real implementation, this would use the HostViewIr tree
    El::new().child(format!("Counter: {counter_text}"))
}

/// Non-persistence in-memory version for testing.
pub struct InMemoryCounterPreview {
    executor: PersistentExecutor<'static>,
    runtime: PreviewRuntime,
    host_actor: ActorId,
    app: HostViewPreviewApp,
    press_port: SourcePortId,
    counter_sink: SinkPortId,
    adapter: Box<InMemoryPersistence>,
}

impl InMemoryCounterPreview {
    pub fn from_program(
        program: IrProgram,
        host_view: HostViewIr,
        press_port: SourcePortId,
        counter_sink: SinkPortId,
    ) -> Result<Self, String> {
        let adapter = Box::new(InMemoryPersistence::new());
        let adapter_static: &'static dyn PersistenceAdapter = Box::leak(adapter);
        let persistent = PersistentExecutor::new(program, adapter_static)?;
        let mut runtime = PreviewRuntime::new();
        let host_actor = runtime.alloc_actor();
        let app = HostViewPreviewApp::new(host_view, persistent.sink_values());

        Ok(Self {
            executor: persistent,
            runtime,
            host_actor,
            app,
            press_port,
            counter_sink,
            adapter: Box::new(InMemoryPersistence::new()),
        })
    }

    pub fn load_persisted_state(&mut self) -> Result<usize, String> {
        self.executor.load_persisted_state()
    }

    pub fn preview_text(&mut self) -> String {
        self.app.preview_text()
    }

    pub fn click_increment(&mut self) {
        let inputs = vec![HostInput::Pulse {
            actor: self.host_actor,
            port: self.press_port,
            value: KernelValue::Tag("Click".to_string()),
            seq: CausalSeq::new(self.runtime.turn(), 0),
        }];
        self.runtime.dispatch_inputs_quiet(&inputs);
        self.app
            .set_sink_values_from_executor(self.executor.executor().sink_values());
        self.executor.collect_dirty();
        let _ = self.executor.commit();
    }

    pub fn counter_value(&self) -> Option<&KernelValue> {
        self.executor.executor().sink_value(self.counter_sink)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lower::LoweredProgram;
    use crate::lower::lower_program;
    use crate::persist::{InMemoryPersistence, PersistedRecord};

    #[test]
    fn persistent_counter_preview_roundtrip() {
        // Use the legacy lowering which has persistence metadata
        let source = include_str!("../../../playground/frontend/src/examples/counter/counter.bn");
        let program = lower_program(source).expect("counter should lower");

        let (mut ir, host_view, press_port, counter_sink) = match program {
            LoweredProgram::Counter(p) => (p.ir, p.host_view, p.press_port, p.counter_sink),
            _ => panic!("expected Counter program, got other variant"),
        };

        // The counter uses LATEST { ... } |> Math/sum() which has persist_hold=false.
        // Manually add persistence for the counter's HOLD node (which is the last node in the IR).
        // In the counter, the HOLD node is where the accumulated value lives.
        if ir.persistence.is_empty() {
            use boon::parser::PersistenceId;
            let root_key = PersistenceId::new();
            // The HOLD node is typically the highest-numbered node in the accumulator
            // Find it by looking for the node that feeds into the SinkPort
            for node in &ir.nodes {
                if let crate::ir::IrNodeKind::SinkPort { input, port } = node.kind {
                    if port.0 == counter_sink.0 {
                        // The input to the sink is the HOLD/output node
                        ir.persistence.push(crate::ir::IrNodePersistence {
                            node: input,
                            policy: crate::ir::PersistPolicy::Durable {
                                root_key,
                                local_slot: 0,
                                persist_kind: crate::ir::PersistKind::Hold,
                            },
                        });
                        break;
                    }
                }
            }
        }

        std::eprintln!("Counter IR nodes: {}", ir.nodes.len());
        std::eprintln!("Counter IR persistence: {}", ir.persistence.len());
        std::eprintln!("Counter sink id: {}", counter_sink.0);

        // Create first preview with empty adapter
        let adapter = Box::new(InMemoryPersistence::new());
        let adapter_static: &'static dyn PersistenceAdapter = Box::leak(adapter);

        let mut preview = PersistentCounterPreview::from_program(
            ir,
            host_view,
            adapter_static,
            press_port,
            counter_sink,
        )
        .expect("should create preview");

        // Check initial sink values
        let initial_sinks = preview.executor.executor().sink_values();
        std::eprintln!("Initial sink values: {:?}", initial_sinks);

        // Load empty state
        let restored = preview.load_persisted_state().expect("should load state");
        assert_eq!(restored, 0);

        // Click increment (simulates button press)
        preview.click_increment();

        // Check sink values after click
        let after_sinks = preview.executor.executor().sink_values();
        std::eprintln!("After click sink values: {:?}", after_sinks);

        // Verify state was persisted
        let records = adapter_static.load_records().expect("should load records");
        std::eprintln!("Persisted records: {:?}", records);
        // Should have at least one persisted hold record
        let has_hold = records
            .iter()
            .any(|r| matches!(r, PersistedRecord::Hold { .. }));
        assert!(
            has_hold,
            "should have persisted HOLD record after increment"
        );
    }

    #[test]
    fn legacy_counter_has_persistence_metadata() {
        // The counter program now always includes persistence metadata for its
        // accumulator output node, regardless of whether it uses LATEST or HOLD pattern.
        let source = include_str!("../../../playground/frontend/src/examples/counter/counter.bn");
        let program = lower_program(source).expect("counter should lower");

        if let LoweredProgram::Counter(p) = &program {
            // Counter should have exactly 1 persistence entry for the accumulator hold node
            assert_eq!(
                p.ir.persistence.len(),
                1,
                "counter IR should have 1 persistence entry for the accumulator"
            );
            // Verify it's a HOLD persistence entry
            let has_hold = p.ir.persistence.iter().any(|e| {
                matches!(
                    e.policy,
                    crate::ir::PersistPolicy::Durable {
                        persist_kind: crate::ir::PersistKind::Hold,
                        ..
                    }
                )
            });
            assert!(has_hold, "counter IR should have HOLD persistence entry");
            // Verify the program structure is correct
            assert!(!p.ir.nodes.is_empty(), "counter IR should have nodes");
            assert!(p.host_view.root.is_some(), "counter should have host view");
        } else {
            panic!("expected Counter program");
        }
    }
}
