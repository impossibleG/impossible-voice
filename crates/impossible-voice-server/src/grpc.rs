//! Bounded gRPC Voice services with health and reflection.

use std::{pin::Pin, sync::Arc, time::Duration};

use impossible_server_core::{CancellationToken, RequestContext, RequestIdSource, RequestStop};
use impossible_voice_audio::{AudioLimits, MonoPcm};
use impossible_voice_protocol::v1::{
    AudioChunk, AudioEncoding, FILE_DESCRIPTOR_SET, StreamTranscribeRequest, SynthesizeRequest,
    SynthesizeResponse, TranscribeRequest, TranscribeResponse, TranscriptEvent,
    stream_transcribe_request,
    voice_server::{Voice, VoiceServer},
};
use impossible_voice_stt::TranscriptKind;
use tokio::{net::TcpListener, sync::mpsc};
use tokio_stream::{
    Stream,
    wrappers::{ReceiverStream, TcpListenerStream},
};
use tonic::{Request, Response, Status, transport::Server};

use crate::voice_api::{RealtimeTranscriber, VoiceBackend, VoiceBackendError};

const MAX_GRPC_MESSAGE_BYTES: usize = 16 * 1024 * 1024;
const MAX_STREAM_AUDIO_BYTES: usize = 64 * 1024;
const OUTPUT_QUEUE: usize = 8;
const TTS_CHUNK_BYTES: usize = 32 * 1024;

type ResponseStream<T> = Pin<Box<dyn Stream<Item = Result<T, Status>> + Send + 'static>>;

#[derive(Clone)]
struct GrpcVoice {
    backend: Arc<dyn VoiceBackend>,
    request_ids: RequestIdSource,
    timeout: Duration,
    shutdown: CancellationToken,
}

impl GrpcVoice {
    fn context(&self, cancellation: CancellationToken) -> Result<RequestContext, Status> {
        let id = self
            .request_ids
            .next()
            .map_err(|_| Status::internal("request identifiers are unavailable"))?;
        let request_stop = cancellation.clone();
        let server_stop = self.shutdown.clone();
        tokio::spawn(async move {
            tokio::select! {
                () = server_stop.cancelled() => { let _ = request_stop.cancel(); }
                () = request_stop.cancelled() => {}
            }
        });
        RequestContext::new(id, cancellation, Some(self.timeout))
            .map_err(|_| Status::internal("request deadline is unavailable"))
    }
}

struct CancelOnDrop(CancellationToken);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        let _ = self.0.cancel();
    }
}

#[tonic::async_trait]
impl Voice for GrpcVoice {
    async fn transcribe(
        &self,
        request: Request<TranscribeRequest>,
    ) -> Result<Response<TranscribeResponse>, Status> {
        let cancellation = CancellationToken::new();
        let _cancel_on_drop = CancelOnDrop(cancellation.clone());
        let context = self.context(cancellation)?;
        let request = request.into_inner();
        let audio = decode_audio(&request)?;
        let backend = Arc::clone(&self.backend);
        let text = tokio::task::spawn_blocking(move || backend.transcribe(&audio, &context))
            .await
            .map_err(|_| Status::internal("speech recognition worker failed"))?
            .map_err(map_backend_error)?;
        Ok(Response::new(TranscribeResponse { text }))
    }

    type StreamTranscribeStream = ResponseStream<TranscriptEvent>;

