use crate::routes::exact::DomainResult;

/// A search result with ranking information
pub struct RankedResult {
    pub domain: DomainResult,
    pub match_count: usize,
    pub bm25_score: f32,
}

impl RankedResult {
    /// Calculate a combined score for ranking
    ///
    /// Priority order:
    /// 1. match_count (higher is better)
    /// 2. domain length (shorter is better)
    /// 3. BM25 score (higher is better)
    pub fn combined_score(&self) -> f64 {
        // Normalize match_count to 0-1 range (assuming max 10 keywords)
        let match_score = (self.match_count as f64) / 10.0;

        // Normalize length to 0-1 range (shorter is better, max 63 chars)
        let length_score = 1.0 - (self.domain.length as f64 / 63.0);

        // Normalize BM25 (typically 0-20 range)
        let bm25_normalized = (self.bm25_score as f64).min(20.0) / 20.0;

        // Weighted combination
        match_score * 100.0 + length_score * 10.0 + bm25_normalized
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_result(match_count: usize, length: u64, bm25: f32) -> RankedResult {
        RankedResult {
            domain: DomainResult {
                domain: "test.com".to_string(),
                label: "test".to_string(),
                tld: "com".to_string(),
                length,
                has_hyphen: false,
                tokens: vec![],
            },
            match_count,
            bm25_score: bm25,
        }
    }

    #[test]
    fn test_ranking_prefers_more_matches() {
        let r1 = make_result(3, 10, 5.0);
        let r2 = make_result(2, 10, 5.0);

        assert!(r1.combined_score() > r2.combined_score());
    }

    #[test]
    fn test_ranking_prefers_shorter_domains() {
        let r1 = make_result(2, 5, 5.0);
        let r2 = make_result(2, 20, 5.0);

        assert!(r1.combined_score() > r2.combined_score());
    }

    #[test]
    fn test_ranking_match_count_dominates() {
        // More matches should beat shorter domain
        let r1 = make_result(3, 20, 5.0);
        let r2 = make_result(2, 5, 5.0);

        assert!(r1.combined_score() > r2.combined_score());
    }
}
