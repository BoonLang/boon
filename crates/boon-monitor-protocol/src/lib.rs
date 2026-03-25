use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonitorEnvelope {
    pub source: MonitorSource,
    pub event: MonitorEvent,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MonitorSource {
    Actors,
    ActorsLite,
    Dd,
    Wasm,
    Renderer(String),
    Storage(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MonitorEvent {
    Revision {
        entity: String,
        revision: u64,
    },
    Dependency {
        from: String,
        to: String,
    },
    Queue {
        queue: String,
        depth: usize,
    },
    RenderDiff {
        renderer: String,
        op_count: usize,
    },
    Storage {
        backend: String,
        operation: String,
        key: String,
    },
    Message {
        level: MonitorLevel,
        text: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MonitorLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}
