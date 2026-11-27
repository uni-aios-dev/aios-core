//! Cross-platform microphone capture with PTT/toggle modes, VAD and level metering.
//!
//! Unit tests use [`MockAudioSource`], so no physical microphone is required.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::io::Cursor;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Target sample rate of the voice pipeline (Whisper-compatible mono PCM).
pub const TARGET_SAMPLE_RATE: u32 = 16_000;

/// How capture is armed and stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum CaptureMode {
    /// Press and hold to record, release to stop.
    PushToTalk,
    /// Press once to start, press again (or VAD) to stop.
    #[default]
    Toggle,
}

/// Instantaneous microphone level used by the UI meter.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Level {
    /// Root-mean-square amplitude in `[0.0, 1.0]`.
    pub rms: f32,
    /// Peak absolute amplitude in `[0.0, 1.0]`.
    pub peak: f32,
}

/// Monotonic 16 kHz mono PCM block handed to the STT engine.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioFrame {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
}

impl AudioFrame {
    /// Compute RMS and peak over the whole frame.
    pub fn level(&self) -> Level {
        level_of(&self.samples)
    }

    /// Frame duration in milliseconds.
    pub fn duration_ms(&self) -> u64 {
        if self.sample_rate == 0 {
            return 0;
        }
        self.samples.len() as u64 * 1000 / self.sample_rate as u64
    }

    /// Encode the frame as a 16-bit mono WAV buffer.
    pub fn to_wav(&self) -> Result<Vec<u8>, String> {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: self.sample_rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut writer =
                hound::WavWriter::new(&mut cursor, spec).map_err(|e| format!("WAV writer: {e}"))?;
            for sample in &self.samples {
                let clamped = sample.clamp(-1.0, 1.0);
                let value = (clamped * i16::MAX as f32) as i16;
                writer
                    .write_sample(value)
                    .map_err(|e| format!("WAV sample: {e}"))?;
            }
            writer
                .finalize()
                .map_err(|e| format!("WAV finalize: {e}"))?;
        }
        Ok(cursor.into_inner())
    }
}

/// Decode a WAV upload (or raw `s16le` fallback) into a mono 16 kHz frame.
pub fn decode_audio(bytes: &[u8], sample_rate: u32) -> Result<AudioFrame, String> {
    if bytes.len() < 44 && bytes[..4] != *b"RIFF" {
        if !bytes.len().is_multiple_of(2) {
            return Err("raw PCM must contain 16-bit samples".into());
        }
        let samples: Vec<f32> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| i16::from_le_bytes(*c) as f32 / i16::MAX as f32)
            .collect();
        return Ok(AudioFrame {
            samples: resample(&samples, sample_rate, TARGET_SAMPLE_RATE),
            sample_rate: TARGET_SAMPLE_RATE,
        });
    }

    let mut reader =
        hound::WavReader::new(Cursor::new(bytes)).map_err(|e| format!("WAV read: {e}"))?;
    let spec = reader.spec();
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => {
            let divisor = 2f32.powi(i32::from(spec.bits_per_sample) - 1);
            reader
                .samples::<i32>()
                .filter_map(Result::ok)
                .map(|s| s as f32 / divisor)
                .collect()
        }
        hound::SampleFormat::Float => reader.samples::<f32>().filter_map(Result::ok).collect(),
    };
    let mono = if spec.channels <= 1 {
        samples
    } else {
        let channels = spec.channels as usize;
        samples
            .chunks(channels)
            .map(|chunk| chunk.iter().sum::<f32>() / channels as f32)
            .collect()
    };
    Ok(AudioFrame {
        samples: resample(&mono, spec.sample_rate, TARGET_SAMPLE_RATE),
        sample_rate: TARGET_SAMPLE_RATE,
    })
}

fn resample(samples: &[f32], from: u32, to: u32) -> Vec<f32> {
    if samples.is_empty() || from == to || from == 0 {
        return samples.to_vec();
    }
    let ratio = f64::from(from) / f64::from(to);
    let out_len = ((samples.len() as f64 / ratio).ceil() as usize).max(1);
    (0..out_len)
        .map(|i| {
            let pos = i as f64 * ratio;
            let low = pos.floor() as usize;
            let high = (low + 1).min(samples.len() - 1);
            let frac = pos - low as f64;
            let a = samples.get(low).copied().unwrap_or(0.0) as f64;
            let b = samples.get(high).copied().unwrap_or(0.0) as f64;
            (a + (b - a) * frac) as f32
        })
        .collect()
}

