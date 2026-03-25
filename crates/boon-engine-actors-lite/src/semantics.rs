use boon::platform::browser::kernel::KernelValue;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct CausalSeq {
    pub turn: u64,
    pub seq: u32,
}

impl CausalSeq {
    #[must_use]
    pub const fn new(turn: u64, seq: u32) -> Self {
        Self { turn, seq }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct LatestCandidate {
    pub value: KernelValue,
    pub last_changed: CausalSeq,
}

impl LatestCandidate {
    #[must_use]
    pub fn new(value: KernelValue, last_changed: CausalSeq) -> Self {
        Self {
            value,
            last_changed,
        }
    }
}

/// ActorsLite reference selection rule for `LATEST`.
///
/// This intentionally mirrors the kernel contract:
/// - ignore `SKIP` unless every candidate is `SKIP`
/// - choose the candidate with greatest causal recency
/// - on ties, choose the earliest input in source order
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
    use boon::platform::browser::kernel::{
        KernelValue, LatestCandidate as KernelLatestCandidate, TickId, TickSeq,
        select_latest as kernel_select_latest,
    };

    fn compare_to_kernel(candidates: &[(KernelValue, CausalSeq)]) {
        let lite_candidates = candidates
            .iter()
            .cloned()
            .map(|(value, seq)| LatestCandidate::new(value, seq))
            .collect::<Vec<_>>();

        let kernel_candidates = candidates
            .iter()
            .cloned()
            .map(|(value, seq)| {
                KernelLatestCandidate::new(value, TickSeq::new(TickId(seq.turn), seq.seq))
            })
            .collect::<Vec<_>>();

        assert_eq!(
            select_latest(&lite_candidates),
            kernel_select_latest(&kernel_candidates),
        );
    }

    #[test]
    fn latest_matches_kernel_for_most_recent_non_skip() {
        compare_to_kernel(&[
            (KernelValue::Skip, CausalSeq::new(3, 8)),
            (KernelValue::from("old"), CausalSeq::new(3, 2)),
            (KernelValue::from("new"), CausalSeq::new(3, 9)),
        ]);
    }

    #[test]
    fn latest_matches_kernel_for_source_order_tie_break() {
        compare_to_kernel(&[
            (KernelValue::from("left"), CausalSeq::new(4, 1)),
            (KernelValue::from("right"), CausalSeq::new(4, 1)),
        ]);
    }

    #[test]
    fn latest_matches_kernel_when_all_candidates_are_skip() {
        compare_to_kernel(&[
            (KernelValue::Skip, CausalSeq::new(1, 0)),
            (KernelValue::Skip, CausalSeq::new(1, 1)),
        ]);
    }

    #[test]
    fn latest_matches_kernel_for_randomized_candidate_sets() {
        let mut state = 0x1234_5678_9abc_def0u64;

        let mut next_u64 = || {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            state
        };

        for _case in 0..256 {
            let len = (next_u64() % 6 + 1) as usize;
            let mut candidates = Vec::with_capacity(len);
            for index in 0..len {
                let turn = next_u64() % 16;
                let seq = (next_u64() % 8) as u32;
                let value = if next_u64() % 5 == 0 {
                    KernelValue::Skip
                } else {
                    KernelValue::from(format!("v{index}-{}", next_u64() % 7))
                };
                candidates.push((value, CausalSeq::new(turn, seq)));
            }
            compare_to_kernel(&candidates);
        }
    }
}
