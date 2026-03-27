use crate::FabricValue;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ScopeId {
    pub slot: u32,
    pub generation: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct FunctionInstanceId {
    pub slot: u32,
    pub generation: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ViewNodeId {
    pub slot: u32,
    pub generation: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ListHandleId {
    pub slot: u32,
    pub generation: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct MapperSiteId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ListItemId(pub u64);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListEntry {
    pub id: ListItemId,
    pub value: FabricValue,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ListStore {
    entries: Vec<ListEntry>,
}

impl ListStore {
    #[must_use]
    pub fn entries(&self) -> &[ListEntry] {
        &self.entries
    }
}

#[derive(Debug, Clone, Default)]
pub struct ListDepot {
    next_handle_slot: u32,
    free_handle_slots: Vec<u32>,
    handle_generations: BTreeMap<u32, u32>,
    lists: BTreeMap<u32, ListStore>,
    next_item_id: u64,
}

impl ListDepot {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn alloc_list(&mut self) -> ListHandleId {
        let slot = self.free_handle_slots.pop().unwrap_or_else(|| {
            let slot = self.next_handle_slot;
            self.next_handle_slot += 1;
            slot
        });
        let generation = self
            .handle_generations
            .get(&slot)
            .map_or(0, |generation| generation.wrapping_add(1));
        self.handle_generations.insert(slot, generation);
        self.lists.insert(slot, ListStore::default());
        ListHandleId { slot, generation }
    }

    pub fn drop_list(&mut self, handle: ListHandleId) -> bool {
        if !self.is_live(handle) {
            return false;
        }
        self.lists.remove(&handle.slot);
        self.free_handle_slots.push(handle.slot);
        true
    }

    #[must_use]
    pub fn is_live(&self, handle: ListHandleId) -> bool {
        self.handle_generations
            .get(&handle.slot)
            .is_some_and(|generation| *generation == handle.generation)
            && self.lists.contains_key(&handle.slot)
    }

    pub fn list_literal(&mut self, values: impl IntoIterator<Item = FabricValue>) -> ListHandleId {
        let handle = self.alloc_list();
        for value in values {
            self.append(handle, value)
                .expect("fresh list handle must accept appended value");
        }
        handle
    }

    pub fn append(
        &mut self,
        handle: ListHandleId,
        value: FabricValue,
    ) -> Result<ListItemId, String> {
        let item_id = self.alloc_item_id();
        let list = self.list_mut(handle)?;
        list.entries.push(ListEntry { id: item_id, value });
        Ok(item_id)
    }

    pub fn remove_item(&mut self, handle: ListHandleId, item: ListItemId) -> Result<bool, String> {
        let list = self.list_mut(handle)?;
        let original_len = list.entries.len();
        list.entries.retain(|entry| entry.id != item);
        Ok(list.entries.len() != original_len)
    }

    pub fn retain(
        &mut self,
        handle: ListHandleId,
        keep: impl FnMut(&ListEntry) -> bool,
    ) -> Result<(), String> {
        let list = self.list_mut(handle)?;
        list.entries.retain(keep);
        Ok(())
    }

    #[must_use]
    pub fn list(&self, handle: ListHandleId) -> Option<&ListStore> {
        self.is_live(handle)
            .then(|| self.lists.get(&handle.slot))
            .flatten()
    }

    fn list_mut(&mut self, handle: ListHandleId) -> Result<&mut ListStore, String> {
        if !self.is_live(handle) {
            return Err(format!(
                "FactoryFabric runtime error: stale list handle {:?}",
                handle
            ));
        }
        self.lists.get_mut(&handle.slot).ok_or_else(|| {
            format!(
                "FactoryFabric runtime error: missing live list {:?}",
                handle
            )
        })
    }

    fn alloc_item_id(&mut self) -> ListItemId {
        let id = ListItemId(self.next_item_id);
        self.next_item_id = self.next_item_id.wrapping_add(1);
        id
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListMapInstanceTable {
    pub site: MapperSiteId,
    next_slot: u32,
    free_slots: Vec<u32>,
    generations: BTreeMap<u32, u32>,
    live_instances: BTreeMap<ListItemId, FunctionInstanceId>,
}

impl ListMapInstanceTable {
    #[must_use]
    pub fn new(site: MapperSiteId) -> Self {
        Self {
            site,
            next_slot: 0,
            free_slots: Vec::new(),
            generations: BTreeMap::new(),
            live_instances: BTreeMap::new(),
        }
    }

    pub fn sync_items(&mut self, items: &[ListItemId]) -> ListMapSync {
        let wanted = items.iter().copied().collect::<BTreeSet<_>>();
        let dropped = self
            .live_instances
            .keys()
            .copied()
            .filter(|item| !wanted.contains(item))
            .collect::<Vec<_>>();

        for item in &dropped {
            if let Some(instance) = self.live_instances.remove(item) {
                self.free_slots.push(instance.slot);
            }
        }

        let mut retained = Vec::new();
        let mut created = Vec::new();
        for item in items {
            if let Some(instance) = self.live_instances.get(item).copied() {
                retained.push((*item, instance));
                continue;
            }
            let instance = self.alloc_instance();
            self.live_instances.insert(*item, instance);
            created.push((*item, instance));
        }

        ListMapSync {
            retained,
            created,
            dropped,
        }
    }

    #[must_use]
    pub fn instance_for(&self, item: ListItemId) -> Option<FunctionInstanceId> {
        self.live_instances.get(&item).copied()
    }

    fn alloc_instance(&mut self) -> FunctionInstanceId {
        let slot = self.free_slots.pop().unwrap_or_else(|| {
            let slot = self.next_slot;
            self.next_slot += 1;
            slot
        });
        let generation = self
            .generations
            .get(&slot)
            .map_or(0, |generation| generation.wrapping_add(1));
        self.generations.insert(slot, generation);
        FunctionInstanceId { slot, generation }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListMapSync {
    pub retained: Vec<(ListItemId, FunctionInstanceId)>,
    pub created: Vec<(ListItemId, FunctionInstanceId)>,
    pub dropped: Vec<ListItemId>,
}

#[cfg(test)]
mod tests {
    use super::{ListDepot, ListMapInstanceTable, MapperSiteId};
    use crate::FabricValue;

    #[test]
    fn literal_append_and_remove_preserve_survivor_item_identity() {
        let mut depot = ListDepot::new();
        let handle = depot.list_literal([
            FabricValue::Number(1),
            FabricValue::Number(2),
            FabricValue::Number(3),
        ]);
        let initial = depot
            .list(handle)
            .expect("list should exist")
            .entries()
            .iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>();
        let appended = depot
            .append(handle, FabricValue::Number(4))
            .expect("append should succeed");
        assert!(!initial.contains(&appended));

        depot
            .remove_item(handle, initial[1])
            .expect("remove should succeed");

        let remaining = depot
            .list(handle)
            .expect("list should exist")
            .entries()
            .iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>();
        assert_eq!(remaining, vec![initial[0], initial[2], appended]);
    }

    #[test]
    fn stale_list_handles_are_rejected_after_reuse() {
        let mut depot = ListDepot::new();
        let first = depot.alloc_list();
        assert!(depot.drop_list(first));
        let second = depot.alloc_list();
        assert_ne!(first, second);
        assert!(depot.list(first).is_none());
        let error = depot
            .append(first, FabricValue::Number(9))
            .expect_err("stale handle should be rejected");
        assert!(error.contains("stale list handle"));
        depot
            .append(second, FabricValue::Number(1))
            .expect("live handle should accept append");
    }

    #[test]
    fn mapped_instances_preserve_survivors_and_recreate_only_new_items() {
        let mut depot = ListDepot::new();
        let handle =
            depot.list_literal([FabricValue::Text("a".into()), FabricValue::Text("b".into())]);
        let items = depot
            .list(handle)
            .expect("list should exist")
            .entries()
            .iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>();

        let mut table = ListMapInstanceTable::new(MapperSiteId(7));
        let first_sync = table.sync_items(&items);
        assert_eq!(first_sync.retained.len(), 0);
        assert_eq!(first_sync.created.len(), 2);

        let added = depot
            .append(handle, FabricValue::Text("c".into()))
            .expect("append should succeed");
        depot
            .remove_item(handle, items[0])
            .expect("remove should succeed");
        let updated = depot
            .list(handle)
            .expect("list should exist")
            .entries()
            .iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>();
        let second_sync = table.sync_items(&updated);

        assert_eq!(second_sync.dropped, vec![items[0]]);
        assert_eq!(
            second_sync.created,
            vec![(added, table.instance_for(added).unwrap())]
        );
        assert_eq!(
            second_sync.retained,
            vec![(items[1], table.instance_for(items[1]).unwrap())]
        );
    }

    #[test]
    fn mapped_instances_do_not_recreate_unaffected_items_on_resync() {
        let mut depot = ListDepot::new();
        let handle = depot.list_literal([
            FabricValue::Text("keep".into()),
            FabricValue::Text("also-keep".into()),
        ]);
        let items = depot
            .list(handle)
            .expect("list should exist")
            .entries()
            .iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>();

        let mut table = ListMapInstanceTable::new(MapperSiteId(3));
        table.sync_items(&items);
        let first_instances = items
            .iter()
            .map(|item| table.instance_for(*item).unwrap())
            .collect::<Vec<_>>();

        let sync = table.sync_items(&items);
        let second_instances = items
            .iter()
            .map(|item| table.instance_for(*item).unwrap())
            .collect::<Vec<_>>();

        assert!(sync.created.is_empty());
        assert!(sync.dropped.is_empty());
        assert_eq!(first_instances, second_instances);
    }
}
