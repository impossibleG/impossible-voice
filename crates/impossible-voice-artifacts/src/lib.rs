//! Curated, checksum-verified Voice runtime and model installation.

use std::{
    collections::{HashMap, HashSet},
    error::Error,
    fmt,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use bzip2::read::BzDecoder;
use fs2::FileExt;
use futures_util::StreamExt;
use reqwest::{Client, Url, redirect::Policy};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

const MANIFEST_JSON: &str = include_str!("../manifests/curated-v1.json");
const INSTALLATION_FILE: &str = ".impossible-voice-installation.json";
const MAX_ARCHIVE_ENTRIES: usize = 16_384;

/// Sanitized artifact lifecycle failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArtifactError {
    message: &'static str,
}

impl ArtifactError {
    const fn new(message: &'static str) -> Self {
        Self { message }
    }
}

impl fmt::Display for ArtifactError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl Error for ArtifactError {}

#[derive(Debug, Clone, Deserialize)]
struct CuratedManifest {
    schema_version: u32,
    profile: String,
    artifacts: Vec<ArtifactSpec>,
}

#[derive(Debug, Clone, Deserialize)]
struct ArtifactSpec {
    id: String,
    kind: String,
    platforms: Vec<String>,
    url: String,
    size: u64,
    sha256: String,
    archive_root: String,
    max_unpacked_bytes: u64,
    required_paths: Vec<String>,
    licenses: Vec<LicenseSpec>,
}

#[derive(Debug, Clone, Deserialize)]
struct LicenseSpec {
    scope: String,
    spdx: String,
    source: String,
    revision: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct InstallationManifest {
    schema_version: u32,
    artifact_id: String,
    archive_sha256: String,
    files: Vec<InstalledFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct InstalledFile {
    path: String,
    size: u64,
    sha256: String,
}

/// Per-artifact installation result.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ArtifactResult {
    /// Stable curated artifact identifier.
    pub id: String,
    /// Artifact role: runtime, STT model, or TTS model.
    pub kind: String,
    /// Whether setup installed bytes or verified an existing object.
    pub state: &'static str,
}

/// Machine-readable setup or offline-verification report.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SetupReport {
    /// Curated profile identifier.
    pub profile: String,
    /// Selected supported platform.
    pub platform: &'static str,
    /// Overall status.
    pub status: &'static str,
    /// Results for the platform runtime and both models.
    pub artifacts: Vec<ArtifactResult>,
}

/// Content-addressed artifact store.
#[derive(Debug, Clone)]
pub struct ArtifactStore {
    root: PathBuf,
    client: Client,
}

/// Verified private paths required to load the curated native engines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedProfile {
    runtime: PathBuf,
    stt: PathBuf,
    tts: PathBuf,
}

impl VerifiedProfile {
    /// Root of the selected platform runtime object.
    #[must_use]
    pub fn runtime_root(&self) -> &Path {
        &self.runtime
    }

    /// Streaming `NeMo` CTC model path.
    #[must_use]
    pub fn stt_model(&self) -> PathBuf {
        self.stt.join("model.int8.onnx")
    }

    /// Streaming `NeMo` token table path.
    #[must_use]
    pub fn stt_tokens(&self) -> PathBuf {
        self.stt.join("tokens.txt")
    }

    /// Kristin VITS model path.
    #[must_use]
    pub fn tts_model(&self) -> PathBuf {
        self.tts.join("en_US-kristin-medium.onnx")
    }

    /// Kristin VITS token table path.
    #[must_use]
    pub fn tts_tokens(&self) -> PathBuf {
        self.tts.join("tokens.txt")
    }

    /// Pinned `espeak-ng-data` path used for English phonemization.
    #[must_use]
    pub fn tts_data_dir(&self) -> PathBuf {
        self.tts.join("espeak-ng-data")
    }
}

impl ArtifactStore {
    /// Creates an artifact store without reading, writing, or downloading anything.
    ///
    /// # Errors
    /// Returns a sanitized error if the bounded HTTP client cannot be constructed.
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, ArtifactError> {
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .timeout(Duration::from_secs(30 * 60))
            .redirect(Policy::limited(5))
            .https_only(true)
            .build()
            .map_err(|_| ArtifactError::new("artifact HTTP client initialization failed"))?;
        Ok(Self {
            root: root.into(),
            client,
        })
    }

