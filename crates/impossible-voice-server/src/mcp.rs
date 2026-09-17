//! Bounded MCP-compatible JSON-RPC tools and resources over local HTTP.

use std::sync::Arc;

use axum::{
    Extension, Json, Router,
    body::Bytes,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use impossible_server_core::RequestContext;
use impossible_voice_audio::{AudioLimits, MonoPcm};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::voice_api::{VoiceBackend, VoiceBackendError};

const MCP_PROTOCOL_VERSION: &str = "2025-03-26";
const MAX_MCP_AUDIO_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone)]
struct McpState {
    backend: Arc<dyn VoiceBackend>,
}

/// Builds the bounded local MCP endpoint.
pub fn routes(backend: Arc<dyn VoiceBackend>) -> Router {
    Router::new()
        .route("/mcp", post(handle))
        .with_state(McpState { backend })
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonRpcRequest {
    jsonrpc: String,
    #[serde(default)]
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Serialize)]
struct JsonRpcResponse {
    jsonrpc: &'static str,
    id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
struct JsonRpcError {
    code: i32,
    message: &'static str,
}

async fn handle(
    State(state): State<McpState>,
    Extension(context): Extension<RequestContext>,
    body: Bytes,
) -> Response {
    let value: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(_) => return rpc_error(None, -32_700, "JSON-RPC parse error"),
    };
    let request: JsonRpcRequest = match serde_json::from_value(value) {
        Ok(request) => request,
        Err(_) => return rpc_error(None, -32_600, "invalid JSON-RPC request"),
    };
    if request.jsonrpc != "2.0" {
        return rpc_error(request.id, -32_600, "invalid JSON-RPC request");
    }
    if request.id.is_none() {
        return StatusCode::ACCEPTED.into_response();
    }
    let id = request.id.clone();
    let result = match request.method.as_str() {
        "initialize" => initialize(request.params),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(tools()),
        "tools/call" => call_tool(state, context, request.params).await,
        "resources/list" => Ok(resources()),
        "resources/read" => read_resource(request.params),
        _ => return rpc_error(id, -32_601, "method not found"),
    };
    match result {
        Ok(result) => rpc_result(id, result),
        Err(error) => rpc_error(id, error.code, error.message),
    }
}

#[derive(Debug, Deserialize)]
struct InitializeParams {
    #[serde(rename = "protocolVersion")]
    protocol_version: String,
}

fn initialize(params: Value) -> Result<Value, RpcFailure> {
    let params: InitializeParams = serde_json::from_value(params).map_err(|_| invalid_request())?;
    if params.protocol_version != MCP_PROTOCOL_VERSION {
        return Err(RpcFailure {
            code: -32_602,
            message: "MCP protocol version is unsupported",
        });
    }
    Ok(json!({
        "protocolVersion": MCP_PROTOCOL_VERSION,
        "capabilities": {
            "tools": { "listChanged": false },
            "resources": { "subscribe": false, "listChanged": false }
        },
        "serverInfo": { "name": "impossible-voice", "version": env!("CARGO_PKG_VERSION") }
    }))
}

fn tools() -> Value {
    json!({
        "tools": [
            {
                "name": "transcribe_audio",
                "description": "Transcribe bounded base64 WAV or raw PCM16 using the local model.",
                "inputSchema": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["audio_base64", "encoding"],
                    "properties": {
                        "audio_base64": { "type": "string", "maxLength": 5_592_408 },
                        "encoding": { "enum": ["wav", "pcm16"] },
                        "sample_rate": { "type": "integer", "minimum": 8000, "maximum": 48000 }
                    }
                }
            },
            {
                "name": "synthesize_speech",
                "description": "Synthesize bounded text with the local Kristin voice.",
                "inputSchema": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["text"],
                    "properties": {
                        "text": { "type": "string", "maxLength": 4096 },
                        "speed": { "type": "number", "minimum": 0.5, "maximum": 2.0 },
                        "format": { "enum": ["wav", "pcm16"] }
                    }
                }
            },
            {
                "name": "list_models",
                "description": "List the curated local STT model and TTS voice.",
                "inputSchema": { "type": "object", "additionalProperties": false }
            },
            {
                "name": "health",
                "description": "Return local engine readiness.",
                "inputSchema": { "type": "object", "additionalProperties": false }
            }
        ]
    })
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolCall {
    name: String,
    #[serde(default)]
    arguments: Value,
}

