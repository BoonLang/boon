use super::ids::TickSeq;
use super::value::KernelValue;

#[derive(Debug, Clone, PartialEq)]
pub struct LatestCandidate {
    pub value: KernelValue,
    pub last_changed: TickSeq,
}

impl LatestCandidate {
    #[must_use]
    pub fn new(value: KernelValue, last_changed: TickSeq) -> Self {
        Self {
            value,
            last_changed,
        }
    }
}

/// Reference `LATEST` semantics:
/// - ignore `SKIP` candidates unless all are `SKIP`
/// - choose the candidate with the greatest `last_changed`
/// - on ties, keep the lowest index for deterministic behavior
#[must_use]
pub fn select_latest(candidates: &[LatestCandidate]) -> KernelValue {
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
        return KernelValue::Skip;
    };

    candidate.value.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::browser::kernel::{TickId, TickSeq};

    #[test]
    fn latest_picks_most_recent_non_skip_value() {
        let selected = select_latest(&[
            LatestCandidate::new(KernelValue::Skip, TickSeq::new(TickId(3), 8)),
            LatestCandidate::new(KernelValue::from("old"), TickSeq::new(TickId(3), 2)),
            LatestCandidate::new(KernelValue::from("new"), TickSeq::new(TickId(3), 9)),
        ]);

        assert_eq!(selected, KernelValue::from("new"));
    }

    #[test]
    fn latest_uses_input_order_as_tie_breaker() {
        let seq = TickSeq::new(TickId(4), 1);
        let selected = select_latest(&[
            LatestCandidate::new(KernelValue::from("left"), seq),
            LatestCandidate::new(KernelValue::from("right"), seq),
        ]);

        assert_eq!(selected, KernelValue::from("left"));
    }

    #[test]
    fn latest_returns_skip_when_everything_is_skip() {
        let selected = select_latest(&[
            LatestCandidate::new(KernelValue::Skip, TickSeq::new(TickId(1), 0)),
            LatestCandidate::new(KernelValue::Skip, TickSeq::new(TickId(1), 1)),
        ]);

        assert_eq!(selected, KernelValue::Skip);
    }
}
