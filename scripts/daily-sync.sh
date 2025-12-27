#!/bin/bash
set -euo pipefail

# Daily sync script for zonefile-search-tantivy
# Run via cron to update index with daily changes

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "${SCRIPT_DIR}")"
LOG_DIR="${LOG_DIR:-/var/log/zonefile-search}"
DATE=$(date +%Y-%m-%d)

# Ensure log directory exists
mkdir -p "${LOG_DIR}"

LOG_FILE="${LOG_DIR}/daily-sync-${DATE}.log"

# Log function
log() {
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] $*" | tee -a "${LOG_FILE}"
}

log "=== Starting Daily Sync ==="

# Load environment
if [ -f "${PROJECT_DIR}/.env" ]; then
    export $(grep -v '^#' "${PROJECT_DIR}/.env" | xargs)
fi

# Run the indexer
log "Running domain-indexer daily update..."
"${PROJECT_DIR}/target/release/domain-indexer" daily --download 2>&1 | tee -a "${LOG_FILE}"

RESULT=$?

if [ ${RESULT} -eq 0 ]; then
    log "Daily sync completed successfully"
else
    log "ERROR: Daily sync failed with exit code ${RESULT}"
fi

# Clean up old logs (keep last 30 days)
find "${LOG_DIR}" -name "daily-sync-*.log" -mtime +30 -delete 2>/dev/null || true

log "=== Daily Sync Finished ==="

exit ${RESULT}
