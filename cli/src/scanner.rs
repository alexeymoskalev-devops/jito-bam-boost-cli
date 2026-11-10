use crate::{pda, JITOSOL_MINT};
use futures::stream::{self, StreamExt};
use jito_bam_boost_merkle_tree::bam_boost_entry::BamBoostEntry;
use serde::{Deserialize, Serialize};
use solana_pubkey::Pubkey;
use solana_rpc_client::rpc_client::RpcClient;
use std::collections::{HashMap, HashSet};
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

/// Merges per-epoch amounts, claim flags and expiry into the final status list.
pub fn combine(
    epochs: &[u64],
    amounts: &HashMap<u64, Option<u64>>,
    claimed: &HashMap<u64, bool>,
    expired: &HashSet<u64>,
) -> Vec<EpochStatus> {
    epochs
        .iter()
        .map(|&epoch| EpochStatus {
            epoch,
            amount: amounts.get(&epoch).copied().flatten(),
            claimed: claimed.get(&epoch).copied().unwrap_or(false),
            expired: expired.contains(&epoch),
        })
        .collect()
}

/// Claim window of an epoch's MerkleDistributor account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClaimWindow {
    pub epoch: u64,
    /// Epoch from which unclaimed funds may be clawed back.
    pub clawback_start_epoch: u64,
    /// Unclaimed funds were already clawed back (claims fail with `ClaimExpired`).
    pub clawed_back: bool,
}

impl ClaimWindow {
    /// Conservative: whether claims are still accepted during `clawback_start_epoch`
    /// itself is unconfirmed, so that epoch counts as open unless clawback already
    /// happened. Worst case is a claim attempt that fails, never a hidden allocation.
    pub fn is_expired(&self, current_epoch: u64) -> bool {
        self.clawed_back || current_epoch > self.clawback_start_epoch
    }
}

// MerkleDistributor layout, decoded from mainnet accounts (the program source is
// not public): discriminator(8) bump(1) root(32) mint(32), then u64 LE fields
// max_total_claim, max_num_nodes, total_amount_claimed, num_nodes_claimed,
// epoch, end_epoch, clawback_start_epoch, then clawback_receiver(32), clawed_back(1).
const DISTRIBUTOR_DISCRIMINATOR: [u8; 8] = [0x4d, 0x77, 0x8b, 0x46, 0x54, 0xf7, 0x0c, 0x1a];
const EPOCH_OFFSET: usize = 8 + 1 + 32 + 32 + 4 * 8;
const CLAWBACK_START_OFFSET: usize = EPOCH_OFFSET + 2 * 8;
const CLAWED_BACK_OFFSET: usize = CLAWBACK_START_OFFSET + 8 + 32;

/// Decodes the claim window from raw MerkleDistributor account data.
/// Returns `None` if the data is not a MerkleDistributor of the known layout.
pub fn parse_claim_window(data: &[u8]) -> Option<ClaimWindow> {
    if data.len() <= CLAWED_BACK_OFFSET || data[..8] != DISTRIBUTOR_DISCRIMINATOR {
        return None;
    }
    let read_u64 = |offset: usize| u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
    let clawed_back = match data[CLAWED_BACK_OFFSET] {
        0 => false,
        1 => true,
        _ => return None,
    };
    Some(ClaimWindow {
        epoch: read_u64(EPOCH_OFFSET),
        clawback_start_epoch: read_u64(CLAWBACK_START_OFFSET),
        clawed_back,
    })
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

        let unclaimed: Vec<u64> = eligible
            .iter()
            .copied()
            .filter(|e| !claimed.get(e).copied().unwrap_or(false))
            .collect();
        // Expiry is advisory: if it cannot be determined, keep epochs claimable
        // rather than failing the whole scan.
        let expired = expired_epochs(&unclaimed, rpc, program_id).unwrap_or_else(|e| {
            log::warn!("could not determine claim window expiry: {e}");
            HashSet::new()
        });

        Ok(combine(&epochs, &amounts, &claimed, &expired))
    }
}

/// Returns the epochs (out of `epochs`) whose claim window has closed.
/// An epoch whose distributor cannot be read or decoded is logged and treated
/// as open, so a layout change never hides a claimable allocation.
fn expired_epochs(
    epochs: &[u64],
    rpc: &RpcClient,
    program_id: &Pubkey,
) -> anyhow::Result<HashSet<u64>> {
    let mut expired = HashSet::new();
    if epochs.is_empty() {
        return Ok(expired);
    }
    let current_epoch = rpc.get_epoch_info()?.epoch;
    let distributors: Vec<Pubkey> = epochs
        .iter()
        .map(|&epoch| pda::merkle_distributor_address(program_id, &JITOSOL_MINT, epoch))
        .collect();
    for (chunk_epochs, chunk_pdas) in epochs.chunks(RPC_BATCH).zip(distributors.chunks(RPC_BATCH)) {
        let accounts = rpc.get_multiple_accounts(chunk_pdas)?;
        for (&epoch, account) in chunk_epochs.iter().zip(accounts) {
            let Some(account) = account else {
                log::warn!("epoch {epoch}: distributor account not found, expiry unknown");
                continue;
            };
            match parse_claim_window(&account.data) {
                Some(window) if window.epoch != epoch => log::warn!(
                    "epoch {epoch}: distributor reports epoch {}, expiry unknown",
                    window.epoch
                ),
                Some(window) if window.is_expired(current_epoch) => {
                    expired.insert(epoch);
                }
                Some(_) => {}
                None => log::warn!("epoch {epoch}: unexpected distributor layout, expiry unknown"),
            }
        }
    }
    Ok(expired)
}

