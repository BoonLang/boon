//! Auto-detection of playground and WebSocket ports from MoonZoon.toml
//!
//! Convention: ws_port = playground_port + WS_PORT_OFFSET (1141)

use std::path::{Path, PathBuf};

/// Port offset: WS port = playground port + this value
pub const WS_PORT_OFFSET: u16 = 1141;

pub const DEFAULT_PLAYGROUND_PORT: u16 = 8083;
pub const DEFAULT_WS_PORT: u16 = DEFAULT_PLAYGROUND_PORT + WS_PORT_OFFSET; // 9224

pub struct PortConfig {
    pub playground_port: u16,
    pub ws_port: u16,
    pub source: PortSource,
}

pub enum PortSource {
    /// Read from MoonZoon.toml
    MoonZoonToml(PathBuf),
    /// Default values (no config found)
    Default,
}

/// Find the boon repo root by searching upward from a starting directory.
/// Looks for `playground/MoonZoon.toml` as the marker.
fn find_moonzoon_toml(start: &Path) -> Option<PathBuf> {
    let mut current = start.to_path_buf();
    loop {
        let candidate = current.join("playground").join("MoonZoon.toml");
        if candidate.exists() {
            return Some(candidate);
        }
        if !current.pop() {
            return None;
        }
    }
}

/// Read playground port from MoonZoon.toml (top-level `port = XXXX`)
fn read_playground_port(moonzoon_toml: &Path) -> Option<u16> {
    let content = std::fs::read_to_string(moonzoon_toml).ok()?;
    let table: toml::Table = content.parse().ok()?;
    table
        .get("port")
        .and_then(|v| v.as_integer())
        .and_then(|v| u16::try_from(v).ok())
}

/// Auto-detect port configuration.
///
/// Search order:
/// 1. Current working directory upward for playground/MoonZoon.toml
/// 2. Relative to the binary path (target/release -> repo root)
/// 3. Fall back to defaults
pub fn detect_ports() -> PortConfig {
    // Try from CWD
    if let Ok(cwd) = std::env::current_dir() {
        if let Some(toml_path) = find_moonzoon_toml(&cwd) {
            if let Some(port) = read_playground_port(&toml_path) {
                return PortConfig {
                    playground_port: port,
                    ws_port: port + WS_PORT_OFFSET,
                    source: PortSource::MoonZoonToml(toml_path),
                };
            }
        }
    }

    // Try from binary location (target/release/boon-tools -> ../../playground/)
    if let Ok(exe) = std::env::current_exe() {
        // Walk up from exe path
        if let Some(toml_path) = exe.parent().and_then(find_moonzoon_toml) {
            if let Some(port) = read_playground_port(&toml_path) {
                return PortConfig {
                    playground_port: port,
                    ws_port: port + WS_PORT_OFFSET,
                    source: PortSource::MoonZoonToml(toml_path),
                };
            }
        }
    }

    // Default
    PortConfig {
        playground_port: DEFAULT_PLAYGROUND_PORT,
        ws_port: DEFAULT_WS_PORT,
        source: PortSource::Default,
    }
}