    /// Installs or verifies the complete curated profile for the current platform.
    ///
    /// Offline mode performs no network requests and succeeds only when every installed object
    /// passes its complete per-file inventory.
    ///
    /// # Errors
    /// Returns a sanitized error for unsupported platforms, invalid manifests, failed downloads,
    /// archive attacks, integrity failures, or filesystem failures.
    pub async fn setup(&self, offline: bool) -> Result<SetupReport, ArtifactError> {
        let manifest = parse_manifest()?;
        let platform = current_platform()?;
        let selected = select_artifacts(&manifest, platform)?;
        fs::create_dir_all(self.root.join("objects"))
            .and_then(|()| fs::create_dir_all(self.root.join("downloads")))
            .and_then(|()| fs::create_dir_all(self.root.join("staging")))
            .and_then(|()| fs::create_dir_all(self.root.join("recovery")))
            .map_err(|_| ArtifactError::new("artifact storage initialization failed"))?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.root.join(".setup.lock"))
            .map_err(|_| ArtifactError::new("artifact setup lock could not be opened"))?;
        FileExt::lock_exclusive(&lock)
            .map_err(|_| ArtifactError::new("artifact setup lock could not be acquired"))?;

        let mut results = Vec::with_capacity(selected.len());
        for artifact in selected {
            let state = self.ensure_artifact(&artifact, offline).await?;
            results.push(ArtifactResult {
                id: artifact.id,
                kind: artifact.kind,
                state,
            });
        }
        Ok(SetupReport {
            profile: manifest.profile,
            platform,
            status: "ready",
            artifacts: results,
        })
    }

    /// Inspects the curated profile without writing or making network requests.
    ///
    /// # Errors
    /// Returns a sanitized error if the compiled manifest or current platform is unsupported.
    pub fn status(&self) -> Result<SetupReport, ArtifactError> {
        let manifest = parse_manifest()?;
        let platform = current_platform()?;
        let selected = select_artifacts(&manifest, platform)?;
        let mut ready = true;
        let artifacts = selected
            .into_iter()
            .map(|artifact| {
                let target = self.object_path(&artifact);
                let state = if !target.exists() {
                    ready = false;
                    "missing"
                } else if verify_object(&target, &artifact).is_ok() {
                    "verified"
                } else {
                    ready = false;
                    "invalid"
                };
                ArtifactResult {
                    id: artifact.id,
                    kind: artifact.kind,
                    state,
                }
            })
            .collect();
        Ok(SetupReport {
            profile: manifest.profile,
            platform,
            status: if ready { "ready" } else { "not-ready" },
            artifacts,
        })
    }

    /// Verifies the complete profile and returns private engine paths without writing or downloading.
    ///
    /// # Errors
    /// Returns a sanitized error when any selected object is missing, altered, or unsupported.
    pub fn verified_profile(&self) -> Result<VerifiedProfile, ArtifactError> {
        let manifest = parse_manifest()?;
        let platform = current_platform()?;
        let selected = select_artifacts(&manifest, platform)?;
        let runtime = verified_kind_path(self, &selected, "runtime")?;
        let stt = verified_kind_path(self, &selected, "stt-model")?;
        let tts = verified_kind_path(self, &selected, "tts-model")?;
        Ok(VerifiedProfile { runtime, stt, tts })
    }

    async fn ensure_artifact(
        &self,
        artifact: &ArtifactSpec,
        offline: bool,
    ) -> Result<&'static str, ArtifactError> {
        let target = self.object_path(artifact);
        if target.exists() {
            let verify_target = target.clone();
            let verify_spec = artifact.clone();
            if tokio::task::spawn_blocking(move || verify_object(&verify_target, &verify_spec))
                .await
                .map_err(|_| ArtifactError::new("artifact verification worker failed"))?
                .is_ok()
            {
                return Ok("verified");
            }
            recover_path(&target, &self.root.join("recovery"), "object")?;
        }
        if offline {
            return Err(ArtifactError::new(
                "offline setup requires a complete verified artifact profile",
            ));
        }

        let archive = self
            .root
            .join("downloads")
            .join(format!("{}-{}.tar.bz2", artifact.id, artifact.sha256));
        if archive.exists() {
            let verify_archive = archive.clone();
            let expected_size = artifact.size;
            let expected_hash = artifact.sha256.clone();
            let valid = tokio::task::spawn_blocking(move || {
                verify_archive_file(&verify_archive, expected_size, &expected_hash)
            })
            .await
            .map_err(|_| ArtifactError::new("archive verification worker failed"))?
            .is_ok();
            if !valid {
                recover_path(&archive, &self.root.join("recovery"), "archive")?;
            }
        }
        if !archive.exists() {
            self.download(artifact, &archive).await?;
        }

        let staging = self.root.join("staging").join(format!(
            "{}-{}-{}",
            artifact.id,
            std::process::id(),
            unique_suffix()
        ));
        fs::create_dir(&staging)
            .map_err(|_| ArtifactError::new("artifact staging directory could not be created"))?;
        let extract_archive = archive.clone();
        let extract_staging = staging.clone();
        let extract_spec = artifact.clone();
        let extraction = tokio::task::spawn_blocking(move || {
            extract_verified_archive(&extract_archive, &extract_staging, &extract_spec)
        })
        .await
        .map_err(|_| ArtifactError::new("artifact extraction worker failed"))?;
        if let Err(error) = extraction {
            recover_path(&staging, &self.root.join("recovery"), "staging")?;
            return Err(error);
        }
        let verify_staging = staging.clone();
        let verify_spec = artifact.clone();
        let verification =
            tokio::task::spawn_blocking(move || verify_object(&verify_staging, &verify_spec))
                .await
                .map_err(|_| ArtifactError::new("artifact verification worker failed"))?;
        if let Err(error) = verification {
            recover_path(&staging, &self.root.join("recovery"), "staging")?;
            return Err(error);
        }
        fs::rename(&staging, &target)
            .map_err(|_| ArtifactError::new("artifact activation failed"))?;
        Ok("installed")
    }

    async fn download(&self, artifact: &ArtifactSpec, archive: &Path) -> Result<(), ArtifactError> {
        let url = Url::parse(&artifact.url)
            .map_err(|_| ArtifactError::new("curated artifact URL is invalid"))?;
        if url.scheme() != "https" || url.host_str() != Some("github.com") {
            return Err(ArtifactError::new(
                "curated artifact origin is not permitted",
            ));
        }
        let response = self
            .client
            .get(url)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|_| ArtifactError::new("artifact download failed"))?;
        if response
            .content_length()
            .is_some_and(|size| size != artifact.size)
        {
            return Err(ArtifactError::new("artifact download size mismatch"));
        }
        let partial = archive.with_extension(format!(
            "partial-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        let mut file = tokio::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&partial)
            .await
            .map_err(|_| ArtifactError::new("artifact download staging failed"))?;
        let mut stream = response.bytes_stream();
        let mut size = 0_u64;
        let mut hash = Sha256::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| ArtifactError::new("artifact download failed"))?;
            size = size
                .checked_add(chunk.len() as u64)
                .ok_or_else(|| ArtifactError::new("artifact download exceeded its bound"))?;
            if size > artifact.size {
                return Err(ArtifactError::new("artifact download exceeded its bound"));
            }
            hash.update(&chunk);
            file.write_all(&chunk)
                .await
                .map_err(|_| ArtifactError::new("artifact download staging failed"))?;
        }
        file.flush()
            .await
            .map_err(|_| ArtifactError::new("artifact download staging failed"))?;
        drop(file);
        if size != artifact.size || hex_digest(hash.finalize()) != artifact.sha256 {
            recover_path(&partial, &self.root.join("recovery"), "download")?;
            return Err(ArtifactError::new("artifact download integrity mismatch"));
        }
        fs::rename(&partial, archive)
            .map_err(|_| ArtifactError::new("artifact archive activation failed"))?;
        Ok(())
    }

    fn object_path(&self, artifact: &ArtifactSpec) -> PathBuf {
        self.root
            .join("objects")
            .join(format!("{}-{}", artifact.id, &artifact.sha256[..16]))
    }
}

