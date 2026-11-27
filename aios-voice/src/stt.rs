//! Speech-to-text engines: local Whisper (candle), compatible HTTP API and a mock.

use crate::capture::{AudioFrame, TARGET_SAMPLE_RATE};
use candle::Tensor;
use candle_nn::VarBuilder;
use candle_transformers::models::whisper::model::Whisper;
use candle_transformers::models::whisper::{self as whisper_model, Config};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::Duration;

/// Requested recognition language.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Language {
    /// Let the deployment default decide (`ru` unless overridden).
    #[default]
    Auto,
    Russian,
    English,
}

impl Language {
    /// Whisper language code used by both local and cloud engines.
    pub fn code(self) -> &'static str {
        match self {
            Self::English => "en",
            Self::Russian | Self::Auto => "ru",
        }
    }

    /// Parse a language tag (`ru`, `en`, `auto`, case-insensitive).
    pub fn parse(value: &str) -> Result<Self, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            "ru" | "russian" | "русский" => Ok(Self::Russian),
            "en" | "english" | "английский" => Ok(Self::English),
            "auto" | "" => Ok(Self::Auto),
            other => Err(format!("unsupported language: {other}")),
        }
    }
}

/// STT failure with a human-readable reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SttError(pub String);

impl std::fmt::Display for SttError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SttError {}

/// Common interface for local, cloud and mock recognition engines.
pub trait SttEngine: Send + Sync {
    /// Human-readable engine name for telemetry.
    fn name(&self) -> String;

    /// Transcribe one 16 kHz mono frame.
    fn transcribe<'a>(
        &'a self,
        audio: &'a AudioFrame,
        language: Language,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<String, SttError>> + Send + 'a>>;
}

/// Deterministic engine used by tests; returns preconfigured transcripts.
pub struct MockSttEngine {
    responses: Vec<String>,
    calls: std::sync::Mutex<usize>,
}

impl MockSttEngine {
    /// Build an engine that always returns `text`.
    pub fn with_text(text: impl Into<String>) -> Self {
        Self {
            responses: vec![text.into()],
            calls: std::sync::Mutex::new(0),
        }
    }

    /// Build an engine returning one response per call (last one repeats).
    pub fn with_responses(responses: Vec<String>) -> Self {
        Self {
            responses,
            calls: std::sync::Mutex::new(0),
        }
    }

    /// Number of transcription calls so far.
    pub fn call_count(&self) -> usize {
        *self.calls.lock().unwrap_or_else(|poisoned| {
            log::warn!("Recovering poisoned mock STT mutex");
            poisoned.into_inner()
        })
    }
}

impl SttEngine for MockSttEngine {
    fn name(&self) -> String {
        "mock".into()
    }

    fn transcribe<'a>(
        &'a self,
        _audio: &'a AudioFrame,
        _language: Language,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<String, SttError>> + Send + 'a>> {
        Box::pin(async move {
            let mut calls = self.calls.lock().unwrap_or_else(|poisoned| {
                log::warn!("Recovering poisoned mock STT mutex");
                poisoned.into_inner()
            });
            let index = (*calls).min(self.responses.len().saturating_sub(1));
            *calls += 1;
            self.responses
                .get(index)
                .cloned()
                .ok_or_else(|| SttError("no mock response configured".into()))
        })
    }
}

/// Fail-closed engine used when neither a local model nor a cloud endpoint is
/// configured. It never fabricates a transcript.
pub struct UnavailableSttEngine;

impl SttEngine for UnavailableSttEngine {
    fn name(&self) -> String {
        "unavailable".into()
    }

    fn transcribe<'a>(
        &'a self,
        _audio: &'a AudioFrame,
        _language: Language,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<String, SttError>> + Send + 'a>> {
        Box::pin(async {
            Err(SttError(
                "STT is not configured: set AIOS_WHISPER_MODEL or AIOS_STT_URL".into(),
            ))
        })
    }
}

/// Configuration of a Whisper-compatible HTTP endpoint.
#[derive(Debug, Clone)]
pub struct CloudSttConfig {
    pub url: String,
    pub model: String,
    /// Optional bearer token. Never hardcoded: provided via env or `aios_core::secrets`.
    pub api_key: Option<String>,
    pub timeout: Duration,
    pub max_audio_bytes: usize,
}

