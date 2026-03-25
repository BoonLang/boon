#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ActorId {
    pub index: u32,
    pub generation: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ScopeId {
    pub index: u32,
    pub generation: u32,
}

pub trait GenerationalId: Copy + Eq {
    fn new(index: u32, generation: u32) -> Self;
    fn index(self) -> usize;
    fn generation(self) -> u32;
}

impl GenerationalId for ActorId {
    fn new(index: u32, generation: u32) -> Self {
        Self { index, generation }
    }

    fn index(self) -> usize {
        self.index as usize
    }

    fn generation(self) -> u32 {
        self.generation
    }
}

impl GenerationalId for ScopeId {
    fn new(index: u32, generation: u32) -> Self {
        Self { index, generation }
    }

    fn index(self) -> usize {
        self.index as usize
    }

    fn generation(self) -> u32 {
        self.generation
    }
}
