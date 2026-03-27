#!/bin/bash
set -e

# Monthly domain reachability recheck
# Exports all domains from Tantivy, runs HEAD pre-check, imports results.
# Expected runtime: ~14 days at 2000 concurrency for 314M domains.
#
# Usage: ./scripts/monthly_recheck.sh [concurrency] [timeout]
#   concurrency: max concurrent HEAD requests (default: 2000)
#   timeout:     per-request timeout in seconds (default: 5)

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

echo "=== Monthly Recheck: $(date) ===" | tee -a "$LOG_FILE"
echo "Concurrency: $CONCURRENCY, Timeout: ${TIMEOUT}s" | tee -a "$LOG_FILE"

# Step 1: Export all domains from Tantivy index
echo "[$(date)] Step 1: Exporting domains from index..." | tee -a "$LOG_FILE"
$BIN export-domains --index "$INDEX" --output "$DOMAINS_FILE" >> "$LOG_FILE" 2>&1
DOMAIN_COUNT=$(wc -l < "$DOMAINS_FILE")
echo "[$(date)] Exported $DOMAIN_COUNT domains" | tee -a "$LOG_FILE"

# Step 2: Run HEAD pre-check with auto-import
echo "[$(date)] Step 2: Running HEAD pre-check..." | tee -a "$LOG_FILE"
$BIN precheck \
    --input "$DOMAINS_FILE" \
    --output "$REACHABLE_FILE" \
    --filtered "$FILTERED_FILE" \
    --concurrency "$CONCURRENCY" \
    --timeout "$TIMEOUT" \
    --auto-import \
    --db "$DB" \
    >> "$LOG_FILE" 2>&1

echo "[$(date)] Pre-check complete" | tee -a "$LOG_FILE"

# Step 3: Show stats
echo "[$(date)] Step 3: Precheck stats:" | tee -a "$LOG_FILE"
$BIN precheck-stats --db "$DB" 2>/dev/null | tee -a "$LOG_FILE"

# Step 4: Cleanup old files (keep last 2 months)
find /tmp -name "recheck_*" -mtime +60 -delete 2>/dev/null || true

REACHABLE_COUNT=$(wc -l < "$REACHABLE_FILE")
FILTERED_COUNT=$(wc -l < "$FILTERED_FILE")
echo "" | tee -a "$LOG_FILE"
echo "=== Summary ===" | tee -a "$LOG_FILE"
echo "Total domains:  $DOMAIN_COUNT" | tee -a "$LOG_FILE"
echo "Reachable:      $REACHABLE_COUNT" | tee -a "$LOG_FILE"
echo "Filtered:       $FILTERED_COUNT" | tee -a "$LOG_FILE"
echo "Log:            $LOG_FILE" | tee -a "$LOG_FILE"
echo "=== Done: $(date) ===" | tee -a "$LOG_FILE"
