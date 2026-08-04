use crate::types::*;
use candle::quantized::gguf_file;
use candle::{Device, Tensor};
use candle_transformers::generation::{LogitsProcessor, Sampling};
use hf_hub::HFClientSync;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub enum LocalModelKind {
    Micro,
    Full,
}

enum LoadedModel {
    Qwen2(Mutex<candle_transformers::models::quantized_qwen2::ModelWeights>),
}

#[allow(dead_code)]
pub struct LocalEngine {
    config: LlmConfig,
    kind: LocalModelKind,
    model: Option<LoadedModel>,
    tokenizer: Option<Mutex<tokenizers::Tokenizer>>,
    device: Device,
    model_path: Option<PathBuf>,
}

impl LocalEngine {
    pub fn new(config: LlmConfig, kind: LocalModelKind) -> Self {
        Self {
            config,
            kind,
            model: None,
            tokenizer: None,
            device: Device::Cpu,
            model_path: None,
        }
    }

    /// Returns the configuration this engine was created with.
    pub fn config(&self) -> &LlmConfig {
        &self.config
    }

    pub async fn query(&self, request: &LlmRequest) -> LlmResult<LlmResponse> {
        let model = self.model.as_ref().ok_or_else(|| {
            LlmError::NotAvailable(
                "Model not loaded. Call load_model() or set AIOS_MODEL_PATH.".into(),
            )
        })?;
        let tokenizer_lock = self
            .tokenizer
            .as_ref()
            .ok_or_else(|| LlmError::NotAvailable("Tokenizer not loaded".into()))?;

        let start = std::time::Instant::now();
        let formatted = format!(
            "<|im_start|>system\n{}<|im_end|>\n<|im_start|>user\n{}<|im_end|>\n<|im_start|>assistant\n",
            request.system_prompt, request.user_prompt
        );

        let tokenizer = tokenizer_lock
            .lock()
            .map_err(|e| LlmError::ApiError(e.to_string()))?;
        let encoding = tokenizer
            .encode(formatted, true)
            .map_err(|e| LlmError::ApiError(format!("Tokenization failed: {e}")))?;
        let tokens = encoding.get_ids().to_vec();
        let eos_token = tokenizer.token_to_id("<|im_end|>").unwrap_or(151645);
        let prompt_len = tokens.len();
        if prompt_len == 0 {
            return Err(LlmError::ApiError("Empty prompt after tokenization".into()));
        }
        drop(tokenizer);

        let max_tokens = request.max_tokens as usize;
        let temperature = request.temperature;
        let sampling = if temperature <= 0.0 {
            Sampling::ArgMax
        } else {
            Sampling::All {
                temperature: temperature as f64,
            }
        };
        let mut logits_processor = LogitsProcessor::from_sampling(rand::random::<u64>(), sampling);

        let input = Tensor::new(&tokens[..], &self.device)
            .map_err(|e| LlmError::ApiError(format!("Tensor: {e}")))?
            .unsqueeze(0)
            .map_err(|e| LlmError::ApiError(format!("Unsqueeze: {e}")))?;

        let mut output_text = String::new();
        let mut all_tokens = Vec::new();

        let mut next_token = self.sample_next(model, &input, 0, &mut logits_processor)?;
        all_tokens.push(next_token);
        if let Ok(decoded) = self.decode_token(tokenizer_lock, next_token) {
            output_text.push_str(&decoded);
        }

        for index in 0..max_tokens.saturating_sub(1) {
            let next = Tensor::new(&[next_token], &self.device)
                .map_err(|e| LlmError::ApiError(format!("Tensor: {e}")))?
                .unsqueeze(0)
                .map_err(|e| LlmError::ApiError(format!("Unsqueeze: {e}")))?;

            next_token =
                self.sample_next(model, &next, prompt_len + index, &mut logits_processor)?;
            all_tokens.push(next_token);

            if let Ok(decoded) = self.decode_token(tokenizer_lock, next_token) {
                output_text.push_str(&decoded);
            }
            if next_token == eos_token {
                break;
            }
        }

        Ok(LlmResponse {
            text: output_text,
            tokens_used: all_tokens.len() as u32,
            duration_ms: start.elapsed().as_millis() as u64,
        })
    }

