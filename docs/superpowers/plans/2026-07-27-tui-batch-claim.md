# BAM Boost TUI & Batch Claim Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add epoch auto-discovery (`status`), batch claiming (`claim-all`), and a full-screen ratatui TUI to the jito-bam-boost-cli fork.

**Architecture:** Extend the existing `cli` crate. A new `scanner` module discovers epochs from the public GCS bucket, looks up the claimant's amounts, and checks ClaimStatus PDAs on-chain — producing `Vec<EpochStatus>`, the shared model for the `status` command, `claim-all`, and the TUI. A new `batch_claim` module reuses the existing claim instruction logic per epoch. The TUI is a pure-reducer state machine (`tui/app.rs`) with rendering (`tui/ui.rs`) and an async event loop (`tui/mod.rs`).

**Tech Stack:** Rust 1.89 (rust-toolchain.toml), clap 4 (derive), tokio, reqwest, ratatui 0.29 (bundled crossterm), httpmock (dev), solana-* 3.0 crates.

## Global Constraints

- Rust toolchain pinned: `1.89.0` (rust-toolchain.toml) — do not change.
- Workspace lints apply (`[lints] workspace = true`); code must pass `cargo clippy --all-targets` and `cargo fmt --check`.
- Follow upstream style: clap derive, `anyhow::Result`, `log::info!` for user-facing progress in CLI paths.
- Two `Pubkey` types exist: `solana_pubkey::Pubkey` (cli crate) and `solana_program::pubkey::Pubkey` (merkle-tree crate). Convert with `Pubkey::new_from_array(x.to_bytes())` exactly as the existing handler does.
- `BamBoostEntry { pubkey: String, amount: u64 }` — amounts are lamports of JitoSOL (9 decimals, `MINT_DECIMALS`).
- GCS base URL must be injectable (struct field), never hard-coded inside methods — tests use httpmock.
- Commits: conventional format (`feat:`, `test:`, `refactor:`, `docs:`), no attribution footer.
- Files stay under 800 lines; split if a module approaches that.
- Run tests with `cargo test -p jito-bam-boost-cli` from repo root.

---

### Task 1: Dependencies + `EpochStatus` model + GCS listing parser

**Files:**
- Modify: `Cargo.toml` (workspace deps)
- Modify: `cli/Cargo.toml`
- Create: `cli/src/scanner.rs`
- Modify: `cli/src/lib.rs` (add `pub mod scanner;`)

**Interfaces:**
- Produces: `scanner::EpochStatus { epoch: u64, amount: Option<u64>, claimed: bool }` (Serialize/Deserialize/Clone/Debug/PartialEq); `scanner::parse_epoch_from_object_name(name: &str, network: &str) -> Option<u64>`; `scanner::ListResponse` (private helper deserialization types).

- [ ] **Step 1: Add workspace dependencies**

In root `Cargo.toml` under `[workspace.dependencies]` add (alphabetical position):

```toml
dirs = "6.0.0"
futures = "0.3.31"
httpmock = "0.7.0"
ratatui = "0.29.0"
tempfile = "3.20.0"
```

And change tokio features to include sync/time:

```toml
tokio = { version = "1.43.0", features = ["macros", "rt-multi-thread", "sync", "time"] }
```

In `cli/Cargo.toml` add to `[dependencies]`:

```toml
dirs.workspace = true
futures.workspace = true
ratatui.workspace = true
serde.workspace = true
```

and a new section:

```toml
[dev-dependencies]
httpmock.workspace = true
tempfile.workspace = true
```

- [ ] **Step 2: Verify the workspace still builds**

Run: `cargo build -p jito-bam-boost-cli`
Expected: success (new deps download and compile).

- [ ] **Step 3: Write failing tests for the listing parser**

Create `cli/src/scanner.rs` containing only a test module for now:

```rust
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
}
```

Add `pub mod scanner;` to `cli/src/lib.rs`.

- [ ] **Step 4: Run tests to verify they fail**

Run: `cargo test -p jito-bam-boost-cli scanner`
Expected: compile error — `parse_epoch_from_object_name` not found.

- [ ] **Step 5: Implement model and parser**

Prepend to `cli/src/scanner.rs`:

```rust
use serde::{Deserialize, Serialize};

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
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test -p jito-bam-boost-cli scanner`
Expected: 2 passed.

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml Cargo.lock cli/Cargo.toml cli/src/scanner.rs cli/src/lib.rs
git commit -m "feat: add scanner module with epoch listing parser"
```

---

### Task 2: Extract PDA derivation into `pda.rs`

**Files:**
- Create: `cli/src/pda.rs`
- Modify: `cli/src/lib.rs` (add `pub mod pda;`)
- Modify: `cli/src/bam_boost_handler.rs` (replace private methods with calls to `pda::*`)

**Interfaces:**
- Produces: `pda::merkle_distributor_address(program_id: &Pubkey, mint: &Pubkey, epoch: u64) -> Pubkey`; `pda::claim_status_address(program_id: &Pubkey, claimant: &Pubkey, distributor: &Pubkey) -> Pubkey` (all `solana_pubkey::Pubkey`).
- Consumes: seeds copied verbatim from `bam_boost_handler.rs` private methods.

- [ ] **Step 1: Write failing oracle tests**

Create `cli/src/pda.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::JITOSOL_MINT;
    use solana_pubkey::Pubkey;
    use std::str::FromStr;

    fn program_id() -> Pubkey {
        Pubkey::from_str("BoostxbPp2ENYHGcTLYt1obpcY13HE4NojdqNWdzqSSb").unwrap()
    }

    #[test]
    fn distributor_pda_matches_seed_oracle() {
        let epoch = 1000u64;
        let expected = Pubkey::find_program_address(
            &[
                b"merkle_distributor",
                JITOSOL_MINT.to_bytes().as_slice(),
                epoch.to_le_bytes().as_slice(),
            ],
            &program_id(),
        )
        .0;
        assert_eq!(
            merkle_distributor_address(&program_id(), &JITOSOL_MINT, epoch),
            expected
        );
    }

    #[test]
    fn claim_status_pda_matches_seed_oracle() {
        let claimant = Pubkey::new_unique();
        let distributor = merkle_distributor_address(&program_id(), &JITOSOL_MINT, 5);
        let expected = Pubkey::find_program_address(
            &[
                b"claim_status",
                claimant.to_bytes().as_slice(),
                distributor.to_bytes().as_slice(),
            ],
            &program_id(),
        )
        .0;
        assert_eq!(
            claim_status_address(&program_id(), &claimant, &distributor),
            expected
        );
    }
}
```

Add `pub mod pda;` to `cli/src/lib.rs`.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p jito-bam-boost-cli pda`
Expected: compile error — functions not found.

- [ ] **Step 3: Implement, moving logic from the handler**

Prepend to `cli/src/pda.rs`:

