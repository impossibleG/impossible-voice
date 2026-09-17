//! Impossible Voice process entry point.

use std::sync::Arc;

use clap::Parser;
use impossible_server_core::CancellationToken;
use impossible_voice_artifacts::ArtifactStore;
use impossible_voice_server::{
    TemplateServer, VoiceEngineWorkload,
    config::{Cli, Command, ProcessEnvironment},
    engines::{VoiceEngines, inspect},
    grpc,
    voice_api::VoiceBackend,
};
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let environment = ProcessEnvironment;
    match cli.command {
        Command::Serve(options) => {
            let config = options.resolve(&environment)?;
            let engines = VoiceEngines::load(
                config.artifact_root(),
                config.limits().max_concurrent_requests(),
            );
            let backend = engines.map_or_else(
                |error| {
                    eprintln!("{error}");
                    None
                },
                |engines| Some(Arc::new(engines) as Arc<dyn VoiceBackend>),
            );
            let workload = backend
                .as_ref()
                .map_or_else(VoiceEngineWorkload::unavailable, |backend| {
                    VoiceEngineWorkload::with_backend(Arc::clone(backend))
                });
            let listener = TcpListener::bind(config.bind()).await?;
            let cancellation = CancellationToken::new();
            let signal = cancellation.clone();
            tokio::spawn(async move {
                if tokio::signal::ctrl_c().await.is_ok() {
                    let _ = signal.cancel();
                }
            });
            let grpc_task = if let Some(backend) = backend {
                let grpc_listener = TcpListener::bind(config.grpc_bind()).await?;
                let grpc_shutdown = cancellation.clone();
                let timeout = config.limits().request_timeout();
                Some(tokio::spawn(async move {
                    grpc::serve(grpc_listener, backend, timeout, grpc_shutdown).await
                }))
            } else {
                None
            };
            let http_result = TemplateServer::with_limits(workload, config.limits())
                .serve(listener, cancellation.clone())
                .await;
            let _ = cancellation.cancel();
            if let Some(grpc_task) = grpc_task {
                match grpc_task.await {
                    Ok(Ok(())) => {}
                    Ok(Err(_)) | Err(_) => return Err("gRPC server failed".into()),
                }
            }
            http_result?;
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
