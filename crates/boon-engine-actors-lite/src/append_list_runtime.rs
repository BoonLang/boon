use crate::host_view_preview::HostViewPreviewApp;
use crate::ids::ActorId;
use crate::input_form_runtime::{FormInputBinding, FormInputEvent, InputFormRuntime};
use crate::ir::SourcePortId;
use crate::preview_runtime::PreviewRuntime;
use crate::runtime::Msg;
use crate::text_input::decode_key_down_payload;
use boon::platform::browser::kernel::KernelValue;
use boon_scene::{UiEventBatch, UiEventKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppendCommitMode {
    PushRaw,
    PushTrimmedAndClearInput,
}

#[derive(Debug, Clone, Copy)]
pub struct AppendListConfig {
    pub input_change_port: SourcePortId,
    pub input_key_down_port: SourcePortId,
    pub clear_press_port: Option<SourcePortId>,
    pub max_items: usize,
    pub commit_mode: AppendCommitMode,
}

pub struct AppendListRuntime {
    runtime: PreviewRuntime,
    input_actor: ActorId,
    clear_actor: Option<ActorId>,
    config: AppendListConfig,
    inputs: InputFormRuntime<1>,
    items: Vec<String>,
}

impl AppendListRuntime {
    #[must_use]
    pub fn new(config: AppendListConfig, initial_items: Vec<String>) -> Self {
        let mut runtime = PreviewRuntime::new();
        let input_actor = runtime.alloc_actor();
        let clear_actor = config.clear_press_port.map(|_| runtime.alloc_actor());
        Self {
            runtime,
            input_actor,
            clear_actor,
            config,
            inputs: InputFormRuntime::new([FormInputBinding {
                change_port: config.input_change_port,
                key_down_port: Some(config.input_key_down_port),
            }]),
            items: initial_items,
        }
    }

    pub(crate) fn dispatch_ui_events(
        &mut self,
        app: &HostViewPreviewApp,
        batch: UiEventBatch,
    ) -> bool {
        let clear_port = self
            .config
            .clear_press_port
            .and_then(|port| app.event_port_for_source(port));

        let dispatch = self.inputs.dispatch_ui_events(app, &batch);
        let mut changed = dispatch.changed;
        for form_event in dispatch.events {
            match form_event {
                FormInputEvent::Changed { index: 0 } => {
                    let messages = self.runtime.dispatch_pulse(
                        self.input_actor,
                        self.config.input_change_port,
                        KernelValue::from(self.inputs.input(0).to_string()),
                    );
                    changed |= self.apply_runtime_messages(messages);
                }
                FormInputEvent::KeyDown { index: 0, key } => {
                    let payload = format!("{key}\u{1F}{}", self.inputs.input(0));
                    let messages = self.runtime.dispatch_pulse(
                        self.input_actor,
                        self.config.input_key_down_port,
                        KernelValue::from(payload),
                    );
                    changed |= self.apply_runtime_messages(messages);
                }
                _ => {}
            }
        }

        for event in batch.events {
            match event.kind {
                UiEventKind::Click
                    if self.config.clear_press_port.is_some()
                        && Some(event.target) == clear_port =>
                {
                    let messages = self.runtime.dispatch_pulse(
                        self.clear_actor.expect("clear actor"),
                        self.config.clear_press_port.expect("clear port"),
                        KernelValue::from("press"),
                    );
                    changed |= self.apply_runtime_messages(messages);
                }
                _ => {}
            }
        }
        changed
    }

    #[must_use]
    pub fn input(&self) -> &str {
        self.inputs.input(0)
    }

    #[must_use]
    pub fn items(&self) -> &[String] {
        &self.items
    }

    fn apply_runtime_messages(&mut self, messages: Vec<Msg>) -> bool {
        let mut changed = false;
        for message in messages {
            match message {
                Msg::SourcePulse { port, value, .. } if port == self.config.input_change_port => {
                    let next = match value {
                        KernelValue::Text(text) | KernelValue::Tag(text) => text,
                        _ => String::new(),
                    };
                    changed |= self.inputs.set_input(0, next);
                }
                Msg::SourcePulse { port, value, .. } if port == self.config.input_key_down_port => {
                    let payload = match value {
                        KernelValue::Text(text) | KernelValue::Tag(text) => Some(text),
                        _ => None,
                    };
                    if decode_key_down_payload(payload.as_deref()).key == "Enter" {
                        changed |= self.commit_input();
                    }
                }
                Msg::SourcePulse { port, .. }
                    if Some(port) == self.config.clear_press_port && !self.items.is_empty() =>
                {
                    self.items.clear();
                    changed = true;
                }
                _ => {}
            }
        }
        changed
    }

    fn commit_input(&mut self) -> bool {
        match self.config.commit_mode {
            AppendCommitMode::PushRaw => {
                self.items.push(self.inputs.input(0).to_string());
                if self.items.len() > self.config.max_items {
                    self.items.truncate(self.config.max_items);
                }
                true
            }
            AppendCommitMode::PushTrimmedAndClearInput => {
                let trimmed = self.inputs.input(0).trim();
                if trimmed.is_empty() {
                    return false;
                }
                self.items.push(trimmed.to_string());
                if self.items.len() > self.config.max_items {
                    self.items.truncate(self.config.max_items);
                }
                self.inputs.clear_input(0);
                true
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::{HostButtonLabel, HostViewIr, HostViewKind, HostViewNode};
    use crate::host_view_preview::HostViewPreviewApp;
    use crate::ir::{FunctionInstanceId, RetainedNodeKey, SinkPortId, ViewSiteId};
    use boon_scene::{UiEvent, UiNode};
    use std::collections::BTreeMap;

    fn input_app(config: AppendListConfig) -> HostViewPreviewApp {
        let host_view = HostViewIr {
            root: Some(HostViewNode {
                retained_key: RetainedNodeKey {
                    view_site: ViewSiteId(1),
                    function_instance: Some(FunctionInstanceId(1)),
                    mapped_item_identity: None,
                },
                kind: HostViewKind::TextInput {
                    value_sink: SinkPortId(1),
                    placeholder: "x".to_string(),
                    change_port: config.input_change_port,
                    key_down_port: config.input_key_down_port,
                    blur_port: None,
                    focus_port: None,
                    focus_on_mount: true,
                    disabled_sink: None,
                },
                children: Vec::new(),
            }),
        };
        HostViewPreviewApp::new(host_view, BTreeMap::new())
    }

    #[test]
    fn raw_mode_appends_without_clearing_input() {
        let config = AppendListConfig {
            input_change_port: SourcePortId(1),
            input_key_down_port: SourcePortId(2),
            clear_press_port: None,
            max_items: 4,
            commit_mode: AppendCommitMode::PushRaw,
        };
        let mut state = AppendListRuntime::new(config, vec!["Initial".to_string()]);
        let mut app = input_app(config);
        let _: UiNode = app.render_root();
        let change = app.event_port_for_source(config.input_change_port).unwrap();
        let key = app
            .event_port_for_source(config.input_key_down_port)
            .unwrap();

        assert!(state.dispatch_ui_events(
            &app,
            UiEventBatch {
                events: vec![UiEvent {
                    target: change,
                    kind: UiEventKind::Change,
                    payload: Some("Apple".to_string()),
                }],
            }
        ));
        assert_eq!(state.input(), "Apple");
        assert!(state.dispatch_ui_events(
            &app,
            UiEventBatch {
                events: vec![UiEvent {
                    target: key,
                    kind: UiEventKind::KeyDown,
                    payload: Some("Enter\u{1F}Apple".to_string()),
                }],
            }
        ));
        assert_eq!(state.input(), "Apple");
        assert_eq!(state.items(), ["Initial".to_string(), "Apple".to_string()]);
    }

    #[test]
    fn trimmed_mode_clears_input_and_clear_button_empties_items() {
        let config = AppendListConfig {
            input_change_port: SourcePortId(10),
            input_key_down_port: SourcePortId(11),
            clear_press_port: Some(SourcePortId(12)),
            max_items: 4,
            commit_mode: AppendCommitMode::PushTrimmedAndClearInput,
        };
        let mut state = AppendListRuntime::new(config, Vec::new());
        let mut app = HostViewPreviewApp::new(
            HostViewIr {
                root: Some(HostViewNode {
                    retained_key: RetainedNodeKey {
                        view_site: ViewSiteId(1),
                        function_instance: Some(FunctionInstanceId(1)),
                        mapped_item_identity: None,
                    },
                    kind: HostViewKind::Document,
                    children: vec![
                        HostViewNode {
                            retained_key: RetainedNodeKey {
                                view_site: ViewSiteId(2),
                                function_instance: Some(FunctionInstanceId(1)),
                                mapped_item_identity: None,
                            },
                            kind: HostViewKind::TextInput {
                                value_sink: SinkPortId(1),
                                placeholder: "x".to_string(),
                                change_port: config.input_change_port,
                                key_down_port: config.input_key_down_port,
                                blur_port: None,
                                focus_port: None,
                                focus_on_mount: true,
                                disabled_sink: None,
                            },
                            children: Vec::new(),
                        },
                        HostViewNode {
                            retained_key: RetainedNodeKey {
                                view_site: ViewSiteId(3),
                                function_instance: Some(FunctionInstanceId(1)),
                                mapped_item_identity: None,
                            },
                            kind: HostViewKind::Button {
                                label: HostButtonLabel::Static("Clear".to_string()),
                                press_port: config.clear_press_port.unwrap(),
                                disabled_sink: None,
                            },
                            children: Vec::new(),
                        },
                    ],
                }),
            },
            BTreeMap::new(),
        );
        let _: UiNode = app.render_root();
        let change = app.event_port_for_source(config.input_change_port).unwrap();
        let key = app
            .event_port_for_source(config.input_key_down_port)
            .unwrap();
        let clear = app
            .event_port_for_source(config.clear_press_port.unwrap())
            .unwrap();

        assert!(state.dispatch_ui_events(
            &app,
            UiEventBatch {
                events: vec![UiEvent {
                    target: change,
                    kind: UiEventKind::Change,
                    payload: Some("  Milk  ".to_string()),
                }],
            }
        ));
        assert!(state.dispatch_ui_events(
            &app,
            UiEventBatch {
                events: vec![UiEvent {
                    target: key,
                    kind: UiEventKind::KeyDown,
                    payload: Some("Enter\u{1F}  Milk  ".to_string()),
                }],
            }
        ));
        assert_eq!(state.input(), "");
        assert_eq!(state.items(), ["Milk".to_string()]);

        assert!(state.dispatch_ui_events(
            &app,
            UiEventBatch {
                events: vec![UiEvent {
                    target: clear,
                    kind: UiEventKind::Click,
                    payload: None,
                }],
            }
        ));
        assert!(state.items().is_empty());
    }
}
