//! Layered, sanitized configuration for the Voice control plane.

use std::{
    env,
    error::Error,
    ffi::OsString,
    fmt, fs,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    time::Duration,
};

use clap::{Args, Parser, Subcommand};
use impossible_server_core::ServerLimits;
use serde::{Deserialize, Serialize};

const MAX_CONFIG_BYTES: u64 = 64 * 1024;
const DEFAULT_BIND: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8080);

/// Impossible Voice command-line interface.
#[derive(Debug, Parser)]
#[command(name = "impossible-voice", version)]
pub struct Cli {
    /// Operation to perform.
    #[command(subcommand)]
    pub command: Command,
}

/// Supported process operations.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Start the local Voice control plane.
    Serve(ServerOptions),
    /// Validate configuration without starting listeners or downloading artifacts.
    Doctor(ServerOptions),
    /// Install or verify the curated runtime and English STT/TTS models.
    Setup(ArtifactOptions),
    /// Inspect curated artifacts without writing or making network requests.
    Status(ArtifactLocation),
}

/// Curated artifact setup options.
#[derive(Debug, Args)]
pub struct ArtifactOptions {
    /// Artifact store root. The directory is ignored by the repository.
    #[arg(long, default_value = "runtime-artifacts")]
    pub artifact_root: PathBuf,
    /// Forbid downloads and verify already installed artifacts only.
    #[arg(long)]
    pub offline: bool,
}

/// Curated artifact inspection options.
#[derive(Debug, Args)]
pub struct ArtifactLocation {
    /// Artifact store root. The directory is ignored by the repository.
    #[arg(long, default_value = "runtime-artifacts")]
    pub artifact_root: PathBuf,
}

/// CLI overrides shared by `serve` and `doctor`.
#[derive(Debug, Args, Default)]
pub struct ServerOptions {
    /// Optional bounded TOML configuration file.
    #[arg(long)]
    config: Option<PathBuf>,
    /// Loopback bind address.
    #[arg(long)]
    bind: Option<SocketAddr>,
    /// Maximum encoded workload request body in bytes.
    #[arg(long)]
    max_request_bytes: Option<usize>,
    /// Maximum requests waiting behind the execution bound.
    #[arg(long)]
    queue_capacity: Option<usize>,
    /// Maximum concurrently executing workload requests.
    #[arg(long)]
    max_concurrent_requests: Option<usize>,
    /// Overall request deadline in milliseconds.
    #[arg(long)]
    request_timeout_ms: Option<u64>,
    /// Total drain and cleanup deadline in milliseconds.
    #[arg(long)]
    shutdown_timeout_ms: Option<u64>,
}

/// Environment lookup seam used by configuration resolution and tests.
pub trait Environment {
    /// Returns an environment variable without lossy conversion.
    fn get(&self, name: &str) -> Option<OsString>;
}

/// Process environment implementation.
#[derive(Debug, Clone, Copy)]
pub struct ProcessEnvironment;

impl Environment for ProcessEnvironment {
    fn get(&self, name: &str) -> Option<OsString> {
        env::var_os(name)
    }
}

/// Sanitized configuration failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfigError {
    field: &'static str,
    message: &'static str,
}

impl ConfigError {
    const fn new(field: &'static str, message: &'static str) -> Self {
        Self { field, message }
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} {}", self.field, self.message)
    }
}

impl Error for ConfigError {}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct PartialConfig {
    bind: Option<SocketAddr>,
    max_request_bytes: Option<usize>,
    queue_capacity: Option<usize>,
    max_concurrent_requests: Option<usize>,
    request_timeout_ms: Option<u64>,
    shutdown_timeout_ms: Option<u64>,
}

impl PartialConfig {
    fn overlay(mut self, higher: Self) -> Self {
        macro_rules! replace {
            ($field:ident) => {
                if higher.$field.is_some() {
                    self.$field = higher.$field;
                }
            };
        }
        replace!(bind);
        replace!(max_request_bytes);
        replace!(queue_capacity);
        replace!(max_concurrent_requests);
        replace!(request_timeout_ms);
        replace!(shutdown_timeout_ms);
        self
    }
}

