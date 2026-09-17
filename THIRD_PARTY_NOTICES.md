# Third-party notices

Impossible Voice source packages do not contain model weights or the sherpa-onnx native runtime.
The `setup` command downloads the following pinned upstream artifacts directly into the local,
ignored artifact store. See [`docs/artifact-licenses.md`](docs/artifact-licenses.md) for immutable
revisions, provenance, and qualification details.

| Component | Use | License |
| --- | --- | --- |
| sherpa-onnx 1.13.8 | Native STT/TTS API | Apache-2.0 |
| ONNX Runtime 1.28.2 | Inference runtime embedded by sherpa-onnx | MIT |
| Piper Phonemize | Phonemizer embedded by sherpa-onnx | MIT |
| eSpeak NG | Embedded phonemizer and TTS language data | GPL-3.0-or-later |
| NVIDIA NeMo streaming FastConformer model | English speech recognition | CC-BY-4.0 |
| Piper Kristin voice | English speech synthesis | MIT repository; public-domain recordings |

The official sherpa-onnx TTS-enabled binary embeds GPL-licensed eSpeak NG. Impossible Voice does
not redistribute that binary in its source or native server archives: setup downloads it from the
upstream release. Anyone redistributing a combined bundle must independently satisfy the applicable
GPL source and notice obligations. This notice is operational provenance, not legal advice.
