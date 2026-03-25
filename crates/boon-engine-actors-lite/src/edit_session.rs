use crate::text_input::TextInputState;
use crate::text_input::decode_key_down_payload;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EditSession<Target> {
    pub target: Target,
    pub input: TextInputState,
}

impl<Target> EditSession<Target> {
    #[must_use]
    pub fn with_focus_hint(target: Target, draft: impl Into<String>) -> Self {
        Self {
            target,
            input: TextInputState::with_focus_hint(draft),
        }
    }
}

impl<Target> EditSession<Target>
where
    Target: PartialEq,
{
    #[must_use]
    pub fn matches(&self, target: &Target) -> bool {
        self.target == *target
    }
}

pub(crate) trait EditSessionStateExt<Target> {
    fn begin_edit_session(&mut self, target: Target, draft: impl Into<String>) -> bool;
    fn has_edit_session(&self, target: &Target) -> bool;
    fn clear_edit_session(&mut self, target: &Target) -> bool;
    fn set_edit_draft(&mut self, target: &Target, draft: impl Into<String>) -> bool;
    fn apply_edit_focus(&mut self, target: &Target, focused: bool) -> bool;
}

impl<Target> EditSessionStateExt<Target> for Option<EditSession<Target>>
where
    Target: Clone + PartialEq,
{
    fn begin_edit_session(&mut self, target: Target, draft: impl Into<String>) -> bool {
        let next = Some(EditSession::with_focus_hint(target, draft));
        if *self == next {
            return false;
        }
        *self = next;
        true
    }

    fn has_edit_session(&self, target: &Target) -> bool {
        self.as_ref().is_some_and(|editing| editing.matches(target))
    }

    fn clear_edit_session(&mut self, target: &Target) -> bool {
        if self.has_edit_session(target) {
            *self = None;
            true
        } else {
            false
        }
    }

    fn set_edit_draft(&mut self, target: &Target, draft: impl Into<String>) -> bool {
        if let Some(editing) = self.as_mut() {
            if editing.matches(target) {
                return editing.input.set_draft(draft);
            }
        }
        false
    }

    fn apply_edit_focus(&mut self, target: &Target, focused: bool) -> bool {
        if let Some(editing) = self.as_mut() {
            if editing.matches(target) {
                return editing.input.apply_focus(focused);
            }
        }
        false
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EditKeyDownOutcome {
    pub matched: bool,
    pub input_changed: bool,
    pub committed_draft: Option<String>,
    pub cancelled: bool,
}

impl EditKeyDownOutcome {
    #[must_use]
    pub fn no_match() -> Self {
        Self {
            matched: false,
            input_changed: false,
            committed_draft: None,
            cancelled: false,
        }
    }
}

pub(crate) fn apply_edit_session_key_down<Target>(
    editing: &mut Option<EditSession<Target>>,
    target: &Target,
    payload: Option<&str>,
) -> EditKeyDownOutcome
where
    Target: PartialEq,
{
    let Some(active) = editing.as_mut() else {
        return EditKeyDownOutcome::no_match();
    };
    if !active.matches(target) {
        return EditKeyDownOutcome::no_match();
    }

    let keydown = decode_key_down_payload(payload);
    let mut input_changed = false;
    if let Some(current_text) = keydown.current_text {
        input_changed |= active.input.set_draft(current_text);
    }

    match keydown.key.as_str() {
        "Enter" => {
            let committed_draft = active.input.draft.clone();
            *editing = None;
            EditKeyDownOutcome {
                matched: true,
                input_changed,
                committed_draft: Some(committed_draft),
                cancelled: false,
            }
        }
        "Escape" => {
            *editing = None;
            EditKeyDownOutcome {
                matched: true,
                input_changed,
                committed_draft: None,
                cancelled: true,
            }
        }
        _ => EditKeyDownOutcome {
            matched: true,
            input_changed,
            committed_draft: None,
            cancelled: false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text_input::KEYDOWN_TEXT_SEPARATOR;

    #[test]
    fn keydown_enter_commits_and_clears_session() {
        let mut editing = Some(EditSession::with_focus_hint(1_u64, "Alpha"));
        let outcome = apply_edit_session_key_down(
            &mut editing,
            &1,
            Some(&format!("Enter{KEYDOWN_TEXT_SEPARATOR}Bravo")),
        );
        assert_eq!(
            outcome,
            EditKeyDownOutcome {
                matched: true,
                input_changed: true,
                committed_draft: Some("Bravo".to_string()),
                cancelled: false,
            }
        );
        assert!(editing.is_none());
    }

    #[test]
    fn keydown_escape_cancels_and_clears_session() {
        let mut editing = Some(EditSession::with_focus_hint(1_u64, "Alpha"));
        let outcome = apply_edit_session_key_down(&mut editing, &1, Some("Escape"));
        assert_eq!(
            outcome,
            EditKeyDownOutcome {
                matched: true,
                input_changed: false,
                committed_draft: None,
                cancelled: true,
            }
        );
        assert!(editing.is_none());
    }

    #[test]
    fn keydown_other_key_only_updates_draft() {
        let mut editing = Some(EditSession::with_focus_hint((1_u32, 2_u32), "Alpha"));
        let outcome = apply_edit_session_key_down(
            &mut editing,
            &(1, 2),
            Some(&format!("x{KEYDOWN_TEXT_SEPARATOR}Bravo")),
        );
        assert_eq!(
            outcome,
            EditKeyDownOutcome {
                matched: true,
                input_changed: true,
                committed_draft: None,
                cancelled: false,
            }
        );
        assert_eq!(
            editing.as_ref().expect("editing remains").input.draft,
            "Bravo".to_string()
        );
    }
}
