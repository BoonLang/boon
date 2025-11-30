mod cdp;
mod commands;
mod ws_server;

use anyhow::Result;
use clap::{Parser, Subcommand};
use ws_server::{Command as WsCommand, Response as WsResponse};

#[derive(Parser)]
#[command(name = "boon-tools")]
#[command(about = "Boon Playground Browser Automation Tools")]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Playground URL
    #[arg(long, global = true, default_value = "http://localhost:8081")]
    url: String,
}

#[derive(Subcommand)]
enum Commands {
    /// Capture screenshot of the playground
    Screenshot {
        /// Output PNG file path
        #[arg(short, long)]
        output: String,

        /// Viewport width
        #[arg(long, default_value = "1280")]
        width: u32,

        /// Viewport height
        #[arg(long, default_value = "800")]
        height: u32,
    },

    /// Monitor browser console output
    Console {
        /// How long to wait for messages (seconds)
        #[arg(short, long, default_value = "3")]
        wait: u64,

        /// Only show errors
        #[arg(long)]
        errors_only: bool,
    },

    /// Inject code into the editor
    Inject {
        /// Code to inject (use @filename to read from file)
        content: String,
    },

    /// Trigger code execution (Shift+Enter)
    Run {
        /// Wait time after triggering run (seconds)
        #[arg(short, long, default_value = "2")]
        wait: u64,
    },

    /// Scroll the preview panel
    Scroll {
        /// Scroll to absolute Y position
        #[arg(short, long)]
        y: Option<i32>,

        /// Scroll by relative amount
        #[arg(short, long)]
        delta: Option<i32>,

        /// Scroll to bottom
        #[arg(long)]
        to_bottom: bool,
    },

    /// Test: inject code, run, and check output
    Test {
        /// Code to inject (use @filename to read from file)
        content: String,

        /// Wait time after run (seconds)
        #[arg(short, long, default_value = "3")]
        wait: u64,

        /// Take screenshot
        #[arg(short, long)]
        screenshot: Option<String>,
    },

    /// Start WebSocket server for extension communication
    Server {
        #[command(subcommand)]
        action: ServerAction,
    },

    /// Execute command via WebSocket server (requires extension)
    Exec {
        #[command(subcommand)]
        action: ExecAction,

        /// Server port
        #[arg(short, long, default_value = "9222")]
        port: u16,
    },
}

#[derive(Subcommand)]
enum ServerAction {
    /// Start the WebSocket server
    Start {
        /// Port to listen on
        #[arg(short, long, default_value = "9222")]
        port: u16,

        /// Watch directory for extension hot reload
        #[arg(short, long)]
        watch: Option<String>,
    },
}

#[derive(Subcommand)]
enum ExecAction {
    /// Inject code into editor
    Inject {
        /// Code to inject
        code: String,
    },

    /// Trigger run
    Run,

    /// Take screenshot
    Screenshot {
        /// Output file path
        #[arg(short, long, default_value = "screenshot.png")]
        output: String,
    },

    /// Get preview text
    Preview,

    /// Click element by selector
    Click {
        /// CSS selector
        selector: String,
    },

    /// Type text into element
    Type {
        /// CSS selector
        selector: String,
        /// Text to type
        text: String,
    },

    /// Check connection status
    Status,

    /// Get console messages from browser
    Console,

    /// Reload the extension (hot reload)
    Reload,

    /// Full test: inject, run, check
    Test {
        /// Code to inject
        code: String,
        /// Expected text in preview
        #[arg(long)]
        expect: Option<String>,
        /// Screenshot output
        #[arg(short, long)]
        screenshot: Option<String>,
    },
}

fn main() -> Result<()> {
    // Configure logging - filter out harmless chromiumoxide deserialization warnings
    env_logger::Builder::from_default_env()
        .filter_module("chromiumoxide::conn", log::LevelFilter::Warn)
        .filter_module("chromiumoxide::handler", log::LevelFilter::Warn)
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Screenshot {
            output,
            width,
            height,
        } => {
            commands::screenshot::run(&cli.url, &output, width, height)?;
        }

        Commands::Console { wait, errors_only } => {
            commands::console::run(&cli.url, wait, errors_only)?;
        }

        Commands::Inject { content } => {
            commands::inject::run(&cli.url, &content)?;
        }

        Commands::Run { wait } => {
            commands::run::run(&cli.url, wait)?;
        }

        Commands::Scroll { y, delta, to_bottom } => {
            commands::scroll::run(&cli.url, y, delta, to_bottom)?;
        }

        Commands::Test { content, wait, screenshot } => {
            commands::test::run(&cli.url, &content, wait, screenshot.as_deref())?;
        }

        Commands::Server { action } => match action {
            ServerAction::Start { port, watch } => {
                let rt = tokio::runtime::Runtime::new()?;
                let watch_path = watch.as_ref().map(std::path::Path::new);
                rt.block_on(ws_server::start_server(port, watch_path))?;
            }
        },

        Commands::Exec { action, port } => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(handle_exec(action, port))?;
        }
    }

    Ok(())
}