    fn sample_next(
        &self,
        model: &LoadedModel,
        input: &Tensor,
        pos: usize,
        lp: &mut LogitsProcessor,
    ) -> LlmResult<u32> {
        match model {
            LoadedModel::Qwen2(m) => {
                let m = &mut *m.lock().map_err(|e| LlmError::ApiError(e.to_string()))?;
                let logits = m
                    .forward(input, pos)
                    .map_err(|e| LlmError::ApiError(format!("Inference: {e}")))?;
                let logits = logits
                    .squeeze(0)
                    .map_err(|e| LlmError::ApiError(format!("Squeeze: {e}")))?;
                lp.sample(&logits)
                    .map_err(|e| LlmError::ApiError(format!("Sampling: {e}")))
            }
        }
    }

    fn decode_token(
        &self,
        tlock: &Mutex<tokenizers::Tokenizer>,
        token: u32,
    ) -> Result<String, String> {
        let tok = tlock.lock().map_err(|e| e.to_string())?;
        tok.decode(&[token], true).map_err(|e| e.to_string())
    }

    pub fn load_model_from_path(&mut self, path: &Path) -> LlmResult<()> {
        self.load_gguf(path)
    }

    fn load_gguf(&mut self, path: &Path) -> LlmResult<()> {
        let mut file = std::fs::File::open(path)
            .map_err(|e| LlmError::ApiError(format!("Cannot open {path:?}: {e}")))?;

        let content = gguf_file::Content::read(&mut file)
            .map_err(|e| LlmError::ApiError(format!("GGUF parse: {e}")))?;

        let model_type: String = content
            .metadata
            .get("general.architecture")
            .and_then(|v| v.to_string().ok())
            .map(|s| s.to_string())
            .unwrap_or_default();

        let model = match model_type.as_str() {
            "qwen2" | "qwen3" | "" => {
                let m = candle_transformers::models::quantized_qwen2::ModelWeights::from_gguf(
                    content,
                    &mut file,
                    &self.device,
                )
                .map_err(|e| LlmError::ApiError(format!("Model build: {e}")))?;
                LoadedModel::Qwen2(Mutex::new(m))
            }
            "llama" => {
                return Err(LlmError::ApiError(
                    "LLaMA quantized model support not yet enabled, use Qwen2 GGUF".into(),
                ));
            }
            other => {
                return Err(LlmError::ApiError(format!(
                    "Unsupported model architecture: {other}"
                )));
            }
        };

        self.model_path = Some(path.to_path_buf());
        self.model = Some(model);

        if let Some(tp) = Self::find_tokenizer(path) {
            let t = tokenizers::Tokenizer::from_file(&tp)
                .map_err(|e| LlmError::ApiError(format!("Tokenizer: {e}")))?;
            self.tokenizer = Some(Mutex::new(t));
        }

        Ok(())
    }

    fn find_tokenizer(model_path: &Path) -> Option<PathBuf> {
        let dir = model_path.parent()?;
        let p = dir.join("tokenizer.json");
        if p.exists() {
            Some(p)
        } else {
            None
        }
    }

    pub fn is_available(&self) -> bool {
        self.model.is_some() && self.tokenizer.is_some()
    }

    pub fn model_path(&self) -> Option<&Path> {
        self.model_path.as_deref()
    }
}

pub fn detect_local_models() -> Vec<String> {
    let dirs = vec![
        PathBuf::from(std::env::var("AIOS_MODELS_DIR").unwrap_or_else(|_| "models".into())),
        PathBuf::from("models"),
    ];
    let mut found = Vec::new();
    for dir in &dirs {
        if dir.exists() {
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().is_some_and(|e| e == "gguf") {
                        if let Some(name) = path.file_stem() {
                            found.push(name.to_string_lossy().to_string());
                        }
                    }
                }
            }
        }
    }
    found
}

pub fn download_default_model(kind: LocalModelKind) -> LlmResult<PathBuf> {
    let (repo_id, filename) = match kind {
        LocalModelKind::Micro => (
            "Qwen/Qwen2.5-0.5B-Instruct-GGUF".to_string(),
            "qwen2.5-0.5b-instruct-q4_k_m.gguf".to_string(),
        ),
        LocalModelKind::Full => (
            "Qwen/Qwen2.5-7B-Instruct-GGUF".to_string(),
            "qwen2.5-7b-instruct-q4_k_m.gguf".to_string(),
        ),
    };

    let client =
        HFClientSync::new().map_err(|e| LlmError::ApiError(format!("HF Hub init: {e}")))?;
    let (org, name) = repo_id.split_once('/').unwrap_or(("Qwen", &repo_id));
    let model = client.model(org, name);
    let path = model
        .download_file()
        .filename(&filename)
        .send()
        .map_err(|e| LlmError::ApiError(format!("Download {filename}: {e}")))?;

    let _ = model.download_file().filename("tokenizer.json").send();
    Ok(path)
}
