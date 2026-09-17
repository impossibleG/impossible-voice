//! Impossible Voice process entry point.

use clap::Parser;
use impossible_server_core::CancellationToken;
use impossible_voice_artifacts::ArtifactStore;
use impossible_voice_server::{
    TemplateServer, VoiceEngineWorkload,
    config::{Cli, Command, ProcessEnvironment},
    engines::{VoiceEngines, inspect},
};
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let environment = ProcessEnvironment;
    match cli.command {
        Command::Serve(options) => {
            let config = options.resolve(&environment)?;
            let workload = VoiceEngines::load(
                config.artifact_root(),
                config.limits().max_concurrent_requests(),
            )
            .map_or_else(
                |error| {
                    eprintln!("{error}");
                    VoiceEngineWorkload::unavailable()
                },
                VoiceEngineWorkload::new,
            );
            let listener = TcpListener::bind(config.bind()).await?;
            let cancellation = CancellationToken::new();
            let signal = cancellation.clone();
            tokio::spawn(async move {
                if tokio::signal::ctrl_c().await.is_ok() {
                    let _ = signal.cancel();
                }
            });
            TemplateServer::with_limits(workload, config.limits())
                .serve(listener, cancellation)
                .await?;
        }
        Command::Doctor(options) => {
            let config = options.resolve(&environment)?;
            println!(
                "{}",
                serde_json::to_string(&serde_json::json!({
                    "config": config.doctor_report(),
                    "engines": inspect(config.artifact_root())?,
                }))?
            );
        }
        Command::Setup(options) => {
            let report = ArtifactStore::new(options.artifact_root)?
                .setup(options.offline)
                .await?;
            println!("{}", serde_json::to_string(&report)?);
        }
        Command::Status(options) => {
            let report = inspect(&options.artifact_root)?;
            println!("{}", serde_json::to_string(&report)?);
        }
    }
    Ok(())
}
