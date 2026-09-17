# Impossible Voice v0.1 API

After `setup`, ordinary `serve` is local and offline. HTTP, WebSocket, and MCP share the loopback
HTTP listener at `127.0.0.1:8080`; gRPC uses the distinct loopback listener at
`127.0.0.1:50051`. Non-loopback configuration is rejected. Responses never contain artifact paths.

## HTTP

`POST /v1/audio/transcriptions` accepts bounded multipart form data:

- `file` (required): `audio/wav`, `audio/x-wav`, `audio/pcm`, or `audio/L16` bytes.
- `sample_rate` (required only for raw PCM16): one supported rate from 8, 16, 22.05, 24, 32,
  44.1, or 48 kHz.
- `model` (optional): accepted for compatibility; the curated model is always used.

The native `POST /api/v1/transcriptions` accepts the audio as the complete request body. Use
`content-type: audio/wav` for WAV, or `content-type: audio/pcm` with
`x-audio-sample-rate: 16000` for raw signed little-endian mono PCM16. The response is
`{"text":"..."}`.

`POST /v1/audio/speech` and `POST /api/v1/speech` accept:

```json
{"input":"Hello locally.","voice":"kristin","speed":1.0,"response_format":"wav"}
```

`response_format` is `wav` or `pcm`. Successful responses include `content-type`,
`x-audio-sample-rate`, `x-audio-channels: 1`, and `x-audio-format: pcm_s16le`. Stable JSON errors
and `x-request-id` are returned at the shared admission boundary.

Control routes are `GET /health/live`, `/health/ready`, `/metrics`, `/version`, `/v1/models`, and
`/v1/capabilities`.

## Realtime WebSocket

Connect to `ws://127.0.0.1:8080/api/v1/realtime`. Text frames are strict JSON events. Binary PCM
frames are legal only immediately after an `audio_append` declaration with the exact byte count.

STT sequence:

```json
{"type":"session_start","mode":"stt","sample_rate":16000}
{"type":"audio_append","bytes":640}
```

Send exactly 640 binary PCM16 bytes, repeat append/binary pairs, then send:

```json
{"type":"audio_commit"}
```

The server emits `session_ready`, `transcript_interim`, `transcript_final`, and
`session_completed`. VAD can emit finals before explicit commit. `ping`, `cancel`, and `close` are
also supported. Wrong ordering emits an `error` event and closes the session.

TTS sequence:

```json
{"type":"session_start","mode":"tts","sample_rate":null}
{"type":"speech_generate","input":"Hello locally.","speed":1.0}
```

The server emits `speech_metadata`, bounded binary raw-PCM16 chunks, then `speech_completed`.
Metadata identifies `chunking` as `post_synthesis_transport`: Kristin VITS completes synthesis
before the first transport chunk is available.

## gRPC

The committed protocol is [`voice.proto`](../crates/impossible-voice-protocol/proto/voice.proto).
The `impossible.voice.v1.Voice` service provides:

- unary `Transcribe` and `Synthesize`;
- bidirectional `StreamTranscribe` with strict config, audio, then commit/cancel ordering;
- server-streaming `StreamSynthesize`, using raw PCM16 and post-synthesis transport chunks.

The listener enforces message bounds and serves standard gRPC health plus v1 reflection. A local
client can inspect it with:

```text
grpcurl -plaintext 127.0.0.1:50051 list
```

## MCP

`POST /mcp` is a bounded MCP-compatible JSON-RPC 2.0 endpoint. It implements `initialize`, `ping`,
`tools/list`, `tools/call`, `resources/list`, and `resources/read`.

Clients must initialize with protocol version `2025-03-26`; unsupported versions are rejected with
a JSON-RPC error. Malformed JSON is returned as a JSON-RPC parse error rather than an HTTP extractor
response.

Tools are `transcribe_audio`, `synthesize_speech`, `list_models`, and `health`. Audio input uses a
bounded `audio_base64` value and an explicit `wav` or `pcm16` encoding. There are no filesystem-path
inputs and MCP never downloads artifacts. Synthesized audio is returned as MCP audio content with
base64 data. Resources are `voice://capabilities` and `voice://status`.

Example health call:

```text
curl -H "content-type: application/json" \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"health","arguments":{}}}' \
  http://127.0.0.1:8080/mcp
```

## Bounds and limitations

The host applies body size, queue, concurrency, deadline, shutdown, session, WebSocket frame,
streaming audio, and output queue bounds. Defaults are shown in
[`config/impossible-voice.example.toml`](../config/impossible-voice.example.toml).

v0.1 is English-only, CPU-only, and limited to WAV/raw mono PCM16. It has one NeMo streaming STT
model and one Kristin VITS TTS voice. Diarization, cloning, arbitrary models, compressed audio,
translation, GPU qualification, and telephony processing are deferred. A native Linux CPU runtime
is installable, but a prebuilt Linux container is not yet published.
