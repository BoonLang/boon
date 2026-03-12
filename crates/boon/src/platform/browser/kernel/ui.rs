use std::collections::{HashMap, HashSet};

use super::ids::{ElementId, TickId, TickSeq};
use super::value::KernelValue;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum EventType {
    Click,
    Press,
    KeyDown,
    Change,
    Blur,
    Focus,
    DoubleClick,
    HoverChange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EventPortId {
    pub element: ElementId,
    pub ty: EventType,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EventPortState {
    pub last_pulse: Option<(TickSeq, KernelValue)>,
}

impl EventPortState {
    #[must_use]
    pub const fn new() -> Self {
        Self { last_pulse: None }
    }
}

impl Default for EventPortState {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Default)]
pub struct UiStore {
    event_ports: HashMap<EventPortId, EventPortState>,
    text_inputs: HashMap<ElementId, String>,
    focused: Option<ElementId>,
    hovered: HashSet<ElementId>,
}

impl UiStore {
    pub fn record_event(&mut self, port: EventPortId, seq: TickSeq, payload: KernelValue) {
        self.event_ports.entry(port).or_default().last_pulse = Some((seq, payload));
    }

    #[must_use]
    pub fn read_event_for_tick(&self, port: EventPortId, tick: TickId) -> KernelValue {
        self.event_ports
            .get(&port)
            .and_then(|state| state.last_pulse.as_ref())
            .filter(|(seq, _)| seq.tick == tick)
            .map(|(_, payload)| payload.clone())
            .unwrap_or(KernelValue::Skip)
    }

    #[must_use]
    pub fn event_state(&self, port: EventPortId) -> Option<&EventPortState> {
        self.event_ports.get(&port)
    }

    pub fn set_text(&mut self, element: ElementId, text: impl Into<String>) {
        self.text_inputs.insert(element, text.into());
    }

    #[must_use]
    pub fn text(&self, element: ElementId) -> Option<&str> {
        self.text_inputs.get(&element).map(String::as_str)
    }

    pub fn set_focus(&mut self, element: Option<ElementId>) {
        self.focused = element;
    }

    #[must_use]
    pub const fn focused(&self) -> Option<ElementId> {
        self.focused
    }

    pub fn set_hovered(&mut self, element: ElementId, hovered: bool) {
        if hovered {
            self.hovered.insert(element);
        } else {
            self.hovered.remove(&element);
        }
    }

    #[must_use]
    pub fn is_hovered(&self, element: ElementId) -> bool {
        self.hovered.contains(&element)
    }
}