async fn handle_exec(action: ExecAction, port: u16) -> Result<()> {
    use ws_server::send_command_to_server;

    match action {
        ExecAction::Inject { code } => {
            let response = send_command_to_server(port, WsCommand::InjectCode { code }).await?;
            print_response(response);
        }

        ExecAction::Run => {
            let response = send_command_to_server(port, WsCommand::TriggerRun).await?;
            print_response(response);
        }

        ExecAction::Screenshot { output } => {
            let response = send_command_to_server(port, WsCommand::Screenshot).await?;
            match response {
                WsResponse::Screenshot { base64 } => {
                    let data = base64::Engine::decode(
                        &base64::engine::general_purpose::STANDARD,
                        &base64,
                    )?;
                    std::fs::write(&output, data)?;
                    println!("Screenshot saved to: {}", output);
                }
                WsResponse::Error { message } => {
                    eprintln!("Error: {}", message);
                }
                _ => {
                    eprintln!("Unexpected response");
                }
            }
        }

        ExecAction::Preview => {
            let response = send_command_to_server(port, WsCommand::GetPreviewText).await?;
            match response {
                WsResponse::PreviewText { text } => {
                    println!("{}", text);
                }
                _ => print_response(response),
            }
        }

        ExecAction::Click { selector } => {
            let response = send_command_to_server(port, WsCommand::Click { selector }).await?;
            print_response(response);
        }

        ExecAction::Type { selector, text } => {
            let response = send_command_to_server(port, WsCommand::Type { selector, text }).await?;
            print_response(response);
        }

        ExecAction::Status => {
            let response = send_command_to_server(port, WsCommand::GetStatus).await?;
            print_response(response);
        }

        ExecAction::Console => {
            let response = send_command_to_server(port, WsCommand::GetConsole).await?;
            match response {
                WsResponse::Console { messages } => {
                    if messages.is_empty() {
                        println!("No console messages captured.");
                    } else {
                        for msg in messages {
                            let level_indicator = match msg.level.as_str() {
                                "error" => "[ERROR]",
                                "warn" => "[WARN]",
                                "info" => "[INFO]",
                                _ => "[LOG]",
                            };
                            println!("{} {}", level_indicator, msg.text);
                        }
                    }
                }
                _ => print_response(response),
            }
        }

        ExecAction::Reload => {
            println!("Sending reload command to extension...");
            let response = send_command_to_server(port, WsCommand::Reload).await?;
            print_response(response);
        }

        ExecAction::Test { code, expect, screenshot } => {
            // Inject code
            println!("Injecting code...");
            let response = send_command_to_server(port, WsCommand::InjectCode { code }).await?;
            if matches!(response, WsResponse::Error { .. }) {
                print_response(response);
                return Ok(());
            }

            // Trigger run
            println!("Triggering run...");
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            let response = send_command_to_server(port, WsCommand::TriggerRun).await?;
            if matches!(response, WsResponse::Error { .. }) {
                print_response(response);
                return Ok(());
            }

            // Wait for execution
            println!("Waiting for execution...");
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;

            // Get preview text
            let response = send_command_to_server(port, WsCommand::GetPreviewText).await?;
            match response {
                WsResponse::PreviewText { text } => {
                    println!("Preview: {}", text);
                    if let Some(expected) = expect {
                        if text.contains(&expected) {
                            println!("PASS: Found expected text '{}'", expected);
                        } else {
                            println!("FAIL: Expected '{}' not found", expected);
                        }
                    }
                }
                _ => print_response(response),
            }

            // Take screenshot if requested
            if let Some(output) = screenshot {
                let response = send_command_to_server(port, WsCommand::Screenshot).await?;
                if let WsResponse::Screenshot { base64 } = response {
                    let data = base64::Engine::decode(
                        &base64::engine::general_purpose::STANDARD,
                        &base64,
                    )?;
                    std::fs::write(&output, data)?;
                    println!("Screenshot saved to: {}", output);
                }
            }
        }
    }

    Ok(())
}

fn print_response(response: WsResponse) {
    match response {
        WsResponse::Success { data } => {
            println!("Success");
            if let Some(data) = data {
                println!("{}", serde_json::to_string_pretty(&data).unwrap());
            }
        }
        WsResponse::Error { message } => {
            eprintln!("Error: {}", message);
        }
        WsResponse::Pong => {
            println!("Pong");
        }
        WsResponse::Status { connected, page_url, api_ready } => {
            println!("Connected: {}", connected);
            println!("Page URL: {:?}", page_url);
            println!("API Ready: {}", api_ready);
        }
        other => {
            println!("{:?}", other);
        }
    }
}
