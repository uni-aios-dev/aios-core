use crate::manifest::ManifestInfo;
use serde_json;

pub struct StoreClient {
    store_url: String,
    client: reqwest::Client,
}

impl StoreClient {
    pub fn new(store_url: &str) -> Self {
        Self {
            store_url: store_url.trim_end_matches('/').to_string(),
            client: reqwest::Client::new(),
        }
    }

    pub async fn fetch_index(&self) -> Result<Vec<ManifestInfo>, String> {
        let url = format!("{}/index.json", self.store_url);
        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("HTTP error: {e}"))?;

        let bytes = response
            .bytes()
            .await
            .map_err(|e| format!("Read error: {e}"))?;

        let index: Vec<ManifestInfo> =
            serde_json::from_slice(&bytes).map_err(|e| format!("JSON error: {e}"))?;

        Ok(index)
    }

    pub async fn download_block(
        &self,
        manifest: &ManifestInfo,
    ) -> Result<Vec<u8>, String> {
        let url = format!("{}/blocks/{}.wasm", self.store_url, manifest.name);
        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("HTTP error: {e}"))?;

        let bytes = response
            .bytes()
            .await
            .map_err(|e| format!("Read error: {e}"))?;

        Ok(bytes.to_vec())
    }

    pub fn store_url(&self) -> &str {
        &self.store_url
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_store_client_creation() {
        let client = StoreClient::new("https://github.com/uni-aios-dev/aios-official-store");
        assert!(client.store_url().contains("uni-aios-dev"));
    }
}
