use crate::host_view_preview::HostViewPreviewApp;
use crate::ir::SourcePortId;
use crate::mapped_click_runtime::MappedClickRuntime;
use crate::selected_list_filter::SelectedListFilter;
use boon_scene::UiEventBatch;

pub(crate) struct SelectedFilterClickRuntime<F> {
    clicks: MappedClickRuntime,
    filter: SelectedListFilter<F>,
}

impl<F: Copy + PartialEq> SelectedFilterClickRuntime<F> {
    #[must_use]
    pub(crate) fn new(initial: F, source_ports: impl IntoIterator<Item = SourcePortId>) -> Self {
        Self {
            clicks: MappedClickRuntime::new(source_ports),
            filter: SelectedListFilter::new(initial),
        }
    }

    pub(crate) fn dispatch_ui_events(
        &mut self,
        app: &HostViewPreviewApp,
        batch: UiEventBatch,
        apply_clicked_port: impl FnMut(&mut SelectedListFilter<F>, SourcePortId) -> bool,
    ) -> bool {
        let clicked_ports = self.clicks.dispatch_clicks(app, batch);
        self.apply_clicked_ports(clicked_ports, apply_clicked_port)
    }

    pub(crate) fn apply_clicked_ports(
        &mut self,
        clicked_ports: Vec<SourcePortId>,
        mut apply_clicked_port: impl FnMut(&mut SelectedListFilter<F>, SourcePortId) -> bool,
    ) -> bool {
        let mut changed = false;
        for port in clicked_ports {
            changed |= apply_clicked_port(&mut self.filter, port);
        }
        changed
    }

    #[must_use]
    pub(crate) fn current(&self) -> F {
        self.filter.current()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::{HostButtonLabel, HostViewIr, HostViewKind, HostViewNode};
    use crate::host_view_preview::HostViewPreviewApp;
    use crate::ir::{FunctionInstanceId, RetainedNodeKey, ViewSiteId};
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
                                view_site: ViewSiteId(3),
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
            BTreeMap::new(),
        )
    }

    #[test]
    fn dispatch_ui_events_updates_filter_in_click_order() {
        let first_port = SourcePortId(1);
        let second_port = SourcePortId(2);
        let mut runtime = SelectedFilterClickRuntime::new(false, [first_port, second_port]);
        let mut app = button_app(first_port, second_port);
        let _ = app.render_root();

        let changed = runtime.dispatch_ui_events(
            &app,
            UiEventBatch {
                events: vec![
                    UiEvent {
                        target: app.event_port_for_source(second_port).unwrap(),
                        kind: UiEventKind::Click,
                        payload: None,
                    },
                    UiEvent {
                        target: app.event_port_for_source(first_port).unwrap(),
                        kind: UiEventKind::Click,
                        payload: None,
                    },
                ],
            },
            |filter, port| filter.select(port == second_port),
        );

        assert!(changed);
        assert!(!runtime.current());
    }
}
