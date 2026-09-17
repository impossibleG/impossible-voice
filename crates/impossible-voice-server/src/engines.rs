//! Read-only artifact inspection and native Voice engine composition.

use std::{error::Error, fmt, path::Path};

use impossible_voice_artifacts::{ArtifactStore, SetupReport};
use impossible_voice_audio::VadConfig;
use impossible_voice_sherpa_sys::SherpaRuntime;
use impossible_voice_stt::{SttEngine, SttLimits};
use impossible_voice_tts::{TtsEngine, TtsLimits};
use serde::Serialize;

/// Sanitized engine composition failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineLoadError(&'static str);

impl EngineLoadError {
    const fn new(message: &'static str) -> Self {
        Self(message)
    }
}

impl fmt::Display for EngineLoadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}

impl Error for EngineLoadError {}

/// Privacy-safe read-only engine inspection result.
#[derive(Debug, Clone, Serialize)]
pub struct EngineInspection {
    status: &'static str,
    artifacts: SetupReport,
    native_runtime: &'static str,
    speech_to_text: &'static str,
    text_to_speech: &'static str,
    downloads_attempted: bool,
}

impl EngineInspection {
    /// Whether artifacts and both native engines are loadable.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.status == "ready"
    }
}

/// Fully loaded local STT and TTS engines.
#[derive(Debug, Clone)]
pub struct VoiceEngines {
    stt: SttEngine,
    tts: TtsEngine,
}

impl VoiceEngines {
    /// Verifies installed artifacts and loads both pinned CPU engines without downloading.
    ///
    /// # Errors
    /// Returns a sanitized error if verification, native loading, or model initialization fails.
    pub fn load(root: &Path, max_concurrent: usize) -> Result<Self, EngineLoadError> {
        if max_concurrent == 0 {
            return Err(EngineLoadError::new("engine concurrency is invalid"));
        }
        let store = ArtifactStore::new(root)
            .map_err(|_| EngineLoadError::new("artifact inspection initialization failed"))?;
        let profile = store
            .verified_profile()
            .map_err(|_| EngineLoadError::new("voice artifacts are not verified"))?;
        let runtime = SherpaRuntime::load(profile.runtime_root())
            .map_err(|_| EngineLoadError::new("native voice runtime is not loadable"))?;
        let recognizer = runtime
            .create_nemo_recognizer(&profile.stt_model(), &profile.stt_tokens(), 2)
            .map_err(|_| EngineLoadError::new("speech recognition model is not loadable"))?;
        let synthesizer = runtime
            .create_vits_tts(
                &profile.tts_model(),
                &profile.tts_tokens(),
                &profile.tts_data_dir(),
                2,
            )
            .map_err(|_| EngineLoadError::new("speech synthesis model is not loadable"))?;
        let stt = SttEngine::new(
            recognizer,
            SttLimits {
                max_concurrent,
                ..SttLimits::default()
            },
            VadConfig::default(),
        )
        .map_err(|_| EngineLoadError::new("speech recognition policy is invalid"))?;
        let tts = TtsEngine::new(
            synthesizer,
            TtsLimits {
                max_concurrent,
                ..TtsLimits::default()
            },
        )
        .map_err(|_| EngineLoadError::new("speech synthesis policy is invalid"))?;
        Ok(Self { stt, tts })
    }

    /// Loaded speech-recognition engine.
    #[must_use]
    pub const fn stt(&self) -> &SttEngine {
        &self.stt
    }

    /// Loaded speech-synthesis engine.
    #[must_use]
    pub const fn tts(&self) -> &TtsEngine {
        &self.tts
    }
}

/// Inspects artifact verification and engine loadability without downloads or writes.
///
/// # Errors
/// Returns a sanitized error only if read-only artifact inspection cannot be initialized.
pub fn inspect(root: &Path) -> Result<EngineInspection, EngineLoadError> {
    let store = ArtifactStore::new(root)
        .map_err(|_| EngineLoadError::new("artifact inspection initialization failed"))?;
    let artifacts = store
        .status()
        .map_err(|_| EngineLoadError::new("artifact status is unavailable"))?;
    if artifacts.status != "ready" {
        return Ok(EngineInspection {
            status: "not-ready",
            artifacts,
            native_runtime: "not-checked",
            speech_to_text: "not-checked",
            text_to_speech: "not-checked",
            downloads_attempted: false,
        });
    }
    let Ok(profile) = store.verified_profile() else {
        return Ok(not_loadable(
            artifacts,
            "not-loadable",
            "not-checked",
            "not-checked",
        ));
    };
    let Ok(runtime) = SherpaRuntime::load(profile.runtime_root()) else {
        return Ok(not_loadable(
            artifacts,
            "not-loadable",
            "not-checked",
            "not-checked",
        ));
    };
    if runtime
        .create_nemo_recognizer(&profile.stt_model(), &profile.stt_tokens(), 2)
        .is_err()
    {
        return Ok(not_loadable(
            artifacts,
            "loadable",
            "not-loadable",
            "not-checked",
        ));
    }
    if runtime
        .create_vits_tts(
            &profile.tts_model(),
            &profile.tts_tokens(),
            &profile.tts_data_dir(),
            2,
        )
        .is_err()
    {
        return Ok(not_loadable(
            artifacts,
            "loadable",
            "loadable",
            "not-loadable",
        ));
    }
    Ok(EngineInspection {
        status: "ready",
        artifacts,
        native_runtime: "loadable",
        speech_to_text: "loadable",
        text_to_speech: "loadable",
        downloads_attempted: false,
    })
}

fn not_loadable(
    artifacts: SetupReport,
    native_runtime: &'static str,
    speech_to_text: &'static str,
    text_to_speech: &'static str,
) -> EngineInspection {
    EngineInspection {
        status: "not-ready",
        artifacts,
        native_runtime,
        speech_to_text,
        text_to_speech,
        downloads_attempted: false,
    }
}

#[cfg(test)]
mod tests {
    use std::{env, path::Path, time::Duration};

    use impossible_server_core::{CancellationToken, RequestContext, RequestIdSource};
    use tempfile::tempdir;

    use super::{VoiceEngines, inspect};

    #[test]
    fn missing_profile_is_not_ready_without_creating_artifacts()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempdir()?;
        let inspection = inspect(directory.path())?;
        assert!(!inspection.is_ready());
        assert_eq!(directory.path().read_dir()?.count(), 0);
        Ok(())
    }

    #[test]
    #[ignore = "requires the ignored locally installed curated artifact profile"]
    fn real_engines_synthesize_and_transcribe() -> Result<(), Box<dyn std::error::Error>> {
        let root = env::var_os("IMPOSSIBLE_VOICE_TEST_ARTIFACT_ROOT")
            .ok_or("real artifact root is not configured")?;
        let engines = VoiceEngines::load(Path::new(&root), 1)?;
        let context = RequestContext::new(
            RequestIdSource::default().next()?,
            CancellationToken::new(),
            Some(Duration::from_secs(120)),
        )?;
        let speech = engines
            .tts()
            .synthesize("A short voice test.", 1.0, &context)?;
        assert!(!speech.audio().samples().is_empty());
        assert!(
            !engines
                .stt()
                .transcribe(speech.audio(), &context)?
                .is_empty()
        );
        Ok(())
    }
}