impl Default for CloudSttConfig {
    fn default() -> Self {
        Self {
            url: String::new(),
            model: "whisper-1".into(),
            api_key: None,
            timeout: Duration::from_secs(30),
            max_audio_bytes: 10 * 1024 * 1024,
        }
    }
}

impl CloudSttConfig {
    /// Read configuration from `AIOS_STT_URL`, `AIOS_STT_MODEL`, `AIOS_STT_API_KEY`.
    /// When no explicit key is set, a named secret is resolved through `aios_core::secrets`.
    pub fn from_env() -> Self {
        let mut config = Self::default();
        if let Ok(url) = std::env::var("AIOS_STT_URL") {
            config.url = url;
        }
        if let Ok(model) = std::env::var("AIOS_STT_MODEL") {
            config.model = model;
        }
        config.api_key = std::env::var("AIOS_STT_API_KEY").ok().or_else(|| {
            aios_core::secrets::resolve_secret("stt")
                .ok()
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        });
        config
    }
}

/// HTTP client for Whisper-compatible `/audio/transcriptions` endpoints.
pub struct CloudSttEngine {
    client: reqwest::Client,
    config: CloudSttConfig,
}

impl CloudSttEngine {
    /// Create an engine with explicit configuration.
    pub fn new(config: CloudSttConfig) -> Result<Self, SttError> {
        let client = reqwest::Client::builder()
            .timeout(config.timeout)
            .build()
            .map_err(|e| SttError(format!("HTTP client: {e}")))?;
        Ok(Self { client, config })
    }

    /// Configuration in use (API key excluded from debug output by callers).
    pub fn config(&self) -> &CloudSttConfig {
        &self.config
    }
}

impl SttEngine for CloudSttEngine {
    fn name(&self) -> String {
        "cloud".into()
    }

    fn transcribe<'a>(
        &'a self,
        audio: &'a AudioFrame,
        language: Language,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<String, SttError>> + Send + 'a>> {
        Box::pin(async move {
            if self.config.url.is_empty() {
                return Err(SttError("AIOS_STT_URL is not configured".into()));
            }
            let wav = audio.to_wav().map_err(SttError)?;
            if wav.len() > self.config.max_audio_bytes {
                return Err(SttError(format!(
                    "audio exceeds {} byte limit",
                    self.config.max_audio_bytes
                )));
            }
            let mut form = reqwest::multipart::Form::new()
                .part(
                    "file",
                    reqwest::multipart::Part::bytes(wav)
                        .file_name("voice.wav")
                        .mime_str("audio/wav")
                        .map_err(|e| SttError(format!("multipart: {e}")))?,
                )
                .text("model", self.config.model.clone())
                .text("language", language.code())
                .text("response_format", "json");
            if let Some(ref key) = self.config.api_key {
                form = form.text("api_key", key.clone());
            }
            let mut request = self
                .client
                .post(&self.config.url)
                .multipart(form)
                .header("Accept", "application/json");
            if let Some(ref key) = self.config.api_key {
                request = request.header("Authorization", format!("Bearer {key}"));
            }
            let response = request
                .send()
                .await
                .map_err(|e| SttError(format!("STT request: {e}")))?;
            if !response.status().is_success() {
                return Err(SttError(format!("STT HTTP {}", response.status())));
            }
            let body = response
                .bytes()
                .await
                .map_err(|e| SttError(format!("STT body: {e}")))?;
            let text = serde_json::from_slice::<serde_json::Value>(&body)
                .ok()
                .and_then(|json| {
                    json.get("text")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string)
                })
                .unwrap_or_else(|| String::from_utf8_lossy(&body).trim().to_string());
            if text.is_empty() {
                return Err(SttError("empty transcript".into()));
            }
            Ok(text)
        })
    }
}

/// Local offline Whisper engine backed by candle (CPU inference).
///
/// Expects a model directory with `config.json`, `tokenizer.json` and
/// `model.safetensors`. No network access is performed.
pub struct LocalWhisperEngine {
    model_dir: PathBuf,
    device: candle::Device,
    max_tokens: usize,
}

impl LocalWhisperEngine {
    /// Point the engine at a local Whisper model directory.
    pub fn new(model_dir: impl AsRef<Path>) -> Self {
        Self {
            model_dir: model_dir.as_ref().to_path_buf(),
            device: candle::Device::Cpu,
            max_tokens: 224,
        }
    }