fn verified_kind_path(
    store: &ArtifactStore,
    selected: &[ArtifactSpec],
    kind: &str,
) -> Result<PathBuf, ArtifactError> {
    let artifact = selected
        .iter()
        .find(|artifact| artifact.kind == kind)
        .ok_or_else(|| ArtifactError::new("curated artifact profile is incomplete"))?;
    let path = store.object_path(artifact);
    verify_object(&path, artifact)?;
    Ok(path)
}

fn parse_manifest() -> Result<CuratedManifest, ArtifactError> {
    let manifest: CuratedManifest = serde_json::from_str(MANIFEST_JSON)
        .map_err(|_| ArtifactError::new("curated artifact manifest is malformed"))?;
    if manifest.schema_version != 1 || manifest.profile != "impossible-voice-en-cpu-v1" {
        return Err(ArtifactError::new(
            "curated artifact manifest identity is invalid",
        ));
    }
    let mut ids = HashSet::new();
    for artifact in &manifest.artifacts {
        validate_artifact(artifact)?;
        if !ids.insert(&artifact.id) {
            return Err(ArtifactError::new(
                "curated artifact manifest contains duplicate identifiers",
            ));
        }
    }
    Ok(manifest)
}

fn validate_artifact(artifact: &ArtifactSpec) -> Result<(), ArtifactError> {
    if artifact.id.is_empty()
        || !artifact
            .id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        || !matches!(
            artifact.kind.as_str(),
            "runtime" | "stt-model" | "tts-model"
        )
        || artifact.size == 0
        || artifact.max_unpacked_bytes == 0
        || artifact.sha256.len() != 64
        || !artifact.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
        || artifact.archive_root.is_empty()
        || artifact.required_paths.is_empty()
        || artifact.licenses.is_empty()
    {
        return Err(ArtifactError::new(
            "curated artifact manifest contains an invalid entry",
        ));
    }
    for license in &artifact.licenses {
        if license.scope.is_empty()
            || license.spdx.is_empty()
            || license.source.is_empty()
            || license.revision.is_empty()
        {
            return Err(ArtifactError::new(
                "curated artifact license metadata is incomplete",
            ));
        }
    }
    for path in &artifact.required_paths {
        safe_relative_path(Path::new(path))?;
    }
    Ok(())
}

