use crate::bridge::{HostInput, HostSnapshot, enqueue_host_snapshot};
use crate::ids::{ActorId, ScopeId};
use crate::ir::SourcePortId;
use crate::runtime::{ActorKind, Msg, RuntimeCore, RuntimeTelemetrySnapshot};
use crate::semantics::CausalSeq;
use boon::platform::browser::kernel::KernelValue;

#[derive(Debug)]
pub struct PreviewRuntime {
    runtime: RuntimeCore,
    root_scope: ScopeId,
    turn: u64,
}

impl Default for PreviewRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl PreviewRuntime {
    #[must_use]
    pub fn new() -> Self {
        let mut runtime = RuntimeCore::new();
        let root_scope = runtime.alloc_scope(None);
        Self {
            runtime,
            root_scope,
            turn: 1,
        }
    }

    #[must_use]
    pub fn root_scope_id(&self) -> ScopeId {
        self.root_scope
    }

    pub fn alloc_actor(&mut self, kind: ActorKind) -> ActorId {
        self.runtime.alloc_actor(kind, self.root_scope)
    }

    #[must_use]
    pub fn causal_seq(&self, seq: u32) -> CausalSeq {
        CausalSeq::new(self.turn, seq)
    }

    pub fn dispatch_pulse(
        &mut self,
        actor: ActorId,
        port: SourcePortId,
        value: KernelValue,
    ) -> Vec<(ActorId, Msg)> {
        self.dispatch_snapshot(HostSnapshot::new(vec![HostInput::Pulse {
            actor,
            port,
            value,
            seq: self.causal_seq(0),
        }]))
    }

    pub fn dispatch_snapshot(&mut self, snapshot: HostSnapshot) -> Vec<(ActorId, Msg)> {
        enqueue_host_snapshot(&mut self.runtime, &snapshot);
        let drained = self.drain_to_quiescence();
        self.turn += 1;
        drained
    }

    fn drain_to_quiescence(&mut self) -> Vec<(ActorId, Msg)> {
        let mut delivered = Vec::new();
        while let Some(actor_id) = self.runtime.pop_ready() {
            let messages = {
                let actor = self
                    .runtime
                    .actors
                    .get_mut(actor_id)
                    .expect("ready actor should exist");
                actor.mailbox.drain(..).collect::<Vec<_>>()
            };

            for message in messages {
                delivered.push((actor_id, message));
            }

            self.runtime.mark_unscheduled_if_idle(actor_id);
        }
        delivered
    }

    #[must_use]
    pub fn telemetry_snapshot(&self) -> RuntimeTelemetrySnapshot {
        self.runtime.telemetry_snapshot()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::MirrorCellId;

    #[test]
    fn dispatch_snapshot_returns_messages_in_snapshot_order() {
        let mut runtime = PreviewRuntime::new();
        let actor = runtime.alloc_actor(ActorKind::SourcePort);

        let drained = runtime.dispatch_snapshot(HostSnapshot::new(vec![
            HostInput::Mirror {
                actor,
                cell: MirrorCellId(7),
                value: KernelValue::from("draft"),
                seq: runtime.causal_seq(0),
            },
            HostInput::Pulse {
                actor,
                port: SourcePortId(3),
                value: KernelValue::from("Enter"),
                seq: runtime.causal_seq(1),
            },
        ]));

        assert_eq!(
            drained,
            vec![
                (
                    actor,
                    Msg::MirrorWrite {
                        cell: MirrorCellId(7),
                        value: KernelValue::from("draft"),
                        seq: CausalSeq::new(1, 0),
                    },
                ),
                (
                    actor,
                    Msg::SourcePulse {
                        port: SourcePortId(3),
                        value: KernelValue::from("Enter"),
                        seq: CausalSeq::new(1, 1),
                    },
                ),
            ]
        );
    }

    #[test]
    fn dispatch_pulse_uses_current_turn_and_advances_it() {
        let mut runtime = PreviewRuntime::new();
        let actor = runtime.alloc_actor(ActorKind::SourcePort);

        let first = runtime.dispatch_pulse(actor, SourcePortId(1), KernelValue::from("click"));
        let second = runtime.dispatch_pulse(actor, SourcePortId(1), KernelValue::from("click"));

        assert_eq!(
            first,
            vec![(
                actor,
                Msg::SourcePulse {
                    port: SourcePortId(1),
                    value: KernelValue::from("click"),
                    seq: CausalSeq::new(1, 0),
                },
            )]
        );
        assert_eq!(
            second,
            vec![(
                actor,
                Msg::SourcePulse {
                    port: SourcePortId(1),
                    value: KernelValue::from("click"),
                    seq: CausalSeq::new(2, 0),
                },
            )]
        );
    }
}
