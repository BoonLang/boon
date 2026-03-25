use crate::host_view_preview::HostViewPreviewApp;
use crate::ir::SourcePortId;
use crate::mapped_click_runtime::MappedClickRuntime;
use crate::mapped_list_runtime::MappedListItem;
use crate::multi_input_state::MultiInputState;
use crate::targeted_list_runtime::TargetedListRuntime;
use boon_scene::UiEventBatch;

pub struct EditableMappedListRuntime<T, const INPUTS: usize, const ROWS: usize> {
    items: TargetedListRuntime<T>,
    inputs: MultiInputState<INPUTS>,
    row_ports: [SourcePortId; ROWS],
    row_clicks: MappedClickRuntime,
}

impl<T, const INPUTS: usize, const ROWS: usize> EditableMappedListRuntime<T, INPUTS, ROWS> {
    #[must_use]
    pub fn new(
        initial_items: impl IntoIterator<Item = (u64, T)>,
        next_id: u64,
        input_ports: [SourcePortId; INPUTS],
        row_ports: [SourcePortId; ROWS],
    ) -> Self {
        Self {
            items: TargetedListRuntime::new(initial_items, next_id),
            row_ports,
            inputs: MultiInputState::new(input_ports),
            row_clicks: MappedClickRuntime::new(row_ports),
        }
    }

    #[must_use]
    pub fn list(&self) -> &crate::mapped_list_view_runtime::MappedListViewRuntime<T> {
        self.items.list()
    }

    pub fn list_mut(&mut self) -> &mut crate::mapped_list_view_runtime::MappedListViewRuntime<T> {
        self.items.list_mut()
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

    pub fn clear_selection(&mut self) -> bool {
        self.items.clear_selection()
    }

    #[must_use]
    pub fn selected_id(&self) -> Option<u64> {
        self.items.selected_id()
    }

    pub fn select_id(&mut self, id: u64) -> bool {
        self.items.select_id(id)
    }

    pub fn append(&mut self, value: T) -> u64 {
        self.items.append(value)
    }

    pub fn remove_selected(&mut self) -> bool {
        self.items.remove_selected()
    }

    pub fn update_selected(&mut self, mut update: impl FnMut(&mut MappedListItem<T>)) -> bool {
        self.items.update_selected(move |item| update(item))
    }

    pub fn dispatch_input_events(
        &mut self,
        app: &HostViewPreviewApp,
        batch: &UiEventBatch,
    ) -> bool {
        self.inputs.dispatch_ui_events(app, batch)
    }

    pub fn dispatch_row_clicks(
        &mut self,
        app: &HostViewPreviewApp,
        batch: UiEventBatch,
        is_visible: impl Fn(&MappedListItem<T>) -> bool,
    ) -> bool {
        let clicked = self.row_clicks.dispatch_clicks(app, batch);
        let mut changed = false;
        for port in clicked {
            let Some(index) = self
                .row_ports
                .iter()
                .position(|candidate| *candidate == port)
            else {
                continue;
            };
            if let Some(id) = self
                .items
                .list()
                .visible_item_id(index, |item| is_visible(item))
            {
                changed |= self.items.select_id(id);
            }
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::{HostViewIr, HostViewKind, HostViewNode};
    use crate::host_view_preview::HostViewPreviewApp;
    use crate::ir::{FunctionInstanceId, RetainedNodeKey, SinkPortId, ViewSiteId};
    use boon::platform::browser::kernel::KernelValue;
    use boon_scene::{UiEvent, UiEventKind, UiNode};
    use std::collections::BTreeMap;

    fn app(input_ports: [SourcePortId; 2], row_ports: [SourcePortId; 2]) -> HostViewPreviewApp {
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
                                focus_on_mount: false,
                                disabled_sink: None,
                            },
                            children: Vec::new(),
                        },
                        HostViewNode {
                            retained_key: RetainedNodeKey {
                                view_site: ViewSiteId(3),
                                function_instance: Some(FunctionInstanceId(1)),
                                mapped_item_identity: Some(1),
                            },
                            kind: HostViewKind::TextInput {
                                value_sink: SinkPortId(2),
                                placeholder: "B".to_string(),
                                change_port: input_ports[1],
                                key_down_port: SourcePortId(21),
                                focus_on_mount: false,
                                disabled_sink: None,
                            },
                            children: Vec::new(),
                        },
                        HostViewNode {
                            retained_key: RetainedNodeKey {
                                view_site: ViewSiteId(4),
                                function_instance: Some(FunctionInstanceId(1)),
                                mapped_item_identity: Some(1),
                            },
                            kind: HostViewKind::Button {
                                label: "Row 0".to_string(),
                                press_port: row_ports[0],
                                disabled_sink: None,
                            },
                            children: Vec::new(),
                        },
                        HostViewNode {
                            retained_key: RetainedNodeKey {
                                view_site: ViewSiteId(5),
                                function_instance: Some(FunctionInstanceId(1)),
                                mapped_item_identity: Some(2),
                            },
                            kind: HostViewKind::Button {
                                label: "Row 1".to_string(),
                                press_port: row_ports[1],
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
    fn dispatches_inputs_and_selects_visible_rows() {
        let input_ports = [SourcePortId(1), SourcePortId(2)];
        let row_ports = [SourcePortId(3), SourcePortId(4)];
        let mut state = EditableMappedListRuntime::new(
            [(0, "Alpha".to_string()), (1, "Bravo".to_string())],
            2,
            input_ports,
            row_ports,
        );
        let mut app = app(input_ports, row_ports);
        let _: UiNode = app.render_root();

        let first_input = app.event_port_for_source(input_ports[0]).unwrap();
        assert!(state.dispatch_input_events(
            &app,
            &UiEventBatch {
                events: vec![UiEvent {
                    target: first_input,
                    kind: UiEventKind::Input,
                    payload: Some("Br".to_string()),
                }],
            }
        ));
        assert_eq!(state.input(0), "Br");

        let second_row = app.event_port_for_source(row_ports[0]).unwrap();
        let prefix = state.input(0).to_string();
        assert!(state.dispatch_row_clicks(
            &app,
            UiEventBatch {
                events: vec![UiEvent {
                    target: second_row,
                    kind: UiEventKind::Click,
                    payload: None,
                }],
            },
            |item| item.value.starts_with(&prefix)
        ));
        assert_eq!(state.selected_id(), Some(1));
        assert!(state.remove_selected());
        assert_eq!(
            state
                .list()
                .items()
                .iter()
                .map(|item| item.id)
                .collect::<Vec<_>>(),
            vec![0]
        );
    }
}