fn select_artifacts(
    manifest: &CuratedManifest,
    platform: &str,
) -> Result<Vec<ArtifactSpec>, ArtifactError> {
    let selected: Vec<_> = manifest
        .artifacts
        .iter()
        .filter(|artifact| artifact.platforms.iter().any(|item| item == platform))
        .cloned()
        .collect();
    let counts: HashMap<&str, usize> = selected.iter().fold(HashMap::new(), |mut map, item| {
        *map.entry(item.kind.as_str()).or_default() += 1;
        map
    });
    if selected.len() != 3
        || counts.get("runtime") != Some(&1)
        || counts.get("stt-model") != Some(&1)
        || counts.get("tts-model") != Some(&1)
    {
        return Err(ArtifactError::new(
            "curated artifact profile is incomplete for this platform",
        ));
    }
    Ok(selected)
}

fn current_platform() -> Result<&'static str, ArtifactError> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => Ok("windows-x86_64"),
        ("linux", "x86_64") => Ok("linux-x86_64"),
        _ => Err(ArtifactError::new(
            "the curated artifact profile does not support this platform",
        )),
    }
}

fn verify_archive_file(path: &Path, size: u64, sha256: &str) -> Result<(), ArtifactError> {
    let metadata = fs::metadata(path)
        .map_err(|_| ArtifactError::new("artifact archive could not be inspected"))?;
    if !metadata.is_file() || metadata.len() != size {
        return Err(ArtifactError::new("artifact archive size mismatch"));
    }
    let mut file = File::open(path)
        .map_err(|_| ArtifactError::new("artifact archive could not be inspected"))?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024].into_boxed_slice();
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|_| ArtifactError::new("artifact archive could not be inspected"))?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    if hex_digest(hash.finalize()) != sha256 {
        return Err(ArtifactError::new("artifact archive digest mismatch"));
    }
    Ok(())
}

