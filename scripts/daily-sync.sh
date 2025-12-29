#!/bin/bash
set -e

# Configuration
APP_DIR="${APP_DIR:-/opt/zonefile-search}"
LOG_FILE="${LOG_FILE:-/var/log/domain-sync.log}"
LOCK_FILE="/tmp/domain-sync.lock"

# Logging function
log() {
    echo "$(date '+%Y-%m-%d %H:%M:%S') - $1" | tee -a "$LOG_FILE"
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
./target/release/domain-indexer daily --download --index "${INDEX_PATH:-./data/index}" 2>&1 | tee -a "$LOG_FILE"

# Note: API auto-reloads via Tantivy's file watcher (no restart needed)

# Clear Redis cache for fresh results
log "Clearing Redis cache..."
if command -v redis-cli &> /dev/null; then
    redis-cli FLUSHDB 2>/dev/null || true
fi

log "Daily sync completed successfully"