async fn call_tool(
    state: McpState,
    context: RequestContext,
    params: Value,
) -> Result<Value, RpcFailure> {
    let call: ToolCall = serde_json::from_value(params).map_err(|_| invalid_params())?;
    match call.name.as_str() {
        "transcribe_audio" => transcribe(state, context, call.arguments).await,
        "synthesize_speech" => synthesize(state, context, call.arguments).await,
        "list_models" => {
            require_empty(&call.arguments)?;
            let text = json!({
                "stt": "impossible-voice-stt",
                "tts": "impossible-voice-tts",
                "voice": "kristin"
            })
            .to_string();
            Ok(text_content(&text))
        }
        "health" => {
            require_empty(&call.arguments)?;
            Ok(text_content("{\"status\":\"ready\"}"))
        }
        _ => Err(RpcFailure {
            code: -32_602,
            message: "tool not found",
        }),
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TranscribeArguments {
    audio_base64: String,
    encoding: String,
    #[serde(default)]
    sample_rate: Option<u32>,
}

async fn transcribe(
    state: McpState,
    context: RequestContext,
    arguments: Value,
) -> Result<Value, RpcFailure> {
    let arguments: TranscribeArguments =
        serde_json::from_value(arguments).map_err(|_| invalid_params())?;
    if arguments.audio_base64.len() > 5_592_408 {
        return Err(invalid_params());
    }
    let bytes = STANDARD
        .decode(arguments.audio_base64)
        .map_err(|_| invalid_params())?;
    if bytes.len() > MAX_MCP_AUDIO_BYTES {
        return Err(invalid_params());
    }
    let audio = match arguments.encoding.as_str() {
        "wav" if arguments.sample_rate.is_none() => {
            MonoPcm::from_wav(&bytes, AudioLimits::default()).map_err(|_| invalid_params())?
        }
        "pcm16" => MonoPcm::from_pcm16_le(
            &bytes,
            arguments.sample_rate.ok_or_else(invalid_params)?,
            AudioLimits::default(),
        )
        .map_err(|_| invalid_params())?,
        _ => return Err(invalid_params()),
    };
    let text = tokio::task::spawn_blocking(move || state.backend.transcribe(&audio, &context))
        .await
        .map_err(|_| engine_failure())?
        .map_err(map_backend_error)?;
    Ok(text_content(&text))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SynthesizeArguments {
    text: String,
    #[serde(default = "default_speed")]
    speed: f32,
    #[serde(default = "default_format")]
    format: String,
}

const fn default_speed() -> f32 {
    1.0
}

fn default_format() -> String {
    "wav".to_owned()
}

async fn synthesize(
    state: McpState,
    context: RequestContext,
    arguments: Value,
) -> Result<Value, RpcFailure> {
    let arguments: SynthesizeArguments =
        serde_json::from_value(arguments).map_err(|_| invalid_params())?;
    if !matches!(arguments.format.as_str(), "wav" | "pcm16") {
        return Err(invalid_params());
    }
    let format = arguments.format;
    let synthesis = tokio::task::spawn_blocking(move || {
        state
            .backend
            .synthesize(&arguments.text, arguments.speed, &context)
    })
    .await
    .map_err(|_| engine_failure())?
    .map_err(map_backend_error)?;
    let (mime_type, bytes) = if format == "wav" {
        ("audio/wav", synthesis.wav().map_err(|_| engine_failure())?)
    } else {
        ("audio/pcm", synthesis.pcm16())
    };
    if bytes.len() > MAX_MCP_AUDIO_BYTES {
        return Err(RpcFailure {
            code: -32_003,
            message: "tool result exceeds the MCP limit",
        });
    }
    Ok(json!({
        "content": [{ "type": "audio", "data": STANDARD.encode(bytes), "mimeType": mime_type }],
        "isError": false
    }))
}

fn resources() -> Value {
    json!({
        "resources": [
            { "uri": "voice://capabilities", "name": "Voice capabilities", "mimeType": "application/json" },
            { "uri": "voice://status", "name": "Voice status", "mimeType": "application/json" }
        ]
    })
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadResource {
    uri: String,
}

fn read_resource(params: Value) -> Result<Value, RpcFailure> {
    let request: ReadResource = serde_json::from_value(params).map_err(|_| invalid_params())?;
    let text = match request.uri.as_str() {
        "voice://capabilities" => json!({
            "speech_to_text": true,
            "text_to_speech": true,
            "http": true,
            "websocket": true,
            "grpc": true,
            "mcp": true,
            "offline_after_setup": true
        }),
        "voice://status" => json!({ "status": "ready", "downloads_during_serve": false }),
        _ => {
            return Err(RpcFailure {
                code: -32_002,
                message: "resource not found",
            });
        }
    };
    Ok(json!({
        "contents": [{ "uri": request.uri, "mimeType": "application/json", "text": text.to_string() }]
    }))
}

fn require_empty(value: &Value) -> Result<(), RpcFailure> {
    if value.as_object().is_none_or(serde_json::Map::is_empty) {
        Ok(())
    } else {
        Err(invalid_params())
    }
}

fn text_content(text: &str) -> Value {
    json!({ "content": [{ "type": "text", "text": text }], "isError": false })
}

#[derive(Debug, Clone, Copy)]
struct RpcFailure {
    code: i32,
    message: &'static str,
}

const fn invalid_params() -> RpcFailure {
    RpcFailure {
        code: -32_602,
        message: "invalid tool parameters",
    }
}

const fn invalid_request() -> RpcFailure {
    RpcFailure {
        code: -32_600,
        message: "invalid JSON-RPC request",
    }
}

const fn engine_failure() -> RpcFailure {
    RpcFailure {
        code: -32_003,
        message: "local voice engine failed",
    }
}

fn map_backend_error(error: VoiceBackendError) -> RpcFailure {
    match error {
        VoiceBackendError::InvalidInput => invalid_params(),
        VoiceBackendError::Busy => RpcFailure {
            code: -32_003,
            message: "voice capacity is busy",
        },
        VoiceBackendError::Cancelled => RpcFailure {
            code: -32_800,
            message: "request cancelled",
        },
        VoiceBackendError::DeadlineExceeded => RpcFailure {
            code: -32_800,
            message: "request deadline exceeded",
        },
        VoiceBackendError::Engine => engine_failure(),
    }
}

fn rpc_result(id: Option<Value>, result: Value) -> Response {
    Json(JsonRpcResponse {
        jsonrpc: "2.0",
        id: id.unwrap_or(Value::Null),
        result: Some(result),
        error: None,
    })
    .into_response()
}

fn rpc_error(id: Option<Value>, code: i32, message: &'static str) -> Response {
    Json(JsonRpcResponse {
        jsonrpc: "2.0",
        id: id.unwrap_or(Value::Null),
        result: None,
        error: Some(JsonRpcError { code, message }),
    })
    .into_response()
}

#[cfg(test)]
mod tests {
    use std::{error::Error, sync::Arc, time::Duration};

    use axum::http::StatusCode;
    use base64::{Engine, engine::general_purpose::STANDARD};
    use impossible_server_core::{CancellationToken, RequestContext, ServerLimits};
    use impossible_voice_audio::MonoPcm;
    use impossible_voice_tts::Synthesis;
    use reqwest::Client;
    use serde_json::{Value, json};
    use tokio::{net::TcpListener, task::JoinHandle};

    use super::MCP_PROTOCOL_VERSION;
    use crate::voice_api::{RealtimeTranscriber, VoiceBackend, VoiceBackendError};
    use crate::{TemplateServer, VoiceEngineWorkload};

    struct FakeBackend;

    impl VoiceBackend for FakeBackend {
        fn transcribe(
            &self,
            _audio: &MonoPcm,
            _context: &RequestContext,
        ) -> Result<String, VoiceBackendError> {
            Ok("mcp transcript".to_owned())
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
            Err(VoiceBackendError::InvalidInput)
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
        Ok((format!("http://{address}/mcp"), shutdown, task))
    }

    async fn call(client: &Client, endpoint: &str, body: Value) -> Result<Value, Box<dyn Error>> {
        let response = client
            .post(endpoint)
            .header("content-type", "application/json")
            .body(body.to_string())
            .send()
            .await?;
        assert_eq!(response.status(), StatusCode::OK);
        Ok(serde_json::from_slice(&response.bytes().await?)?)
    }

    #[tokio::test]
    async fn real_mcp_network_supports_tools_resources_and_rejects_paths()
    -> Result<(), Box<dyn Error>> {
        let (endpoint, shutdown, task) = start_server().await?;
        let client = Client::new();
        let initialized = call(
            &client,
            &endpoint,
            rpc(
                1,
                "initialize",
                json!({ "protocolVersion": MCP_PROTOCOL_VERSION }),
            ),
        )
        .await?;
        assert_eq!(
            initialized["result"]["serverInfo"]["name"],
            "impossible-voice"
        );

        let listed = call(&client, &endpoint, rpc(2, "tools/list", json!({}))).await?;
        assert_eq!(listed["result"]["tools"].as_array().map(Vec::len), Some(4));

        let pcm = STANDARD.encode(vec![0_u8; 640]);
        let transcribed = call(
            &client,
            &endpoint,
            rpc(
                3,
                "tools/call",
                json!({
                    "name": "transcribe_audio",
                    "arguments": { "audio_base64": pcm, "encoding": "pcm16", "sample_rate": 16000 }
                }),
            ),
        )
        .await?;
        assert_eq!(
            transcribed["result"]["content"][0]["text"],
            "mcp transcript"
        );

        let synthesized = call(
            &client,
            &endpoint,
            rpc(
                4,
                "tools/call",
                json!({ "name": "synthesize_speech", "arguments": { "text": "hello", "format": "wav" } }),
            ),
        )
        .await?;
        let audio = synthesized["result"]["content"][0]["data"]
            .as_str()
            .ok_or("missing audio")?;
        assert_eq!(&STANDARD.decode(audio)?[..4], b"RIFF");

        let resource = call(
            &client,
            &endpoint,
            rpc(
                5,
                "resources/read",
                json!({ "uri": "voice://capabilities" }),
            ),
        )
        .await?;
        assert!(
            resource["result"]["contents"][0]["text"]
                .as_str()
                .is_some_and(|text| text.contains("speech_to_text"))
        );

        let rejected = call(
            &client,
            &endpoint,
            rpc(
                6,
                "tools/call",
                json!({
                    "name": "transcribe_audio",
                    "arguments": { "path": "private.wav", "encoding": "wav", "audio_base64": "" }
                }),
            ),
        )
        .await?;
        assert_eq!(rejected["error"]["code"], -32_602);

        let incompatible = call(
            &client,
            &endpoint,
            rpc(7, "initialize", json!({ "protocolVersion": "1900-01-01" })),
        )
        .await?;
        assert_eq!(incompatible["error"]["code"], -32_602);

        let malformed = client
            .post(&endpoint)
            .header("content-type", "application/json")
            .body("{")
            .send()
            .await?;
        assert_eq!(malformed.status(), StatusCode::OK);
        let malformed: Value = serde_json::from_slice(&malformed.bytes().await?)?;
        assert_eq!(malformed["error"]["code"], -32_700);

        let _ = shutdown.cancel();
        tokio::time::timeout(Duration::from_secs(2), task).await???;
        Ok(())
    }

    #[allow(clippy::needless_pass_by_value)]
    fn rpc(id: u64, method: &str, params: Value) -> Value {
        json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
    }
}
