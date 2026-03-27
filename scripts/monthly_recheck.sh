#!/bin/bash
set -e

# Monthly domain reachability recheck + metadata fetch pipeline
# 1. Export all domains from Tantivy (catches new domains from daily sync)
# 2. HEAD pre-check all domains → find reachable ones
# 3. Import precheck results into DB
# 4. Fetch page metadata for reachable domains (--resume skips already done)
# 5. Import metadata into Tantivy index for API serving
#
# Expected runtime: ~14 days HEAD check + ~10 days fetch-pages ≈ 24 days
# Runs 1st of each month. Next run starts after previous finishes naturally.
#
# Usage: ./scripts/monthly_recheck.sh [concurrency] [timeout]
#   concurrency: max concurrent requests (default: 2000)
#   timeout:     per-request timeout in seconds (default: 5)

# Bump file descriptor limit for high concurrency
ulimit -n 65535 2>/dev/null || true

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
BIN="$PROJECT_DIR/target/release/domain-metadata"
DB="$PROJECT_DIR/data/metadata.db"
INDEX="$PROJECT_DIR/data/index"

CONCURRENCY="${1:-2000}"
TIMEOUT="${2:-5}"

# Timestamped output files
DATE=$(date +%Y%m%d)
DOMAINS_FILE="/tmp/recheck_domains_${DATE}.txt"
REACHABLE_FILE="/tmp/recheck_reachable_${DATE}.txt"
FILTERED_FILE="/tmp/recheck_filtered_${DATE}.txt"
LOG_FILE="/tmp/recheck_${DATE}.log"

log() {
    echo "[$(date)] $1" | tee -a "$LOG_FILE"
}

log "=== Monthly Recheck Pipeline Started ==="
log "Concurrency: $CONCURRENCY, Timeout: ${TIMEOUT}s"

# ── Step 1: Export all domains from Tantivy index ──
log "Step 1/5: Exporting domains from index..."
$BIN export-domains --index "$INDEX" --output "$DOMAINS_FILE" >> "$LOG_FILE" 2>&1
DOMAIN_COUNT=$(wc -l < "$DOMAINS_FILE")
log "Exported $DOMAIN_COUNT domains"

# ── Step 2: HEAD pre-check with auto-import ──
log "Step 2/5: Running HEAD pre-check on $DOMAIN_COUNT domains..."
$BIN precheck \
    --input "$DOMAINS_FILE" \
    --output "$REACHABLE_FILE" \
    --filtered "$FILTERED_FILE" \
    --concurrency "$CONCURRENCY" \
    --timeout "$TIMEOUT" \
    --auto-import \
    --db "$DB" \
    >> "$LOG_FILE" 2>&1

REACHABLE_COUNT=$(wc -l < "$REACHABLE_FILE")
FILTERED_COUNT=$(wc -l < "$FILTERED_FILE")
log "Pre-check complete: $REACHABLE_COUNT reachable, $FILTERED_COUNT filtered"

# ── Step 3: Precheck stats ──
log "Step 3/5: Precheck stats:"
$BIN precheck-stats --db "$DB" 2>/dev/null | tee -a "$LOG_FILE"

# ── Step 4: Fetch page metadata for reachable domains ──
# --resume skips domains that already have metadata in the DB
log "Step 4/5: Fetching page metadata for $REACHABLE_COUNT reachable domains..."
$BIN fetch-pages \
    --domains-file "$REACHABLE_FILE" \
    --db "$DB" \
    --concurrency "$CONCURRENCY" \
    --resume \
    >> "$LOG_FILE" 2>&1
log "Page metadata fetch complete"

# ── Step 5: Import metadata into Tantivy index ──
log "Step 5/5: Importing metadata into Tantivy index..."
$BIN import --db "$DB" --index "$INDEX" >> "$LOG_FILE" 2>&1
log "Tantivy import complete"

# Show final page stats
$BIN page-stats --db "$DB" 2>/dev/null | tee -a "$LOG_FILE"

# Cleanup old files (keep last 2 months)
find /tmp -name "recheck_*" -mtime +60 -delete 2>/dev/null || true

log ""
log "=== Monthly Pipeline Summary ==="
log "Total domains:  $DOMAIN_COUNT"
log "Reachable:      $REACHABLE_COUNT"
log "Filtered:       $FILTERED_COUNT"
log "Log:            $LOG_FILE"
log "=== Pipeline Complete ==="
