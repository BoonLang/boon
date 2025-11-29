mod cdp;
mod commands;

use anyhow::Result;
use clap::{Parser, Subcommand};

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
    }

    Ok(())
}
