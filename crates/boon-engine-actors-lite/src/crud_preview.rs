use crate::bridge::HostViewIr;
use crate::editable_list_actions::{EditableListActionPorts, apply_editable_list_actions};
use crate::editable_mapped_list_preview_runtime::{
    EditableMappedListPreviewRuntime, EditableMappedListProjection,
    render_editable_mapped_list_preview,
};
use crate::editable_mapped_list_runtime::EditableMappedListRuntime;
use crate::list_form_actions::update_selected_from_inputs;
use crate::lower::{CrudProgram, try_lower_crud};
use crate::text_filtered_editable_list_preview_runtime::{
    TextFilteredEditableMappedListProjection, dispatch_text_filtered_ui_events, text_filtered_items,
};
use boon::platform::browser::kernel::KernelValue;
use boon::zoon::*;
use boon_scene::{UiEventBatch, UiNode};
use std::collections::BTreeMap;

struct CrudPerson {
    name: String,
    surname: String,
}

struct CrudProjection {
    program: CrudProgram,
}

impl EditableMappedListProjection<CrudPerson, 3, 4> for CrudProjection {
    fn host_view(&self) -> &HostViewIr {
        &self.program.host_view
    }

    fn initial_sink_values(
        &self,
        people: &EditableMappedListRuntime<CrudPerson, 3, 4>,
    ) -> BTreeMap<crate::ir::SinkPortId, KernelValue> {
        initial_sink_values(&self.program, people)
    }

    fn refresh_sink_values(
        &self,
        app: &mut crate::host_view_preview::HostViewPreviewApp,
        people: &EditableMappedListRuntime<CrudPerson, 3, 4>,
    ) {
        refresh_sink_values(app, &self.program, people);
    }
}

impl TextFilteredEditableMappedListProjection<CrudPerson, 3, 4> for CrudProjection {
    const FILTER_INPUT_INDEX: usize = 0;

    fn item_matches_filter(
        filter_text: &str,
        item: &crate::mapped_list_runtime::MappedListItem<CrudPerson>,
    ) -> bool {
        filter_text.is_empty() || item.value.surname.starts_with(filter_text)
    }
}

pub struct CrudPreview {
    program: CrudProgram,
    runtime: EditableMappedListPreviewRuntime<CrudPerson, CrudProjection, 3, 4, 3>,
}

impl CrudPreview {
    pub fn new(source: &str) -> Result<Self, String> {
        let program = try_lower_crud(source)?;
        let people = EditableMappedListRuntime::new(
            [
                (
                    0,
                    CrudPerson {
                        name: "Hans".to_string(),
                        surname: "Emil".to_string(),
                    },
                ),
                (
                    1,
                    CrudPerson {
                        name: "Max".to_string(),
                        surname: "Mustermann".to_string(),
                    },
                ),
                (
                    2,
                    CrudPerson {
                        name: "Roman".to_string(),
                        surname: "Tansen".to_string(),
                    },
                ),
            ],
            3,
            [
                program.filter_change_port,
                program.name_change_port,
                program.surname_change_port,
            ],
            program.row_press_ports,
        );
        let runtime = EditableMappedListPreviewRuntime::new(
            CrudProjection {
                program: program.clone(),
            },
            people,
            [
                program.create_press_port,
                program.update_press_port,
                program.delete_press_port,
            ],
        );

        Ok(Self { program, runtime })
    }

    pub fn dispatch_ui_events(&mut self, batch: UiEventBatch) {
        let program = self.program.clone();
        dispatch_text_filtered_ui_events(&mut self.runtime, batch, move |people, clicked| {
            apply_button_clicks(&program, people, clicked)
        });
    }

    #[must_use]
    pub fn render_root(&mut self) -> UiNode {
        self.runtime.render_root()
    }

    #[must_use]
    pub fn preview_text(&mut self) -> String {
        self.runtime.preview_text()
    }

    #[must_use]
    #[cfg(test)]
    pub(crate) fn app(&self) -> &crate::host_view_preview::HostViewPreviewApp {
        self.runtime.app()
    }
}

