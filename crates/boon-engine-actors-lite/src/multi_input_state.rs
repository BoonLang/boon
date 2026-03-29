use crate::host_view_preview::HostViewPreviewApp;
use crate::ir::SourcePortId;
use boon_scene::{UiEventBatch, UiEventKind};

pub struct MultiInputState<const N: usize> {
    input_ports: [SourcePortId; N],
    input_values: [String; N],
}

impl<const N: usize> MultiInputState<N> {
    #[must_use]
    pub fn new(input_ports: [SourcePortId; N]) -> Self {
        Self {
            input_ports,
            input_values: std::array::from_fn(|_| String::new()),
        }
    }

    #[must_use]
    pub fn with_values(input_ports: [SourcePortId; N], input_values: [String; N]) -> Self {
        Self {
            input_ports,
            input_values,
        }
    }

    #[must_use]
    pub fn input(&self, index: usize) -> &str {
        self.input_values[index].as_str()
    }

    pub fn set_input(&mut self, index: usize, next: impl Into<String>) -> bool {
        let next = next.into();
        if self.input_values[index] == next {
            return false;
        }
        self.input_values[index] = next;
        true
    }

    pub fn clear_input(&mut self, index: usize) -> bool {
        if self.input_values[index].is_empty() {
            return false;
        }
        self.input_values[index].clear();
        true
    }

    pub(crate) fn dispatch_ui_events(
        &mut self,
        app: &HostViewPreviewApp,
        batch: &UiEventBatch,
    ) -> bool {
        let event_ports = self
            .input_ports()
            .iter()
            .enumerate()
            .filter_map(|(index, port)| {
                app.event_port_for_source(*port)
                    .map(|event_port| (index, event_port))
            })
            .collect::<Vec<_>>();

        let mut changed = false;
        for event in &batch.events {
            if !matches!(event.kind, UiEventKind::Input | UiEventKind::Change) {
                continue;
            }
            let Some((index, _event_port)) = event_ports
                .iter()
                .find(|(_, event_port)| *event_port == event.target)
            else {
                continue;
            };
            changed |= self.set_input(*index, event.payload.clone().unwrap_or_default());
        }

        changed
    }

    #[must_use]
    pub(crate) fn input_ports(&self) -> &[SourcePortId; N] {
        &self.input_ports
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::{HostViewIr, HostViewKind, HostViewNode};
    use crate::ir::{FunctionInstanceId, RetainedNodeKey, SinkPortId, ViewSiteId};
    use boon::platform::browser::kernel::KernelValue;
    use boon_scene::{UiEvent, UiNode};
    use std::collections::BTreeMap;

    fn app(input_ports: [SourcePortId; 2]) -> HostViewPreviewApp {
        HostViewPreviewApp::new(
            HostViewIr {
                root: Some(HostViewNode {
                    retained_key: RetainedNodeKey {
                        view_site: ViewSiteId(1),
                        function_instance: Some(FunctionInstanceId(1)),
                        mapped_item_identity: None,
                    },
                    kind: HostViewKind::Stripe,
                    children: vec![
                        HostViewNode {
                            retained_key: RetainedNodeKey {
                                view_site: ViewSiteId(2),
                                function_instance: Some(FunctionInstanceId(1)),
                                mapped_item_identity: Some(1),
                            },
                            kind: HostViewKind::TextInput {
                                value_sink: SinkPortId(1),
                                placeholder: "A".to_string(),
                                change_port: input_ports[0],
                                key_down_port: SourcePortId(20),
                                blur_port: None,
                                focus_port: None,
                                focus_on_mount: false,
                                disabled_sink: None,
                            },
                            children: Vec::new(),
                        },
                        HostViewNode {
                            retained_key: RetainedNodeKey {
                                view_site: ViewSiteId(3),
                                function_instance: Some(FunctionInstanceId(1)),
                                mapped_item_identity: Some(2),
                            },
                            kind: HostViewKind::TextInput {
                                value_sink: SinkPortId(2),
                                placeholder: "B".to_string(),
                                change_port: input_ports[1],
                                key_down_port: SourcePortId(21),
                                blur_port: None,
                                focus_port: None,
                                focus_on_mount: false,
                                disabled_sink: None,
                            },
                            children: Vec::new(),
                        },
                    ],
                }),
            },
            BTreeMap::from([
                (SinkPortId(1), KernelValue::from("")),
                (SinkPortId(2), KernelValue::from("")),
            ]),
        )
    }

    #[test]
    fn dispatches_change_events_and_clears_inputs() {
        let input_ports = [SourcePortId(1), SourcePortId(2)];
        let mut state = MultiInputState::new(input_ports);
        let mut app = app(input_ports);
        let _: UiNode = app.render_root();

        let first_input = app.event_port_for_source(input_ports[0]).unwrap();
        assert!(state.dispatch_ui_events(
            &app,
            &UiEventBatch {
                events: vec![UiEvent {
                    target: first_input,
                    kind: UiEventKind::Input,
                    payload: Some("Alpha".to_string()),
                }],
            }
        ));
        assert_eq!(state.input(0), "Alpha");
        assert_eq!(state.input(1), "");

        assert!(state.set_input(1, "Bravo"));
        assert_eq!(state.input(1), "Bravo");
        assert!(state.clear_input(1));
        assert_eq!(state.input(1), "");
    }
}
