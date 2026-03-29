use crate::host_view_preview::HostViewPreviewApp;
use crate::ids::ActorId;
use crate::ir::SourcePortId;
use crate::preview_runtime::PreviewRuntime;
use crate::runtime::Msg;
use boon::platform::browser::kernel::KernelValue;
use boon_scene::{UiEventBatch, UiEventKind};
use std::collections::BTreeMap;

pub struct MappedClickRuntime {
    runtime: PreviewRuntime,
    actors_by_port: BTreeMap<SourcePortId, ActorId>,
}

impl MappedClickRuntime {
    #[must_use]
    pub fn new(source_ports: impl IntoIterator<Item = SourcePortId>) -> Self {
        let mut runtime = PreviewRuntime::new();
        let mut actors_by_port = BTreeMap::new();

        for source_port in source_ports {
            actors_by_port.insert(source_port, runtime.alloc_actor());
        }

        Self {
            runtime,
            actors_by_port,
        }
    }

    #[must_use]
    pub(crate) fn dispatch_clicks(
        &mut self,
        app: &HostViewPreviewApp,
        batch: UiEventBatch,
    ) -> Vec<SourcePortId> {
        let event_ports = self
            .actors_by_port
            .keys()
            .filter_map(|source_port| {
                app.event_port_for_source(*source_port)
                    .map(|event_port| (event_port, *source_port))
            })
            .collect::<Vec<_>>();

        let mut clicked = Vec::new();
        for event in batch.events {
            if event.kind != UiEventKind::Click {
                continue;
            }
            let Some(source_port) = event_ports.iter().find_map(|(event_port, source_port)| {
                (*event_port == event.target).then_some(*source_port)
            }) else {
                continue;
            };
            let actor = self.actors_by_port[&source_port];
            self.runtime.dispatch_pulse_batches(
                actor,
                source_port,
                KernelValue::from("press"),
                |messages| clicked.extend(Self::clicked_ports(messages)),
            );
        }

        clicked
    }

    fn clicked_ports(messages: &[Msg]) -> Vec<SourcePortId> {
        messages
            .iter()
            .filter_map(|message| match message {
                Msg::SourcePulse { port, .. } => Some(*port),
                _ => None,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::{HostButtonLabel, HostViewIr, HostViewKind, HostViewNode};
    use crate::host_view_preview::HostViewPreviewApp;
    use crate::ir::{FunctionInstanceId, RetainedNodeKey, SinkPortId, ViewSiteId};
    use boon::platform::browser::kernel::KernelValue;
    use boon_scene::UiEvent;

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
                            kind: HostViewKind::Checkbox {
                                checked_sink: SinkPortId(1),
                                click_port: second_port,
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
    fn dispatch_clicks_returns_clicked_source_ports_in_order() {
        let first_port = SourcePortId(1);
        let second_port = SourcePortId(2);
        let mut runtime = MappedClickRuntime::new([first_port, second_port]);
        let mut app = button_app(first_port, second_port);
        let _ = app.render_root();

        let clicked = runtime.dispatch_clicks(
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
        );

        assert_eq!(clicked, vec![second_port, first_port]);
    }
}
