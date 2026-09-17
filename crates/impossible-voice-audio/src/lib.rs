//! Bounded mono PCM admission, WAV decoding, deterministic resampling, and voice activity.

use std::{error::Error, fmt};

const SUPPORTED_SAMPLE_RATES: [u32; 7] = [8_000, 16_000, 22_050, 24_000, 32_000, 44_100, 48_000];

/// Sanitized audio admission or conversion failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioError(&'static str);

impl AudioError {
    const fn new(message: &'static str) -> Self {
        Self(message)
    }
}

impl fmt::Display for AudioError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}

impl Error for AudioError {}

/// Admission limits applied before allocating decoded audio.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioLimits {
    max_encoded_bytes: usize,
    max_duration_ms: u64,
}

impl AudioLimits {
    /// Creates non-zero encoded-size and duration bounds.
    ///
    /// # Errors
    /// Returns a sanitized error when either bound is zero.
    pub const fn new(max_encoded_bytes: usize, max_duration_ms: u64) -> Result<Self, AudioError> {
        if max_encoded_bytes == 0 || max_duration_ms == 0 {
            return Err(AudioError::new("audio limits must be non-zero"));
        }
        Ok(Self {
            max_encoded_bytes,
            max_duration_ms,
        })
    }

    fn admit(
        self,
        encoded_bytes: usize,
        sample_rate: u32,
        samples: usize,
    ) -> Result<(), AudioError> {
        if encoded_bytes > self.max_encoded_bytes {
            return Err(AudioError::new("encoded audio exceeds its byte limit"));
        }
        let duration_numerator = u64::try_from(samples)
            .ok()
            .and_then(|count| count.checked_mul(1_000))
            .ok_or_else(|| AudioError::new("audio duration exceeds its limit"))?;
        let duration_limit = u64::from(sample_rate)
            .checked_mul(self.max_duration_ms)
            .ok_or_else(|| AudioError::new("audio duration exceeds its limit"))?;
        if duration_numerator > duration_limit {
            return Err(AudioError::new("audio duration exceeds its limit"));
        }
        Ok(())
    }
}

impl Default for AudioLimits {
    fn default() -> Self {
        Self {
            max_encoded_bytes: 32 * 1024 * 1024,
            max_duration_ms: 5 * 60 * 1_000,
        }
    }
}

/// Validated mono floating-point PCM.
#[derive(Debug, Clone, PartialEq)]
pub struct MonoPcm {
    sample_rate: u32,
    samples: Vec<f32>,
}

impl MonoPcm {
    /// Validates normalized mono PCM at a supported sample rate.
    ///
    /// # Errors
    /// Rejects empty, non-finite, out-of-range, or unsupported audio.
    pub fn new(sample_rate: u32, samples: Vec<f32>) -> Result<Self, AudioError> {
        validate_rate(sample_rate)?;
        if samples.is_empty() {
            return Err(AudioError::new("audio must contain samples"));
        }
        if samples
            .iter()
            .any(|sample| !sample.is_finite() || !(-1.0..=1.0).contains(sample))
        {
            return Err(AudioError::new("audio samples are invalid"));
        }
        Ok(Self {
            sample_rate,
            samples,
        })
    }

    /// Decodes raw little-endian signed PCM16 mono.
    ///
    /// # Errors
    /// Rejects unsupported rates, odd byte lengths, empty audio, and configured bounds.
    pub fn from_pcm16_le(
        bytes: &[u8],
        sample_rate: u32,
        limits: AudioLimits,
    ) -> Result<Self, AudioError> {
        validate_rate(sample_rate)?;
        if bytes.is_empty() || bytes.len() % 2 != 0 {
            return Err(AudioError::new("PCM16 audio has an invalid byte length"));
        }
        limits.admit(bytes.len(), sample_rate, bytes.len() / 2)?;
        let samples = bytes
            .chunks_exact(2)
            .map(|chunk| f32::from(i16::from_le_bytes([chunk[0], chunk[1]])) / 32_768.0)
            .collect();
        Self::new(sample_rate, samples)
    }

