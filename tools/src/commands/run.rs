//! Run command - triggers Shift+Enter to execute code

use anyhow::Result;
use crate::cdp::BrowserSession;

pub fn run(url: &str, wait_secs: u64) -> Result<()> {
    tokio::runtime::Runtime::new()?.block_on(async {
        println!("Launching browser...");
        let session = BrowserSession::launch(1280, 800).await?;

        println!("Navigating to: {}", url);
        let page = session.navigate(url).await?;

        // Wait for page to load
        println!("Waiting for playground to initialize...");
        tokio::time::sleep(tokio::time::Duration::from_millis(2000)).await;

        // Trigger Shift+Enter
        println!("Triggering Run (Shift+Enter)...");
        let js = r#"(() => {
            const event = new KeyboardEvent('keydown', {
                key: 'Enter',
                code: 'Enter',
                shiftKey: true,
                bubbles: true,
                cancelable: true
            });
            document.dispatchEvent(event);
            return true;
        })()"#;

        let _: bool = BrowserSession::evaluate(&page, js).await?;
        println!("Run triggered!");

        // Wait for execution
        if wait_secs > 0 {
            println!("Waiting {} seconds for execution...", wait_secs);
            tokio::time::sleep(tokio::time::Duration::from_secs(wait_secs)).await;
        }

        // Collect any console output
        println!("Collecting console output...");
        let mut report = BrowserSession::collect_console(&page, 1).await?;
        report.url = url.to_string();

        if !report.messages.is_empty() || !report.exceptions.is_empty() {
            report.print_all();
        }

        Ok(())
    })
}