    /// Resolve the model directory from `AIOS_WHISPER_MODEL` or `models/whisper`.
    pub fn from_env() -> Self {
        let path = std::env::var("AIOS_WHISPER_MODEL")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("models/whisper"));
        Self::new(path)
    }

    /// Check that all required model files exist.
    pub fn is_available(&self) -> bool {
        ["config.json", "tokenizer.json", "model.safetensors"]
            .iter()
            .all(|name| self.model_dir.join(name).is_file())
    }

    fn load(&self) -> Result<(Whisper, tokenizers::Tokenizer, Config), SttError> {
        let config_path = self.model_dir.join("config.json");
        let config: Config = serde_json::from_slice(
            &std::fs::read(&config_path)
                .map_err(|e| SttError(format!("read {}: {e}", config_path.display())))?,
        )
        .map_err(|e| SttError(format!("parse config: {e}")))?;
        let tokenizer = tokenizers::Tokenizer::from_file(self.model_dir.join("tokenizer.json"))
            .map_err(|e| SttError(format!("load tokenizer: {e}")))?;
        let path = self.model_dir.join("model.safetensors");
        // SAFETY: the memory-mapped weights file is opened read-only and the
        // mapping lives only until `Whisper::load` has copied tensors into owns
        // storage; no external process may mutate the model file while loading.
        let vb = unsafe {
            VarBuilder::from_mmaped_safetensors(
                std::slice::from_ref(&path),
                candle::DType::F32,
                &self.device,
            )
        }
        .map_err(|e| SttError(format!("load weights {}: {e}", path.display())))?;
        let model = Whisper::load(&vb, config.clone())
            .map_err(|e| SttError(format!("load whisper model: {e}")))?;
        Ok((model, tokenizer, config))
    }

    /// Greedy decode of one 30-second padded mel spectrogram.
    fn greedy_decode(
        device: &candle::Device,
        max_tokens: usize,
        model: &mut Whisper,
        tokenizer: &tokenizers::Tokenizer,
        samples: &[f32],
        language: Language,
    ) -> Result<String, SttError> {
        let config = model.config.clone();
        let padded = pad_audio(samples, whisper_model::N_SAMPLES);
        let filters = mel_filters(config.num_mel_bins);
        let mel = whisper_model::audio::pcm_to_mel(&config, &padded, &filters);
        let frames = whisper_model::N_FRAMES;
        let n_mels = config.num_mel_bins;
        if mel.len() < n_mels * frames {
            return Err(SttError("mel spectrogram too short".into()));
        }
        let mel_tensor = Tensor::from_slice(&mel[..n_mels * frames], (n_mels, frames), device)
            .and_then(|t| t.transpose(0, 1))
            .and_then(|t| t.unsqueeze(0))
            .map_err(|e| SttError(format!("mel tensor: {e}")))?;
        let encoded = model
            .encoder
            .forward(&mel_tensor, true)
            .map_err(|e| SttError(format!("whisper encoder: {e}")))?;
        model.reset_kv_cache();

        let token = |name: &str| -> Result<u32, SttError> {
            tokenizer
                .token_to_id(name)
                .ok_or_else(|| SttError(format!("token not in vocabulary: {name}")))
        };
        let mut tokens = vec![
            token(whisper_model::SOT_TOKEN)?,
            token(&format!("<|{}|>", language.code()))?,
            token(whisper_model::TRANSCRIBE_TOKEN)?,
            token(whisper_model::NO_TIMESTAMPS_TOKEN)?,
        ];
        let eot = token(whisper_model::EOT_TOKEN)?;
        let mut decoded = String::new();
        for step in 0..max_tokens {
            let input = Tensor::new(tokens.as_slice(), device)
                .and_then(|t| t.unsqueeze(0))
                .map_err(|e| SttError(format!("decoder input: {e}")))?;
            let hidden = model
                .decoder
                .forward(&input, &encoded, step == 0)
                .map_err(|e| SttError(format!("whisper decoder: {e}")))?;
            let logits = model
                .decoder
                .final_linear(&hidden)
                .map_err(|e| SttError(format!("whisper logits: {e}")))?;
            let last = logits
                .get(0)
                .and_then(|t| t.get(tokens.len().saturating_sub(1)))
                .and_then(|t| t.argmax(candle::D::Minus1))
                .and_then(|t| t.to_scalar::<u32>())
                .map_err(|e| SttError(format!("whisper argmax: {e}")))?;
            if last == eot {
                break;
            }
            tokens.push(last);
            let piece = tokenizer.decode(&[last], false).unwrap_or_default();
            if !piece.starts_with("<|") {
                decoded.push_str(&piece);
            }
        }
        Ok(decoded.trim().to_string())
    }
}

