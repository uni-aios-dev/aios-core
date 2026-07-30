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
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_millis()
        .init();

    let cli = Cli::parse();
    let config = AppConfig::default();

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
        log::info!("AIOS TUI mode — starting interactive dashboard");
        tui::run_tui(state)?;
    }

    Ok(())
}
