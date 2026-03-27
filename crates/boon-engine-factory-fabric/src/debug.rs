use crate::runtime::{FabricTick, RegionId};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineStatus {
    pub engine: &'static str,
    pub supported: bool,
    pub quiescent: bool,
    pub last_flush_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct HostCommandDebug {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DebugSnapshot {
    pub tick: FabricTick,
    pub quiescent: bool,
    pub last_flush_id: u64,
    pub ready_regions: Vec<RegionId>,
    pub regions: Vec<RegionId>,
    pub dirty_sinks: Vec<u16>,
    pub host_commands: Vec<HostCommandDebug>,
    pub retained_node_creations: usize,
    pub retained_node_deletions: usize,
    pub recreated_mapped_scopes: usize,
    pub dirty_closure_size: usize,
    pub last_error: Option<String>,
}

thread_local! {
    static LAST_STATUS: RefCell<Option<EngineStatus>> = const { RefCell::new(None) };
    static LAST_DEBUG: RefCell<Option<DebugSnapshot>> = const { RefCell::new(None) };
}

pub fn publish_runtime_state(status: EngineStatus, debug: Option<DebugSnapshot>) {
    LAST_STATUS.with(|slot| {
        *slot.borrow_mut() = Some(status.clone());
    });
    LAST_DEBUG.with(|slot| {
        *slot.borrow_mut() = debug.clone();
    });
    publish_window_value("__boonEngineStatus", Some(&status));
    publish_window_value("__boonEngineDebug", debug.as_ref());
}

pub fn clear_runtime_state(engine: &'static str) {
    publish_runtime_state(
        EngineStatus {
            engine,
            supported: false,
            quiescent: true,
            last_flush_id: 0,
        },
        None,
    );
}

pub fn publish_error_state(message: impl Into<String>) {
    let message = message.into();
    publish_runtime_state(
        EngineStatus {
            engine: "FactoryFabric",
            supported: false,
            quiescent: true,
            last_flush_id: 0,
        },
        Some(DebugSnapshot {
            tick: FabricTick(0),
            quiescent: true,
            last_flush_id: 0,
            ready_regions: Vec::new(),
            regions: vec![RegionId {
                slot: 0,
                generation: 0,
            }],
            dirty_sinks: Vec::new(),
            host_commands: Vec::new(),
            retained_node_creations: 0,
            retained_node_deletions: 0,
            recreated_mapped_scopes: 0,
            dirty_closure_size: 0,
            last_error: Some(message),
        }),
    );
}

pub fn last_status() -> Option<EngineStatus> {
    LAST_STATUS.with(|slot| slot.borrow().clone())
}

pub fn last_debug_snapshot() -> Option<DebugSnapshot> {
    LAST_DEBUG.with(|slot| slot.borrow().clone())
}

#[cfg(target_arch = "wasm32")]
fn publish_window_value<T: Serialize>(key: &str, value: Option<&T>) {
    use boon::zoon::{js_sys, wasm_bindgen::JsValue, web_sys};

    let Some(window) = web_sys::window() else {
        return;
    };

    let js_value = value
        .and_then(|value| serde_json::to_string(value).ok())
        .and_then(|json| js_sys::JSON::parse(&json).ok())
        .unwrap_or(JsValue::NULL);
    let _ = js_sys::Reflect::set(&window, &JsValue::from_str(key), &js_value);
}

#[cfg(not(target_arch = "wasm32"))]
fn publish_window_value<T: Serialize>(_key: &str, _value: Option<&T>) {}