fn apply_button_clicks(
    program: &CrudProgram,
    people: &mut EditableMappedListRuntime<CrudPerson, 3, 4>,
    clicked: Vec<crate::ir::SourcePortId>,
) -> bool {
    apply_editable_list_actions(
        EditableListActionPorts {
            create_port: Some(program.create_press_port),
            update_port: Some(program.update_press_port),
            delete_port: Some(program.delete_press_port),
        },
        people,
        clicked,
        |people| {
            Some(CrudPerson {
                name: people.input(1).to_string(),
                surname: people.input(2).to_string(),
            })
        },
        &[1, 2],
        |people| {
            update_selected_from_inputs(
                people,
                |people| Some((people.input(1).to_string(), people.input(2).to_string())),
                |person, (name, surname)| {
                    let changed = person.name != name || person.surname != surname;
                    person.name = name;
                    person.surname = surname;
                    changed
                },
            )
        },
    )
}

fn initial_sink_values(
    program: &CrudProgram,
    people: &EditableMappedListRuntime<CrudPerson, 3, 4>,
) -> BTreeMap<crate::ir::SinkPortId, KernelValue> {
    let mut sink_values = BTreeMap::new();
    sink_values.insert(program.title_sink, KernelValue::from("CRUD"));
    refresh_sink_values_into(&mut sink_values, program, people);
    sink_values
}

fn refresh_sink_values(
    app: &mut crate::host_view_preview::HostViewPreviewApp,
    program: &CrudProgram,
    people: &EditableMappedListRuntime<CrudPerson, 3, 4>,
) {
    let visible_people = text_filtered_items::<CrudPerson, CrudProjection, 3, 4>(people);
    app.set_sink_value(program.title_sink, KernelValue::from("CRUD"));
    app.set_sink_value(
        program.filter_input_sink,
        KernelValue::from(people.input(0).to_string()),
    );
    app.set_sink_value(
        program.name_input_sink,
        KernelValue::from(people.input(1).to_string()),
    );
    app.set_sink_value(
        program.surname_input_sink,
        KernelValue::from(people.input(2).to_string()),
    );
    visible_people.project_into_app(
        app,
        &program.row_label_sinks,
        |item| {
            let prefix = if Some(item.id) == people.selected_id() {
                "\u{25BA} "
            } else {
                ""
            };
            KernelValue::from(format!(
                "{prefix}{}, {}",
                item.value.surname, item.value.name
            ))
        },
        KernelValue::from(""),
    );
    visible_people.project_into_app(
        app,
        &program.row_selected_sinks,
        |item| KernelValue::Bool(Some(item.id) == people.selected_id()),
        KernelValue::Bool(false),
    );
}

fn refresh_sink_values_into(
    sink_values: &mut BTreeMap<crate::ir::SinkPortId, KernelValue>,
    program: &CrudProgram,
    people: &EditableMappedListRuntime<CrudPerson, 3, 4>,
) {
    let visible_people = text_filtered_items::<CrudPerson, CrudProjection, 3, 4>(people);
    sink_values.insert(
        program.filter_input_sink,
        KernelValue::from(people.input(0).to_string()),
    );
    sink_values.insert(
        program.name_input_sink,
        KernelValue::from(people.input(1).to_string()),
    );
    sink_values.insert(
        program.surname_input_sink,
        KernelValue::from(people.input(2).to_string()),
    );
    visible_people.project_into_map(
        sink_values,
        &program.row_label_sinks,
        |item| {
            let prefix = if Some(item.id) == people.selected_id() {
                "\u{25BA} "
            } else {
                ""
            };
            KernelValue::from(format!(
                "{prefix}{}, {}",
                item.value.surname, item.value.name
            ))
        },
        KernelValue::from(""),
    );
    visible_people.project_into_map(
        sink_values,
        &program.row_selected_sinks,
        |item| KernelValue::Bool(Some(item.id) == people.selected_id()),
        KernelValue::Bool(false),
    );
}