    /// Decodes a strict mono PCM16 RIFF/WAVE file.
    ///
    /// # Errors
    /// Rejects malformed containers, unsupported encodings, and configured bounds.
    pub fn from_wav(bytes: &[u8], limits: AudioLimits) -> Result<Self, AudioError> {
        if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
            return Err(AudioError::new("WAV container is malformed"));
        }
        limits.admit(bytes.len(), 48_000, 0)?;
        let declared = usize::try_from(read_u32(bytes, 4)?)
            .ok()
            .and_then(|size| size.checked_add(8))
            .ok_or_else(|| AudioError::new("WAV container is malformed"))?;
        if declared != bytes.len() {
            return Err(AudioError::new("WAV container length is invalid"));
        }

        let mut offset = 12_usize;
        let mut format = None;
        let mut data = None;
        while offset < bytes.len() {
            let header_end = offset
                .checked_add(8)
                .ok_or_else(|| AudioError::new("WAV chunk is malformed"))?;
            if header_end > bytes.len() {
                return Err(AudioError::new("WAV chunk is malformed"));
            }
            let size = usize::try_from(read_u32(bytes, offset + 4)?)
                .map_err(|_| AudioError::new("WAV chunk is malformed"))?;
            let start = header_end;
            let end = start
                .checked_add(size)
                .ok_or_else(|| AudioError::new("WAV chunk is malformed"))?;
            if end > bytes.len() {
                return Err(AudioError::new("WAV chunk is malformed"));
            }
            match &bytes[offset..offset + 4] {
                b"fmt " if format.is_none() => {
                    format = Some(parse_wave_format(&bytes[start..end])?);
                }
                b"data" if data.is_none() => data = Some(&bytes[start..end]),
                _ => {}
            }
            offset = end
                .checked_add(size % 2)
                .ok_or_else(|| AudioError::new("WAV chunk is malformed"))?;
        }
        let sample_rate = format.ok_or_else(|| AudioError::new("WAV format chunk is missing"))?;
        let pcm = data.ok_or_else(|| AudioError::new("WAV data chunk is missing"))?;
        Self::from_pcm16_le(pcm, sample_rate, limits)
    }

    /// Deterministically converts to another supported sample rate using linear interpolation.
    ///
    /// # Errors
    /// Rejects unsupported target rates or output lengths that overflow addressable memory.
    pub fn resample(&self, target_rate: u32) -> Result<Self, AudioError> {
        validate_rate(target_rate)?;
        if target_rate == self.sample_rate {
            return Ok(self.clone());
        }
        let intervals = self.samples.len().saturating_sub(1) as u64;
        let output_len = intervals
            .checked_mul(u64::from(target_rate))
            .and_then(|value| value.checked_div(u64::from(self.sample_rate)))
            .and_then(|value| value.checked_add(1))
            .and_then(|value| usize::try_from(value).ok())
            .ok_or_else(|| AudioError::new("resampled audio is too large"))?;
        let mut output = Vec::with_capacity(output_len);
        for index in 0..output_len {
            let position = (index as u64)
                .checked_mul(u64::from(self.sample_rate))
                .ok_or_else(|| AudioError::new("resampled audio is too large"))?;
            let left = usize::try_from(position / u64::from(target_rate))
                .map_err(|_| AudioError::new("resampled audio is too large"))?;
            let remainder = position % u64::from(target_rate);
            let right = left.saturating_add(1).min(self.samples.len() - 1);
            let remainder = u16::try_from(remainder)
                .map_err(|_| AudioError::new("resampled audio is too large"))?;
            let target = u16::try_from(target_rate)
                .map_err(|_| AudioError::new("resampled audio is too large"))?;
            let fraction = f32::from(remainder) / f32::from(target);
            output.push(self.samples[left] + (self.samples[right] - self.samples[left]) * fraction);
        }
        Self::new(target_rate, output)
    }

    /// Encodes samples as raw little-endian signed PCM16.
    #[must_use]
    pub fn to_pcm16_le(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(self.samples.len() * 2);
        for sample in &self.samples {
            bytes.extend_from_slice(&float_to_i16(*sample).to_le_bytes());
        }
        bytes
    }

    /// Encodes samples as a mono PCM16 RIFF/WAVE file.
    ///
    /// # Errors
    /// Returns an error if the encoded data cannot be represented by RIFF/WAVE length fields.
    pub fn to_wav_pcm16(&self) -> Result<Vec<u8>, AudioError> {
        let pcm = self.to_pcm16_le();
        let data_len =
            u32::try_from(pcm.len()).map_err(|_| AudioError::new("WAV output is too large"))?;
        let riff_len = data_len
            .checked_add(36)
            .ok_or_else(|| AudioError::new("WAV output is too large"))?;
        let byte_rate = self
            .sample_rate
            .checked_mul(2)
            .ok_or_else(|| AudioError::new("WAV output is too large"))?;
        let mut bytes = Vec::with_capacity(pcm.len() + 44);
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&riff_len.to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16_u32.to_le_bytes());
        bytes.extend_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&self.sample_rate.to_le_bytes());
        bytes.extend_from_slice(&byte_rate.to_le_bytes());
        bytes.extend_from_slice(&2_u16.to_le_bytes());
        bytes.extend_from_slice(&16_u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data_len.to_le_bytes());
        bytes.extend_from_slice(&pcm);
        Ok(bytes)
    }

    /// Sample rate in Hz.
    #[must_use]
    pub const fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Normalized mono samples.
    #[must_use]
    pub fn samples(&self) -> &[f32] {
        &self.samples
    }
}

