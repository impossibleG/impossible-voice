//! Isolated dynamic FFI boundary for the pinned sherpa-onnx v1.13.8 C API.
//!
//! Unsafe code is confined to this crate. Public handles validate lengths, pointers, UTF-8,
//! ownership, and library version before exposing owned Rust values.

use std::{
    error::Error,
    ffi::{CStr, CString, c_char, c_void},
    fmt,
    path::Path,
    sync::{Arc, Mutex, MutexGuard},
};

use libloading::Library;

const EXPECTED_VERSION: &str = "1.13.8";
const MAX_DECODE_STEPS: usize = 100_000;

/// Sanitized native runtime failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeError(&'static str);

impl NativeError {
    const fn new(message: &'static str) -> Self {
        Self(message)
    }
}

impl fmt::Display for NativeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}

impl Error for NativeError {}

#[repr(C)]
#[derive(Clone, Copy)]
struct OnlineTransducerModelConfig {
    encoder: *const c_char,
    decoder: *const c_char,
    joiner: *const c_char,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct OnlineParaformerModelConfig {
    encoder: *const c_char,
    decoder: *const c_char,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct OnlineZipformer2CtcModelConfig {
    model: *const c_char,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct OnlineNemoCtcModelConfig {
    model: *const c_char,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct OnlineToneCtcModelConfig {
    model: *const c_char,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct OnlineModelConfig {
    transducer: OnlineTransducerModelConfig,
    paraformer: OnlineParaformerModelConfig,
    zipformer2_ctc: OnlineZipformer2CtcModelConfig,
    tokens: *const c_char,
    num_threads: i32,
    provider: *const c_char,
    debug: i32,
    model_type: *const c_char,
    modeling_unit: *const c_char,
    bpe_vocab: *const c_char,
    tokens_buf: *const c_char,
    tokens_buf_size: i32,
    nemo_ctc: OnlineNemoCtcModelConfig,
    t_one_ctc: OnlineToneCtcModelConfig,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct FeatureConfig {
    sample_rate: i32,
    feature_dim: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct OnlineCtcFstDecoderConfig {
    graph: *const c_char,
    max_active: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct HomophoneReplacerConfig {
    dict_dir: *const c_char,
    lexicon: *const c_char,
    rule_fsts: *const c_char,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct OnlineRecognizerConfig {
    feat_config: FeatureConfig,
    model_config: OnlineModelConfig,
    decoding_method: *const c_char,
    max_active_paths: i32,
    enable_endpoint: i32,
    rule1_min_trailing_silence: f32,
    rule2_min_trailing_silence: f32,
    rule3_min_utterance_length: f32,
    hotwords_file: *const c_char,
    hotwords_score: f32,
    ctc_fst_decoder_config: OnlineCtcFstDecoderConfig,
    rule_fsts: *const c_char,
    rule_fars: *const c_char,
    blank_penalty: f32,
    hotwords_buf: *const c_char,
    hotwords_buf_size: i32,
    hr: HomophoneReplacerConfig,
}

#[repr(C)]
struct OnlineRecognizerResult {
    text: *const c_char,
    tokens: *const c_char,
    tokens_arr: *const *const c_char,
    timestamps: *mut f32,
    count: i32,
    json: *const c_char,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct OfflineTtsVitsModelConfig {
    model: *const c_char,
    lexicon: *const c_char,
    tokens: *const c_char,
    data_dir: *const c_char,
    noise_scale: f32,
    noise_scale_w: f32,
    length_scale: f32,
    dict_dir: *const c_char,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct OfflineTtsMatchaModelConfig {
    acoustic_model: *const c_char,
    vocoder: *const c_char,
    lexicon: *const c_char,
    tokens: *const c_char,
    data_dir: *const c_char,
    noise_scale: f32,
    length_scale: f32,
    dict_dir: *const c_char,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct OfflineTtsKokoroModelConfig {
    model: *const c_char,
    voices: *const c_char,
    tokens: *const c_char,
    data_dir: *const c_char,
    length_scale: f32,
    dict_dir: *const c_char,
    lexicon: *const c_char,
    lang: *const c_char,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct OfflineTtsKittenModelConfig {
    model: *const c_char,
    voices: *const c_char,
    tokens: *const c_char,
    data_dir: *const c_char,
    length_scale: f32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct OfflineTtsZipvoiceModelConfig {
    tokens: *const c_char,
    encoder: *const c_char,
    decoder: *const c_char,
    vocoder: *const c_char,
    data_dir: *const c_char,
    lexicon: *const c_char,
    feat_scale: f32,
    t_shift: f32,
    target_rms: f32,
    guidance_scale: f32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct OfflineTtsPocketModelConfig {
    lm_flow: *const c_char,
    lm_main: *const c_char,
    encoder: *const c_char,
    decoder: *const c_char,
    text_conditioner: *const c_char,
    vocab_json: *const c_char,
    token_scores_json: *const c_char,
    voice_embedding_cache_capacity: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct OfflineTtsSupertonicModelConfig {
    duration_predictor: *const c_char,
    text_encoder: *const c_char,
    vector_estimator: *const c_char,
    vocoder: *const c_char,
    tts_json: *const c_char,
    unicode_indexer: *const c_char,
    voice_style: *const c_char,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct OfflineTtsModelConfig {
    vits: OfflineTtsVitsModelConfig,
    num_threads: i32,
    debug: i32,
    provider: *const c_char,
    matcha: OfflineTtsMatchaModelConfig,
    kokoro: OfflineTtsKokoroModelConfig,
    kitten: OfflineTtsKittenModelConfig,
    zipvoice: OfflineTtsZipvoiceModelConfig,
    pocket: OfflineTtsPocketModelConfig,
    supertonic: OfflineTtsSupertonicModelConfig,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct OfflineTtsConfig {
    model: OfflineTtsModelConfig,
    rule_fsts: *const c_char,
    max_num_sentences: i32,
    rule_fars: *const c_char,
    silence_scale: f32,
}

#[repr(C)]
struct GeneratedAudioRaw {
    samples: *const f32,
    n: i32,
    sample_rate: i32,
}

type GetString = unsafe extern "C" fn() -> *const c_char;
type CreateOnline = unsafe extern "C" fn(*const OnlineRecognizerConfig) -> *const c_void;
type DestroyOnline = unsafe extern "C" fn(*const c_void);
type CreateStream = unsafe extern "C" fn(*const c_void) -> *const c_void;
type DestroyStream = unsafe extern "C" fn(*const c_void);
type AcceptWaveform = unsafe extern "C" fn(*const c_void, i32, *const f32, i32);
type IsReady = unsafe extern "C" fn(*const c_void, *const c_void) -> i32;
type Decode = unsafe extern "C" fn(*const c_void, *const c_void);
type InputFinished = unsafe extern "C" fn(*const c_void);
type GetResult =
    unsafe extern "C" fn(*const c_void, *const c_void) -> *const OnlineRecognizerResult;
type DestroyResult = unsafe extern "C" fn(*const OnlineRecognizerResult);
type CreateTts = unsafe extern "C" fn(*const OfflineTtsConfig) -> *const c_void;
type DestroyTts = unsafe extern "C" fn(*const c_void);
type GenerateTts =
    unsafe extern "C" fn(*const c_void, *const c_char, i32, f32) -> *const GeneratedAudioRaw;
type DestroyGeneratedAudio = unsafe extern "C" fn(*const GeneratedAudioRaw);

struct Api {
    _dependencies: Vec<Library>,
    _library: Library,
    get_version: GetString,
    create_online: CreateOnline,
    destroy_online: DestroyOnline,
    create_stream: CreateStream,
    destroy_stream: DestroyStream,
    accept_waveform: AcceptWaveform,
    is_ready: IsReady,
    decode: Decode,
    input_finished: InputFinished,
    get_result: GetResult,
    destroy_result: DestroyResult,
    create_tts: CreateTts,
    destroy_tts: DestroyTts,
    generate_tts: GenerateTts,
    destroy_generated_audio: DestroyGeneratedAudio,
}

impl Api {
    fn load(runtime_root: &Path) -> Result<Self, NativeError> {
        let library_dir = runtime_root.join("lib");
        let mut dependencies = Vec::new();
        for dependency in dependency_names() {
            let path = library_dir.join(dependency);
            if path.exists() {
                dependencies.push(load_library(&path)?);
            }
        }
        let library = load_library(&library_dir.join(sherpa_library_name()))?;
        unsafe {
            Ok(Self {
                get_version: load_symbol(&library, b"SherpaOnnxGetVersionStr\0")?,
                create_online: load_symbol(&library, b"SherpaOnnxCreateOnlineRecognizer\0")?,
                destroy_online: load_symbol(&library, b"SherpaOnnxDestroyOnlineRecognizer\0")?,
                create_stream: load_symbol(&library, b"SherpaOnnxCreateOnlineStream\0")?,
                destroy_stream: load_symbol(&library, b"SherpaOnnxDestroyOnlineStream\0")?,
                accept_waveform: load_symbol(&library, b"SherpaOnnxOnlineStreamAcceptWaveform\0")?,
                is_ready: load_symbol(&library, b"SherpaOnnxIsOnlineStreamReady\0")?,
                decode: load_symbol(&library, b"SherpaOnnxDecodeOnlineStream\0")?,
                input_finished: load_symbol(&library, b"SherpaOnnxOnlineStreamInputFinished\0")?,
                get_result: load_symbol(&library, b"SherpaOnnxGetOnlineStreamResult\0")?,
                destroy_result: load_symbol(
                    &library,
                    b"SherpaOnnxDestroyOnlineRecognizerResult\0",
                )?,
                create_tts: load_symbol(&library, b"SherpaOnnxCreateOfflineTts\0")?,
                destroy_tts: load_symbol(&library, b"SherpaOnnxDestroyOfflineTts\0")?,
                generate_tts: load_symbol(&library, b"SherpaOnnxOfflineTtsGenerate\0")?,
                destroy_generated_audio: load_symbol(
                    &library,
                    b"SherpaOnnxDestroyOfflineTtsGeneratedAudio\0",
                )?,
                _dependencies: dependencies,
                _library: library,
            })
        }
    }
}

/// Loaded, version-checked sherpa-onnx runtime.
#[derive(Clone)]
pub struct SherpaRuntime(Arc<Api>);

impl fmt::Debug for SherpaRuntime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SherpaRuntime")
            .finish_non_exhaustive()
    }
}

impl SherpaRuntime {
    /// Loads the shared runtime and verifies its API version.
    ///
    /// # Errors
    /// Returns a sanitized error for missing dependencies, symbols, or a version mismatch.
    pub fn load(runtime_root: &Path) -> Result<Self, NativeError> {
        let api = Api::load(runtime_root)?;
        let version = read_static_string(unsafe { (api.get_version)() })?;
        if version != EXPECTED_VERSION {
            return Err(NativeError::new("native runtime version is incompatible"));
        }
        Ok(Self(Arc::new(api)))
    }

    /// Creates the pinned English `NeMo` streaming CTC recognizer.
    ///
    /// # Errors
    /// Rejects non-UTF-8/NUL paths, invalid thread counts, or native initialization failure.
    pub fn create_nemo_recognizer(
        &self,
        model: &Path,
        tokens: &Path,
        num_threads: u16,
    ) -> Result<OnlineRecognizer, NativeError> {
        if num_threads == 0 || num_threads > 32 {
            return Err(NativeError::new("native thread count is invalid"));
        }
        let model = path_to_cstring(model)?;
        let tokens = path_to_cstring(tokens)?;
        let provider =
            CString::new("cpu").map_err(|_| NativeError::new("native configuration is invalid"))?;
        let decoding = CString::new("greedy_search")
            .map_err(|_| NativeError::new("native configuration is invalid"))?;
        let mut config = zeroed::<OnlineRecognizerConfig>();
        config.feat_config.sample_rate = 16_000;
        config.feat_config.feature_dim = 80;
        config.model_config.nemo_ctc.model = model.as_ptr();
        config.model_config.tokens = tokens.as_ptr();
        config.model_config.num_threads = i32::from(num_threads);
        config.model_config.provider = provider.as_ptr();
        config.decoding_method = decoding.as_ptr();
        config.max_active_paths = 4;
        let pointer = unsafe { (self.0.create_online)(&raw const config) };
        if pointer.is_null() {
            return Err(NativeError::new("speech recognizer initialization failed"));
        }
        Ok(OnlineRecognizer(Arc::new(RecognizerInner {
            runtime: self.clone(),
            pointer: pointer as usize,
            gate: Mutex::new(()),
        })))
    }

    /// Creates the pinned English Kristin VITS synthesizer.
    ///
    /// # Errors
    /// Rejects invalid paths/thread counts or native initialization failure.
    pub fn create_vits_tts(
        &self,
        model: &Path,
        tokens: &Path,
        data_dir: &Path,
        num_threads: u16,
    ) -> Result<OfflineTts, NativeError> {
        if num_threads == 0 || num_threads > 32 {
            return Err(NativeError::new("native thread count is invalid"));
        }
        let model = path_to_cstring(model)?;
        let tokens = path_to_cstring(tokens)?;
        let data_dir = path_to_cstring(data_dir)?;
        let provider =
            CString::new("cpu").map_err(|_| NativeError::new("native configuration is invalid"))?;
        let mut config = zeroed::<OfflineTtsConfig>();
        config.model.vits.model = model.as_ptr();
        config.model.vits.tokens = tokens.as_ptr();
        config.model.vits.data_dir = data_dir.as_ptr();
        config.model.vits.noise_scale = 0.667;
        config.model.vits.noise_scale_w = 0.8;
        config.model.vits.length_scale = 1.0;
        config.model.num_threads = i32::from(num_threads);
        config.model.provider = provider.as_ptr();
        config.max_num_sentences = 1;
        config.silence_scale = 0.2;
        let pointer = unsafe { (self.0.create_tts)(&raw const config) };
        if pointer.is_null() {
            return Err(NativeError::new("speech synthesizer initialization failed"));
        }
        Ok(OfflineTts {
            runtime: self.clone(),
            pointer: pointer as usize,
            gate: Mutex::new(()),
        })
    }
}

struct RecognizerInner {
    runtime: SherpaRuntime,
    pointer: usize,
    gate: Mutex<()>,
}

impl Drop for RecognizerInner {
    fn drop(&mut self) {
        unsafe { (self.runtime.0.destroy_online)(self.pointer as *const c_void) };
    }
}

/// Reusable online recognizer. Calls are serialized because upstream does not promise reentrancy.
#[derive(Clone)]
pub struct OnlineRecognizer(Arc<RecognizerInner>);

impl fmt::Debug for OnlineRecognizer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OnlineRecognizer")
            .finish_non_exhaustive()
    }
}

impl OnlineRecognizer {
    /// Creates an independent decoder stream.
    ///
    /// # Errors
    /// Returns a sanitized error if native stream allocation fails.
    pub fn create_stream(&self) -> Result<OnlineStream, NativeError> {
        let _guard = lock(&self.0.gate)?;
        let pointer = unsafe { (self.0.runtime.0.create_stream)(self.0.pointer as *const c_void) };
        if pointer.is_null() {
            return Err(NativeError::new("speech stream initialization failed"));
        }
        Ok(OnlineStream {
            recognizer: self.clone(),
            pointer: pointer as usize,
            finished: false,
        })
    }
}

/// One owned online recognition stream.
pub struct OnlineStream {
    recognizer: OnlineRecognizer,
    pointer: usize,
    finished: bool,
}

impl fmt::Debug for OnlineStream {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OnlineStream")
            .field("finished", &self.finished)
            .finish_non_exhaustive()
    }
}

impl OnlineStream {
    /// Appends normalized mono 16 kHz PCM.
    ///
    /// # Errors
    /// Rejects empty/invalid/oversized chunks or appends after finish.
    pub fn accept(&mut self, samples: &[f32]) -> Result<(), NativeError> {
        if self.finished
            || samples.is_empty()
            || samples.len() > i32::MAX as usize
            || samples.iter().any(|sample| !sample.is_finite())
        {
            return Err(NativeError::new("speech stream input is invalid"));
        }
        let length = i32::try_from(samples.len())
            .map_err(|_| NativeError::new("speech stream input is invalid"))?;
        unsafe {
            (self.recognizer.0.runtime.0.accept_waveform)(
                self.pointer as *const c_void,
                16_000,
                samples.as_ptr(),
                length,
            );
        }
        self.decode_available()
    }

    /// Returns an owned snapshot of the current interim text.
    ///
    /// # Errors
    /// Returns a sanitized error for invalid native output.
    pub fn text(&self) -> Result<String, NativeError> {
        let _guard = lock(&self.recognizer.0.gate)?;
        let result = unsafe {
            (self.recognizer.0.runtime.0.get_result)(
                self.recognizer.0.pointer as *const c_void,
                self.pointer as *const c_void,
            )
        };
        if result.is_null() {
            return Err(NativeError::new("speech recognition result is unavailable"));
        }
        let text = unsafe { read_owned_string((*result).text) };
        unsafe { (self.recognizer.0.runtime.0.destroy_result)(result) };
        text
    }

    /// Signals end-of-input, drains ready decoder work, and returns final text.
    ///
    /// # Errors
    /// Rejects repeated finish or invalid native output.
    pub fn finish(&mut self) -> Result<String, NativeError> {
        if self.finished {
            return Err(NativeError::new("speech stream is already finished"));
        }
        self.finished = true;
        unsafe {
            (self.recognizer.0.runtime.0.input_finished)(self.pointer as *const c_void);
        }
        self.decode_available()?;
        self.text()
    }

    fn decode_available(&self) -> Result<(), NativeError> {
        let _guard = lock(&self.recognizer.0.gate)?;
        for _ in 0..MAX_DECODE_STEPS {
            let ready = unsafe {
                (self.recognizer.0.runtime.0.is_ready)(
                    self.recognizer.0.pointer as *const c_void,
                    self.pointer as *const c_void,
                )
            };
            if ready == 0 {
                return Ok(());
            }
            unsafe {
                (self.recognizer.0.runtime.0.decode)(
                    self.recognizer.0.pointer as *const c_void,
                    self.pointer as *const c_void,
                );
            }
        }
        Err(NativeError::new("speech decoder exceeded its work bound"))
    }
}

impl Drop for OnlineStream {
    fn drop(&mut self) {
        unsafe {
            (self.recognizer.0.runtime.0.destroy_stream)(self.pointer as *const c_void);
        }
    }
}

/// Owned synthesized mono audio.
#[derive(Debug, Clone, PartialEq)]
pub struct GeneratedAudio {
    /// Normalized mono samples.
    pub samples: Vec<f32>,
    /// Native output sample rate.
    pub sample_rate: u32,
}

/// Loaded VITS synthesizer with serialized native access.
pub struct OfflineTts {
    runtime: SherpaRuntime,
    pointer: usize,
    gate: Mutex<()>,
}

impl fmt::Debug for OfflineTts {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("OfflineTts").finish_non_exhaustive()
    }
}

impl OfflineTts {
    /// Synthesizes one bounded UTF-8 string with the pinned speaker.
    ///
    /// # Errors
    /// Rejects empty/NUL text, invalid speed, native failure, or invalid native output.
    pub fn generate(
        &self,
        text: &str,
        speed: f32,
        max_samples: usize,
    ) -> Result<GeneratedAudio, NativeError> {
        if text.trim().is_empty()
            || text.len() > 16_384
            || !speed.is_finite()
            || !(0.5..=2.0).contains(&speed)
            || max_samples == 0
        {
            return Err(NativeError::new("speech synthesis input is invalid"));
        }
        let text = CString::new(text)
            .map_err(|_| NativeError::new("speech synthesis input is invalid"))?;
        let _guard = lock(&self.gate)?;
        let result = unsafe {
            (self.runtime.0.generate_tts)(self.pointer as *const c_void, text.as_ptr(), 0, speed)
        };
        if result.is_null() {
            return Err(NativeError::new("speech synthesis failed"));
        }
        let audio = unsafe { copy_generated_audio(result, max_samples) };
        unsafe { (self.runtime.0.destroy_generated_audio)(result) };
        audio
    }
}

impl Drop for OfflineTts {
    fn drop(&mut self) {
        unsafe { (self.runtime.0.destroy_tts)(self.pointer as *const c_void) };
    }
}

unsafe fn copy_generated_audio(
    pointer: *const GeneratedAudioRaw,
    max_samples: usize,
) -> Result<GeneratedAudio, NativeError> {
    let raw = unsafe { &*pointer };
    if raw.samples.is_null() || raw.n <= 0 || raw.sample_rate <= 0 {
        return Err(NativeError::new("speech synthesis output is invalid"));
    }
    let length = usize::try_from(raw.n)
        .map_err(|_| NativeError::new("speech synthesis output is invalid"))?;
    if length > max_samples {
        return Err(NativeError::new(
            "speech synthesis output exceeds its limit",
        ));
    }
    let samples = unsafe { std::slice::from_raw_parts(raw.samples, length) }.to_vec();
    if samples.iter().any(|sample| !sample.is_finite()) {
        return Err(NativeError::new("speech synthesis output is invalid"));
    }
    let sample_rate = u32::try_from(raw.sample_rate)
        .map_err(|_| NativeError::new("speech synthesis output is invalid"))?;
    Ok(GeneratedAudio {
        samples,
        sample_rate,
    })
}

fn path_to_cstring(path: &Path) -> Result<CString, NativeError> {
    let text = path
        .to_str()
        .ok_or_else(|| NativeError::new("native artifact path is invalid"))?;
    CString::new(text).map_err(|_| NativeError::new("native artifact path is invalid"))
}

fn lock(mutex: &Mutex<()>) -> Result<MutexGuard<'_, ()>, NativeError> {
    mutex
        .lock()
        .map_err(|_| NativeError::new("native engine lock is unavailable"))
}

fn read_static_string(pointer: *const c_char) -> Result<String, NativeError> {
    unsafe { read_owned_string(pointer) }
}

unsafe fn read_owned_string(pointer: *const c_char) -> Result<String, NativeError> {
    if pointer.is_null() {
        return Err(NativeError::new("native runtime returned invalid text"));
    }
    unsafe { CStr::from_ptr(pointer) }
        .to_str()
        .map(str::to_owned)
        .map_err(|_| NativeError::new("native runtime returned invalid text"))
}

fn zeroed<T>() -> T {
    // All sherpa configuration structs are C POD records explicitly documented to be zero-filled.
    unsafe { std::mem::zeroed() }
}

unsafe fn load_symbol<T: Copy>(library: &Library, name: &[u8]) -> Result<T, NativeError> {
    unsafe { library.get::<T>(name) }
        .map(|symbol| *symbol)
        .map_err(|_| NativeError::new("native runtime symbol is missing"))
}

fn load_library(path: &Path) -> Result<Library, NativeError> {
    if !path.is_file() {
        return Err(NativeError::new("native runtime library is missing"));
    }
    unsafe { Library::new(path) }
        .map_err(|_| NativeError::new("native runtime library could not be loaded"))
}

#[cfg(target_os = "windows")]
const fn sherpa_library_name() -> &'static str {
    "sherpa-onnx-c-api.dll"
}

#[cfg(target_os = "linux")]
const fn sherpa_library_name() -> &'static str {
    "libsherpa-onnx-c-api.so"
}

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
const fn sherpa_library_name() -> &'static str {
    "unsupported"
}

#[cfg(target_os = "windows")]
fn dependency_names() -> &'static [&'static str] {
    &["onnxruntime.dll", "onnxruntime_providers_shared.dll"]
}

#[cfg(target_os = "linux")]
fn dependency_names() -> &'static [&'static str] {
    &["libonnxruntime.so", "libonnxruntime_providers_shared.so"]
}

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
fn dependency_names() -> &'static [&'static str] {
    &[]
}

#[cfg(test)]
mod tests {
    use std::{
        mem::{align_of, size_of},
        path::PathBuf,
    };

    use super::{OfflineTtsConfig, OnlineRecognizerConfig, SherpaRuntime};

    #[test]
    fn ffi_configuration_layouts_are_nonzero_and_pointer_aligned() {
        assert!(size_of::<OnlineRecognizerConfig>() > 128);
        assert!(size_of::<OfflineTtsConfig>() > 256);
        assert_eq!(align_of::<OnlineRecognizerConfig>(), align_of::<usize>());
        assert_eq!(align_of::<OfflineTtsConfig>(), align_of::<usize>());
    }

    #[test]
    #[ignore = "requires a locally installed curated native runtime"]
    fn real_runtime_loads_at_the_pinned_version() -> Result<(), Box<dyn std::error::Error>> {
        let root = std::env::var_os("IMPOSSIBLE_VOICE_TEST_RUNTIME_ROOT")
            .map(PathBuf::from)
            .ok_or("test runtime root is not configured")?;
        let _runtime = SherpaRuntime::load(&root)?;
        Ok(())
    }
}
