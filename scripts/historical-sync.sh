#!/bin/bash
set -e

# Historical sync script for applying missed daily updates
# Uses the domains-monitor.com historical API
#
# Usage: ./historical-sync.sh DATE [DATE2 DATE3 ...]
#        ./historical-sync.sh --range START_DATE END_DATE
#
# Date format: YYYY-MM-DD (will be converted to dd.mm.yyyy for API)
#
# Examples:
#   ./historical-sync.sh 2025-01-01
#   ./historical-sync.sh 2025-01-01 2025-01-02 2025-01-03
#   ./historical-sync.sh --range 2025-01-01 2025-01-04

# Configuration
REDIS_PASSWORD="${REDIS_PASSWORD:-}"

APP_DIR="${APP_DIR:-/root/zonefile-search-tantivy}"
LOG_FILE="${LOG_FILE:-/var/log/domain-sync.log}"
DOWNLOAD_DIR="/tmp/zonefile-historical"

# Load environment
cd "$APP_DIR"
if [ -f .env ]; then
    source .env
fi

# Check required env vars
if [ -z "$ZONEFILE_TOKEN" ]; then
    echo "ERROR: ZONEFILE_TOKEN not set in environment"
    exit 1
fi

ZONEFILE_API_URL="${ZONEFILE_API_URL:-https://domains-monitor.com/api/v1}"

log() {
    echo "$(date '+%Y-%m-%d %H:%M:%S') - $1" | tee -a "$LOG_FILE"
}

# Convert YYYY-MM-DD to dd.mm.yyyy
convert_date() {
    local input_date="$1"
    # Parse and reformat
    date -d "$input_date" +"%d.%m.%Y"
}

# Download historical file
download_historical() {
    local date_api="$1"  # dd.mm.yyyy format
    local type="$2"      # dailyupdate or dailyremove
    local output_file="$3"

    local url="${ZONEFILE_API_URL}/${ZONEFILE_TOKEN}/historical/${type}/${date_api}/"

    log "Downloading ${type} for ${date_api}..."

    # Download the ZIP file
    local zip_file="${output_file}.zip"
    local http_code=$(curl -s -w "%{http_code}" -o "$zip_file" "$url")

    if [ "$http_code" != "200" ]; then
        log "ERROR: Failed to download ${type} for ${date_api} (HTTP ${http_code})"
        rm -f "$zip_file"
        return 1
    fi

    # Check if it's actually a ZIP file
    if ! file "$zip_file" | grep -q "Zip archive"; then
        log "ERROR: Downloaded file is not a ZIP archive for ${type} ${date_api}"
        cat "$zip_file"  # Show error message
        rm -f "$zip_file"
        return 1
    fi

    # Extract the txt file
    unzip -o -q "$zip_file" -d "$(dirname "$output_file")"

    # Find and rename the extracted txt file
    local extracted=$(find "$(dirname "$output_file")" -maxdepth 1 -name "*.txt" -newer "$zip_file" -o -name "domains.txt" 2>/dev/null | head -1)
    if [ -z "$extracted" ]; then
        # Try listing all txt files and get the most recent
        extracted=$(ls -t "$(dirname "$output_file")"/*.txt 2>/dev/null | head -1)
    fi

    if [ -n "$extracted" ] && [ "$extracted" != "$output_file" ]; then
        mv "$extracted" "$output_file"
    fi

    rm -f "$zip_file"

    if [ -f "$output_file" ]; then
        local lines=$(wc -l < "$output_file")
        log "Downloaded ${type}: ${lines} domains"
        return 0
    else
        log "ERROR: Failed to extract ${type} for ${date_api}"
        return 1
    fi
}

# Process a single date
process_date() {
    local date_ymd="$1"  # YYYY-MM-DD format
    local date_api=$(convert_date "$date_ymd")  # dd.mm.yyyy format

    log "=========================================="
    log "Processing historical sync for ${date_ymd}"
    log "=========================================="

    mkdir -p "$DOWNLOAD_DIR"

    local update_file="${DOWNLOAD_DIR}/dailyupdate_${date_ymd}.txt"
    local remove_file="${DOWNLOAD_DIR}/dailyremove_${date_ymd}.txt"

    # Download both files
    local has_updates=0
    local has_removes=0

    if download_historical "$date_api" "dailyupdate" "$update_file"; then
        has_updates=1
    fi

    if download_historical "$date_api" "dailyremove" "$remove_file"; then
        has_removes=1
    fi

    # Skip if neither file was downloaded
    if [ $has_updates -eq 0 ] && [ $has_removes -eq 0 ]; then
        log "WARNING: No data available for ${date_ymd}, skipping"
        return 0
    fi

    # Build command arguments
    local cmd_args="daily --index ${INDEX_PATH:-./data/index}"

    if [ $has_updates -eq 1 ] && [ -f "$update_file" ]; then
        cmd_args="$cmd_args --adds $update_file"
    fi

    if [ $has_removes -eq 1 ] && [ -f "$remove_file" ]; then
        cmd_args="$cmd_args --removes $remove_file"
    fi

    # Run the indexer
    log "Applying updates to index..."
    ./target/release/domain-indexer $cmd_args 2>&1 | tee -a "$LOG_FILE"

    # Clean up
    rm -f "$update_file" "$remove_file"

    log "Completed historical sync for ${date_ymd}"
}

# Generate date range
generate_date_range() {
    local start_date="$1"
    local end_date="$2"

    local current="$start_date"
    while [ "$(date -d "$current" +%s)" -le "$(date -d "$end_date" +%s)" ]; do
        echo "$current"
        current=$(date -d "$current + 1 day" +%Y-%m-%d)
    done
}

# Main
if [ $# -eq 0 ]; then
    echo "Usage: $0 DATE [DATE2 DATE3 ...]"
    echo "       $0 --range START_DATE END_DATE"
    echo ""
    echo "Date format: YYYY-MM-DD"
    echo ""
    echo "Examples:"
    echo "  $0 2025-01-01"
    echo "  $0 2025-01-01 2025-01-02 2025-01-03"
    echo "  $0 --range 2025-01-01 2025-01-04"
    exit 1
fi

dates=()

if [ "$1" == "--range" ]; then
    if [ $# -ne 3 ]; then
        echo "ERROR: --range requires START_DATE and END_DATE"
        exit 1
    fi

    start_date="$2"
    end_date="$3"

    log "Generating date range from ${start_date} to ${end_date}"

    while IFS= read -r date; do
        dates+=("$date")
    done < <(generate_date_range "$start_date" "$end_date")
else
    dates=("$@")
fi

log "=========================================="
log "Historical Sync - Processing ${#dates[@]} date(s)"
log "=========================================="

for date in "${dates[@]}"; do
    process_date "$date"
done

# Clear Redis cache
log "Clearing Redis cache..."
if command -v redis-cli &> /dev/null; then
    redis-cli -a "$REDIS_PASSWORD" FLUSHDB 2>/dev/null || true
fi

log "=========================================="
log "Historical sync completed for all dates"
log "=========================================="
