use crate::editable_mapped_list_runtime::EditableMappedListRuntime;
use crate::ir::SourcePortId;
use crate::list_form_actions::append_created_item_from_inputs;

#[derive(Debug, Clone, Copy)]
pub struct EditableListActionPorts {
    pub create_port: Option<SourcePortId>,
    pub update_port: Option<SourcePortId>,
    pub delete_port: Option<SourcePortId>,
}

pub fn apply_editable_list_actions<T, const INPUTS: usize, const ROWS: usize>(
    ports: EditableListActionPorts,
    state: &mut EditableMappedListRuntime<T, INPUTS, ROWS>,
    clicked: Vec<SourcePortId>,
    mut create_item: impl FnMut(&EditableMappedListRuntime<T, INPUTS, ROWS>) -> Option<T>,
    clear_create_inputs: &[usize],
    mut update_selected: impl FnMut(&mut EditableMappedListRuntime<T, INPUTS, ROWS>) -> bool,
) -> bool {
    let mut changed = false;

    for port in clicked {
        if ports.create_port == Some(port) {
            changed |= append_created_item_from_inputs(
                state,
                |state| create_item(state),
                clear_create_inputs,
            );
        } else if ports.delete_port == Some(port) {
            changed |= state.remove_selected();
        } else if ports.update_port == Some(port) {
            changed |= update_selected(state);
        }
    }

    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_action_appends_and_clears_inputs() {
        let mut state = EditableMappedListRuntime::new(
            [(0, "Alpha".to_string())],
            1,
            [SourcePortId(1), SourcePortId(2), SourcePortId(3)],
            [SourcePortId(10), SourcePortId(11)],
        );
        state.set_input(1, "John");
        state.set_input(2, "Doe");

        let changed = apply_editable_list_actions(
            EditableListActionPorts {
                create_port: Some(SourcePortId(20)),
                update_port: Some(SourcePortId(21)),
                delete_port: Some(SourcePortId(22)),
            },
            &mut state,
            vec![SourcePortId(20)],
            |state| Some(format!("{}, {}", state.input(2), state.input(1))),
            &[1, 2],
            |_state| false,
        );

        assert!(changed);
        assert_eq!(
            state
                .list()
                .items()
                .iter()
                .map(|item| item.value.clone())
                .collect::<Vec<_>>(),
            vec!["Alpha".to_string(), "Doe, John".to_string()]
        );
        assert_eq!(state.input(1), "");
        assert_eq!(state.input(2), "");
    }

    #[test]
    fn delete_and_update_actions_flow_through_runtime() {
        let mut state = EditableMappedListRuntime::new(
            [(0, "Alpha".to_string()), (1, "Bravo".to_string())],
            2,
            [SourcePortId(1), SourcePortId(2)],
            [SourcePortId(10), SourcePortId(11)],
        );
        assert!(state.select_id(1));

        let updated = apply_editable_list_actions(
            EditableListActionPorts {
                create_port: Some(SourcePortId(20)),
                update_port: Some(SourcePortId(21)),
                delete_port: Some(SourcePortId(22)),
            },
            &mut state,
            vec![SourcePortId(21)],
            |_state| None,
            &[],
            |state| state.update_selected(|item| item.value.push('!')),
        );
        assert!(updated);
        assert_eq!(state.list().items()[1].value, "Bravo!");

        let deleted = apply_editable_list_actions(
            EditableListActionPorts {
                create_port: Some(SourcePortId(20)),
                update_port: Some(SourcePortId(21)),
                delete_port: Some(SourcePortId(22)),
            },
            &mut state,
            vec![SourcePortId(22)],
            |_state| None,
            &[],
            |_state| false,
        );
        assert!(deleted);
        assert_eq!(
            state
                .list()
                .items()
                .iter()
                .map(|item| item.value.clone())
                .collect::<Vec<_>>(),
            vec!["Alpha".to_string()]
        );
        assert_eq!(state.selected_id(), None);
    }
}