fn extract_verified_archive(
    archive_path: &Path,
    staging: &Path,
    artifact: &ArtifactSpec,
) -> Result<(), ArtifactError> {
    verify_archive_file(archive_path, artifact.size, &artifact.sha256)?;
    let file = File::open(archive_path)
        .map_err(|_| ArtifactError::new("artifact archive could not be opened"))?;
    let decoder = BzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);
    let entries = archive
        .entries()
        .map_err(|_| ArtifactError::new("artifact archive is malformed"))?;
    let mut installed = Vec::new();
    let mut seen = HashSet::new();
    let mut total = 0_u64;
    for (index, entry) in entries.enumerate() {
        if index >= MAX_ARCHIVE_ENTRIES {
            return Err(ArtifactError::new("artifact archive has too many entries"));
        }
        let mut entry = entry.map_err(|_| ArtifactError::new("artifact archive is malformed"))?;
        let path = entry
            .path()
            .map_err(|_| ArtifactError::new("artifact archive path is invalid"))?;
        let relative = strip_archive_root(&path, &artifact.archive_root)?;
        if relative.as_os_str().is_empty() {
            continue;
        }
        let relative_text = relative
            .to_str()
            .ok_or_else(|| ArtifactError::new("artifact archive path is invalid"))?
            .replace('\\', "/");
        let kind = entry.header().entry_type();
        let destination = staging.join(&relative);
        if kind.is_dir() {
            fs::create_dir_all(&destination)
                .map_err(|_| ArtifactError::new("artifact directory extraction failed"))?;
            continue;
        }
        if !kind.is_file() || !seen.insert(relative_text.clone()) {
            return Err(ArtifactError::new(
                "artifact archive contains an unsupported or duplicate entry",
            ));
        }
        installed.push(write_extracted_file(
            &mut entry,
            &destination,
            relative_text,
            &mut total,
            artifact.max_unpacked_bytes,
        )?);
    }
    installed.sort_by(|left, right| left.path.cmp(&right.path));
    if installed.is_empty() {
        return Err(ArtifactError::new("artifact archive contained no files"));
    }
    let manifest = InstallationManifest {
        schema_version: 1,
        artifact_id: artifact.id.clone(),
        archive_sha256: artifact.sha256.clone(),
        files: installed,
    };
    let bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|_| ArtifactError::new("installation inventory could not be encoded"))?;
    let mut marker = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(staging.join(INSTALLATION_FILE))
        .map_err(|_| ArtifactError::new("installation inventory could not be written"))?;
    marker
        .write_all(&bytes)
        .and_then(|()| marker.sync_all())
        .map_err(|_| ArtifactError::new("installation inventory could not be written"))?;
    Ok(())
}

fn write_extracted_file(
    input: &mut impl Read,
    destination: &Path,
    relative_path: String,
    total: &mut u64,
    max_unpacked_bytes: u64,
) -> Result<InstalledFile, ArtifactError> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)
            .map_err(|_| ArtifactError::new("artifact directory extraction failed"))?;
    }
    let mut output = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(destination)
        .map_err(|_| ArtifactError::new("artifact file extraction failed"))?;
    let mut hash = Sha256::new();
    let mut size = 0_u64;
    let mut buffer = vec![0_u8; 64 * 1024].into_boxed_slice();
    loop {
        let count = input
            .read(&mut buffer)
            .map_err(|_| ArtifactError::new("artifact file extraction failed"))?;
        if count == 0 {
            break;
        }
        size = size
            .checked_add(count as u64)
            .ok_or_else(|| ArtifactError::new("artifact extraction exceeded its bound"))?;
        *total = total
            .checked_add(count as u64)
            .ok_or_else(|| ArtifactError::new("artifact extraction exceeded its bound"))?;
        if *total > max_unpacked_bytes {
            return Err(ArtifactError::new("artifact extraction exceeded its bound"));
        }
        hash.update(&buffer[..count]);
        output
            .write_all(&buffer[..count])
            .map_err(|_| ArtifactError::new("artifact file extraction failed"))?;
    }
    output
        .sync_all()
        .map_err(|_| ArtifactError::new("artifact file extraction failed"))?;
    Ok(InstalledFile {
        path: relative_path,
        size,
        sha256: hex_digest(hash.finalize()),
    })
}

