use boon_scene::{RenderDiffBatch, UiEventBatch, UiFactBatch};
use serde::{Serialize, de::DeserializeOwned};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BufferRange {
    pub ptr: u32,
    pub len: u32,
}

impl BufferRange {
    #[must_use]
    pub const fn new(ptr: u32, len: u32) -> Self {
        Self { ptr, len }
    }

    #[must_use]
    pub const fn pack(self) -> u64 {
        ((self.ptr as u64) << 32) | (self.len as u64)
    }

    #[must_use]
    pub const fn from_packed(value: u64) -> Self {
        Self {
            ptr: (value >> 32) as u32,
            len: value as u32,
        }
    }

    #[must_use]
    pub fn slice<'a>(self, memory: &'a [u8]) -> Option<&'a [u8]> {
        let start = self.ptr as usize;
        let end = start.checked_add(self.len as usize)?;
        memory.get(start..end)
    }
}

pub fn encode_json_batch<T: Serialize>(value: &T) -> Vec<u8> {
    serde_json::to_vec(value).expect("Wasm Pro ABI batches should serialize to JSON")
}

pub fn decode_json_batch<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, serde_json::Error> {
    serde_json::from_slice(bytes)
}

pub fn encode_render_diff_batch(batch: &RenderDiffBatch) -> Vec<u8> {
    encode_json_batch(batch)
}

pub fn decode_render_diff_batch(bytes: &[u8]) -> Result<RenderDiffBatch, serde_json::Error> {
    decode_json_batch(bytes)
}

pub fn encode_ui_event_batch(batch: &UiEventBatch) -> Vec<u8> {
    encode_json_batch(batch)
}

pub fn decode_ui_event_batch(bytes: &[u8]) -> Result<UiEventBatch, serde_json::Error> {
    decode_json_batch(bytes)
}

pub fn encode_ui_fact_batch(batch: &UiFactBatch) -> Vec<u8> {
    encode_json_batch(batch)
}

pub fn decode_ui_fact_batch(bytes: &[u8]) -> Result<UiFactBatch, serde_json::Error> {
    decode_json_batch(bytes)
}

#[cfg(test)]
mod tests {
    use boon_scene::{
        EventPortId, NodeId, RenderDiffBatch, RenderOp, UiEvent, UiEventBatch, UiEventKind, UiFact,
        UiFactBatch, UiFactKind,
    };

    use super::{
        BufferRange, decode_render_diff_batch, decode_ui_event_batch, decode_ui_fact_batch,
        encode_render_diff_batch, encode_ui_event_batch, encode_ui_fact_batch,
    };

    #[test]
    fn buffer_range_round_trips_through_packed_u64() {
        let range = BufferRange::new(0x1234_5678, 0x90ab_cdef);
        let restored = BufferRange::from_packed(range.pack());

        assert_eq!(restored, range);
    }

    #[test]
    fn buffer_range_keeps_zero_length() {
        let range = BufferRange::new(42, 0);

        assert_eq!(BufferRange::from_packed(range.pack()), range);
    }

    #[test]
    fn buffer_range_slices_memory_safely() {
        let memory = b"hello wasm pro";
        let range = BufferRange::new(6, 4);

        assert_eq!(range.slice(memory), Some(&b"wasm"[..]));
        assert_eq!(BufferRange::new(100, 4).slice(memory), None);
    }

    #[test]
    fn render_diff_batch_json_round_trips() {
        let batch = RenderDiffBatch {
            ops: vec![RenderOp::DetachEventPort {
                id: NodeId::new(),
                port: EventPortId::new(),
            }],
        };

        let bytes = encode_render_diff_batch(&batch);
        let restored = decode_render_diff_batch(&bytes).expect("render diff batch should decode");

        assert_eq!(restored, batch);
    }

    #[test]
    fn ui_event_batch_json_round_trips() {
        let batch = UiEventBatch {
            events: vec![UiEvent {
                target: EventPortId::new(),
                kind: UiEventKind::Input,
                payload: Some("value".to_string()),
            }],
        };

        let bytes = encode_ui_event_batch(&batch);
        let restored = decode_ui_event_batch(&bytes).expect("ui event batch should decode");

        assert_eq!(restored, batch);
    }

    #[test]
    fn ui_fact_batch_json_round_trips() {
        let batch = UiFactBatch {
            facts: vec![UiFact {
                id: NodeId::new(),
                kind: UiFactKind::DraftText("draft".to_string()),
            }],
        };

        let bytes = encode_ui_fact_batch(&batch);
        let restored = decode_ui_fact_batch(&bytes).expect("ui fact batch should decode");

        assert_eq!(restored, batch);
    }
}
