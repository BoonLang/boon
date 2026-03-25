use crate::host_view_preview::HostViewPreviewApp;
use crate::lower::{ListMapBlockProgram, try_lower_list_map_block};
use boon::platform::browser::kernel::KernelValue;
use boon::zoon::*;
use std::collections::BTreeMap;

const ITEMS: [i64; 5] = [1, 2, 3, 4, 5];

pub struct ListMapBlockPreview {
    app: HostViewPreviewApp,
}

impl ListMapBlockPreview {
    pub fn new(source: &str) -> Result<Self, String> {
        let program = try_lower_list_map_block(source)?;
        let app = HostViewPreviewApp::new(program.host_view.clone(), initial_sink_values(&program));
        Ok(Self { app })
    }

    #[must_use]
    pub fn preview_text(&mut self) -> String {
        self.app.preview_text()
    }
}

fn initial_sink_values(
    program: &ListMapBlockProgram,
) -> BTreeMap<crate::ir::SinkPortId, KernelValue> {
    let mut sink_values = BTreeMap::new();
    sink_values.insert(program.mode_sink, KernelValue::from("Mode: All"));
    for (sink, value) in program.direct_item_sinks.iter().zip(ITEMS) {
        sink_values.insert(*sink, KernelValue::from(value as f64));
    }
    for (sink, value) in program.block_item_sinks.iter().zip(ITEMS) {
        sink_values.insert(*sink, KernelValue::from(value as f64));
    }
    sink_values
}

pub fn render_list_map_block_preview(preview: ListMapBlockPreview) -> impl Element {
    crate::host_view_preview::render_static_host_view(preview.app)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_map_block_preview_renders_mode_and_both_rows() {
        let source = include_str!(
            "../../../playground/frontend/src/examples/list_map_block/list_map_block.bn"
        );
        let mut preview = ListMapBlockPreview::new(source).expect("list_map_block preview");
        assert_eq!(preview.preview_text(), "Mode: All1234512345");
    }
}
