//! Bounded local speech-to-text execution over the pinned sherpa-onnx recognizer.

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
use impossible_voice_audio::{
    AudioLimits, EnergyVad, IncrementalPcm16, MonoPcm, StreamingResampler, VadConfig, VadEvent,
};
use impossible_voice_sherpa_sys::{OnlineRecognizer, OnlineStream};

const ENGINE_SAMPLE_RATE: u32 = 16_000;
const VAD_FRAME_SAMPLES: usize = 320;

/// Sanitized speech-recognition failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SttError {
    /// Input or configuration was invalid.
    InvalidInput,
    /// The bounded execution capacity is occupied.
    Busy,
    /// The caller or server cancelled the operation.
    Cancelled,
    /// The operation exhausted its deadline.
    DeadlineExceeded,
    /// Audio decoding or conversion failed.
    Audio,
    /// Native recognition failed without exposing private runtime details.
    Engine,
}

impl fmt::Display for SttError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidInput => "speech recognition input is invalid",
            Self::Busy => "speech recognition capacity is busy",
            Self::Cancelled => "speech recognition was cancelled",
            Self::DeadlineExceeded => "speech recognition deadline was exceeded",
            Self::Audio => "speech recognition audio is invalid",
            Self::Engine => "speech recognition engine failed",
        })
    }
}

impl Error for SttError {}

/// Bounded speech-recognition resource policy.
#[derive(Debug, Clone, Copy)]
pub struct SttLimits {
    /// Maximum concurrent batch requests and live streaming sessions.
    pub max_concurrent: usize,
    /// Maximum bytes accepted in one streaming frame.
    pub max_chunk_bytes: usize,
    /// Total encoded-byte and duration bound for one stream or batch request.
    pub audio: AudioLimits,
}

impl Default for SttLimits {
    fn default() -> Self {
        Self {
            max_concurrent: 2,
            max_chunk_bytes: 64 * 1024,
            audio: AudioLimits::default(),
        }
    }
}

/// Kind of transcript update produced by a streaming session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptKind {
    /// A replaceable partial hypothesis.
    Interim,
    /// An utterance closed by VAD or explicit end-of-input.
    Final,
}

/// Owned streaming transcript update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transcript {
    /// Whether the text is interim or final.
    pub kind: TranscriptKind,
    /// Recognized UTF-8 text.
    pub text: String,
}

trait BackendStream: Send {
    fn accept(&mut self, samples: &[f32]) -> Result<(), SttError>;
    fn interim(&self) -> Result<String, SttError>;
    fn finish(&mut self) -> Result<String, SttError>;
}

trait Backend: Send + Sync {
    fn stream(&self) -> Result<Box<dyn BackendStream>, SttError>;
}

#[derive(Debug)]
struct NativeBackend(OnlineRecognizer);

impl Backend for NativeBackend {
    fn stream(&self) -> Result<Box<dyn BackendStream>, SttError> {
        self.0
            .create_stream()
            .map(|stream| Box::new(NativeStream(stream)) as Box<dyn BackendStream>)
            .map_err(|_| SttError::Engine)
    }
}

struct NativeStream(OnlineStream);

impl BackendStream for NativeStream {
    fn accept(&mut self, samples: &[f32]) -> Result<(), SttError> {
        self.0.accept(samples).map_err(|_| SttError::Engine)
    }

    fn interim(&self) -> Result<String, SttError> {
        self.0.text().map_err(|_| SttError::Engine)
    }

    fn finish(&mut self) -> Result<String, SttError> {
        self.0.finish().map_err(|_| SttError::Engine)
    }
}

#[derive(Debug)]
struct Limiter {
    active: AtomicUsize,
    maximum: usize,
}

impl Limiter {
    fn acquire(self: &Arc<Self>) -> Result<Permit, SttError> {
        self.active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                (active < self.maximum).then_some(active + 1)
            })
            .map_err(|_| SttError::Busy)?;
        Ok(Permit(Arc::clone(self)))
    }
}

struct Permit(Arc<Limiter>);

impl Drop for Permit {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Thread-safe local speech-recognition engine.
#[derive(Clone)]
pub struct SttEngine {
    backend: Arc<dyn Backend>,
    limiter: Arc<Limiter>,
    limits: SttLimits,
    vad: VadConfig,
}

impl fmt::Debug for SttEngine {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("SttEngine").finish_non_exhaustive()
    }
}

