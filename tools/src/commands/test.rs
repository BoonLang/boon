//! Test command - inject code, run, and check output in one session

use anyhow::{Context, Result};
use crate::cdp::BrowserSession;
use std::fs;

pub fn run(url: &str, content: &str, wait_secs: u64, screenshot_path: Option<&str>) -> Result<()> {
    // If content starts with @, read from file
    let code = if let Some(filename) = content.strip_prefix('@') {
        fs::read_to_string(filename)
            .context(format!("Failed to read file: {}", filename))?
    } else {
        content.to_string()
    };

    tokio::runtime::Runtime::new()?.block_on(async {
        println!("Launching browser...");
        let session = BrowserSession::launch(1280, 800).await?;

        println!("Navigating to: {}", url);
        let page = session.navigate(url).await?;

        // Wait for CodeMirror to initialize
        println!("Waiting for editor to initialize...");
        tokio::time::sleep(tokio::time::Duration::from_millis(2000)).await;

        // Enable console monitoring before we inject and run
        page.enable_runtime().await?;
        page.enable_log().await?;

        // Inject code into CodeMirror
        println!("Injecting code ({} chars)...", code.len());

        // Escape the code for JavaScript
        let escaped_code = code
            .replace('\\', "\\\\")
            .replace('`', "\\`")
            .replace("${", "\\${");

        let inject_js = format!(
            r#"(() => {{
                const cm = document.querySelector('.cm-content');
                if (!cm) {{
                    console.error('CodeMirror .cm-content not found');
                    return false;
                }}
                const view = cm.cmView?.view;
                if (!view) {{
                    console.error('CodeMirror view not found');
                    return false;
                }}
                view.dispatch({{
                    changes: {{ from: 0, to: view.state.doc.length, insert: `{}` }}
                }});
                return true;
            }})()"#,
            escaped_code
        );

        let success: bool = BrowserSession::evaluate(&page, &inject_js).await?;

        if !success {
            println!("Failed to inject code - editor not found");
            std::process::exit(1);
        }
        println!("Code injected!");

        // Small delay for UI to update
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

        // Check if listener is registered (marker set by WASM)
        println!("Checking if boon-run listener is registered...");
        let check_js = r#"(() => {
            return {
                listenerAdded: window._boonRunListenerAdded === true,
                eventReceivedBefore: window._boonRunEventReceived === true
            };
        })()"#;
        let check_result: serde_json::Value = BrowserSession::evaluate(&page, check_js).await?;
        println!("Before dispatch - listenerAdded: {}, eventReceived: {}",
            check_result.get("listenerAdded").and_then(|v| v.as_bool()).unwrap_or(false),
            check_result.get("eventReceivedBefore").and_then(|v| v.as_bool()).unwrap_or(false)
        );

        // Dispatch the boon-run custom event
        println!("Dispatching 'boon-run' custom event...");
        let event_js = r#"(() => {
            const event = new Event('boon-run', { bubbles: true, cancelable: true });
            window.dispatchEvent(event);
            return 'dispatched';
        })()"#;
        let _: String = BrowserSession::evaluate(&page, event_js).await?;

        // Wait a moment for the event to be processed
        tokio::time::sleep(tokio::time::Duration::from_millis(1000)).await;

        // Check if event was received
        let after_js = r#"(() => {
            return {
                eventReceived: window._boonRunEventReceived === true
            };
        })()"#;
        let after_result: serde_json::Value = BrowserSession::evaluate(&page, after_js).await?;
        println!("After dispatch - eventReceived: {}",
            after_result.get("eventReceived").and_then(|v| v.as_bool()).unwrap_or(false)
        );

        // Wait for execution
        println!("Waiting {} seconds for execution...", wait_secs);
        tokio::time::sleep(tokio::time::Duration::from_secs(wait_secs)).await;

        // Take screenshot if requested
        if let Some(path) = screenshot_path {
            println!("Taking screenshot...");
            let data = BrowserSession::screenshot(&page).await?;
            fs::write(path, &data)
                .context(format!("Failed to write screenshot to {}", path))?;
            println!("Screenshot saved: {} ({} bytes)", path, data.len());
        }

        // Get preview content
        println!("\nChecking preview content...");
        let preview_js = r#"(() => {
            // Try to find the preview/output panel content
            // The preview is in the right panel, rendered by Boon
            const panels = document.querySelectorAll('[class*="panel"]');
            let previewText = '';

            // Look for text content in preview area
            const body = document.body;
            const walker = document.createTreeWalker(body, NodeFilter.SHOW_TEXT, null, false);
            const texts = [];
            let node;
            while (node = walker.nextNode()) {
                const text = node.textContent.trim();
                if (text && text.length > 0 && text.length < 100) {
                    texts.push(text);
                }
            }

            return {
                allText: texts.join(' | '),
                bodyHTML: document.body.innerHTML.substring(0, 2000)
            };
        })()"#;

        let result: serde_json::Value = BrowserSession::evaluate(&page, preview_js).await?;
        println!("Page text content: {}", result.get("allText").and_then(|v| v.as_str()).unwrap_or("(none)"));

        // Collect console output
        println!("\nCollecting console output...");
        let mut report = BrowserSession::collect_console(&page, 1).await?;
        report.url = url.to_string();
        report.print_all();

        // Check for success criteria
        let has_errors = report.has_errors();

        // Check if "123" appears in the preview
        let page_text = result.get("allText").and_then(|v| v.as_str()).unwrap_or("");
        let has_123 = page_text.contains("123");

        println!("\n--- Test Results ---");
        println!("Console errors: {}", if has_errors { "YES" } else { "NO" });
        println!("Preview contains '123': {}", if has_123 { "YES" } else { "NO" });

        if has_errors {
            println!("\nTest FAILED: Console errors detected");
            std::process::exit(1);
        }

        if !has_123 {
            println!("\nTest FAILED: '123' not found in preview");
            std::process::exit(1);
        }

        println!("\nTest PASSED!");

        Ok(())
    })
}