fn level_of(samples: &[f32]) -> Level {
    if samples.is_empty() {
        return Level::default();
    }
    let mut sum = 0f64;
    let mut peak = 0f32;
    for sample in samples {
        sum += f64::from(*sample) * f64::from(*sample);
        peak = peak.max(sample.abs());
    }
    Level {
        rms: (sum / samples.len() as f64).sqrt().min(1.0) as f32,
        peak: peak.min(1.0),
    }
}

#[derive(Debug, Default)]
struct CaptureBuffer {
    samples: Vec<f32>,
    level: Level,
}

fn lock_buffer(buffer: &Arc<Mutex<CaptureBuffer>>) -> MutexGuard<'_, CaptureBuffer> {
    buffer.lock().unwrap_or_else(|poisoned| {
        log::warn!("Recovering poisoned audio capture mutex");
        poisoned.into_inner()
    })
}

fn lock_samples(samples: &Arc<Mutex<Vec<f32>>>) -> MutexGuard<'_, Vec<f32>> {
    samples.lock().unwrap_or_else(|poisoned| {
        log::warn!("Recovering poisoned audio sample mutex");
        poisoned.into_inner()
    })
}

fn lock_level(level: &Arc<Mutex<Level>>) -> MutexGuard<'_, Level> {
    level.lock().unwrap_or_else(|poisoned| {
        log::warn!("Recovering poisoned audio level mutex");
        poisoned.into_inner()
    })
}

/// Audio producer abstraction; implementations must be hardware-independent in tests.
pub trait AudioSource: Send {
    /// Arm the source. Returns an error if no input device is available.
    fn start(&mut self, config: &CaptureConfig) -> Result<(), String>;
    /// Stop the source and release hardware resources.
    fn stop(&mut self);
    /// Advance a simulated source by one chunk (no-op for real hardware).
    fn tick(&mut self) -> Option<Vec<f32>>;
    /// Drain samples collected since the previous call.
    fn collect(&mut self) -> Vec<f32>;
    /// Latest RMS/peak level.
    fn level(&self) -> Level;
    /// Hardware availability probe.
    fn is_available(&self) -> bool;
}

/// Real microphone source backed by `cpal` (16 kHz mono, f32 samples).
///
/// `cpal::Stream` is not `Send` on all platforms, so the stream is owned by a
/// dedicated thread while this struct only keeps `Send` handles to it.
pub struct MicrophoneSource {
    buffer: Arc<Mutex<CaptureBuffer>>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    sample_rate: u32,
}

impl Default for MicrophoneSource {
    fn default() -> Self {
        Self::new()
    }
}

impl MicrophoneSource {
    /// Create an unarmed microphone source.
    pub fn new() -> Self {
        Self {
            buffer: Arc::new(Mutex::new(CaptureBuffer::default())),
            stop: Arc::new(AtomicBool::new(false)),
            handle: None,
            sample_rate: TARGET_SAMPLE_RATE,
        }
    }
}

