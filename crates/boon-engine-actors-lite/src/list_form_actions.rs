use crate::editable_mapped_list_runtime::EditableMappedListRuntime;
use crate::targeted_list_runtime::TargetedListRuntime;
use crate::text_input::TextInputState;

pub(crate) fn append_created_item_from_inputs<T, const INPUTS: usize, const ROWS: usize>(
    state: &mut EditableMappedListRuntime<T, INPUTS, ROWS>,
    create_item: impl FnOnce(&EditableMappedListRuntime<T, INPUTS, ROWS>) -> Option<T>,
    clear_create_inputs: &[usize],
) -> bool {
    let Some(item) = create_item(state) else {
        return false;
    };
    state.append(item);
    for index in clear_create_inputs {
        let _ = state.clear_input(*index);
    }
    true
}

pub(crate) fn append_trimmed_text_input<T>(
    list: &mut TargetedListRuntime<T>,
    input: &mut TextInputState,
    make_item: impl FnOnce(String) -> T,
) -> bool {
    let trimmed = input.draft.trim();
    if trimmed.is_empty() {
        return false;
    }
    let item = make_item(trimmed.to_string());
    list.append(item);
    input.draft.clear();
    input.request_focus();
    true
}

pub(crate) fn update_selected_from_inputs<T, U, const INPUTS: usize, const ROWS: usize>(
    state: &mut EditableMappedListRuntime<T, INPUTS, ROWS>,
    read_update: impl FnOnce(&EditableMappedListRuntime<T, INPUTS, ROWS>) -> Option<U>,
    mut apply_update: impl FnMut(&mut T, U) -> bool,
) -> bool {
    let Some(update) = read_update(state) else {
        return false;
    };

    update_selected_value(state, update, |value, update| apply_update(value, update))
}

pub(crate) fn update_selected_value<T, U, const INPUTS: usize, const ROWS: usize>(
    state: &mut EditableMappedListRuntime<T, INPUTS, ROWS>,
    update: U,
    mut apply_update: impl FnMut(&mut T, U) -> bool,
) -> bool {
    let mut update = Some(update);
    let mut changed = false;
    let updated = state.update_selected(|item| {
        let next = update
            .take()
            .expect("update_selected_value applies at most once");
        changed = apply_update(&mut item.value, next);
    });

    updated && changed
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn update_item_by_id<T, U>(
    list: &mut TargetedListRuntime<T>,
    id: u64,
    update: U,
    mut apply_update: impl FnMut(&mut T, U) -> bool,
) -> bool {
    let mut update = Some(update);
    let mut changed = false;
    let updated = list.update_by_id(id, |item| {
        let next = update
            .take()
            .expect("update_item_by_id applies at most once");
        changed = apply_update(&mut item.value, next);
    });

    updated && changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::SourcePortId;

    #[test]
    fn append_created_item_from_inputs_appends_and_clears_requested_inputs() {
        let mut state = EditableMappedListRuntime::new(
            [(0, "Alpha".to_string())],
            1,
            [SourcePortId(1), SourcePortId(2), SourcePortId(3)],
            [SourcePortId(10), SourcePortId(11)],
        );
        state.set_input(1, "John");
        state.set_input(2, "Doe");

        assert!(append_created_item_from_inputs(
            &mut state,
            |state| Some(format!("{}, {}", state.input(2), state.input(1))),
            &[1, 2],
        ));
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
    fn append_trimmed_text_input_appends_and_refocuses() {
        let mut list = TargetedListRuntime::new([(1, "Alpha".to_string())], 2);
        let mut input = TextInputState::focused_with_hint("  Bravo  ");

        assert!(append_trimmed_text_input(&mut list, &mut input, |text| {
            text
        }));
        assert_eq!(
            list.items()
                .iter()
                .map(|item| item.value.clone())
                .collect::<Vec<_>>(),
            vec!["Alpha".to_string(), "Bravo".to_string()]
        );
        assert_eq!(input.draft, "");
        assert!(input.focused);
        assert!(input.focus_hint);
    }

    #[test]
    fn update_selected_from_inputs_mutates_selected_item_only() {
        let mut state = EditableMappedListRuntime::new(
            [
                (0, ("Hans".to_string(), "Emil".to_string())),
                (1, ("Max".to_string(), "Mustermann".to_string())),
            ],
            2,
            [SourcePortId(1), SourcePortId(2), SourcePortId(3)],
            [SourcePortId(10), SourcePortId(11)],
        );
        assert!(state.select_id(1));
        state.set_input(1, "Rita");
        state.set_input(2, "Tester");

        assert!(update_selected_from_inputs(
            &mut state,
            |state| Some((state.input(1).to_string(), state.input(2).to_string())),
            |person, (name, surname)| {
                let changed = person.0 != name || person.1 != surname;
                person.0 = name;
                person.1 = surname;
                changed
            },
        ));

        assert_eq!(
            state
                .list()
                .items()
                .iter()
                .map(|item| item.value.clone())
                .collect::<Vec<_>>(),
            vec![
                ("Hans".to_string(), "Emil".to_string()),
                ("Rita".to_string(), "Tester".to_string())
            ]
        );
        assert_eq!(state.selected_id(), Some(1));
    }

    #[test]
    fn update_item_by_id_mutates_only_matching_target() {
        let mut list = TargetedListRuntime::new(
            [
                (0, "Alpha".to_string()),
                (1, "Bravo".to_string()),
                (2, "Charlie".to_string()),
            ],
            3,
        );

        assert!(update_item_by_id(
            &mut list,
            1,
            "!".to_string(),
            |value, suffix| {
                let changed = !value.ends_with(&suffix);
                value.push_str(&suffix);
                changed
            }
        ));

        assert_eq!(
            list.items()
                .iter()
                .map(|item| item.value.clone())
                .collect::<Vec<_>>(),
            vec![
                "Alpha".to_string(),
                "Bravo!".to_string(),
                "Charlie".to_string()
            ]
        );
    }
}