impl SttEngine {
    /// Wraps a loaded pinned native recognizer with bounded product policy.
    ///
    /// # Errors
    /// Rejects zero resource limits or invalid VAD configuration.
    pub fn new(
        recognizer: OnlineRecognizer,
        limits: SttLimits,
        vad: VadConfig,
    ) -> Result<Self, SttError> {
        Self::with_backend(Arc::new(NativeBackend(recognizer)), limits, vad)
    }

    fn with_backend(
        backend: Arc<dyn Backend>,
        limits: SttLimits,
        vad: VadConfig,
    ) -> Result<Self, SttError> {
        if limits.max_concurrent == 0 || limits.max_chunk_bytes == 0 {
            return Err(SttError::InvalidInput);
        }
        EnergyVad::new(vad, ENGINE_SAMPLE_RATE).map_err(|_| SttError::InvalidInput)?;
        Ok(Self {
            backend,
            limiter: Arc::new(Limiter {
                active: AtomicUsize::new(0),
                maximum: limits.max_concurrent,
            }),
            limits,
            vad,
        })
    }

    /// Transcribes a complete validated mono recording.
    ///
    /// This is the non-streaming product operation; it shares the pinned streaming model and
    /// returns only its final result.
    ///
    /// # Errors
    /// Returns a sanitized bound, cancellation, audio, or native engine error.
    pub fn transcribe(
        &self,
        audio: &MonoPcm,
        context: &RequestContext,
    ) -> Result<String, SttError> {
        ensure_active(context)?;
        let _permit = self.limiter.acquire()?;
        self.limits
            .audio
            .validate_pcm(audio)
            .map_err(|_| SttError::Audio)?;
        let converted = audio
            .resample(ENGINE_SAMPLE_RATE)
            .map_err(|_| SttError::Audio)?;
        let mut stream = self.backend.stream()?;
        stream.accept(converted.samples())?;
        ensure_active(context)?;
        let text = stream.finish()?;
        ensure_active(context)?;
        Ok(text)
    }

    /// Opens a bounded incremental raw-PCM16 session at a supported source rate.
    ///
    /// # Errors
    /// Returns a sanitized bound, cancellation, audio, or native engine error.
    pub fn start_stream(
        &self,
        source_sample_rate: u32,
        context: RequestContext,
    ) -> Result<StreamingSession, SttError> {
        ensure_active(&context)?;
        let permit = self.limiter.acquire()?;
        Ok(StreamingSession {
            stream: self.backend.stream()?,
            backend: Arc::clone(&self.backend),
            decoder: IncrementalPcm16::new(source_sample_rate, self.limits.audio)
                .map_err(|_| SttError::Audio)?,
            resampler: StreamingResampler::new(source_sample_rate, ENGINE_SAMPLE_RATE)
                .map_err(|_| SttError::Audio)?,
            vad: EnergyVad::new(self.vad, ENGINE_SAMPLE_RATE).map_err(|_| SttError::Audio)?,
            context,
            max_chunk_bytes: self.limits.max_chunk_bytes,
            residual: Vec::with_capacity(VAD_FRAME_SAMPLES),
            last_interim: String::new(),
            last_final: None,
            current_has_audio: false,
            finished: false,
            _permit: permit,
        })
    }
}

/// One bounded incremental raw-PCM16 recognition session.
pub struct StreamingSession {
    stream: Box<dyn BackendStream>,
    backend: Arc<dyn Backend>,
    decoder: IncrementalPcm16,
    resampler: StreamingResampler,
    vad: EnergyVad,
    context: RequestContext,
    max_chunk_bytes: usize,
    residual: Vec<f32>,
    last_interim: String,
    last_final: Option<String>,
    current_has_audio: bool,
    finished: bool,
    _permit: Permit,
}

impl fmt::Debug for StreamingSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StreamingSession")
            .field("finished", &self.finished)
            .finish_non_exhaustive()
    }
}