impl AudioSource for MicrophoneSource {
    fn start(&mut self, config: &CaptureConfig) -> Result<(), String> {
        self.stop();
        let buffer = Arc::clone(&self.buffer);
        let stop = Arc::clone(&self.stop);
        let sample_rate = config.sample_rate;
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let handle = std::thread::Builder::new()
            .name("aios-voice-capture".into())
            .spawn(move || {
                let host = cpal::default_host();
                let result = (|| -> Result<cpal::Stream, String> {
                    let device = host
                        .default_input_device()
                        .ok_or_else(|| "no default input device".to_string())?;
                    let default_config = device
                        .default_input_config()
                        .map_err(|e| format!("input config: {e}"))?;
                    let stream_config = cpal::StreamConfig {
                        channels: 1,
                        sample_rate: cpal::SampleRate(sample_rate),
                        buffer_size: cpal::BufferSize::Default,
                    };
                    let on_error = |e: cpal::StreamError| log::warn!("Audio capture error: {e}");
                    let stream = match default_config.sample_format() {
                        cpal::SampleFormat::F32 => build_input_stream::<f32>(
                            &device,
                            &stream_config,
                            Arc::clone(&buffer),
                            on_error,
                        ),
                        cpal::SampleFormat::I16 => build_input_stream::<i16>(
                            &device,
                            &stream_config,
                            Arc::clone(&buffer),
                            on_error,
                        ),
                        cpal::SampleFormat::U16 => build_input_stream::<u16>(
                            &device,
                            &stream_config,
                            Arc::clone(&buffer),
                            on_error,
                        ),
                        cpal::SampleFormat::I32 => build_input_stream::<i32>(
                            &device,
                            &stream_config,
                            Arc::clone(&buffer),
                            on_error,
                        ),
                        other => Err(format!("unsupported input sample format: {other:?}")),
                    }?;
                    stream.play().map_err(|e| format!("stream play: {e}"))?;
                    Ok(stream)
                })();
                match result {
                    Ok(stream) => {
                        let _ = ready_tx.send(Ok(()));
                        while !stop.load(Ordering::Relaxed) {
                            std::thread::sleep(Duration::from_millis(50));
                        }
                        drop(stream);
                    }
                    Err(reason) => {
                        let _ = ready_tx.send(Err(reason));
                    }
                }
            })
            .map_err(|e| format!("capture thread: {e}"))?;
        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(())) => {}
            Ok(Err(reason)) => {
                let _ = handle.join();
                return Err(reason);
            }
            Err(e) => {
                self.stop.store(true, Ordering::Relaxed);
                let _ = handle.join();
                return Err(format!("capture thread handshake: {e}"));
            }
        }
        lock_buffer(&self.buffer).samples.clear();
        self.sample_rate = sample_rate;
        self.handle = Some(handle);
        Ok(())
    }

    fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        self.stop.store(false, Ordering::Relaxed);
    }

    fn tick(&mut self) -> Option<Vec<f32>> {
        None
    }

    fn collect(&mut self) -> Vec<f32> {
        std::mem::take(&mut lock_buffer(&self.buffer).samples)
    }

    fn level(&self) -> Level {
        lock_buffer(&self.buffer).level
    }

    fn is_available(&self) -> bool {
        cpal::default_host().default_input_device().is_some()
    }
}

fn build_input_stream<S>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    buffer: Arc<Mutex<CaptureBuffer>>,
    on_error: impl Fn(cpal::StreamError) + Send + 'static,
) -> Result<cpal::Stream, String>
where
    S: cpal::SizedSample,
    f32: cpal::FromSample<S>,
{
    let stream = device
        .build_input_stream(
            config,
            move |data: &[S], _: &cpal::InputCallbackInfo| {
                let mut guard = lock_buffer(&buffer);
                for sample in data {
                    let value: f32 = sample.to_sample();
                    guard.samples.push(value);
                }
                guard.level = level_of(&guard.samples[guard.samples.len().saturating_sub(512)..]);
            },
            on_error,
            None,
        )
        .map_err(|e| format!("build input stream: {e}"))?;
    Ok(stream)
}

/// Deterministic audio source for unit and integration tests.
pub struct MockAudioSource {
    chunks: VecDeque<Vec<f32>>,
    rate: u32,
    active: bool,
    produced: Arc<Mutex<Vec<f32>>>,
    last_level: Arc<Mutex<Level>>,
}

impl MockAudioSource {
    /// Build a source from pre-recorded PCM chunks.
    pub fn new(chunks: Vec<Vec<f32>>) -> Self {
        Self {
            chunks: chunks.into(),
            rate: TARGET_SAMPLE_RATE,
            active: false,
            produced: Arc::new(Mutex::new(Vec::new())),
            last_level: Arc::new(Mutex::new(Level::default())),
        }
    }

