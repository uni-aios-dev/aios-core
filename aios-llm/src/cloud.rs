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
