# Curated artifact provenance and licenses

The v0.1 setup profile installs exactly one platform runtime, one English streaming ASR model, and
one English TTS voice. Archive byte sizes and SHA-256 digests in
`crates/impossible-voice-artifacts/manifests/curated-v1.json` were verified independently against
the downloaded release bytes. Setup rejects every size, digest, archive-layout, and required-file
mismatch before activation.

## sherpa-onnx runtime v1.13.8

- Source and binary publisher: [k2-fsa/sherpa-onnx](https://github.com/k2-fsa/sherpa-onnx/tree/v1.13.8)
- License: Apache-2.0, from the repository's pinned
  [license file](https://github.com/k2-fsa/sherpa-onnx/blob/v1.13.8/LICENSE)
- CPU packages: official Linux x86-64 shared library and Windows x86-64 MD Release shared library
  assets from the v1.13.8 release.
- Embedded ONNX Runtime 1.28.2: MIT, from its pinned
  [license file](https://github.com/microsoft/onnxruntime/blob/v1.28.2/LICENSE). The exact version is
  fixed by sherpa-onnx v1.13.8's platform build definitions and confirmed by binary metadata.
- Embedded Piper Phonemize revision `f3ff95afc03640bc1399e113e83361192a2fafb4`: MIT.
- Embedded eSpeak NG revision `ed530aa113046142eb5115cf2fc9157854d0ffe1`:
  GPL-3.0-or-later. Both immutable revisions are fixed by sherpa-onnx v1.13.8's official CMake
  definitions.

The curated sherpa-onnx runtime is therefore not a purely Apache-2.0 binary: TTS support embeds the
GPL-licensed eSpeak NG phonemizer. The repository source remains MIT OR Apache-2.0, while anyone who
redistributes a combined native package must separately satisfy the GPL and corresponding-source
requirements of the downloaded runtime. Setup downloads the upstream archives rather than
committing or republishing them. This is operational provenance, not legal advice.

## English streaming speech-to-text model

- Packager: the official sherpa-onnx `asr-models` release
- Artifact: `sherpa-onnx-nemo-streaming-fast-conformer-ctc-en-80ms-int8`
- Upstream model: NVIDIA
  [`stt_en_fastconformer_hybrid_large_streaming_multi`](https://huggingface.co/nvidia/stt_en_fastconformer_hybrid_large_streaming_multi),
  pinned to revision `ae98143333690bd7ced4bc8ec16769bcb8918374`
- License: CC-BY-4.0 as declared by the pinned upstream model card

The server must preserve the required attribution when redistribution packaging is implemented.

## English text-to-speech voice

- Packager: the official sherpa-onnx `tts-models` release
- Artifact: `vits-piper-en_US-kristin-medium-int8`
- Voice source: the official
  [`rhasspy/piper-voices`](https://huggingface.co/rhasspy/piper-voices) repository, pinned to revision
  `1162a9173d0ce503555aed757976b7a9912eae4c`
- Repository license: MIT
- Kristin training recordings: LibriVox public-domain recordings, as declared by the pinned
  [voice-specific model card](https://huggingface.co/rhasspy/piper-voices/blob/1162a9173d0ce503555aed757976b7a9912eae4c/en/en_US/kristin/medium/MODEL_CARD)
- Bundled `espeak-ng-data`: GPL-3.0-or-later at the same immutable eSpeak NG revision used by the
  runtime.

No Lessac-derived voice is included. Its referenced source-data license is research-only and failed
the production-use qualification gate.
