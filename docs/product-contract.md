# Impossible Voice v0.1 product contract

Impossible Voice v0.1 is a ready-made, self-hosted speech-to-text and text-to-speech server. A user
must be able to clone or download the project, run one documented setup command, start the service
with one documented command, and use it without a paid inference API. Setup acquires and verifies
the curated artifacts; after setup, startup and inference operate locally and offline.

This document is the public completion boundary. Passing tests against a reduced internal contract
does not make the release complete.

## Required vertical slice

- One curated, versioned speech-to-text model.
- One curated, versioned text-to-speech model and voice.
- CPU operation on Windows x86-64 and Linux x86-64.
- Automatic, checksum-verified model and runtime installation with staged activation and recovery.
- WAV input and signed PCM16 input for realtime sessions.
- Bounded resampling from common input sample rates to the STT engine's required rate.
- WAV and signed PCM16 TTS output.
- Basic bounded voice-activity detection for realtime transcription.
- HTTP batch transcription and speech generation.
- A genuine versioned WebSocket protocol for realtime transcription and streamed speech output.
- Unary and streaming gRPC equivalents.
- Bounded MCP tools for transcription, synthesis, model status, and health.
- Liveness, readiness, truthful model status and capabilities, metrics, structured logs, deadlines,
  cancellation, backpressure, concurrency limits, and graceful shutdown.
- Native archives and a Linux CPU container, or an explicit documented packaging limitation.

## HTTP contract

The server provides the following public routes:

- `POST /v1/audio/transcriptions`: a bounded OpenAI-compatible transcription subset.
- `POST /v1/audio/speech`: a bounded OpenAI-compatible speech-generation subset.
- Native typed routes under `/api/v1` when richer options are required.
- `GET /health/live`, `GET /health/ready`, `GET /metrics`, `GET /version`, `GET /v1/models`, and
  `GET /v1/capabilities`.

Requests have documented byte, text, duration, deadline, queue, and concurrency limits. Errors use
stable machine-readable codes and never include input content, credentials, or private host paths.

## Realtime WebSocket contract

The versioned realtime endpoint supports typed session start, audio append, audio commit, text
submission for synthesis, cancellation, ping, and close messages. Transcription emits ordered
partial and final events with stable session and request identifiers. Synthesis emits ordered audio
chunks followed by a final completion event.

Frame size, buffered audio, concurrent operations, session duration, and output queues are bounded.
Cancellation propagates to the running operation. Protocol violations and overload use deterministic
error and close codes.

## gRPC and MCP contract

gRPC provides unary transcription and synthesis, client-streaming or bidirectional realtime
transcription, server-streaming synthesis, standard health behavior, message limits, deadlines, and
cancellation.

MCP provides bounded `transcribe_audio`, `synthesize_speech`, `list_models`, and `health` tools. MCP
is a convenience interface rather than the high-throughput streaming plane.

## Privacy and offline behavior

Normal logs and telemetry exclude request audio, transcript text, generated speech, credentials,
model paths, and private diagnostics. Telemetry remains local unless an operator explicitly
configures an exporter. After successful setup, restarting and repeating inference requires no
network connection.

## Explicitly deferred

- Speaker diarization or speaker identification.
- Voice cloning, user-trained voices, or arbitrary model upload.
- Translation and multilingual guarantees beyond the curated models' documented capability.
- MP3, AAC, Opus, video-container, or arbitrary media ingestion.
- Multiple simultaneously loaded STT models or TTS voices.
- GPU qualification or automatic accelerator selection.
- Noise suppression, echo cancellation, telephony integrations, a hosted control plane, billing,
  or a large dashboard.

Deferred features are future enhancements; they do not weaken any required item above.
