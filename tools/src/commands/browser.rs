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

/// Return PIDs of Chromium processes using Boon's persistent automation profile.
#[cfg(target_os = "linux")]
pub fn running_automation_pids() -> Vec<u32> {
    let output = Command::new("pgrep")
        .args(["-f", "tools/.chrome-profile"])
        .output();

    match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .lines()
            .filter_map(|line| line.trim().parse::<u32>().ok())
            .collect(),
        _ => Vec::new(),
    }
}

/// Return PIDs of Chromium processes using Boon's persistent automation profile.
#[cfg(not(target_os = "linux"))]
pub fn running_automation_pids() -> Vec<u32> {
    Vec::new()
}

/// True when a Boon automation browser instance is already running.
pub fn has_running_automation_browser() -> bool {
    !running_automation_pids().is_empty()
}

/// Options for launching the browser
#[allow(dead_code)]
pub struct LaunchOptions {
    pub playground_port: u16,
    pub ws_port: u16,
    pub headless: bool,
    pub keep_open: bool,
    pub browser_path: Option<PathBuf>,
    pub initial_engine: Option<String>,
    pub initial_example: Option<String>,
}

impl Default for LaunchOptions {
    fn default() -> Self {
        let ports = crate::port_config::detect_ports();
        Self {
            playground_port: ports.playground_port,
            ws_port: ports.ws_port,
            headless: false,
            keep_open: false,
            browser_path: None,
            initial_engine: None,
            initial_example: None,
        }
    }
}

fn engine_query_value(engine: &str) -> &str {
    match engine {
        "Actors" => "actors",
        "ActorsLite" => "actorslite",
        "FactoryFabric" => "factoryfabric",
        "DD" => "dd",
        "Wasm" => "wasm",
        other => other,
    }
}

/// Launch Chromium with the Boon extension pre-loaded
pub fn launch_browser(opts: LaunchOptions) -> Result<Child> {
    if has_running_automation_browser() {
        let pids = running_automation_pids();
        return Err(anyhow!(
            "Boon automation Chromium already running (PID(s): {:?}). Reuse existing session instead of launching a new instance.",
            pids
        ));
    }

    let extension_path = find_extension_path()?;
    let user_data_dir = create_persistent_profile()?;
    let browser = opts
        .browser_path
        .map(Ok)
        .unwrap_or_else(find_chromium_binary)?;

    println!("Launching Chromium from: {}", browser.display());
    println!("Extension path: {}", extension_path.display());
    println!("User data dir: {}", user_data_dir.display());

    let mut browser_args: Vec<String> = vec![
        format!("--load-extension={}", extension_path.display()),
        format!("--user-data-dir={}", user_data_dir.display()),
        "--no-first-run".to_string(),
        "--no-default-browser-check".to_string(),
        "--disable-default-apps".to_string(),
        "--disable-popup-blocking".to_string(),
        "--disable-translate".to_string(),
        "--disable-sync".to_string(),
        "--disable-session-crashed-bubble".to_string(),
        "--hide-crash-restore-bubble".to_string(),
        "--disable-background-timer-throttling".to_string(),
        "--disable-backgrounding-occluded-windows".to_string(),
        "--disable-renderer-backgrounding".to_string(),
    ];

    if !opts.headless && cfg!(target_os = "linux") && std::env::var_os("DISPLAY").is_some() {
        // The desktop Chromium sessions on this machine are stable on X11, while
        // detached launches can disappear immediately under the default auto mode.
        browser_args.push("--ozone-platform=x11".to_string());
    }

    if opts.headless {
        // Use new headless mode that supports extensions
        browser_args.push("--headless=new".to_string());
    }

    // Open the playground URL with safe defaults to avoid loading a heavy example
    // from a previous session. Allow callers to override the initial engine/example
    // when the automation flow wants to boot directly into a specific backend.
    let initial_engine = opts
        .initial_engine
        .as_deref()
        .map(engine_query_value)
        .unwrap_or("actors");
    let initial_example = opts.initial_example.as_deref().unwrap_or("counter");
    browser_args.push(format!(
        "http://localhost:{}/?engine={}&example={}",
        opts.playground_port, initial_engine, initial_example
    ));

    let mut cmd = if cfg!(target_os = "linux") && opts.keep_open && !opts.headless {
        // Detach the desktop browser from the launcher process group. Without this,
        // Chromium can be torn down as soon as the CLI command exits even though the
        // launch itself reported success.
        let mut detached = Command::new("setsid");
        detached.arg("-f");
        detached.arg(&browser);
        detached
    } else {
        Command::new(&browser)
    };
    cmd.args(&browser_args);

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
        match crate::ws_server::send_command_to_server(port, crate::ws_server::Command::GetStatus)
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
            .args(["-f", "tools/.chrome-profile"])
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