/// Status of one epoch's subsidy for a claimant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpochStatus {
    pub epoch: u64,
    /// Allocation in JitoSOL lamports; `None` = claimant not in that epoch's tree.
    pub amount: Option<u64>,
    pub claimed: bool,
    /// Unclaimed allocation whose claim window has closed.
    #[serde(default)]
    pub expired: bool,
}

impl EpochStatus {
    /// Whether this epoch has an unclaimed allocation that can still be claimed.
    pub fn is_claimable(&self) -> bool {
        self.amount.is_some() && !self.claimed && !self.expired
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
        use std::collections::{HashMap, HashSet};
        let epochs = vec![1, 2, 3, 4];
        let amounts: HashMap<u64, Option<u64>> =
            [(1, Some(10)), (2, None), (3, Some(30)), (4, Some(40))].into();
        let claimed: HashMap<u64, bool> = [(1, true), (3, false), (4, false)].into();
        let expired: HashSet<u64> = [4].into();
        let out = combine(&epochs, &amounts, &claimed, &expired);
        assert_eq!(
            out,
            vec![
                EpochStatus {
                    epoch: 1,
                    amount: Some(10),
                    claimed: true,
                    expired: false,
                },
                EpochStatus {
                    epoch: 2,
                    amount: None,
                    claimed: false,
                    expired: false,
                },
                EpochStatus {
                    epoch: 3,
                    amount: Some(30),
                    claimed: false,
                    expired: false,
                },
                EpochStatus {
                    epoch: 4,
                    amount: Some(40),
                    claimed: false,
                    expired: true,
                },
            ]
        );
        assert!(out[2].is_claimable());
        assert!(
            !out[3].is_claimable(),
            "expired epoch must not be claimable"
        );
    }

    // Raw mainnet MerkleDistributor accounts (fetched 2026-11-09, current epoch 1082).
    const DISTRIBUTOR_931_HEX: &str = "4d778b4654f70c1aff2872092f678907dc13997196080f67d9e36f6f3cb7a5ca6aea62d87753a58e6ffcd141e9832caf10ad917495ca0f271b5b293cd47027ea737007ed40eb39a0bd5e67ca8b830000002901000000000000742e9e0274000000f600000000000000a303000000000000ac03000000000000ad03000000000000768f28a3a3b2fb52f35a58a7e58b595c44c778ed012cf71aa6f9c236700c6427017fe91e5d11000000ccfced2411000000000000000000";
    const DISTRIBUTOR_1081_HEX: &str = "4d778b4654f70c1aff70f92d5e40de705f2914e656a2192745443121c6da3d10972c380e613ae649bafcd141e9832caf10ad917495ca0f271b5b293cd47027ea737007ed40eb39a0bd2cee2db22800000068010000000000000e12803c11000000940000000000000039040000000000004c040000000000004d04000000000000768f28a3a3b2fb52f35a58a7e58b595c44c778ed012cf71aa6f9c236700c642700b0244aeb0500000089efd36803000000000000000000";

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn parses_claim_window_from_distributor_account() {
        let w931 = parse_claim_window(&hex(DISTRIBUTOR_931_HEX)).unwrap();
        assert_eq!(
            w931,
            ClaimWindow {
                epoch: 931,
                clawback_start_epoch: 941,
                clawed_back: true
            }
        );
        let w1081 = parse_claim_window(&hex(DISTRIBUTOR_1081_HEX)).unwrap();
        assert_eq!(
            w1081,
            ClaimWindow {
                epoch: 1081,
                clawback_start_epoch: 1101,
                clawed_back: false
            }
        );
    }

    #[test]
    fn claim_window_expiry() {
        let open = ClaimWindow {
            epoch: 1081,
            clawback_start_epoch: 1101,
            clawed_back: false,
        };
        assert!(!open.is_expired(1082));
        assert!(!open.is_expired(1100));
        assert!(
            !open.is_expired(1101),
            "clawback start epoch itself counts as open"
        );
        assert!(open.is_expired(1102));
        let clawed = ClaimWindow {
            clawed_back: true,
            ..open
        };
        assert!(clawed.is_expired(1082));
    }

    #[test]
    fn rejects_foreign_or_short_account_data() {
        let mut data = hex(DISTRIBUTOR_931_HEX);
        assert!(parse_claim_window(&data[..100]).is_none());
        data[0] ^= 0xff;
        assert!(parse_claim_window(&data).is_none());
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
