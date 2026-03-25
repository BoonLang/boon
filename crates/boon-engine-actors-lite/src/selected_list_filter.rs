use crate::text_input::TextInputState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SelectedListFilter<F> {
    current: F,
}

impl<F: Copy> SelectedListFilter<F> {
    #[must_use]
    pub(crate) fn new(initial: F) -> Self {
        Self { current: initial }
    }

    #[must_use]
    pub(crate) fn current(&self) -> F {
        self.current
    }

    #[must_use]
    pub(crate) fn is(&self, filter: F) -> bool
    where
        F: PartialEq,
    {
        self.current == filter
    }

    pub(crate) fn select(&mut self, next: F) -> bool
    where
        F: PartialEq,
    {
        if self.current == next {
            return false;
        }
        self.current = next;
        true
    }

    pub(crate) fn select_and_clear_focus_hint(
        &mut self,
        next: F,
        input: &mut TextInputState,
    ) -> bool
    where
        F: PartialEq,
    {
        let changed = self.select(next) || input.focus_hint;
        let _ = input.clear_focus_hint();
        changed
    }
}

impl SelectedListFilter<bool> {
    pub(crate) fn toggle(&mut self) -> bool {
        self.select(!self.current)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Filter {
        All,
        Active,
    }

    #[test]
    fn select_changes_only_when_filter_changes() {
        let mut filter = SelectedListFilter::new(Filter::All);
        assert!(!filter.select(Filter::All));
        assert!(filter.select(Filter::Active));
        assert!(filter.is(Filter::Active));
    }

    #[test]
    fn select_and_clear_focus_hint_clears_hint_even_without_filter_change() {
        let mut filter = SelectedListFilter::new(Filter::All);
        let mut input = TextInputState::focused_with_hint("");
        assert!(filter.select_and_clear_focus_hint(Filter::All, &mut input));
        assert!(!input.focus_hint);
        assert!(input.focused);
    }

    #[test]
    fn bool_filter_toggle_flips_state() {
        let mut filter = SelectedListFilter::new(false);
        assert!(filter.toggle());
        assert!(filter.current());
        assert!(filter.toggle());
        assert!(!filter.current());
    }
}
