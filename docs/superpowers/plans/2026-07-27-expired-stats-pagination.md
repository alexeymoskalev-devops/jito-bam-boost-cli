# Expired Status, Claimed Stats, Dashboard Redesign — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Mark swept epochs as `expired` (excluded from claiming), add claimed/unclaimed/expired statistics, and redesign the TUI dashboard: an always-visible "Unclaimed" panel on top (where selection lives) plus a paginated read-only history list below.

**Architecture:** `EpochStatus` gains `expired: bool`, detected in `Scanner::scan` via one extra `getMultipleAccounts` batch over distributor ATAs (SPL token amount at data offset 64, LE u64; missing account or amount==0 ⇒ drained). `is_claimable()` excludes expired. TUI dashboard becomes two zones: selection operates only on the claimable subset; the full list is paginated (page state in the reducer, page size supplied by the renderer via a fixed constant).

**Tech Stack:** unchanged (Rust 1.89, ratatui 0.29, existing crates).

## Global Constraints

- All prior global constraints from `2026-07-27-tui-batch-claim.md` still apply (clippy/fmt/tests clean, conventional commits, no attribution, files < 800 lines, injectable base URL, blocking RpcClient pattern).
- `expired` semantics EXACTLY: `amount.is_some() && !claimed && vault_drained` where `vault_drained` = distributor ATA account missing OR its SPL amount == 0. Never infer from epoch arithmetic.
- `is_claimable()` becomes `amount.is_some() && !claimed && !expired` — single source of truth; all call sites (TUI selection, claim-all filter, footers) must keep using it.
- Status string in tables: exactly `"expired"`.
- Serialized `EpochStatus` gains the `expired` field (breaking JSON change is acceptable — pre-release).
- Dashboard sort: descending epoch (newest first) in BOTH the unclaimed panel and the paginated list. `status` CLI table keeps ascending order (log-style) but adds the stats footer.
- Stats line (CLI footer and TUI footer), exact shape:
  `Claimed: {n} epochs, {amt} JitoSOL | Unclaimed: {n} epochs, {amt} JitoSOL | Expired: {n} epochs, {amt} JitoSOL`
- SPL token account layout: amount is bytes 64..72 LE u64 (standard spl-token Account). Parse only if data length >= 72.

---

### Task A: scanner `expired` + `status` stats footer

**Files:** Modify `cli/src/scanner.rs`, `cli/src/bam_boost_handler.rs`.

**Interfaces:**
- `EpochStatus { epoch: u64, amount: Option<u64>, claimed: bool, expired: bool }`; `is_claimable(&self) -> bool` per Global Constraints.
- `scanner::parse_token_amount(data: &[u8]) -> Option<u64>` (pure; None if len < 72).
- `combine(epochs, amounts, claimed, drained: &HashMap<u64, bool>) -> Vec<EpochStatus>` — expired computed per Global Constraints; missing drained entry ⇒ false.
- `scanner::Stats { claimed_count, claimed_total, unclaimed_count, unclaimed_total, expired_count, expired_total }` with `Stats::from(&[EpochStatus])` (pure) and `format(&self) -> String` producing the exact stats line (amounts via a 9-decimal formatter — move `format_jitosol` into `scanner.rs` or call it; keep ONE implementation, re-export from the old path so existing imports keep compiling).
- `Scanner::scan`: after the claim-status batch, derive distributor ATAs for eligible-and-unclaimed epochs (`get_associated_token_address_with_program_id(distributor_pda, JITOSOL_MINT, spl_token)` — same call used in batch_claim) and batch `get_multiple_accounts` (chunks of 100) → drained map.

**Steps (TDD):**
- [ ] Failing tests: `parse_token_amount` (72-byte buffer with known amount at 64..72; short buffer → None); `combine` with drained map → expired computed only for unclaimed+eligible; `is_claimable` false for expired; `Stats::from` + exact `format` string on a mixed fixture.
- [ ] Implement; extend `scan()` with the ATA batch.
- [ ] `status` handler: print `"expired"` rows; replace old footer with `Stats::format` line; `--print-json` now includes `expired` field.
- [ ] Gates: `cargo test -p jito-bam-boost-cli`, clippy, fmt. Commit `feat: detect expired epochs via drained distributor vaults`.

### Task B: claim-all skips expired + stats

**Files:** Modify `cli/src/bam_boost_handler.rs`.

- [ ] `claim_all` filter already uses `is_claimable()` — verify expired now excluded automatically; add a printed note when expired epochs exist: `Skipping {n} expired epoch(s) ({amt} JitoSOL no longer claimable)`. Print `Stats::format` line after the scan, before the unclaimed listing.
- [ ] Gates + commit `feat: claim-all reports stats and skips expired epochs`.

### Task C: TUI reducer — two-zone dashboard + pagination

**Files:** Modify `cli/src/tui/app.rs`.

**Interfaces (reducer rules):**
- New fields: `page: usize` (0-based, main list), keep `cursor`/`selected` but they now index the CLAIMABLE subset sorted descending (`claimable_sorted(&self) -> Vec<u64>` helper: epochs where `is_claimable()`, desc).
- `PAGE_SIZE: usize = 20` (const in app.rs; ui renders exactly this many rows per page).
- After `ScanFinished(Ok)`: statuses stored as received; `page = 0`, `cursor = 0`, `selected.clear()`.
- Dashboard keys: `Up`/`Down` move cursor within claimable subset (clamped); `Space` toggles epoch under cursor; `a` selects all claimable; `c` → Confirm if selection non-empty; `Left`/`PageUp` and `Right`/`PageDown` change `page` clamped to `0..=max_page` where `max_page = statuses.len().saturating_sub(1) / PAGE_SIZE`; `r` (only when `!scanning`) rescan; `q`/`Esc` quit.
- [ ] Failing reducer tests first: cursor navigation over claimable subset (fixture with mixed claimed/expired/unclaimed, desc order); paging clamps at both ends; select-all excludes expired; existing tests updated for the new `expired` field (add `expired: false` to fixtures).
- [ ] Implement; gates; commit `feat: TUI reducer for unclaimed panel and paginated history`.

### Task D: TUI rendering + docs

**Files:** Modify `cli/src/tui/ui.rs`, `docs/claiming.md`.

- [ ] Dashboard layout top-to-bottom: header; bordered "Unclaimed" panel listing claimable epochs desc with `[x]`/`[ ]` markers + amounts + REVERSED highlight on cursor row (stateful scroll if it overflows; panel height min(claimable+2, 8)); bordered `All epochs — page {p+1}/{max+1}` table showing `statuses` sorted desc, rows `page*PAGE_SIZE .. +PAGE_SIZE`, read-only (no highlight), status column incl. `"expired"`; footer line 1 = `Stats::format`, line 2 = `space select · a all · c claim · ←/→ page · r rescan · q quit`. When claimable set is empty, panel shows `Nothing to claim`.
- [ ] TestBackend tests: mixed fixture — unclaimed panel contains only claimable epochs; page 0 shows newest epoch and not the oldest; after simulating page change (set `app.page = 1`) the buffer shows the next slice; stats line present; `expired` string rendered.
- [ ] Update `docs/claiming.md`: expired semantics (claim window ~9 epochs, vaults swept — cite the on-chain behavior), new keyboard reference (←/→ pages), stats line description, note that expired epochs are auto-skipped by claim-all.
- [ ] Gates; commit `feat: TUI unclaimed panel, paginated history and stats footer` + `docs: document expired epochs, pagination and stats`.

## Final verification

- [ ] Full `cargo test` workspace, clippy, fmt.
- [ ] Push branch (updates PR #1).