/// Validated effective process configuration.
#[derive(Debug, Clone, Copy)]
pub struct RuntimeConfig {
    bind: SocketAddr,
    limits: ServerLimits,
}

impl RuntimeConfig {
    /// Listener address, guaranteed to be loopback-only.
    #[must_use]
    pub const fn bind(self) -> SocketAddr {
        self.bind
    }

    /// Validated resource and lifecycle limits.
    #[must_use]
    pub const fn limits(self) -> ServerLimits {
        self.limits
    }

    /// Privacy-safe configuration validation report.
    #[must_use]
    pub fn doctor_report(self) -> DoctorReport {
        DoctorReport {
            status: "ok",
            bind_scope: "loopback",
            max_request_bytes: self.limits.max_request_bytes(),
            queue_capacity: self.limits.queue_capacity(),
            max_concurrent_requests: self.limits.max_concurrent_requests(),
            request_timeout_ms: self.limits.request_timeout().as_millis(),
            shutdown_timeout_ms: self.limits.shutdown_timeout().as_millis(),
            downloads_during_serve: false,
        }
    }
}

/// Privacy-safe `doctor` output.
#[derive(Debug, Serialize)]
pub struct DoctorReport {
    status: &'static str,
    bind_scope: &'static str,
    max_request_bytes: usize,
    queue_capacity: usize,
    max_concurrent_requests: usize,
    request_timeout_ms: u128,
    shutdown_timeout_ms: u128,
    downloads_during_serve: bool,
}

impl ServerOptions {
    /// Resolves defaults, then file, environment, and CLI in increasing precedence order.
    ///
    /// # Errors
    /// Returns a sanitized error for malformed, oversized, unreadable, unsafe, or out-of-range
    /// configuration.
    pub fn resolve<E: Environment>(&self, environment: &E) -> Result<RuntimeConfig, ConfigError> {
        let file_path = self.config.clone().or_else(|| {
            environment
                .get("IMPOSSIBLE_VOICE_CONFIG")
                .map(PathBuf::from)
        });
        let file = file_path
            .as_deref()
            .map(read_file)
            .transpose()?
            .unwrap_or_default();
        let environment = read_environment(environment)?;
        let cli = PartialConfig {
            bind: self.bind,
            max_request_bytes: self.max_request_bytes,
            queue_capacity: self.queue_capacity,
            max_concurrent_requests: self.max_concurrent_requests,
            request_timeout_ms: self.request_timeout_ms,
            shutdown_timeout_ms: self.shutdown_timeout_ms,
        };
        resolve_layers(file.overlay(environment).overlay(cli))
    }
}

fn read_file(path: &Path) -> Result<PartialConfig, ConfigError> {
    let metadata =
        fs::metadata(path).map_err(|_| ConfigError::new("config", "could not be read"))?;
    if !metadata.is_file() || metadata.len() > MAX_CONFIG_BYTES {
        return Err(ConfigError::new("config", "is outside the supported bound"));
    }
    let text =
        fs::read_to_string(path).map_err(|_| ConfigError::new("config", "could not be read"))?;
    toml::from_str(&text).map_err(|_| ConfigError::new("config", "is malformed"))
}

fn read_environment<E: Environment>(environment: &E) -> Result<PartialConfig, ConfigError> {
    Ok(PartialConfig {
        bind: parse_environment(environment, "IMPOSSIBLE_VOICE_BIND", "bind")?,
        max_request_bytes: parse_environment(
            environment,
            "IMPOSSIBLE_VOICE_MAX_REQUEST_BYTES",
            "max_request_bytes",
        )?,
        queue_capacity: parse_environment(
            environment,
            "IMPOSSIBLE_VOICE_QUEUE_CAPACITY",
            "queue_capacity",
        )?,
        max_concurrent_requests: parse_environment(
            environment,
            "IMPOSSIBLE_VOICE_MAX_CONCURRENT_REQUESTS",
            "max_concurrent_requests",
        )?,
        request_timeout_ms: parse_environment(
            environment,
            "IMPOSSIBLE_VOICE_REQUEST_TIMEOUT_MS",
            "request_timeout_ms",
        )?,
        shutdown_timeout_ms: parse_environment(
            environment,
            "IMPOSSIBLE_VOICE_SHUTDOWN_TIMEOUT_MS",
            "shutdown_timeout_ms",
        )?,
    })
}

