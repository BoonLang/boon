use crate::host_view_preview::HostViewPreviewApp;
use crate::ir::SourcePortId;
use crate::multi_input_state::MultiInputState;
use crate::text_input::decode_key_down_payload;
use boon_scene::{UiEventBatch, UiEventKind};

#[derive(Debug, Clone, Copy)]
pub struct FormInputBinding {
    pub change_port: SourcePortId,
    pub key_down_port: Option<SourcePortId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormInputEvent {
    Changed { index: usize },
    KeyDown { index: usize, key: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormInputDispatch {
    pub changed: bool,
    pub events: Vec<FormInputEvent>,
}

pub struct InputFormRuntime<const N: usize> {
    inputs: MultiInputState<N>,
    key_down_ports: [Option<SourcePortId>; N],
}

impl<const N: usize> InputFormRuntime<N> {
    #[must_use]
    pub fn new(bindings: [FormInputBinding; N]) -> Self {
        Self {
            inputs: MultiInputState::new(bindings.map(|binding| binding.change_port)),
            key_down_ports: bindings.map(|binding| binding.key_down_port),
        }
    }

    #[must_use]
    pub fn input(&self, index: usize) -> &str {
        self.inputs.input(index)
    }

    pub fn set_input(&mut self, index: usize, next: impl Into<String>) -> bool {
        self.inputs.set_input(index, next)
    }

    pub fn clear_input(&mut self, index: usize) -> bool {
        self.inputs.clear_input(index)
    }

    #[must_use]
    pub(crate) fn dispatch_ui_events(
        &mut self,
        app: &HostViewPreviewApp,
        batch: &UiEventBatch,
    ) -> FormInputDispatch {
        let change_event_ports = self
            .inputs
            .input_ports()
            .iter()
            .enumerate()
            .filter_map(|(index, port)| {
                app.event_port_for_source(*port)
                    .map(|event_port| (index, event_port))
            })
            .collect::<Vec<_>>();
        let key_event_ports = self
            .key_down_ports
            .iter()
            .enumerate()
            .filter_map(|(index, port)| {
                port.and_then(|port| {
                    app.event_port_for_source(port)
                        .map(|event_port| (index, event_port))
                })
            })
            .collect::<Vec<_>>();

        let mut changed = false;
        let mut events = Vec::new();

        for event in &batch.events {
            match event.kind {
                UiEventKind::Input | UiEventKind::Change => {
                    let Some((index, _)) = change_event_ports
                        .iter()
                        .find(|(_, event_port)| *event_port == event.target)
                    else {
                        continue;
                    };
                    let next = event.payload.clone().unwrap_or_default();
                    if self.inputs.set_input(*index, next) {
                        changed = true;
                        events.push(FormInputEvent::Changed { index: *index });
                    }
                }
                UiEventKind::KeyDown => {
                    let Some((index, _)) = key_event_ports
                        .iter()
                        .find(|(_, event_port)| *event_port == event.target)
                    else {
                        continue;
                    };
                    let decoded = decode_key_down_payload(event.payload.as_deref());
                    if let Some(current_text) = decoded.current_text {
                        changed |= self.inputs.set_input(*index, current_text);
                    }
                    events.push(FormInputEvent::KeyDown {
                        index: *index,
                        key: decoded.key,
                    });
                }
                _ => {}
            }
        }

        FormInputDispatch { changed, events }
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

    fn app(bindings: [FormInputBinding; 2]) -> HostViewPreviewApp {
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
                                change_port: bindings[0].change_port,
                                key_down_port: bindings[0].key_down_port.expect("key port"),
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
                                change_port: bindings[1].change_port,
                                key_down_port: bindings[1].key_down_port.expect("key port"),
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
    fn dispatches_change_and_keydown_events() {
        let bindings = [
            FormInputBinding {
                change_port: SourcePortId(1),
                key_down_port: Some(SourcePortId(2)),
            },
            FormInputBinding {
                change_port: SourcePortId(3),
                key_down_port: Some(SourcePortId(4)),
            },
        ];
        let mut runtime = InputFormRuntime::new(bindings);
        let mut app = app(bindings);
        let _: UiNode = app.render_root();

        let first_change = app.event_port_for_source(bindings[0].change_port).unwrap();
        let second_key = app
            .event_port_for_source(bindings[1].key_down_port.unwrap())
            .unwrap();

        let dispatch = runtime.dispatch_ui_events(
            &app,
            &UiEventBatch {
                events: vec![
                    UiEvent {
                        target: first_change,
                        kind: UiEventKind::Input,
                        payload: Some("Alpha".to_string()),
                    },
                    UiEvent {
                        target: second_key,
                        kind: UiEventKind::KeyDown,
                        payload: Some("Enter\u{1F}Bravo".to_string()),
                    },
                ],
            },
        );

        assert!(dispatch.changed);
        assert_eq!(runtime.input(0), "Alpha");
        assert_eq!(runtime.input(1), "Bravo");
        assert_eq!(
            dispatch.events,
            vec![
                FormInputEvent::Changed { index: 0 },
                FormInputEvent::KeyDown {
                    index: 1,
                    key: "Enter".to_string()
                }
            ]
        );
    }
}
