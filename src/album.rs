use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context;
use image::RgbImage;
use reqwest::Client;
use tokio::sync::Mutex;

use crate::config::FitMethod;

const CACHE_TTL: Duration = Duration::from_hours(1);
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
    share_url: String,
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
            share_url,
            cache: Arc::new(Mutex::new(None)),
        })
    }

    /// Fetches one photo from the album, resized by Google according to `fit`.
    /// The caller picks the index via `select`, which receives the current
    /// album size and a list of indices that are new since the previously
    /// cached scrape (empty on cache hit, on first-ever scrape, or when no
    /// photos changed). It must return an index in `[0, n)`. The returned
    /// image's dimensions are whatever Google returned — it is the caller's
    /// job to reconcile them with the target screen size. When `fresh` is
    /// true the cached share-page contents are dropped before resolving,
    /// forcing a re-scrape.
    pub async fn pick<F>(
        &self,
        width: u32,
        height: u32,
        fit: &FitMethod,
        fresh: bool,
        select: F,
    ) -> anyhow::Result<RgbImage>
    where
        F: FnOnce(usize, &[usize]) -> usize,
    {
        let (urls, new) = self.photo_urls(fresh).await?;
        anyhow::ensure!(!urls.is_empty(), "album returned no photos");
        let index = select(urls.len(), &new);
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

    /// Returns the album URLs and a list of indices that are new compared to
    /// the previously cached scrape. The new-list is empty on a cache hit, on
    /// the first-ever scrape (no prior cache to diff against), and when the
    /// scrape returns the same set of URLs as before.
    async fn photo_urls(&self, fresh: bool) -> anyhow::Result<(Arc<Vec<String>>, Vec<usize>)> {
        let mut guard = self.cache.lock().await;
        if !fresh
            && let Some(c) = &*guard
            && c.fetched_at.elapsed() < CACHE_TTL
        {
            return Ok((Arc::clone(&c.urls), Vec::new()));
        }

        let urls = self.scrape().await?;
        tracing::info!(count = urls.len(), share_url = %self.share_url, "loaded album");
        anyhow::ensure!(
            !urls.is_empty(),
            "no photos found on share page — is the album public?"
        );

        let new = match guard.as_ref() {
            Some(prev) => new_indices_in(&prev.urls, &urls),
            None => Vec::new(),
        };
        if !new.is_empty() {
            tracing::info!(count = new.len(), "detected new photos in album");
        }

        let urls = Arc::new(urls);
        *guard = Some(Cache {
            urls: Arc::clone(&urls),
            fetched_at: Instant::now(),
        });
        Ok((urls, new))
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

/// Indices in `current` whose URL does not appear in `previous`.
fn new_indices_in(previous: &[String], current: &[String]) -> Vec<usize> {
    let prev: HashSet<&str> = previous.iter().map(String::as_str).collect();
    current
        .iter()
        .enumerate()
        .filter_map(|(i, u)| (!prev.contains(u.as_str())).then_some(i))
        .collect()
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

    fn s(values: &[&str]) -> Vec<String> {
        values.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn new_indices_finds_added_photos() {
        let prev = s(&["a", "b", "c"]);
        let curr = s(&["a", "d", "b", "e"]);
        assert_eq!(new_indices_in(&prev, &curr), vec![1, 3]);
    }

    #[test]
    fn new_indices_ignores_removed_photos() {
        let prev = s(&["a", "b", "c"]);
        let curr = s(&["a", "c"]);
        assert!(new_indices_in(&prev, &curr).is_empty());
    }

    #[test]
    fn new_indices_empty_when_unchanged() {
        let urls = s(&["a", "b", "c"]);
        assert!(new_indices_in(&urls, &urls).is_empty());
    }

    #[test]
    fn new_indices_all_when_previous_empty() {
        let curr = s(&["a", "b"]);
        assert_eq!(new_indices_in(&[], &curr), vec![0, 1]);
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
