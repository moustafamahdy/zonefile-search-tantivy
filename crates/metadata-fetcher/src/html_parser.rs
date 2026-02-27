use scraper::{Html, Selector};
use std::sync::LazyLock;

/// Pre-compiled CSS selectors (compiled once, reused across all domains)
static SEL_TITLE: LazyLock<Selector> = LazyLock::new(|| Selector::parse("title").unwrap());
static SEL_META_DESC: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse(r#"meta[name="description"]"#).unwrap());
static SEL_META_DESC_PROP: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse(r#"meta[property="description"]"#).unwrap());
static SEL_OG_TITLE: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse(r#"meta[property="og:title"]"#).unwrap());
static SEL_OG_DESC: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse(r#"meta[property="og:description"]"#).unwrap());
static SEL_OG_IMAGE: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse(r#"meta[property="og:image"]"#).unwrap());
static SEL_CANONICAL: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse(r#"link[rel="canonical"]"#).unwrap());
static SEL_GENERATOR: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse(r#"meta[name="generator"]"#).unwrap());
static SEL_HTML: LazyLock<Selector> = LazyLock::new(|| Selector::parse("html").unwrap());
static SEL_BODY: LazyLock<Selector> = LazyLock::new(|| Selector::parse("body").unwrap());

/// Parsed page metadata
pub struct ParsedPageMeta {
    pub title: Option<String>,
    pub meta_description: Option<String>,
    pub og_title: Option<String>,
    pub og_description: Option<String>,
    pub og_image: Option<String>,
    pub canonical: Option<String>,
    pub language: Option<String>,
    pub generator: Option<String>,
    pub snippet: Option<String>,
}

/// Parse HTML and extract metadata from <head> and a text snippet from <body>.
pub fn parse_html_metadata(html: &str) -> ParsedPageMeta {
    let document = Html::parse_document(html);

    // <title>
    let title = document
        .select(&SEL_TITLE)
        .next()
        .map(|el| normalize_text(&el.text().collect::<String>()))
        .filter(|s| !s.is_empty())
        .map(|s| truncate(&s, 500));

    // <meta name="description"> or <meta property="description">
    let meta_description = get_meta_content(&document, &SEL_META_DESC)
        .or_else(|| get_meta_content(&document, &SEL_META_DESC_PROP))
        .map(|s| truncate(&s, 1000));

    // OG tags
    let og_title = get_meta_content(&document, &SEL_OG_TITLE).map(|s| truncate(&s, 500));
    let og_description = get_meta_content(&document, &SEL_OG_DESC).map(|s| truncate(&s, 1000));
    let og_image = get_meta_content(&document, &SEL_OG_IMAGE).map(|s| truncate(&s, 2000));

    // <link rel="canonical">
    let canonical = document
        .select(&SEL_CANONICAL)
        .next()
        .and_then(|el| el.value().attr("href"))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .map(|s| truncate(&s, 2000));

    // <html lang="...">
    let language = document
        .select(&SEL_HTML)
        .next()
        .and_then(|el| el.value().attr("lang"))
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .map(|s| truncate(&s, 10));

    // <meta name="generator">
    let generator = get_meta_content(&document, &SEL_GENERATOR).map(|s| truncate(&s, 200));

    // Body text snippet — first 300 chars of visible text
    let snippet = document
        .select(&SEL_BODY)
        .next()
        .map(|body| {
            let text: String = body.text().collect();
            let normalized = normalize_text(&text);
            truncate(&normalized, 300)
        })
        .filter(|s| !s.is_empty());

    ParsedPageMeta {
        title,
        meta_description,
        og_title,
        og_description,
        og_image,
        canonical,
        language,
        generator,
        snippet,
    }
}

/// Extract content attribute from a meta tag
fn get_meta_content(document: &Html, selector: &Selector) -> Option<String> {
    document
        .select(selector)
        .next()
        .and_then(|el| el.value().attr("content"))
        .map(|s| normalize_text(s))
        .filter(|s| !s.is_empty())
}

/// Collapse whitespace and trim
fn normalize_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Truncate a string to max_chars (at a char boundary)
fn truncate(s: &str, max_chars: usize) -> String {
    if s.len() <= max_chars {
        return s.to_string();
    }
    // Find char boundary
    let mut end = max_chars;
    while !s.is_char_boundary(end) && end > 0 {
        end -= 1;
    }
    s[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_html() {
        let html = r#"
        <html lang="en">
        <head>
            <title>Example Site</title>
            <meta name="description" content="This is a test site.">
            <meta property="og:title" content="OG Title">
            <meta property="og:description" content="OG Description">
            <meta property="og:image" content="https://example.com/image.png">
            <link rel="canonical" href="https://example.com/">
            <meta name="generator" content="WordPress 6.0">
        </head>
        <body>
            <p>Hello world, this is the body text content.</p>
        </body>
        </html>
        "#;

        let meta = parse_html_metadata(html);
        assert_eq!(meta.title.as_deref(), Some("Example Site"));
        assert_eq!(meta.meta_description.as_deref(), Some("This is a test site."));
        assert_eq!(meta.og_title.as_deref(), Some("OG Title"));
        assert_eq!(meta.og_description.as_deref(), Some("OG Description"));
        assert_eq!(meta.og_image.as_deref(), Some("https://example.com/image.png"));
        assert_eq!(meta.canonical.as_deref(), Some("https://example.com/"));
        assert_eq!(meta.language.as_deref(), Some("en"));
        assert_eq!(meta.generator.as_deref(), Some("WordPress 6.0"));
        assert!(meta.snippet.unwrap().contains("Hello world"));
    }

    #[test]
    fn test_empty_html() {
        let meta = parse_html_metadata("");
        assert!(meta.title.is_none());
        assert!(meta.meta_description.is_none());
        assert!(meta.og_title.is_none());
    }

    #[test]
    fn test_whitespace_normalization() {
        let html = r#"<html><head><title>  Multiple   Spaces   Here  </title></head></html>"#;
        let meta = parse_html_metadata(html);
        assert_eq!(meta.title.as_deref(), Some("Multiple Spaces Here"));
    }
}
