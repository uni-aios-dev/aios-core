use crate::types::*;
use reqwest::Client;

pub struct CloudEngine {
    client: Client,
    config: LlmConfig,
}

impl CloudEngine {
    pub fn new(config: LlmConfig) -> Self {
        Self {
            client: Client::new(),
            config,
        }
    }

    /// Returns the configuration this engine was created with.
    pub fn config(&self) -> &LlmConfig {
        &self.config
    }

    pub async fn query(&self, request: &LlmRequest) -> LlmResult<LlmResponse> {
        let start = std::time::Instant::now();
        let body = self.build_body(request);
        let api_key = self
            .config
            .api_key
            .as_deref()
            .ok_or_else(|| LlmError::NotAvailable("API key not set".into()))?;

        let url = self
            .config
            .api_url
            .clone()
            .unwrap_or_else(|| self.provider_url());

        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(LlmError::ApiError(format!("HTTP {status}: {text}")));
        }

        let elapsed = start.elapsed().as_millis() as u64;
        let response_text = self.extract_response(resp.json().await?)?;

        Ok(LlmResponse {
            text: response_text,
            tokens_used: 0,
            duration_ms: elapsed,
        })
    }

    /// Streams the completion token-by-token over `tx` instead of returning
    /// a full response. Sends `Err` first on any failure; sends nothing on
    /// success after the final delta.
    pub async fn query_stream(&self, request: &LlmRequest, tx: LlmStreamSink) {
        let api_key = match self.config.api_key.as_deref() {
            Some(key) => key,
            None => {
                let _ = tx.send(Err(LlmError::NotAvailable("API key not set".into())));
                return;
            }
        };

        let url = self
            .config
            .api_url
            .clone()
            .unwrap_or_else(|| self.provider_url());

        let google_shape = matches!(
            self.config.backend,
            BackendKind::Cloud(CloudProvider::GoogleAiStudio)
        );
        let body = self.build_body(request);

        let mut resp = match self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
        {
            Ok(resp) => resp,
            Err(e) => {
                let _ = tx.send(Err(LlmError::HttpError(e)));
                return;
            }
        };

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            let _ = tx.send(Err(LlmError::ApiError(format!("HTTP {status}: {text}"))));
            return;
        }

        let mut buf = String::new();
        loop {
            let chunk = match resp.chunk().await {
                Ok(Some(chunk)) => chunk,
                Ok(None) => break,
                Err(e) => {
                    let _ = tx.send(Err(LlmError::HttpError(e)));
                    return;
                }
            };
            buf.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(pos) = buf.find('\n') {
                let line = buf[..pos].to_string();
                buf.drain(..pos + 1);
                let trimmed = line.trim_end_matches('\r').trim();
                if trimmed.is_empty() {
                    continue;
                }
                let Some(payload) = trimmed.strip_prefix("data:") else {
                    continue;
                };
                let payload = payload.trim();
                if payload == "[DONE]" {
                    continue;
                }
                let delta = match extract_stream_delta(payload, google_shape) {
                    Ok(delta) => delta,
                    Err(e) => {
                        let _ = tx.send(Err(e));
                        return;
                    }
                };
                if !delta.is_empty() && tx.send(Ok(delta)).is_err() {
                    return;
                }
            }
        }
    }

    fn provider_url(&self) -> String {
        match self.config.backend {
            BackendKind::Cloud(ref provider) => provider.default_url().to_string(),
            _ => unreachable!(),
        }
    }

    fn build_body(&self, request: &LlmRequest) -> serde_json::Value {
        serde_json::json!({
            "model": self.config.model,
            "messages": [
                {"role": "system", "content": request.system_prompt},
                {"role": "user", "content": request.user_prompt}
            ],
            "max_tokens": request.max_tokens,
            "temperature": request.temperature,
        })
    }

    fn extract_response(&self, json: serde_json::Value) -> LlmResult<String> {
        let text = json["choices"][0]["message"]["content"]
            .as_str()
            .or_else(|| json["candidates"][0]["content"]["parts"][0]["text"].as_str())
            .ok_or_else(|| LlmError::ApiError("No content in response".into()))?;
        Ok(text.to_string())
    }
}
