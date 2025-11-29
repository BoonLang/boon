//! Screenshot capture command

use anyhow::{Context, Result};
use crate::cdp::BrowserSession;
use std::fs;

pub fn run(url: &str, output: &str, width: u32, height: u32) -> Result<()> {
    tokio::runtime::Runtime::new()?.block_on(async {
        println!("Launching browser ({}x{})...", width, height);
        let session = BrowserSession::launch(width, height).await?;

        println!("Navigating to: {}", url);
        let page = session.navigate(url).await?;

        // Set exact viewport dimensions
        BrowserSession::set_viewport(&page, width, height).await?;

        // Wait for page to load and render
        println!("Waiting for page to render...");
        tokio::time::sleep(tokio::time::Duration::from_millis(2000)).await;

        // Capture screenshot
        println!("Capturing screenshot...");
        let data = BrowserSession::screenshot(&page).await?;

        // Write to file
        fs::write(output, &data)
            .context(format!("Failed to write screenshot to {}", output))?;

        println!("Screenshot saved: {} ({} bytes)", output, data.len());

        Ok(())
    })
}
