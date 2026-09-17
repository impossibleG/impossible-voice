//! Bounded local text-to-speech execution over the pinned Kristin VITS voice.

use std::{
    error::Error,
    fmt,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use impossible_server_core::RequestContext;
use impossible_voice_audio::MonoPcm;
use impossible_voice_sherpa_sys::{GeneratedAudio, OfflineTts};

/// Sanitized speech-synthesis failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TtsError {
    /// Input or configuration was invalid.
    InvalidInput,
    /// The bounded execution capacity is occupied.
    Busy,
    /// The caller or server cancelled the operation.
    Cancelled,
    /// The operation exhausted its deadline.
    DeadlineExceeded,
    /// The native synthesizer failed without exposing private runtime details.
    Engine,
    /// Native output was invalid or outside the product bound.
    Output,
}

impl fmt::Display for TtsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidInput => "speech synthesis input is invalid",
            Self::Busy => "speech synthesis capacity is busy",
            Self::Cancelled => "speech synthesis was cancelled",
            Self::DeadlineExceeded => "speech synthesis deadline was exceeded",
            Self::Engine => "speech synthesis engine failed",
            Self::Output => "speech synthesis output is invalid",
        })
    }
}

impl Error for TtsError {}

/// Bounded speech-synthesis resource policy.
#[derive(Debug, Clone, Copy)]
pub struct TtsLimits {
    /// Maximum simultaneous native synthesis calls.
    pub max_concurrent: usize,
    /// Maximum UTF-8 request length.
    pub max_text_bytes: usize,
    /// Maximum samples copied from the native runtime.
    pub max_output_samples: usize,
    /// Maximum samples exposed in one post-synthesis transport chunk.
    pub max_chunk_samples: usize,
}

impl Default for TtsLimits {
    fn default() -> Self {
        Self {
            max_concurrent: 1,
            max_text_bytes: 4_096,
            max_output_samples: 22_050 * 5 * 60,
            max_chunk_samples: 22_050,
        }
    }
}

trait Backend: Send + Sync {
    fn generate(
        &self,
        text: &str,
        speed: f32,
        max_samples: usize,
    ) -> Result<GeneratedAudio, TtsError>;
}

struct NativeBackend(OfflineTts);

impl Backend for NativeBackend {
    fn generate(
        &self,
        text: &str,
        speed: f32,
        max_samples: usize,
    ) -> Result<GeneratedAudio, TtsError> {
        self.0
            .generate(text, speed, max_samples)
            .map_err(|_| TtsError::Engine)
    }
}

#[derive(Debug)]
struct Limiter {
    active: AtomicUsize,
    maximum: usize,
}

impl Limiter {
    fn acquire(self: &Arc<Self>) -> Result<Permit, TtsError> {
        self.active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                (active < self.maximum).then_some(active + 1)
            })
            .map_err(|_| TtsError::Busy)?;
        Ok(Permit(Arc::clone(self)))
    }
}

struct Permit(Arc<Limiter>);

impl Drop for Permit {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::AcqRel);
    }
}

/// One bounded chunk cut from a completed synthesis result.
///
/// These are transport chunks produced after full native synthesis, not incremental synthesis.
#[derive(Debug, Clone, PartialEq)]
pub struct TransportChunk {
    /// Normalized mono samples.
    pub samples: Vec<f32>,
    /// Whether this is the final chunk of the completed result.
    pub final_chunk: bool,
}

/// Completed owned speech-synthesis result.
#[derive(Debug, Clone, PartialEq)]
pub struct Synthesis {
    audio: MonoPcm,
    max_chunk_samples: usize,
}

impl Synthesis {
    /// Creates a completed synthesis result from validated mono PCM.
    ///
    /// This constructor supports alternate local backends and deterministic transport tests.
    ///
    /// # Errors
    /// Rejects a zero transport chunk bound.
    pub fn from_audio(audio: MonoPcm, max_chunk_samples: usize) -> Result<Self, TtsError> {
        if max_chunk_samples == 0 {
            return Err(TtsError::InvalidInput);
        }
        Ok(Self {
            audio,
            max_chunk_samples,
        })
    }

    /// Validated synthesized mono PCM.
    #[must_use]
    pub const fn audio(&self) -> &MonoPcm {
        &self.audio
    }

    /// Encodes the completed result as raw little-endian PCM16.
    #[must_use]
    pub fn pcm16(&self) -> Vec<u8> {
        self.audio.to_pcm16_le()
    }

    /// Encodes the completed result as a mono PCM16 RIFF/WAVE file.
    ///
    /// # Errors
    /// Returns a sanitized error if RIFF lengths cannot represent the output.
    pub fn wav(&self) -> Result<Vec<u8>, TtsError> {
        self.audio.to_wav_pcm16().map_err(|_| TtsError::Output)
    }

    /// Splits completed PCM into bounded transport chunks.
    ///
    /// This does not make the pinned VITS runtime incremental: native synthesis finishes before
    /// the first chunk exists. Cancellation and disconnect fencing prevents publication afterward.
    #[must_use]
    pub fn transport_chunks(&self) -> Vec<TransportChunk> {
        let count = self.audio.samples().len().div_ceil(self.max_chunk_samples);
        self.audio
            .samples()
            .chunks(self.max_chunk_samples)
            .enumerate()
            .map(|(index, samples)| TransportChunk {
                samples: samples.to_vec(),
                final_chunk: index + 1 == count,
            })
            .collect()
    }
}