fn parse_environment<T: std::str::FromStr, E: Environment>(
    environment: &E,
    name: &str,
    field: &'static str,
) -> Result<Option<T>, ConfigError> {
    let Some(value) = environment.get(name) else {
        return Ok(None);
    };
    let value = value
        .into_string()
        .map_err(|_| ConfigError::new(field, "is malformed"))?;
    value
        .parse()
        .map(Some)
        .map_err(|_| ConfigError::new(field, "is malformed"))
}

fn resolve_layers(values: PartialConfig) -> Result<RuntimeConfig, ConfigError> {
    let bind = values.bind.unwrap_or(DEFAULT_BIND);
    if !bind.ip().is_loopback() {
        return Err(ConfigError::new("bind", "must be loopback"));
    }
    let limits = ServerLimits::new(
        values.max_request_bytes.unwrap_or(16 * 1024 * 1024),
        values.queue_capacity.unwrap_or(128),
        values.max_concurrent_requests.unwrap_or(4),
        Duration::from_millis(values.request_timeout_ms.unwrap_or(60_000)),
        Duration::from_millis(values.shutdown_timeout_ms.unwrap_or(10_000)),
    )
    .map_err(|error| ConfigError::new(error.field(), "is outside the supported bound"))?;
    Ok(RuntimeConfig { bind, limits })
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, ffi::OsString};

    use super::{Environment, PartialConfig, ServerOptions, resolve_layers};

    #[derive(Default)]
    struct FakeEnvironment(HashMap<String, OsString>);

    impl Environment for FakeEnvironment {
        fn get(&self, name: &str) -> Option<OsString> {
            self.0.get(name).cloned()
        }
    }

    #[test]
    fn defaults_are_loopback_and_bounded() -> Result<(), Box<dyn std::error::Error>> {
        let config = ServerOptions::default().resolve(&FakeEnvironment::default())?;
        assert!(config.bind().ip().is_loopback());
        assert_eq!(config.limits().max_concurrent_requests(), 4);
        assert!(!config.doctor_report().downloads_during_serve);
        Ok(())
    }

    #[test]
    fn environment_overrides_defaults_and_cli_overrides_environment()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut environment = FakeEnvironment::default();
        environment.0.insert(
            "IMPOSSIBLE_VOICE_MAX_CONCURRENT_REQUESTS".into(),
            "7".into(),
        );
        let options = ServerOptions {
            max_concurrent_requests: Some(9),
            ..ServerOptions::default()
        };
        let config = options.resolve(&environment)?;
        assert_eq!(config.limits().max_concurrent_requests(), 9);
        Ok(())
    }

    #[test]
    fn file_values_are_below_environment_values() -> Result<(), Box<dyn std::error::Error>> {
        let file: PartialConfig = toml::from_str("max_concurrent_requests = 5")?;
        let environment = PartialConfig {
            max_concurrent_requests: Some(7),
            ..PartialConfig::default()
        };
        let config = resolve_layers(file.overlay(environment))?;
        assert_eq!(config.limits().max_concurrent_requests(), 7);
        Ok(())
    }

    #[test]
    fn non_loopback_bind_is_rejected_without_echoing_it() {
        let values = PartialConfig {
            bind: "0.0.0.0:8080".parse().ok(),
            ..PartialConfig::default()
        };
        let result = resolve_layers(values);
        assert!(result.is_err());
        if let Err(error) = result {
            assert_eq!(error.to_string(), "bind must be loopback");
        }
    }

    #[test]
    fn malformed_environment_is_sanitized() {
        let mut environment = FakeEnvironment::default();
        environment.0.insert(
            "IMPOSSIBLE_VOICE_MAX_REQUEST_BYTES".into(),
            "private-payload".into(),
        );
        let result = ServerOptions::default().resolve(&environment);
        assert!(result.is_err());
        if let Err(error) = result {
            assert_eq!(error.to_string(), "max_request_bytes is malformed");
        }
    }
}
