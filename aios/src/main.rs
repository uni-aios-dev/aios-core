mod hw_probe;
mod orchestrator;
mod tui;

use clap::Parser;
use orchestrator::{initialize, AppConfig};
use std::sync::{Arc, Mutex};

#[derive(Parser)]
#[command(name = "aios", version = "1.0.0", about = "AIOS — AI Operating System")]
struct Cli {
    #[arg(long, help = "Run in headless daemon mode")]
    daemon: bool,
    #[arg(
        long,
        help = "Boot into safe mode (skip third-party blocks, disable bridge)"
    )]
    safe_mode: bool,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_millis()
        .init();

    let cli = Cli::parse();
    let config = AppConfig {
        safe_mode: cli.safe_mode,
        ..AppConfig::default()
    };

    let state = initialize(&config).await?;
    let state = Arc::new(Mutex::new(state));

    if cli.daemon {
        log::info!("AIOS daemon mode — running in background");
        let join = tokio::spawn(async move {
            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
            }
        });
        join.await?;
    } else {
        if cli.safe_mode {
            log::info!("AIOS SAFE MODE — minimal shell only");
        }
        log::info!("AIOS TUI mode — starting interactive dashboard");
        tui::run_tui(state)?;
    }

    Ok(())
}