    #[allow(clippy::too_many_lines)]
    async fn stream_transcribe(
        &self,
        request: Request<tonic::Streaming<StreamTranscribeRequest>>,
    ) -> Result<Response<Self::StreamTranscribeStream>, Status> {
        let mut input = request.into_inner();
        let backend = Arc::clone(&self.backend);
        let cancellation = CancellationToken::new();
        let context = self.context(cancellation.clone())?;
        let (sender, receiver) = mpsc::channel(OUTPUT_QUEUE);
        tokio::spawn(async move {
            let _cancel_on_drop = CancelOnDrop(cancellation);
            let mut session: Option<Box<dyn RealtimeTranscriber>> = None;
            let mut committed = false;
            loop {
                let message = match tokio::select! {
                    biased;
                    stop = context.stopped() => {
                        let status = match stop {
                            RequestStop::Cancelled => Status::cancelled("the request was cancelled"),
                            RequestStop::DeadlineExceeded => Status::deadline_exceeded("the request deadline was exceeded"),
                        };
                        let _ = sender.send(Err(status)).await;
                        return;
                    }
                    message = input.message() => message,
                } {
                    Ok(Some(message)) => message,
                    Ok(None) => break,
                    Err(_) => {
                        let _ = sender
                            .send(Err(Status::cancelled("input stream failed")))
                            .await;
                        return;
                    }
                };
                let result = match message.event {
                    Some(stream_transcribe_request::Event::Config(config))
                        if session.is_none() && !committed =>
                    {
                        backend
                            .start_transcription(config.sample_rate, context.clone())
                            .map(|opened| session = Some(opened))
                    }
                    Some(stream_transcribe_request::Event::Audio(bytes))
                        if session.is_some()
                            && !committed
                            && !bytes.is_empty()
                            && bytes.len() <= MAX_STREAM_AUDIO_BYTES =>
                    {
                        let updates = session
                            .as_mut()
                            .ok_or(VoiceBackendError::InvalidInput)
                            .and_then(|active| active.push_pcm16(&bytes));
                        match updates {
                            Ok(updates) => {
                                for update in updates {
                                    if sender
                                        .send(Ok(TranscriptEvent {
                                            text: update.text,
                                            r#final: update.kind == TranscriptKind::Final,
                                        }))
                                        .await
                                        .is_err()
                                    {
                                        return;
                                    }
                                }
                                Ok(())
                            }
                            Err(error) => Err(error),
                        }
                    }
                    Some(stream_transcribe_request::Event::Commit(true))
                        if session.is_some() && !committed =>
                    {
                        committed = true;
                        session
                            .take()
                            .ok_or(VoiceBackendError::InvalidInput)
                            .and_then(RealtimeTranscriber::finish)
                            .and_then(|final_result| {
                                sender
                                    .try_send(Ok(TranscriptEvent {
                                        text: final_result.text,
                                        r#final: true,
                                    }))
                                    .map_err(|_| VoiceBackendError::Busy)
                            })
                    }
                    Some(stream_transcribe_request::Event::Cancel(true)) => {
                        let _ = context.cancellation().cancel();
                        return;
                    }
                    _ => Err(VoiceBackendError::InvalidInput),
                };
                if let Err(error) = result {
                    let _ = sender.send(Err(map_backend_error(error))).await;
                    return;
                }
                if committed {
                    return;
                }
            }
            if !committed {
                let _ = sender
                    .send(Err(Status::invalid_argument("stream ended before commit")))
                    .await;
            }
        });
        Ok(Response::new(Box::pin(ReceiverStream::new(receiver))))
    }

    async fn synthesize(
        &self,
        request: Request<SynthesizeRequest>,
    ) -> Result<Response<SynthesizeResponse>, Status> {
        let cancellation = CancellationToken::new();
        let _cancel_on_drop = CancelOnDrop(cancellation.clone());
        let context = self.context(cancellation)?;
        let request = request.into_inner();
        let encoding = parse_output_encoding(request.encoding)?;
        let backend = Arc::clone(&self.backend);
        let synthesis = tokio::task::spawn_blocking(move || {
            backend.synthesize(&request.input, normalize_speed(request.speed), &context)
        })
        .await
        .map_err(|_| Status::internal("speech synthesis worker failed"))?
        .map_err(map_backend_error)?;
        let sample_rate = synthesis.audio().sample_rate();
        let audio = encode_synthesis(&synthesis, encoding)?;
        Ok(Response::new(SynthesizeResponse {
            audio,
            encoding: encoding as i32,
            sample_rate,
            channels: 1,
        }))
    }

    type StreamSynthesizeStream = ResponseStream<AudioChunk>;

    async fn stream_synthesize(
        &self,
        request: Request<SynthesizeRequest>,
    ) -> Result<Response<Self::StreamSynthesizeStream>, Status> {
        let cancellation = CancellationToken::new();
        let cancel_on_drop = CancelOnDrop(cancellation.clone());
        let context = self.context(cancellation.clone())?;
        let request = request.into_inner();
        let encoding = parse_output_encoding(request.encoding)?;
        if encoding != AudioEncoding::RawPcm16 {
            return Err(Status::invalid_argument(
                "streaming synthesis requires raw PCM16 output",
            ));
        }
        let backend = Arc::clone(&self.backend);
        let synthesis = tokio::task::spawn_blocking(move || {
            backend.synthesize(&request.input, normalize_speed(request.speed), &context)
        })
        .await
        .map_err(|_| Status::internal("speech synthesis worker failed"))?
        .map_err(map_backend_error)?;
        let sample_rate = synthesis.audio().sample_rate();
        let pcm = synthesis.pcm16();
        let (sender, receiver) = mpsc::channel(OUTPUT_QUEUE);
        tokio::spawn(async move {
            let _cancel_on_drop = cancel_on_drop;
            let chunks = pcm.len().div_ceil(TTS_CHUNK_BYTES);
            for (index, audio) in pcm.chunks(TTS_CHUNK_BYTES).enumerate() {
                let Ok(sequence) = u32::try_from(index) else {
                    let _ = sender
                        .send(Err(Status::internal("chunk sequence overflow")))
                        .await;
                    return;
                };
                if sender
                    .send(Ok(AudioChunk {
                        audio: audio.to_vec(),
                        sequence,
                        r#final: index + 1 == chunks,
                        sample_rate,
                        channels: 1,
                        chunking: "post_synthesis_transport".to_owned(),
                    }))
                    .await
                    .is_err()
                {
                    return;
                }
            }
        });
        Ok(Response::new(Box::pin(ReceiverStream::new(receiver))))
    }
}

