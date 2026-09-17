//! Bounded HTTP and WebSocket Voice API transports.

use std::{fmt, sync::Arc, time::Duration};

use axum::{
    Extension, Json, Router,
    body::Bytes,
    extract::{
        Multipart, State, WebSocketUpgrade,
        ws::{CloseFrame, Message, WebSocket},
    },
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use impossible_server_core::{CancellationToken, RequestContext, ServerLimits};
use impossible_voice_audio::{AudioLimits, MonoPcm};
use impossible_voice_stt::{StreamingSession, Transcript, TranscriptKind};
use impossible_voice_tts::Synthesis;
use serde::{Deserialize, Serialize};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

const MAX_MULTIPART_FIELD_BYTES: usize = 64 * 1024;
const MAX_WS_MESSAGE_BYTES: usize = 64 * 1024;
const MAX_WS_SESSION_SECONDS: u64 = 5 * 60;
const TTS_CHUNK_BYTES: usize = 32 * 1024;

/// Sanitized error returned by a local Voice backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceBackendError {
    /// The request is invalid.
    InvalidInput,
    /// The bounded engine capacity is occupied.
    Busy,
    /// The request was cancelled or disconnected.
    Cancelled,
    /// The request deadline elapsed.
    DeadlineExceeded,
    /// A local engine failed.
    Engine,
}

/// Incremental STT operation used by realtime transports.
pub trait RealtimeTranscriber: Send {
    /// Appends one raw little-endian PCM16 frame.
    ///
    /// # Errors
    /// Returns a sanitized input, capacity, cancellation, deadline, or engine failure.
    fn push_pcm16(&mut self, bytes: &[u8]) -> Result<Vec<Transcript>, VoiceBackendError>;
    /// Commits the remaining audio and returns a final transcript.
    ///
    /// # Errors
    /// Returns a sanitized input, cancellation, deadline, or engine failure.
    fn finish(self: Box<Self>) -> Result<Transcript, VoiceBackendError>;
}

impl RealtimeTranscriber for StreamingSession {
    fn push_pcm16(&mut self, bytes: &[u8]) -> Result<Vec<Transcript>, VoiceBackendError> {
        StreamingSession::push_pcm16(self, bytes).map_err(map_stt_error)
    }

    fn finish(self: Box<Self>) -> Result<Transcript, VoiceBackendError> {
        StreamingSession::finish(*self).map_err(map_stt_error)
    }
}

/// Transport-independent local Voice operation boundary.
pub trait VoiceBackend: Send + Sync + 'static {
    /// Transcribes a complete validated recording.
    ///
    /// # Errors
    /// Returns a sanitized input, capacity, cancellation, deadline, or engine failure.
    fn transcribe(
        &self,
        audio: &MonoPcm,
        context: &RequestContext,
    ) -> Result<String, VoiceBackendError>;

    /// Synthesizes a complete recording with the curated voice.
    ///
    /// # Errors
    /// Returns a sanitized input, capacity, cancellation, deadline, or engine failure.
    fn synthesize(
        &self,
        text: &str,
        speed: f32,
        context: &RequestContext,
    ) -> Result<Synthesis, VoiceBackendError>;

    /// Opens one bounded realtime STT session.
    ///
    /// # Errors
    /// Returns a sanitized input, capacity, cancellation, deadline, or engine failure.
    fn start_transcription(
        &self,
        sample_rate: u32,
        context: RequestContext,
    ) -> Result<Box<dyn RealtimeTranscriber>, VoiceBackendError>;
}

fn map_stt_error(error: impossible_voice_stt::SttError) -> VoiceBackendError {
    use impossible_voice_stt::SttError;
    match error {
        SttError::InvalidInput | SttError::Audio => VoiceBackendError::InvalidInput,
        SttError::Busy => VoiceBackendError::Busy,
        SttError::Cancelled => VoiceBackendError::Cancelled,
        SttError::DeadlineExceeded => VoiceBackendError::DeadlineExceeded,
        SttError::Engine => VoiceBackendError::Engine,
    }
}