/// Bounded decoder for PCM16 bytes arriving across arbitrary frame boundaries.
#[derive(Debug)]
pub struct IncrementalPcm16 {
    sample_rate: u32,
    limits: AudioLimits,
    samples: Vec<f32>,
    pending: Option<u8>,
}

impl IncrementalPcm16 {
    /// Creates an empty incremental decoder.
    ///
    /// # Errors
    /// Rejects unsupported sample rates.
    pub fn new(sample_rate: u32, limits: AudioLimits) -> Result<Self, AudioError> {
        validate_rate(sample_rate)?;
        Ok(Self {
            sample_rate,
            limits,
            samples: Vec::new(),
            pending: None,
        })
    }

    /// Appends one arbitrary byte frame and returns newly decoded samples.
    ///
    /// # Errors
    /// Rejects input that would exceed the configured duration or byte bounds.
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<f32>, AudioError> {
        let existing_bytes =
            self.samples.len().saturating_mul(2) + usize::from(self.pending.is_some());
        let total_bytes = existing_bytes
            .checked_add(bytes.len())
            .ok_or_else(|| AudioError::new("encoded audio exceeds its byte limit"))?;
        let prospective_samples = total_bytes / 2;
        self.limits
            .admit(total_bytes, self.sample_rate, prospective_samples)?;

        let mut decoded = Vec::with_capacity((bytes.len() + 1) / 2);
        let mut index = 0;
        if let Some(low) = self.pending.take() {
            if let Some(high) = bytes.first() {
                decoded.push(f32::from(i16::from_le_bytes([low, *high])) / 32_768.0);
                index = 1;
            } else {
                self.pending = Some(low);
            }
        }
        while index + 1 < bytes.len() {
            decoded
                .push(f32::from(i16::from_le_bytes([bytes[index], bytes[index + 1]])) / 32_768.0);
            index += 2;
        }
        if index < bytes.len() {
            self.pending = Some(bytes[index]);
        }
        self.samples.extend_from_slice(&decoded);
        Ok(decoded)
    }

    /// Finishes the stream and returns all decoded audio.
    ///
    /// # Errors
    /// Rejects a dangling byte or an empty stream.
    pub fn finish(self) -> Result<MonoPcm, AudioError> {
        if self.pending.is_some() {
            return Err(AudioError::new("PCM16 stream ended with a partial sample"));
        }
        MonoPcm::new(self.sample_rate, self.samples)
    }
}

/// Bounded energy voice-activity configuration.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VadConfig {
    /// Minimum root-mean-square amplitude considered speech.
    pub rms_threshold: f32,
    /// Consecutive speech duration needed to open an utterance.
    pub min_speech_ms: u32,
    /// Consecutive silence duration needed to close an utterance.
    pub trailing_silence_ms: u32,
    /// Hard upper bound for one utterance.
    pub max_utterance_ms: u32,
}

impl Default for VadConfig {
    fn default() -> Self {
        Self {
            rms_threshold: 0.015,
            min_speech_ms: 120,
            trailing_silence_ms: 600,
            max_utterance_ms: 30_000,
        }
    }
}

/// Voice activity transition emitted after one PCM frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VadEvent {
    /// No utterance transition.
    None,
    /// Speech has remained above the threshold long enough.
    Started,
    /// Trailing silence or the utterance hard limit closed the utterance.
    Ended,
}

/// Deterministic bounded energy voice-activity detector.
#[derive(Debug)]
pub struct EnergyVad {
    config: VadConfig,
    sample_rate: u32,
    speech_run: usize,
    silence_run: usize,
    utterance_samples: usize,
    active: bool,
}

