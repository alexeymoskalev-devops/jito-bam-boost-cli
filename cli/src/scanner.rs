use jito_bam_boost_merkle_tree::bam_boost_entry::BamBoostEntry;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

pub const DEFAULT_GCS_BASE: &str = "https://storage.googleapis.com";

const RETRY_ATTEMPTS: u32 = 3;
const RETRY_BASE_DELAY_MS: u64 = 300;

/// Discovers BAM Boost epochs, allocations, and claim statuses.
pub struct Scanner {
    base_url: String,
    cache_dir: PathBuf,
    http: reqwest::Client,
}

impl Scanner {
    pub fn new(cache_dir: PathBuf) -> Self {
        Self::with_base_url(DEFAULT_GCS_BASE.to_string(), cache_dir)
    }

    pub fn with_base_url(base_url: String, cache_dir: PathBuf) -> Self {
        Self {
            base_url,
            cache_dir,
            http: reqwest::Client::new(),
        }
    }

    /// Lists all epochs that have a published merkle tree, ascending.
    pub async fn list_epochs(&self, network: &str) -> anyhow::Result<Vec<u64>> {
        let mut epochs = Vec::new();
        let mut page_token: Option<String> = None;
        loop {
            let mut url = format!(
                "{}/storage/v1/b/jito-bam-boost/o?prefix={network}/&fields=items/name,nextPageToken&maxResults=1000",
                self.base_url
            );
            if let Some(token) = &page_token {
                url.push_str(&format!("&pageToken={token}"));
            }
            let resp: ListResponse = self
                .http
                .get(&url)
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            epochs.extend(
                resp.items
                    .iter()
                    .filter_map(|item| parse_epoch_from_object_name(&item.name, network)),
            );
            match resp.next_page_token {
                Some(token) => page_token = Some(token),
                None => break,
            }
        }
        epochs.sort_unstable();
        Ok(epochs)
    }

    /// Downloads (or reads from cache) the entry list for an epoch.
    /// Returns `None` when no distribution exists for that epoch (HTTP 404).
    pub async fn fetch_entries(
        &self,
        network: &str,
        epoch: u64,
    ) -> anyhow::Result<Option<Vec<BamBoostEntry>>> {
        let cache_path = self.cache_dir.join(network).join(format!("{epoch}.json"));
        if let Ok(raw) = std::fs::read_to_string(&cache_path) {
            match serde_json::from_str::<Vec<BamBoostEntry>>(&raw) {
                Ok(entries) => return Ok(Some(entries)),
                Err(e) => {
                    log::warn!(
                        "corrupted cache for {network}/{epoch}: {e}, deleting and re-fetching"
                    );
                    let _ = std::fs::remove_file(&cache_path);
                }
            }
        }

        let url = format!(
            "{}/jito-bam-boost/{network}/{epoch}/merkle_tree.json",
            self.base_url
        );

        let mut delay = Duration::from_millis(RETRY_BASE_DELAY_MS);
        for attempt in 1..=RETRY_ATTEMPTS {
            match self.http.get(&url).send().await {
                Ok(resp) if resp.status() == reqwest::StatusCode::NOT_FOUND => return Ok(None),
                Ok(resp) => match resp.error_for_status() {
                    Ok(resp) => {
                        let raw = resp.text().await?;
                        let entries: Vec<BamBoostEntry> = serde_json::from_str(&raw)?;
                        if let Some(parent) = cache_path.parent() {
                            std::fs::create_dir_all(parent)?;
                        }
                        // Atomic write: write to temp file, then rename
                        let tmp_path = cache_path.with_extension("json.tmp");
                        std::fs::write(&tmp_path, &raw)?;
                        std::fs::rename(&tmp_path, &cache_path)?;
                        return Ok(Some(entries));
                    }
                    Err(e) if attempt < RETRY_ATTEMPTS => {
                        log::warn!("fetch epoch {epoch} attempt {attempt} failed: {e}");
                    }
                    Err(e) => return Err(e.into()),
                },
                Err(e) if attempt < RETRY_ATTEMPTS => {
                    log::warn!("fetch epoch {epoch} attempt {attempt} failed: {e}");
                }
                Err(e) => return Err(e.into()),
            }
            tokio::time::sleep(delay).await;
            delay *= 2;
        }
        unreachable!("retry loop always returns")
    }
}

/// Status of one epoch's subsidy for a claimant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpochStatus {
    pub epoch: u64,
    /// Allocation in JitoSOL lamports; `None` = claimant not in that epoch's tree.
    pub amount: Option<u64>,
    pub claimed: bool,
}

#[derive(Deserialize)]
struct ListResponse {
    #[serde(default)]
    items: Vec<ListItem>,
    #[serde(rename = "nextPageToken")]
    next_page_token: Option<String>,
}

#[derive(Deserialize)]
struct ListItem {
    name: String,
}

