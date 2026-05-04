# Plan Modes — Detailed vs. Standard

The indexer can run against either the **detailed** (Pro plan) or **standard** (basic plan) zonefile feeds from domains-monitor.com. This doc explains how to switch between them and what to expect.

## What each plan provides

| Endpoint | Pro plan (detailed) | Standard plan |
|---|---|---|
| `get-detailed/full/list/zip` | ✅ CSV with `dns_servers, ip, country, web_server, email, phone, seo_rank` | ❌ 403/404 |
| `get-detailed/full-update/list/zip` | ✅ Daily added domains, detailed CSV | ❌ 403/404 |
| `get/full/list/zip` | ✅ Plain `domains.txt` | ✅ Plain `domains.txt` |
| `get/dailyupdate/list/zip` | ✅ Plain | ✅ Plain |
| `get/dailyremove/list/zip` | ✅ Plain | ✅ Plain |

Removals are always plain on both plans — there is no detailed remove feed.

## Switching modes

The toggle lives in `.env`:

```bash
# Pro plan (detailed)
DETAILED_MODE=true
NOTIFY_NS_CHANGES=true   # optional; only meaningful in detailed mode

# Standard plan (plain)
DETAILED_MODE=false
NOTIFY_NS_CHANGES=false
```

After editing `.env`, the next cron firing picks up the change. Run `./scripts/daily-sync.sh` once manually to verify before walking away.

## Auto-detect & fallback

When `DETAILED_MODE=true`, the indexer **probes** the detailed endpoint at sync start (a `Range: bytes=0-0` GET). The probe is a safety net for the case where operator config and plan tier drift apart:

| Probe outcome | Action | Sync result |
|---|---|---|
| 200 / 206 | Use detailed | success |
| 403 / 404 | Fall back to plain + WARN log | success |
| 401 | Propagate (token issue) | fail (loud) |
| 5xx, timeout, network error | Propagate | fail (loud) |

The asymmetry is deliberate: a **plan mismatch** self-heals so 2am cron doesn't wake anyone up; a **real outage** propagates so it stays visible and gets investigated.

When `DETAILED_MODE=false`, the indexer skips the probe entirely and uses plain mode. No safety net is needed because the operator has explicitly chosen plain.

## Reading the [MODE] log lines

Every sync prints exactly one `[MODE]` line near the top of its output:

| Log line | Meaning |
|---|---|
| `[MODE] running in mode=detailed (probe ok)` | Pro plan, detailed CSV downloaded |
| `[MODE] running in mode=plain (operator preference)` | `DETAILED_MODE=false`; no probe |
| `[MODE] WARN: detailed requested but plan rejected; falling back to plain mode — set DETAILED_MODE=false to silence this warning` | Probe returned 403/404; plan likely downgraded since `.env` was last touched |

If you see the WARN line repeatedly, edit `.env` and set `DETAILED_MODE=false` — the safety net is doing its job, but it's logging the discrepancy on every run until you fix the config.

## What happens to existing index data when you switch

**Switching detailed → plain** (the current downgrade scenario):

- New domains added during plain-plan periods will have **no** `dns_servers`, `ip`, `country`, `web_server`, `email`, `phone`, or `seo_rank` fields.
- Existing 311M+ domains keep their last-known detailed values, even though those values are no longer being refreshed.
- Search and `/exact` API responses transparently omit the missing fields (via serde's `skip_serializing_if = "Option::is_none"`).
- Filtering queries like `country:us` continue to work but only match docs that retained that data from before the downgrade — not a fresh truth.

This is intentional. Wiping or rebuilding the index would produce a cleaner state but would also cost hours of downtime / re-download. The dual-mode design accepts the staleness for the duration of plan-tier changes.

**Switching plain → detailed** (re-upgrade):

- The probe returns Available; new daily updates start writing detailed fields again.
- Existing docs added during the plain period are **not** retroactively enriched. Only domains touched by daily-add CSVs get fresh detailed values.
- A future `domain-indexer wipe-detailed` subcommand or a full re-build would fix this if drift becomes a problem; not yet implemented.

## NS-change notifications

The `--notify-ns-changes` flag (and `NOTIFY_NS_CHANGES=true` in `.env`) only does anything in detailed mode. The feature compares each newly added domain's nameservers against the existing index entry and POSTs changes to the `NS_CHANGE_ENDPOINT`. With no detailed CSV available, there's nothing to compare against — the gate `notify_ns_changes && detailed` is always false on the standard plan, and the feature silently no-ops.

When you re-upgrade to Pro, flip both `DETAILED_MODE=true` and `NOTIFY_NS_CHANGES=true` in `.env` to reactivate.

## Local-file mode

The local-file paths (`domain-indexer full --input <file>` and `domain-indexer daily --adds <file> --removes <file>`) **skip the probe entirely**. The operator handed the indexer a specific file, so we trust the `--detailed` flag verbatim — no fallback. This matters when running ad-hoc rebuilds against a CSV someone copied to disk.

## Reference

Full design rationale: [`docs/superpowers/specs/2026-05-04-zonefile-dual-mode-design.md`](superpowers/specs/2026-05-04-zonefile-dual-mode-design.md).