impl EnergyVad {
    /// Validates VAD configuration for a supported sample rate.
    ///
    /// # Errors
    /// Rejects invalid thresholds, zero durations, or unsupported rates.
    pub fn new(config: VadConfig, sample_rate: u32) -> Result<Self, AudioError> {
        validate_rate(sample_rate)?;
        if !config.rms_threshold.is_finite()
            || !(0.0..=1.0).contains(&config.rms_threshold)
            || config.min_speech_ms == 0
            || config.trailing_silence_ms == 0
            || config.max_utterance_ms < config.min_speech_ms
        {
            return Err(AudioError::new("voice activity configuration is invalid"));
        }
        Ok(Self {
            config,
            sample_rate,
            speech_run: 0,
            silence_run: 0,
            utterance_samples: 0,
            active: false,
        })
    }

    /// Processes one non-empty normalized mono PCM frame.
    ///
    /// # Errors
    /// Rejects empty or non-finite samples.
    pub fn process(&mut self, samples: &[f32]) -> Result<VadEvent, AudioError> {
        if samples.is_empty() || samples.iter().any(|sample| !sample.is_finite()) {
            return Err(AudioError::new("voice activity frame is invalid"));
        }
        let frame_len = u32::try_from(samples.len())
            .map_err(|_| AudioError::new("voice activity frame is invalid"))?;
        let energy = samples
            .iter()
            .map(|sample| f64::from(*sample) * f64::from(*sample))
            .sum::<f64>()
            / f64::from(frame_len);
        let voiced = energy.sqrt() >= f64::from(self.config.rms_threshold);
        if voiced {
            self.speech_run = self.speech_run.saturating_add(samples.len());
            self.silence_run = 0;
        } else {
            self.speech_run = 0;
            if self.active {
                self.silence_run = self.silence_run.saturating_add(samples.len());
            }
        }
        if self.active {
            self.utterance_samples = self.utterance_samples.saturating_add(samples.len());
            if self.silence_run >= self.samples_for(self.config.trailing_silence_ms)
                || self.utterance_samples >= self.samples_for(self.config.max_utterance_ms)
            {
                self.reset();
                return Ok(VadEvent::Ended);
            }
        } else if self.speech_run >= self.samples_for(self.config.min_speech_ms) {
            self.active = true;
            self.utterance_samples = self.speech_run;
            return Ok(VadEvent::Started);
        }
        Ok(VadEvent::None)
    }

    /// Whether an utterance is currently active.
    #[must_use]
    pub const fn active(&self) -> bool {
        self.active
    }

    fn samples_for(&self, milliseconds: u32) -> usize {
        usize::try_from(u64::from(self.sample_rate).saturating_mul(u64::from(milliseconds)) / 1_000)
            .unwrap_or(usize::MAX)
    }

    fn reset(&mut self) {
        self.speech_run = 0;
        self.silence_run = 0;
        self.utterance_samples = 0;
        self.active = false;
    }
}

fn validate_rate(sample_rate: u32) -> Result<(), AudioError> {
    if SUPPORTED_SAMPLE_RATES.contains(&sample_rate) {
        Ok(())
    } else {
        Err(AudioError::new("audio sample rate is unsupported"))
    }
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, AudioError> {
    let value = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| AudioError::new("WAV container is malformed"))?;
    Ok(u32::from_le_bytes([value[0], value[1], value[2], value[3]]))
}

fn parse_wave_format(bytes: &[u8]) -> Result<u32, AudioError> {
    if bytes.len() < 16 {
        return Err(AudioError::new("WAV format chunk is malformed"));
    }
    let format = u16::from_le_bytes([bytes[0], bytes[1]]);
    let channels = u16::from_le_bytes([bytes[2], bytes[3]]);
    let sample_rate = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    let byte_rate = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
    let block_align = u16::from_le_bytes([bytes[12], bytes[13]]);
    let bits = u16::from_le_bytes([bytes[14], bytes[15]]);
    validate_rate(sample_rate)?;
    if format != 1
        || channels != 1
        || bits != 16
        || block_align != 2
        || byte_rate != sample_rate.saturating_mul(2)
    {
        return Err(AudioError::new("WAV encoding must be mono PCM16"));
    }
    Ok(sample_rate)
}

