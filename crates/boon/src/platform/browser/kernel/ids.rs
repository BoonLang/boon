use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ExprId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SourceId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TickId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TickSeq {
    pub tick: TickId,
    pub seq: u32,
}

impl TickSeq {
    #[must_use]
    pub const fn new(tick: TickId, seq: u32) -> Self {
        Self { tick, seq }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ScopeId(pub u64);

impl ScopeId {
    pub const ROOT: Self = Self(0);

    #[must_use]
    pub fn child(self, source: SourceId, discriminator: u64) -> Self {
        let mut state = self.0 ^ ((source.0 as u64) << 32) ^ discriminator.rotate_left(13);
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        state = (state ^ (state >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        state = (state ^ (state >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        Self(state ^ (state >> 31))
    }
}

impl fmt::Debug for ScopeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ScopeId({:#x})", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SlotKey {
    pub scope: ScopeId,
    pub expr: ExprId,
}

impl SlotKey {
    #[must_use]
    pub const fn new(scope: ScopeId, expr: ExprId) -> Self {
        Self { scope, expr }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ItemKey(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ElementId(pub u128);

impl ElementId {
    #[must_use]
    pub const fn new(source: SourceId, scope: ScopeId, ordinal: u32) -> Self {
        let hi = scope.0 as u128;
        let lo = ((source.0 as u128) << 32) | (ordinal as u128);
        Self((hi << 64) | lo)
    }
}
