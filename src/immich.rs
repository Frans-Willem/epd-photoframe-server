use anyhow::Context;
use reqwest::Client;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Asset {
    pub id: String,
}

#[derive(Clone)]
pub struct ImmichClient {
    client: Client,
    base_url: String,
    api_key: String,
}

impl ImmichClient {
    pub fn new(base_url: String, api_key: String) -> Self {
        Self {
            client: Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key,
        }
    }

    pub async fn random_image_bytes(&self) -> anyhow::Result<Vec<u8>> {
        let assets: Vec<Asset> = self
            .client
            .get(format!("{}/api/assets/random", self.base_url))
            .query(&[("count", "1")])
            .header("x-api-key", &self.api_key)
            .send()
            .await
            .context("requesting random asset")?
            .error_for_status()
            .context("random asset API error")?
            .json()
            .await
            .context("parsing random asset response")?;

        let asset = assets
            .into_iter()
            .next()
            .context("Immich returned empty asset list")?;

        tracing::debug!(id = %asset.id, "fetching asset");

        let bytes = self
            .client
            .get(format!("{}/api/assets/{}/original", self.base_url, asset.id))
            .header("x-api-key", &self.api_key)
            .send()
            .await
            .context("downloading asset")?
            .error_for_status()
            .context("asset download error")?
            .bytes()
            .await
            .context("reading asset bytes")?;

        Ok(bytes.to_vec())
    }
}
