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
    Reload,
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
