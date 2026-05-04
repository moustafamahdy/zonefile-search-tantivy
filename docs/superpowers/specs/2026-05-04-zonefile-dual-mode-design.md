# Zonefile Indexer — Dual-Mode (Detailed / Standard) Design

**Date:** 2026-05-04
**Status:** Approved (brainstorming complete; ready for implementation plan)
**Author:** Sina (with Claude)
**Driver:** Downgrade of domains-monitor.com plan from Pro to Standard. Daily sync must keep functioning under both plan tiers, and switching back must be a one-line `.env` change.

---

## 1. Problem & context

The indexer currently consumes the **detailed** zonefile from domains-monitor.com (Pro plan), which provides a CSV with per-domain fields: `dns_servers, ip, country, web_server, email, phone, seo_rank`. These fields are written into the Tantivy index (`crates/domain-core/src/schema.rs:95-104`) and exposed through the API (`crates/api/src/routes/exact.rs:29-66`).

The plan is being downgraded to **standard**, which only provides bare domain lists (`get/full/list/zip` and `get/dailyupdate/list/zip`). The detailed endpoints (`/get-detailed/...`) will return 403/404.

The dual-mode plumbing is already largely in place:
- `ZonefileType` enum has both `Full`/`DetailedFull` and `DailyUpdate`/`DetailedDailyUpdate` variants (`crates/zonefile-client/src/downloader.rs:13-24`).
- `parser.rs` exports both `DomainStream` (txt) and `DetailedDomainStream` (csv).
- `daily.rs` and `full.rs` both branch on a `detailed: bool` flag.
- `daily-sync.sh` already toggles `--detailed` from the `DETAILED_MODE` env var.
- `DomainSchema` makes detailed fields `Option<Field>`, so older indexes still load.
- API responses already serialize detailed fields with `#[serde(skip_serializing_if = "Option::is_none")]`.

The remaining work is **operational hardening** (auto-detect, logging, error UX) — not a new architecture.

## 2. Goals

- Daily sync runs successfully on both Pro and Standard plans without code changes — only `.env` changes.
- 2am cron must not fail because operator config and plan tier are out of sync.
- Switching back to Pro must be a one-line `DETAILED_MODE=true` change with no migration steps.
- Real outages (5xx, timeouts) must remain visible — never silently downgrade to plain mode.
- The chosen mode and the reason for it must appear in every sync's log output.

## 3. Non-goals

The following are deferred to separate work:

- **Wiping or rebuilding the existing 311M-doc index.** Existing detailed fields will age in place; new domains added under the standard plan will have no detail. Search/filter consumers (cosniper-front) tolerate this — `country:us`-style queries become "matches whichever country was last seen for that domain" rather than fresh-truth, which is acceptable for the lander/parking signal use case.
- **A live-DNS NS-change path for plain mode.** The NS-change feature (currently uncommitted on `main`) only functions in detailed mode and stays gated. cosniper-front already has a `dnsResolveNs` fallback (`lib/outbound/lander-filter.ts:163-168`) for live nameserver lookups.
- **cosniper-front modifications.** The `lander-filter.ts` fallback already activates automatically when `result.nameservers` is missing.
- **namemaxi-sync's `nb-cohort-analysis.js` script.** Hardcodes detailed endpoints; will break post-downgrade. Tracked separately as ad-hoc analytics, not a daily-critical workflow.
- **Schema or API-response changes.** `Option<Field>` and `skip_serializing_if = "Option::is_none"` already handle the missing-field case correctly.

## 4. Decisions (recap)

| # | Decision | Choice |
|---|---|---|
| Q1 | Existing 311M docs with stale detailed fields | **Leave as-is.** No wipe, no rebuild. Data ages in place. |
| Q2 | Uncommitted NS-change WIP feature | **Commit as-is, dormant.** Gated by `notify_ns_changes && detailed`; reactivates automatically on Pro re-upgrade. |
| Q3 | Operator UX when config/plan drift | **Auto-detect with fallback.** Probe detailed endpoint at sync start; on 403/404 fall back to plain; transient errors propagate. |
| Q4 | Role of `DETAILED_MODE` env var | **Preference / hint.** `true` = try detailed first, fall back if plan rejects. `false` = skip probe, go plain directly. |