/// Thread-safe local speech-synthesis engine.
#[derive(Clone)]
pub struct TtsEngine {
    backend: Arc<dyn Backend>,
    limiter: Arc<Limiter>,
    limits: TtsLimits,
}

impl fmt::Debug for TtsEngine {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("TtsEngine").finish_non_exhaustive()
    }
}

impl TtsEngine {
    /// Wraps a loaded pinned native Kristin synthesizer with bounded product policy.
    ///
    /// # Errors
    /// Rejects zero resource bounds.
    pub fn new(synthesizer: OfflineTts, limits: TtsLimits) -> Result<Self, TtsError> {
        Self::with_backend(Arc::new(NativeBackend(synthesizer)), limits)
    }

    fn with_backend(backend: Arc<dyn Backend>, limits: TtsLimits) -> Result<Self, TtsError> {
        if limits.max_concurrent == 0
            || limits.max_text_bytes == 0
            || limits.max_output_samples == 0
            || limits.max_chunk_samples == 0
        {
            return Err(TtsError::InvalidInput);
        }
        Ok(Self {
            backend,
            limiter: Arc::new(Limiter {
                active: AtomicUsize::new(0),
                maximum: limits.max_concurrent,
            }),
            limits,
        })
    }

    /// Synthesizes one bounded UTF-8 text using the pinned English Kristin voice.
    ///
    /// The native call is not interruptible. Cancellation, deadline expiry, or disconnect during
    /// that call fences the completed audio so it is never returned to a transport.
    ///
    /// # Errors
    /// Returns a sanitized input, capacity, cancellation, deadline, output, or native error.
    pub fn synthesize(
        &self,
        text: &str,
        speed: f32,
        context: &RequestContext,
    ) -> Result<Synthesis, TtsError> {
        if text.trim().is_empty()
            || text.len() > self.limits.max_text_bytes
            || text.contains('\0')
            || !speed.is_finite()
            || !(0.5..=2.0).contains(&speed)
        {
            return Err(TtsError::InvalidInput);
        }
        ensure_active(context)?;
        let _permit = self.limiter.acquire()?;
        let generated = self
            .backend
            .generate(text, speed, self.limits.max_output_samples)?;
        ensure_active(context)?;
        if generated.samples.len() > self.limits.max_output_samples {
            return Err(TtsError::Output);
        }
        let audio =
            MonoPcm::new(generated.sample_rate, generated.samples).map_err(|_| TtsError::Output)?;
        Ok(Synthesis {
            audio,
            max_chunk_samples: self.limits.max_chunk_samples,
        })
    }
}

fn ensure_active(context: &RequestContext) -> Result<(), TtsError> {
    if context.cancellation().is_cancelled() {
        Err(TtsError::Cancelled)
    } else if context.remaining() == Some(Duration::ZERO) {
        Err(TtsError::DeadlineExceeded)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use impossible_server_core::{CancellationToken, RequestIdSource};

    use super::*;

    struct FakeBackend {
        samples: usize,
    }

    impl Backend for FakeBackend {
        fn generate(
            &self,
            _text: &str,
            _speed: f32,
            max_samples: usize,
        ) -> Result<GeneratedAudio, TtsError> {
            if self.samples > max_samples {
                return Err(TtsError::Output);
            }
            Ok(GeneratedAudio {
                samples: vec![0.25; self.samples],
                sample_rate: 22_050,
            })
        }
    }

    fn context(token: CancellationToken) -> Result<RequestContext, Box<dyn Error>> {
        Ok(RequestContext::new(
            RequestIdSource::default().next()?,
            token,
            Some(Duration::from_secs(5)),
        )?)
    }

    fn engine(samples: usize) -> Result<TtsEngine, TtsError> {
        TtsEngine::with_backend(
            Arc::new(FakeBackend { samples }),
            TtsLimits {
                max_chunk_samples: 3,
                max_output_samples: 10,
                ..TtsLimits::default()
            },
        )
    }

    #[test]
    fn synthesis_produces_valid_wav_and_post_synthesis_chunks() -> Result<(), Box<dyn Error>> {
        let synthesis = engine(7)?.synthesize("hello", 1.0, &context(CancellationToken::new())?)?;
        assert_eq!(synthesis.wav()?[..4], *b"RIFF");
        let chunks = synthesis.transport_chunks();
        assert_eq!(
            chunks
                .iter()
                .map(|chunk| chunk.samples.len())
                .sum::<usize>(),
            7
        );
        assert_eq!(chunks.iter().filter(|chunk| chunk.final_chunk).count(), 1);
        assert!(chunks.last().is_some_and(|chunk| chunk.final_chunk));
        Ok(())
    }

    #[test]
    fn text_output_and_capacity_limits_fail_closed() -> Result<(), Box<dyn Error>> {
        let engine = engine(11)?;
        let context = context(CancellationToken::new())?;
        assert_eq!(
            engine.synthesize("", 1.0, &context),
            Err(TtsError::InvalidInput)
        );
        assert_eq!(
            engine.synthesize("hello", 1.0, &context),
            Err(TtsError::Output)
        );
        let permit = engine.limiter.acquire()?;
        assert_eq!(
            engine.synthesize("hello", 1.0, &context),
            Err(TtsError::Busy)
        );
        drop(permit);
        Ok(())
    }

    #[test]
    fn cancellation_prevents_native_work_and_publication() -> Result<(), Box<dyn Error>> {
        let engine = engine(2)?;
        let token = CancellationToken::new();
        assert!(token.cancel());
        assert_eq!(
            engine.synthesize("hello", 1.0, &context(token)?),
            Err(TtsError::Cancelled)
        );
        Ok(())
    }
}
