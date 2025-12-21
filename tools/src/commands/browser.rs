//! Browser launching and management for automated testing
//!
//! Uses Chromium (not Chrome) because:
//! - `--load-extension` flag is deprecated in Chrome 137+ branded builds
//! - Chromium keeps all developer flags permanently (open-source project)
//! - Available via `apt install chromium-browser`

use anyhow::{anyhow, Result};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// Find the extension directory relative to the boon-tools binary
fn find_extension_path() -> Result<PathBuf> {
    // Try relative to current executable
    if let Ok(exe_path) = std::env::current_exe() {
        // Binary is in target/release or target/debug
        // Extension is in tools/extension
        if let Some(parent) = exe_path.parent() {
            // Check if we're in target/release or target/debug
            if parent.ends_with("release") || parent.ends_with("debug") {
                // Go up to repo root: target/release -> target -> repo
                if let Some(target_dir) = parent.parent() {
                    if let Some(repo_root) = target_dir.parent() {
                        let ext_path = repo_root.join("tools").join("extension");
                        if ext_path.exists() {
                            return Ok(ext_path);
                        }
                    }
                }
            }
        }
    }

    // Try relative to current directory
    let candidates = [
        PathBuf::from("tools/extension"),
        PathBuf::from("extension"),
        PathBuf::from("../tools/extension"),
    ];

    for path in &candidates {
        if path.exists() {
            return Ok(path.canonicalize()?);
        }
    }

    Err(anyhow!(
        "Extension directory not found. Run from boon repo root or tools directory."
    ))
}

/// Find Chromium binary in PATH
///
/// Only searches for Chromium (not Chrome) because:
/// - Chrome 137+ deprecated --load-extension flag
/// - Chromium keeps all developer flags
fn find_chromium_binary() -> Result<PathBuf> {
    let candidates = [
        "chromium-browser", // Debian/Ubuntu
        "chromium",         // Arch/Fedora
    ];

    for name in candidates {
        if let Ok(path) = which::which(name) {
            log::info!("Found Chromium at: {}", path.display());
            return Ok(path);
        }
    }

    Err(anyhow!(
        "Chromium not found in PATH.\n\
        Install with: apt install chromium-browser (Debian/Ubuntu)\n\
        or: pacman -S chromium (Arch)\n\
        or: dnf install chromium (Fedora)\n\n\
        Note: Chrome is not supported because --load-extension was deprecated in Chrome 137+"
    ))
}

/// Create a user data directory for browser profile
/// Uses tools/.chrome-profile/ for persistence (gitignored)
fn create_persistent_profile() -> Result<PathBuf> {
    // Binary is at target/release/boon-tools, repo root is 3 levels up
    let exe = std::env::current_exe()?;
    let repo_root = exe.parent().unwrap().parent().unwrap().parent().unwrap();
    let profile_dir = repo_root.join("tools").join(".chrome-profile");
    std::fs::create_dir_all(&profile_dir)?;
    log::info!("Using profile at: {}", profile_dir.display());
    Ok(profile_dir)
}

/// Options for launching the browser
pub struct LaunchOptions {
    pub playground_port: u16,
    pub ws_port: u16,
    pub headless: bool,
    pub keep_open: bool,
    pub browser_path: Option<PathBuf>,
}

impl Default for LaunchOptions {
    fn default() -> Self {
        Self {
            playground_port: 8081,
            ws_port: 9222,
            headless: false,
            keep_open: false,
            browser_path: None,
        }
    }
}

/// Launch Chromium with the Boon extension pre-loaded
pub fn launch_browser(opts: LaunchOptions) -> Result<Child> {
    let extension_path = find_extension_path()?;
    let user_data_dir = create_persistent_profile()?;
    let browser = opts
        .browser_path
        .map(Ok)
        .unwrap_or_else(find_chromium_binary)?;

    println!("Launching Chromium from: {}", browser.display());
    println!("Extension path: {}", extension_path.display());
    println!("User data dir: {}", user_data_dir.display());

    let mut cmd = Command::new(&browser);

    // Core flags for automation
    cmd.args([
        &format!("--load-extension={}", extension_path.display()),
        &format!("--user-data-dir={}", user_data_dir.display()),
        "--no-first-run",
        "--no-default-browser-check",
        "--disable-default-apps",
        "--disable-popup-blocking",
        "--disable-translate",
        "--disable-sync",
        // Disable background throttling so extension stays responsive
        "--disable-background-timer-throttling",
        "--disable-backgrounding-occluded-windows",
        "--disable-renderer-backgrounding",
    ]);

    if opts.headless {
        // Use new headless mode that supports extensions
        cmd.arg("--headless=new");
    }

    // Open the playground URL
    cmd.arg(&format!("http://localhost:{}", opts.playground_port));

    // Suppress browser output unless in debug mode
    if std::env::var("RUST_LOG").is_err() {
        cmd.stdout(Stdio::null());
        cmd.stderr(Stdio::null());
    }

    let child = cmd.spawn().map_err(|e| {
        anyhow!(
            "Failed to launch Chromium: {}.\n\
            Binary: {}\n\
            Is Chromium installed?",
            e,
            browser.display()
        )
    })?;

    println!("Chromium launched with PID: {}", child.id());
    println!("Waiting for extension to connect...");

    Ok(child)
}

/// Wait for the extension to connect to the WebSocket server
pub async fn wait_for_extension_connection(port: u16, timeout: Duration) -> Result<()> {
    use tokio::time::{sleep, Instant};

    let start = Instant::now();
    let check_interval = Duration::from_millis(500);

    while start.elapsed() < timeout {
        // Try to get status from the server
        match crate::ws_server::send_command_to_server(
            port,
            crate::ws_server::Command::GetStatus,
        )
        .await
        {
            Ok(crate::ws_server::Response::Status { connected, .. }) => {
                if connected {
                    println!("Extension connected!");
                    return Ok(());
                }
            }
            _ => {}
        }

        sleep(check_interval).await;
    }

    Err(anyhow!(
        "Extension did not connect within {:?}.\n\
        Check that:\n\
        1. Chromium launched successfully\n\
        2. Extension loaded without errors (check chrome://extensions)\n\
        3. WebSocket server is running on port {}",
        timeout,
        port
    ))
}

/// Kill all boon-related Chromium processes
pub fn kill_browser_instances() -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        // Find and kill Chromium processes with our profile
        // Note: we don't delete the profile directory to preserve developer mode settings
        let output = Command::new("pkill")
            .args(["-f", "boon-chromium"])
            .output();

        match output {
            Ok(o) if o.status.success() => {
                println!("Killed Chromium automation instances");
            }
            Ok(_) => {
                println!("No Chromium automation instances found");
            }
            Err(e) => {
                log::warn!("pkill failed: {}", e);
            }
        }
    }

    #[cfg(not(target_os = "linux"))]
    {
        println!("Browser kill not implemented for this platform");
    }

    Ok(())
}

/// Check if Chromium is available
pub fn check_chromium_available() -> Result<PathBuf> {
    find_chromium_binary()
}
