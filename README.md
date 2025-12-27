# Zonefile Search Tantivy

High-performance domain search engine built with Rust and Tantivy. Indexes 300M+ domains with sub-50ms search latency.

## Features

- **Fast keyword search** with match-count ranking
- **Exact domain lookup**
- **TLD filtering** via facets
- **Daily incremental updates** (add/delete)
- **Redis caching** (24h TTL)
- **Word segmentation** for compound domains (e.g., "middleofnight" → ["middle", "of", "night"])

## Architecture

```
┌─────────────────┐     ┌─────────────────┐
│  domains-       │     │  word-splitter  │
│  monitor.com    │     │  API            │
└────────┬────────┘     └────────┬────────┘
         │                       │
         ▼                       ▼
┌─────────────────────────────────────────┐
│           domain-indexer                 │
│  (full build / daily sync)              │
└────────────────┬────────────────────────┘
                 │
                 ▼
┌─────────────────────────────────────────┐
│           Tantivy Index                  │
│  (~20-30GB for 314M domains)            │
└────────────────┬────────────────────────┘
                 │
                 ▼
┌─────────────────────────────────────────┐
│           domain-api (Axum)              │
│  GET /search, /exact, /health           │
└────────────────┬────────────────────────┘
                 │
                 ▼
┌─────────────────────────────────────────┐
│           Redis Cache                    │
│  (24h TTL, optional)                    │
└─────────────────────────────────────────┘
```

## Quick Start

### Prerequisites

- Rust 1.75+
- Redis (optional, for caching)

### Build

```bash
cargo build --release
```

### Environment Setup

```bash
cp .env.example .env
# Edit .env with your API credentials
```

### Full Index Build

```bash
# Download zonefile and build index
./target/release/domain-indexer full --download --output ./data/index

# Or use a local file
./target/release/domain-indexer full --input /path/to/domains.txt --output ./data/index
```

### Daily Sync

```bash
# Download daily updates and apply
./target/release/domain-indexer daily --download --index ./data/index
```

### Run API Server

```bash
./target/release/domain-api
```

## API Endpoints

### Search

```bash
# Keyword search
curl "http://localhost:3000/search?q=middle+night&tld=com&limit=50"

# Response
{
  "results": [
    {
      "domain": "middleofnight.com",
      "label": "middleofnight",
      "tld": "com",
      "length": 13,
      "has_hyphen": false,
      "tokens": ["middle", "of", "night"],
      "match_count": 2,
      "score": 15.5
    }
  ],
  "total_candidates": 1234,
  "query_time_ms": 12.5,
  "cached": false
}
```

### Exact Lookup

```bash
curl "http://localhost:3000/exact?domain=example.com"

# Response
{
  "found": true,
  "domain": {
    "domain": "example.com",
    "label": "example",
    "tld": "com",
    "length": 7,
    "has_hyphen": false,
    "tokens": ["example"]
  },
  "query_time_ms": 0.5
}
```

### Bulk Search

```bash
curl -X POST "http://localhost:3000/search/bulk" \
  -H "Content-Type: application/json" \
  -d '{
    "queries": [
      {"q": "crypto", "tld": "com"},
      {"q": "finance bank", "min_match": 2}
    ],
    "limit": 20
  }'
```

### Health & Stats

```bash
curl "http://localhost:3000/health"
curl "http://localhost:3000/stats"
```

## Deployment

### Build on MacBook, Deploy to Server

```bash
# Build release binaries
cargo build --release

# Run full indexing locally (uses 24GB RAM efficiently)
./target/release/domain-indexer full --download --heap-gb 8 --output ./data/index

# Deploy to server
./scripts/deploy.sh
```

### Set up systemd service

```bash
sudo cp scripts/domain-api.service /etc/systemd/system/
sudo systemctl enable domain-api
sudo systemctl start domain-api
```

### Cron for daily sync

```cron
0 2 * * * /opt/zonefile-search-tantivy/scripts/daily-sync.sh
```

## Performance

| Metric | Value |
|--------|-------|
| Index size | ~20-30GB (314M domains) |
| Search latency | <50ms (hot), <200ms (cold) |
| Exact lookup | <5ms |
| Full indexing | ~1-2 hours (depends on API) |
| Daily sync | ~5-10 minutes |

## Configuration

| Variable | Description | Default |
|----------|-------------|---------|
| `WORD_SPLITTER_URL` | Word segmentation API URL | Required |
| `WORD_SPLITTER_USER` | API username | Required |
| `WORD_SPLITTER_PASS` | API password | Required |
| `ZONEFILE_TOKEN` | domains-monitor.com token | Required |
| `INDEX_PATH` | Tantivy index directory | `./data/index` |
| `REDIS_URL` | Redis connection URL | Optional |
| `API_PORT` | HTTP API port | `3000` |
| `INDEX_HEAP_SIZE` | IndexWriter heap (bytes) | `4GB` |
| `WORD_BATCH_SIZE` | Labels per API request | `500` |

## License

MIT
