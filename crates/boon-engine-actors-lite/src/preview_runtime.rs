use crate::bridge::{HostInput, enqueue_host_inputs, enqueue_host_pulse};
use crate::ids::ActorId;
use crate::ir::SourcePortId;
use crate::runtime::{Msg, RuntimeCore, RuntimeTelemetrySnapshot};
use crate::semantics::CausalSeq;
use boon::platform::browser::kernel::KernelValue;

#[derive(Debug)]
pub(crate) struct PreviewRuntime {
    runtime: RuntimeCore,
    turn: u64,
    message_scratch: Vec<Msg>,
}

impl Default for PreviewRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl PreviewRuntime {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            runtime: RuntimeCore::new(),
            turn: 1,
            message_scratch: Vec::new(),
        }
    }

    pub(crate) fn alloc_actor(&mut self) -> ActorId {
        self.runtime.alloc_actor()
    }

    #[must_use]
    pub(crate) fn causal_seq(&self, seq: u32) -> CausalSeq {
        CausalSeq::new(self.turn, seq)
    }

    #[cfg(test)]
    pub(crate) fn dispatch_pulse(
        &mut self,
        actor: ActorId,
        port: SourcePortId,
        value: KernelValue,
    ) -> Vec<Msg> {
        let mut delivered = Vec::new();
        self.dispatch_pulse_batches(actor, port, value, |messages| {
            delivered.extend_from_slice(messages.as_slice());
        });
        delivered
    }

    #[cfg(test)]
    pub(crate) fn dispatch_inputs(&mut self, inputs: &[HostInput]) -> Vec<Msg> {
        let mut delivered = Vec::new();
        self.dispatch_inputs_batches(inputs, |messages| {
            delivered.extend_from_slice(messages.as_slice());
        });
        delivered
    }

    pub(crate) fn dispatch_pulse_batches(
        &mut self,
        actor: ActorId,
        port: SourcePortId,
        value: KernelValue,
        mut deliver: impl FnMut(&mut Vec<Msg>),
    ) {
        let seq = self.causal_seq(0);
        let _ = enqueue_host_pulse(&mut self.runtime, actor, port, value, seq);
        self.drain_to_quiescence_batches(&mut deliver);
        self.turn += 1;
    }

    pub(crate) fn dispatch_inputs_batches(
        &mut self,
        inputs: &[HostInput],
        mut deliver: impl FnMut(&mut Vec<Msg>),
    ) {
        enqueue_host_inputs(&mut self.runtime, inputs);
        self.drain_to_quiescence_batches(&mut deliver);
        self.turn += 1;
    }

    fn drain_to_quiescence_batches(&mut self, mut deliver: impl FnMut(&mut Vec<Msg>)) {
        self.message_scratch.clear();
        while self
            .runtime
            .drain_next_ready_messages(&mut self.message_scratch)
        {
            deliver(&mut self.message_scratch);
            self.message_scratch.clear();
        }
    }

    #[must_use]
    pub(crate) fn telemetry_snapshot(&self) -> RuntimeTelemetrySnapshot {
        self.runtime.telemetry_snapshot()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::HostInput;
    use crate::ir::MirrorCellId;

    #[test]
    fn dispatch_inputs_returns_messages_in_input_order() {
        let mut runtime = PreviewRuntime::new();
        let actor = runtime.alloc_actor();

        let drained = runtime.dispatch_inputs(&[
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
        ]);

        assert_eq!(
            drained,
            vec![
                Msg::MirrorWrite {
                    cell: MirrorCellId(7),
                    value: KernelValue::from("draft"),
                    seq: CausalSeq::new(1, 0),
                },
                Msg::SourcePulse {
                    port: SourcePortId(3),
                    value: KernelValue::from("Enter"),
                    seq: CausalSeq::new(1, 1),
                },
            ]
        );
    }

    #[test]
    fn dispatch_pulse_uses_current_turn_and_advances_it() {
        let mut runtime = PreviewRuntime::new();
        let actor = runtime.alloc_actor();

        let first = runtime.dispatch_pulse(actor, SourcePortId(1), KernelValue::from("click"));
        let second = runtime.dispatch_pulse(actor, SourcePortId(1), KernelValue::from("click"));

        assert_eq!(
            first,
            vec![Msg::SourcePulse {
                port: SourcePortId(1),
                value: KernelValue::from("click"),
                seq: CausalSeq::new(1, 0),
            }]
        );
        assert_eq!(
            second,
            vec![Msg::SourcePulse {
                port: SourcePortId(1),
                value: KernelValue::from("click"),
                seq: CausalSeq::new(2, 0),
            }]
        );
    }

    #[test]
    fn dispatch_inputs_drains_ready_actors_in_first_seen_actor_order() {
        let mut runtime = PreviewRuntime::new();
        let first_actor = runtime.alloc_actor();
        let second_actor = runtime.alloc_actor();

        let drained = runtime.dispatch_inputs(&[
            HostInput::Pulse {
                actor: first_actor,
                port: SourcePortId(1),
                value: KernelValue::from("first"),
                seq: runtime.causal_seq(0),
            },
            HostInput::Pulse {
                actor: second_actor,
                port: SourcePortId(2),
                value: KernelValue::from("second"),
                seq: runtime.causal_seq(1),
            },
            HostInput::Mirror {
                actor: first_actor,
                cell: MirrorCellId(9),
                value: KernelValue::from("draft"),
                seq: runtime.causal_seq(2),
            },
        ]);

        assert_eq!(
            drained,
            vec![
                Msg::SourcePulse {
                    port: SourcePortId(1),
                    value: KernelValue::from("first"),
                    seq: CausalSeq::new(1, 0),
                },
                Msg::MirrorWrite {
                    cell: MirrorCellId(9),
                    value: KernelValue::from("draft"),
                    seq: CausalSeq::new(1, 2),
                },
                Msg::SourcePulse {
                    port: SourcePortId(2),
                    value: KernelValue::from("second"),
                    seq: CausalSeq::new(1, 1),
                },
            ]
        );
    }

    #[test]
    fn dispatch_inputs_batches_preserve_message_order_across_ready_drains() {
        let mut runtime = PreviewRuntime::new();
        let first_actor = runtime.alloc_actor();
        let second_actor = runtime.alloc_actor();
        let mut drained = Vec::new();

        runtime.dispatch_inputs_batches(
            &[
                HostInput::Pulse {
                    actor: first_actor,
                    port: SourcePortId(1),
                    value: KernelValue::from("first"),
                    seq: runtime.causal_seq(0),
                },
                HostInput::Pulse {
                    actor: second_actor,
                    port: SourcePortId(2),
                    value: KernelValue::from("second"),
                    seq: runtime.causal_seq(1),
                },
                HostInput::Mirror {
                    actor: first_actor,
                    cell: MirrorCellId(9),
                    value: KernelValue::from("draft"),
                    seq: runtime.causal_seq(2),
                },
            ],
            |messages| drained.extend_from_slice(messages),
        );

        assert_eq!(
            drained,
            vec![
                Msg::SourcePulse {
                    port: SourcePortId(1),
                    value: KernelValue::from("first"),
                    seq: CausalSeq::new(1, 0),
                },
                Msg::MirrorWrite {
                    cell: MirrorCellId(9),
                    value: KernelValue::from("draft"),
                    seq: CausalSeq::new(1, 2),
                },
                Msg::SourcePulse {
                    port: SourcePortId(2),
                    value: KernelValue::from("second"),
                    seq: CausalSeq::new(1, 1),
                },
            ]
        );
    }
}