## 5. Architecture

Two layers, bridged by auto-detect:

- **Operator intent layer:** `DETAILED_MODE` env var (read by `daily-sync.sh`, passed as `--detailed` CLI flag).
- **Plan reality layer:** Probe of the detailed endpoint, returning Available / PlanRejected / Transient-error.

`resolve_mode(prefer_detailed, &downloader)` reconciles them, returning a single `bool` that the existing `if detailed { ... } else { ... }` branches in `daily.rs` and `full.rs` consume unchanged.

No new architectural layers, no schema changes, no API contract changes.

## 6. Components

### 6.1 `ZonefileDownloader::probe_detailed_available()` — new

**File:** `crates/zonefile-client/src/downloader.rs`
**Lines:** ~30 added

```rust
pub enum ProbeResult {
    Available,                      // 200 / 206
    PlanRejected { status: u16 },   // 403 / 404
}

pub async fn probe_detailed_available(&self) -> Result<ProbeResult>
```

Sends `Range: bytes=0-0` GET to `{base_url}/{token}/get-detailed/full/list/zip` with a 10s timeout (overrides the downloader's default 1-hour download timeout — this is a probe, not a download). Returns:
- `Ok(ProbeResult::Available)` on 200/206
- `Ok(ProbeResult::PlanRejected { status })` on 403/404
- `Err(Error::...)` on any other status, timeout, or network error

Uses the same `reqwest::Client` as actual downloads so probe and production share TLS/DNS/timeout behavior.

The `ProbeResult` enum (rather than `Result<bool>`) forces the call site to distinguish "probe succeeded with negative result" from "probe failed transiently". A `bool` would tempt callers to swallow real outages with `.unwrap_or(false)`.

### 6.2 `resolve_mode()` — new

**File:** `crates/indexer/src/mode.rs` (new) or `crates/indexer/src/main.rs`
**Lines:** ~25 added

```rust
async fn resolve_mode(prefer_detailed: bool, downloader: &ZonefileDownloader) -> Result<bool>
```

- `prefer_detailed = false` → return `Ok(false)`. No probe.
  - Log: `info!("[MODE] running in mode=plain (operator preference)")`
- `prefer_detailed = true` → call `probe_detailed_available()`:
  - `Ok(Available)` → return `Ok(true)`.
    - Log: `info!("[MODE] running in mode=detailed (probe ok)")`
  - `Ok(PlanRejected { status })` → return `Ok(false)`.
    - Log: `warn!("[MODE] detailed requested but plan rejected (status={status}); falling back to plain — set DETAILED_MODE=false to silence this warning")`
  - `Err(e)` → propagate. Cron exits non-zero, alerting fires.

### 6.3 `daily.rs::run_with_download` and `full.rs::run_with_download` — modified

Each function calls `resolve_mode(detailed, &downloader)` immediately after constructing the downloader, then uses the resolved boolean for all downstream branches and for the NS-change-feature gate.

Local-file `run()` entrypoints (taking explicit `--input`/`--adds`/`--removes` paths) are **not** modified. The operator handed the indexer a specific file; respect that — no probe, no fallback.

### 6.4 NS-change detection feature — commit as-is

**Files modified by the existing WIP:** `crates/indexer/src/daily.rs`, `crates/indexer/src/main.rs`, `crates/indexer/Cargo.toml`, `scripts/daily-sync.sh`, `Cargo.lock`.

The gate `notify_ns_changes && detailed` (daily.rs:107) is already correct. Under auto-detect, the `detailed` value passed into the gate is the *resolved* boolean, so the feature correctly:
- Runs when the operator wants detailed AND the plan supports it AND `--notify-ns-changes` is set.
- No-ops silently in every other case.

One-line comment added to clarify that `detailed` here is the post-resolve value, not the raw CLI flag.

### 6.5 `scripts/daily-sync.sh` — minor

Behavior unchanged. Add a comment block above the `DETAILED_MODE` block explaining the new auto-fallback semantics so the next operator reading the script understands what `--detailed` means now ("prefer detailed, fall back to plain on plan-rejection") versus the old meaning ("force detailed, fail on rejection").

### 6.6 Operator documentation — new

**File:** `docs/PLAN_MODES.md`
**Length:** ~80 lines

Contents:
- Detailed vs. standard plan: what each provides
- How to switch: edit `.env`, set `DETAILED_MODE=true|false`, restart cron (no code deploy needed)
- What auto-detect does and how to read its log lines
- What stays stale in the index after a downgrade (Q1 → A): existing docs retain old detailed-field values; new domains added under standard plan have no detail; decision was made to accept this rather than wipe or rebuild.

## 7. Data flow

```
Cron fires
  ↓
daily-sync.sh sources .env
  ↓
domain-indexer daily --download [--detailed if DETAILED_MODE=true]
  ↓
construct ZonefileDownloader
  ↓
resolve_mode(prefer_detailed, &downloader)
  ├─ prefer=false → return false (no probe)
  └─ prefer=true → probe Range: bytes=0-0
       ├─ 200/206 → Available → return true
       ├─ 403/404 → PlanRejected → return false + WARN log
       └─ 5xx/timeout/network → Err → propagate (cron fails)
  ↓
log "[MODE] running in mode=<detailed|plain> (<reason>)"
  ↓
ZonefileDownloader::download(DetailedDailyUpdate | DailyUpdate)
ZonefileDownloader::download(DailyRemove)        [removals always plain]
  ↓
process_removals()
process_additions(detailed=resolved)
  ↓
NS-change gate: (notify_ns_changes && resolved_detailed)
  ↓
writer.commit() → reader auto-reload → API serves updated index
```

## 8. Error handling

| Outcome | HTTP signal | Fallback action | Sync result |
|---|---|---|---|
| Plan supports detailed | 200 / 206 | Use detailed | success |
| Plan rejects detailed | 403 / 404 | Fall back to plain + WARN log | success |
| Auth/token bad | 401 | Propagate | fail (loud) |
| Server error | 500 / 502 / 503 / 504 | Propagate | fail (loud) |
| Network error | timeout / connect refused / DNS fail | Propagate | fail (loud) |
| Body or zip-extraction error (post-download) | n/a | Existing `Error` variants — unchanged | fail (loud) |

Design rules:

1. **Only 403/404 trigger fallback.** Any other status or transport-level error means we don't know whether the plan supports detailed *or* whether the API is healthy — neither is a safe state for silent downgrade.
2. **Probe shares its `reqwest::Client` with the actual download.** One client, one timeout config, one retry policy — probe and production cannot diverge in subtle ways.
3. **No new error variants.** `ProbeResult` is a success type, not an error type. Failures use existing `Error::DownloadFailed { status, message }`.

## 9. Testing strategy

### 9.1 Unit tests — `probe_detailed_available()`

Mock HTTP server (`wiremock`); 6 cases: status 200, 206, 403, 404, 500, timeout. Assert correct `ProbeResult` or `Err` for each.

### 9.2 Unit tests — `resolve_mode()`

Table-driven, 6 combinations:
- `prefer=false` × any probe outcome → returns `false`, no probe call (verify probe not invoked)
- `prefer=true` × `Available` → returns `true`
- `prefer=true` × `PlanRejected` → returns `false`
- `prefer=true` × `Err(transient)` → propagates `Err`

Probe is mocked at the trait/closure level — no HTTP needed for `resolve_mode` tests.

### 9.3 Integration test

End-to-end daily sync against `wiremock` returning 403 on `/get-detailed/...` and 200 on `/get/...`. Assert:
- Sync completes successfully.
- The plain (`/get/...`) endpoint was hit, the detailed endpoint was hit exactly once (the probe).
- Log output contains `[MODE] WARN` line about fallback.
- Index doc count increased correctly.

This is the most valuable test — it catches regressions where someone refactors `resolve_mode` to return `bool` instead of erroring on transients, which would silently downgrade real outages to plain mode.

### 9.4 Manual pre-deploy verification

On staging (currently Pro plan):
- `DETAILED_MODE=true ./daily-sync.sh` → confirm `[MODE] detailed (probe ok)` log + detailed CSV downloaded.
- `DETAILED_MODE=false ./daily-sync.sh` → confirm `[MODE] plain (operator preference)` + standard endpoint downloaded.
- (Optional) Manually rotate token to invalid value and confirm 401 propagates as a hard error.

After plan downgrade on prod:
- Confirm `DETAILED_MODE=false` in `.env`.
- Run sync once manually; confirm log + index update.
- (Optional) flip `DETAILED_MODE=true` for one run to confirm fallback log fires (then revert).

### 9.5 Regression tests

None needed for the existing `--detailed`/`!--detailed` branches — those paths are unchanged. We are only changing how the boolean is *resolved* before they execute.

## 10. Implementation surface

Files to create:
- `crates/indexer/src/mode.rs` (or fold into `main.rs`)
- `crates/indexer/tests/auto_detect_integration.rs`
- `docs/PLAN_MODES.md`

Files to modify:
- `crates/zonefile-client/src/downloader.rs` (+ `ProbeResult` enum, `probe_detailed_available`)
- `crates/zonefile-client/Cargo.toml` (add `wiremock` to dev-dependencies if not present)
- `crates/indexer/src/daily.rs` (call `resolve_mode`, comment on NS-change gate)
- `crates/indexer/src/full.rs` (call `resolve_mode`)
- `crates/indexer/src/main.rs` (rename `--detailed` semantic in CLI help text)
- `scripts/daily-sync.sh` (add comment block; no behavior change)

Existing uncommitted WIP on `main` (NS-change feature) is committed as part of this work, with the one-line clarifying comment described in §6.4.

## 11. Operational rollout

1. Implement and merge to `main`. No production deploy yet.
2. On staging server: deploy, run pre-deploy verification (§9.4), confirm both modes work.
3. Set `DETAILED_MODE=false` on prod `.env` *before* the domains-monitor.com plan downgrade goes through.
4. Run `./daily-sync.sh` manually once (or wait for next 2am firing). Verify the log line says `[MODE] running in mode=plain (operator preference)`.
5. Downgrade the domains-monitor.com plan.
6. Wait for next 2am cron; verify the sync completed in plain mode and index doc count is sane.

## 12. Rollback

- If implementation causes issues post-deploy: revert the deploy commit; previous binary supports detailed mode (which is the only path it knows). Plan must still be Pro at this point for previous binary to function.
- If plan was already downgraded: revert is more complex — the previous binary will fail every sync. Mitigation: keep the new binary build artifact deployable from any git ref so we can roll forward to a fix rather than backward.

## 13. Future work

- One-time wipe utility (Q1 → B): `domain-indexer wipe-detailed` subcommand to strip detailed fields from existing docs in batch. Useful if the plan stays on standard for an extended period and stale-data drift becomes user-visible.
- Live-DNS NS-change path (Q2 → C): for namemaxi-sync to keep getting NS-change events under the standard plan. Requires a Rust DNS resolver (e.g., `trust-dns-resolver`) and FD-limit care, similar to lessons learned in cosniper-front's `lander-filter.ts`.
- `domain-indexer doctor` subcommand: probes both endpoints and prints what's available vs. what `.env` requests. Diagnostic helper, not strictly needed.