fn decode_audio(request: &TranscribeRequest) -> Result<MonoPcm, Status> {
    if request.audio.len() > MAX_GRPC_MESSAGE_BYTES {
        return Err(Status::resource_exhausted(
            "audio exceeds the message limit",
        ));
    }
    match AudioEncoding::try_from(request.encoding).ok() {
        Some(AudioEncoding::WavPcm16) if request.sample_rate == 0 => {
            MonoPcm::from_wav(&request.audio, AudioLimits::default())
                .map_err(|_| Status::invalid_argument("WAV audio is invalid"))
        }
        Some(AudioEncoding::RawPcm16) if request.sample_rate > 0 => {
            MonoPcm::from_pcm16_le(&request.audio, request.sample_rate, AudioLimits::default())
                .map_err(|_| Status::invalid_argument("raw PCM16 audio is invalid"))
        }
        _ => Err(Status::invalid_argument(
            "encoding and sample rate are inconsistent",
        )),
    }
}

fn parse_output_encoding(value: i32) -> Result<AudioEncoding, Status> {
    match AudioEncoding::try_from(value).ok() {
        Some(encoding @ (AudioEncoding::WavPcm16 | AudioEncoding::RawPcm16)) => Ok(encoding),
        _ => Err(Status::invalid_argument("output encoding is unsupported")),
    }
}

fn normalize_speed(speed: f32) -> f32 {
    if speed == 0.0 { 1.0 } else { speed }
}

fn encode_synthesis(
    synthesis: &impossible_voice_tts::Synthesis,
    encoding: AudioEncoding,
) -> Result<Vec<u8>, Status> {
    match encoding {
        AudioEncoding::WavPcm16 => synthesis
            .wav()
            .map_err(|_| Status::internal("WAV encoding failed")),
        AudioEncoding::RawPcm16 => Ok(synthesis.pcm16()),
        AudioEncoding::Unspecified => Err(Status::invalid_argument("output encoding is required")),
    }
}

fn map_backend_error(error: VoiceBackendError) -> Status {
    match error {
        VoiceBackendError::InvalidInput => Status::invalid_argument("voice input is invalid"),
        VoiceBackendError::Busy => Status::resource_exhausted("voice capacity is busy"),
        VoiceBackendError::Cancelled => Status::cancelled("the request was cancelled"),
        VoiceBackendError::DeadlineExceeded => {
            Status::deadline_exceeded("the request deadline was exceeded")
        }
        VoiceBackendError::Engine => Status::internal("the local voice engine failed"),
    }
}

