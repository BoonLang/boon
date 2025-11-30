//! Chrome DevTools Protocol utilities for browser automation.
//! Adapted from raybox/tools/src/cdp/mod.rs

use anyhow::{Context, Result};
use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::emulation::SetDeviceMetricsOverrideParams;
use chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotParams;
use chromiumoxide::cdp::js_protocol::runtime::{
    EventConsoleApiCalled, EventExceptionThrown, RemoteObject,
};
use chromiumoxide::Page;
use futures::StreamExt;
use std::time::Duration;
use tokio::time::timeout;

/// Console message from the browser
#[derive(Debug, Clone)]
pub struct ConsoleMessage {
    pub level: String,
    pub text: String,
    #[allow(dead_code)]
    pub timestamp: std::time::SystemTime,
}

/// Exception thrown in the browser
#[derive(Debug, Clone)]
pub struct BrowserException {
    pub message: String,
    pub stack_trace: Option<String>,
}

/// Report of a page check
#[derive(Debug)]
pub struct PageReport {
    pub url: String,
    pub messages: Vec<ConsoleMessage>,
    pub exceptions: Vec<BrowserException>,
}

impl PageReport {
    /// Get only error messages
    pub fn errors(&self) -> Vec<&ConsoleMessage> {
        self.messages
            .iter()
            .filter(|m| m.level.contains("error") || m.level.contains("Error"))
            .collect()
    }

    /// Check if there are any errors or exceptions
    pub fn has_errors(&self) -> bool {
        !self.errors().is_empty() || !self.exceptions.is_empty()
    }

    /// Print a summary report
    pub fn print_summary(&self) {
        println!("\n Browser Console Report");
        println!("   URL: {}", self.url);
        println!("   Messages: {} total", self.messages.len());
        println!("   Errors: {}", self.errors().len());
        println!("   Exceptions: {}", self.exceptions.len());

        if !self.errors().is_empty() {
            println!("\n Console Errors:");
            for msg in self.errors() {
                println!("   [{}] {}", msg.level, msg.text);
            }
        }

        if !self.exceptions.is_empty() {
            println!("\n Exceptions:");
            for exc in &self.exceptions {
                println!("   {}", exc.message);
                if let Some(stack) = &exc.stack_trace {
                    println!("   Stack: {}", stack);
                }
            }
        }

        if !self.has_errors() {
            println!("   No errors detected!");
        }
    }

    /// Print all messages
    pub fn print_all(&self) {
        println!("\n Browser Console ({} messages)", self.messages.len());
        for msg in &self.messages {
            println!("   [{}] {}", msg.level, msg.text);
        }

        if !self.exceptions.is_empty() {
            println!("\n Exceptions:");
            for exc in &self.exceptions {
                println!("   {}", exc.message);
            }
        }
    }
}

/// Browser session for automation
pub struct BrowserSession {
    pub browser: Browser,
}

impl BrowserSession {
    /// Launch a new headed Chrome instance
    pub async fn launch(width: u32, height: u32) -> Result<Self> {
        let cfg = BrowserConfig::builder()
            .with_head() // Headed mode for debugging
            .window_size(width, height)
            .args(vec![
                "--disable-dev-shm-usage",
                "--no-sandbox",
                "--hide-scrollbars",
                "--disable-session-crashed-bubble",
                "--hide-crash-restore-bubble",
                "--disable-application-cache",
                "--disable-cache",
                "--disk-cache-size=0",
                "--aggressive-cache-discard",
                "--incognito",
            ])
            .build()
            .map_err(|e| anyhow::anyhow!("Failed to build browser config: {}", e))?;

        let (browser, mut handler) = Browser::launch(cfg)
            .await
            .context("Failed to launch Chrome")?;

        // Spawn handler task to process Chrome events
        tokio::spawn(async move {
            while handler.next().await.is_some() {
                // Handler events are processed by chromiumoxide internally
            }
        });

        Ok(Self { browser })
    }

    /// Navigate to URL and return page handle
    pub async fn navigate(&self, url: &str) -> Result<Page> {
        let page = self
            .browser
            .new_page(url)
            .await
            .context("Failed to create new page")?;

        Ok(page)
    }

    /// Set viewport dimensions
    pub async fn set_viewport(page: &Page, width: u32, height: u32) -> Result<()> {
        page.execute(SetDeviceMetricsOverrideParams::new(
            width as i64,
            height as i64,
            1.0,   // device_scale_factor
            false, // mobile
        ))
        .await
        .context("Failed to set viewport dimensions")?;
        Ok(())
    }

    /// Take a screenshot
    pub async fn screenshot(page: &Page) -> Result<Vec<u8>> {
        let data = page
            .screenshot(CaptureScreenshotParams::default())
            .await
            .context("Failed to capture screenshot")?;
        Ok(data)
    }

