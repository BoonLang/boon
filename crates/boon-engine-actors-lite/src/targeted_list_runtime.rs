use crate::edit_session::{
    EditKeyDownOutcome, EditSession, EditSessionStateExt, apply_edit_session_key_down,
};
use crate::mapped_list_runtime::MappedListItem;
use crate::mapped_list_view_runtime::MappedListViewRuntime;

pub struct TargetedListRuntime<T> {
    list: MappedListViewRuntime<T>,
    selected_id: Option<u64>,
    editing: Option<EditSession<u64>>,
}

impl<T> TargetedListRuntime<T> {
    #[must_use]
    pub fn new(initial_items: impl IntoIterator<Item = (u64, T)>, next_id: u64) -> Self {
        Self {
            list: MappedListViewRuntime::new(initial_items, next_id),
            selected_id: None,
            editing: None,
        }
    }

    #[must_use]
    pub fn list(&self) -> &MappedListViewRuntime<T> {
        &self.list
    }

    pub fn list_mut(&mut self) -> &mut MappedListViewRuntime<T> {
        &mut self.list
    }

    pub fn items_mut(&mut self) -> &mut [MappedListItem<T>] {
        self.list.items_mut()
    }

    #[must_use]
    pub fn items(&self) -> &[MappedListItem<T>] {
        self.list.items()
    }

    pub fn iter(&self) -> impl Iterator<Item = &MappedListItem<T>> {
        self.list.iter()
    }