/// Extracts the epoch from a GCS object name like `mainnet/1000/merkle_tree.json`.
pub fn parse_epoch_from_object_name(name: &str, network: &str) -> Option<u64> {
    let rest = name.strip_prefix(network)?.strip_prefix('/')?;
    let (epoch_str, file) = rest.split_once('/')?;
    if file != "merkle_tree.json" {
        return None;
    }
    epoch_str.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_epoch_from_object_name() {
        assert_eq!(
            parse_epoch_from_object_name("mainnet/1000/merkle_tree.json", "mainnet"),
            Some(1000)
        );
    }

    #[test]
    fn ignores_directory_placeholder_and_foreign_names() {
        assert_eq!(parse_epoch_from_object_name("mainnet/", "mainnet"), None);
        assert_eq!(
            parse_epoch_from_object_name("testnet/900/merkle_tree.json", "mainnet"),
            None
        );
        assert_eq!(
            parse_epoch_from_object_name("mainnet/abc/merkle_tree.json", "mainnet"),
            None
        );
    }

    #[tokio::test]
    async fn lists_epochs_across_pages() {
        let server = httpmock::MockServer::start();
        let page1 = server.mock(|when, then| {
            when.method("GET")
                .path("/storage/v1/b/jito-bam-boost/o")
                .query_param("prefix", "mainnet/")
                .matches(|req| {
                    // page 1 = request without a pageToken query param
                    req.query_params
                        .as_ref()
                        .is_none_or(|ps| !ps.iter().any(|(k, _)| k == "pageToken"))
                });
            then.status(200).json_body(serde_json::json!({
                "items": [
                    {"name": "mainnet/"},
                    {"name": "mainnet/1000/merkle_tree.json"},
                    {"name": "mainnet/998/merkle_tree.json"}
                ],
                "nextPageToken": "tok1"
            }));
        });
        let page2 = server.mock(|when, then| {
            when.method("GET")
                .path("/storage/v1/b/jito-bam-boost/o")
                .query_param("pageToken", "tok1");
            then.status(200).json_body(serde_json::json!({
                "items": [{"name": "mainnet/999/merkle_tree.json"}]
            }));
        });

        let tmp = tempfile::tempdir().unwrap();
        let scanner = Scanner::with_base_url(server.base_url(), tmp.path().to_path_buf());
        let epochs = scanner.list_epochs("mainnet").await.unwrap();

        page1.assert();
        page2.assert();
        assert_eq!(epochs, vec![998, 999, 1000]);
    }

    fn entries_json() -> serde_json::Value {
        serde_json::json!([
            {"pubkey": "J1toso1uCk3RLmjorhTtrVwY9HJ7X8V9yYac6Y7kGCPn", "amount": 1234}
        ])
    }

    #[tokio::test]
    async fn fetches_entries_and_caches_them() {
        let server = httpmock::MockServer::start();
        let mock = server.mock(|when, then| {
            when.method("GET")
                .path("/jito-bam-boost/mainnet/7/merkle_tree.json");
            then.status(200).json_body(entries_json());
        });
        let tmp = tempfile::tempdir().unwrap();
        let scanner = Scanner::with_base_url(server.base_url(), tmp.path().to_path_buf());

        let first = scanner.fetch_entries("mainnet", 7).await.unwrap().unwrap();
        let second = scanner.fetch_entries("mainnet", 7).await.unwrap().unwrap();

        mock.assert_hits(1); // second call served from cache
        assert_eq!(first[0].amount, 1234);
        assert_eq!(first, second);
        assert!(tmp.path().join("mainnet/7.json").exists());
    }

    #[tokio::test]
    async fn returns_none_on_404() {
        let server = httpmock::MockServer::start();
        server.mock(|when, then| {
            when.method("GET")
                .path("/jito-bam-boost/mainnet/8/merkle_tree.json");
            then.status(404);
        });
        let tmp = tempfile::tempdir().unwrap();
        let scanner = Scanner::with_base_url(server.base_url(), tmp.path().to_path_buf());
        assert!(scanner.fetch_entries("mainnet", 8).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn retries_transient_errors() {
        let server = httpmock::MockServer::start();
        // httpmock serves mocks in order of creation once exhausted; emulate
        // one 500 then success via hit-limited mock.
        let failing = server.mock(|when, then| {
            when.method("GET")
                .path("/jito-bam-boost/mainnet/9/merkle_tree.json");
            then.status(500);
        });
        let tmp = tempfile::tempdir().unwrap();
        let scanner = Scanner::with_base_url(server.base_url(), tmp.path().to_path_buf());
        let err = scanner.fetch_entries("mainnet", 9).await;
        assert!(err.is_err());
        failing.assert_hits(3); // 3 attempts total
    }

    #[tokio::test]
    async fn recovers_from_corrupted_cache() {
        let server = httpmock::MockServer::start();
        let mock = server.mock(|when, then| {
            when.method("GET")
                .path("/jito-bam-boost/mainnet/10/merkle_tree.json");
            then.status(200).json_body(entries_json());
        });
        let tmp = tempfile::tempdir().unwrap();
        let scanner = Scanner::with_base_url(server.base_url(), tmp.path().to_path_buf());

        // Write corrupted cache file
        let cache_path = tmp.path().join("mainnet").join("10.json");
        std::fs::create_dir_all(cache_path.parent().unwrap()).unwrap();
        std::fs::write(&cache_path, "{invalid").unwrap();

        // Fetch should recover from corrupted cache and fetch from network
        let entries = scanner.fetch_entries("mainnet", 10).await.unwrap().unwrap();

        // Verify entries were fetched and cache was healed
        assert_eq!(entries[0].amount, 1234);
        mock.assert_hits(1);
        let cached = std::fs::read_to_string(&cache_path).unwrap();
        let reparsed: Vec<BamBoostEntry> = serde_json::from_str(&cached).unwrap();
        assert_eq!(reparsed, entries);
    }
}
