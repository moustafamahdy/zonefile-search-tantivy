#!/bin/bash
set -e

# Deployment script for Ubuntu server
# Usage: ./scripts/deploy.sh [user@server]

SERVER="${1:-user@server}"
APP_DIR="/opt/zonefile-search"

echo "=== Building release binaries ==="
cargo build --release

echo "=== Creating deployment package ==="
PACKAGE="deploy-$(date +%Y%m%d-%H%M%S).tar.gz"
tar -czvf "/tmp/$PACKAGE" \
    target/release/domain-api \
    target/release/domain-indexer \
    scripts/ \
    .env.example \
    docs/

echo "=== Uploading to server ==="
scp "/tmp/$PACKAGE" "$SERVER:/tmp/"

echo "=== Deploying on server ==="
ssh "$SERVER" << REMOTE
    set -e
    
    # Stop service
    sudo systemctl stop domain-api 2>/dev/null || true
    
    # Extract package
    sudo mkdir -p $APP_DIR
    sudo tar -xzvf /tmp/$PACKAGE -C $APP_DIR
    
    # Set permissions
    sudo chown -R www-data:www-data $APP_DIR/data 2>/dev/null || true
    
    # Install service
    sudo cp $APP_DIR/scripts/domain-api.service /etc/systemd/system/
    sudo systemctl daemon-reload
    sudo systemctl enable domain-api
    
    # Start service
    sudo systemctl start domain-api
    
    # Cleanup
    rm /tmp/$PACKAGE
    
    echo "=== Deployment complete ==="
    sudo systemctl status domain-api
REMOTE

rm "/tmp/$PACKAGE"
echo "Done!"
