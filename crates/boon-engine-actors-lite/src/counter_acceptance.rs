#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CounterAcceptanceAction {
    ClickButton { index: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CounterAcceptanceSequence {
    pub description: &'static str,
    pub actions: Vec<CounterAcceptanceAction>,
    pub expect: &'static str,
}

pub fn counter_acceptance_sequences() -> Vec<CounterAcceptanceSequence> {
    vec![
        CounterAcceptanceSequence {
            description: "Click increment button",
            actions: vec![CounterAcceptanceAction::ClickButton { index: 0 }],
            expect: "1+",
        },
        CounterAcceptanceSequence {
            description: "Click again",
            actions: vec![CounterAcceptanceAction::ClickButton { index: 0 }],
            expect: "2+",
        },
        CounterAcceptanceSequence {
            description: "Burst click three more times",
            actions: vec![
                CounterAcceptanceAction::ClickButton { index: 0 },
                CounterAcceptanceAction::ClickButton { index: 0 },
                CounterAcceptanceAction::ClickButton { index: 0 },
            ],
            expect: "5+",
        },
    ]
}
