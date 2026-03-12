use std::cmp::Ordering;
use std::collections::HashMap;

use super::ids::{ItemKey, SlotKey, SourceId, TickId, TickSeq};
use super::ui::{EventPortId, UiStore};
use super::value::KernelValue;

#[derive(Debug, Clone, PartialEq)]
pub enum Trigger {
    DomEvent { port: EventPortId, seq: TickSeq },
    HoldUpdate { cell: SlotKey, seq: TickSeq },
    LinkBind { cell: SlotKey, seq: TickSeq },
    ListMutation { cell: SlotKey, seq: TickSeq },
    System { seq: TickSeq },
}

impl Trigger {
    #[must_use]
    pub const fn seq(&self) -> TickSeq {
        match self {
            Self::DomEvent { seq, .. }
            | Self::HoldUpdate { seq, .. }
            | Self::LinkBind { seq, .. }
            | Self::ListMutation { seq, .. }
            | Self::System { seq } => *seq,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct HoldCell {
    value: KernelValue,
    last_changed: TickSeq,
}

impl HoldCell {
    #[must_use]
    pub fn new(value: KernelValue, last_changed: TickSeq) -> Self {
        Self {
            value,
            last_changed,
        }
    }

    #[must_use]
    pub fn value(&self) -> &KernelValue {
        &self.value
    }

    #[must_use]
    pub const fn last_changed(&self) -> TickSeq {
        self.last_changed
    }

    fn set_if_changed(&mut self, value: KernelValue, seq: TickSeq) -> bool {
        if self.value == value {
            return false;
        }
        self.value = value;
        self.last_changed = seq;
        true
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum LinkBinding {
    Slot(SlotKey),
    Value(KernelValue),
}

#[derive(Debug, Clone, PartialEq)]
pub struct LinkCell {
    binding: Option<LinkBinding>,
    last_changed: TickSeq,
}

impl LinkCell {
    #[must_use]
    pub fn new(last_changed: TickSeq) -> Self {
        Self {
            binding: None,
            last_changed,
        }
    }

    #[must_use]
    pub fn binding(&self) -> Option<&LinkBinding> {
        self.binding.as_ref()
    }

    #[must_use]
    pub const fn last_changed(&self) -> TickSeq {
        self.last_changed
    }

    fn bind_if_changed(&mut self, binding: LinkBinding, seq: TickSeq) -> bool {
        if self.binding.as_ref() == Some(&binding) {
            return false;
        }
        self.binding = Some(binding);
        self.last_changed = seq;
        true
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ListEntry {
    pub key: ItemKey,
    pub value: KernelValue,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ListCell {
    alloc_site: SourceId,
    next_item: u64,
    items: Vec<ListEntry>,
    last_changed: TickSeq,
}

impl ListCell {
    #[must_use]
    pub fn new(alloc_site: SourceId, last_changed: TickSeq) -> Self {
        Self {
            alloc_site,
            next_item: 0,
            items: Vec::new(),
            last_changed,
        }
    }

    #[must_use]
    pub const fn alloc_site(&self) -> SourceId {
        self.alloc_site
    }

    #[must_use]
    pub fn items(&self) -> &[ListEntry] {
        &self.items
    }

    #[must_use]
    pub const fn last_changed(&self) -> TickSeq {
        self.last_changed
    }

    fn append(&mut self, value: KernelValue, seq: TickSeq) -> ItemKey {
        let key = ItemKey(self.next_item);
        self.next_item = self.next_item.wrapping_add(1);
        self.items.push(ListEntry { key, value });
        self.last_changed = seq;
        key
    }

    fn remove(&mut self, key: ItemKey, seq: TickSeq) -> bool {
        let original_len = self.items.len();
        self.items.retain(|item| item.key != key);
        let changed = self.items.len() != original_len;
        if changed {
            self.last_changed = seq;
        }
        changed
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum RuntimeUpdate {
    HoldValue {
        slot: SlotKey,
        value: KernelValue,
        trigger: Trigger,
    },
    LinkBinding {
        slot: SlotKey,
        binding: LinkBinding,
        trigger: Trigger,
    },
    ListAppend {
        slot: SlotKey,
        alloc_site: SourceId,
        value: KernelValue,
        trigger: Trigger,
    },
    ListRemove {
        slot: SlotKey,
        item: ItemKey,
        trigger: Trigger,
    },
}

impl RuntimeUpdate {
    fn order_key(&self) -> (TickSeq, SlotKey, u8) {
        match self {
            Self::HoldValue { slot, trigger, .. } => (trigger.seq(), *slot, 0),
            Self::LinkBinding { slot, trigger, .. } => (trigger.seq(), *slot, 1),
            Self::ListAppend { slot, trigger, .. } => (trigger.seq(), *slot, 2),
            Self::ListRemove { slot, trigger, .. } => (trigger.seq(), *slot, 3),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum AppliedUpdate {
    HoldValue {
        slot: SlotKey,
        changed: bool,
    },
    LinkBinding {
        slot: SlotKey,
        changed: bool,
    },
    ListAppend {
        slot: SlotKey,
        item: ItemKey,
    },
    ListRemove {
        slot: SlotKey,
        item: ItemKey,
        changed: bool,
    },
}

#[derive(Debug)]
pub struct Runtime {
    tick: TickId,
    next_seq: u32,
    holds: HashMap<SlotKey, HoldCell>,
    links: HashMap<SlotKey, LinkCell>,
    lists: HashMap<SlotKey, ListCell>,
    ui: UiStore,
}

impl Runtime {
    #[must_use]
    pub fn new() -> Self {
        Self {
            tick: TickId(1),
            next_seq: 0,
            holds: HashMap::new(),
            links: HashMap::new(),
            lists: HashMap::new(),
            ui: UiStore::default(),
        }
    }

    #[must_use]
    pub const fn tick(&self) -> TickId {
        self.tick
    }

    pub fn begin_tick(&mut self) -> TickId {
        self.tick = TickId(self.tick.0.wrapping_add(1));
        self.next_seq = 0;
        self.tick
    }

    #[must_use]
    pub fn next_seq(&mut self) -> TickSeq {
        let seq = TickSeq::new(self.tick, self.next_seq);
        self.next_seq = self.next_seq.wrapping_add(1);
        seq
    }

    #[must_use]
    pub fn ui(&self) -> &UiStore {
        &self.ui
    }

    #[must_use]
    pub fn ui_mut(&mut self) -> &mut UiStore {
        &mut self.ui
    }

    pub fn create_hold(&mut self, slot: SlotKey, value: KernelValue) {
        let seq = TickSeq::new(self.tick, 0);
        self.holds
            .entry(slot)
            .or_insert_with(|| HoldCell::new(value, seq));
    }

    #[must_use]
    pub fn hold(&self, slot: SlotKey) -> Option<&HoldCell> {
        self.holds.get(&slot)
    }

    #[must_use]
    pub fn hold_last_changed(&self, slot: SlotKey) -> Option<TickSeq> {
        self.hold(slot).map(HoldCell::last_changed)
    }

    pub fn create_link(&mut self, slot: SlotKey) {
        let seq = TickSeq::new(self.tick, 0);
        self.links.entry(slot).or_insert_with(|| LinkCell::new(seq));
    }

    #[must_use]
    pub fn link(&self, slot: SlotKey) -> Option<&LinkCell> {
        self.links.get(&slot)
    }

    #[must_use]
    pub fn link_last_changed(&self, slot: SlotKey) -> Option<TickSeq> {
        self.link(slot).map(LinkCell::last_changed)
    }

    #[must_use]
    pub fn read_link_value(&self, slot: SlotKey) -> KernelValue {
        let Some(link) = self.links.get(&slot) else {
            return KernelValue::Skip;
        };
        match link.binding() {
            None => KernelValue::Skip,
            Some(LinkBinding::Value(value)) => value.clone(),
            Some(LinkBinding::Slot(target)) => self
                .holds
                .get(target)
                .map(|cell| cell.value().clone())
                .unwrap_or(KernelValue::Skip),
        }
    }

    pub fn create_list(&mut self, slot: SlotKey, alloc_site: SourceId) {
        let seq = TickSeq::new(self.tick, 0);
        self.lists
            .entry(slot)
            .or_insert_with(|| ListCell::new(alloc_site, seq));
    }

    #[must_use]
    pub fn list(&self, slot: SlotKey) -> Option<&ListCell> {
        self.lists.get(&slot)
    }

    #[must_use]
    pub fn list_last_changed(&self, slot: SlotKey) -> Option<TickSeq> {
        self.list(slot).map(ListCell::last_changed)
    }

    #[must_use]
    pub fn slot_last_changed(&self, slot: SlotKey) -> Option<TickSeq> {
        self.hold_last_changed(slot)
            .or_else(|| self.link_last_changed(slot))
            .or_else(|| self.list_last_changed(slot))
    }

    #[must_use]
    pub fn item_scope(&self, slot: SlotKey, item: ItemKey) -> Option<super::ids::ScopeId> {
        self.lists
            .get(&slot)
            .map(|list| slot.scope.child(list.alloc_site(), item.0))
    }

    pub fn commit_updates(&mut self, updates: Vec<RuntimeUpdate>) -> Vec<AppliedUpdate> {
        // Commit order is deterministic across ticks and scopes.
        let mut ordered = updates;
        ordered.sort_by(|lhs, rhs| match lhs.order_key().cmp(&rhs.order_key()) {
            Ordering::Equal => Ordering::Equal,
            other => other,
        });

        let mut applied = Vec::with_capacity(ordered.len());
        for update in ordered {
            match update {
                RuntimeUpdate::HoldValue {
                    slot,
                    value,
                    trigger,
                } => {
                    let cell = self
                        .holds
                        .entry(slot)
                        .or_insert_with(|| HoldCell::new(KernelValue::Skip, trigger.seq()));
                    let changed = cell.set_if_changed(value, trigger.seq());
                    applied.push(AppliedUpdate::HoldValue { slot, changed });
                }
                RuntimeUpdate::LinkBinding {
                    slot,
                    binding,
                    trigger,
                } => {
                    let cell = self
                        .links
                        .entry(slot)
                        .or_insert_with(|| LinkCell::new(trigger.seq()));
                    let changed = cell.bind_if_changed(binding, trigger.seq());
                    applied.push(AppliedUpdate::LinkBinding { slot, changed });
                }
                RuntimeUpdate::ListAppend {
                    slot,
                    alloc_site,
                    value,
                    trigger,
                } => {
                    let list = self
                        .lists
                        .entry(slot)
                        .or_insert_with(|| ListCell::new(alloc_site, trigger.seq()));
                    let item = list.append(value, trigger.seq());
                    applied.push(AppliedUpdate::ListAppend { slot, item });
                }
                RuntimeUpdate::ListRemove {
                    slot,
                    item,
                    trigger,
                } => {
                    let changed = self
                        .lists
                        .get_mut(&slot)
                        .map(|list| list.remove(item, trigger.seq()))
                        .unwrap_or(false);
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::browser::kernel::{
        ElementId, EventPortId, EventType, ExprId, ScopeId, SlotKey, SourceId,
    };

    fn hold_slot(scope: u64, expr: u32) -> SlotKey {
        SlotKey::new(ScopeId(scope), ExprId(expr))
    }

    #[test]
    fn unbound_link_reads_skip() {
        let mut runtime = Runtime::new();
        let slot = hold_slot(1, 7);
        runtime.create_link(slot);

        assert_eq!(runtime.read_link_value(slot), KernelValue::Skip);
    }

    #[test]
    fn bound_link_reads_follow_the_target_hold_slot() {
        let mut runtime = Runtime::new();
        let source_hold_slot = hold_slot(2, 3);
        let link_slot = hold_slot(2, 4);
        runtime.create_hold(source_hold_slot, KernelValue::from("seed"));
        runtime.create_link(link_slot);

        let bind_seq = TickSeq::new(runtime.tick(), 1);
        runtime.commit_updates(vec![RuntimeUpdate::LinkBinding {
            slot: link_slot,
            binding: LinkBinding::Slot(source_hold_slot),
            trigger: Trigger::LinkBind {
                cell: link_slot,
                seq: bind_seq,
            },
        }]);
        assert_eq!(
            runtime.read_link_value(link_slot),
            KernelValue::from("seed")
        );

        let update_seq = TickSeq::new(runtime.tick(), 2);
        runtime.commit_updates(vec![RuntimeUpdate::HoldValue {
            slot: source_hold_slot,
            value: KernelValue::from("next"),
            trigger: Trigger::HoldUpdate {
                cell: source_hold_slot,
                seq: update_seq,
            },
        }]);
        assert_eq!(
            runtime.read_link_value(link_slot),
            KernelValue::from("next")
        );
        assert_eq!(runtime.link_last_changed(link_slot), Some(bind_seq));
        assert_eq!(
            runtime.hold_last_changed(source_hold_slot),
            Some(update_seq)
        );
    }

    #[test]
    fn commit_updates_are_sorted_by_seq_then_slot() {
        let mut runtime = Runtime::new();
        let first = hold_slot(1, 2);
        let second = hold_slot(1, 1);
        runtime.create_hold(first, 0.0.into());
        runtime.create_hold(second, 0.0.into());

        let seq = TickSeq::new(runtime.tick(), 3);
        let applied = runtime.commit_updates(vec![
            RuntimeUpdate::HoldValue {
                slot: first,
                value: 2.0.into(),
                trigger: Trigger::System { seq },
            },
            RuntimeUpdate::HoldValue {
                slot: second,
                value: 1.0.into(),
                trigger: Trigger::System { seq },
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
        assert_eq!(
            runtime.hold(second).unwrap().value(),
            &KernelValue::Number(1.0)
        );
        assert_eq!(
            runtime.hold(first).unwrap().value(),
            &KernelValue::Number(2.0)
        );
        assert_eq!(runtime.slot_last_changed(second), Some(seq));
        assert_eq!(runtime.slot_last_changed(first), Some(seq));
    }

    #[test]
    fn hold_last_changed_updates_only_when_value_changes() {
        let mut runtime = Runtime::new();
        let slot = hold_slot(5, 3);
        runtime.create_hold(slot, KernelValue::from("seed"));

        let first_seq = TickSeq::new(runtime.tick(), 1);
        let applied = runtime.commit_updates(vec![RuntimeUpdate::HoldValue {
            slot,
            value: KernelValue::from("seed"),
            trigger: Trigger::System { seq: first_seq },
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
            Some(TickSeq::new(runtime.tick(), 0)),
            "same-value HOLD updates should not advance last_changed"
        );

        let second_seq = TickSeq::new(runtime.tick(), 2);
        let applied = runtime.commit_updates(vec![RuntimeUpdate::HoldValue {
            slot,
            value: KernelValue::from("next"),
            trigger: Trigger::System { seq: second_seq },
        }]);
        assert_eq!(
            applied,
            vec![AppliedUpdate::HoldValue {
                slot,
                changed: true,
            }]
        );
        assert_eq!(
            runtime.hold(slot).unwrap().value(),
            &KernelValue::from("next")
        );
        assert_eq!(runtime.hold_last_changed(slot), Some(second_seq));
    }

    #[test]
    fn list_item_keys_and_scopes_stay_stable() {
        let mut runtime = Runtime::new();
        let slot = hold_slot(9, 11);
        runtime.create_list(slot, SourceId(44));

        let seq = TickSeq::new(runtime.tick(), 1);
        let applied = runtime.commit_updates(vec![
            RuntimeUpdate::ListAppend {
                slot,
                alloc_site: SourceId(44),
                value: "A".into(),
                trigger: Trigger::ListMutation { cell: slot, seq },
            },
            RuntimeUpdate::ListAppend {
                slot,
                alloc_site: SourceId(44),
                value: "B".into(),
                trigger: Trigger::ListMutation {
                    cell: slot,
                    seq: TickSeq::new(runtime.tick(), 2),
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

        assert_eq!(first, ItemKey(0));
        assert_eq!(second, ItemKey(1));
        assert_eq!(
            runtime.item_scope(slot, first),
            runtime.item_scope(slot, first)
        );
        assert_ne!(
            runtime.item_scope(slot, first),
            runtime.item_scope(slot, second)
        );

        let remove_seq = TickSeq::new(runtime.tick(), 3);
        runtime.commit_updates(vec![RuntimeUpdate::ListRemove {
            slot,
            item: first,
            trigger: Trigger::ListMutation {
                cell: slot,
                seq: remove_seq,
            },
        }]);

        let append_seq = TickSeq::new(runtime.tick(), 4);
        let appended = runtime.commit_updates(vec![RuntimeUpdate::ListAppend {
            slot,
            alloc_site: SourceId(44),
            value: "C".into(),
            trigger: Trigger::ListMutation {
                cell: slot,
                seq: append_seq,
            },
        }]);
        let third = match appended[0] {
            AppliedUpdate::ListAppend { item, .. } => item,
            _ => panic!("expected list append"),
        };

        assert_eq!(third, ItemKey(2));
        assert_ne!(
            runtime.item_scope(slot, second),
            runtime.item_scope(slot, third)
        );
    }

    #[test]
    fn ui_event_values_are_visible_only_in_the_matching_tick() {
        let mut runtime = Runtime::new();
        let element = ElementId::new(SourceId(3), ScopeId::ROOT, 0);
        let port = EventPortId {
            element,
            ty: EventType::DoubleClick,
        };
        let seq = runtime.next_seq();
        runtime
            .ui_mut()
            .record_event(port, seq, KernelValue::from("payload"));

        assert_eq!(
            runtime.ui().read_event_for_tick(port, runtime.tick()),
            KernelValue::from("payload")
        );

        runtime.begin_tick();
        assert_eq!(
            runtime.ui().read_event_for_tick(port, runtime.tick()),
            KernelValue::Skip
        );
    }
}