#[allow(clippy::cast_possible_truncation)]
fn float_to_i16(sample: f32) -> i16 {
    if sample <= -1.0 {
        i16::MIN
    } else {
        (sample.clamp(-1.0, 1.0) * 32_768.0)
            .round()
            .clamp(f32::from(i16::MIN), f32::from(i16::MAX)) as i16
    }
}

#[cfg(test)]
mod tests {
    use super::{AudioLimits, EnergyVad, IncrementalPcm16, MonoPcm, VadConfig, VadEvent};

    #[test]
    fn pcm16_round_trips_through_wav() -> Result<(), Box<dyn std::error::Error>> {
        let pcm = [i16::MIN, -1, 0, 1, i16::MAX]
            .into_iter()
            .flat_map(i16::to_le_bytes)
            .collect::<Vec<_>>();
        let audio = MonoPcm::from_pcm16_le(&pcm, 16_000, AudioLimits::default())?;
        let wav = audio.to_wav_pcm16()?;
        let decoded = MonoPcm::from_wav(&wav, AudioLimits::default())?;
        assert_eq!(decoded.to_pcm16_le(), pcm);
        Ok(())
    }

    #[test]
    fn wav_rejects_stereo_and_declared_length_mismatch() -> Result<(), Box<dyn std::error::Error>> {
        let audio = MonoPcm::new(16_000, vec![0.0, 0.5])?;
        let mut wav = audio.to_wav_pcm16()?;
        wav[22..24].copy_from_slice(&2_u16.to_le_bytes());
        assert!(MonoPcm::from_wav(&wav, AudioLimits::default()).is_err());
        wav[22..24].copy_from_slice(&1_u16.to_le_bytes());
        wav.push(0);
        assert!(MonoPcm::from_wav(&wav, AudioLimits::default()).is_err());
        Ok(())
    }

    #[test]
    fn duration_and_encoded_size_limits_fail_before_decode()
    -> Result<(), Box<dyn std::error::Error>> {
        let bytes = vec![0_u8; 3_202];
        let short = AudioLimits::new(bytes.len(), 100)?;
        assert!(MonoPcm::from_pcm16_le(&bytes, 16_000, short).is_err());
        let small = AudioLimits::new(2, 1_000)?;
        assert!(MonoPcm::from_pcm16_le(&bytes, 16_000, small).is_err());
        Ok(())
    }

    #[test]
    fn incremental_pcm_accepts_split_samples_and_rejects_partial_finish()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut decoder = IncrementalPcm16::new(16_000, AudioLimits::default())?;
        assert!(decoder.push(&[0x34])?.is_empty());
        let emitted = decoder.push(&[0x12, 0x78, 0x56])?;
        assert_eq!(emitted.len(), 2);
        assert_eq!(decoder.finish()?.to_pcm16_le(), [0x34, 0x12, 0x78, 0x56]);

        let mut partial = IncrementalPcm16::new(16_000, AudioLimits::default())?;
        let _ = partial.push(&[1])?;
        assert!(partial.finish().is_err());
        Ok(())
    }

    #[test]
    fn resampling_is_deterministic_and_preserves_endpoints()
    -> Result<(), Box<dyn std::error::Error>> {
        let input = MonoPcm::new(8_000, vec![-1.0, 0.0, 1.0])?;
        let first = input.resample(16_000)?;
        let second = input.resample(16_000)?;
        assert_eq!(first, second);
        assert_eq!(first.samples().first(), Some(&-1.0));
        assert_eq!(first.samples().last(), Some(&1.0));
        assert_eq!(first.samples().len(), 5);
        Ok(())
    }

    #[test]
    fn vad_has_bounded_start_and_end_transitions() -> Result<(), Box<dyn std::error::Error>> {
        let config = VadConfig {
            rms_threshold: 0.1,
            min_speech_ms: 20,
            trailing_silence_ms: 40,
            max_utterance_ms: 1_000,
        };
        let mut vad = EnergyVad::new(config, 16_000)?;
        assert_eq!(vad.process(&vec![0.5; 160])?, VadEvent::None);
        assert_eq!(vad.process(&vec![0.5; 160])?, VadEvent::Started);
        assert!(vad.active());
        assert_eq!(vad.process(&vec![0.0; 320])?, VadEvent::None);
        assert_eq!(vad.process(&vec![0.0; 320])?, VadEvent::Ended);
        assert!(!vad.active());
        Ok(())
    }
}