    /// Generate `speech_ms` of a 440 Hz tone followed by `silence_ms` of silence.
    pub fn with_tone(
        speech_ms: u64,
        silence_ms: u64,
        amplitude: f32,
        chunk_ms: u64,
        sample_rate: u32,
    ) -> Self {
        let mut chunks = Vec::new();
        let chunk_len = (chunk_ms.max(1) as usize * sample_rate as usize / 1000).max(1);
        let total = ((speech_ms + silence_ms).max(1) as usize * sample_rate as usize / 1000).max(1);
        let loud_len = (speech_ms as usize * sample_rate as usize / 1000).min(total);
        for start in (0..total).step_by(chunk_len) {
            let end = (start + chunk_len).min(total);
            let chunk = (start..end)
                .map(|i| {
                    if i < loud_len {
                        let phase =
                            2.0 * std::f32::consts::PI * 440.0 * i as f32 / sample_rate as f32;
                        amplitude * phase.sin()
                    } else {
                        0.0
                    }
                })
                .collect::<Vec<_>>();
            chunks.push(chunk);
        }
        let mut source = Self::new(chunks);
        source.rate = sample_rate;
        source
    }
}

impl AudioSource for MockAudioSource {
    fn start(&mut self, _config: &CaptureConfig) -> Result<(), String> {
        self.active = true;
        Ok(())
    }

    fn tick(&mut self) -> Option<Vec<f32>> {
        if !self.active {
            return None;
        }
        let chunk = self.chunks.pop_front();
        if let Some(ref samples) = chunk {
            *lock_level(&self.last_level) = level_of(samples);
        }
        chunk
    }

    fn stop(&mut self) {
        self.active = false;
    }

    fn collect(&mut self) -> Vec<f32> {
        std::mem::take(&mut *lock_samples(&self.produced))
    }

    fn level(&self) -> Level {
        *lock_level(&self.last_level)
    }

    fn is_available(&self) -> bool {
        true
    }
}

/// Voice activity detection and capture limits.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureConfig {
    pub mode: CaptureMode,
    pub sample_rate: u32,
    /// Silence duration that triggers automatic stop in toggle mode.
    pub vad_silence_ms: u64,
    /// RMS threshold below which audio counts as silence.
    pub vad_threshold_rms: f32,
    /// Hard cap on a single recording.
    pub max_duration_ms: u64,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            mode: CaptureMode::Toggle,
            sample_rate: TARGET_SAMPLE_RATE,
            vad_silence_ms: 1_200,
            vad_threshold_rms: 0.012,
            max_duration_ms: 60_000,
        }
    }
}

/// Result of one controller poll.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CaptureStatus {
    pub active: bool,
    pub level: Level,
    /// Set when VAD or the duration cap ended the recording.
    pub auto_stopped: bool,
}

/// Drives an [`AudioSource`], applies VAD and produces the final [`AudioFrame`].
pub struct CaptureController {
    source: Box<dyn AudioSource>,
    config: CaptureConfig,
    samples: Vec<f32>,
    started_at: Option<Instant>,
    last_voice_at: Option<Instant>,
    voice_started: bool,
    active: bool,
    session_open: bool,
}

impl CaptureController {
    /// Wrap a source (use [`MicrophoneSource`] in production).
    pub fn new(source: Box<dyn AudioSource>, config: CaptureConfig) -> Self {
        Self {
            source,
            config,
            samples: Vec::new(),
            started_at: None,
            last_voice_at: None,
            voice_started: false,
            active: false,
            session_open: false,
        }
    }

    /// Begin capture, clearing any previous buffer.
    pub fn start(&mut self) -> Result<(), String> {
        self.source.start(&self.config)?;
        self.samples.clear();
        self.started_at = None;
        self.last_voice_at = None;
        self.voice_started = false;
        self.active = true;
        self.session_open = true;
        Ok(())
    }

    /// Stop capture and return the recorded frame (if any audio was collected).
    pub fn stop(&mut self) -> Option<AudioFrame> {
        if !self.session_open {
            return None;
        }
        self.active = false;
        self.session_open = false;
        self.samples.extend(self.source.collect());
        self.source.stop();
        if self.samples.is_empty() {
            return None;
        }
        Some(AudioFrame {
            samples: std::mem::take(&mut self.samples),
            sample_rate: self.config.sample_rate,
        })
    }