pub fn render_crud_preview(preview: CrudPreview) -> impl Element {
    let program = preview.program.clone();
    render_editable_mapped_list_preview(preview.runtime, move |preview, batch| {
        let _ = dispatch_text_filtered_ui_events(preview, batch, |people, clicked| {
            apply_button_clicks(&program, people, clicked)
        });
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use boon_scene::{UiEvent, UiEventKind};

    #[test]
    fn crud_preview_filters_updates_creates_selects_and_deletes() {
        let source = include_str!("../../../playground/frontend/src/examples/crud/crud.bn");
        let mut preview = CrudPreview::new(source).expect("crud preview");
        assert!(preview.preview_text().contains("Emil, Hans"));
        assert!(preview.preview_text().contains("Mustermann, Max"));
        assert!(preview.preview_text().contains("Tansen, Roman"));

        let _ = preview.render_root();
        preview.dispatch_ui_events(UiEventBatch {
            events: vec![UiEvent {
                target: preview
                    .app()
                    .event_port_for_source(preview.program.filter_change_port)
                    .expect("filter port"),
                kind: UiEventKind::Change,
                payload: Some("M".to_string()),
            }],
        });
        assert!(preview.preview_text().contains("Mustermann, Max"));
        assert!(!preview.preview_text().contains("Emil, Hans"));

        preview.dispatch_ui_events(UiEventBatch {
            events: vec![
                UiEvent {
                    target: preview
                        .app()
                        .event_port_for_source(preview.program.filter_change_port)
                        .expect("filter port"),
                    kind: UiEventKind::Change,
                    payload: Some(String::new()),
                },
                UiEvent {
                    target: preview
                        .app()
                        .event_port_for_source(preview.program.name_change_port)
                        .expect("name port"),
                    kind: UiEventKind::Change,
                    payload: Some("John".to_string()),
                },
                UiEvent {
                    target: preview
                        .app()
                        .event_port_for_source(preview.program.surname_change_port)
                        .expect("surname port"),
                    kind: UiEventKind::Change,
                    payload: Some("Doe".to_string()),
                },
            ],
        });
        preview.dispatch_ui_events(UiEventBatch {
            events: vec![UiEvent {
                target: preview
                    .app()
                    .event_port_for_source(preview.program.create_press_port)
                    .expect("create port"),
                kind: UiEventKind::Click,
                payload: None,
            }],
        });
        assert!(preview.preview_text().contains("Doe, John"));

        preview.dispatch_ui_events(UiEventBatch {
            events: vec![UiEvent {
                target: preview
                    .app()
                    .event_port_for_source(preview.program.row_press_ports[2])
                    .expect("row port"),
                kind: UiEventKind::Click,
                payload: None,
            }],
        });
        assert!(preview.preview_text().contains("\u{25BA} Tansen, Roman"));

        preview.dispatch_ui_events(UiEventBatch {
            events: vec![
                UiEvent {
                    target: preview
                        .app()
                        .event_port_for_source(preview.program.name_change_port)
                        .expect("name port"),
                    kind: UiEventKind::Change,
                    payload: Some("Rita".to_string()),
                },
                UiEvent {
                    target: preview
                        .app()
                        .event_port_for_source(preview.program.surname_change_port)
                        .expect("surname port"),
                    kind: UiEventKind::Change,
                    payload: Some("Tester".to_string()),
                },
            ],
        });
        preview.dispatch_ui_events(UiEventBatch {
            events: vec![UiEvent {
                target: preview
                    .app()
                    .event_port_for_source(preview.program.update_press_port)
                    .expect("update port"),
                kind: UiEventKind::Click,
                payload: None,
            }],
        });
        assert!(preview.preview_text().contains("\u{25BA} Tester, Rita"));
        assert!(!preview.preview_text().contains("Tansen, Roman"));

        preview.dispatch_ui_events(UiEventBatch {
            events: vec![UiEvent {
                target: preview
                    .app()
                    .event_port_for_source(preview.program.delete_press_port)
                    .expect("delete port"),
                kind: UiEventKind::Click,
                payload: None,
            }],
        });
        assert!(!preview.preview_text().contains("Tester, Rita"));
        assert!(preview.preview_text().contains("Doe, John"));
    }
}
