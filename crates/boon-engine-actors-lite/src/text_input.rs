#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TextInputState {
    pub draft: String,
    pub focus_hint: bool,
    pub focused: bool,
}

impl TextInputState {
    #[must_use]
    pub fn with_focus_hint(draft: impl Into<String>) -> Self {
        Self {
            draft: draft.into(),
            focus_hint: true,
            focused: false,
        }
    }

    #[must_use]
    pub fn focused_with_hint(draft: impl Into<String>) -> Self {
        Self {
            draft: draft.into(),
            focus_hint: true,
            focused: true,
        }
    }

    pub fn set_draft(&mut self, next: impl Into<String>) -> bool {
        let next = next.into();
        if self.draft == next {
            return false;
        }
        self.draft = next;
        true
    }

    pub fn apply_focus(&mut self, focused: bool) -> bool {
        let changed = self.focused != focused || (focused && self.focus_hint);
        self.focused = focused;
        if focused {
            self.focus_hint = false;
        }
        changed
    }

    pub fn blur(&mut self) -> bool {
        if !self.focused {
            return false;
        }
        self.focused = false;
        true
    }

    pub fn clear_focus_hint(&mut self) -> bool {
        if !self.focus_hint {
            return false;
        }
        self.focus_hint = false;
        true
    }

    pub fn request_focus(&mut self) -> bool {
        let changed = !self.focused || !self.focus_hint;
        self.focused = true;
        self.focus_hint = true;
        changed
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DecodedKeyDown {
    pub key: String,
    pub current_text: Option<String>,
}

pub(crate) const KEYDOWN_TEXT_SEPARATOR: char = '\u{1F}';

#[must_use]
pub(crate) fn decode_key_down_payload(payload: Option<&str>) -> DecodedKeyDown {
    let Some(payload) = payload else {
        return DecodedKeyDown {
            key: String::new(),
            current_text: None,
        };
    };
    match payload.split_once(KEYDOWN_TEXT_SEPARATOR) {
        Some((key, text)) => DecodedKeyDown {
            key: key.to_string(),
            current_text: Some(text.to_string()),
        },
        None => DecodedKeyDown {
            key: payload.to_string(),
            current_text: None,
        },
    }
}
