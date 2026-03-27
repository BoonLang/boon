use crate::{FabricSeq, FabricTick, FabricValue};
use boon_scene::{EventPortId, NodeId};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FabricUiEventState {
    pub last_pulse: Option<(FabricSeq, FabricValue)>,
}

impl FabricUiEventState {
    #[must_use]
    pub const fn new() -> Self {
        Self { last_pulse: None }
    }
}

impl Default for FabricUiEventState {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Default)]
pub struct FabricUiStore {
    event_ports: HashMap<EventPortId, FabricUiEventState>,
    text_inputs: HashMap<NodeId, String>,
    focused: Option<NodeId>,
}

impl FabricUiStore {
    pub fn record_event(&mut self, port: EventPortId, seq: FabricSeq, payload: FabricValue) {
        self.event_ports.entry(port).or_default().last_pulse = Some((seq, payload));
    }

    #[must_use]
    pub fn read_event_for_tick(&self, port: EventPortId, tick: FabricTick) -> FabricValue {
        self.event_ports
            .get(&port)
            .and_then(|state| state.last_pulse.as_ref())
            .filter(|(seq, _)| seq.tick == tick)
            .map(|(_, payload)| payload.clone())
            .unwrap_or(FabricValue::Skip)
    }

    #[must_use]
    pub fn event_state(&self, port: EventPortId) -> Option<&FabricUiEventState> {
        self.event_ports.get(&port)
    }

    pub fn set_text(&mut self, element: NodeId, text: impl Into<String>) {
        self.text_inputs.insert(element, text.into());
    }

    #[must_use]
    pub fn text(&self, element: NodeId) -> Option<&str> {
        self.text_inputs.get(&element).map(String::as_str)
    }

    pub fn set_focus(&mut self, element: Option<NodeId>) {
        self.focused = element;
    }

    #[must_use]
    pub const fn focused(&self) -> Option<NodeId> {
        self.focused
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use boon::platform::browser::kernel::{
        ElementId, EventPortId as KernelEventPortId, EventType, KernelValue, ScopeId, SourceId,
        TickId, TickSeq, UiStore,
    };

    fn to_kernel_value(value: &FabricValue) -> KernelValue {
        match value {
            FabricValue::Number(value) => KernelValue::Number(*value as f64),
            FabricValue::Text(value) => KernelValue::Text(value.clone()),
            FabricValue::Bool(value) => KernelValue::Bool(*value),
            FabricValue::Skip => KernelValue::Skip,
        }
    }

    #[test]
    fn ui_event_values_are_visible_only_in_the_matching_tick() {
        let mut ui = FabricUiStore::default();
        let port = EventPortId::new();
        let seq = FabricSeq {
            tick: FabricTick(7),
            order: 1,
        };
        ui.record_event(port, seq, FabricValue::from("payload"));

        assert_eq!(
            ui.read_event_for_tick(port, FabricTick(7)),
            FabricValue::from("payload")
        );
        assert_eq!(
            ui.read_event_for_tick(port, FabricTick(8)),
            FabricValue::Skip
        );
    }

    #[test]
    fn ui_store_matches_kernel_for_text_focus_and_event_visibility() {
        let mut fabric = FabricUiStore::default();
        let mut kernel = UiStore::default();
        let fabric_port = EventPortId::new();
        let kernel_element = ElementId::new(SourceId(7), ScopeId::ROOT, 1);
        let kernel_port = KernelEventPortId {
            element: kernel_element,
            ty: EventType::KeyDown,
        };
        let fabric_node = NodeId::new();
        let seq = FabricSeq {
            tick: FabricTick(5),
            order: 1,
        };

        fabric.record_event(fabric_port, seq, FabricValue::from("typed"));
        kernel.record_event(
            kernel_port,
            TickSeq::new(TickId(5), 1),
            KernelValue::from("typed"),
        );

        fabric.set_text(fabric_node, "hello");
        kernel.set_text(kernel_element, "hello");
        fabric.set_focus(Some(fabric_node));
        kernel.set_focus(Some(kernel_element));

        assert_eq!(
            to_kernel_value(&fabric.read_event_for_tick(fabric_port, FabricTick(5))),
            kernel.read_event_for_tick(kernel_port, TickId(5))
        );
        assert_eq!(
            to_kernel_value(&fabric.read_event_for_tick(fabric_port, FabricTick(6))),
            kernel.read_event_for_tick(kernel_port, TickId(6))
        );
        assert_eq!(fabric.text(fabric_node), kernel.text(kernel_element));
        assert_eq!(fabric.focused().is_some(), kernel.focused().is_some());
    }

    #[test]
    fn randomized_ui_traces_preserve_kernel_equivalence() {
        #[derive(Clone)]
        struct TraceRng(u64);

        impl TraceRng {
            fn new(seed: u64) -> Self {
                Self(seed)
            }

            fn next_u32(&mut self) -> u32 {
                self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1);
                (self.0 >> 32) as u32
            }

            fn next_range(&mut self, upper: u32) -> u32 {
                self.next_u32() % upper
            }
        }

        let mut fabric = FabricUiStore::default();
        let mut kernel = UiStore::default();
        let fabric_port = EventPortId::new();
        let fabric_node = NodeId::new();
        let kernel_element = ElementId::new(SourceId(8), ScopeId::ROOT, 2);
        let kernel_port = KernelEventPortId {
            element: kernel_element,
            ty: EventType::Change,
        };
        let mut rng = TraceRng::new(0xFACE_u64);

        for tick in 0..48_u64 {
            match rng.next_range(3) {
                0 => {
                    let payload = format!("value-{}", rng.next_range(10));
                    let seq = FabricSeq {
                        tick: FabricTick(tick),
                        order: rng.next_range(4),
                    };
                    fabric.record_event(fabric_port, seq, FabricValue::from(payload.clone()));
                    kernel.record_event(
                        kernel_port,
                        TickSeq::new(TickId(tick), seq.order),
                        KernelValue::from(payload),
                    );
                }
                1 => {
                    let text = format!("draft-{}", rng.next_range(10));
                    fabric.set_text(fabric_node, text.clone());
                    kernel.set_text(kernel_element, text);
                }
                _ => {
                    let focused = rng.next_range(2) == 0;
                    fabric.set_focus(focused.then_some(fabric_node));
                    kernel.set_focus(focused.then_some(kernel_element));
                }
            }

            assert_eq!(
                to_kernel_value(&fabric.read_event_for_tick(fabric_port, FabricTick(tick))),
                kernel.read_event_for_tick(kernel_port, TickId(tick))
            );
            assert_eq!(fabric.text(fabric_node), kernel.text(kernel_element));
            assert_eq!(fabric.focused().is_some(), kernel.focused().is_some());
        }
    }
}