    #[must_use]
    pub fn first(&self) -> Option<&MappedListItem<T>> {
        self.list.first()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.list.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    #[must_use]
    pub fn find(&self, id: u64) -> Option<&MappedListItem<T>> {
        self.list.find(id)
    }

    pub fn find_mut(&mut self, id: u64) -> Option<&mut MappedListItem<T>> {
        self.list.find_mut(id)
    }

    pub fn append(&mut self, value: T) -> u64 {
        self.list.append(value)
    }

    #[must_use]
    pub fn next_id(&self) -> u64 {
        self.list.next_id()
    }

    pub fn replace_all(&mut self, initial_items: impl IntoIterator<Item = (u64, T)>, next_id: u64) {
        let items = initial_items.into_iter().collect::<Vec<_>>();
        let selected_id = self
            .selected_id
            .filter(|id| items.iter().any(|(item_id, _)| item_id == id));
        let editing = self
            .editing
            .take()
            .filter(|editing| items.iter().any(|(item_id, _)| *item_id == editing.target));
        self.list = MappedListViewRuntime::new(items, next_id);
        self.selected_id = selected_id;
        self.editing = editing;
    }

    pub fn update_by_id(&mut self, id: u64, update: impl FnMut(&mut MappedListItem<T>)) -> bool {
        self.list.update_by_id(id, update)
    }

    pub fn retain(&mut self, mut keep: impl FnMut(&MappedListItem<T>) -> bool) -> bool {
        let selected_removed = self
            .selected_id
            .is_some_and(|id| self.find(id).is_some_and(|item| !keep(item)));
        let editing_removed = self
            .editing
            .as_ref()
            .is_some_and(|editing| self.find(editing.target).is_some_and(|item| !keep(item)));
        let changed = self.list.retain(|item| keep(item));
        if selected_removed {
            self.selected_id = None;
        }
        if editing_removed {
            self.editing = None;
        }
        changed || selected_removed || editing_removed
    }

    pub fn remove_by_id(&mut self, id: u64) -> bool {
        let removed = self.list.remove_by_id(id);
        if removed {
            if self.selected_id == Some(id) {
                self.selected_id = None;
            }
            let _ = self.editing.clear_edit_session(&id);
        }
        removed
    }

    pub fn remove_selected(&mut self) -> bool {
        let Some(id) = self.selected_id else {
            return false;
        };
        self.remove_by_id(id)
    }

    #[must_use]
    pub fn selected_id(&self) -> Option<u64> {
        self.selected_id
    }

    pub fn select_id(&mut self, id: u64) -> bool {
        if self.find(id).is_none() || self.selected_id == Some(id) {
            return false;
        }
        self.selected_id = Some(id);
        true
    }

    pub fn clear_selection(&mut self) -> bool {
        self.selected_id.take().is_some()
    }

    pub fn update_selected(&mut self, mut update: impl FnMut(&mut MappedListItem<T>)) -> bool {
        let Some(id) = self.selected_id else {
            return false;
        };
        self.update_by_id(id, move |item| update(item))
    }

    #[must_use]
    pub fn all(&self, mut predicate: impl FnMut(&MappedListItem<T>) -> bool) -> bool {
        self.items().iter().all(|item| predicate(item))
    }

    #[must_use]
    pub fn count(&self, mut predicate: impl FnMut(&MappedListItem<T>) -> bool) -> usize {
        self.items().iter().filter(|item| predicate(item)).count()
    }

    #[must_use]
    pub fn ids_where(&self, mut predicate: impl FnMut(&MappedListItem<T>) -> bool) -> Vec<u64> {
        self.items()
            .iter()
            .filter(|item| predicate(item))
            .map(|item| item.id)
            .collect()
    }

    #[must_use]
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn editing(&self) -> Option<&EditSession<u64>> {
        self.editing.as_ref()
    }

    pub fn begin_edit_session(&mut self, id: u64, draft: impl Into<String>) -> bool {
        if self.find(id).is_none() {
            return false;
        }
        self.editing.begin_edit_session(id, draft)
    }

    pub fn set_edit_draft(&mut self, id: u64, draft: impl Into<String>) -> bool {
        self.editing.set_edit_draft(&id, draft)
    }

    pub fn apply_edit_focus(&mut self, id: u64, focused: bool) -> bool {
        self.editing.apply_edit_focus(&id, focused)
    }

    pub fn clear_edit_session(&mut self, id: u64) -> bool {
        self.editing.clear_edit_session(&id)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn apply_edit_key_down(
        &mut self,
        id: u64,
        payload: Option<&str>,
    ) -> EditKeyDownOutcome {
        apply_edit_session_key_down(&mut self.editing, &id, payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text_input::KEYDOWN_TEXT_SEPARATOR;

    #[test]
    fn remove_by_id_clears_selected_and_editing_targets() {
        let mut runtime = TargetedListRuntime::new([(1, "A"), (2, "B")], 3);
        assert!(runtime.select_id(2));
        assert!(runtime.begin_edit_session(2, "B"));
        assert!(runtime.remove_by_id(2));
        assert_eq!(runtime.selected_id(), None);
        assert!(runtime.editing().is_none());
    }

    #[test]
    fn retain_clears_targeted_state_for_removed_items() {
        let mut runtime = TargetedListRuntime::new([(1, "A"), (2, "B")], 3);
        assert!(runtime.select_id(2));
        assert!(runtime.begin_edit_session(2, "B"));
        assert!(runtime.retain(|item| item.id != 2));
        assert_eq!(runtime.selected_id(), None);
        assert!(runtime.editing().is_none());
    }

    #[test]
    fn edit_keydown_flows_through_shared_session_logic() {
        let mut runtime = TargetedListRuntime::new([(1, "A")], 2);
        assert!(runtime.begin_edit_session(1, "A"));
        let outcome =
            runtime.apply_edit_key_down(1, Some(&format!("Enter{KEYDOWN_TEXT_SEPARATOR}B")));
        assert_eq!(outcome.committed_draft, Some("B".to_string()));
        assert!(runtime.editing().is_none());
    }

    #[test]
    fn update_remove_and_query_helpers_flow_through_targeted_state() {
        let mut runtime = TargetedListRuntime::new([(1, 10_i64), (2, 20_i64), (3, 30_i64)], 4);
        assert!(runtime.select_id(2));
        assert!(runtime.update_selected(|item| item.value = 25));
        assert!(runtime.update_by_id(3, |item| item.value = 35));
        assert_eq!(runtime.count(|item| item.value >= 25), 2);
        assert_eq!(runtime.ids_where(|item| item.value % 2 == 1), vec![2, 3]);
        assert!(!runtime.all(|item| item.value < 30));
        assert!(runtime.remove_selected());
        assert_eq!(
            runtime
                .items()
                .iter()
                .map(|item| (item.id, item.value))
                .collect::<Vec<_>>(),
            vec![(1, 10), (3, 35)]
        );
        assert_eq!(runtime.selected_id(), None);
    }
}