impl StreamingSession {
    /// Accepts one arbitrary raw little-endian PCM16 byte frame.
    ///
    /// VAD emits final utterance boundaries; interim hypotheses are replaceable. The decoder and
    /// resampler retain only data inside the configured total stream bound.
    ///
    /// # Errors
    /// Rejects oversized frames, stopped/finished sessions, invalid PCM, or native failures.
    pub fn push_pcm16(&mut self, bytes: &[u8]) -> Result<Vec<Transcript>, SttError> {
        if self.finished || bytes.is_empty() || bytes.len() > self.max_chunk_bytes {
            return Err(SttError::InvalidInput);
        }
        ensure_active(&self.context)?;
        let decoded = self.decoder.push(bytes).map_err(|_| SttError::Audio)?;
        if decoded.is_empty() {
            return Ok(Vec::new());
        }
        let converted = self.resampler.push(&decoded).map_err(|_| SttError::Audio)?;
        self.residual.extend_from_slice(&converted);
        let complete = (self.residual.len() / VAD_FRAME_SAMPLES) * VAD_FRAME_SAMPLES;
        let frames = self.residual.drain(..complete).collect::<Vec<_>>();
        let mut updates = Vec::new();
        for frame in frames.chunks_exact(VAD_FRAME_SAMPLES) {
            ensure_active(&self.context)?;
            self.stream.accept(frame)?;
            self.current_has_audio = true;
            let transition = self.vad.process(frame).map_err(|_| SttError::Audio)?;
            if transition == VadEvent::Ended {
                let text = self.stream.finish()?;
                ensure_active(&self.context)?;
                self.last_final = Some(text.clone());
                updates.push(Transcript {
                    kind: TranscriptKind::Final,
                    text,
                });
                self.stream = self.backend.stream()?;
                self.current_has_audio = false;
                self.last_interim.clear();
            } else {
                let text = self.stream.interim()?;
                if !text.is_empty() && text != self.last_interim {
                    self.last_interim.clone_from(&text);
                    updates.push(Transcript {
                        kind: TranscriptKind::Interim,
                        text,
                    });
                }
            }
        }
        ensure_active(&self.context)?;
        Ok(updates)
    }

    /// Closes input and returns the last final result.
    ///
    /// # Errors
    /// Rejects repeated finish, partial/empty PCM, stopped sessions, or native failures.
    pub fn finish(mut self) -> Result<Transcript, SttError> {
        if self.finished {
            return Err(SttError::InvalidInput);
        }
        ensure_active(&self.context)?;
        self.decoder.finish().map_err(|_| SttError::Audio)?;
        if !self.residual.is_empty() {
            self.stream.accept(&self.residual)?;
            self.current_has_audio = true;
            self.vad
                .process(&self.residual)
                .map_err(|_| SttError::Audio)?;
            self.residual.clear();
        }
        self.finished = true;
        let text = if self.current_has_audio {
            self.stream.finish()?
        } else {
            self.last_final.take().ok_or(SttError::Audio)?
        };
        ensure_active(&self.context)?;
        Ok(Transcript {
            kind: TranscriptKind::Final,
            text,
        })
    }
}