    /// Poll the source. Call every ~100 ms while listening.
    pub fn tick(&mut self) -> CaptureStatus {
        self.tick_at(Instant::now())
    }

    /// Deterministic variant of [`Self::tick`] used by tests.
    pub fn tick_at(&mut self, now: Instant) -> CaptureStatus {
        if !self.active {
            return CaptureStatus {
                active: false,
                level: Level::default(),
                auto_stopped: false,
            };
        }
        let started = self.started_at.get_or_insert(now);
        if let Some(chunk) = self.source.tick() {
            self.samples.extend_from_slice(&chunk);
        }
        self.samples.extend(self.source.collect());
        let recent = self.samples.len().saturating_sub(512);
        let level = level_of(&self.samples[recent..]);
        if level.rms >= self.config.vad_threshold_rms {
            self.voice_started = true;
            self.last_voice_at = Some(now);
        }
        let silence_for = self
            .last_voice_at
            .map(|last| now.saturating_duration_since(last))
            .unwrap_or_default();
        let elapsed = now.saturating_duration_since(*started);
        let too_long = elapsed >= Duration::from_millis(self.config.max_duration_ms);
        let silent_too_long =
            self.voice_started && silence_for >= Duration::from_millis(self.config.vad_silence_ms);
        let auto_stopped = too_long || silent_too_long;
        if auto_stopped {
            self.active = false;
        }
        CaptureStatus {
            active: self.active,
            level,
            auto_stopped,
        }
    }

    /// Whether a recording is currently active.
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Current capture configuration.
    pub fn config(&self) -> &CaptureConfig {
        &self.config
    }

    /// Mutable capture configuration (used to arm PTT/toggle mode).
    pub fn config_mut(&mut self) -> &mut CaptureConfig {
        &mut self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_and_wav_roundtrip() {
        let frame = AudioFrame {
            samples: vec![0.0, 0.5, -0.5, 1.0],
            sample_rate: 16_000,
        };
        let level = frame.level();
        assert!(level.peak > 0.99);
        assert!(level.rms > 0.4 && level.rms <= 1.0);
        let wav = frame.to_wav().unwrap();
        let decoded = decode_audio(&wav, 16_000).unwrap();
        assert_eq!(decoded.sample_rate, 16_000);
        assert_eq!(decoded.samples.len(), frame.samples.len());
    }

    #[test]
    fn mock_capture_collects_tone() {
        let source = MockAudioSource::with_tone(200, 100, 0.6, 50, 16_000);
        let mut controller = CaptureController::new(
            Box::new(source),
            CaptureConfig {
                vad_silence_ms: 10_000,
                ..CaptureConfig::default()
            },
        );
        controller.start().unwrap();
        let mut status = CaptureStatus::default();
        let mut heard = false;
        for _ in 0..6 {
            status = controller.tick();
            heard |= status.level.rms > 0.0;
        }
        assert!(status.active);
        assert!(heard);
        let frame = controller.stop().unwrap();
        assert_eq!(frame.duration_ms(), 300);
    }

    #[test]
    fn vad_auto_stops_after_silence() {
        let source = MockAudioSource::with_tone(100, 500, 0.7, 50, 16_000);
        let config = CaptureConfig {
            vad_silence_ms: 100,
            ..CaptureConfig::default()
        };
        let mut controller = CaptureController::new(Box::new(source), config);
        let mut event = CaptureStatus::default();
        let start = Instant::now();
        controller.start().unwrap();
        for i in 0..12 {
            event = controller.tick_at(start + Duration::from_millis(i * 50 + 50));
            if event.auto_stopped {
                break;
            }
        }
        assert!(event.auto_stopped, "VAD did not auto-stop");
        assert!(!event.active);
        assert!(controller.stop().is_some());
    }

    #[test]
    fn raw_pcm_fallback_is_resampled() {
        let bytes: Vec<u8> = [0i16, 1000, -1000, 500]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        let frame = decode_audio(&bytes, 8_000).unwrap();
        assert_eq!(frame.sample_rate, TARGET_SAMPLE_RATE);
        assert!(frame.samples.len() > 4);
    }
}