impl SttEngine for LocalWhisperEngine {
    fn name(&self) -> String {
        "local/whisper".into()
    }

    fn transcribe<'a>(
        &'a self,
        audio: &'a AudioFrame,
        language: Language,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<String, SttError>> + Send + 'a>> {
        Box::pin(async move {
            if audio.sample_rate != TARGET_SAMPLE_RATE {
                return Err(SttError(format!(
                    "unsupported sample rate {} (expected {TARGET_SAMPLE_RATE})",
                    audio.sample_rate
                )));
            }
            if !self.is_available() {
                return Err(SttError(format!(
                    "Whisper model not found in {}",
                    self.model_dir.display()
                )));
            }
            if audio.samples.is_empty() {
                return Ok(String::new());
            }
            let (mut model, tokenizer, _) = self.load()?;
            let audio = audio.clone();
            let device = self.device.clone();
            let max_tokens = self.max_tokens;
            tokio::task::spawn_blocking(move || {
                Self::greedy_decode(
                    &device,
                    max_tokens,
                    &mut model,
                    &tokenizer,
                    &audio.samples,
                    language,
                )
            })
            .await
            .map_err(|e| SttError(format!("whisper task: {e}")))?
        })
    }
}

fn pad_audio(samples: &[f32], target: usize) -> Vec<f32> {
    let mut padded = Vec::with_capacity(target);
    padded.extend_from_slice(&samples[..samples.len().min(target)]);
    padded.resize(target, 0.0);
    padded
}

/// Slaney-style mel filterbank matching whisper.cpp's layout (`bin * 201 + freq`).
fn mel_filters(n_mels: usize) -> Vec<f32> {
    let n_fft = 1 + whisper_model::N_FFT / 2;
    let hz_to_mel = |hz: f64| 2595.0 * (1.0 + hz / 700.0).log10();
    let mel_to_hz = |mel: f64| 700.0 * (10f64.powf(mel / 2595.0) - 1.0);
    let max_mel = hz_to_mel(TARGET_SAMPLE_RATE as f64 / 2.0);
    let points: Vec<f64> = (0..=n_mels + 1)
        .map(|i| mel_to_hz(max_mel * i as f64 / (n_mels + 1) as f64))
        .collect();
    let mut filters = vec![0f32; n_mels * n_fft];
    for mel_index in 0..n_mels {
        let left = points[mel_index];
        let center = points[mel_index + 1];
        let right = points[mel_index + 2];
        let norm = 2.0 / (right - left);
        for bin in 0..n_fft {
            let freq = bin as f64 * f64::from(TARGET_SAMPLE_RATE) / whisper_model::N_FFT as f64;
            let weight = if freq >= left && freq <= center {
                (freq - left) / (center - left)
            } else if freq > center && freq <= right {
                (right - freq) / (right - center)
            } else {
                0.0
            };
            filters[mel_index * n_fft + bin] = (weight * norm) as f32;
        }
    }
    filters
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mock_engine_is_deterministic() {
        let engine = MockSttEngine::with_responses(vec!["привет".into(), "hello".into()]);
        let frame = AudioFrame {
            samples: vec![0.0; 16],
            sample_rate: 16_000,
        };
        assert_eq!(
            engine.transcribe(&frame, Language::Russian).await.unwrap(),
            "привет"
        );
        assert_eq!(
            engine.transcribe(&frame, Language::English).await.unwrap(),
            "hello"
        );
        assert_eq!(engine.call_count(), 2);
    }

    #[test]
    fn language_codes() {
        assert_eq!(Language::parse("RU").unwrap().code(), "ru");
        assert_eq!(Language::parse("english").unwrap().code(), "en");
        assert!(Language::parse("de").is_err());
    }

    #[test]
    fn mel_filterbank_shape() {
        let filters = mel_filters(80);
        assert_eq!(filters.len(), 80 * 201);
        assert!(filters.iter().any(|v| *v > 0.0));
    }

    #[test]
    fn local_engine_reports_missing_model() {
        let engine = LocalWhisperEngine::new("definitely-missing-model-dir");
        assert!(!engine.is_available());
        assert_eq!(engine.name(), "local/whisper");
    }
}
