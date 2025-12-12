//! WebSocket protocol types for CLI <-> Server <-> Extension communication

use serde::{Deserialize, Serialize};

/// Commands sent from CLI to Extension via Server
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Command {
    /// Click an element by selector
    Click { selector: String },

    /// Type text into an element
    Type { selector: String, text: String },

    /// Press a special key (Enter, Tab, Escape, Backspace, Delete)
    Key { key: String },

    /// Inject code into the CodeMirror editor
    InjectCode { code: String },

    /// Trigger run (call boonPlayground.run())
    TriggerRun,

    /// Take a screenshot
    Screenshot,

    /// Get console messages
    GetConsole,

    /// Get preview panel text content
    GetPreviewText,

    /// Check if extension is connected and ready
    Ping,

    /// Get extension status
    GetStatus,

    /// Reload the extension itself (hot reload for development)
    /// WARNING: This disconnects the extension. Use Refresh instead for page reload.
    Reload,

    /// Detach CDP debugger (use when "debugger already attached" errors occur)
    Detach,

    /// Refresh the page without reloading extension (safer than Reload)
    Refresh,

    /// Scroll the preview panel
    Scroll {
        #[serde(skip_serializing_if = "Option::is_none")]
        y: Option<i32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        delta: Option<i32>,
        #[serde(default)]
        to_bottom: bool,
    },

    /// Get DOM structure for debugging
    GetDOM {
        #[serde(skip_serializing_if = "Option::is_none")]
        selector: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        depth: Option<u32>,
    },

    /// Get preview panel elements with bounding boxes
    GetPreviewElements,

    /// Click at absolute screen coordinates
    ClickAt { x: i32, y: i32 },

    /// Clear saved states (reset localStorage for Boon playground)
    ClearStates,

    /// Select an example by name (e.g., "todo_mvc.bn", "counter.bn")
    SelectExample { name: String },
}

/// Response from Extension to CLI via Server
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Response {
    /// Command succeeded
    Success {
        #[serde(skip_serializing_if = "Option::is_none")]
        data: Option<serde_json::Value>,
    },

    /// Command failed
    Error { message: String },

    /// Screenshot data
    Screenshot { base64: String },

    /// Console messages
    Console { messages: Vec<ConsoleMessage> },

    /// Preview text
    PreviewText { text: String },

    /// Pong response
    Pong,

    /// Extension status
    Status {
        connected: bool,
        #[serde(rename = "pageUrl")]
        page_url: Option<String>,
        #[serde(rename = "apiReady")]
        api_ready: bool,
    },

    /// DOM structure
    Dom { structure: String },

    /// Preview elements with bounding boxes
    PreviewElements { data: serde_json::Value },
}

/// Console message from browser
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsoleMessage {
    pub level: String,
    pub text: String,
    pub timestamp: Option<u64>,
}

/// Request wrapper with ID for request/response matching
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub id: u64,
    pub command: Command,
}

/// Response wrapper with ID
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseMessage {
    pub id: u64,
    pub response: Response,
}
