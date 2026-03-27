use crate::{FabricUiStore, FabricValue};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
pub struct FabricTick(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
pub struct FabricSeq {
    pub tick: FabricTick,
    pub order: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RegionId {
    pub slot: u32,
    pub generation: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct MachineId {
    pub slot: u32,
    pub generation: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct BusSlotId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct FabricListItem(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct FabricListScope(pub u64);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BusSlot {
    pub value: Option<FabricValue>,
    pub dirty: bool,
    pub last_changed: Option<FabricSeq>,
}

impl BusSlot {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            value: None,
            dirty: false,
            last_changed: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegionState {
    pub id: RegionId,
    pub scheduled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct HoldCell {
    value: FabricValue,
    last_changed: FabricSeq,
}

impl HoldCell {
    #[must_use]
    fn new(value: FabricValue, last_changed: FabricSeq) -> Self {
        Self {
            value,
            last_changed,
        }
    }

    #[must_use]
    fn value(&self) -> &FabricValue {
        &self.value
    }

    #[must_use]
    const fn last_changed(&self) -> FabricSeq {
        self.last_changed
    }

    fn set_if_changed(&mut self, value: FabricValue, seq: FabricSeq) -> bool {
        if self.value == value {
            return false;
        }
        self.value = value;
        self.last_changed = seq;
        true
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FabricLinkBinding {
    Slot(BusSlotId),
    Value(FabricValue),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct LinkCell {
    binding: Option<FabricLinkBinding>,
    last_changed: FabricSeq,
}

impl LinkCell {
    #[must_use]
    fn new(last_changed: FabricSeq) -> Self {
        Self {
            binding: None,
            last_changed,
        }
    }

    #[must_use]
    fn binding(&self) -> Option<&FabricLinkBinding> {
        self.binding.as_ref()
    }

    #[must_use]
    const fn last_changed(&self) -> FabricSeq {
        self.last_changed
    }

    fn bind_if_changed(&mut self, binding: FabricLinkBinding, seq: FabricSeq) -> bool {
        if self.binding.as_ref() == Some(&binding) {
            return false;
        }
        self.binding = Some(binding);
        self.last_changed = seq;
        true
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ListEntry {
    key: FabricListItem,
    scope: FabricListScope,
    value: FabricValue,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ListCell {
    next_item: u64,
    next_scope: u64,
    items: Vec<ListEntry>,
    last_changed: FabricSeq,
}

impl ListCell {
    #[must_use]
    fn new(last_changed: FabricSeq) -> Self {
        Self {
            next_item: 0,
            next_scope: 0,
            items: Vec::new(),
            last_changed,
        }
    }

    #[must_use]
    const fn last_changed(&self) -> FabricSeq {
        self.last_changed
    }

    fn append(&mut self, value: FabricValue, seq: FabricSeq) -> FabricListItem {
        let key = FabricListItem(self.next_item);
        let scope = FabricListScope(self.next_scope);
        self.next_item = self.next_item.wrapping_add(1);
        self.next_scope = self.next_scope.wrapping_add(1);
        self.items.push(ListEntry { key, scope, value });
        self.last_changed = seq;
        key
    }

    fn remove(&mut self, key: FabricListItem, seq: FabricSeq) -> bool {
        let original_len = self.items.len();
        self.items.retain(|item| item.key != key);
        let changed = self.items.len() != original_len;
        if changed {
            self.last_changed = seq;
        }
        changed
    }

    fn item_scope(&self, key: FabricListItem) -> Option<FabricListScope> {
        self.items
            .iter()
            .find(|item| item.key == key)
            .map(|item| item.scope)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FabricTrigger {
    HoldUpdate { slot: BusSlotId, seq: FabricSeq },
    LinkBind { slot: BusSlotId, seq: FabricSeq },
    ListMutation { slot: BusSlotId, seq: FabricSeq },
    System { seq: FabricSeq },
}

impl FabricTrigger {
    #[must_use]
    pub const fn seq(&self) -> FabricSeq {
        match self {
            Self::HoldUpdate { seq, .. }
            | Self::LinkBind { seq, .. }
            | Self::ListMutation { seq, .. }
            | Self::System { seq } => *seq,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FabricUpdate {
    HoldValue {
        slot: BusSlotId,
        value: FabricValue,
        trigger: FabricTrigger,
    },
    LinkBinding {
        slot: BusSlotId,
        binding: FabricLinkBinding,
        trigger: FabricTrigger,
    },
    ListAppend {
        slot: BusSlotId,
        value: FabricValue,
        trigger: FabricTrigger,
    },
    ListRemove {
        slot: BusSlotId,
        item: FabricListItem,
        trigger: FabricTrigger,
    },
}

impl FabricUpdate {
    fn order_key(&self) -> (FabricSeq, BusSlotId, u8) {
        match self {
            Self::HoldValue { slot, trigger, .. } => (trigger.seq(), *slot, 0),
            Self::LinkBinding { slot, trigger, .. } => (trigger.seq(), *slot, 1),
            Self::ListAppend { slot, trigger, .. } => (trigger.seq(), *slot, 2),
            Self::ListRemove { slot, trigger, .. } => (trigger.seq(), *slot, 3),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppliedUpdate {
    HoldValue {
        slot: BusSlotId,
        changed: bool,
    },
    LinkBinding {
        slot: BusSlotId,
        changed: bool,
    },
    ListAppend {
        slot: BusSlotId,
        item: FabricListItem,
    },
    ListRemove {
        slot: BusSlotId,
        item: FabricListItem,
        changed: bool,
    },
}

#[derive(Debug, Clone)]
struct RegionRecord {
    generation: u32,
    scheduled: bool,
}

#[derive(Debug, Clone)]
pub struct RuntimeCore {
    tick: FabricTick,
    next_order: u32,
    next_region_slot: u32,
    free_region_slots: Vec<u32>,
    region_generations: BTreeMap<u32, u32>,
    region_records: BTreeMap<u32, RegionRecord>,
    bus_slots: BTreeMap<BusSlotId, BusSlot>,
    holds: BTreeMap<BusSlotId, HoldCell>,
    links: BTreeMap<BusSlotId, LinkCell>,
    lists: BTreeMap<BusSlotId, ListCell>,
    ui: FabricUiStore,
    ready_regions: VecDeque<RegionId>,
}

impl Default for RuntimeCore {
    fn default() -> Self {
        Self::new()
    }
}

impl RuntimeCore {
    #[must_use]
    pub fn new() -> Self {
        Self {
            tick: FabricTick(0),
            next_order: 0,
            next_region_slot: 0,
            free_region_slots: Vec::new(),
            region_generations: BTreeMap::new(),
            region_records: BTreeMap::new(),
            bus_slots: BTreeMap::new(),
            holds: BTreeMap::new(),
            links: BTreeMap::new(),
            lists: BTreeMap::new(),
            ui: FabricUiStore::default(),
            ready_regions: VecDeque::new(),
        }
    }

    pub fn begin_host_batch(&mut self) -> FabricTick {
        self.tick.0 += 1;
        self.next_order = 0;
        self.tick
    }

    #[must_use]
    pub const fn tick(&self) -> FabricTick {
        self.tick
    }

    pub fn stamp_write(&mut self) -> FabricSeq {
        let seq = FabricSeq {
            tick: self.tick,
            order: self.next_order,
        };
        self.next_order += 1;
        seq
    }

    pub fn alloc_region(&mut self) -> RegionId {
        let slot = self.free_region_slots.pop().unwrap_or_else(|| {
            let slot = self.next_region_slot;
            self.next_region_slot += 1;
            slot
        });
        let generation = self
            .region_generations
            .get(&slot)
            .map_or(0, |generation| generation.wrapping_add(1));
        self.region_generations.insert(slot, generation);
        self.region_records.insert(
            slot,
            RegionRecord {
                generation,
                scheduled: false,
            },
        );
        RegionId { slot, generation }
    }

    pub fn drop_region(&mut self, id: RegionId) -> bool {
        if !self.region_is_live(id) {
            return false;
        }
        self.region_records.remove(&id.slot);
        self.free_region_slots.push(id.slot);
        self.ready_regions.retain(|queued| *queued != id);
        true
    }

    #[must_use]
    pub fn region_is_live(&self, id: RegionId) -> bool {
        self.region_records
            .get(&id.slot)
            .is_some_and(|record| record.generation == id.generation)
    }

    pub fn schedule_region(&mut self, id: RegionId) -> bool {
        let Some(record) = self.region_records.get_mut(&id.slot) else {
            return false;
        };
        if record.generation != id.generation || record.scheduled {
            return false;
        }
        record.scheduled = true;
        self.ready_regions.push_back(id);
        true
    }

    pub fn pop_ready_region(&mut self) -> Option<RegionId> {
        let id = self.ready_regions.pop_front()?;
        if let Some(record) = self.region_records.get_mut(&id.slot) {
            if record.generation == id.generation {
                record.scheduled = false;
                return Some(id);
            }
        }
        self.pop_ready_region()
    }

    #[must_use]
    pub fn ready_regions(&self) -> Vec<RegionId> {
        self.ready_regions.iter().copied().collect()
    }

    #[must_use]
    pub fn region_states(&self) -> Vec<RegionState> {
        self.region_records
            .iter()
            .map(|(slot, record)| RegionState {
                id: RegionId {
                    slot: *slot,
                    generation: record.generation,
                },
                scheduled: record.scheduled,
            })
            .collect()
    }

    pub fn ensure_bus_slot(&mut self, id: BusSlotId) -> &mut BusSlot {
        self.bus_slots.entry(id).or_insert_with(BusSlot::new)
    }

    pub fn write_bus_value(&mut self, id: BusSlotId, value: FabricValue) -> FabricSeq {
        let seq = self.stamp_write();
        let slot = self.ensure_bus_slot(id);
        slot.value = Some(value);
        slot.dirty = true;
        slot.last_changed = Some(seq);
        seq
    }

    pub fn clear_dirty_bits(&mut self) {
        for slot in self.bus_slots.values_mut() {
            slot.dirty = false;
        }
    }

    #[must_use]
    pub fn dirty_bus_slots(&self) -> Vec<BusSlotId> {
        self.bus_slots
            .iter()
            .filter_map(|(id, slot)| slot.dirty.then_some(*id))
            .collect()
    }

    #[must_use]
    pub fn bus_slot(&self, id: BusSlotId) -> Option<&BusSlot> {
        self.bus_slots.get(&id)
    }

    pub fn create_hold(&mut self, slot: BusSlotId, value: FabricValue) {
        self.holds
            .insert(slot, HoldCell::new(value, self.current_seq()));
    }

    #[must_use]
    pub fn hold(&self, slot: BusSlotId) -> Option<&FabricValue> {
        self.holds.get(&slot).map(HoldCell::value)
    }

    #[must_use]
    pub fn hold_last_changed(&self, slot: BusSlotId) -> Option<FabricSeq> {
        self.holds.get(&slot).map(HoldCell::last_changed)
    }

    pub fn create_link(&mut self, slot: BusSlotId) {
        self.links.insert(slot, LinkCell::new(self.current_seq()));
    }

    #[must_use]
    pub fn read_link_value(&self, slot: BusSlotId) -> FabricValue {
        let Some(link) = self.links.get(&slot) else {
            return FabricValue::Skip;
        };
        match link.binding() {
            Some(FabricLinkBinding::Slot(target)) => {
                self.hold(*target).cloned().unwrap_or(FabricValue::Skip)
            }
            Some(FabricLinkBinding::Value(value)) => value.clone(),
            None => FabricValue::Skip,
        }
    }

    #[must_use]
    pub fn link_last_changed(&self, slot: BusSlotId) -> Option<FabricSeq> {
        self.links.get(&slot).map(LinkCell::last_changed)
    }

    pub fn create_list(&mut self, slot: BusSlotId) {
        self.lists.insert(slot, ListCell::new(self.current_seq()));
    }

    #[must_use]
    pub fn item_scope(&self, slot: BusSlotId, item: FabricListItem) -> Option<FabricListScope> {
        self.lists.get(&slot).and_then(|list| list.item_scope(item))
    }

    #[must_use]
    pub fn list_last_changed(&self, slot: BusSlotId) -> Option<FabricSeq> {
        self.lists.get(&slot).map(ListCell::last_changed)
    }

    #[must_use]
    pub const fn ui(&self) -> &FabricUiStore {
        &self.ui
    }

    pub fn ui_mut(&mut self) -> &mut FabricUiStore {
        &mut self.ui
    }

    pub fn commit_updates(&mut self, mut updates: Vec<FabricUpdate>) -> Vec<AppliedUpdate> {
        updates.sort_by_key(FabricUpdate::order_key);
        let mut applied = Vec::with_capacity(updates.len());
        for update in updates {
            match update {
                FabricUpdate::HoldValue {
                    slot,
                    value,
                    trigger,
                } => {
                    let changed = self
                        .holds
                        .get_mut(&slot)
                        .is_some_and(|hold| hold.set_if_changed(value, trigger.seq()));
                    applied.push(AppliedUpdate::HoldValue { slot, changed });
                }
                FabricUpdate::LinkBinding {
                    slot,
                    binding,
                    trigger,
                } => {
                    let changed = self
                        .links
                        .get_mut(&slot)
                        .is_some_and(|link| link.bind_if_changed(binding, trigger.seq()));
                    applied.push(AppliedUpdate::LinkBinding { slot, changed });
                }
                FabricUpdate::ListAppend {
                    slot,
                    value,
                    trigger,
                } => {
                    let item = self
                        .lists
                        .get_mut(&slot)
                        .map(|list| list.append(value, trigger.seq()))
                        .unwrap_or(FabricListItem(0));
                    applied.push(AppliedUpdate::ListAppend { slot, item });
                }
                FabricUpdate::ListRemove {
                    slot,
                    item,
                    trigger,
                } => {
                    let changed = self
                        .lists
                        .get_mut(&slot)
                        .is_some_and(|list| list.remove(item, trigger.seq()));
                    applied.push(AppliedUpdate::ListRemove {
                        slot,
                        item,
                        changed,
                    });
                }
            }
        }
        applied
    }

    #[must_use]
    const fn current_seq(&self) -> FabricSeq {
        FabricSeq {
            tick: self.tick,
            order: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AppliedUpdate, BusSlotId, FabricLinkBinding, FabricListItem, FabricSeq, FabricTick,
        FabricTrigger, FabricUpdate, RuntimeCore,
    };
    use crate::FabricValue;
    use boon::platform::browser::kernel::{
        ExprId, ItemKey, KernelValue, LinkBinding as KernelLinkBinding, Runtime as KernelRuntime,
        RuntimeUpdate as KernelRuntimeUpdate, ScopeId, SlotKey, SourceId, TickId,
        Trigger as KernelTrigger,
    };
    use boon_scene::EventPortId;

    #[test]
    fn region_schedules_once_on_first_wakeup() {
        let mut runtime = RuntimeCore::new();
        let region = runtime.alloc_region();
        assert!(runtime.schedule_region(region));
        assert!(!runtime.schedule_region(region));
        assert_eq!(runtime.ready_regions(), vec![region]);
    }

    #[test]
    fn repeated_wakeups_while_scheduled_do_not_duplicate_queue_entries() {
        let mut runtime = RuntimeCore::new();
        let region = runtime.alloc_region();
        runtime.schedule_region(region);
        runtime.schedule_region(region);
        runtime.schedule_region(region);
        assert_eq!(runtime.ready_regions(), vec![region]);
    }

    #[test]
    fn region_drain_order_is_deterministic() {
        let mut runtime = RuntimeCore::new();
        let a = runtime.alloc_region();
        let b = runtime.alloc_region();
        let c = runtime.alloc_region();
        runtime.schedule_region(b);
        runtime.schedule_region(a);
        runtime.schedule_region(c);
        assert_eq!(runtime.pop_ready_region(), Some(b));
        assert_eq!(runtime.pop_ready_region(), Some(a));
        assert_eq!(runtime.pop_ready_region(), Some(c));
        assert_eq!(runtime.pop_ready_region(), None);
    }

    #[test]
    fn stamp_ordering_is_deterministic_within_a_tick() {
        let mut runtime = RuntimeCore::new();
        assert_eq!(runtime.begin_host_batch(), FabricTick(1));
        assert_eq!(
            runtime.write_bus_value(BusSlotId(0), FabricValue::Number(1)),
            FabricSeq {
                tick: FabricTick(1),
                order: 0
            }
        );
        assert_eq!(
            runtime.write_bus_value(BusSlotId(1), FabricValue::Number(2)),
            FabricSeq {
                tick: FabricTick(1),
                order: 1
            }
        );
        assert_eq!(runtime.begin_host_batch(), FabricTick(2));
        assert_eq!(
            runtime.write_bus_value(BusSlotId(2), FabricValue::Number(3)),
            FabricSeq {
                tick: FabricTick(2),
                order: 0
            }
        );
    }

    #[test]
    fn stale_ids_are_rejected_safely() {
        let mut runtime = RuntimeCore::new();
        let region = runtime.alloc_region();
        assert!(runtime.drop_region(region));
        assert!(!runtime.schedule_region(region));
        let replacement = runtime.alloc_region();
        assert_ne!(replacement, region);
        assert!(!runtime.region_is_live(region));
        assert!(runtime.region_is_live(replacement));
    }

    #[test]
    fn unbound_link_reads_skip() {
        let mut runtime = RuntimeCore::new();
        let slot = BusSlotId(7);
        runtime.create_link(slot);

        assert_eq!(runtime.read_link_value(slot), FabricValue::Skip);
    }

    #[test]
    fn bound_link_reads_follow_the_target_hold_slot() {
        let mut runtime = RuntimeCore::new();
        let source_hold_slot = BusSlotId(3);
        let link_slot = BusSlotId(4);
        runtime.create_hold(source_hold_slot, FabricValue::from("seed"));
        runtime.create_link(link_slot);

        let bind_seq = FabricSeq {
            tick: runtime.tick(),
            order: 1,
        };
        runtime.commit_updates(vec![FabricUpdate::LinkBinding {
            slot: link_slot,
            binding: FabricLinkBinding::Slot(source_hold_slot),
            trigger: FabricTrigger::LinkBind {
                slot: link_slot,
                seq: bind_seq,
            },
        }]);
        assert_eq!(
            runtime.read_link_value(link_slot),
            FabricValue::from("seed")
        );

        let update_seq = FabricSeq {
            tick: runtime.tick(),
            order: 2,
        };
        runtime.commit_updates(vec![FabricUpdate::HoldValue {
            slot: source_hold_slot,
            value: FabricValue::from("next"),
            trigger: FabricTrigger::HoldUpdate {
                slot: source_hold_slot,
                seq: update_seq,
            },
        }]);
        assert_eq!(
            runtime.read_link_value(link_slot),
            FabricValue::from("next")
        );
        assert_eq!(runtime.link_last_changed(link_slot), Some(bind_seq));
        assert_eq!(
            runtime.hold_last_changed(source_hold_slot),
            Some(update_seq)
        );
    }

    #[test]
    fn commit_updates_are_sorted_by_seq_then_slot() {
        let mut runtime = RuntimeCore::new();
        let first = BusSlotId(2);
        let second = BusSlotId(1);
        runtime.create_hold(first, FabricValue::from(0_i64));
        runtime.create_hold(second, FabricValue::from(0_i64));

        let seq = FabricSeq {
            tick: runtime.tick(),
            order: 3,
        };
        let applied = runtime.commit_updates(vec![
            FabricUpdate::HoldValue {
                slot: first,
                value: FabricValue::from(2_i64),
                trigger: FabricTrigger::System { seq },
            },
            FabricUpdate::HoldValue {
                slot: second,
                value: FabricValue::from(1_i64),
                trigger: FabricTrigger::System { seq },
            },
        ]);

        assert_eq!(
            applied,
            vec![
                AppliedUpdate::HoldValue {
                    slot: second,
                    changed: true,
                },
                AppliedUpdate::HoldValue {
                    slot: first,
                    changed: true,
                },
            ]
        );
        assert_eq!(runtime.hold(second), Some(&FabricValue::from(1_i64)));
        assert_eq!(runtime.hold(first), Some(&FabricValue::from(2_i64)));
        assert_eq!(runtime.hold_last_changed(second), Some(seq));
        assert_eq!(runtime.hold_last_changed(first), Some(seq));
    }

    #[test]
    fn hold_last_changed_updates_only_when_value_changes() {
        let mut runtime = RuntimeCore::new();
        let slot = BusSlotId(5);
        runtime.create_hold(slot, FabricValue::from("seed"));

        let first_seq = FabricSeq {
            tick: runtime.tick(),
            order: 1,
        };
        let applied = runtime.commit_updates(vec![FabricUpdate::HoldValue {
            slot,
            value: FabricValue::from("seed"),
            trigger: FabricTrigger::System { seq: first_seq },
        }]);
        assert_eq!(
            applied,
            vec![AppliedUpdate::HoldValue {
                slot,
                changed: false,
            }]
        );
        assert_eq!(
            runtime.hold_last_changed(slot),
            Some(FabricSeq {
                tick: runtime.tick(),
                order: 0,
            }),
            "same-value HOLD updates should not advance last_changed"
        );

        let second_seq = FabricSeq {
            tick: runtime.tick(),
            order: 2,
        };
        let applied = runtime.commit_updates(vec![FabricUpdate::HoldValue {
            slot,
            value: FabricValue::from("next"),
            trigger: FabricTrigger::System { seq: second_seq },
        }]);
        assert_eq!(
            applied,
            vec![AppliedUpdate::HoldValue {
                slot,
                changed: true,
            }]
        );
        assert_eq!(runtime.hold(slot), Some(&FabricValue::from("next")));
        assert_eq!(runtime.hold_last_changed(slot), Some(second_seq));
    }

    #[test]
    fn list_item_keys_and_scopes_stay_stable() {
        let mut runtime = RuntimeCore::new();
        let slot = BusSlotId(11);
        runtime.create_list(slot);

        let seq = FabricSeq {
            tick: runtime.tick(),
            order: 1,
        };
        let applied = runtime.commit_updates(vec![
            FabricUpdate::ListAppend {
                slot,
                value: FabricValue::from("A"),
                trigger: FabricTrigger::ListMutation { slot, seq },
            },
            FabricUpdate::ListAppend {
                slot,
                value: FabricValue::from("B"),
                trigger: FabricTrigger::ListMutation {
                    slot,
                    seq: FabricSeq {
                        tick: runtime.tick(),
                        order: 2,
                    },
                },
            },
        ]);

        let first = match applied[0] {
            AppliedUpdate::ListAppend { item, .. } => item,
            _ => panic!("expected list append"),
        };
        let second = match applied[1] {
            AppliedUpdate::ListAppend { item, .. } => item,
            _ => panic!("expected list append"),
        };

        assert_eq!(first, FabricListItem(0));
        assert_eq!(second, FabricListItem(1));
        assert_eq!(
            runtime.item_scope(slot, first),
            runtime.item_scope(slot, first)
        );
        assert_ne!(
            runtime.item_scope(slot, first),
            runtime.item_scope(slot, second)
        );

        let remove_seq = FabricSeq {
            tick: runtime.tick(),
            order: 3,
        };
        runtime.commit_updates(vec![FabricUpdate::ListRemove {
            slot,
            item: first,
            trigger: FabricTrigger::ListMutation {
                slot,
                seq: remove_seq,
            },
        }]);

        let append_seq = FabricSeq {
            tick: runtime.tick(),
            order: 4,
        };
        let appended = runtime.commit_updates(vec![FabricUpdate::ListAppend {
            slot,
            value: FabricValue::from("C"),
            trigger: FabricTrigger::ListMutation {
                slot,
                seq: append_seq,
            },
        }]);
        let third = match appended[0] {
            AppliedUpdate::ListAppend { item, .. } => item,
            _ => panic!("expected list append"),
        };

        assert_eq!(third, FabricListItem(2));
        assert_ne!(
            runtime.item_scope(slot, second),
            runtime.item_scope(slot, third)
        );
    }

    #[test]
    fn ui_event_values_are_visible_only_in_the_matching_tick() {
        let mut runtime = RuntimeCore::new();
        let port = EventPortId::new();
        let seq = FabricSeq {
            tick: runtime.tick(),
            order: 1,
        };
        runtime
            .ui_mut()
            .record_event(port, seq, FabricValue::from("payload"));

        assert_eq!(
            runtime.ui().read_event_for_tick(port, runtime.tick()),
            FabricValue::from("payload")
        );

        runtime.begin_host_batch();
        assert_eq!(
            runtime.ui().read_event_for_tick(port, runtime.tick()),
            FabricValue::Skip
        );
    }

    fn kernel_slot(slot: BusSlotId) -> SlotKey {
        SlotKey::new(ScopeId(slot.0 as u64 + 1), ExprId(slot.0))
    }

    fn to_kernel_seq(seq: FabricSeq) -> boon::platform::browser::kernel::TickSeq {
        boon::platform::browser::kernel::TickSeq::new(TickId(seq.tick.0), seq.order)
    }

    fn to_kernel_value(value: &FabricValue) -> KernelValue {
        match value {
            FabricValue::Number(value) => KernelValue::Number(*value as f64),
            FabricValue::Text(value) => KernelValue::Text(value.clone()),
            FabricValue::Bool(value) => KernelValue::Bool(*value),
            FabricValue::Skip => KernelValue::Skip,
        }
    }

    fn normalized_fabric_updates(applied: &[AppliedUpdate]) -> Vec<String> {
        applied
            .iter()
            .map(|update| match update {
                AppliedUpdate::HoldValue { slot, changed } => {
                    format!("hold:{}:{changed}", slot.0)
                }
                AppliedUpdate::LinkBinding { slot, changed } => {
                    format!("link:{}:{changed}", slot.0)
                }
                AppliedUpdate::ListAppend { slot, item } => {
                    format!("append:{}:{}", slot.0, item.0)
                }
                AppliedUpdate::ListRemove {
                    slot,
                    item,
                    changed,
                } => format!("remove:{}:{}:{changed}", slot.0, item.0),
            })
            .collect()
    }

    fn normalized_kernel_updates(
        applied: &[boon::platform::browser::kernel::AppliedUpdate],
    ) -> Vec<String> {
        applied
            .iter()
            .map(|update| match update {
                boon::platform::browser::kernel::AppliedUpdate::HoldValue { slot, changed } => {
                    format!("hold:{}:{changed}", slot.expr.0)
                }
                boon::platform::browser::kernel::AppliedUpdate::LinkBinding { slot, changed } => {
                    format!("link:{}:{changed}", slot.expr.0)
                }
                boon::platform::browser::kernel::AppliedUpdate::ListAppend { slot, item } => {
                    format!("append:{}:{}", slot.expr.0, item.0)
                }
                boon::platform::browser::kernel::AppliedUpdate::ListRemove {
                    slot,
                    item,
                    changed,
                } => format!("remove:{}:{}:{changed}", slot.expr.0, item.0),
            })
            .collect()
    }

    #[test]
    fn runtime_matches_kernel_for_deterministic_hold_link_and_list_trace() {
        let mut fabric = RuntimeCore::new();
        let mut kernel = KernelRuntime::new();

        let hold_slot = BusSlotId(1);
        let link_slot = BusSlotId(2);
        let list_slot = BusSlotId(3);

        fabric.create_hold(hold_slot, FabricValue::from("seed"));
        fabric.create_link(link_slot);
        fabric.create_list(list_slot);

        kernel.create_hold(kernel_slot(hold_slot), KernelValue::from("seed"));
        kernel.create_link(kernel_slot(link_slot));
        kernel.create_list(kernel_slot(list_slot), SourceId(44));

        let fabric_applied = fabric.commit_updates(vec![
            FabricUpdate::LinkBinding {
                slot: link_slot,
                binding: FabricLinkBinding::Slot(hold_slot),
                trigger: FabricTrigger::LinkBind {
                    slot: link_slot,
                    seq: FabricSeq {
                        tick: FabricTick(0),
                        order: 1,
                    },
                },
            },
            FabricUpdate::HoldValue {
                slot: hold_slot,
                value: FabricValue::from("next"),
                trigger: FabricTrigger::HoldUpdate {
                    slot: hold_slot,
                    seq: FabricSeq {
                        tick: FabricTick(0),
                        order: 2,
                    },
                },
            },
            FabricUpdate::ListAppend {
                slot: list_slot,
                value: FabricValue::from("A"),
                trigger: FabricTrigger::ListMutation {
                    slot: list_slot,
                    seq: FabricSeq {
                        tick: FabricTick(0),
                        order: 3,
                    },
                },
            },
            FabricUpdate::ListAppend {
                slot: list_slot,
                value: FabricValue::from("B"),
                trigger: FabricTrigger::ListMutation {
                    slot: list_slot,
                    seq: FabricSeq {
                        tick: FabricTick(0),
                        order: 4,
                    },
                },
            },
        ]);
        let kernel_applied = kernel.commit_updates(vec![
            KernelRuntimeUpdate::LinkBinding {
                slot: kernel_slot(link_slot),
                binding: KernelLinkBinding::Slot(kernel_slot(hold_slot)),
                trigger: KernelTrigger::LinkBind {
                    cell: kernel_slot(link_slot),
                    seq: to_kernel_seq(FabricSeq {
                        tick: FabricTick(0),
                        order: 1,
                    }),
                },
            },
            KernelRuntimeUpdate::HoldValue {
                slot: kernel_slot(hold_slot),
                value: KernelValue::from("next"),
                trigger: KernelTrigger::HoldUpdate {
                    cell: kernel_slot(hold_slot),
                    seq: to_kernel_seq(FabricSeq {
                        tick: FabricTick(0),
                        order: 2,
                    }),
                },
            },
            KernelRuntimeUpdate::ListAppend {
                slot: kernel_slot(list_slot),
                alloc_site: SourceId(44),
                value: KernelValue::from("A"),
                trigger: KernelTrigger::ListMutation {
                    cell: kernel_slot(list_slot),
                    seq: to_kernel_seq(FabricSeq {
                        tick: FabricTick(0),
                        order: 3,
                    }),
                },
            },
            KernelRuntimeUpdate::ListAppend {
                slot: kernel_slot(list_slot),
                alloc_site: SourceId(44),
                value: KernelValue::from("B"),
                trigger: KernelTrigger::ListMutation {
                    cell: kernel_slot(list_slot),
                    seq: to_kernel_seq(FabricSeq {
                        tick: FabricTick(0),
                        order: 4,
                    }),
                },
            },
        ]);

        assert_eq!(
            normalized_fabric_updates(&fabric_applied),
            normalized_kernel_updates(&kernel_applied)
        );
        assert_eq!(
            to_kernel_value(&fabric.read_link_value(link_slot)),
            kernel.read_link_value(kernel_slot(link_slot))
        );

        let fabric_items = fabric
            .lists
            .get(&list_slot)
            .expect("list exists")
            .items
            .iter()
            .map(|entry| (entry.key.0, to_kernel_value(&entry.value)))
            .collect::<Vec<_>>();
        let kernel_items = kernel
            .list(kernel_slot(list_slot))
            .expect("kernel list exists")
            .items()
            .iter()
            .map(|entry| (entry.key.0, entry.value.clone()))
            .collect::<Vec<_>>();
        assert_eq!(fabric_items, kernel_items);

        let first = FabricListItem(0);
        let second = FabricListItem(1);
        assert!(fabric.item_scope(list_slot, first).is_some());
        assert!(
            kernel
                .item_scope(kernel_slot(list_slot), ItemKey(first.0))
                .is_some()
        );
        assert_ne!(
            fabric.item_scope(list_slot, first),
            fabric.item_scope(list_slot, second)
        );
        assert_ne!(
            kernel.item_scope(kernel_slot(list_slot), ItemKey(first.0)),
            kernel.item_scope(kernel_slot(list_slot), ItemKey(second.0))
        );
    }

    #[test]
    fn runtime_matches_kernel_for_randomized_small_update_traces() {
        #[derive(Clone)]
        struct TraceRng(u64);

        impl TraceRng {
            fn new(seed: u64) -> Self {
                Self(seed)
            }

            fn next_u32(&mut self) -> u32 {
                self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1);
                (self.0 >> 32) as u32
            }

            fn next_range(&mut self, upper: u32) -> u32 {
                if upper == 0 {
                    0
                } else {
                    self.next_u32() % upper
                }
            }
        }

        let mut rng = TraceRng::new(0xFAB1C_u64);
        let mut fabric = RuntimeCore::new();
        let mut kernel = KernelRuntime::new();
        let hold_a = BusSlotId(1);
        let hold_b = BusSlotId(2);
        let link = BusSlotId(3);
        let list = BusSlotId(4);

        fabric.create_hold(hold_a, FabricValue::from(0_i64));
        fabric.create_hold(hold_b, FabricValue::from(0_i64));
        fabric.create_link(link);
        fabric.create_list(list);

        kernel.create_hold(kernel_slot(hold_a), KernelValue::from(0.0));
        kernel.create_hold(kernel_slot(hold_b), KernelValue::from(0.0));
        kernel.create_link(kernel_slot(link));
        kernel.create_list(kernel_slot(list), SourceId(99));

        for step in 0..96_u32 {
            let seq = FabricSeq {
                tick: FabricTick(step as u64),
                order: 0,
            };
            match rng.next_range(4) {
                0 => {
                    let slot = if rng.next_range(2) == 0 {
                        hold_a
                    } else {
                        hold_b
                    };
                    let value = FabricValue::from(rng.next_range(10) as i64);
                    let fabric_applied = fabric.commit_updates(vec![FabricUpdate::HoldValue {
                        slot,
                        value: value.clone(),
                        trigger: FabricTrigger::HoldUpdate { slot, seq },
                    }]);
                    let kernel_applied =
                        kernel.commit_updates(vec![KernelRuntimeUpdate::HoldValue {
                            slot: kernel_slot(slot),
                            value: to_kernel_value(&value),
                            trigger: KernelTrigger::HoldUpdate {
                                cell: kernel_slot(slot),
                                seq: to_kernel_seq(seq),
                            },
                        }]);
                    assert_eq!(
                        normalized_fabric_updates(&fabric_applied),
                        normalized_kernel_updates(&kernel_applied)
                    );
                }
                1 => {
                    let target = if rng.next_range(2) == 0 {
                        hold_a
                    } else {
                        hold_b
                    };
                    let fabric_applied = fabric.commit_updates(vec![FabricUpdate::LinkBinding {
                        slot: link,
                        binding: FabricLinkBinding::Slot(target),
                        trigger: FabricTrigger::LinkBind { slot: link, seq },
                    }]);
                    let kernel_applied =
                        kernel.commit_updates(vec![KernelRuntimeUpdate::LinkBinding {
                            slot: kernel_slot(link),
                            binding: KernelLinkBinding::Slot(kernel_slot(target)),
                            trigger: KernelTrigger::LinkBind {
                                cell: kernel_slot(link),
                                seq: to_kernel_seq(seq),
                            },
                        }]);
                    assert_eq!(
                        normalized_fabric_updates(&fabric_applied),
                        normalized_kernel_updates(&kernel_applied)
                    );
                }
                2 => {
                    let value = FabricValue::from(rng.next_range(20) as i64);
                    let fabric_applied = fabric.commit_updates(vec![FabricUpdate::ListAppend {
                        slot: list,
                        value: value.clone(),
                        trigger: FabricTrigger::ListMutation { slot: list, seq },
                    }]);
                    let kernel_applied =
                        kernel.commit_updates(vec![KernelRuntimeUpdate::ListAppend {
                            slot: kernel_slot(list),
                            alloc_site: SourceId(99),
                            value: to_kernel_value(&value),
                            trigger: KernelTrigger::ListMutation {
                                cell: kernel_slot(list),
                                seq: to_kernel_seq(seq),
                            },
                        }]);
                    assert_eq!(
                        normalized_fabric_updates(&fabric_applied),
                        normalized_kernel_updates(&kernel_applied)
                    );
                }
                _ => {
                    let Some(item) = fabric.lists.get(&list).and_then(|list_state| {
                        if list_state.items.is_empty() {
                            None
                        } else {
                            Some(
                                list_state.items
                                    [(rng.next_range(list_state.items.len() as u32)) as usize]
                                    .key,
                            )
                        }
                    }) else {
                        continue;
                    };
                    let fabric_applied = fabric.commit_updates(vec![FabricUpdate::ListRemove {
                        slot: list,
                        item,
                        trigger: FabricTrigger::ListMutation { slot: list, seq },
                    }]);
                    let kernel_applied =
                        kernel.commit_updates(vec![KernelRuntimeUpdate::ListRemove {
                            slot: kernel_slot(list),
                            item: ItemKey(item.0),
                            trigger: KernelTrigger::ListMutation {
                                cell: kernel_slot(list),
                                seq: to_kernel_seq(seq),
                            },
                        }]);
                    assert_eq!(
                        normalized_fabric_updates(&fabric_applied),
                        normalized_kernel_updates(&kernel_applied)
                    );
                }
            }

            assert_eq!(
                to_kernel_value(&fabric.read_link_value(link)),
                kernel.read_link_value(kernel_slot(link))
            );
            assert_eq!(
                fabric.hold(hold_a).map(to_kernel_value),
                kernel
                    .hold(kernel_slot(hold_a))
                    .map(|cell| cell.value().clone())
            );
            assert_eq!(
                fabric.hold(hold_b).map(to_kernel_value),
                kernel
                    .hold(kernel_slot(hold_b))
                    .map(|cell| cell.value().clone())
            );

            let fabric_items = fabric
                .lists
                .get(&list)
                .expect("fabric list exists")
                .items
                .iter()
                .map(|entry| (entry.key.0, to_kernel_value(&entry.value)))
                .collect::<Vec<_>>();
            let kernel_items = kernel
                .list(kernel_slot(list))
                .expect("kernel list exists")
                .items()
                .iter()
                .map(|entry| (entry.key.0, entry.value.clone()))
                .collect::<Vec<_>>();
            assert_eq!(fabric_items, kernel_items);
        }
    }
}