    /// Collect console messages for a duration
    pub async fn collect_console(page: &Page, wait_secs: u64) -> Result<PageReport> {
        // Enable Runtime domain to receive console events
        page.enable_runtime().await?;
        page.enable_log().await?;

        // Listen for console messages
        let mut console_rx = page.event_listener::<EventConsoleApiCalled>().await?;

        // Listen for exceptions
        let mut exception_rx = page.event_listener::<EventExceptionThrown>().await?;

        let mut messages = Vec::new();
        let mut exceptions = Vec::new();

        // Wait for page to initialize and collect messages
        let wait_duration = Duration::from_secs(wait_secs);
        let _ = timeout(wait_duration, async {
            loop {
                tokio::select! {
                    Some(event) = console_rx.next() => {
                        let level = format!("{:?}", event.r#type);

                        // Extract all arguments and join them
                        let text = if event.args.is_empty() {
                            "<empty>".to_string()
                        } else {
                            event.args
                                .iter()
                                .filter_map(|arg| extract_text(arg))
                                .collect::<Vec<_>>()
                                .join(" ")
                        };

                        messages.push(ConsoleMessage {
                            level,
                            text,
                            timestamp: std::time::SystemTime::now(),
                        });
                    }
                    Some(event) = exception_rx.next() => {
                        let message = event.exception_details.text.clone();
                        let stack_trace = event.exception_details.stack_trace
                            .as_ref()
                            .map(|st| format!("{:?}", st));

                        exceptions.push(BrowserException {
                            message,
                            stack_trace,
                        });
                    }
                    else => break,
                }
            }
        })
        .await;

        Ok(PageReport {
            url: String::new(), // Will be set by caller
            messages,
            exceptions,
        })
    }

    /// Evaluate JavaScript and return result
    pub async fn evaluate<T: serde::de::DeserializeOwned>(page: &Page, js: &str) -> Result<T> {
        let result = page
            .evaluate(js)
            .await
            .context("Failed to evaluate JavaScript")?;
        result
            .into_value()
            .map_err(|e| anyhow::anyhow!("Failed to deserialize JS result: {:?}", e))
    }

    /// Evaluate JavaScript without expecting a return value
    pub async fn evaluate_void(page: &Page, js: &str) -> Result<()> {
        page.evaluate(js)
            .await
            .context("Failed to evaluate JavaScript")?;
        Ok(())
    }

    /// Click at specific coordinates using CDP Input domain
    pub async fn click_at(page: &Page, x: f64, y: f64) -> Result<()> {
        use chromiumoxide::cdp::browser_protocol::input::{
            DispatchMouseEventParams, DispatchMouseEventType, MouseButton,
        };

        // Mouse move to element first
        page.execute(
            DispatchMouseEventParams::builder()
                .r#type(DispatchMouseEventType::MouseMoved)
                .x(x)
                .y(y)
                .build()
                .unwrap(),
        )
        .await
        .context("Failed to dispatch mousemove")?;

        // Small delay
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        // Mouse down
        page.execute(
            DispatchMouseEventParams::builder()
                .r#type(DispatchMouseEventType::MousePressed)
                .x(x)
                .y(y)
                .button(MouseButton::Left)
                .click_count(1)
                .build()
                .unwrap(),
        )
        .await
        .context("Failed to dispatch mousedown")?;

        // Small delay
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        // Mouse up
        page.execute(
            DispatchMouseEventParams::builder()
                .r#type(DispatchMouseEventType::MouseReleased)
                .x(x)
                .y(y)
                .button(MouseButton::Left)
                .click_count(1)
                .build()
                .unwrap(),
        )
        .await
        .context("Failed to dispatch mouseup")?;

        Ok(())
    }

    /// Send Shift+Enter keyboard shortcut using CDP Input domain
    pub async fn send_shift_enter(page: &Page) -> Result<()> {
        use chromiumoxide::cdp::browser_protocol::input::{
            DispatchKeyEventParams, DispatchKeyEventType,
        };

        // Key down for Shift
        page.execute(
            DispatchKeyEventParams::builder()
                .r#type(DispatchKeyEventType::KeyDown)
                .key("Shift")
                .code("ShiftLeft")
                .modifiers(8) // Shift modifier
                .build()
                .unwrap(),
        )
        .await
        .context("Failed to dispatch Shift keydown")?;

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        // Key down for Enter (with Shift held)
        page.execute(
            DispatchKeyEventParams::builder()
                .r#type(DispatchKeyEventType::KeyDown)
                .key("Enter")
                .code("Enter")
                .modifiers(8) // Shift modifier
                .build()
                .unwrap(),
        )
        .await
        .context("Failed to dispatch Enter keydown")?;

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        // Key up for Enter
        page.execute(
            DispatchKeyEventParams::builder()
                .r#type(DispatchKeyEventType::KeyUp)
                .key("Enter")
                .code("Enter")
                .modifiers(8) // Shift modifier
                .build()
                .unwrap(),
        )
        .await
        .context("Failed to dispatch Enter keyup")?;

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        // Key up for Shift
        page.execute(
            DispatchKeyEventParams::builder()
                .r#type(DispatchKeyEventType::KeyUp)
                .key("Shift")
                .code("ShiftLeft")
                .modifiers(0) // No modifiers
                .build()
                .unwrap(),
        )
        .await
        .context("Failed to dispatch Shift keyup")?;

        Ok(())
    }
}

// Helper to extract text from RemoteObject
fn extract_text(obj: &RemoteObject) -> Option<String> {
    // Try to extract value as different types
    if let Some(value) = &obj.value {
        // String values
        if let Some(s) = value.as_str() {
            return Some(s.to_string());
        }
        // Number values
        if let Some(n) = value.as_f64() {
            return Some(n.to_string());
        }
        // Boolean values
        if let Some(b) = value.as_bool() {
            return Some(b.to_string());
        }
        // Null
        if value.is_null() {
            return Some("null".to_string());
        }
        // Objects/Arrays - try to serialize
        if let Ok(serialized) = serde_json::to_string(value) {
            return Some(serialized);
        }
    }

    // Fall back to description (for Error objects, DOM elements, etc.)
    obj.description.clone()
}
