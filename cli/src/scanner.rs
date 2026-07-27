use crate::{pda, JITOSOL_MINT};
use futures::stream::{self, StreamExt};
use jito_bam_boost_merkle_tree::bam_boost_entry::BamBoostEntry;
use serde::{Deserialize, Serialize};
use solana_pubkey::Pubkey;
use solana_rpc_client::rpc_client::RpcClient;
use std::collections::HashMap;
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

/// Finds the claimant's allocation in an epoch's entry list.
pub fn amount_for(entries: &[BamBoostEntry], claimant: &Pubkey) -> Option<u64> {
    let claimant = claimant.to_string();
    entries
        .iter()
        .find(|e| e.pubkey == claimant)
        .map(|e| e.amount)
}

/// Merges per-epoch amounts and claim flags into the final status list.
pub fn combine(
    epochs: &[u64],
    amounts: &HashMap<u64, Option<u64>>,
    claimed: &HashMap<u64, bool>,
) -> Vec<EpochStatus> {
    epochs
        .iter()
        .map(|&epoch| EpochStatus {
            epoch,
            amount: amounts.get(&epoch).copied().flatten(),
            claimed: claimed.get(&epoch).copied().unwrap_or(false),
        })
        .collect()
}

const FETCH_CONCURRENCY: usize = 8;
const RPC_BATCH: usize = 100;

impl Scanner {
    /// Full scan: which epochs exist, what the claimant is owed, what is claimed.
    pub async fn scan(
        &self,
        network: &str,
        claimant: &Pubkey,
        rpc: &RpcClient,
        program_id: &Pubkey,
    ) -> anyhow::Result<Vec<EpochStatus>> {
        let epochs = self.list_epochs(network).await?;

        let scanner = self;
        let amounts: HashMap<u64, Option<u64>> = stream::iter(epochs.clone())
            .map(|epoch| async move {
                let entries = scanner.fetch_entries(network, epoch).await?;
                let amount = entries.as_deref().and_then(|e| amount_for(e, claimant));
                Ok::<_, anyhow::Error>((epoch, amount))
            })
            .buffer_unordered(FETCH_CONCURRENCY)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<Result<_, _>>()?;

        // Only epochs with an allocation need an on-chain check.
        let eligible: Vec<u64> = epochs
            .iter()
            .copied()
            .filter(|e| matches!(amounts.get(e), Some(Some(_))))
            .collect();

        let pdas: Vec<Pubkey> = eligible
            .iter()
            .map(|&epoch| {
                let distributor = pda::merkle_distributor_address(program_id, &JITOSOL_MINT, epoch);
                pda::claim_status_address(program_id, claimant, &distributor)
            })
            .collect();

        let mut claimed = HashMap::new();
        for (chunk_epochs, chunk_pdas) in eligible.chunks(RPC_BATCH).zip(pdas.chunks(RPC_BATCH)) {
            let accounts = rpc.get_multiple_accounts(chunk_pdas)?;
            for (&epoch, account) in chunk_epochs.iter().zip(accounts) {
                claimed.insert(epoch, account.is_some());
            }
        }

        Ok(combine(&epochs, &amounts, &claimed))
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

impl EpochStatus {
    /// Whether this epoch has an unclaimed allocation for the claimant.
    pub fn is_claimable(&self) -> bool {
        self.amount.is_some() && !self.claimed
    }
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
    fn amount_for_finds_claimant_by_string_pubkey() {
        use jito_bam_boost_merkle_tree::bam_boost_entry::BamBoostEntry;
        let claimant = solana_pubkey::Pubkey::new_unique();
        let entries = vec![
            BamBoostEntry::new(solana_pubkey::Pubkey::new_unique().to_string(), 5),
            BamBoostEntry::new(claimant.to_string(), 42),
        ];
        assert_eq!(amount_for(&entries, &claimant), Some(42));
        assert_eq!(amount_for(&entries[..1], &claimant), None);
    }

    #[test]
    fn combine_builds_epoch_statuses_in_order() {
        use std::collections::HashMap;
        let epochs = vec![1, 2, 3];
        let amounts: HashMap<u64, Option<u64>> = [(1, Some(10)), (2, None), (3, Some(30))].into();
        let claimed: HashMap<u64, bool> = [(1, true), (3, false)].into();
        let out = combine(&epochs, &amounts, &claimed);
        assert_eq!(
            out,
            vec![
                EpochStatus {
                    epoch: 1,
                    amount: Some(10),
                    claimed: true
                },
                EpochStatus {
                    epoch: 2,
                    amount: None,
                    claimed: false
                },
                EpochStatus {
                    epoch: 3,
                    amount: Some(30),
                    claimed: false
                },
            ]
        );
    }

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
