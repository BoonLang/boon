use crate::host_view_preview::HostViewPreviewApp;
use crate::ir::SinkPortId;
use crate::mapped_list_runtime::{MappedListItem, MappedListRuntime};
use crate::slot_projection::{project_slot_values_into_app, project_slot_values_into_map};
use boon::platform::browser::kernel::KernelValue;
use std::collections::BTreeMap;

pub struct MappedListViewRuntime<T> {
    items: MappedListRuntime<T>,
}

impl<T> MappedListViewRuntime<T> {
    #[must_use]
    pub fn new(initial_items: impl IntoIterator<Item = (u64, T)>, next_id: u64) -> Self {
        Self {
            items: MappedListRuntime::new(initial_items, next_id),
        }
    }

    #[must_use]
    pub fn items(&self) -> &[MappedListItem<T>] {
        self.items.items()
    }

    pub fn iter(&self) -> impl Iterator<Item = &MappedListItem<T>> {
        self.items().iter()
    }

    #[must_use]
    pub fn first(&self) -> Option<&MappedListItem<T>> {
        self.items().first()
    }

    pub fn items_mut(&mut self) -> &mut [MappedListItem<T>] {
        self.items.items_mut()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    #[must_use]
    pub fn find(&self, id: u64) -> Option<&MappedListItem<T>> {
        self.items.find(id)
    }

    pub fn find_mut(&mut self, id: u64) -> Option<&mut MappedListItem<T>> {
        self.items.find_mut(id)
    }

    pub fn append(&mut self, value: T) -> u64 {
        self.items.append(value)
    }

    #[must_use]
    pub fn next_id(&self) -> u64 {
        self.items.next_id()
    }

    pub fn remove_by_id(&mut self, id: u64) -> bool {
        self.items.remove_by_id(id)
    }

    pub fn update_by_id(
        &mut self,
        id: u64,
        mut update: impl FnMut(&mut MappedListItem<T>),
    ) -> bool {
        let Some(item) = self.find_mut(id) else {
            return false;
        };
        update(item);
        true
    }

    pub fn retain(&mut self, keep: impl FnMut(&MappedListItem<T>) -> bool) -> bool {
        self.items.retain(keep)
    }

    #[must_use]
    pub fn visible_item_id(
        &self,
        visible_index: usize,
        is_visible: impl Fn(&MappedListItem<T>) -> bool,
    ) -> Option<u64> {
        self.items
            .items()
            .iter()
            .filter(|item| is_visible(item))
            .nth(visible_index)
            .map(|item| item.id)
    }

    pub fn update_visible(
        &mut self,
        visible_index: usize,
        is_visible: impl Fn(&MappedListItem<T>) -> bool,
        mut update: impl FnMut(&mut MappedListItem<T>),
    ) -> bool {
        let target_index = self
            .items
            .items()
            .iter()
            .enumerate()
            .filter(|(_, item)| is_visible(item))
            .nth(visible_index)
            .map(|(index, _)| index);

        if let Some(index) = target_index {
            if let Some(item) = self.items.item_mut(index) {
                update(item);
                return true;
            }
        }

        false
    }

    pub fn remove_visible(
        &mut self,
        visible_index: usize,
        is_visible: impl Fn(&MappedListItem<T>) -> bool,
    ) -> bool {
        let target_id = self
            .items
            .items()
            .iter()
            .filter(|item| is_visible(item))
            .nth(visible_index)
            .map(|item| item.id);

        target_id.is_some_and(|id| self.items.remove_by_id(id))
    }

    pub fn project_visible_into_map(
        &self,
        sink_values: &mut BTreeMap<SinkPortId, KernelValue>,
        sinks: &[SinkPortId],
        is_visible: impl Fn(&MappedListItem<T>) -> bool,
        to_value: impl Fn(&MappedListItem<T>) -> KernelValue,
        empty: KernelValue,
    ) {
        project_slot_values_into_map(
            sink_values,
            sinks,
            self.items
                .items()
                .iter()
                .filter(|item| is_visible(item))
                .map(to_value),
            empty,
        );
    }

    pub fn project_visible_into_app(
        &self,
        app: &mut HostViewPreviewApp,
        sinks: &[SinkPortId],
        is_visible: impl Fn(&MappedListItem<T>) -> bool,
        to_value: impl Fn(&MappedListItem<T>) -> KernelValue,
        empty: KernelValue,
    ) {
        project_slot_values_into_app(
            app,
            sinks,
            self.items
                .items()
                .iter()
                .filter(|item| is_visible(item))
                .map(to_value),
            empty,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_and_remove_by_visible_index_only_touch_visible_items() {
        let mut runtime =
            MappedListViewRuntime::new([(0, 1_i64), (1, 2_i64), (2, 3_i64), (3, 4_i64)], 4);

        assert!(runtime.update_visible(1, |item| item.value % 2 == 0, |item| item.value = 20));
        assert_eq!(
            runtime
                .items()
                .iter()
                .map(|item| item.value)
                .collect::<Vec<_>>(),
            vec![1, 2, 3, 20]
        );

        assert!(runtime.remove_visible(0, |item| item.value % 2 == 0));
        assert_eq!(
            runtime
                .items()
                .iter()
                .map(|item| (item.id, item.value))
                .collect::<Vec<_>>(),
            vec![(0, 1), (2, 3), (3, 20)]
        );
    }

    #[test]
    fn update_by_id_mutates_only_matching_item() {
        let mut runtime = MappedListViewRuntime::new([(0, 10_i64), (1, 20_i64)], 2);
        assert!(runtime.update_by_id(1, |item| item.value = 99));
        assert_eq!(
            runtime
                .items()
                .iter()
                .map(|item| (item.id, item.value))
                .collect::<Vec<_>>(),
            vec![(0, 10), (1, 99)]
        );
    }
}
