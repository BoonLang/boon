use crate::editable_mapped_list_preview_runtime::{
    EditableMappedListPreviewRuntime, EditableMappedListProjection,
};
use crate::editable_mapped_list_runtime::EditableMappedListRuntime;
use crate::filtered_list_view::{FilteredListView, filtered_list_with_filter};
use crate::ir::SourcePortId;
use crate::mapped_list_runtime::MappedListItem;
use boon_scene::UiEventBatch;

pub(crate) trait TextFilteredEditableMappedListProjection<T, const INPUTS: usize, const ROWS: usize>:
    EditableMappedListProjection<T, INPUTS, ROWS>
{
    const FILTER_INPUT_INDEX: usize;

    fn item_matches_filter(filter_text: &str, item: &MappedListItem<T>) -> bool;
}

pub(crate) fn text_filtered_items<'a, T, P, const INPUTS: usize, const ROWS: usize>(
    state: &'a EditableMappedListRuntime<T, INPUTS, ROWS>,
) -> FilteredListView<'a, T, impl Fn(&MappedListItem<T>) -> bool + 'a>
where
    P: TextFilteredEditableMappedListProjection<T, INPUTS, ROWS>,
{
    let filter_text = state.input(P::FILTER_INPUT_INDEX).to_string();
    filtered_list_with_filter(state.list(), filter_text, |filter_text, item| {
        P::item_matches_filter(filter_text, item)
    })
}

pub(crate) fn dispatch_text_filtered_ui_events<
    T,
    P,
    const INPUTS: usize,
    const ROWS: usize,
    const BUTTONS: usize,
>(
    runtime: &mut EditableMappedListPreviewRuntime<T, P, INPUTS, ROWS, BUTTONS>,
    batch: UiEventBatch,
    on_button_clicks: impl FnMut(
        &mut EditableMappedListRuntime<T, INPUTS, ROWS>,
        Vec<SourcePortId>,
    ) -> bool,
) -> bool
where
    P: TextFilteredEditableMappedListProjection<T, INPUTS, ROWS>,
{
    let filter_text = runtime.state().input(P::FILTER_INPUT_INDEX).to_string();
    runtime.dispatch_ui_events(
        batch,
        move |item| P::item_matches_filter(&filter_text, item),
        on_button_clicks,
    )
}
