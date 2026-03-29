use crate::host_view_preview::HostViewPreviewApp;
use crate::ir::SourcePortId;
use crate::mapped_click_runtime::MappedClickRuntime;
use boon_scene::UiEventBatch;

pub struct MappedItemStateRuntime<T, const N: usize> {
    clicks: MappedClickRuntime,
    source_ports: [SourcePortId; N],
    items: [T; N],
}

impl<T, const N: usize> MappedItemStateRuntime<T, N> {
    #[must_use]
    pub fn new(source_ports: [SourcePortId; N], items: [T; N]) -> Self {
        Self {
            clicks: MappedClickRuntime::new(source_ports),
            source_ports,
            items,
        }
    }

    pub(crate) fn dispatch_ui_events(
        &mut self,
        app: &HostViewPreviewApp,
        batch: UiEventBatch,
        mut on_click: impl FnMut(usize, &mut T),
    ) -> bool {
        let clicked = self.clicks.dispatch_clicks(app, batch);
        self.apply_clicked_ports(clicked, move |index, item| on_click(index, item))
    }

    pub fn apply_clicked_ports(
        &mut self,
        clicked: Vec<SourcePortId>,
        mut on_click: impl FnMut(usize, &mut T),
    ) -> bool {
        let mut changed = false;

        for port in clicked {
            if let Some(index) = self
                .source_ports
                .iter()
                .position(|candidate| *candidate == port)
            {
                on_click(index, &mut self.items[index]);
                changed = true;
            }
        }

        changed
    }

    #[must_use]
    pub fn items(&self) -> &[T; N] {
        &self.items
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::{HostButtonLabel, HostViewIr, HostViewKind, HostViewNode};
    use crate::host_view_preview::HostViewPreviewApp;
    use crate::ir::{FunctionInstanceId, RetainedNodeKey, SinkPortId, ViewSiteId};
    use boon::platform::browser::kernel::KernelValue;
    use boon_scene::{UiEvent, UiEventKind};
    use std::collections::BTreeMap;

    fn button_app(first_port: SourcePortId, second_port: SourcePortId) -> HostViewPreviewApp {
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
                            kind: HostViewKind::Button {
                                label: HostButtonLabel::Static("A".to_string()),
                                press_port: first_port,
                                disabled_sink: None,
                            },
                            children: Vec::new(),
                        },
                        HostViewNode {
                            retained_key: RetainedNodeKey {
                                view_site: ViewSiteId(2),
                                function_instance: Some(FunctionInstanceId(1)),
                                mapped_item_identity: Some(2),
                            },
                            kind: HostViewKind::Button {
                                label: HostButtonLabel::Static("B".to_string()),
                                press_port: second_port,
                                disabled_sink: None,
                            },
                            children: Vec::new(),
                        },
                    ],
                }),
            },
            BTreeMap::from([(SinkPortId(1), KernelValue::Bool(false))]),
        )
    }

    #[test]
    fn dispatch_ui_events_updates_only_clicked_item_state() {
        let first_port = SourcePortId(1);
        let second_port = SourcePortId(2);
        let mut runtime = MappedItemStateRuntime::new([first_port, second_port], [0u32, 0u32]);
        let mut app = button_app(first_port, second_port);
        let _ = app.render_root();

        assert!(runtime.dispatch_ui_events(
            &app,
            UiEventBatch {
                events: vec![UiEvent {
                    target: app.event_port_for_source(second_port).unwrap(),
                    kind: UiEventKind::Click,
                    payload: None,
                }],
            },
            |_index, item| *item += 1,
        ));

        assert_eq!(runtime.items(), &[0, 1]);
    }
}
