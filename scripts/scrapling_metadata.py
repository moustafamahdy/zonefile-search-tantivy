#!/usr/bin/env python3
"""
Fetch page metadata for domains using Scrapling.

Uses two-tier strategy:
1. AsyncFetcher (curl_cffi with Chrome TLS fingerprint) - fast, ~5-10 req/s
2. StealthyFetcher (Camoufox browser) - slow fallback for Cloudflare-blocked sites

Input: text file with one domain per line
Output: JSONL file with one JSON object per line (PageMetadataResult format)

Usage:
    python scrapling_metadata.py --input domains.txt --output metadata.jsonl [--concurrency 20] [--stealth-concurrency 2]

Setup:
    python3 -m venv /opt/scrapling-env
    source /opt/scrapling-env/bin/activate
    pip install "scrapling[all]"
    scrapling install
"""

import argparse
import asyncio
import json
import sys
import time
from pathlib import Path

try:
    from scrapling import AsyncFetcher, StealthyFetcher
except ImportError:
    print("ERROR: scrapling not installed. Run: pip install 'scrapling[all]'", file=sys.stderr)
    sys.exit(1)


def extract_metadata(page, domain: str, source: str = "scrapling") -> dict:
    """Extract metadata from a Scrapling page response."""
    result = {
        "domain": domain,
        "http_status": page.status if hasattr(page, "status") else 200,
        "page_title": None,
        "meta_description": None,
        "og_title": None,
        "og_description": None,
        "og_image": None,
        "canonical_url": None,
        "language": None,
        "generator": None,
        "snippet": None,
        "content_type": None,
        "fetched_at": int(time.time()),
        "source": source,
        "error": None,
    }

    try:
        # Title
        title_el = page.css_first("title")
        if title_el:
            result["page_title"] = title_el.text()[:500].strip() or None

        # Meta description
        desc_el = page.css_first('meta[name="description"]')
        if desc_el:
            result["meta_description"] = (desc_el.attrib.get("content", "") or "")[:1000].strip() or None

        # OG tags
        og_title_el = page.css_first('meta[property="og:title"]')
        if og_title_el:
            result["og_title"] = (og_title_el.attrib.get("content", "") or "")[:500].strip() or None

        og_desc_el = page.css_first('meta[property="og:description"]')
        if og_desc_el:
            result["og_description"] = (og_desc_el.attrib.get("content", "") or "")[:1000].strip() or None

        og_img_el = page.css_first('meta[property="og:image"]')
        if og_img_el:
            result["og_image"] = (og_img_el.attrib.get("content", "") or "")[:2000].strip() or None

        # Canonical URL
        canonical_el = page.css_first('link[rel="canonical"]')
        if canonical_el:
            result["canonical_url"] = (canonical_el.attrib.get("href", "") or "")[:2000].strip() or None

        # Language
        html_el = page.css_first("html")
        if html_el:
            lang = html_el.attrib.get("lang", "")
            if lang:
                # Extract primary language code
                result["language"] = lang.split("-")[0].split("_")[0].lower().strip() or None

        if not result["language"]:
            lang_meta = page.css_first('meta[http-equiv="content-language"]')
            if lang_meta:
                lang = lang_meta.attrib.get("content", "")
                if lang:
                    result["language"] = lang.split("-")[0].split("_")[0].lower().strip() or None

        # Generator
        gen_el = page.css_first('meta[name="generator"]')
        if gen_el:
            result["generator"] = (gen_el.attrib.get("content", "") or "")[:200].strip() or None

        # Snippet (prefer description, fallback to og:description)
        result["snippet"] = result["meta_description"] or result["og_description"]

    except Exception as e:
        result["error"] = f"parse error: {str(e)[:200]}"

    return result


async def fetch_with_async(fetcher, domain: str, semaphore: asyncio.Semaphore) -> dict:
    """Fetch a domain using AsyncFetcher with concurrency control."""
    async with semaphore:
        try:
            url = f"https://{domain}"
            page = await fetcher.get(url, timeout=15)

            if page.status == 403 or page.status == 503:
                return {"domain": domain, "blocked": True, "status": page.status}

            meta = extract_metadata(page, domain, source="scrapling-async")
            return meta

        except Exception as e:
            error_str = str(e)[:200]
            return {
                "domain": domain,
                "http_status": 0,
                "page_title": None,
                "meta_description": None,
                "og_title": None,
                "og_description": None,
                "og_image": None,
                "canonical_url": None,
                "language": None,
                "generator": None,
                "snippet": None,
                "content_type": None,
                "fetched_at": int(time.time()),
                "source": "scrapling-async",
                "error": f"fetch failed: {error_str}",
            }


def fetch_with_stealth(domain: str) -> dict:
    """Fetch a domain using StealthyFetcher (sync, browser-based)."""
    try:
        fetcher = StealthyFetcher()
        url = f"https://{domain}"
        page = fetcher.fetch(url, headless=True, timeout=30000)

        meta = extract_metadata(page, domain, source="scrapling-stealth")
        return meta

    except Exception as e:
        error_str = str(e)[:200]
        return {
            "domain": domain,
            "http_status": 0,
            "page_title": None,
            "meta_description": None,
            "og_title": None,
            "og_description": None,
            "og_image": None,
            "canonical_url": None,
            "language": None,
            "generator": None,
            "snippet": None,
            "content_type": None,
            "fetched_at": int(time.time()),
            "source": "scrapling-stealth",
            "error": f"stealth fetch failed: {error_str}",
        }