```rust
use solana_pubkey::Pubkey;

/// PDA of the MerkleDistributor for a given mint and epoch.
pub fn merkle_distributor_address(program_id: &Pubkey, mint: &Pubkey, epoch: u64) -> Pubkey {
    Pubkey::find_program_address(
        &[
            b"merkle_distributor",
            mint.to_bytes().as_slice(),
            epoch.to_le_bytes().as_slice(),
        ],
        program_id,
    )
    .0
}

/// PDA of a claimant's ClaimStatus for a given distributor.
pub fn claim_status_address(program_id: &Pubkey, claimant: &Pubkey, distributor: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[
            b"claim_status",
            claimant.to_bytes().as_slice(),
            distributor.to_bytes().as_slice(),
        ],
        program_id,
    )
    .0
}
```

In `cli/src/bam_boost_handler.rs`: delete the private `merkle_distributor_address` and `claim_status_address` methods; replace call sites with `crate::pda::merkle_distributor_address(&self.bam_boost_program_id, &JITOSOL_MINT, epoch)` and `crate::pda::claim_status_address(&self.bam_boost_program_id, &claimant, &distributor_pda)` (in `claim()` the claimant is `signer.pubkey()`).

- [ ] **Step 4: Run all tests + clippy**

Run: `cargo test -p jito-bam-boost-cli && cargo clippy -p jito-bam-boost-cli --all-targets`
Expected: all pass, no warnings.

- [ ] **Step 5: Commit**

```bash
git add cli/src/pda.rs cli/src/lib.rs cli/src/bam_boost_handler.rs
git commit -m "refactor: extract PDA derivation into pda module"
```

---

### Task 3: `Scanner` — epoch listing over HTTP (httpmock)

**Files:**
- Modify: `cli/src/scanner.rs`

**Interfaces:**
- Produces: `Scanner::new(cache_dir: PathBuf) -> Scanner` (uses `DEFAULT_GCS_BASE`); `Scanner::with_base_url(base_url: String, cache_dir: PathBuf) -> Scanner`; `Scanner::list_epochs(&self, network: &str) -> anyhow::Result<Vec<u64>>` (sorted ascending); `scanner::DEFAULT_GCS_BASE: &str = "https://storage.googleapis.com"`.

- [ ] **Step 1: Write failing test with httpmock pagination**

Append to the test module in `cli/src/scanner.rs`:

```rust
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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p jito-bam-boost-cli lists_epochs`
Expected: compile error — `Scanner` not found.

- [ ] **Step 3: Implement `Scanner` and `list_epochs`**

Add to `cli/src/scanner.rs`:

