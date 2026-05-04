#!/bin/bash
set -e

# Configuration
APP_DIR="${APP_DIR:-/root/zonefile-search-tantivy}"
LOG_FILE="${LOG_FILE:-/var/log/domain-sync.log}"
LOCK_FILE="/tmp/domain-sync.lock"
REDIS_PASSWORD="${REDIS_PASSWORD:-}"

# Logging function
log() {
    echo "$(date '+%Y-%m-%d %H:%M:%S') - $1" >> "$LOG_FILE"
}

# Check for lock file
if [ -f "$LOCK_FILE" ]; then
    log "ERROR: Sync already running (lock file exists)"
    exit 1
fi

# Create lock file
trap "rm -f $LOCK_FILE" EXIT
touch "$LOCK_FILE"

log "Starting daily sync..."

# Load environment
cd "$APP_DIR"
if [ -f .env ]; then
    source .env
fi

# Run daily sync.
#
# DETAILED_MODE=true  → indexer prefers detailed zonefile (Pro plan).
#                       If the plan no longer supports detailed (e.g. after a
#                       downgrade) the indexer probes the endpoint, gets a
#                       403/404, logs a WARN, and falls back to plain mode so
#                       the cron still completes. To silence the warning,
#                       flip this to false.
# DETAILED_MODE=false → indexer skips the probe and uses plain mode directly.
#                       This is the right setting on the standard plan.
#
# Transient failures (5xx, timeouts) are NOT downgraded — they fail the cron
# noisily so genuine outages stay visible.
log "Downloading and applying updates..."
DETAILED_FLAG=""
if [ "${DETAILED_MODE:-false}" = "true" ]; then
    DETAILED_FLAG="--detailed"
    log "Detailed mode preferred (will probe before download)"
fi
NS_CHANGE_FLAG=""
if [ "${NOTIFY_NS_CHANGES:-false}" = "true" ]; then
    NS_CHANGE_FLAG="--notify-ns-changes"
    log "NS change detection enabled"
fi
./target/release/domain-indexer daily --download ${DETAILED_FLAG} ${NS_CHANGE_FLAG} --index "${INDEX_PATH:-./data/index}" >> "$LOG_FILE" 2>&1

# Note: API auto-reloads via Tantivy's file watcher (no restart needed)

# Clear Redis cache for fresh results
log "Clearing Redis cache..."
if command -v redis-cli &> /dev/null; then
    redis-cli -a "$REDIS_PASSWORD" FLUSHDB 2>/dev/null || true
fi

log "Daily sync completed successfully"