async def run_async_pass(
    domains: list[str],
    concurrency: int,
    output_file: Path,
) -> list[str]:
    """Run AsyncFetcher pass. Returns list of blocked domains for stealth retry."""
    fetcher = AsyncFetcher()
    semaphore = asyncio.Semaphore(concurrency)
    blocked = []
    total = len(domains)
    done = 0
    enriched = 0

    with open(output_file, "a") as f:
        # Process in batches to avoid overwhelming memory
        batch_size = concurrency * 2
        for i in range(0, total, batch_size):
            batch = domains[i : i + batch_size]
            tasks = [fetch_with_async(fetcher, d, semaphore) for d in batch]
            results = await asyncio.gather(*tasks, return_exceptions=True)

            for result in results:
                if isinstance(result, Exception):
                    continue

                if result.get("blocked"):
                    blocked.append(result["domain"])
                    continue

                # Write to JSONL
                f.write(json.dumps(result, ensure_ascii=False) + "\n")
                done += 1
                if result.get("page_title"):
                    enriched += 1

            processed = min(i + batch_size, total)
            print(
                f"\r  AsyncFetcher: {processed}/{total} processed, "
                f"{enriched} enriched, {len(blocked)} blocked",
                end="",
                flush=True,
            )

    print()  # newline after progress
    return blocked


def run_stealth_pass(
    domains: list[str],
    concurrency: int,
    output_file: Path,
):
    """Run StealthyFetcher pass for blocked domains (sync, limited concurrency)."""
    total = len(domains)
    enriched = 0

    with open(output_file, "a") as f:
        for i, domain in enumerate(domains):
            result = fetch_with_stealth(domain)
            f.write(json.dumps(result, ensure_ascii=False) + "\n")

            if result.get("page_title"):
                enriched += 1

            if (i + 1) % 10 == 0 or (i + 1) == total:
                print(
                    f"\r  StealthyFetcher: {i + 1}/{total} processed, "
                    f"{enriched} enriched",
                    end="",
                    flush=True,
                )

    print()  # newline after progress


async def main():
    parser = argparse.ArgumentParser(description="Fetch page metadata using Scrapling")
    parser.add_argument("--input", "-i", required=True, help="Input file (one domain per line)")
    parser.add_argument("--output", "-o", required=True, help="Output JSONL file")
    parser.add_argument("--concurrency", "-c", type=int, default=20, help="AsyncFetcher concurrency (default: 20)")
    parser.add_argument("--stealth-concurrency", type=int, default=2, help="StealthyFetcher concurrency (default: 2)")
    parser.add_argument("--no-stealth", action="store_true", help="Skip StealthyFetcher fallback")
    parser.add_argument("--stealth-only", action="store_true", help="Only use StealthyFetcher (for retry)")
    parser.add_argument("--limit", type=int, default=0, help="Limit number of domains (0 = no limit)")
    args = parser.parse_args()

    # Load domains
    input_path = Path(args.input)
    if not input_path.exists():
        print(f"ERROR: Input file not found: {input_path}", file=sys.stderr)
        sys.exit(1)

    with open(input_path) as f:
        domains = [line.strip() for line in f if line.strip()]

    if args.limit > 0:
        domains = domains[: args.limit]

    print(f"Loaded {len(domains)} domains from {input_path}")

    output_path = Path(args.output)

    if args.stealth_only:
        # Stealth-only mode
        print(f"\n=== StealthyFetcher Pass ({len(domains)} domains) ===")
        run_stealth_pass(domains, args.stealth_concurrency, output_path)
    else:
        # Pass 1: AsyncFetcher
        print(f"\n=== Pass 1: AsyncFetcher ({len(domains)} domains, concurrency={args.concurrency}) ===")
        blocked = await run_async_pass(domains, args.concurrency, output_path)

        # Pass 2: StealthyFetcher for blocked domains
        if blocked and not args.no_stealth:
            print(f"\n=== Pass 2: StealthyFetcher ({len(blocked)} blocked domains) ===")
            run_stealth_pass(blocked, args.stealth_concurrency, output_path)
        elif blocked:
            print(f"\n{len(blocked)} domains were blocked (stealth pass skipped)")

    # Summary
    line_count = 0
    enriched_count = 0
    if output_path.exists():
        with open(output_path) as f:
            for line in f:
                line_count += 1
                try:
                    obj = json.loads(line)
                    if obj.get("page_title"):
                        enriched_count += 1
                except json.JSONDecodeError:
                    pass

    print(f"\n=== Summary ===")
    print(f"Total output lines: {line_count}")
    print(f"With page title:    {enriched_count}")
    print(f"Output file:        {output_path}")


if __name__ == "__main__":
    asyncio.run(main())