fn ensure_active(context: &RequestContext) -> Result<(), SttError> {
    if context.cancellation().is_cancelled() {
        Err(SttError::Cancelled)
    } else if context.remaining() == Some(Duration::ZERO) {
        Err(SttError::DeadlineExceeded)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use impossible_server_core::{CancellationToken, RequestIdSource};

    use super::*;

    struct FakeBackend {
        streams: Mutex<usize>,
    }

    impl Backend for FakeBackend {
        fn stream(&self) -> Result<Box<dyn BackendStream>, SttError> {
            let mut streams = self.streams.lock().map_err(|_| SttError::Engine)?;
            *streams += 1;
            Ok(Box::new(FakeStream {
                accepted: 0,
                finished: false,
            }))
        }
    }

    struct FakeStream {
        accepted: usize,
        finished: bool,
    }

    impl BackendStream for FakeStream {
        fn accept(&mut self, samples: &[f32]) -> Result<(), SttError> {
            self.accepted += samples.len();
            Ok(())
        }

        fn interim(&self) -> Result<String, SttError> {
            Ok(format!("partial-{}", self.accepted))
        }

        fn finish(&mut self) -> Result<String, SttError> {
            if self.finished {
                return Err(SttError::Engine);
            }
            self.finished = true;
            Ok(format!("final-{}", self.accepted))
        }
    }

    fn context(token: CancellationToken) -> Result<RequestContext, Box<dyn Error>> {
        Ok(RequestContext::new(
            RequestIdSource::default().next()?,
            token,
            Some(Duration::from_secs(5)),
        )?)
    }

    fn engine(max_concurrent: usize) -> Result<SttEngine, SttError> {
        SttEngine::with_backend(
            Arc::new(FakeBackend {
                streams: Mutex::new(0),
            }),
            SttLimits {
                max_concurrent,
                ..SttLimits::default()
            },
            VadConfig {
                rms_threshold: 0.01,
                min_speech_ms: 20,
                trailing_silence_ms: 20,
                max_utterance_ms: 1_000,
            },
        )
    }

    #[test]
    fn batch_resamples_and_returns_only_final_text() -> Result<(), Box<dyn Error>> {
        let engine = engine(1)?;
        let audio = MonoPcm::new(8_000, vec![0.25; 800])?;
        let text = engine.transcribe(&audio, &context(CancellationToken::new())?)?;
        assert_eq!(text, "final-1599");
        Ok(())
    }

    #[test]
    fn streaming_emits_interim_and_vad_final_results() -> Result<(), Box<dyn Error>> {
        let engine = engine(1)?;
        let mut session = engine.start_stream(16_000, context(CancellationToken::new())?)?;
        let speech = vec![2_000_i16; VAD_FRAME_SAMPLES]
            .into_iter()
            .flat_map(i16::to_le_bytes)
            .collect::<Vec<_>>();
        let silence = vec![0_i16; VAD_FRAME_SAMPLES]
            .into_iter()
            .flat_map(i16::to_le_bytes)
            .collect::<Vec<_>>();
        assert!(
            session
                .push_pcm16(&speech)?
                .iter()
                .any(|update| update.kind == TranscriptKind::Interim)
        );
        let updates = session.push_pcm16(&silence)?;
        let vad_final = updates
            .iter()
            .find(|update| update.kind == TranscriptKind::Final)
            .ok_or("missing VAD final")?;
        assert_eq!(vad_final.text, "final-640");
        let explicit_final = session.finish()?;
        assert_eq!(explicit_final.kind, TranscriptKind::Final);
        assert_eq!(explicit_final.text, vad_final.text);
        assert_ne!(explicit_final.text, "final-0");
        Ok(())
    }

    #[test]
    fn streaming_results_are_invariant_to_transport_chunk_splits() -> Result<(), Box<dyn Error>> {
        let pcm = [
            vec![2_000_i16; VAD_FRAME_SAMPLES],
            vec![0_i16; VAD_FRAME_SAMPLES],
        ]
        .concat()
        .into_iter()
        .flat_map(i16::to_le_bytes)
        .collect::<Vec<_>>();
        let contiguous = run_stream(&pcm, &[pcm.len()])?;
        let fragmented = run_stream(&pcm, &[1, 3, 17, 129, 5, 511, 7])?;
        assert_eq!(fragmented, contiguous);
        Ok(())
    }

    fn run_stream(
        pcm: &[u8],
        chunk_sizes: &[usize],
    ) -> Result<(Vec<Transcript>, Transcript), Box<dyn Error>> {
        let engine = engine(1)?;
        let mut session = engine.start_stream(16_000, context(CancellationToken::new())?)?;
        let mut updates = Vec::new();
        let mut offset = 0;
        let mut index = 0;
        while offset < pcm.len() {
            let size = chunk_sizes[index % chunk_sizes.len()].min(pcm.len() - offset);
            updates.extend(session.push_pcm16(&pcm[offset..offset + size])?);
            offset += size;
            index += 1;
        }
        let final_result = session.finish()?;
        Ok((updates, final_result))
    }

    #[test]
    fn cancellation_and_concurrency_are_fenced() -> Result<(), Box<dyn Error>> {
        let engine = engine(1)?;
        let token = CancellationToken::new();
        let session = engine.start_stream(16_000, context(token.clone())?)?;
        assert!(matches!(
            engine.start_stream(16_000, context(CancellationToken::new())?),
            Err(SttError::Busy)
        ));
        assert!(token.cancel());
        let audio = MonoPcm::new(16_000, vec![0.0; 320])?;
        assert_eq!(
            engine.transcribe(&audio, &context(token)?),
            Err(SttError::Cancelled)
        );
        drop(session);
        assert!(
            engine
                .start_stream(16_000, context(CancellationToken::new())?)
                .is_ok()
        );
        Ok(())
    }

    #[test]
    fn partial_pcm_and_oversized_chunks_fail_closed() -> Result<(), Box<dyn Error>> {
        let engine = engine(1)?;
        let mut session = engine.start_stream(16_000, context(CancellationToken::new())?)?;
        assert_eq!(
            session.push_pcm16(&vec![0; 64 * 1024 + 1]),
            Err(SttError::InvalidInput)
        );
        session.push_pcm16(&[0])?;
        assert_eq!(session.finish(), Err(SttError::Audio));
        Ok(())
    }
}
