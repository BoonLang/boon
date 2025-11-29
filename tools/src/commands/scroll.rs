//! Scroll command - scrolls the preview panel

use anyhow::Result;
use crate::cdp::BrowserSession;

pub fn run(url: &str, y: Option<i32>, delta: Option<i32>, to_bottom: bool) -> Result<()> {
    tokio::runtime::Runtime::new()?.block_on(async {
        println!("Launching browser...");
        let session = BrowserSession::launch(1280, 800).await?;

        println!("Navigating to: {}", url);
        let page = session.navigate(url).await?;

        // Wait for page to load
        println!("Waiting for playground to initialize...");
        tokio::time::sleep(tokio::time::Duration::from_millis(2000)).await;

        // Find the preview panel and scroll it
        let js = if to_bottom {
            r#"(() => {
                // Find scrollable preview panel - try different selectors
                const selectors = [
                    '[class*="preview"]',
                    '[class*="example-panel"]',
                    '.cm-scroller',
                    'main',
                    'body'
                ];
                for (const sel of selectors) {
                    const el = document.querySelector(sel);
                    if (el && el.scrollHeight > el.clientHeight) {
                        el.scrollTop = el.scrollHeight;
                        return `Scrolled ${sel} to bottom (${el.scrollHeight}px)`;
                    }
                }
                // Fallback to window
                window.scrollTo(0, document.body.scrollHeight);
                return 'Scrolled window to bottom';
            })()"#.to_string()
        } else if let Some(y_pos) = y {
            format!(
                r#"(() => {{
                    const selectors = [
                        '[class*="preview"]',
                        '[class*="example-panel"]',
                        'main',
                        'body'
                    ];
                    for (const sel of selectors) {{
                        const el = document.querySelector(sel);
                        if (el && el.scrollHeight > el.clientHeight) {{
                            el.scrollTop = {};
                            return `Scrolled ${{sel}} to y={}`;
                        }}
                    }}
                    window.scrollTo(0, {});
                    return 'Scrolled window to y={}';
                }})()"#,
                y_pos, y_pos, y_pos, y_pos
            )
        } else if let Some(dy) = delta {
            format!(
                r#"(() => {{
                    const selectors = [
                        '[class*="preview"]',
                        '[class*="example-panel"]',
                        'main',
                        'body'
                    ];
                    for (const sel of selectors) {{
                        const el = document.querySelector(sel);
                        if (el && el.scrollHeight > el.clientHeight) {{
                            el.scrollBy(0, {});
                            return `Scrolled ${{sel}} by {}px`;
                        }}
                    }}
                    window.scrollBy(0, {});
                    return 'Scrolled window by {}px';
                }})()"#,
                dy, dy, dy, dy
            )
        } else {
            println!("No scroll action specified. Use --y, --delta, or --to-bottom");
            return Ok(());
        };

        let result: String = BrowserSession::evaluate(&page, &js).await?;
        println!("{}", result);

        // Brief pause to see result
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

        Ok(())
    })
}
