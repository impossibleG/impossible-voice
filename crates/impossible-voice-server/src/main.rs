//! Impossible Voice process entry point.

use clap::Parser;
use impossible_server_core::CancellationToken;
use impossible_voice_server::{
    PlaceholderWorkload, TemplateServer,
    config::{Cli, Command, ProcessEnvironment},
};
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let environment = ProcessEnvironment;
    match cli.command {
        Command::Serve(options) => {
            let config = options.resolve(&environment)?;
            let listener = TcpListener::bind(config.bind()).await?;
            let cancellation = CancellationToken::new();
            let signal = cancellation.clone();
            tokio::spawn(async move {
                if tokio::signal::ctrl_c().await.is_ok() {
                    let _ = signal.cancel();
                }
            });
            TemplateServer::with_limits(PlaceholderWorkload, config.limits())
                .serve(listener, cancellation)
                .await?;
        }
        Command::Doctor(options) => {
            let config = options.resolve(&environment)?;
            println!("{}", serde_json::to_string(&config.doctor_report())?);
        }
    }
    Ok(())
}