fn verify_object(root: &Path, artifact: &ArtifactSpec) -> Result<(), ArtifactError> {
    let marker = fs::read(root.join(INSTALLATION_FILE))
        .map_err(|_| ArtifactError::new("installation inventory is missing"))?;
    let installation: InstallationManifest = serde_json::from_slice(&marker)
        .map_err(|_| ArtifactError::new("installation inventory is malformed"))?;
    if installation.schema_version != 1
        || installation.artifact_id != artifact.id
        || installation.archive_sha256 != artifact.sha256
        || installation.files.is_empty()
    {
        return Err(ArtifactError::new("installation identity is invalid"));
    }
    let mut expected = HashSet::new();
    for entry in &installation.files {
        let relative = Path::new(&entry.path);
        safe_relative_path(relative)?;
        if !expected.insert(entry.path.clone()) {
            return Err(ArtifactError::new(
                "installation inventory contains duplicate files",
            ));
        }
        verify_archive_file(&root.join(relative), entry.size, &entry.sha256)?;
    }
    let actual = inventory_files(root)?;
    if actual != expected {
        return Err(ArtifactError::new(
            "installed artifact inventory does not match",
        ));
    }
    for required in &artifact.required_paths {
        let path = root.join(required);
        if !path.exists() {
            return Err(ArtifactError::new(
                "installed artifact is missing a required path",
            ));
        }
    }
    Ok(())
}

fn inventory_files(root: &Path) -> Result<HashSet<String>, ArtifactError> {
    let mut pending = vec![root.to_path_buf()];
    let mut files = HashSet::new();
    while let Some(directory) = pending.pop() {
        let entries = fs::read_dir(&directory)
            .map_err(|_| ArtifactError::new("installed artifact could not be inventoried"))?;
        for entry in entries {
            let entry = entry
                .map_err(|_| ArtifactError::new("installed artifact could not be inventoried"))?;
            let file_type = entry
                .file_type()
                .map_err(|_| ArtifactError::new("installed artifact could not be inventoried"))?;
            if file_type.is_symlink() {
                return Err(ArtifactError::new(
                    "installed artifact contains a redirected path",
                ));
            }
            if file_type.is_dir() {
                pending.push(entry.path());
            } else if file_type.is_file() {
                let relative = entry
                    .path()
                    .strip_prefix(root)
                    .map_err(|_| ArtifactError::new("installed artifact path is invalid"))?
                    .to_str()
                    .ok_or_else(|| ArtifactError::new("installed artifact path is invalid"))?
                    .replace('\\', "/");
                if relative != INSTALLATION_FILE {
                    files.insert(relative);
                }
            } else {
                return Err(ArtifactError::new(
                    "installed artifact contains an unsupported entry",
                ));
            }
        }
    }
    Ok(files)
}

fn strip_archive_root(path: &Path, root: &str) -> Result<PathBuf, ArtifactError> {
    let mut components = path.components();
    match components.next() {
        Some(Component::Normal(value)) if value == root => {}
        _ => return Err(ArtifactError::new("artifact archive root is invalid")),
    }
    let mut relative = PathBuf::new();
    for component in components {
        match component {
            Component::Normal(value) => relative.push(value),
            _ => return Err(ArtifactError::new("artifact archive path is invalid")),
        }
    }
    Ok(relative)
}

fn safe_relative_path(path: &Path) -> Result<(), ArtifactError> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(ArtifactError::new("artifact relative path is invalid"));
    }
    Ok(())
}

fn recover_path(path: &Path, recovery: &Path, label: &str) -> Result<(), ArtifactError> {
    if !path.exists() {
        return Ok(());
    }
    fs::create_dir_all(recovery)
        .map_err(|_| ArtifactError::new("artifact recovery directory could not be created"))?;
    let target = recovery.join(format!(
        "{}-{}-{}",
        label,
        std::process::id(),
        unique_suffix()
    ));
    fs::rename(path, target)
        .map_err(|_| ArtifactError::new("invalid artifact could not be preserved for recovery"))
}

fn unique_suffix() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos())
}

fn hex_digest(bytes: impl AsRef<[u8]>) -> String {
    let mut result = String::with_capacity(bytes.as_ref().len() * 2);
    for byte in bytes.as_ref() {
        use fmt::Write as _;
        let _ = write!(result, "{byte:02x}");
    }
    result
}

#[cfg(test)]
mod tests {
    use std::{fs, io::Write};