pub(crate) fn map_tts_error(error: impossible_voice_tts::TtsError) -> VoiceBackendError {
    use impossible_voice_tts::TtsError;
    match error {
        TtsError::InvalidInput | TtsError::Output => VoiceBackendError::InvalidInput,
        TtsError::Busy => VoiceBackendError::Busy,
        TtsError::Cancelled => VoiceBackendError::Cancelled,
        TtsError::DeadlineExceeded => VoiceBackendError::DeadlineExceeded,
        TtsError::Engine => VoiceBackendError::Engine,
    }
}

#[derive(Clone)]
struct ApiState {
    backend: Arc<dyn VoiceBackend>,
    sessions: Arc<Semaphore>,
}

/// Builds versioned HTTP and WebSocket routes over a loaded local backend.
pub fn routes(backend: Arc<dyn VoiceBackend>, limits: ServerLimits) -> Router {
    let state = ApiState {
        backend: Arc::clone(&backend),
        sessions: Arc::new(Semaphore::new(limits.max_concurrent_requests())),
    };
    Router::new()
        .route("/v1/audio/transcriptions", post(openai_transcription))
        .route("/api/v1/transcriptions", post(native_transcription))
        .route("/v1/audio/speech", post(speech))
        .route("/api/v1/speech", post(speech))
        .route("/api/v1/realtime", get(realtime))
        .with_state(state)
        .merge(crate::mcp::routes(backend))
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    error: ErrorDetail,
}

#[derive(Debug, Serialize)]
struct ErrorDetail {
    code: &'static str,
    message: &'static str,
}

#[derive(Debug, Clone, Copy)]
struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: &'static str,
}

impl ApiError {
    const fn invalid(message: &'static str) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "invalid_request",
            message,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorBody {
                error: ErrorDetail {
                    code: self.code,
                    message: self.message,
                },
            }),
        )
            .into_response()
    }
}

impl From<VoiceBackendError> for ApiError {
    fn from(error: VoiceBackendError) -> Self {
        match error {
            VoiceBackendError::InvalidInput => Self::invalid("voice input is invalid"),
            VoiceBackendError::Busy => Self {
                status: StatusCode::TOO_MANY_REQUESTS,
                code: "overloaded",
                message: "voice capacity is busy",
            },
            VoiceBackendError::Cancelled => Self {
                status: StatusCode::SERVICE_UNAVAILABLE,
                code: "cancelled",
                message: "the request was cancelled",
            },
            VoiceBackendError::DeadlineExceeded => Self {
                status: StatusCode::GATEWAY_TIMEOUT,
                code: "deadline_exceeded",
                message: "the request deadline was exceeded",
            },
            VoiceBackendError::Engine => Self {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                code: "engine_error",
                message: "the local voice engine failed",
            },
        }
    }
}

#[derive(Debug, Serialize)]
struct TranscriptionResponse {
    text: String,
}

async fn native_transcription(
    State(state): State<ApiState>,
    Extension(context): Extension<RequestContext>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<TranscriptionResponse>, ApiError> {
    let audio = decode_http_audio(&headers, &body)?;
    transcribe(state, context, audio).await
}

async fn openai_transcription(
    State(state): State<ApiState>,
    Extension(context): Extension<RequestContext>,
    mut multipart: Multipart,
) -> Result<Json<TranscriptionResponse>, ApiError> {
    let mut audio = None;
    let mut sample_rate = None;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|_| ApiError::invalid("multipart body is malformed"))?
    {
        let name = field.name().unwrap_or_default().to_owned();
        let content_type = field.content_type().map(str::to_owned);
        let bytes = field
            .bytes()
            .await
            .map_err(|_| ApiError::invalid("multipart field is malformed"))?;
        match name.as_str() {
            "file" if audio.is_none() => {
                audio = Some((content_type, bytes));
            }
            "sample_rate" if bytes.len() <= 16 => {
                let text = std::str::from_utf8(&bytes)
                    .map_err(|_| ApiError::invalid("sample_rate is malformed"))?;
                sample_rate = Some(
                    text.parse::<u32>()
                        .map_err(|_| ApiError::invalid("sample_rate is malformed"))?,
                );
            }
            "model" if bytes.len() <= MAX_MULTIPART_FIELD_BYTES => {}
            _ => {
                return Err(ApiError::invalid(
                    "multipart field is unsupported or duplicated",
                ));
            }
        }
    }
    let (content_type, bytes) = audio.ok_or_else(|| ApiError::invalid("file is required"))?;
    let audio = decode_audio(
        content_type
            .as_deref()
            .unwrap_or("application/octet-stream"),
        sample_rate,
        &bytes,
    )?;
    transcribe(state, context, audio).await
}

