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

# Run daily sync
log "Downloading and applying updates..."
DETAILED_FLAG=""
if [ "${DETAILED_MODE:-false}" = "true" ]; then
    DETAILED_FLAG="--detailed"
    log "Using detailed mode"
fi
./target/release/domain-indexer daily --download ${DETAILED_FLAG} --index "${INDEX_PATH:-./data/index}" >> "$LOG_FILE" 2>&1

# Note: API auto-reloads via Tantivy's file watcher (no restart needed)

# Clear Redis cache for fresh results
log "Clearing Redis cache..."
if command -v redis-cli &> /dev/null; then
    redis-cli -a "$REDIS_PASSWORD" FLUSHDB 2>/dev/null || true
fi

log "Daily sync completed successfully"