    use bzip2::{Compression, write::BzEncoder};
    use sha2::{Digest, Sha256};
    use tempfile::tempdir;

    use super::{
        ArtifactSpec, ArtifactStore, extract_verified_archive, hex_digest, parse_manifest,
        recover_path, select_artifacts, strip_archive_root, verify_object,
    };

    #[test]
    fn curated_manifest_is_complete_for_supported_platform()
    -> Result<(), Box<dyn std::error::Error>> {
        let manifest = parse_manifest()?;
        let platform = super::current_platform()?;
        let artifacts = select_artifacts(&manifest, platform)?;
        assert_eq!(artifacts.len(), 3);
        assert!(
            artifacts
                .iter()
                .all(|item| item.url.starts_with("https://github.com/"))
        );
        Ok(())
    }

    #[test]
    fn safe_fixture_extracts_and_detects_later_corruption() -> Result<(), Box<dyn std::error::Error>>
    {
        let directory = tempdir()?;
        let archive_path = directory.path().join("fixture.tar.bz2");
        let archive_file = fs::File::create(&archive_path)?;
        let encoder = BzEncoder::new(archive_file, Compression::best());
        let mut builder = tar::Builder::new(encoder);
        let payload = b"verified fixture";
        let mut header = tar::Header::new_gnu();
        header.set_size(payload.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append_data(&mut header, "fixture/required.bin", payload.as_slice())?;
        builder.finish()?;
        let encoder = builder.into_inner()?;
        encoder.finish()?;

        let archive_bytes = fs::read(&archive_path)?;
        let spec = fixture_spec(
            archive_bytes.len() as u64,
            hex_digest(Sha256::digest(&archive_bytes)),
        );
        let staging = directory.path().join("staging");
        fs::create_dir(&staging)?;
        extract_verified_archive(&archive_path, &staging, &spec)?;
        verify_object(&staging, &spec)?;

        let mut changed = fs::OpenOptions::new()
            .append(true)
            .open(staging.join("required.bin"))?;
        changed.write_all(b"changed")?;
        assert!(verify_object(&staging, &spec).is_err());
        Ok(())
    }

    #[test]
    fn archive_paths_cannot_escape_the_expected_root() {
        assert!(strip_archive_root(std::path::Path::new("fixture/../escape"), "fixture").is_err());
        assert!(strip_archive_root(std::path::Path::new("other/file"), "fixture").is_err());
    }

    #[tokio::test]
    async fn offline_setup_fails_closed_when_profile_is_missing()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempdir()?;
        let store = ArtifactStore::new(directory.path())?;
        let result = store.setup(true).await;
        assert!(result.is_err());
        if let Err(error) = result {
            assert_eq!(
                error.to_string(),
                "offline setup requires a complete verified artifact profile"
            );
        }
        Ok(())
    }

    #[test]
    fn status_is_read_only_and_reports_missing_profile() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempdir()?;
        let root = directory.path().join("absent");
        let store = ArtifactStore::new(&root)?;
        let report = store.status()?;
        assert_eq!(report.status, "not-ready");
        assert!(report.artifacts.iter().all(|item| item.state == "missing"));
        assert!(!root.exists());
        Ok(())
    }

    #[test]
    fn recovery_preserves_invalid_artifacts() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempdir()?;
        let source = directory.path().join("invalid-object");
        let recovery = directory.path().join("recovery");
        fs::create_dir(&source)?;
        fs::write(source.join("evidence"), b"preserve me")?;

        recover_path(&source, &recovery, "object")?;
        assert!(!source.exists());
        let recovered: Vec<_> = fs::read_dir(&recovery)?.collect::<Result<_, _>>()?;
        assert_eq!(recovered.len(), 1);
        assert_eq!(
            fs::read(recovered[0].path().join("evidence"))?,
            b"preserve me"
        );
        Ok(())
    }

    fn fixture_spec(size: u64, sha256: String) -> ArtifactSpec {
        ArtifactSpec {
            id: "fixture".into(),
            kind: "runtime".into(),
            platforms: vec!["fixture".into()],
            url: "https://github.com/example/fixture".into(),
            size,
            sha256,
            archive_root: "fixture".into(),
            max_unpacked_bytes: 1024,
            required_paths: vec!["required.bin".into()],
            licenses: vec![],
        }
    }
}
