use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context;
use image::RgbImage;
use reqwest::Client;
use tokio::sync::Mutex;

use crate::config::FitMethod;

const CACHE_TTL: Duration = Duration::from_secs(3600);
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);
const USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 \
    (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";
const URL_PREFIX: &str = "https://lh3.googleusercontent.com/pw/";

struct Cache {
    urls: Arc<Vec<String>>,
    fetched_at: Instant,
}

#[derive(Clone)]
pub struct AlbumClient {
    client: Client,
    share_url: Arc<String>,
    cache: Arc<Mutex<Option<Cache>>>,
}

impl AlbumClient {
    pub fn new(share_url: String) -> anyhow::Result<Self> {
        let client = Client::builder()
            .user_agent(USER_AGENT)
            .timeout(HTTP_TIMEOUT)
            .build()
            .context("building HTTP client")?;
        Ok(Self {
            client,
            share_url: Arc::new(share_url),
            cache: Arc::new(Mutex::new(None)),
        })
    }

    /// Fetches one photo from the album, resized by Google according to `fit`.
    /// The caller picks the index via `select`, which receives the current
    /// album size and returns an index in `[0, n)`. The returned image's
    /// dimensions are whatever Google returned — it is the caller's job to
    /// reconcile them with the target screen size.
    pub async fn pick<F>(
        &self,
        width: u32,
        height: u32,
        fit: &FitMethod,
        select: F,
    ) -> anyhow::Result<RgbImage>
    where
        F: FnOnce(usize) -> usize,
    {
        let urls = self.photo_urls().await?;
        anyhow::ensure!(!urls.is_empty(), "album returned no photos");
        let index = select(urls.len());
        anyhow::ensure!(
            index < urls.len(),
            "selector returned out-of-range index {index}/{}",
            urls.len()
        );

        let base = &urls[index];
        let sized = format!("{base}{}", size_suffix(width, height, fit));

        tracing::debug!(url = %sized, index, of = urls.len(), "downloading photo");
        let bytes = self
            .client
            .get(&sized)
            .send()
            .await
            .context("downloading photo")?
            .error_for_status()
            .context("photo download error")?
            .bytes()
            .await
            .context("reading photo bytes")?;

        Ok(image::load_from_memory(&bytes)
            .context("decoding image")?
            .into_rgb8())
    }

    async fn photo_urls(&self) -> anyhow::Result<Arc<Vec<String>>> {
        let mut guard = self.cache.lock().await;
        if let Some(c) = &*guard
            && c.fetched_at.elapsed() < CACHE_TTL
        {
            return Ok(Arc::clone(&c.urls));
        }

        let urls = self.scrape().await?;
        tracing::info!(count = urls.len(), share_url = %self.share_url, "loaded album");
        anyhow::ensure!(
            !urls.is_empty(),
            "no photos found on share page — is the album public?"
        );

        let urls = Arc::new(urls);
        *guard = Some(Cache {
            urls: Arc::clone(&urls),
            fetched_at: Instant::now(),
        });
        Ok(urls)
    }

    async fn scrape(&self) -> anyhow::Result<Vec<String>> {
        let html = self
            .client
            .get(self.share_url.as_str())
            .send()
            .await
            .context("fetching album page")?
            .error_for_status()
            .context("album page error")?
            .text()
            .await
            .context("reading album page")?;
        Ok(extract_photo_urls(&html))
    }
}

/// Pull `https://lh3.googleusercontent.com/pw/<id>` URLs out of the share-page
/// HTML. These are the inline-JSON photo base URLs — the owner's avatar
/// (under `/a/`) and other non-photo assets don't match the `/pw/` prefix.
fn extract_photo_urls(html: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(pos) = rest.find(URL_PREFIX) {
        let after = &rest[pos + URL_PREFIX.len()..];
        let id_len = after
            .bytes()
            .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b == b'-')
            .count();
        let end = pos + URL_PREFIX.len() + id_len;
        let url = rest[pos..end].to_string();
        if id_len > 0 && seen.insert(url.clone()) {
            out.push(url);
        }
        rest = &rest[end.max(pos + 1)..];
    }
    out
}

fn size_suffix(width: u32, height: u32, fit: &FitMethod) -> String {
    let modifier = match fit {
        FitMethod::Crop => "-c",
        FitMethod::SmartCrop => "-p",
        FitMethod::Resize => "-s",
        FitMethod::Contain => "",
    };
    format!("=w{width}-h{height}{modifier}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_and_dedupes() {
        let html = r#"
            before "https://lh3.googleusercontent.com/pw/ABC_123-xyz" middle
            repeat "https://lh3.googleusercontent.com/pw/ABC_123-xyz" again
            other https://lh3.googleusercontent.com/pw/DEF456?x=1
            avatar https://lh3.googleusercontent.com/a/avatar_id end
        "#;
        let urls = extract_photo_urls(html);
        assert_eq!(
            urls,
            vec![
                "https://lh3.googleusercontent.com/pw/ABC_123-xyz".to_string(),
                "https://lh3.googleusercontent.com/pw/DEF456".to_string(),
            ]
        );
    }

    #[test]
    fn size_suffixes() {
        assert_eq!(size_suffix(1200, 1600, &FitMethod::Crop), "=w1200-h1600-c");
        assert_eq!(
            size_suffix(1200, 1600, &FitMethod::SmartCrop),
            "=w1200-h1600-p"
        );
        assert_eq!(
            size_suffix(1200, 1600, &FitMethod::Resize),
            "=w1200-h1600-s"
        );
        assert_eq!(size_suffix(1200, 1600, &FitMethod::Contain), "=w1200-h1600");
    }
}
