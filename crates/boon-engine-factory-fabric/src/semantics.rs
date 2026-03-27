use crate::{FabricSeq, FabricValue};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LatestCandidate {
    pub value: FabricValue,
    pub last_changed: FabricSeq,
}

impl LatestCandidate {
    #[must_use]
    pub fn new(value: FabricValue, last_changed: FabricSeq) -> Self {
        Self {
            value,
            last_changed,
        }
    }
}

/// Reference `LATEST` semantics:
/// - ignore `SKIP` candidates unless all are `SKIP`
/// - choose the candidate with the greatest `last_changed`
/// - on ties, keep the lowest input index for deterministic behavior
#[must_use]
pub fn select_latest(candidates: &[LatestCandidate]) -> FabricValue {
    let Some((_, candidate)) = candidates
        .iter()
        .enumerate()
        .filter(|(_, candidate)| !candidate.value.is_skip())
        .max_by(|(lhs_idx, lhs), (rhs_idx, rhs)| {
            lhs.last_changed
                .cmp(&rhs.last_changed)
                .then_with(|| rhs_idx.cmp(lhs_idx))
        })
    else {
        return FabricValue::Skip;
    };

    candidate.value.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FabricTick;
    use boon::platform::browser::kernel::{
        LatestCandidate as KernelLatestCandidate, TickId, TickSeq as KernelTickSeq,
        select_latest as kernel_select_latest,
    };

    fn to_kernel_value(value: &FabricValue) -> boon::platform::browser::kernel::KernelValue {
        match value {
            FabricValue::Number(value) => {
                boon::platform::browser::kernel::KernelValue::Number(*value as f64)
            }
            FabricValue::Text(value) => {
                boon::platform::browser::kernel::KernelValue::Text(value.clone())
            }
            FabricValue::Bool(value) => boon::platform::browser::kernel::KernelValue::Bool(*value),
            FabricValue::Skip => boon::platform::browser::kernel::KernelValue::Skip,
        }
    }

    fn to_kernel_seq(seq: FabricSeq) -> KernelTickSeq {
        KernelTickSeq::new(TickId(seq.tick.0), seq.order)
    }

    fn assert_matches_kernel(candidates: &[LatestCandidate]) {
        let expected = kernel_select_latest(
            &candidates
                .iter()
                .map(|candidate| {
                    KernelLatestCandidate::new(
                        to_kernel_value(&candidate.value),
                        to_kernel_seq(candidate.last_changed),
                    )
                })
                .collect::<Vec<_>>(),
        );
        let actual = select_latest(candidates);
        assert_eq!(to_kernel_value(&actual), expected);
    }

    #[test]
    fn latest_picks_most_recent_non_skip_value() {
        let candidates = [
            LatestCandidate::new(
                FabricValue::Skip,
                FabricSeq {
                    tick: FabricTick(3),
                    order: 8,
                },
            ),
            LatestCandidate::new(
                FabricValue::from("old"),
                FabricSeq {
                    tick: FabricTick(3),
                    order: 2,
                },
            ),
            LatestCandidate::new(
                FabricValue::from("new"),
                FabricSeq {
                    tick: FabricTick(3),
                    order: 9,
                },
            ),
        ];
        let selected = select_latest(&candidates);

        assert_eq!(selected, FabricValue::from("new"));
        assert_matches_kernel(&candidates);
    }

    #[test]
    fn latest_uses_input_order_as_tie_breaker() {
        let seq = FabricSeq {
            tick: FabricTick(4),
            order: 1,
        };
        let candidates = [
            LatestCandidate::new(FabricValue::from("left"), seq),
            LatestCandidate::new(FabricValue::from("right"), seq),
        ];
        let selected = select_latest(&candidates);

        assert_eq!(selected, FabricValue::from("left"));
        assert_matches_kernel(&candidates);
    }

    #[test]
    fn latest_returns_skip_when_everything_is_skip() {
        let candidates = [
            LatestCandidate::new(
                FabricValue::Skip,
                FabricSeq {
                    tick: FabricTick(1),
                    order: 0,
                },
            ),
            LatestCandidate::new(
                FabricValue::Skip,
                FabricSeq {
                    tick: FabricTick(1),
                    order: 1,
                },
            ),
        ];
        let selected = select_latest(&candidates);

        assert_eq!(selected, FabricValue::Skip);
        assert_matches_kernel(&candidates);
    }
}
