# API Documentation

Domain Search API - RESTful endpoints for searching and looking up domain names.

## Base URL

```
http://localhost:3000
```

## Authentication

Currently no authentication required. Add reverse proxy with auth for production.

---

## Endpoints

### 1. Health Check

Check API and index status.

```http
GET /health
```

#### Response

```json
{
  "status": "ok",
  "index_documents": 311770911,
  "index_segments": 34,
  "cache_enabled": true
}
```

---

### 2. Index Statistics

Get detailed index statistics.

```http
GET /stats
```

#### Response

```json
{
  "documents": 311770911,
  "segments": 34,
  "index_size_bytes": 22649077760
}
```

---

### 3. Keyword Search

Search domains by keywords with match-count ranking.

```http
GET /search
```

#### Query Parameters

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `q` | string | Yes | - | Search keywords (space-separated) |
| `tld` | string | No | - | Filter by TLD (e.g., "com", "net") |
| `limit` | integer | No | 50 | Maximum results (1-500) |
| `min_match` | integer | No | 1 | Minimum keywords that must match |

#### Example Request

```bash
curl "http://localhost:3000/search?q=cloud+hosting&tld=com&limit=20"
```

#### Response

```json
{
  "results": [
    {
      "domain": "cloud-hosting.com",
      "label": "cloud-hosting",
      "tld": "com",
      "length": 13,
      "has_hyphen": true,
      "tokens": ["cloud", "hosting"],
      "match_count": 2,
      "score": 16.26
    },
    {
      "domain": "cloudhosting.com",
      "label": "cloudhosting",
      "tld": "com",
      "length": 12,
      "has_hyphen": false,
      "tokens": ["cloud", "hosting"],
      "match_count": 2,
      "score": 16.26
    }
  ],
  "total_candidates": 156,
  "query_time_ms": 3.21,
  "cached": false
}
```

#### Response Fields

| Field | Type | Description |
|-------|------|-------------|
| `results` | array | List of matching domains |
| `results[].domain` | string | Full domain name |
| `results[].label` | string | Domain without TLD |
| `results[].tld` | string | Top-level domain |
| `results[].length` | integer | Label character count |
| `results[].has_hyphen` | boolean | Contains hyphen |
| `results[].tokens` | array | Segmented keywords |
| `results[].match_count` | integer | Query keywords matched |
| `results[].score` | float | BM25 relevance score |
| `total_candidates` | integer | Total matches found |
| `query_time_ms` | float | Search time in milliseconds |
| `cached` | boolean | Result from Redis cache |

#### Ranking Algorithm

Results are ranked by:
1. **Match count** (descending) - Domains matching more keywords rank higher
2. **Domain length** (ascending) - Shorter domains rank higher
3. **BM25 score** (descending) - Tantivy relevance score

Results alternate between hyphenated and non-hyphenated domains (50/50 split).

---

### 4. Bulk Search

Execute multiple searches in a single request.

```http
POST /search/bulk
Content-Type: application/json
```

#### Request Body

```json
{
  "queries": [
    {"q": "cloud hosting", "tld": "com"},
    {"q": "web design", "min_match": 2},
    {"q": "mobile app"}
  ],
  "limit": 10
}
```

#### Request Fields

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `queries` | array | Yes | List of search queries (max 100) |
| `queries[].q` | string | Yes | Search keywords |
| `queries[].tld` | string | No | TLD filter |
| `queries[].min_match` | integer | No | Minimum match count |
| `limit` | integer | No | Results per query (default: 50) |

#### Example Request

```bash
curl -X POST "http://localhost:3000/search/bulk" \
  -H "Content-Type: application/json" \
  -d '{
    "queries": [
      {"q": "crypto trading"},
      {"q": "real estate", "tld": "com"}
    ],
    "limit": 5
  }'
```

#### Response

```json
{
  "results": [
    {
      "results": [...],
      "total_candidates": 42,
      "query_time_ms": 125.5,
      "cached": false
    },
    {
      "results": [...],
      "total_candidates": 89,
      "query_time_ms": 98.3,
      "cached": false
    }
  ],
  "total_time_ms": 225.8
}
```

---

### 5. Exact Domain Lookup

Check if a specific domain exists in the index.

```http
GET /exact
```

#### Query Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `domain` | string | Yes | Full domain name (e.g., "example.com") |

#### Example Request

```bash
curl "http://localhost:3000/exact?domain=google.com"
```

#### Response (Found)

```json
{
  "found": true,
  "domain": {
    "domain": "google.com",
    "label": "google",
    "tld": "com",
    "length": 6,
    "has_hyphen": false,
    "tokens": ["google"]
  },
  "query_time_ms": 0.42
}
```

#### Response (Not Found)

```json
{
  "found": false,
  "domain": null,
  "query_time_ms": 0.38
}
```

---

## Error Responses

### 400 Bad Request

```json
{
  "error": "Query cannot be empty"
}
```

### 500 Internal Server Error

```json
{
  "error": "Index error: failed to open index"
}
```

---

## Caching

- Results are cached in Redis with 24-hour TTL
- Cache key includes: query, TLD filter, limit, min_match
- Cached responses include `"cached": true`
- Cache provides ~2500x speedup (350ms -> 0.14ms)

---

## Rate Limits

No rate limits by default. Implement via reverse proxy if needed.

---

## Example Use Cases

### 1. Find domains with specific keywords

```bash
# Find domains containing "crypto" and "wallet"
curl "http://localhost:3000/search?q=crypto+wallet&min_match=2&limit=100"
```

### 2. Search within specific TLD

```bash
# Find .io domains with "startup"
curl "http://localhost:3000/search?q=startup&tld=io&limit=50"
```

### 3. Bulk availability check

```bash
# Check multiple domains at once
for domain in google.com facebook.com myuniquedomain123.com; do
  curl -s "http://localhost:3000/exact?domain=$domain" | jq -r ".domain // \"$domain not found\""
done
```

### 4. Integration with your application

**Python:**
```python
import requests

def search_domains(keywords, tld=None, limit=50):
    params = {"q": keywords, "limit": limit}
    if tld:
        params["tld"] = tld
    
    response = requests.get("http://localhost:3000/search", params=params)
    return response.json()

# Usage
results = search_domains("cloud hosting", tld="com", limit=20)
for domain in results["results"]:
    print(f"{domain['domain']} - {domain['match_count']} matches")
```

**JavaScript/Node.js:**
```javascript
async function searchDomains(keywords, options = {}) {
  const params = new URLSearchParams({
    q: keywords,
    limit: options.limit || 50,
    ...(options.tld && { tld: options.tld }),
    ...(options.minMatch && { min_match: options.minMatch })
  });

  const response = await fetch(`http://localhost:3000/search?${params}`);
  return response.json();
}

// Usage
const results = await searchDomains("cloud hosting", { tld: "com", limit: 20 });
results.results.forEach(d => console.log(d.domain));
```

**cURL (Batch Script):**
```bash
#!/bin/bash
# search.sh - Search domains from command line

QUERY="$1"
TLD="${2:-}"
LIMIT="${3:-50}"

URL="http://localhost:3000/search?q=${QUERY// /+}&limit=$LIMIT"
[ -n "$TLD" ] && URL="$URL&tld=$TLD"

curl -s "$URL" | jq -r '.results[] | "\(.domain)\t\(.match_count) matches"'
```
