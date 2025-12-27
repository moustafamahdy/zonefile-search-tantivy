# Zonefile Search Tantivy

High-performance domain search engine built with Rust and Tantivy. Indexes 311M+ domains with sub-millisecond cached search and 11,000+ requests per second throughput.

## Technologies

| Category | Technology | Purpose |
|----------|------------|---------|
| **Language** | Rust 1.83 | Systems programming, memory safety, performance |
| **Search Engine** | [Tantivy](https://github.com/quickwit-oss/tantivy) | Full-text search, BM25 ranking, inverted index |
| **Web Framework** | [Axum](https://github.com/tokio-rs/axum) | Async HTTP API server |
| **Async Runtime** | [Tokio](https://tokio.rs) | Async I/O, task scheduling |
| **Caching** | [Redis](https://redis.io) | Query result caching (24h TTL) |
| **Serialization** | [Serde](https://serde.rs) + JSON | API request/response handling |
| **HTTP Client** | [Reqwest](https://github.com/seanmonstar/reqwest) | External API calls |
| **CLI** | [Clap](https://github.com/clap-rs/clap) | Command-line argument parsing |
| **Logging** | [Tracing](https://github.com/tokio-rs/tracing) | Structured logging |
| **Compression** | [async-zip](https://github.com/Majored/rs-async-zip) | Zonefile archive extraction |
| **Containerization** | Docker + Docker Compose | Deployment orchestration |

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

### Benchmarks (311M domains, Apple M2 Pro)

| Metric | Value |
|--------|-------|
| **Index size** | 21.09 GB |
| **Documents indexed** | 311,770,911 |
| **Throughput (cached)** | 11,461 req/sec |
| **Throughput (uncached)** | 304-971 req/sec |
| **Search latency (cached)** | 0.14ms |
| **Search latency (uncached)** | 20-350ms |
| **Exact lookup** | <1ms |
| **Full indexing time** | ~3.5 hours |
| **Daily sync time** | ~3 minutes |

### Resource Usage

| Resource | Idle | Under Load |
|----------|------|------------|
| **Memory (RSS)** | 268 MB | 411 MB |
| **CPU** | 0% | 14% peak |

### Server Requirements

| Tier | CPU | RAM | Storage | Expected RPS |
|------|-----|-----|---------|--------------|
| Minimum | 2 cores | 2 GB | 25 GB SSD | 1,000+ |
| Recommended | 4 cores | 4 GB | 50 GB SSD | 10,000+ |
| Production | 8 cores | 8 GB | 100 GB SSD | 20,000+ |

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
