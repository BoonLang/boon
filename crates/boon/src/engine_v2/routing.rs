use std::collections::HashMap;
use super::arena::SlotId;
use super::address::Port;

/// Routes messages between nodes.
#[derive(Debug, Default)]
pub struct RoutingTable {
    /// source_slot -> [(target_slot, target_port)]
    routes: HashMap<SlotId, Vec<(SlotId, Port)>>,
}

impl RoutingTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a route from source to target.
    pub fn add_route(&mut self, source: SlotId, target: SlotId, port: Port) {
        self.routes
            .entry(source)
            .or_default()
            .push((target, port));
    }

    /// Remove a route from source to target.
    pub fn remove_route(&mut self, source: SlotId, target: SlotId, port: Port) {
        if let Some(targets) = self.routes.get_mut(&source) {
            targets.retain(|(t, p)| !(*t == target && *p == port));
        }
    }

    /// Get all targets subscribed to a source.
    pub fn get_subscribers(&self, source: SlotId) -> &[(SlotId, Port)] {
        self.routes.get(&source).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Remove all routes involving a slot (when freed).
    pub fn remove_slot(&mut self, slot: SlotId) {
        self.routes.remove(&slot);
        for targets in self.routes.values_mut() {
            targets.retain(|(t, _)| *t != slot);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routing_add_remove() {
        let mut rt = RoutingTable::new();
        let s1 = SlotId { index: 1, generation: 0 };
        let s2 = SlotId { index: 2, generation: 0 };
        let s3 = SlotId { index: 3, generation: 0 };

        rt.add_route(s1, s2, Port::Output);
        rt.add_route(s1, s3, Port::Input(0));

        let subs = rt.get_subscribers(s1);
        assert_eq!(subs.len(), 2);

        rt.remove_route(s1, s2, Port::Output);
        let subs = rt.get_subscribers(s1);
        assert_eq!(subs.len(), 1);
    }

    #[test]
    fn routing_remove_slot() {
        let mut rt = RoutingTable::new();
        let s1 = SlotId { index: 1, generation: 0 };
        let s2 = SlotId { index: 2, generation: 0 };

        rt.add_route(s1, s2, Port::Output);
        rt.remove_slot(s1);

        assert!(rt.get_subscribers(s1).is_empty());
    }
}
