//! Inject code into the CodeMirror editor

use anyhow::{Context, Result};
use crate::cdp::BrowserSession;
use std::fs;

pub fn run(url: &str, content: &str) -> Result<()> {
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

        // Inject code into CodeMirror
        println!("Injecting code ({} chars)...", code.len());

        // Escape the code for JavaScript
        let escaped_code = code
            .replace('\\', "\\\\")
            .replace('`', "\\`")
            .replace("${", "\\${");

        let js = format!(
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

        let success: bool = BrowserSession::evaluate(&page, &js).await?;

        if success {
            println!("Code injected successfully!");
        } else {
            println!("Failed to inject code - editor not found");
            std::process::exit(1);
        }

        // Keep browser open briefly to see result
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

        Ok(())
    })
}