```rust
use std::path::PathBuf;

pub const DEFAULT_GCS_BASE: &str = "https://storage.googleapis.com";

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
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p jito-bam-boost-cli lists_epochs`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add cli/src/scanner.rs
git commit -m "feat: scanner lists published epochs from GCS bucket"
```

---

### Task 4: `Scanner::fetch_entries` — tree download with cache, 404, retries

**Files:**
- Modify: `cli/src/scanner.rs`

**Interfaces:**
- Produces: `Scanner::fetch_entries(&self, network: &str, epoch: u64) -> anyhow::Result<Option<Vec<BamBoostEntry>>>` — `None` on 404; caches raw JSON at `{cache_dir}/{network}/{epoch}.json`.
- Consumes: `jito_bam_boost_merkle_tree::bam_boost_entry::BamBoostEntry` (fields `pubkey: String`, `amount: u64`).

- [ ] **Step 1: Write failing tests (cache hit, 404, transient retry)**

Append to the test module:

```rust
    fn entries_json() -> serde_json::Value {
        serde_json::json!([
            {"pubkey": "J1toso1uCk3RLmjorhTtrVwY9HJ7X8V9yYac6Y7kGCPn", "amount": 1234}
        ])
    }

    #[tokio::test]
    async fn fetches_entries_and_caches_them() {
        let server = httpmock::MockServer::start();
        let mock = server.mock(|when, then| {
            when.method("GET").path("/jito-bam-boost/mainnet/7/merkle_tree.json");
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
            when.method("GET").path("/jito-bam-boost/mainnet/8/merkle_tree.json");
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
            when.method("GET").path("/jito-bam-boost/mainnet/9/merkle_tree.json");
            then.status(500);
        });
        let tmp = tempfile::tempdir().unwrap();
        let scanner = Scanner::with_base_url(server.base_url(), tmp.path().to_path_buf());
        let err = scanner.fetch_entries("mainnet", 9).await;
        assert!(err.is_err());
        failing.assert_hits(3); // 3 attempts total
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p jito-bam-boost-cli fetch`
Expected: compile error — `fetch_entries` not found.

- [ ] **Step 3: Implement with retry helper**

Add to `cli/src/scanner.rs` (imports: `use jito_bam_boost_merkle_tree::bam_boost_entry::BamBoostEntry; use std::time::Duration;`):

```rust
const RETRY_ATTEMPTS: u32 = 3;
const RETRY_BASE_DELAY_MS: u64 = 300;

impl Scanner {
    /// Downloads (or reads from cache) the entry list for an epoch.
    /// Returns `None` when no distribution exists for that epoch (HTTP 404).
    pub async fn fetch_entries(
        &self,
        network: &str,
        epoch: u64,
    ) -> anyhow::Result<Option<Vec<BamBoostEntry>>> {
        let cache_path = self.cache_dir.join(network).join(format!("{epoch}.json"));
        if let Ok(raw) = std::fs::read_to_string(&cache_path) {
            return Ok(Some(serde_json::from_str(&raw)?));
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
                        std::fs::write(&cache_path, &raw)?;
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
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p jito-bam-boost-cli scanner`
Expected: all scanner tests pass (retry test takes ~1s due to backoff).

- [ ] **Step 5: Commit**

```bash
git add cli/src/scanner.rs
git commit -m "feat: scanner fetches merkle entries with cache and retries"
```

---

### Task 5: `Scanner::scan` — combine amounts + on-chain claim status

**Files:**
- Modify: `cli/src/scanner.rs`

**Interfaces:**
- Produces:
  - `scanner::combine(epochs: &[u64], amounts: &HashMap<u64, Option<u64>>, claimed: &HashMap<u64, bool>) -> Vec<EpochStatus>` (pure, tested);
  - `scanner::amount_for(entries: &[BamBoostEntry], claimant: &Pubkey) -> Option<u64>` (pure, tested);
  - `Scanner::scan(&self, network: &str, claimant: &Pubkey, rpc: &RpcClient, program_id: &Pubkey) -> anyhow::Result<Vec<EpochStatus>>` (thin orchestration: list → fetch concurrently (limit 8) → `get_multiple_accounts` in chunks of 100 → `combine`).
- Consumes: `pda::merkle_distributor_address`, `pda::claim_status_address`, `crate::JITOSOL_MINT`.

- [ ] **Step 1: Write failing tests for the pure parts**

```rust
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
        let amounts: HashMap<u64, Option<u64>> =
            [(1, Some(10)), (2, None), (3, Some(30))].into();
        let claimed: HashMap<u64, bool> = [(1, true), (3, false)].into();
        let out = combine(&epochs, &amounts, &claimed);
        assert_eq!(
            out,
            vec![
                EpochStatus { epoch: 1, amount: Some(10), claimed: true },
                EpochStatus { epoch: 2, amount: None, claimed: false },
                EpochStatus { epoch: 3, amount: Some(30), claimed: false },
            ]
        );
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p jito-bam-boost-cli combine`
Expected: compile error.

- [ ] **Step 3: Implement pure functions + orchestration**

Add to `cli/src/scanner.rs` (imports: `use std::collections::HashMap; use futures::stream::{self, StreamExt}; use solana_pubkey::Pubkey; use solana_rpc_client::rpc_client::RpcClient; use crate::{pda, JITOSOL_MINT};`):

```rust
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

        let amounts: HashMap<u64, Option<u64>> = stream::iter(epochs.clone())
            .map(|epoch| async move {
                let entries = self.fetch_entries(network, epoch).await?;
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
```

- [ ] **Step 4: Run tests + clippy**

Run: `cargo test -p jito-bam-boost-cli && cargo clippy -p jito-bam-boost-cli --all-targets`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add cli/src/scanner.rs
git commit -m "feat: scanner full scan combining amounts and claim status"
```

---

### Task 6: `status` CLI command

**Files:**
- Modify: `cli/src/bam_boost.rs` (new action)
- Modify: `cli/src/bam_boost_handler.rs` (handle it)

**Interfaces:**
- Consumes: `Scanner::scan`, `EpochStatus`.
- Produces: CLI `bam-boost merkle-distributor status --network mainnet --claimant <PUBKEY>`; helper `bam_boost_handler::format_jitosol(lamports: u64) -> String` (9 decimals, e.g. `1234 -> "0.000001234"`); `bam_boost_handler::default_cache_dir() -> PathBuf` (`dirs::cache_dir()/jito-bam-boost`, falls back to `.jito-bam-boost-cache`).

- [ ] **Step 1: Write failing test for `format_jitosol`**

In `cli/src/bam_boost_handler.rs` add a test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_jitosol_amounts() {
        assert_eq!(format_jitosol(0), "0.000000000");
        assert_eq!(format_jitosol(1_234), "0.000001234");
        assert_eq!(format_jitosol(1_500_000_000), "1.500000000");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p jito-bam-boost-cli formats_jitosol`
Expected: compile error.

- [ ] **Step 3: Implement formatting, CLI variant, and handler**

In `cli/src/bam_boost.rs` extend `MerkleDistributorActions`:

```rust
    /// Show subsidy status for every published epoch
    Status {
        /// Network type (mainnet or testnet)
        #[arg(long, value_enum)]
        network: NetworkArg,

        /// Claimant pubkey (validator identity); no keypair needed
        #[arg(long)]
        claimant: Pubkey,
    },
```

In `cli/src/bam_boost_handler.rs`:

```rust
pub fn format_jitosol(lamports: u64) -> String {
    let base = 10u64.pow(jito_bam_boost_merkle_tree::tree_node::MINT_DECIMALS);
    format!("{}.{:09}", lamports / base, lamports % base)
}

pub fn default_cache_dir() -> std::path::PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("jito-bam-boost")
}
```

Add match arm in `handle()`:

```rust
            BamBoostCommands::MerkleDistributor {
                action: MerkleDistributorActions::Status { network, claimant },
            } => {
                let network = match network {
                    NetworkArg::Mainnet => "mainnet",
                    NetworkArg::Testnet => "testnet",
                };
                self.status(network, claimant).await
            }
```

And the method:

```rust
    async fn status(&self, network: &str, claimant: Pubkey) -> anyhow::Result<()> {
        let scanner = crate::scanner::Scanner::new(default_cache_dir());
        let rpc_client = self.get_rpc_client();
        let statuses = scanner
            .scan(network, &claimant, &rpc_client, &self.bam_boost_program_id)
            .await?;

        if self.print_json {
            println!("{}", serde_json::to_string_pretty(&statuses)?);
            return Ok(());
        }

        println!("{:>8}  {:>18}  {}", "Epoch", "Amount (JitoSOL)", "Status");
        let mut unclaimed_total = 0u64;
        let mut unclaimed_count = 0u64;
        for s in &statuses {
            let (amount, state) = match (s.amount, s.claimed) {
                (Some(a), true) => (format_jitosol(a), "claimed"),
                (Some(a), false) => {
                    unclaimed_total = unclaimed_total.saturating_add(a);
                    unclaimed_count += 1;
                    (format_jitosol(a), "unclaimed")
                }
                (None, _) => ("-".to_string(), "not eligible"),
            };
            println!("{:>8}  {:>18}  {}", s.epoch, amount, state);
        }
        println!(
            "\nUnclaimed: {unclaimed_count} epoch(s), {} JitoSOL",
            format_jitosol(unclaimed_total)
        );
        Ok(())
    }
```

- [ ] **Step 4: Run tests, then a live smoke test**

Run: `cargo test -p jito-bam-boost-cli`
Expected: PASS.

Run (read-only, safe):

```bash
cargo r -p jito-bam-boost-cli -- --rpc-url https://api.mainnet-beta.solana.com --commitment confirmed \
  bam-boost merkle-distributor status --network mainnet \
  --claimant J1toso1uCk3RLmjorhTtrVwY9HJ7X8V9yYac6Y7kGCPn
```

Expected: table of epochs, all "not eligible" (mint address is not a claimant) — proves listing + fetching + RPC path works end-to-end.

- [ ] **Step 5: Commit**

```bash
git add cli/src/bam_boost.rs cli/src/bam_boost_handler.rs
git commit -m "feat: add status command with epoch dashboard"
```

---

### Task 7: `batch_claim` — instruction builder + sequential claimer

**Files:**
- Create: `cli/src/batch_claim.rs`
- Modify: `cli/src/lib.rs` (add `pub mod batch_claim;`)
- Modify: `cli/src/bam_boost_handler.rs` (rewire `claim()` to use the builder)

**Interfaces:**
- Produces:
  - `batch_claim::build_claim_ixs(program_id: &Pubkey, claimant: &Pubkey, epoch: u64, amount: u64, proof: Vec<[u8; 32]>) -> Vec<Instruction>` — `[create_ata_idempotent, claim]`, exactly the instructions the existing `claim()` sends;
  - `batch_claim::ClaimEvent { pub epoch: u64, pub state: ClaimState }`, `enum ClaimState { Started, Success(String), Failed(String), Skipped(String) }`;
  - `batch_claim::claim_epochs(scanner: &Scanner, cli_config: &CliConfig, program_id: &Pubkey, network: &str, epochs: &[u64], progress: &mut dyn FnMut(ClaimEvent)) -> anyhow::Result<Vec<ClaimEvent>>` — sequential; failure of one epoch continues; returns final per-epoch events.
- Consumes: `pda::*`, `Scanner::fetch_entries`, `BamBoostMerkleTree::new_from_entries`, `CliConfig` (needs `signer`), `ClaimBuilder`.

- [ ] **Step 1: Write failing test for `build_claim_ixs`**

Create `cli/src/batch_claim.rs` with test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use solana_pubkey::Pubkey;
    use std::str::FromStr;

    #[test]
    fn builds_ata_then_claim_instruction() {
        let program_id =
            Pubkey::from_str("BoostxbPp2ENYHGcTLYt1obpcY13HE4NojdqNWdzqSSb").unwrap();
        let claimant = Pubkey::new_unique();
        let proof = vec![[7u8; 32]];

        let ixs = build_claim_ixs(&program_id, &claimant, 42, 1000, proof);

        assert_eq!(ixs.len(), 2);
        // First: ATA creation for the claimant's JitoSOL account.
        assert_eq!(
            ixs[0].program_id.to_string(),
            "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL"
        );
        // Second: claim instruction owned by the BAM Boost program.
        assert_eq!(ixs[1].program_id, program_id);
        // Claimant must be a signer of the claim instruction.
        assert!(ixs[1]
            .accounts
            .iter()
            .any(|m| m.pubkey == claimant && m.is_signer));
    }
}
```

Add `pub mod batch_claim;` to `cli/src/lib.rs`.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p jito-bam-boost-cli builds_ata`
Expected: compile error.

- [ ] **Step 3: Implement builder by moving logic out of `claim()`**

`cli/src/batch_claim.rs`:

```rust
use jito_bam_boost_client::instructions::ClaimBuilder;
use solana_pubkey::Pubkey;
use solana_transaction::Instruction;
use spl_associated_token_account_interface::{
    address::get_associated_token_address_with_program_id,
    instruction::create_associated_token_account_idempotent,
};

use crate::{pda, JITOSOL_MINT};

/// Builds the [create ATA, claim] instruction pair for one epoch.
pub fn build_claim_ixs(
    program_id: &Pubkey,
    claimant: &Pubkey,
    epoch: u64,
    amount: u64,
    proof: Vec<[u8; 32]>,
) -> Vec<Instruction> {
    let distributor_pda = pda::merkle_distributor_address(program_id, &JITOSOL_MINT, epoch);
    let distributor_token_address = get_associated_token_address_with_program_id(
        &distributor_pda,
        &JITOSOL_MINT,
        &spl_token_interface::id(),
    );
    let claim_status_pda = pda::claim_status_address(program_id, claimant, &distributor_pda);
    let claimant_token_address = get_associated_token_address_with_program_id(
        claimant,
        &JITOSOL_MINT,
        &spl_token_interface::id(),
    );

    let mut ix_builder = ClaimBuilder::new();
    ix_builder
        .distributor(distributor_pda)
        .claim_status(claim_status_pda)
        .from(distributor_token_address)
        .to(claimant_token_address)
        .claimant(*claimant)
        .token_program(spl_token_interface::id())
        .amount(amount)
        .proof(proof);
    let mut claim_ix = ix_builder.instruction();
    claim_ix.program_id = *program_id;

    vec![
        create_associated_token_account_idempotent(
            claimant,
            claimant,
            &JITOSOL_MINT,
            &spl_token_interface::id(),
        ),
        claim_ix,
    ]
}
```

NOTE: the existing `claim()` wraps some pubkeys via `Pubkey::new_from_array(...)` because of mixed crate versions. If the compiler complains about mismatched `Pubkey` types anywhere above, apply the same `Pubkey::new_from_array(x.to_bytes())` conversion the current handler uses at that exact spot — do not change dependency versions.

In `cli/src/bam_boost_handler.rs`, replace the instruction-building section of `claim()` (from `let distributor_pda = ...` through construction of `ix`) with:

```rust
        let node = merkle_tree.get_node(&signer.pubkey());
        let proof = node
            .proof
            .clone()
            .ok_or_else(|| anyhow!("merkle proof missing for claimant"))?;
        let ixs = crate::batch_claim::build_claim_ixs(
            &self.bam_boost_program_id,
            &signer.pubkey(),
            epoch,
            node.amount,
            proof,
        );
```

Keep the existing ClaimStatus pre-flight check before it (recompute `claim_status_pda` via `pda::` functions) and pass `&ixs` to `process_transaction`.

- [ ] **Step 4: Run all tests + verify claim path compiles identically**

Run: `cargo test -p jito-bam-boost-cli && cargo clippy -p jito-bam-boost-cli --all-targets`
Expected: PASS, no warnings.

Verify tx building is unchanged with the print-only path (no funds, no keys — use any throwaway keypair):

```bash
solana-keygen new --no-bip39-passphrase -s -o /tmp/throwaway.json
cargo r -p jito-bam-boost-cli -- --rpc-url https://api.mainnet-beta.solana.com --commitment confirmed \
  --signer /tmp/throwaway.json --print-tx \
  bam-boost merkle-distributor claim --network mainnet --epoch 1000 || true
```

Expected: fails at "Claimant not found in tree" panic or prints a Base58 tx — either proves the wiring; must NOT fail with a type/compile error.

- [ ] **Step 5: Commit**

```bash
git add cli/src/batch_claim.rs cli/src/lib.rs cli/src/bam_boost_handler.rs
git commit -m "refactor: extract claim instruction building into batch_claim"
```

- [ ] **Step 6: Implement `claim_epochs` (sequential batch)**

Append to `cli/src/batch_claim.rs` (imports: `use std::sync::Arc; use jito_bam_boost_merkle_tree::bam_boost_merkle_tree::BamBoostMerkleTree; use solana_keypair::Signer as _; use solana_rpc_client::rpc_client::RpcClient; use solana_transaction::Transaction; use crate::{cli_config::CliConfig, scanner::Scanner};`):

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimState {
    Started,
    Success(String),
    Failed(String),
    Skipped(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimEvent {
    pub epoch: u64,
    pub state: ClaimState,
}

/// Claims each epoch sequentially; one failure does not stop the rest.
pub async fn claim_epochs(
    scanner: &Scanner,
    cli_config: &CliConfig,
    program_id: &Pubkey,
    network: &str,
    epochs: &[u64],
    progress: &mut dyn FnMut(ClaimEvent),
) -> anyhow::Result<Vec<ClaimEvent>> {
    let signer = cli_config
        .signer
        .clone()
        .ok_or_else(|| anyhow::anyhow!("signer is required"))?;
    let rpc = RpcClient::new_with_commitment(cli_config.rpc_url.clone(), cli_config.commitment);
    let mut results = Vec::with_capacity(epochs.len());

    for &epoch in epochs {
        progress(ClaimEvent { epoch, state: ClaimState::Started });
        let state = claim_one(scanner, &rpc, &signer, program_id, network, epoch).await;
        let event = ClaimEvent { epoch, state };
        progress(event.clone());
        results.push(event);
    }
    Ok(results)
}

async fn claim_one(
    scanner: &Scanner,
    rpc: &RpcClient,
    signer: &Arc<solana_keypair::Keypair>,
    program_id: &Pubkey,
    network: &str,
    epoch: u64,
) -> ClaimState {
    let entries = match scanner.fetch_entries(network, epoch).await {
        Ok(Some(entries)) => entries,
        Ok(None) => return ClaimState::Skipped("no distribution for epoch".into()),
        Err(e) => return ClaimState::Failed(format!("fetch merkle tree: {e}")),
    };

    let tree = match BamBoostMerkleTree::new_from_entries(entries) {
        Ok(tree) => tree,
        Err(e) => return ClaimState::Failed(format!("build merkle tree: {e}")),
    };

    let claimant = signer.pubkey();
    let Some(node) = tree
        .tree_nodes
        .iter()
        .find(|n| n.claimant.to_bytes() == claimant.to_bytes())
        .cloned()
    else {
        return ClaimState::Skipped("claimant not in tree".into());
    };
    let Some(proof) = node.proof.clone() else {
        return ClaimState::Failed("merkle proof missing".into());
    };

    let distributor = pda::merkle_distributor_address(program_id, &JITOSOL_MINT, epoch);
    let claim_status = pda::claim_status_address(program_id, &claimant, &distributor);
    match rpc.get_account(&claim_status) {
        Ok(_) => return ClaimState::Skipped("already claimed".into()),
        Err(_) => {} // account absent — proceed
    }

    let ixs = build_claim_ixs(program_id, &claimant, epoch, node.amount, proof);
    let blockhash = match rpc.get_latest_blockhash() {
        Ok(b) => b,
        Err(e) => return ClaimState::Failed(format!("blockhash: {e}")),
    };
    let tx = Transaction::new_signed_with_payer(&ixs, Some(&claimant), &[signer.clone()], blockhash);
    match rpc.send_and_confirm_transaction(&tx) {
        Ok(sig) => ClaimState::Success(sig.to_string()),
        Err(e) => ClaimState::Failed(format!("send: {e}")),
    }
}
```

(`node.claimant` is the merkle-tree crate's `Pubkey`; comparing via `.to_bytes()` sidesteps the mixed crate versions.)

- [ ] **Step 7: Build + clippy, then commit**

Run: `cargo clippy -p jito-bam-boost-cli --all-targets`
Expected: clean.

```bash
git add cli/src/batch_claim.rs
git commit -m "feat: sequential batch claim over multiple epochs"
```

---

### Task 8: `claim-all` CLI command

**Files:**
- Modify: `cli/src/bam_boost.rs`
- Modify: `cli/src/bam_boost_handler.rs`

**Interfaces:**
- Produces: CLI `bam-boost merkle-distributor claim-all --network mainnet [--yes]` — scans, prints unclaimed table, asks `Claim N epochs totalling X JitoSOL? [y/N]` on stdin unless `--yes`, then runs `claim_epochs` printing progress.
- Consumes: `Scanner::scan`, `claim_epochs`, `format_jitosol`, `default_cache_dir`.

- [ ] **Step 1: Add the CLI variant**

In `cli/src/bam_boost.rs` extend `MerkleDistributorActions`:

```rust
    /// Claim every unclaimed epoch for the signer
    ClaimAll {
        /// Network type (mainnet or testnet)
        #[arg(long, value_enum)]
        network: NetworkArg,

        /// Skip the interactive confirmation
        #[arg(long, default_value = "false")]
        yes: bool,
    },
```

- [ ] **Step 2: Implement the handler**

Match arm in `handle()`:

```rust
            BamBoostCommands::MerkleDistributor {
                action: MerkleDistributorActions::ClaimAll { network, yes },
            } => {
                let network = match network {
                    NetworkArg::Mainnet => "mainnet",
                    NetworkArg::Testnet => "testnet",
                };
                self.claim_all(network, yes).await
            }
```

Method:

```rust
    async fn claim_all(&self, network: &str, yes: bool) -> anyhow::Result<()> {
        let signer = self
            .cli_config
            .signer
            .clone()
            .ok_or_else(|| anyhow::anyhow!("signer is required"))?;
        let claimant = signer.pubkey();

        let scanner = crate::scanner::Scanner::new(default_cache_dir());
        let rpc_client = self.get_rpc_client();
        let statuses = scanner
            .scan(network, &claimant, &rpc_client, &self.bam_boost_program_id)
            .await?;

        let unclaimed: Vec<_> = statuses
            .iter()
            .filter(|s| s.amount.is_some() && !s.claimed)
            .collect();
        if unclaimed.is_empty() {
            println!("Nothing to claim: no unclaimed epochs for {claimant}");
            return Ok(());
        }

        let total: u64 = unclaimed.iter().filter_map(|s| s.amount).sum();
        println!("Unclaimed epochs for {claimant}:");
        for s in &unclaimed {
            println!("  epoch {:>6}: {} JitoSOL", s.epoch, format_jitosol(s.amount.unwrap_or(0)));
        }
        println!("Total: {} JitoSOL across {} epoch(s)", format_jitosol(total), unclaimed.len());

        if !yes {
            print!("Proceed with claiming? [y/N] ");
            use std::io::Write as _;
            std::io::stdout().flush()?;
            let mut answer = String::new();
            std::io::stdin().read_line(&mut answer)?;
            if !matches!(answer.trim(), "y" | "Y" | "yes") {
                println!("Aborted.");
                return Ok(());
            }
        }

        let epochs: Vec<u64> = unclaimed.iter().map(|s| s.epoch).collect();
        let results = crate::batch_claim::claim_epochs(
            &scanner,
            &self.cli_config,
            &self.bam_boost_program_id,
            network,
            &epochs,
            &mut |event| match &event.state {
                crate::batch_claim::ClaimState::Started => {
                    println!("epoch {}: claiming...", event.epoch)
                }
                crate::batch_claim::ClaimState::Success(sig) => {
                    println!("epoch {}: OK  {sig}", event.epoch)
                }
                crate::batch_claim::ClaimState::Failed(e) => {
                    println!("epoch {}: FAILED  {e}", event.epoch)
                }
                crate::batch_claim::ClaimState::Skipped(r) => {
                    println!("epoch {}: skipped  {r}", event.epoch)
                }
            },
        )
        .await?;

        let ok = results.iter().filter(|r| matches!(r.state, crate::batch_claim::ClaimState::Success(_))).count();
        let failed = results.iter().filter(|r| matches!(r.state, crate::batch_claim::ClaimState::Failed(_))).count();
        println!("\nDone: {ok} claimed, {failed} failed, {} other", results.len() - ok - failed);
        Ok(())
    }
```

- [ ] **Step 3: Build, clippy, help-text check**

Run: `cargo clippy -p jito-bam-boost-cli --all-targets && cargo r -p jito-bam-boost-cli -- bam-boost merkle-distributor claim-all --help`
Expected: clean build; help shows `--network`, `--yes`.

- [ ] **Step 4: Commit**

```bash
git add cli/src/bam_boost.rs cli/src/bam_boost_handler.rs
git commit -m "feat: add claim-all command with confirmation and progress"
```

---

### Task 9: TUI state machine (`tui/app.rs`)

**Files:**
- Create: `cli/src/tui/mod.rs` (module decl only for now: `pub mod app;`)
- Create: `cli/src/tui/app.rs`
- Modify: `cli/src/lib.rs` (add `pub mod tui;`)

**Interfaces:**
- Produces (all in `tui::app`):

```rust
pub enum Screen { Setup, Dashboard, Confirm, Progress }
pub enum SetupField { Network, RpcUrl, Claimant, Start }        // Tab order
pub enum AppEvent {
    Key(ratatui::crossterm::event::KeyEvent),
    ScanFinished(Result<Vec<EpochStatus>, String>),
    Claim(ClaimEvent),
    ClaimRunFinished,
}
pub enum Action { StartScan, StartClaim { epochs: Vec<u64>, keypair_path: String }, Quit }
pub struct App { /* fields below */ }
impl App {
    pub fn new() -> Self;                                        // Setup screen, mainnet, default RPC
    pub fn handle(&mut self, event: AppEvent) -> Option<Action>; // pure reducer
}
```

- `App` public fields (ui.rs reads them): `screen: Screen`, `network: String` ("mainnet"/"testnet"), `rpc_url: String`, `claimant_input: String`, `keypair_input: String`, `setup_focus: SetupField`, `setup_error: Option<String>`, `statuses: Vec<EpochStatus>`, `cursor: usize`, `selected: std::collections::HashSet<u64>`, `scanning: bool`, `progress_rows: Vec<ClaimEvent>`, `claim_done: bool`.
- Consumes: `scanner::EpochStatus`, `batch_claim::{ClaimEvent, ClaimState}`.

- [ ] **Step 1: Write failing reducer tests**

`cli/src/tui/app.rs` test module (helper builds `KeyEvent`s):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> AppEvent {
        AppEvent::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn scanned_app() -> App {
        let mut app = App::new();
        app.claimant_input = "J1toso1uCk3RLmjorhTtrVwY9HJ7X8V9yYac6Y7kGCPn".into();
        app.setup_focus = SetupField::Start;
        assert!(matches!(app.handle(key(KeyCode::Enter)), Some(Action::StartScan)));
        app.handle(AppEvent::ScanFinished(Ok(vec![
            EpochStatus { epoch: 1, amount: Some(10), claimed: false },
            EpochStatus { epoch: 2, amount: Some(20), claimed: true },
            EpochStatus { epoch: 3, amount: None, claimed: false },
        ])));
        app
    }

    #[test]
    fn setup_enter_on_start_triggers_scan_and_moves_to_dashboard() {
        let app = scanned_app();
        assert!(matches!(app.screen, Screen::Dashboard));
        assert_eq!(app.statuses.len(), 3);
        assert!(!app.scanning);
    }

    #[test]
    fn setup_rejects_invalid_claimant() {
        let mut app = App::new();
        app.claimant_input = "not-a-pubkey".into();
        app.setup_focus = SetupField::Start;
        assert!(app.handle(key(KeyCode::Enter)).is_none());
        assert!(app.setup_error.is_some());
        assert!(matches!(app.screen, Screen::Setup));
    }

    #[test]
    fn select_all_unclaimed_picks_only_eligible_unclaimed() {
        let mut app = scanned_app();
        app.handle(key(KeyCode::Char('a')));
        assert_eq!(app.selected, std::collections::HashSet::from([1]));
    }

    #[test]
    fn claim_flow_confirm_then_progress() {
        let mut app = scanned_app();
        app.handle(key(KeyCode::Char('a')));
        app.handle(key(KeyCode::Char('c')));
        assert!(matches!(app.screen, Screen::Confirm));
        app.keypair_input = "/tmp/id.json".into();
        let action = app.handle(key(KeyCode::Char('y')));
        match action {
            Some(Action::StartClaim { epochs, keypair_path }) => {
                assert_eq!(epochs, vec![1]);
                assert_eq!(keypair_path, "/tmp/id.json");
            }
            other => panic!("expected StartClaim, got {other:?}"),
        }
        assert!(matches!(app.screen, Screen::Progress));
        app.handle(AppEvent::Claim(ClaimEvent { epoch: 1, state: ClaimState::Started }));
        app.handle(AppEvent::Claim(ClaimEvent { epoch: 1, state: ClaimState::Success("sig".into()) }));
        app.handle(AppEvent::ClaimRunFinished);
        assert!(app.claim_done);
        assert_eq!(app.progress_rows.last().unwrap().state, ClaimState::Success("sig".into()));
    }

    #[test]
    fn q_quits_from_dashboard() {
        let mut app = scanned_app();
        assert!(matches!(app.handle(key(KeyCode::Char('q'))), Some(Action::Quit)));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p jito-bam-boost-cli tui`
Expected: compile error.

- [ ] **Step 3: Implement the reducer**

Implement `App` in `cli/src/tui/app.rs`. Key behaviors (derive `Debug` on `Action`, `Screen`, `SetupField`):

```rust
use std::collections::HashSet;

use ratatui::crossterm::event::{KeyCode, KeyEvent};
use solana_pubkey::Pubkey;

use crate::batch_claim::{ClaimEvent, ClaimState};
use crate::scanner::EpochStatus;

pub const DEFAULT_MAINNET_RPC: &str = "https://api.mainnet-beta.solana.com";
pub const DEFAULT_TESTNET_RPC: &str = "https://api.testnet.solana.com";
```

Reducer rules (implement exactly; each is a small match arm in `handle`):

- `Screen::Setup`:
  - `Tab`/`Down` → next `setup_focus`; `BackTab`/`Up` → previous.
  - Focus `Network` + `Left`/`Right`/`Space` → toggle `network` between "mainnet"/"testnet", and if `rpc_url` is one of the two defaults, swap it to the new network's default.
  - Focus `RpcUrl`/`Claimant` + `Char(c)` → push char to that field's String; `Backspace` → pop.
  - Focus `Start` + `Enter`: validate `claimant_input.parse::<Pubkey>()` — on error set `setup_error`, stay; on success clear error, set `scanning = true`, `screen = Dashboard`, return `Some(Action::StartScan)`. (A keypair file path is also accepted later at claim time; Setup takes a pubkey.)
  - `Esc` → `Some(Action::Quit)`.
- `AppEvent::ScanFinished(Ok(statuses))` → `scanning = false; statuses = ...; cursor = 0; selected.clear()`.
- `AppEvent::ScanFinished(Err(msg))` → `scanning = false; setup_error = Some(msg); screen = Setup`.
- `Screen::Dashboard`:
  - `Up`/`Down` → move `cursor` clamped to `statuses.len().saturating_sub(1)`.
  - `Space` → toggle the epoch under cursor in `selected` only if it is claimable (`amount.is_some() && !claimed`).
  - `Char('a')` → `selected` = all claimable epochs.
  - `Char('c')` → if `selected` non-empty: `screen = Confirm`.
  - `Char('r')` → `scanning = true`, return `Some(Action::StartScan)`.
  - `Char('q')`/`Esc` → `Some(Action::Quit)`.
- `Screen::Confirm`:
  - `Char(c)`/`Backspace` edit `keypair_input` (typed keypair path), except `y`/`n` when `keypair_input` is non-empty are handled first as answers — to keep tests simple: `Char('y')` confirms (requires non-empty `keypair_input`, else sets `setup_error`), `Char('n')`/`Esc` returns to Dashboard, and path editing uses any other chars plus `Backspace`.
  - On confirm: `screen = Progress`, `progress_rows.clear()`, `claim_done = false`, return `Some(Action::StartClaim { epochs: sorted selected, keypair_path: keypair_input.clone() })`.
- `Screen::Progress`:
  - `AppEvent::Claim(ev)` → if last row has same epoch and `Started` state, replace it; else push.
  - `AppEvent::ClaimRunFinished` → `claim_done = true`.
  - `Char('q')`/`Esc` (only when `claim_done`) → `Some(Action::Quit)`; `Char('b')` when done → back to Dashboard with `scanning = true` + `Some(Action::StartScan)` (refresh statuses).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p jito-bam-boost-cli tui`
Expected: all reducer tests pass.

- [ ] **Step 5: Commit**

```bash
git add cli/src/tui/ cli/src/lib.rs
git commit -m "feat: TUI state machine with setup/dashboard/confirm/progress"
```

---

### Task 10: TUI rendering (`tui/ui.rs`) + snapshot test

**Files:**
- Create: `cli/src/tui/ui.rs`
- Modify: `cli/src/tui/mod.rs` (add `pub mod ui;`)

**Interfaces:**
- Produces: `tui::ui::draw(frame: &mut ratatui::Frame, app: &App)` — renders the current screen.
- Consumes: `App` public fields, `format_jitosol` from `bam_boost_handler`.

- [ ] **Step 1: Write failing TestBackend test**

In `cli/src/tui/ui.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanner::EpochStatus;
    use crate::tui::app::App;
    use ratatui::{backend::TestBackend, Terminal};

    fn buffer_text(terminal: &Terminal<TestBackend>) -> String {
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    #[test]
    fn dashboard_shows_epochs_and_unclaimed_summary() {
        let mut app = App::new();
        app.screen = crate::tui::app::Screen::Dashboard;
        app.claimant_input = "J1toso1uCk3RLmjorhTtrVwY9HJ7X8V9yYac6Y7kGCPn".into();
        app.statuses = vec![
            EpochStatus { epoch: 1000, amount: Some(1_500_000_000), claimed: false },
            EpochStatus { epoch: 999, amount: Some(1_000_000_000), claimed: true },
            EpochStatus { epoch: 998, amount: None, claimed: false },
        ];

        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        let text = buffer_text(&terminal);

        assert!(text.contains("1000"));
        assert!(text.contains("unclaimed"));
        assert!(text.contains("claimed"));
        assert!(text.contains("not eligible"));
        assert!(text.contains("1.500000000")); // formatted JitoSOL
    }

    #[test]
    fn setup_screen_shows_fields() {
        let app = App::new();
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("Network"));
        assert!(text.contains("RPC URL"));
        assert!(text.contains("Claimant"));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p jito-bam-boost-cli ui`
Expected: compile error — `draw` not found.

- [ ] **Step 3: Implement rendering**

Implement `draw` dispatching on `app.screen`; one private fn per screen. Layout requirements (use `ratatui::layout::{Layout, Constraint, Direction}`, `widgets::{Block, Borders, Paragraph, Row, Table, Clear}`, `style::{Style, Modifier, Color}`):

- **Setup**: centered vertical stack, one bordered `Paragraph` per field; focused field gets `Style::default().add_modifier(Modifier::BOLD)` border and title suffix `" ◀"`; `setup_error` rendered in red below; footer hint line `"Tab: next · Enter on [Start scan]: scan · Esc: quit"`.
- **Dashboard**: header `Paragraph` (claimant + network), `Table` with columns `Sel | Epoch | Amount (JitoSOL) | Status` — `Sel` is `"[x]"`/`"[ ]"`/`"   "` (non-claimable), amount via `crate::bam_boost_handler::format_jitosol`, status strings exactly `"unclaimed"`, `"claimed"`, `"not eligible"`; row under cursor highlighted with `Style::default().add_modifier(Modifier::REVERSED)`; footer with unclaimed count/total and key hints `"space select · a all · c claim · r rescan · q quit"`. When `app.scanning`, render `"Scanning..."` paragraph instead of the table.
- **Confirm**: dashboard rendered underneath, then `Clear` + centered bordered popup listing selected epochs (up to 10, then `"… and N more"`), total JitoSOL, keypair path input line, hint `"y confirm · n cancel"`.
- **Progress**: `Table` of `progress_rows`: epoch, state (`"…"` for Started, `"OK <sig>"`, `"FAIL <err>"`, `"skip <reason>"`); when `claim_done`, footer `"b back to dashboard · q quit"`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p jito-bam-boost-cli ui`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add cli/src/tui/
git commit -m "feat: TUI rendering for all four screens"
```

---

### Task 11: TUI event loop + `tui` command wiring

**Files:**
- Modify: `cli/src/tui/mod.rs` (event loop)
- Modify: `cli/src/cli_args.rs` (new top-level command)
- Modify: `cli/src/bin/main.rs` (dispatch + relaxed config requirements for tui)

**Interfaces:**
- Produces: `tui::run(network: Option<String>, rpc_url: String, commitment: CommitmentConfig, signer_path: Option<String>, program_id: Pubkey) -> anyhow::Result<()>`; CLI `jito-bam-boost-cli tui [--network mainnet]` (global `--rpc-url`, `--signer`, `--commitment` respected).
- Consumes: `App`, `ui::draw`, `Scanner`, `claim_epochs`, `read_keypair_file`.

- [ ] **Step 1: Implement the event loop in `tui/mod.rs`**

```rust
pub mod app;
pub mod ui;

use std::sync::Arc;

use ratatui::crossterm::event::{self, Event};
use solana_commitment_config::CommitmentConfig;
use solana_keypair::read_keypair_file;
use solana_pubkey::Pubkey;
use solana_rpc_client::rpc_client::RpcClient;
use tokio::sync::mpsc;

use crate::batch_claim::{claim_epochs, ClaimEvent, ClaimState};
use crate::bam_boost_handler::default_cache_dir;
use crate::cli_config::CliConfig;
use crate::scanner::Scanner;
use app::{Action, App, AppEvent};

pub async fn run(
    network: Option<String>,
    rpc_url: String,
    commitment: CommitmentConfig,
    signer_path: Option<String>,
    program_id: Pubkey,
) -> anyhow::Result<()> {
    let mut app = App::new();
    if let Some(network) = network {
        app.network = network;
    }
    app.rpc_url = rpc_url;
    if let Some(path) = &signer_path {
        // Pre-fill: signer keypair gives us both claimant and claim signing.
        let keypair = read_keypair_file(path)
            .map_err(|e| anyhow::anyhow!("Failed to read keypair: {e}"))?;
        app.claimant_input = solana_keypair::Signer::pubkey(&keypair).to_string();
        app.keypair_input = path.clone();
    }

    let (tx, mut rx) = mpsc::unbounded_channel::<AppEvent>();

    // Keyboard reader thread → AppEvent::Key
    let key_tx = tx.clone();
    std::thread::spawn(move || loop {
        match event::read() {
            Ok(Event::Key(key)) if key.kind == event::KeyEventKind::Press => {
                if key_tx.send(AppEvent::Key(key)).is_err() {
                    break;
                }
            }
            Ok(_) => {}
            Err(_) => break,
        }
    });

    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, &mut app, &tx, &mut rx, commitment, program_id).await;
    ratatui::restore();
    result
}

async fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    tx: &mpsc::UnboundedSender<AppEvent>,
    rx: &mut mpsc::UnboundedReceiver<AppEvent>,
    commitment: CommitmentConfig,
    program_id: Pubkey,
) -> anyhow::Result<()> {
    loop {
        terminal.draw(|frame| ui::draw(frame, app))?;
        let Some(event) = rx.recv().await else { return Ok(()) };
        match app.handle(event) {
            Some(Action::Quit) => return Ok(()),
            Some(Action::StartScan) => {
                let tx = tx.clone();
                let network = app.network.clone();
                let rpc_url = app.rpc_url.clone();
                let claimant: Pubkey = app.claimant_input.parse()?;
                tokio::spawn(async move {
                    let scanner = Scanner::new(default_cache_dir());
                    let rpc = RpcClient::new_with_commitment(rpc_url, commitment);
                    let result = scanner
                        .scan(&network, &claimant, &rpc, &program_id)
                        .await
                        .map_err(|e| e.to_string());
                    let _ = tx.send(AppEvent::ScanFinished(result));
                });
            }
            Some(Action::StartClaim { epochs, keypair_path }) => {
                let tx = tx.clone();
                let network = app.network.clone();
                let rpc_url = app.rpc_url.clone();
                tokio::spawn(async move {
                    let outcome = run_claims(
                        &network, rpc_url, commitment, &keypair_path, program_id, &epochs, &tx,
                    )
                    .await;
                    if let Err(e) = outcome {
                        let _ = tx.send(AppEvent::Claim(ClaimEvent {
                            epoch: 0,
                            state: ClaimState::Failed(e.to_string()),
                        }));
                    }
                    let _ = tx.send(AppEvent::ClaimRunFinished);
                });
            }
            None => {}
        }
    }
}

async fn run_claims(
    network: &str,
    rpc_url: String,
    commitment: CommitmentConfig,
    keypair_path: &str,
    program_id: Pubkey,
    epochs: &[u64],
    tx: &mpsc::UnboundedSender<AppEvent>,
) -> anyhow::Result<()> {
    let keypair = read_keypair_file(keypair_path)
        .map_err(|e| anyhow::anyhow!("Failed to read keypair: {e}"))?;
    let cli_config = CliConfig {
        rpc_url,
        commitment,
        signer: Some(Arc::new(keypair)),
    };
    let scanner = Scanner::new(default_cache_dir());
    let progress_tx = tx.clone();
    claim_epochs(
        &scanner,
        &cli_config,
        &program_id,
        network,
        epochs,
        &mut move |event| {
            let _ = progress_tx.send(AppEvent::Claim(event));
        },
    )
    .await?;
    Ok(())
}
```

- [ ] **Step 2: Wire the `tui` command**

In `cli/src/cli_args.rs` add to `ProgramCommand`:

```rust
    /// Launch the full-screen interactive interface
    Tui {
        /// Network type (mainnet or testnet)
        #[arg(long)]
        network: Option<String>,
    },
```

In `cli/src/bin/main.rs`, handle it BEFORE `get_cli_config` (tui must not require `--commitment`):

```rust
    if let Some(ProgramCommand::Tui { network }) = &args.command {
        let commitment = match &args.commitment {
            Some(c) => CommitmentConfig::from_str(c)?,
            None => CommitmentConfig::confirmed(),
        };
        let rpc_url = args
            .rpc_url
            .clone()
            .unwrap_or_else(|| "https://api.mainnet-beta.solana.com".to_string());
        return jito_bam_boost_cli::tui::run(
            network.clone(),
            rpc_url,
            commitment,
            args.signer.clone(),
            bam_boost_program_id,
        )
        .await;
    }
```

(Move the `bam_boost_program_id` resolution above this block. The existing `match args.command ...` then needs a `ProgramCommand::Tui { .. } => unreachable!()` arm or restructure with `if let`.)

Note: `env_logger` prints to stderr and corrupts the alternate screen — for the tui path, skip `env_logger::Builder::init()` (initialize the logger only for non-tui commands).

- [ ] **Step 3: Full check + manual smoke test**

Run: `cargo test -p jito-bam-boost-cli && cargo clippy -p jito-bam-boost-cli --all-targets && cargo fmt --check`
Expected: clean.

Manual smoke (interactive — run in a real terminal):

```bash
cargo r -p jito-bam-boost-cli -- tui
```

Walk through: Setup renders → enter any validator identity pubkey (e.g. from `solana validators` output) → scan populates dashboard → `a` selects → `c` opens confirm → `n` cancels → `q` quits, terminal restored cleanly (no raw-mode residue: run `echo ok`).

- [ ] **Step 4: Commit**

```bash
git add cli/src/tui/ cli/src/cli_args.rs cli/src/bin/main.rs
git commit -m "feat: wire full-screen tui command with async scan and claim"
```

---

### Task 12: Onboarding documentation

**Files:**
- Create: `docs/claiming.md`
- Modify: `README.md` (add short section linking to it)

**Interfaces:**
- Consumes: final CLI syntax from Tasks 6, 8, 11 — verify each command line against `--help` output before writing it into the doc.

- [ ] **Step 1: Write `docs/claiming.md`**

Structure (modeled on Monad's validator-onboarding doc; fill every section with the actual commands implemented above):

1. **Summary** — numbered overview: install/build, check status, claim (CLI or TUI), verify.
2. **Prerequisites** — Rust toolchain, validator identity keypair (WARNING callout: the signer is the validator identity; keep it on a secure host; status checks need only the pubkey).
3. **Check your subsidies** — `status` command example with sample table output.
4. **Claim: one epoch** — existing `claim` command (copy from README).
5. **Claim: everything at once** — `claim-all` example with sample confirmation output.
6. **TUI workflow** — `tui` command, keyboard reference table (`space/a/c/r/q/y/n/b`), description of the four screens.
7. **Verification** — `claim-status get` example; explain ClaimStatus PDA semantics ("account exists = claimed").
8. **Troubleshooting** — "Claim status account already exists" (already claimed), claimant not in tree (not eligible that epoch), RPC rate limits (use a private RPC), GCS 404 (no distribution that epoch).

- [ ] **Step 2: Add README section**

After the existing "How to Claim Subsidy" section add:

```markdown
## Status Dashboard, Batch Claim & TUI

Check all epochs at once, claim everything unclaimed, or use the interactive
full-screen interface. See [docs/claiming.md](./docs/claiming.md).

```bash
# Read-only dashboard (no keypair needed)
cargo r -p jito-bam-boost-cli -- --rpc-url <RPC_URL> --commitment confirmed \
    bam-boost merkle-distributor status --network mainnet --claimant <IDENTITY_PUBKEY>

# Claim every unclaimed epoch
cargo r -p jito-bam-boost-cli -- --rpc-url <RPC_URL> --commitment confirmed \
    --signer <PATH_TO_IDENTITY_KEYPAIR> \
    bam-boost merkle-distributor claim-all --network mainnet

# Interactive TUI
cargo r -p jito-bam-boost-cli -- tui
```
```

- [ ] **Step 3: Verify commands in docs against --help**

Run each `--help` for `status`, `claim-all`, `tui` and diff flags against the doc text.
Expected: flags match exactly.

- [ ] **Step 4: Commit**

```bash
git add docs/claiming.md README.md
git commit -m "docs: add claiming guide covering status, claim-all and tui"
```

---

## Final verification (after all tasks)

- [ ] `cargo test` (workspace) — all green.
- [ ] `cargo clippy --all-targets` — no warnings.
- [ ] `cargo fmt --check` — clean.
- [ ] Live read-only run of `status` against mainnet with a real BAM validator identity pubkey — table shows plausible amounts.
- [ ] `tui` manual walkthrough (Setup → Dashboard → Confirm → cancel → quit).
- [ ] Push branch: `git push -u origin feat/tui-batch-claim`.
