#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TodoAcceptanceAction {
    DblClickText { text: &'static str },
    AssertFocused { index: u32 },
    AssertInputTypeable { index: u32 },
    TypeText { text: &'static str },
    FocusInput { index: u32 },
    Key { key: &'static str },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TodoAcceptanceSequence {
    pub description: &'static str,
    pub actions: Vec<TodoAcceptanceAction>,
    pub expect: &'static str,
}

pub fn todo_edit_save_acceptance_sequences() -> Vec<TodoAcceptanceSequence> {
    vec![
        TodoAcceptanceSequence {
            description: "Double-click Buy milk to enter edit mode for save test",
            actions: vec![TodoAcceptanceAction::DblClickText { text: "Buy milk" }],
            expect: "1 item left",
        },
        TodoAcceptanceSequence {
            description: "BUG TEST: Save edit input should have focus",
            actions: vec![TodoAcceptanceAction::AssertFocused { index: 1 }],
            expect: "1 item left",
        },
        TodoAcceptanceSequence {
            description: "BUG TEST: Save edit input should be typeable",
            actions: vec![TodoAcceptanceAction::AssertInputTypeable { index: 1 }],
            expect: "1 item left",
        },
        TodoAcceptanceSequence {
            description: "BUG TEST: Type ' EDITED' to append to title",
            actions: vec![TodoAcceptanceAction::TypeText { text: " EDITED" }],
            expect: "1 item left",
        },
        TodoAcceptanceSequence {
            description: "BUG TEST: Press Enter to save the edited title",
            actions: vec![
                TodoAcceptanceAction::FocusInput { index: 1 },
                TodoAcceptanceAction::Key { key: "Enter" },
            ],
            expect: "1 item left",
        },
        TodoAcceptanceSequence {
            description: "BUG TEST: Verify the title was saved with appended text",
            actions: vec![],
            expect: "Buy milk EDITED",
        },
    ]
}
