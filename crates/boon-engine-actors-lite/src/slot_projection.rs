use crate::host_view_preview::HostViewPreviewApp;
use crate::ir::SinkPortId;
use boon::platform::browser::kernel::KernelValue;
use std::collections::BTreeMap;

pub fn project_slot_values_into_map(
    sink_values: &mut BTreeMap<SinkPortId, KernelValue>,
    sinks: &[SinkPortId],
    values: impl IntoIterator<Item = KernelValue>,
    empty: KernelValue,
) {
    let mut values = values.into_iter();
    for sink in sinks {
        let value = values.next().unwrap_or_else(|| empty.clone());
        sink_values.insert(*sink, value);
    }
}

pub(crate) fn project_slot_values_into_app(
    app: &mut HostViewPreviewApp,
    sinks: &[SinkPortId],
    values: impl IntoIterator<Item = KernelValue>,
    empty: KernelValue,
) {
    let mut values = values.into_iter();
    for sink in sinks {
        let value = values.next().unwrap_or_else(|| empty.clone());
        app.set_sink_value(*sink, value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fills_remaining_slots_with_empty_value() {
        let sinks = [SinkPortId(1), SinkPortId(2), SinkPortId(3)];
        let mut sink_values = BTreeMap::new();
        project_slot_values_into_map(
            &mut sink_values,
            &sinks,
            [KernelValue::from("A"), KernelValue::from("B")],
            KernelValue::from(""),
        );
        assert_eq!(
            sink_values.get(&SinkPortId(1)),
            Some(&KernelValue::from("A"))
        );
        assert_eq!(
            sink_values.get(&SinkPortId(2)),
            Some(&KernelValue::from("B"))
        );
        assert_eq!(
            sink_values.get(&SinkPortId(3)),
            Some(&KernelValue::from(""))
        );
    }
}
