#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MappedListItem<T> {
    pub id: u64,
    pub value: T,
}

pub struct MappedListRuntime<T> {
    next_id: u64,
    items: Vec<MappedListItem<T>>,
}

impl<T> MappedListRuntime<T> {
    #[must_use]
    pub fn new(initial_items: impl IntoIterator<Item = (u64, T)>, next_id: u64) -> Self {
        let items = initial_items
            .into_iter()
            .map(|(id, value)| MappedListItem { id, value })
            .collect();
        Self { next_id, items }
    }

    #[must_use]
    pub fn items(&self) -> &[MappedListItem<T>] {
        &self.items
    }

    #[must_use]
    pub fn item(&self, index: usize) -> Option<&MappedListItem<T>> {
        self.items.get(index)
    }

    pub fn item_mut(&mut self, index: usize) -> Option<&mut MappedListItem<T>> {
        self.items.get_mut(index)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn items_mut(&mut self) -> &mut [MappedListItem<T>] {
        &mut self.items
    }

    #[must_use]
    pub fn find(&self, id: u64) -> Option<&MappedListItem<T>> {
        self.items.iter().find(|item| item.id == id)
    }

    pub fn find_mut(&mut self, id: u64) -> Option<&mut MappedListItem<T>> {
        self.items.iter_mut().find(|item| item.id == id)
    }

    pub fn append(&mut self, value: T) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.items.push(MappedListItem { id, value });
        id
    }

    #[must_use]
    pub fn next_id(&self) -> u64 {
        self.next_id
    }

    pub fn remove_by_id(&mut self, id: u64) -> bool {
        let original_len = self.items.len();
        self.items.retain(|item| item.id != id);
        self.items.len() != original_len
    }

    pub fn retain(&mut self, mut keep: impl FnMut(&MappedListItem<T>) -> bool) -> bool {
        let original_len = self.items.len();
        self.items.retain(|item| keep(item));
        self.items.len() != original_len
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_and_remove_preserve_ids() {
        let mut runtime = MappedListRuntime::new([(0, "A"), (1, "B")], 2);
        assert_eq!(runtime.append("C"), 2);
        assert_eq!(
            runtime.items(),
            &[
                MappedListItem { id: 0, value: "A" },
                MappedListItem { id: 1, value: "B" },
                MappedListItem { id: 2, value: "C" },
            ]
        );
        assert!(runtime.remove_by_id(1));
        assert_eq!(
            runtime.items(),
            &[
                MappedListItem { id: 0, value: "A" },
                MappedListItem { id: 2, value: "C" },
            ]
        );
    }

    #[test]
    fn retain_removes_matching_items() {
        let mut runtime = MappedListRuntime::new([(0, 1), (1, 2), (2, 3)], 3);
        assert!(runtime.retain(|item| item.value % 2 == 1));
        assert_eq!(
            runtime.items(),
            &[
                MappedListItem { id: 0, value: 1 },
                MappedListItem { id: 2, value: 3 }
            ]
        );
    }
}
