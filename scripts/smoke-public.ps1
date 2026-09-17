param(
    [string]$HttpBase = 'http://127.0.0.1:8080',
    [string]$WebSocketUrl = 'ws://127.0.0.1:8080/api/v1/realtime',
    [string]$WorkDirectory = '.shame/public-smoke'
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Send-WebSocketText([Net.WebSockets.ClientWebSocket]$Socket, [string]$Text) {
    $bytes = [Text.Encoding]::UTF8.GetBytes($Text)
    $segment = [ArraySegment[byte]]::new($bytes)
    $Socket.SendAsync($segment, [Net.WebSockets.WebSocketMessageType]::Text, $true, [Threading.CancellationToken]::None).GetAwaiter().GetResult() | Out-Null
}

function Receive-WebSocketText([Net.WebSockets.ClientWebSocket]$Socket) {
    $buffer = [byte[]]::new(65536)
    $stream = [IO.MemoryStream]::new()
    try {
        do {
            $segment = [ArraySegment[byte]]::new($buffer)
            $result = $Socket.ReceiveAsync($segment, [Threading.CancellationToken]::None).GetAwaiter().GetResult()
            if ($result.MessageType -eq [Net.WebSockets.WebSocketMessageType]::Close) {
                throw 'WebSocket closed before the expected event.'
            }
            $stream.Write($buffer, 0, $result.Count)
        } while (-not $result.EndOfMessage)
        if ($result.MessageType -ne [Net.WebSockets.WebSocketMessageType]::Text) {
            throw 'Expected a WebSocket text event.'
        }
        return [Text.Encoding]::UTF8.GetString($stream.ToArray())
    }
    finally {
        $stream.Dispose()
    }
}

function Get-WavPcm([byte[]]$Wav) {
    if ($Wav.Length -lt 44 -or [Text.Encoding]::ASCII.GetString($Wav, 0, 4) -ne 'RIFF') {
        throw 'TTS did not return a valid RIFF WAV.'
    }
    $offset = 12
    while ($offset + 8 -le $Wav.Length) {
        $name = [Text.Encoding]::ASCII.GetString($Wav, $offset, 4)
        $length = [BitConverter]::ToUInt32($Wav, $offset + 4)
        $dataStart = $offset + 8
        if ($name -eq 'data') {
            if ($dataStart + $length -gt $Wav.Length) { throw 'WAV data chunk is truncated.' }
            $pcm = [byte[]]::new($length)
            [Array]::Copy($Wav, $dataStart, $pcm, 0, $length)
            return $pcm
        }
        $offset = $dataStart + $length + ($length % 2)
    }
    throw 'WAV data chunk is missing.'
}

New-Item -ItemType Directory -Path $WorkDirectory -Force | Out-Null
$wavPath = Join-Path $WorkDirectory 'speech.wav'
$speechBody = @{ input = 'The quick brown fox jumps over the lazy dog.'; voice = 'kristin'; response_format = 'wav' } | ConvertTo-Json -Compress
Invoke-WebRequest -Uri "$HttpBase/v1/audio/speech" -Method Post -ContentType 'application/json' -Body $speechBody -OutFile $wavPath | Out-Null
$wav = [IO.File]::ReadAllBytes($wavPath)
$pcm = Get-WavPcm $wav

$transcription = Invoke-RestMethod -Uri "$HttpBase/api/v1/transcriptions" -Method Post -ContentType 'audio/wav' -Body $wav
if ([string]::IsNullOrWhiteSpace([string]$transcription.text)) {
    throw 'HTTP STT returned an empty transcript.'
}

$mcpBody = @{
    jsonrpc = '2.0'
    id = 1
    method = 'initialize'
    params = @{ protocolVersion = '2025-03-26'; capabilities = @{}; clientInfo = @{ name = 'release-smoke'; version = '1' } }
} | ConvertTo-Json -Depth 5 -Compress
$mcp = Invoke-RestMethod -Uri "$HttpBase/mcp" -Method Post -ContentType 'application/json' -Body $mcpBody
if ($mcp.result.protocolVersion -ne '2025-03-26') {
    throw 'MCP initialization did not negotiate the pinned version.'
}

$socket = [Net.WebSockets.ClientWebSocket]::new()
try {
    $socket.ConnectAsync([Uri]$WebSocketUrl, [Threading.CancellationToken]::None).GetAwaiter().GetResult() | Out-Null
    Send-WebSocketText $socket '{"type":"session_start","mode":"stt","sample_rate":22050}'
    if ((Receive-WebSocketText $socket | ConvertFrom-Json).type -ne 'session_ready') {
        throw 'WebSocket STT did not become ready.'
    }
    for ($offset = 0; $offset -lt $pcm.Length; $offset += 64000) {
        $length = [Math]::Min(64000, $pcm.Length - $offset)
        if ($length % 2 -ne 0) { $length-- }
        Send-WebSocketText $socket (@{ type = 'audio_append'; bytes = $length } | ConvertTo-Json -Compress)
        $segment = [ArraySegment[byte]]::new($pcm, $offset, $length)
        $socket.SendAsync($segment, [Net.WebSockets.WebSocketMessageType]::Binary, $true, [Threading.CancellationToken]::None).GetAwaiter().GetResult() | Out-Null
    }
    Send-WebSocketText $socket '{"type":"audio_commit"}'
    $final = $null
    $completed = $false
    for ($eventIndex = 0; $eventIndex -lt 100 -and -not $completed; $eventIndex++) {
        $event = Receive-WebSocketText $socket | ConvertFrom-Json
        if ($event.type -eq 'error') { throw "WebSocket STT failed: $($event.code)" }
        if ($event.type -eq 'transcript_final') { $final = [string]$event.text }
        if ($event.type -eq 'session_completed') { $completed = $true }
    }
    if (-not $completed -or [string]::IsNullOrWhiteSpace($final)) {
        throw 'WebSocket STT did not return a final transcript and completion event.'
    }
}
finally {
    $socket.Dispose()
}

Write-Output 'Real public HTTP TTS/STT, WebSocket STT, and MCP smoke passed.'
