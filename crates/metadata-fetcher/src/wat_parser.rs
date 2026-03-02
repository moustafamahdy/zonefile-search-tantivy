use crate::model::PageMetadataResult;
use serde_json::Value;

/// Parse a WAT JSON record into PageMetadataResult.
///
/// WAT JSON structure:
/// ```json
/// {
///   "Envelope": {
///     "Payload-Metadata": {
///       "HTTP-Response-Metadata": {
///         "HTML-Metadata": {
///           "Head": {
///             "Title": "Page Title",
///             "Metas": [
///               {"name": "description", "content": "..."},
///               {"property": "og:title", "content": "..."},
///               {"property": "og:description", "content": "..."},
///               {"property": "og:image", "content": "..."},
///               {"http-equiv": "content-language", "content": "en"}
///             ],
///             "Link": [
///               {"rel": "canonical", "href": "https://..."}
///             ]
///           }
///         },
///         "Headers": {
///           "Content-Type": "text/html; charset=utf-8"
///         }
///       }
///     }
///   }
/// }
/// ```
pub fn parse_wat_metadata(domain: &str, wat_json: &[u8]) -> PageMetadataResult {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

    let mut result = PageMetadataResult {
        domain: domain.to_string(),
        http_status: 200,
        page_title: None,
        meta_description: None,
        og_title: None,
        og_description: None,
        og_image: None,
        canonical_url: None,
        language: None,
        generator: None,
        snippet: None,
        content_type: None,
        fetched_at: now,
        source: "commoncrawl".to_string(),
        error: None,
    };

    let parsed: Value = match serde_json::from_slice(wat_json) {
        Ok(v) => v,
        Err(e) => {
            result.error = Some(format!("WAT JSON parse error: {}", e));
            return result;
        }
    };

    // Navigate to HTML-Metadata
    let html_meta = &parsed["Envelope"]["Payload-Metadata"]["HTTP-Response-Metadata"]["HTML-Metadata"];
    if html_meta.is_null() {
        result.error = Some("No HTML-Metadata in WAT record".to_string());
        return result;
    }

    let head = &html_meta["Head"];

    // Title
    if let Some(title) = head["Title"].as_str() {
        let title = title.trim();
        if !title.is_empty() {
            result.page_title = Some(truncate(title, 500));
        }
    }

    // Metas array
    if let Some(metas) = head["Metas"].as_array() {
        for meta in metas {
            let name = meta["name"].as_str().unwrap_or("").to_lowercase();
            let property = meta["property"].as_str().unwrap_or("").to_lowercase();
            let http_equiv = meta["http-equiv"].as_str().unwrap_or("").to_lowercase();
            let content = meta["content"].as_str().unwrap_or("").trim();

            if content.is_empty() {
                continue;
            }

            if name == "description" && result.meta_description.is_none() {
                result.meta_description = Some(truncate(content, 1000));
            } else if property == "og:title" && result.og_title.is_none() {
                result.og_title = Some(truncate(content, 500));
            } else if property == "og:description" && result.og_description.is_none() {
                result.og_description = Some(truncate(content, 1000));
            } else if property == "og:image" && result.og_image.is_none() {
                result.og_image = Some(truncate(content, 2000));
            } else if name == "generator" && result.generator.is_none() {
                result.generator = Some(truncate(content, 200));
            } else if (http_equiv == "content-language" || name == "language")
                && result.language.is_none()
            {
                // Extract primary language code (e.g., "en" from "en-US")
                let lang = content.split(['-', '_']).next().unwrap_or(content);
                result.language = Some(lang.to_lowercase());
            }
        }
    }

    // Links array (canonical)
    if let Some(links) = head["Link"].as_array() {
        for link in links {
            let rel = link["rel"].as_str().unwrap_or("").to_lowercase();
            if rel == "canonical" {
                if let Some(href) = link["href"].as_str() {
                    let href = href.trim();
                    if !href.is_empty() {
                        result.canonical_url = Some(truncate(href, 2000));
                        break;
                    }
                }
            }
        }
    }

    // Language from html tag attributes (if present)
    if result.language.is_none() {
        if let Some(lang) = html_meta["Head"]["Lang"].as_str() {
            let lang = lang.split(['-', '_']).next().unwrap_or(lang).trim();
            if !lang.is_empty() {
                result.language = Some(lang.to_lowercase());
            }
        }
    }

    // Content-Type from HTTP headers
    let headers = &parsed["Envelope"]["Payload-Metadata"]["HTTP-Response-Metadata"]["Headers"];
    if let Some(ct) = headers["Content-Type"].as_str() {
        result.content_type = Some(ct.to_string());
    }

    // Build snippet from description or title
    if result.meta_description.is_some() {
        result.snippet = result.meta_description.clone();
    } else if result.og_description.is_some() {
        result.snippet = result.og_description.clone();
    }

    result
}

fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        s[..max_len].to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_wat_basic() {
        let wat = serde_json::json!({
            "Envelope": {
                "Payload-Metadata": {
                    "HTTP-Response-Metadata": {
                        "HTML-Metadata": {
                            "Head": {
                                "Title": "Indoor Farming Lights - Best LED Grow Lights",
                                "Metas": [
                                    {"name": "description", "content": "Shop the best indoor farming LED lights for your grow operation."},
                                    {"property": "og:title", "content": "Indoor Farming Lights"},
                                    {"property": "og:image", "content": "https://example.com/img.jpg"},
                                    {"name": "generator", "content": "WordPress 6.4"},
                                    {"http-equiv": "content-language", "content": "en-US"}
                                ],
                                "Link": [
                                    {"rel": "canonical", "href": "https://indoorfarminglights.com/"}
                                ]
                            }
                        },
                        "Headers": {
                            "Content-Type": "text/html; charset=utf-8"
                        }
                    }
                }
            }
        });

        let result = parse_wat_metadata("indoorfarminglights.com", wat.to_string().as_bytes());

        assert_eq!(result.page_title.as_deref(), Some("Indoor Farming Lights - Best LED Grow Lights"));
        assert_eq!(result.meta_description.as_deref(), Some("Shop the best indoor farming LED lights for your grow operation."));
        assert_eq!(result.og_title.as_deref(), Some("Indoor Farming Lights"));
        assert_eq!(result.og_image.as_deref(), Some("https://example.com/img.jpg"));
        assert_eq!(result.generator.as_deref(), Some("WordPress 6.4"));
        assert_eq!(result.language.as_deref(), Some("en"));
        assert_eq!(result.canonical_url.as_deref(), Some("https://indoorfarminglights.com/"));
        assert_eq!(result.source, "commoncrawl");
        assert!(result.error.is_none());
        assert!(result.snippet.is_some());
    }

    #[test]
    fn test_parse_wat_no_metadata() {
        let wat = serde_json::json!({
            "Envelope": {
                "Payload-Metadata": {
                    "HTTP-Response-Metadata": {}
                }
            }
        });

        let result = parse_wat_metadata("example.com", wat.to_string().as_bytes());
        assert!(result.error.is_some());
        assert!(result.page_title.is_none());
    }
}
