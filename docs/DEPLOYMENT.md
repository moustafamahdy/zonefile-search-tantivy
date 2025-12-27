# Deployment Guide

Complete guide for deploying the Domain Search API to production environments.

## Table of Contents

1. [Prerequisites](#prerequisites)
2. [Local Development](#local-development)
3. [Building for Production](#building-for-production)
4. [Docker Deployment](#docker-deployment)
5. [Ubuntu Server Deployment](#ubuntu-server-deployment)
6. [Daily Sync Cron Setup](#daily-sync-cron-setup)
7. [Monitoring & Maintenance](#monitoring--maintenance)
8. [Troubleshooting](#troubleshooting)

---

## Prerequisites

### System Requirements

| Component | Minimum | Recommended |
|-----------|---------|-------------|
| CPU | 2 cores | 4+ cores |
| RAM | 2 GB | 4+ GB |
| Storage | 25 GB SSD | 50+ GB SSD |
| OS | Ubuntu 20.04+ | Ubuntu 22.04 LTS |

### Software Requirements

- Rust 1.75+ (for building)
- Docker & Docker Compose (for containerized deployment)
- Redis 7+ (for caching)
- Git

---

## Local Development

### 1. Clone Repository

```bash
git clone https://github.com/moustafamahdy/zonefile-search-tantivy.git
cd zonefile-search-tantivy
```

### 2. Install Rust

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source ~/.cargo/env
```

### 3. Configure Environment

```bash
cp .env.example .env
# Edit .env with your credentials
```

Required environment variables:
```bash
WORD_SPLITTER_URL=https://your-word-splitter-api.com
WORD_SPLITTER_USER=your_username
WORD_SPLITTER_PASS=your_password
ZONEFILE_TOKEN=your_zonefile_token
ZONEFILE_API_URL=https://domains-monitor.com/api/v1
INDEX_PATH=./data/index
REDIS_URL=redis://localhost:6379
API_PORT=3000
```

### 4. Build

```bash
cargo build --release
```

### 5. Run Initial Indexing

```bash
# Download and build full index (takes 3-4 hours)
./target/release/domain-indexer full --download --output ./data/index --heap-gb 8
```

### 6. Start API Server

```bash
./target/release/domain-api
```

---

## Building for Production

### Build Release Binaries

```bash
# Optimized release build
RUSTFLAGS="-C target-cpu=native" cargo build --release

# Binaries location
ls -la target/release/domain-api
ls -la target/release/domain-indexer
```

### Cross-Compile for Linux (from macOS)

```bash
# Install cross-compilation toolchain
rustup target add x86_64-unknown-linux-gnu
brew install filosottile/musl-cross/musl-cross

# Build for Linux
cargo build --release --target x86_64-unknown-linux-gnu
```

---

## Docker Deployment

### 1. Build Docker Image

```bash
docker build -t domain-search-api .
```

### 2. Using Docker Compose

```bash
# Start all services
docker-compose up -d

# View logs
docker-compose logs -f

# Stop services
docker-compose down
```

### 3. Docker Compose Configuration

```yaml
# docker-compose.yml
services:
  api:
    build: .
    container_name: domain-search-api
    ports:
      - "3000:3000"
    environment:
      - RUST_LOG=info
      - INDEX_PATH=/data/index
      - REDIS_URL=redis://redis:6379
    volumes:
      - ./data/index:/data/index:ro
    depends_on:
      redis:
        condition: service_healthy
    restart: unless-stopped
    deploy:
      resources:
        limits:
          memory: 4G

  redis:
    image: redis:7-alpine
    container_name: domain-search-redis
    command: redis-server --appendonly yes --maxmemory 512mb --maxmemory-policy allkeys-lru
    volumes:
      - redis_data:/data
    healthcheck:
      test: ["CMD", "redis-cli", "ping"]
      interval: 10s
      timeout: 3s
      retries: 3
    restart: unless-stopped

volumes:
  redis_data:
```

---

## Ubuntu Server Deployment

### 1. Initial Server Setup

```bash
# Update system
sudo apt update && sudo apt upgrade -y

# Install dependencies
sudo apt install -y build-essential pkg-config libssl-dev curl git

# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source ~/.cargo/env

# Install Redis
sudo apt install -y redis-server
sudo systemctl enable redis-server
sudo systemctl start redis-server
```

### 2. Deploy Application

```bash
# Create application directory
sudo mkdir -p /opt/zonefile-search
sudo chown $USER:$USER /opt/zonefile-search

# Clone repository
cd /opt/zonefile-search
git clone https://github.com/moustafamahdy/zonefile-search-tantivy.git .

# Build
cargo build --release

# Create data directory
mkdir -p /opt/zonefile-search/data/index
```

### 3. Configure Environment

```bash
cat > /opt/zonefile-search/.env << 'EOF'
WORD_SPLITTER_URL=https://your-word-splitter-api.com
WORD_SPLITTER_USER=your_username
WORD_SPLITTER_PASS=your_password
ZONEFILE_TOKEN=your_zonefile_token
ZONEFILE_API_URL=https://domains-monitor.com/api/v1
INDEX_PATH=/opt/zonefile-search/data/index
REDIS_URL=redis://127.0.0.1:6379
API_PORT=3000
RUST_LOG=info
EOF
chmod 600 /opt/zonefile-search/.env
```

### 4. Initial Index Build

```bash
cd /opt/zonefile-search
source .env
./target/release/domain-indexer full --download --output $INDEX_PATH --heap-gb 8
```

### 5. Create Systemd Service

```bash
sudo cat > /etc/systemd/system/domain-api.service << 'EOF'
[Unit]
Description=Domain Search API
After=network.target redis.service
Wants=redis.service

[Service]
Type=simple
User=www-data
Group=www-data
WorkingDirectory=/opt/zonefile-search
EnvironmentFile=/opt/zonefile-search/.env
ExecStart=/opt/zonefile-search/target/release/domain-api
Restart=always
RestartSec=5
StandardOutput=journal
StandardError=journal

# Security
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/opt/zonefile-search/data

# Resource limits
MemoryMax=4G
CPUQuota=200%

[Install]
WantedBy=multi-user.target
EOF

# Set permissions
sudo chown -R www-data:www-data /opt/zonefile-search/data

# Enable and start service
sudo systemctl daemon-reload
sudo systemctl enable domain-api
sudo systemctl start domain-api

# Check status
sudo systemctl status domain-api
```

### 6. Configure Nginx Reverse Proxy (Optional)

```bash
sudo apt install -y nginx

sudo cat > /etc/nginx/sites-available/domain-api << 'EOF'
server {
    listen 80;
    server_name api.yourdomain.com;

    location / {
        proxy_pass http://127.0.0.1:3000;
        proxy_http_version 1.1;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
        
        # Timeouts
        proxy_connect_timeout 60s;
        proxy_send_timeout 60s;
        proxy_read_timeout 60s;
    }
}
EOF

sudo ln -s /etc/nginx/sites-available/domain-api /etc/nginx/sites-enabled/
sudo nginx -t
sudo systemctl reload nginx
```

---

## Daily Sync Cron Setup

### 1. Create Sync Script

```bash
cat > /opt/zonefile-search/scripts/daily-sync.sh << 'EOF'
#!/bin/bash
set -e

# Configuration
APP_DIR="/opt/zonefile-search"
LOG_FILE="/var/log/domain-sync.log"
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
source .env

# Run daily sync
log "Downloading and applying updates..."
./target/release/domain-indexer daily --download --index "$INDEX_PATH" 2>&1 | tee -a "$LOG_FILE"

# Reload API to pick up changes (graceful)
log "Reloading API service..."
sudo systemctl reload domain-api || sudo systemctl restart domain-api

# Clear Redis cache for fresh results
log "Clearing Redis cache..."
redis-cli FLUSHDB

log "Daily sync completed successfully"
EOF

chmod +x /opt/zonefile-search/scripts/daily-sync.sh
```

### 2. Setup Cron Job

```bash
# Edit crontab
sudo crontab -e

# Add daily sync at 2 AM
0 2 * * * /opt/zonefile-search/scripts/daily-sync.sh >> /var/log/domain-sync.log 2>&1
```

### 3. Setup Log Rotation

```bash
sudo cat > /etc/logrotate.d/domain-sync << 'EOF'
/var/log/domain-sync.log {
    daily
    rotate 14
    compress
    delaycompress
    missingok
    notifempty
    create 0640 root root
}
EOF
```

---

## Monitoring & Maintenance

### Health Check

```bash
# Check API health
curl -s http://localhost:3000/health | jq

# Check service status
sudo systemctl status domain-api

# View recent logs
sudo journalctl -u domain-api -f
```

### Performance Monitoring

```bash
# Check memory usage
ps aux | grep domain-api | awk '{print $6/1024 " MB"}'

# Check connections
ss -tlnp | grep 3000

# Redis stats
redis-cli INFO stats
```

### Index Statistics

```bash
cd /opt/zonefile-search
./target/release/domain-indexer stats --index ./data/index
```

### Index Optimization

```bash
# Merge segments (run during low traffic)
./target/release/domain-indexer optimize --index ./data/index
```

---

## Troubleshooting

### API Won't Start

```bash
# Check logs
sudo journalctl -u domain-api -n 100

# Verify index exists
ls -la /opt/zonefile-search/data/index/

# Check permissions
sudo chown -R www-data:www-data /opt/zonefile-search/data
```

### High Memory Usage

```bash
# Limit memory in systemd
sudo systemctl edit domain-api
# Add: MemoryMax=2G

# Restart
sudo systemctl restart domain-api
```

### Slow Queries

```bash
# Check Redis connection
redis-cli ping

# Verify caching is enabled
curl -s http://localhost:3000/health | jq '.cache_enabled'

# Check index segments (high count = needs optimization)
./target/release/domain-indexer stats --index ./data/index
```

### Daily Sync Failures

```bash
# Check sync log
tail -100 /var/log/domain-sync.log

# Test API credentials
source .env
curl -I "${ZONEFILE_API_URL}/${ZONEFILE_TOKEN}/get/dailyupdate/list/zip"

# Run sync manually
./target/release/domain-indexer daily --download --index ./data/index
```

### Redis Connection Issues

```bash
# Check Redis status
sudo systemctl status redis-server

# Test connection
redis-cli ping

# Check memory
redis-cli INFO memory
```

---

## Backup & Recovery

### Backup Index

```bash
# Stop API
sudo systemctl stop domain-api

# Create backup
tar -czvf /backup/index-$(date +%Y%m%d).tar.gz /opt/zonefile-search/data/index

# Start API
sudo systemctl start domain-api
```

### Restore Index

```bash
sudo systemctl stop domain-api
tar -xzvf /backup/index-20240101.tar.gz -C /
sudo systemctl start domain-api
```

---

## Scaling

### Horizontal Scaling

For high-traffic deployments, run multiple API instances behind a load balancer:

```bash
# Instance 1 (port 3001)
API_PORT=3001 ./target/release/domain-api &

# Instance 2 (port 3002)
API_PORT=3002 ./target/release/domain-api &

# Nginx load balancer
upstream domain_api {
    least_conn;
    server 127.0.0.1:3001;
    server 127.0.0.1:3002;
}
```

### Read Replicas

For distributed deployments, sync the index to multiple servers:

```bash
# On primary server, after sync
rsync -avz /opt/zonefile-search/data/index/ replica:/opt/zonefile-search/data/index/
ssh replica 'sudo systemctl restart domain-api'
```