/// Serves the loopback gRPC API, standard health service, and v1 reflection.
///
/// # Errors
/// Returns a sanitized boxed server error if reflection or serving fails.
pub async fn serve(
    listener: TcpListener,
    backend: Arc<dyn VoiceBackend>,
    request_timeout: Duration,
    shutdown_timeout: Duration,
    shutdown: CancellationToken,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let implementation = GrpcVoice {
        backend,
        request_ids: RequestIdSource::default(),
        timeout: request_timeout,
        shutdown: shutdown.clone(),
    };
    let service = VoiceServer::new(implementation)
        .max_decoding_message_size(MAX_GRPC_MESSAGE_BYTES)
        .max_encoding_message_size(MAX_GRPC_MESSAGE_BYTES);
    let (health_reporter, health_service) = tonic_health::server::health_reporter();
    health_reporter
        .set_serving::<VoiceServer<GrpcVoice>>()
        .await;
    let reflection = tonic_reflection::server::Builder::configure()
        .register_encoded_file_descriptor_set(FILE_DESCRIPTOR_SET)
        .build_v1()?;
    let server = Server::builder()
        .add_service(health_service)
        .add_service(reflection)
        .add_service(service)
        .serve_with_incoming_shutdown(TcpListenerStream::new(listener), {
            let shutdown = shutdown.clone();
            async move { shutdown.cancelled().await }
        });
    tokio::pin!(server);
    tokio::select! {
        result = &mut server => result.map_err(Into::into),
        () = shutdown.cancelled() => {
            tokio::time::timeout(shutdown_timeout, &mut server)
                .await
                .map_err(|_| "gRPC shutdown exceeded its configured bound")??;
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        error::Error,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        time::Duration,
    };

    use impossible_server_core::{CancellationToken, RequestContext, RequestIdSource};
    use impossible_voice_audio::MonoPcm;
    use impossible_voice_protocol::v1::{
        AudioEncoding, StreamConfig, StreamTranscribeRequest, SynthesizeRequest, TranscribeRequest,
        stream_transcribe_request, voice_client::VoiceClient, voice_server::Voice,
    };
    use impossible_voice_stt::{Transcript, TranscriptKind};
    use impossible_voice_tts::Synthesis;
    use tokio::{net::TcpListener, sync::mpsc};
    use tokio_stream::{iter, wrappers::ReceiverStream};
    use tonic::Request;
    use tonic_health::pb::{HealthCheckRequest, health_client::HealthClient};

    use super::{GrpcVoice, RealtimeTranscriber, VoiceBackend, VoiceBackendError, serve};

    struct FakeBackend;

    impl VoiceBackend for FakeBackend {
        fn transcribe(
            &self,
            _audio: &MonoPcm,
            _context: &RequestContext,
        ) -> Result<String, VoiceBackendError> {
            Ok("grpc transcript".to_owned())
        }

        fn synthesize(
            &self,
            _text: &str,
            _speed: f32,
            _context: &RequestContext,
        ) -> Result<Synthesis, VoiceBackendError> {
            let audio = MonoPcm::new(22_050, vec![0.0, 0.25, -0.25, 0.0])
                .map_err(|_| VoiceBackendError::Engine)?;
            Synthesis::from_audio(audio, 2).map_err(|_| VoiceBackendError::Engine)
        }

        fn start_transcription(
            &self,
            _sample_rate: u32,
            _context: RequestContext,
        ) -> Result<Box<dyn RealtimeTranscriber>, VoiceBackendError> {
            Ok(Box::new(FakeStream { bytes: 0 }))
        }
    }

    struct FakeStream {
        bytes: usize,
    }

    struct CancelAwareBackend {
        started: Arc<AtomicBool>,
        cancelled: Arc<AtomicBool>,
    }

    impl VoiceBackend for CancelAwareBackend {
        fn transcribe(
            &self,
            _audio: &MonoPcm,
            _context: &RequestContext,
        ) -> Result<String, VoiceBackendError> {
            Err(VoiceBackendError::InvalidInput)
        }

        fn synthesize(
            &self,
            _text: &str,
            _speed: f32,
            context: &RequestContext,
        ) -> Result<Synthesis, VoiceBackendError> {
            self.started.store(true, Ordering::Release);
            while !context.cancellation().is_cancelled() {
                std::thread::sleep(Duration::from_millis(5));
            }
            self.cancelled.store(true, Ordering::Release);
            Err(VoiceBackendError::Cancelled)
        }

        fn start_transcription(
            &self,
            _sample_rate: u32,
            _context: RequestContext,
        ) -> Result<Box<dyn RealtimeTranscriber>, VoiceBackendError> {
            Err(VoiceBackendError::InvalidInput)
        }
    }

    impl RealtimeTranscriber for FakeStream {
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

    #[tokio::test]
    async fn real_grpc_network_supports_unary_streaming_health_and_errors()
    -> Result<(), Box<dyn Error>> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let address = listener.local_addr()?;
        let shutdown = CancellationToken::new();
        let server_shutdown = shutdown.clone();
        let server = tokio::spawn(async move {
            serve(
                listener,
                Arc::new(FakeBackend),
                Duration::from_secs(5),
                Duration::from_secs(2),
                server_shutdown,
            )
            .await
        });
        let endpoint = format!("http://{address}");
        let mut client = VoiceClient::connect(endpoint.clone()).await?;
        let transcript = client
            .transcribe(TranscribeRequest {
                audio: vec![0_u8; 640],
                encoding: AudioEncoding::RawPcm16 as i32,
                sample_rate: 16_000,
            })
            .await?
            .into_inner();
        assert_eq!(transcript.text, "grpc transcript");

        let speech = client
            .synthesize(SynthesizeRequest {
                input: "hello".to_owned(),
                speed: 1.0,
                encoding: AudioEncoding::WavPcm16 as i32,
            })
            .await?
            .into_inner();
        assert_eq!(&speech.audio[..4], b"RIFF");

        let events = vec![
            stream_event(stream_transcribe_request::Event::Config(StreamConfig {
                sample_rate: 16_000,
            })),
            stream_event(stream_transcribe_request::Event::Audio(vec![0_u8; 4])),
            stream_event(stream_transcribe_request::Event::Commit(true)),
        ];
        let mut transcripts = client.stream_transcribe(iter(events)).await?.into_inner();
        assert!(
            !transcripts
                .message()
                .await?
                .ok_or("missing interim")?
                .r#final
        );
        assert!(transcripts.message().await?.ok_or("missing final")?.r#final);

        let mut chunks = client
            .stream_synthesize(SynthesizeRequest {
                input: "hello".to_owned(),
                speed: 1.0,
                encoding: AudioEncoding::RawPcm16 as i32,
            })
            .await?
            .into_inner();
        let chunk = chunks.message().await?.ok_or("missing audio chunk")?;
        assert_eq!(chunk.chunking, "post_synthesis_transport");
        assert!(chunk.r#final);

        let mut invalid = client
            .stream_transcribe(iter([stream_event(
                stream_transcribe_request::Event::Audio(vec![0_u8; 2]),
            )]))
            .await?
            .into_inner();
        assert_eq!(
            invalid.message().await.err().map(|status| status.code()),
            Some(tonic::Code::InvalidArgument)
        );

        let channel = tonic::transport::Endpoint::from_shared(endpoint)?
            .connect()
            .await?;
        let mut health = HealthClient::new(channel);
        assert_eq!(
            health
                .check(HealthCheckRequest {
                    service: String::new(),
                })
                .await?
                .into_inner()
                .status,
            1
        );
        let _ = shutdown.cancel();
        let server_result = server.await?;
        assert!(server_result.is_ok());
        Ok(())
    }

    #[tokio::test]
    async fn streaming_synthesis_cancels_when_pre_response_call_is_dropped()
    -> Result<(), Box<dyn Error>> {
        let started = Arc::new(AtomicBool::new(false));
        let cancelled = Arc::new(AtomicBool::new(false));
        let implementation = GrpcVoice {
            backend: Arc::new(CancelAwareBackend {
                started: Arc::clone(&started),
                cancelled: Arc::clone(&cancelled),
            }),
            request_ids: RequestIdSource::default(),
            timeout: Duration::from_secs(5),
            shutdown: CancellationToken::new(),
        };
        let call = tokio::spawn(async move {
            implementation
                .stream_synthesize(Request::new(SynthesizeRequest {
                    input: "wait".to_owned(),
                    speed: 1.0,
                    encoding: AudioEncoding::RawPcm16 as i32,
                }))
                .await
        });
        tokio::time::timeout(Duration::from_secs(1), async {
            while !started.load(Ordering::Acquire) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await?;
        call.abort();
        let _ = call.await;
        tokio::time::timeout(Duration::from_secs(1), async {
            while !cancelled.load(Ordering::Acquire) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await?;
        Ok(())
    }

    #[tokio::test]
    async fn idle_stream_deadline_and_server_shutdown_are_bounded() -> Result<(), Box<dyn Error>> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let address = listener.local_addr()?;
        let shutdown = CancellationToken::new();
        let server_shutdown = shutdown.clone();
        let server = tokio::spawn(async move {
            serve(
                listener,
                Arc::new(FakeBackend),
                Duration::from_millis(100),
                Duration::from_secs(1),
                server_shutdown,
            )
            .await
        });
        let mut client = VoiceClient::connect(format!("http://{address}")).await?;
        let (_input, receiver) = mpsc::channel(1);
        let mut output = client
            .stream_transcribe(ReceiverStream::new(receiver))
            .await?
            .into_inner();
        let status = tokio::time::timeout(Duration::from_secs(1), output.message())
            .await?
            .err()
            .map(|status| status.code());
        assert_eq!(status, Some(tonic::Code::DeadlineExceeded));

        let (_input, receiver) = mpsc::channel(1);
        let mut output = client
            .stream_transcribe(ReceiverStream::new(receiver))
            .await?
            .into_inner();
        let _ = shutdown.cancel();
        let status = tokio::time::timeout(Duration::from_secs(1), output.message())
            .await?
            .err()
            .map(|status| status.code());
        assert_eq!(status, Some(tonic::Code::Cancelled));
        let server_result = tokio::time::timeout(Duration::from_secs(1), server).await??;
        assert!(server_result.is_ok());
        Ok(())
    }

    fn stream_event(event: stream_transcribe_request::Event) -> StreamTranscribeRequest {
        StreamTranscribeRequest { event: Some(event) }
    }
}
