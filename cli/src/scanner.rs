use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const DEFAULT_GCS_BASE: &str = "https://storage.googleapis.com";

/// Discovers BAM Boost epochs, allocations, and claim statuses.
pub struct Scanner {
    base_url: String,
    #[allow(dead_code)]
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
}
