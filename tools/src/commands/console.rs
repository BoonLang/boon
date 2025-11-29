//! Console monitoring command - captures browser console output

use anyhow::Result;
use crate::cdp::BrowserSession;

pub fn run(url: &str, wait_secs: u64, errors_only: bool) -> Result<()> {
    tokio::runtime::Runtime::new()?.block_on(async {
        println!("Launching browser...");
        let session = BrowserSession::launch(1280, 800).await?;

        println!("Navigating to: {}", url);
        let page = session.navigate(url).await?;

        // Wait for page to load
        tokio::time::sleep(tokio::time::Duration::from_millis(1000)).await;

        println!("Collecting console messages for {} seconds...", wait_secs);
        let mut report = BrowserSession::collect_console(&page, wait_secs).await?;
        report.url = url.to_string();

        if errors_only {
            report.print_summary();
        } else {
            report.print_all();
        }

        if report.has_errors() {
            std::process::exit(1);
        }

        Ok(())
    })
}
