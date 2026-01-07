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
    /// If filename is provided, sets current file first (for persistence)
    InjectCode { code: String, filename: Option<String> },

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

    /// Hover at absolute screen coordinates (move mouse without clicking)
    HoverAt { x: i32, y: i32 },

    /// Double-click at absolute screen coordinates
    DoubleClickAt { x: i32, y: i32 },

    /// Clear saved states (reset localStorage for Boon playground)
    ClearStates,

    /// Select an example by name (e.g., "todo_mvc.bn", "counter.bn")
    SelectExample { name: String },

    /// Get current editor code
    GetEditorCode,

    /// Take screenshot of a specific element by selector
    ScreenshotElement { selector: String },

    /// Get accessibility tree of preview pane
    GetAccessibilityTree,

    /// Click checkbox by index in preview pane (0-indexed)
    ClickCheckbox { index: u32 },

    /// Click button by index in preview pane (0-indexed, buttons only)
    ClickButton { index: u32 },

    /// Click a button near an element with specific text (e.g., × button for a todo item)
    /// More reliable than hover + click_button because hover state can be unreliable
    ClickButtonNearText {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        button_text: Option<String>,
    },

    /// Click any element by its text content
    ClickByText { text: String, exact: bool },

    /// Focus an input element by index in preview pane (0-indexed)
    FocusInput { index: u32 },

    /// Type text into the currently focused element
    TypeText { text: String },

    /// Press a special key (Enter, Escape, Tab, Backspace, Delete)
    PressKey { key: String },

    /// Get localStorage entries, optionally filtered by pattern
    GetLocalStorage {
        #[serde(skip_serializing_if = "Option::is_none")]
        pattern: Option<String>,
    },

    /// Get information about the currently focused element
    GetFocusedElement,

    /// Get properties of an input element by index (placeholder, value, type)
    GetInputProperties { index: u32 },

    /// Get the current page URL
    GetCurrentUrl,

    /// Double-click on element by text content
    DoubleClickByText { text: String, exact: bool },

    /// Hover over element by text content
    HoverByText { text: String, exact: bool },

    /// Verify input is actually typeable (not disabled/readonly/hidden)
    VerifyInputTypeable { index: u32 },

    /// ATOMIC: Run code and capture initial preview BEFORE any async events fire
    /// This is critical for testing initial state before timer-based updates
    RunAndCaptureInitial,

    /// Get checkbox state (checked/unchecked) by index in preview pane (0-indexed)
    GetCheckboxState { index: u32 },

    /// Check if a button has a visible outline (outline CSS property is not "none")
    AssertButtonHasOutline { text: String },

    /// Assert the toggle all checkbox icon is dark (all todos completed)
    AssertToggleAllDarker,

    /// Navigate to a specific route/path (uses history.pushState + popstate event)
    NavigateTo { path: String },

    /// Take screenshot of preview pane at specified dimensions
    /// Temporarily forces size, captures, then resets to auto
    ScreenshotPreview {
        /// Preview width in CSS pixels (default: 700)
        #[serde(skip_serializing_if = "Option::is_none")]
        width: Option<u32>,
        /// Preview height in CSS pixels (default: 700)
        #[serde(skip_serializing_if = "Option::is_none")]
        height: Option<u32>,
        /// If true, output at native device resolution; false = CSS pixel resolution (default)
        #[serde(skip_serializing_if = "Option::is_none")]
        hidpi: Option<bool>,
    },
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

    /// Screenshot data (base64, from extension)
    Screenshot {
        base64: String,
        /// Output image width in pixels
        #[serde(skip_serializing_if = "Option::is_none")]
        width: Option<u32>,
        /// Output image height in pixels
        #[serde(skip_serializing_if = "Option::is_none")]
        height: Option<u32>,
        /// Device pixel ratio (informational)
        #[serde(skip_serializing_if = "Option::is_none")]
        dpr: Option<f64>,
    },

    /// Screenshot saved to file (filepath, transformed by WS server)
    ScreenshotFile { filepath: String },

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

    /// Editor code content
    EditorCode { code: String },

    /// Accessibility tree
    AccessibilityTree { tree: serde_json::Value },

    /// LocalStorage entries
    LocalStorage { entries: serde_json::Value },

    /// Focused element information
    FocusedElement {
        /// Tag name (e.g., "INPUT", "BUTTON", null if no element focused)
        #[serde(skip_serializing_if = "Option::is_none")]
        tag_name: Option<String>,
        /// Input type if it's an input (e.g., "text", "checkbox")
        #[serde(skip_serializing_if = "Option::is_none")]
        input_type: Option<String>,
        /// Index among inputs in preview pane (if applicable)
        #[serde(skip_serializing_if = "Option::is_none")]
        input_index: Option<u32>,
    },

    /// Input element properties
    InputProperties {
        /// Whether the input was found
        found: bool,
        /// Placeholder text
        #[serde(skip_serializing_if = "Option::is_none")]
        placeholder: Option<String>,
        /// Current value
        #[serde(skip_serializing_if = "Option::is_none")]
        value: Option<String>,
        /// Input type (text, checkbox, etc.)
        #[serde(skip_serializing_if = "Option::is_none")]
        input_type: Option<String>,
    },

    /// Current page URL
    CurrentUrl { url: String },

    /// Input typeable verification result
    InputTypeableStatus {
        typeable: bool,
        disabled: bool,
        readonly: bool,
        hidden: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },

    /// Run and capture initial preview result
    RunAndCaptureInitial {
        /// Whether the command succeeded
        success: bool,
        /// Initial preview text captured immediately after run (before any timers fire)
        #[serde(rename = "initialPreview")]
        initial_preview: String,
        /// Timestamp when capture was made
        timestamp: u64,
    },

    /// Checkbox state response
    CheckboxState {
        /// Whether the checkbox was found
        found: bool,
        /// Whether the checkbox is checked
        checked: bool,
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
