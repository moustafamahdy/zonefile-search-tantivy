#!/bin/bash
set -euo pipefail

# Deploy script for zonefile-search-tantivy
# Compresses index and syncs to production server

# Configuration
REMOTE_USER="${REMOTE_USER:-root}"
REMOTE_HOST="${REMOTE_HOST:-your-server.com}"
REMOTE_PATH="${REMOTE_PATH:-/data/index/releases}"
INDEX_PATH="${INDEX_PATH:-./data/index}"

DATE=$(date +%Y-%m-%d)
ARCHIVE_NAME="index-${DATE}.tar.zst"

echo "=== Zonefile Search Tantivy Deployment ==="
echo "Date: ${DATE}"
echo "Index path: ${INDEX_PATH}"
echo "Remote: ${REMOTE_USER}@${REMOTE_HOST}:${REMOTE_PATH}"
echo ""

# Check if index exists
if [ ! -d "${INDEX_PATH}" ]; then
    echo "Error: Index directory not found: ${INDEX_PATH}"
    exit 1
fi

# Calculate index size
INDEX_SIZE=$(du -sh "${INDEX_PATH}" | cut -f1)
echo "Index size: ${INDEX_SIZE}"
echo ""

# Compress with zstd (fastest with good compression)
echo "Compressing index..."
if command -v zstd &> /dev/null; then
    tar -cf - -C "$(dirname "${INDEX_PATH}")" "$(basename "${INDEX_PATH}")" | zstd -T0 -3 > "${ARCHIVE_NAME}"
else
    echo "zstd not found, using gzip..."
    ARCHIVE_NAME="index-${DATE}.tar.gz"
    tar -czf "${ARCHIVE_NAME}" -C "$(dirname "${INDEX_PATH}")" "$(basename "${INDEX_PATH}")"
fi

ARCHIVE_SIZE=$(du -sh "${ARCHIVE_NAME}" | cut -f1)
echo "Archive size: ${ARCHIVE_SIZE}"
echo ""

# Upload to server
echo "Uploading to ${REMOTE_HOST}..."
rsync -avz --progress "${ARCHIVE_NAME}" "${REMOTE_USER}@${REMOTE_HOST}:${REMOTE_PATH}/"

# Extract on server and switch symlink
echo "Extracting on remote server..."
ssh "${REMOTE_USER}@${REMOTE_HOST}" << EOF
    set -e
    cd ${REMOTE_PATH}

    # Create dated directory
    mkdir -p ${DATE}

    # Extract archive
    if [[ "${ARCHIVE_NAME}" == *.zst ]]; then
        zstd -d < ${ARCHIVE_NAME} | tar -xf - -C ${DATE}
    else
        tar -xzf ${ARCHIVE_NAME} -C ${DATE}
    fi

    # Atomic symlink switch
    ln -sfn ${REMOTE_PATH}/${DATE}/index /data/index/current

    # Restart API service
    systemctl restart domain-api || true

    # Clean up old releases (keep last 5)
    ls -dt */ | tail -n +6 | xargs -r rm -rf

    echo "Deployment complete!"
EOF

# Clean up local archive
rm -f "${ARCHIVE_NAME}"

echo ""
echo "=== Deployment Complete ==="
echo "Index deployed to: ${REMOTE_HOST}:${REMOTE_PATH}/${DATE}"
echo "Symlink: /data/index/current"