async fn transcribe(
    state: ApiState,
    context: RequestContext,
    audio: MonoPcm,
) -> Result<Json<TranscriptionResponse>, ApiError> {
    let result = tokio::task::spawn_blocking(move || state.backend.transcribe(&audio, &context))
        .await
        .map_err(|_| ApiError::from(VoiceBackendError::Engine))??;
    Ok(Json(TranscriptionResponse { text: result }))
}

fn decode_http_audio(headers: &HeaderMap, bytes: &[u8]) -> Result<MonoPcm, ApiError> {
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| ApiError::invalid("content-type is required"))?;
    let sample_rate = headers
        .get("x-audio-sample-rate")
        .and_then(|value| value.to_str().ok())
        .map(str::parse::<u32>)
        .transpose()
        .map_err(|_| ApiError::invalid("sample rate is malformed"))?;
    decode_audio(content_type, sample_rate, bytes)
}

fn decode_audio(
    content_type: &str,
    explicit_rate: Option<u32>,
    bytes: &[u8],
) -> Result<MonoPcm, ApiError> {
    let media_type = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    if matches!(media_type.as_str(), "audio/wav" | "audio/x-wav") {
        if explicit_rate.is_some() {
            return Err(ApiError::invalid("WAV sample rate is embedded in the file"));
        }
        MonoPcm::from_wav(bytes, AudioLimits::default())
            .map_err(|_| ApiError::invalid("WAV audio is invalid"))
    } else if matches!(media_type.as_str(), "audio/pcm" | "audio/l16") {
        let parameter_rate = content_type.split(';').skip(1).find_map(|parameter| {
            let (key, value) = parameter.trim().split_once('=')?;
            key.eq_ignore_ascii_case("rate")
                .then(|| value.trim().parse::<u32>().ok())
                .flatten()
        });
        let rate = explicit_rate
            .or(parameter_rate)
            .ok_or_else(|| ApiError::invalid("raw PCM sample rate is required"))?;
        MonoPcm::from_pcm16_le(bytes, rate, AudioLimits::default())
            .map_err(|_| ApiError::invalid("raw PCM16 audio is invalid"))
    } else {
        Err(ApiError::invalid("audio content type is unsupported"))
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SpeechRequest {
    input: String,
    #[serde(default = "default_speed")]
    speed: f32,
    #[serde(default = "default_format")]
    response_format: String,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    voice: Option<String>,
}

const fn default_speed() -> f32 {
    1.0
}

fn default_format() -> String {
    "wav".to_owned()
}

async fn speech(
    State(state): State<ApiState>,
    Extension(context): Extension<RequestContext>,
    Json(request): Json<SpeechRequest>,
) -> Result<Response, ApiError> {
    if request
        .model
        .as_deref()
        .is_some_and(|model| model != "impossible-voice-tts")
        || request
            .voice
            .as_deref()
            .is_some_and(|voice| voice != "kristin")
    {
        return Err(ApiError::invalid("model or voice is unsupported"));
    }
    let format = request.response_format;
    if !matches!(format.as_str(), "wav" | "pcm") {
        return Err(ApiError::invalid("response_format must be wav or pcm"));
    }
    let synthesis = tokio::task::spawn_blocking(move || {
        state
            .backend
            .synthesize(&request.input, request.speed, &context)
    })
    .await
    .map_err(|_| ApiError::from(VoiceBackendError::Engine))??;
    let sample_rate = synthesis.audio().sample_rate();
    let (content_type, bytes) = if format == "wav" {
        (
            "audio/wav",
            synthesis
                .wav()
                .map_err(|_| ApiError::from(VoiceBackendError::Engine))?,
        )
    } else {
        ("audio/pcm", synthesis.pcm16())
    };
    let mut response = bytes.into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        content_type
            .parse()
            .map_err(|_| ApiError::from(VoiceBackendError::Engine))?,
    );
    response.headers_mut().insert(
        "x-audio-sample-rate",
        sample_rate
            .to_string()
            .parse()
            .map_err(|_| ApiError::from(VoiceBackendError::Engine))?,
    );
    response
        .headers_mut()
        .insert("x-audio-channels", header::HeaderValue::from_static("1"));
    response.headers_mut().insert(
        "x-audio-format",
        header::HeaderValue::from_static("pcm_s16le"),
    );
    Ok(response)
}

async fn realtime(
    State(state): State<ApiState>,
    Extension(request): Extension<RequestContext>,
    upgrade: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    let permit = state
        .sessions
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError::from(VoiceBackendError::Busy))?;
    let token = CancellationToken::new();
    let context = RequestContext::new(
        request.id(),
        token.clone(),
        Some(Duration::from_secs(MAX_WS_SESSION_SECONDS)),
    )
    .map_err(|_| ApiError::invalid("session deadline is invalid"))?;
    Ok(upgrade
        .max_message_size(MAX_WS_MESSAGE_BYTES)
        .max_frame_size(MAX_WS_MESSAGE_BYTES)
        .on_upgrade(move |socket| websocket_session(socket, state, context, token, permit)))
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum ClientEvent {
    SessionStart {
        mode: SessionMode,
        sample_rate: Option<u32>,
    },
    AudioAppend {
        bytes: usize,
    },
    AudioCommit,
    SpeechGenerate {
        input: String,
        #[serde(default = "default_speed")]
        speed: f32,
    },
    Cancel,
    Ping,
    Close,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum SessionMode {
    Stt,
    Tts,
}

#[derive(Debug, Serialize)]
struct ServerEvent<'a> {
    #[serde(rename = "type")]
    kind: &'a str,
    session_id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sample_rate: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    channels: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    chunking: Option<&'a str>,
}

struct SessionState {
    mode: Option<SessionMode>,
    pending_audio: Option<usize>,
    transcriber: Option<Box<dyn RealtimeTranscriber>>,
    completed: bool,
}

async fn websocket_session(
    mut socket: WebSocket,
    state: ApiState,
    context: RequestContext,
    token: CancellationToken,
    _permit: OwnedSemaphorePermit,
) {
    let session_id = context.id().get();
    let mut session = SessionState {
        mode: None,
        pending_audio: None,
        transcriber: None,
        completed: false,
    };
    let outcome = tokio::time::timeout(
        Duration::from_secs(MAX_WS_SESSION_SECONDS),
        websocket_loop(&mut socket, &state, &context, &mut session),
    )
    .await;
    let _ = token.cancel();
    if outcome.is_err() {
        let _ = send_error(&mut socket, session_id, "deadline_exceeded").await;
    }
    let _ = socket
        .send(Message::Close(Some(CloseFrame {
            code: 1000,
            reason: "session closed".into(),
        })))
        .await;
}

async fn websocket_loop(
    socket: &mut WebSocket,
    state: &ApiState,
    context: &RequestContext,
    session: &mut SessionState,
) -> Result<(), ()> {
    while let Some(message) = socket.recv().await {
        let message = message.map_err(|_| ())?;
        match message {
            Message::Text(text) => {
                if session.pending_audio.is_some() {
                    send_error(socket, context.id().get(), "audio_frame_expected").await?;
                    return Err(());
                }
                let event: ClientEvent = serde_json::from_str(&text).map_err(|_| ())?;
                if handle_client_event(socket, state, context, session, event).await? {
                    return Ok(());
                }
            }
            Message::Binary(bytes) => {
                let Some(expected) = session.pending_audio.take() else {
                    send_error(socket, context.id().get(), "audio_declaration_required").await?;
                    return Err(());
                };
                if bytes.len() != expected || expected > MAX_WS_MESSAGE_BYTES {
                    send_error(socket, context.id().get(), "audio_frame_invalid").await?;
                    return Err(());
                }
                let Some(transcriber) = session.transcriber.as_mut() else {
                    send_error(socket, context.id().get(), "stt_session_required").await?;
                    return Err(());
                };
                let updates = transcriber.push_pcm16(&bytes).map_err(|_| ())?;
                for update in updates {
                    send_transcript(socket, context.id().get(), &update).await?;
                }
            }
            Message::Ping(bytes) => socket.send(Message::Pong(bytes)).await.map_err(|_| ())?,
            Message::Pong(_) => {}
            Message::Close(_) => return Ok(()),
        }
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
async fn handle_client_event(
    socket: &mut WebSocket,
    state: &ApiState,
    context: &RequestContext,
    session: &mut SessionState,
    event: ClientEvent,
) -> Result<bool, ()> {
    let session_id = context.id().get();
    match event {
        ClientEvent::SessionStart { mode, sample_rate } if session.mode.is_none() => {
            session.mode = Some(mode);
            if mode == SessionMode::Stt {
                let rate = sample_rate.ok_or(())?;
                session.transcriber = Some(
                    state
                        .backend
                        .start_transcription(rate, context.clone())
                        .map_err(|_| ())?,
                );
            } else if sample_rate.is_some() {
                send_error(socket, session_id, "sample_rate_not_allowed").await?;
                return Err(());
            }
            send_event(
                socket,
                "session_ready",
                session_id,
                None,
                None,
                None,
                None,
                None,
            )
            .await?;
        }
        ClientEvent::AudioAppend { bytes }
            if session.mode == Some(SessionMode::Stt)
                && !session.completed
                && bytes > 0
                && bytes <= MAX_WS_MESSAGE_BYTES =>
        {
            session.pending_audio = Some(bytes);
        }
        ClientEvent::AudioCommit
            if session.mode == Some(SessionMode::Stt) && !session.completed =>
        {
            let transcriber = session.transcriber.take().ok_or(())?;
            let update = transcriber.finish().map_err(|_| ())?;
            send_transcript(socket, session_id, &update).await?;
            session.completed = true;
            send_event(
                socket,
                "session_completed",
                session_id,
                None,
                None,
                None,
                None,
                None,
            )
            .await?;
        }
        ClientEvent::SpeechGenerate { input, speed }
            if session.mode == Some(SessionMode::Tts) && !session.completed =>
        {
            let backend = Arc::clone(&state.backend);
            let context = context.clone();
            let synthesis =
                tokio::task::spawn_blocking(move || backend.synthesize(&input, speed, &context))
                    .await
                    .map_err(|_| ())?
                    .map_err(|_| ())?;
            send_event(
                socket,
                "speech_metadata",
                session_id,
                None,
                None,
                Some(synthesis.audio().sample_rate()),
                Some(1),
                Some("post_synthesis_transport"),
            )
            .await?;
            let pcm = synthesis.pcm16();
            for chunk in pcm.chunks(TTS_CHUNK_BYTES) {
                socket
                    .send(Message::Binary(Bytes::copy_from_slice(chunk)))
                    .await
                    .map_err(|_| ())?;
            }
            session.completed = true;
            send_event(
                socket,
                "speech_completed",
                session_id,
                None,
                None,
                None,
                None,
                None,
            )
            .await?;
        }
        ClientEvent::Cancel => {
            let _ = context.cancellation().cancel();
            send_event(
                socket,
                "cancelled",
                session_id,
                None,
                None,
                None,
                None,
                None,
            )
            .await?;
            return Ok(true);
        }
        ClientEvent::Ping => {
            send_event(socket, "pong", session_id, None, None, None, None, None).await?;
        }
        ClientEvent::Close => return Ok(true),
        _ => {
            send_error(socket, session_id, "invalid_sequence").await?;
            return Err(());
        }
    }
    Ok(false)
}

async fn send_transcript(
    socket: &mut WebSocket,
    session_id: u64,
    update: &Transcript,
) -> Result<(), ()> {
    let kind = match update.kind {
        TranscriptKind::Interim => "transcript_interim",
        TranscriptKind::Final => "transcript_final",
    };
    send_event(
        socket,
        kind,
        session_id,
        Some(&update.text),
        None,
        None,
        None,
        None,
    )
    .await
}

async fn send_error(socket: &mut WebSocket, session_id: u64, code: &'static str) -> Result<(), ()> {
    send_event(
        socket,
        "error",
        session_id,
        None,
        Some(code),
        None,
        None,
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn send_event(
    socket: &mut WebSocket,
    kind: &'static str,
    session_id: u64,
    text: Option<&str>,
    code: Option<&'static str>,
    sample_rate: Option<u32>,
    channels: Option<u8>,
    chunking: Option<&'static str>,
) -> Result<(), ()> {
    let payload = serde_json::to_string(&ServerEvent {
        kind,
        session_id,
        text,
        code,
        sample_rate,
        channels,
        chunking,
    })
    .map_err(|_| ())?;
    socket
        .send(Message::Text(payload.into()))
        .await
        .map_err(|_| ())
}

impl fmt::Debug for dyn VoiceBackend {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VoiceBackend")
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use std::{error::Error, sync::Arc, time::Duration};

    use axum::http::StatusCode;
    use futures_util::{SinkExt, StreamExt};
    use impossible_server_core::{CancellationToken, ServerLimits};
    use impossible_voice_audio::MonoPcm;
    use impossible_voice_stt::{Transcript, TranscriptKind};
    use impossible_voice_tts::Synthesis;
    use reqwest::Client;
    use serde_json::Value;
    use tokio::{net::TcpListener, task::JoinHandle};
    use tokio_tungstenite::{connect_async, tungstenite::Message as ClientMessage};

    use super::{RealtimeTranscriber, VoiceBackend, VoiceBackendError};
    use crate::{TemplateServer, VoiceEngineWorkload};

    struct FakeBackend;

    impl VoiceBackend for FakeBackend {
        fn transcribe(
            &self,
            _audio: &MonoPcm,
            _context: &impossible_server_core::RequestContext,
        ) -> Result<String, VoiceBackendError> {
            Ok("fake transcript".to_owned())
        }

        fn synthesize(
            &self,
            _text: &str,
            _speed: f32,
            _context: &impossible_server_core::RequestContext,
        ) -> Result<Synthesis, VoiceBackendError> {
            let audio = MonoPcm::new(22_050, vec![0.0, 0.25, -0.25, 0.0])
                .map_err(|_| VoiceBackendError::Engine)?;
            Synthesis::from_audio(audio, 2).map_err(|_| VoiceBackendError::Engine)
        }

        fn start_transcription(
            &self,
            _sample_rate: u32,
            _context: impossible_server_core::RequestContext,
        ) -> Result<Box<dyn RealtimeTranscriber>, VoiceBackendError> {
            Ok(Box::new(FakeTranscriber { bytes: 0 }))
        }
    }

    struct FakeTranscriber {
        bytes: usize,
    }

    impl RealtimeTranscriber for FakeTranscriber {
        fn push_pcm16(&mut self, bytes: &[u8]) -> Result<Vec<Transcript>, VoiceBackendError> {
            self.bytes += bytes.len();
            Ok(vec![Transcript {
                kind: TranscriptKind::Interim,
                text: format!("partial-{}", self.bytes),
            }])
        }

        fn finish(self: Box<Self>) -> Result<Transcript, VoiceBackendError> {
            Ok(Transcript {
                kind: TranscriptKind::Final,
                text: format!("final-{}", self.bytes),
            })
        }
    }

    async fn start_server()
    -> Result<(String, CancellationToken, JoinHandle<std::io::Result<()>>), Box<dyn Error>> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let address = listener.local_addr()?;
        let shutdown = CancellationToken::new();
        let server = TemplateServer::with_limits(
            VoiceEngineWorkload::with_backend(Arc::new(FakeBackend)),
            ServerLimits::default(),
        );
        let server_shutdown = shutdown.clone();
        let task = tokio::spawn(async move { server.serve(listener, server_shutdown).await });
        Ok((format!("127.0.0.1:{}", address.port()), shutdown, task))
    }

    async fn stop_server(
        shutdown: CancellationToken,
        task: JoinHandle<std::io::Result<()>>,
    ) -> Result<(), Box<dyn Error>> {
        let _ = shutdown.cancel();
        tokio::time::timeout(Duration::from_secs(2), task).await???;
        Ok(())
    }

    #[tokio::test]
    async fn real_http_network_contract_supports_pcm_wav_and_errors() -> Result<(), Box<dyn Error>>
    {
        let (address, shutdown, task) = start_server().await?;
        let client = Client::new();
        let pcm = vec![0_u8; 640];
        let transcription = client
            .post(format!("http://{address}/api/v1/transcriptions"))
            .header("content-type", "audio/pcm")
            .header("x-audio-sample-rate", "16000")
            .body(pcm)
            .send()
            .await?;
        assert_eq!(transcription.status(), StatusCode::OK);
        let transcription: Value = serde_json::from_slice(&transcription.bytes().await?)?;
        assert_eq!(transcription["text"], "fake transcript");

        let speech = client
            .post(format!("http://{address}/v1/audio/speech"))
            .header("content-type", "application/json")
            .body(r#"{"input":"hello","voice":"kristin","response_format":"wav"}"#)
            .send()
            .await?;
        assert_eq!(speech.status(), StatusCode::OK);
        assert_eq!(speech.headers()["content-type"], "audio/wav");
        assert_eq!(&speech.bytes().await?[..4], b"RIFF");

        let invalid = client
            .post(format!("http://{address}/api/v1/transcriptions"))
            .header("content-type", "audio/mpeg")
            .body(vec![0_u8; 4])
            .send()
            .await?;
        assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
        stop_server(shutdown, task).await
    }

    #[tokio::test]
    async fn real_websocket_stt_enforces_sequence_and_emits_interim_final()
    -> Result<(), Box<dyn Error>> {
        let (address, shutdown, task) = start_server().await?;
        let (mut socket, _) = connect_async(format!("ws://{address}/api/v1/realtime")).await?;
        socket
            .send(ClientMessage::Text(
                r#"{"type":"session_start","mode":"stt","sample_rate":16000}"#.into(),
            ))
            .await?;
        assert!(next_text(&mut socket).await?.contains("session_ready"));
        socket
            .send(ClientMessage::Text(
                r#"{"type":"audio_append","bytes":4}"#.into(),
            ))
            .await?;
        socket
            .send(ClientMessage::Binary(vec![0_u8; 4].into()))
            .await?;
        assert!(next_text(&mut socket).await?.contains("transcript_interim"));
        socket
            .send(ClientMessage::Text(r#"{"type":"audio_commit"}"#.into()))
            .await?;
        assert!(next_text(&mut socket).await?.contains("transcript_final"));
        assert!(next_text(&mut socket).await?.contains("session_completed"));
        socket.close(None).await?;

        let (mut invalid, _) = connect_async(format!("ws://{address}/api/v1/realtime")).await?;
        invalid
            .send(ClientMessage::Binary(vec![0_u8; 2].into()))
            .await?;
        assert!(
            next_text(&mut invalid)
                .await?
                .contains("audio_declaration_required")
        );
        stop_server(shutdown, task).await
    }

    #[tokio::test]
    async fn real_websocket_tts_labels_post_synthesis_chunks() -> Result<(), Box<dyn Error>> {
        let (address, shutdown, task) = start_server().await?;
        let (mut socket, _) = connect_async(format!("ws://{address}/api/v1/realtime")).await?;
        socket
            .send(ClientMessage::Text(
                r#"{"type":"session_start","mode":"tts","sample_rate":null}"#.into(),
            ))
            .await?;
        assert!(next_text(&mut socket).await?.contains("session_ready"));
        socket
            .send(ClientMessage::Text(
                r#"{"type":"speech_generate","input":"hello","speed":1.0}"#.into(),
            ))
            .await?;
        let metadata = next_text(&mut socket).await?;
        assert!(metadata.contains("post_synthesis_transport"));
        let audio = socket.next().await.ok_or("missing audio")??;
        assert!(matches!(audio, ClientMessage::Binary(bytes) if !bytes.is_empty()));
        assert!(next_text(&mut socket).await?.contains("speech_completed"));
        stop_server(shutdown, task).await
    }

    async fn next_text<S>(
        socket: &mut tokio_tungstenite::WebSocketStream<S>,
    ) -> Result<String, Box<dyn Error>>
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    {
        loop {
            match socket.next().await.ok_or("socket closed")?? {
                ClientMessage::Text(text) => return Ok(text.to_string()),
                ClientMessage::Ping(bytes) => socket.send(ClientMessage::Pong(bytes)).await?,
                _ => {}
            }
        }
    }
}
