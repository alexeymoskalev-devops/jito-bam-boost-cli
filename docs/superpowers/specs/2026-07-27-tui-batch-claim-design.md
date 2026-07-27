# BAM Boost TUI & Batch Claim — Design

**Date:** 2026-07-27
**Status:** Approved
**Repo:** fork of `jito-foundation/jito-bam-boost-cli` (branch `feat/tui-batch-claim`)

## Goal

Give validators a convenient interface over the BAM Boost subsidy claim flow,
modeled after Monad's `staking-sdk-cli` validator onboarding UX:

1. **Epoch auto-discovery + batch claim** — scan which epochs have unclaimed
   subsidies for a claimant and claim them all with one confirmation.
2. **Status dashboard** — epoch → subsidy amount → claimed/unclaimed overview.
3. **Full-screen TUI** (ratatui) wrapping both, plus scriptable CLI commands.
4. Long-term: contribute pieces upstream as incremental PRs
   (`status` → `claim-all` → `tui`), plus an onboarding doc.

Out of scope (YAGNI): multi-validator management, keypair generation,
non-JitoSOL mints, Ledger signing.

## Background facts (verified)

- Merkle trees are public and immutable:
  `https://storage.googleapis.com/jito-bam-boost/{network}/{epoch}/merkle_tree.json`.
- The bucket is publicly listable via
  `https://storage.googleapis.com/storage/v1/b/jito-bam-boost/o?prefix={network}/`
  with pagination (`nextPageToken`) — epochs can be enumerated automatically.
- Claim state is the existence of a `ClaimStatus` PDA:
  `["claim_status", claimant, distributor_pda]`; distributor PDA:
  `["merkle_distributor", jitosol_mint, epoch_le_bytes]`.
- Therefore a read-only dashboard needs only a **pubkey** — no keypair. The
  keypair is loaded only at the claim step.

## Architecture

Extend the existing `cli` crate, reusing the generated client, merkle-tree
parser, and transaction logic:

```
cli/src/
├── bam_boost.rs            # (existing) + new command variants
├── bam_boost_handler.rs    # (existing) refactor: extract claim-ix building from claim()
├── scanner.rs              # NEW: epoch discovery, amounts, claim statuses
├── batch_claim.rs          # NEW: sequential batch claim with progress reporting
└── tui/                    # NEW: ratatui app
    ├── mod.rs              # terminal setup/teardown, event loop
    ├── app.rs              # pure state machine (event → state)
    ├── ui.rs               # screen rendering
    └── events.rs           # keyboard + async events from background tasks
```

New dependencies: `ratatui`, `crossterm`, `dirs` (cache path). Existing:
`tokio`, `reqwest`, `clap`, solana crates.

### Commands (upstream-style)

- `bam-boost merkle-distributor status --network <mainnet|testnet> --claimant <PUBKEY>`
  — read-only table (respects `--print-json`); no signer required.
- `bam-boost merkle-distributor claim-all --network <...>` — claims every
  unclaimed epoch for the signer; interactive confirmation, `--yes` to skip.
- `tui` — top-level command launching the full-screen interface. Optional
  flags (`--network`, `--signer`, `--rpc-url`) pre-fill or skip the Setup screen.

## Data layer (`scanner.rs`)

- `list_epochs(network) -> Vec<u64>` — GCS object listing with pagination;
  parses `{epoch}/merkle_tree.json` names.
- `fetch_amount(network, epoch, claimant) -> Option<u64>` — downloads the
  merkle tree, finds the claimant node. Trees cached at
  `~/.cache/jito-bam-boost/{network}/{epoch}.json` (immutable → cache forever).
- `check_claimed(rpc, claimant, epochs) -> HashMap<u64, bool>` — derives
  ClaimStatus PDAs, calls `getMultipleAccounts` in batches of 100.
- Tree downloads run concurrently (tokio, limit 8) with 3 retries and backoff.

Scan result — the shared model for TUI, `status`, and `claim-all`:

```rust
struct EpochStatus { epoch: u64, amount: Option<u64>, claimed: bool }
```

`amount: None` ⇒ claimant not in that epoch's tree ("not eligible").

## Batch claim (`batch_claim.rs`)

- Refactor the existing `claim()` so instruction-building is a separate
  function; batch claim reuses it per epoch.
- Sequential sends (validator identity keys — no tx spam), progress events via
  `mpsc` channel (consumed by TUI or printed by `claim-all`).
- One epoch's failure records an error and continues; final summary lists
  successes (with signatures) and failures.
- Keeps the existing pre-flight ClaimStatus-exists check and idempotent
  JitoSOL ATA creation.

## TUI screens

1. **Setup** — network select, RPC URL (network default pre-filled), claimant
   (pubkey or keypair path). Skipped when provided via flags.
2. **Dashboard** — table `Epoch | Amount (JitoSOL) | Status
   (claimed/unclaimed/not eligible)`, footer with unclaimed totals.
   Keys: `↑↓` scroll, `space` toggle select, `a` select all unclaimed,
   `c` claim selected, `r` rescan, `q` quit.
3. **Confirm modal** — selected epochs, total amount, signer pubkey; if only a
   pubkey was entered, prompts for keypair path here (key loaded as late as
   possible). `y/n`.
4. **Progress** — one row per epoch: pending / success (signature) / error
   (message); batch continues past individual failures.

State machine lives in `app.rs` as a pure reducer for testability; background
tasks (scan, claim) communicate via events.

## Error handling

- GCS 404 for an epoch ⇒ no distribution that epoch ⇒ skip silently.
- Claimant absent from tree ⇒ "not eligible" row, not an error.
- Read RPC/HTTP failures ⇒ 3 retries with backoff, then surfaced.
- Claim tx failure ⇒ recorded per epoch, batch continues.
- Invalid pubkey/keypair path ⇒ inline validation on the Setup screen.

## Testing

- Unit: PDA derivation vs known addresses, GCS listing parser (fixtures),
  merkle node lookup, `getMultipleAccounts` batching.
- Scanner against mock HTTP (`httpmock`): listing pagination, cache hit/miss,
  404, retries.
- TUI: reducer tests (event → state), render snapshots via
  `ratatui::TestBackend`.
- Claim building verified via the existing `--print-tx` path (no sends).

## Upstream plan

1. PR 1: `status` command (+ scanner) — small, easy review.
2. PR 2: `claim-all` (+ batch_claim).
3. PR 3: `tui`.
4. PR 4 (any time): `docs/claiming.md` onboarding guide modeled on Monad's
   validator-onboarding doc.
