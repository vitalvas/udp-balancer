mod app;
mod balancer;
mod config;
mod listener;
mod metrics;
mod raw_socket;
mod server;

use app::App;
use clap::Parser;
use config::Config;
use std::process::ExitCode;
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "udp-balancer")]
#[command(about = "High-performance UDP load balancer", long_about = None)]
#[command(version)]
struct Cli {
    /// Path to configuration file
    #[arg(short, long, value_name = "FILE")]
    config: String,
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    info!("Loading config from: {}", cli.config);

    let config = match Config::from_file(&cli.config) {
        Ok(config) => config,
        Err(e) => {
            error!("Failed to load config: {}", e);
            return ExitCode::FAILURE;
        }
    };

    info!("Config loaded successfully");
    info!("Status server: {}", config.status);
    info!("Servers configured: {}", config.servers.len());

    let app = App::new(config);

    if let Err(e) = app.run().await {
        error!("Application error: {}", e);
        return ExitCode::FAILURE;
    }

    ExitCode::SUCCESS
}
