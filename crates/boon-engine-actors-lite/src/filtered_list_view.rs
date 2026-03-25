use crate::host_view_preview::HostViewPreviewApp;
use crate::ir::SinkPortId;
use crate::mapped_list_runtime::MappedListItem;
use crate::mapped_list_view_runtime::MappedListViewRuntime;
use boon::platform::browser::kernel::KernelValue;
use std::collections::BTreeMap;

pub(crate) struct FilteredListView<'a, T, V> {
    list: &'a MappedListViewRuntime<T>,
    is_visible: V,
}

pub(crate) fn filtered_list_with_filter<'a, T, F: 'a>(
    list: &'a MappedListViewRuntime<T>,
    filter: F,
    is_visible: impl Fn(&F, &MappedListItem<T>) -> bool + 'a,
) -> FilteredListView<'a, T, impl Fn(&MappedListItem<T>) -> bool + 'a> {
    FilteredListView::new(list, move |item| is_visible(&filter, item))
}

impl<'a, T, V> FilteredListView<'a, T, V>
where
    V: Fn(&MappedListItem<T>) -> bool,
{
    #[must_use]
    pub(crate) fn new(list: &'a MappedListViewRuntime<T>, is_visible: V) -> Self {
        Self { list, is_visible }
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &'a MappedListItem<T>> {
        self.list
            .items()
            .iter()
            .filter(|item| (self.is_visible)(item))
    }

    #[must_use]
    pub(crate) fn ids(&self) -> Vec<u64> {
        self.iter().map(|item| item.id).collect()
    }

    pub(crate) fn project_into_map(
        &self,
        sink_values: &mut BTreeMap<SinkPortId, KernelValue>,
        sinks: &[SinkPortId],
        to_value: impl Fn(&MappedListItem<T>) -> KernelValue,
        empty: KernelValue,
    ) {
        self.list
            .project_visible_into_map(sink_values, sinks, &self.is_visible, to_value, empty);
    }

    pub(crate) fn project_into_app(
        &self,
        app: &mut HostViewPreviewApp,
        sinks: &[SinkPortId],
        to_value: impl Fn(&MappedListItem<T>) -> KernelValue,
        empty: KernelValue,
    ) {
        self.list
            .project_visible_into_app(app, sinks, &self.is_visible, to_value, empty);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host_view_preview::HostViewPreviewApp;
    use crate::ir::{FunctionInstanceId, RetainedNodeKey, SinkPortId, ViewSiteId};
    use boon::platform::browser::kernel::KernelValue;
    use boon_scene::UiNodeKind;
    use std::collections::BTreeMap;

    #[test]
    fn ids_include_only_visible_items() {
        let list = MappedListViewRuntime::new([(0, 1_i64), (1, 2_i64), (2, 3_i64)], 3);
        let filtered = FilteredListView::new(&list, |item| item.value % 2 == 1);
        assert_eq!(filtered.ids(), vec![0, 2]);
    }

    #[test]
    fn filtered_list_with_filter_binds_external_filter_value() {
        let list = MappedListViewRuntime::new([(0, 1_i64), (1, 2_i64), (2, 3_i64)], 3);
        let filtered = filtered_list_with_filter(&list, true, |show_even, item| {
            *show_even == (item.value % 2 == 0)
        });
        assert_eq!(filtered.ids(), vec![1]);
    }

    #[test]
    fn filtered_list_with_filter_matches_text_prefix() {
        let list = MappedListViewRuntime::new(
            [(0, "Alpha"), (1, "Bravo"), (2, "Beta"), (3, "Charlie")],
            4,
        );
        let filtered = filtered_list_with_filter(&list, "B", |prefix, item| {
            prefix.is_empty() || item.value.starts_with(prefix)
        });
        assert_eq!(filtered.ids(), vec![1, 2]);
    }

    #[test]
    fn projection_fills_only_visible_slots() {
        let list = MappedListViewRuntime::new([(0, 1_i64), (1, 2_i64), (2, 3_i64)], 3);
        let filtered = FilteredListView::new(&list, |item| item.value >= 2);
        let sinks = [SinkPortId(1), SinkPortId(2), SinkPortId(3)];

        let mut sink_values = BTreeMap::new();
        filtered.project_into_map(
            &mut sink_values,
            &sinks,
            |item| KernelValue::from(item.value.to_string()),
            KernelValue::from(""),
        );
        assert_eq!(
            sink_values.get(&SinkPortId(1)),
            Some(&KernelValue::from("2"))
        );
        assert_eq!(
            sink_values.get(&SinkPortId(2)),
            Some(&KernelValue::from("3"))
        );
        assert_eq!(
            sink_values.get(&SinkPortId(3)),
            Some(&KernelValue::from(""))
        );

        let mut app = HostViewPreviewApp::new(
            crate::bridge::HostViewIr {
                root: Some(crate::bridge::HostViewNode {
                    retained_key: RetainedNodeKey {
                        view_site: ViewSiteId(1),
                        function_instance: Some(FunctionInstanceId(1)),
                        mapped_item_identity: None,
                    },
                    kind: crate::bridge::HostViewKind::Stripe,
                    children: Vec::new(),
                }),
            },
            BTreeMap::new(),
        );
        filtered.project_into_app(
            &mut app,
            &sinks,
            |item| KernelValue::from(item.value.to_string()),
            KernelValue::from(""),
        );
        assert_eq!(app.sink_value(SinkPortId(1)), Some(&KernelValue::from("2")));
        assert_eq!(app.sink_value(SinkPortId(2)), Some(&KernelValue::from("3")));
        assert_eq!(app.sink_value(SinkPortId(3)), Some(&KernelValue::from("")));

        let (root, _) = app.render_snapshot();
        match root.kind {
            UiNodeKind::Element { .. } => {}
            _ => panic!("expected ui tree root"),
        }
    }
}
