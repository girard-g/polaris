use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

use crate::db::{Bm25Result, Database, SearchResult, SearchResultWithEmbedding};
use crate::embedding::EmbeddingEngine;
use crate::error::Result;


pub struct SearchEngine<'a> {
    embedding_engine: &'a EmbeddingEngine,
    db: &'a Database,
    mmr_lambda: f32,
    candidate_multiplier: usize,
    heading_boost: f32,
    rrf_k: usize,
}

impl<'a> SearchEngine<'a> {
    pub fn new(
        embedding_engine: &'a EmbeddingEngine,
        db: &'a Database,
        mmr_lambda: f32,
        candidate_multiplier: usize,
        heading_boost: f32,
        rrf_k: usize,
    ) -> Self {
        Self { embedding_engine, db, mmr_lambda, candidate_multiplier, heading_boost, rrf_k }
    }

    /// Search for the `top_k` most relevant chunks for `query`.
    ///
    /// Pipeline: vector KNN + BM25 → RRF fusion → heading boost → MMR rerank.
    pub fn search(&self, query: &str, top_k: usize) -> Result<Vec<SearchResult>> {
        let query_embedding = self.embedding_engine.embed_query(query)?;
        let candidate_count = top_k * self.candidate_multiplier;

        // 1. KNN vector retrieval (with embeddings for MMR).
        let vector_results =
            self.db.search_knn_with_embeddings(&query_embedding, candidate_count)?;

        // 2. BM25 retrieval (graceful degradation on syntax error or empty FTS table).
        let sanitized_query = sanitize_fts5_query(query);
        let bm25_results = self.db.search_bm25(&sanitized_query, candidate_count).unwrap_or_default();

        if vector_results.is_empty() && bm25_results.is_empty() {
            return Ok(vec![]);
        }

        // 3. RRF score fusion.
        let (rrf_scores, bm25_only_ids) =
            compute_rrf_scores(&vector_results, &bm25_results, self.rrf_k);

        // 4. Combine vector results with BM25-only results (fetch metadata + embedding).
        let mut all_results: Vec<SearchResultWithEmbedding> = vector_results;
        for chunk_id in bm25_only_ids {
            if let Ok(Some(r)) = self.db.get_chunk_with_metadata(chunk_id) {
                all_results.push(r);
            }
        }

        if all_results.is_empty() {
            return Ok(vec![]);
        }

        // 5. Apply heading boost on top of RRF scores.
        let query_lower = query.to_lowercase();
        let query_terms: Vec<&str> = query_lower
            .split_whitespace()
            .filter(|t| t.len() >= 3)
            .collect();

        let boosted: Vec<(f32, SearchResultWithEmbedding)> = all_results
            .into_iter()
            .map(|c| {
                let rrf_score = *rrf_scores.get(&c.chunk_id).unwrap_or(&0.0);
                let boost =
                    compute_heading_boost(&c.heading_context, &query_terms, self.heading_boost);
                (rrf_score + boost, c)
            })
            .collect();

        // 6. MMR reranking.
        let selected = mmr_rerank(boosted, top_k, self.mmr_lambda);

        // 7. Normalize scores to [0, 1] by dividing by the maximum score.
        //    The top result always gets 1.000; others are proportional.
        let max_score = selected.iter().map(|(s, _)| *s).fold(0.0_f32, f32::max);

        // 8. Store the normalised score in the `score` field for display.
        Ok(selected
            .into_iter()
            .map(|(s, mut c)| {
                c.score = if max_score > 0.0 { s / max_score } else { 0.0 };
                c.into_search_result()
            })
            .collect())
    }

    /// Format search results as a markdown string (for CLI / MCP output).
    pub fn format_results(results: &[SearchResult]) -> String {
        if results.is_empty() {
            return "No results found.".to_string();
        }

        let mut out = String::new();
        for (i, r) in results.iter().enumerate() {
            write!(out, "### Result {} — score: {:.3}\n", i + 1, r.score).unwrap();
            if !r.heading_context.is_empty() {
                write!(out, "**Section:** {}\n", r.heading_context).unwrap();
            }
            write!(out, "**File:** `{}`\n\n", r.file_path).unwrap();
            out.push_str(&r.content);
            out.push_str("\n\n---\n\n");
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Pure functions (testable without DB)
// ---------------------------------------------------------------------------

/// Sanitize a user query for safe use as an FTS5 query string.
///
/// Strips FTS5 operator characters and leading dashes to prevent syntax errors
/// when the user's query contains punctuation or special operators.
pub fn sanitize_fts5_query(query: &str) -> String {
    // Remove characters with special meaning in FTS5 query syntax.
    const FTS5_SPECIAL: &[char] = &['"', '(', ')', '*', '+', '^', ':', '{', '}', '~'];
    query
        .split_whitespace()
        .map(|token| {
            let t: String = token.chars().filter(|c| !FTS5_SPECIAL.contains(c)).collect();
            // Strip leading dashes (NOT operator in FTS5).
            t.trim_start_matches('-').to_string()
        })
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Dot product of two L2-normalised vectors = cosine similarity.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

/// Additive heading boost: fraction of non-trivial query terms that appear in
/// the lowercased heading context, scaled by `max_boost`.
pub fn compute_heading_boost(heading_context: &str, query_terms: &[&str], max_boost: f32) -> f32 {
    if query_terms.is_empty() || max_boost == 0.0 {
        return 0.0;
    }
    let heading_lower = heading_context.to_lowercase();
    let matching = query_terms
        .iter()
        .filter(|&&t| heading_lower.contains(t))
        .count();
    max_boost * (matching as f32 / query_terms.len() as f32)
}

/// Reciprocal Rank Fusion of vector and BM25 result lists.
///
/// Returns:
/// - `HashMap<chunk_id, rrf_score>` — combined RRF score for every chunk seen in either list.
/// - `Vec<chunk_id>` — chunks that appear only in the BM25 list (need metadata fetched).
///
/// Ranks are 1-based. Formula: `score(d) = 1/(k + rank_vector(d)) + 1/(k + rank_bm25(d))`.
pub fn compute_rrf_scores(
    vector_results: &[SearchResultWithEmbedding],
    bm25_results: &[Bm25Result],
    rrf_k: usize,
) -> (HashMap<i64, f32>, Vec<i64>) {
    let mut scores: HashMap<i64, f32> = HashMap::new();

    // Score from vector results (1-based rank).
    for (rank, r) in vector_results.iter().enumerate() {
        let score = 1.0 / (rrf_k as f32 + (rank + 1) as f32);
        *scores.entry(r.chunk_id).or_insert(0.0) += score;
    }

    // Score from BM25 results; track BM25-only chunk IDs.
    let vector_ids: HashSet<i64> = vector_results.iter().map(|r| r.chunk_id).collect();
    let mut bm25_only = Vec::new();

    for r in bm25_results {
        let score = 1.0 / (rrf_k as f32 + r.bm25_rank as f32);
        *scores.entry(r.chunk_id).or_insert(0.0) += score;
        if !vector_ids.contains(&r.chunk_id) {
            bm25_only.push(r.chunk_id);
        }
    }

    (scores, bm25_only)
}

/// Greedy MMR selection.
///
/// `candidates` — (boosted_relevance, result) pairs, any order.
/// Returns `top_k` items ordered by selection order (best first).
pub fn mmr_rerank(
    candidates: Vec<(f32, SearchResultWithEmbedding)>,
    top_k: usize,
    lambda: f32,
) -> Vec<(f32, SearchResultWithEmbedding)> {
    let n = candidates.len().min(top_k);
    if n == 0 {
        return vec![];
    }

    let mut remaining: Vec<(f32, SearchResultWithEmbedding)> = candidates;
    let mut selected: Vec<(f32, SearchResultWithEmbedding)> = Vec::with_capacity(n);

    for _ in 0..n {
        // For each remaining candidate compute its MMR score.
        let best_idx = remaining
            .iter()
            .enumerate()
            .map(|(i, (rel, cand))| {
                let max_sim = selected
                    .iter()
                    .map(|(_, s)| cosine_similarity(&cand.embedding, &s.embedding))
                    .fold(0.0_f32, f32::max);
                let mmr_score = lambda * rel - (1.0 - lambda) * max_sim;
                (i, mmr_score)
            })
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(i, _)| i)
            .unwrap_or(0);

        let item = remaining.remove(best_idx);
        selected.push(item);
    }

    selected
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::SearchResult;

    fn make_result(
        content: &str,
        heading_context: &str,
        file_path: &str,
        score: f32,
    ) -> SearchResult {
        SearchResult {
            chunk_id: 1,
            content: content.to_string(),
            heading_context: heading_context.to_string(),
            file_path: file_path.to_string(),
            score,
            source_db: None,
        }
    }

    fn make_candidate(score: f32, embedding: Vec<f32>) -> SearchResultWithEmbedding {
        SearchResultWithEmbedding {
            chunk_id: 1,
            content: String::new(),
            heading_context: String::new(),
            file_path: String::new(),
            score,
            embedding,
        }
    }

    // -----------------------------------------------------------------------
    // format_results
    // -----------------------------------------------------------------------

    #[test]
    fn format_results_empty() {
        let output = SearchEngine::format_results(&[]);
        assert_eq!(output, "No results found.");
    }

    #[test]
    fn format_results_single_result_contains_expected_fields() {
        let results = vec![make_result("Content here.", "My Section", "docs/file.md", 0.2)];
        let output = SearchEngine::format_results(&results);
        assert!(output.contains("score:"), "should contain score");
        assert!(output.contains("My Section"), "should contain heading");
        assert!(output.contains("docs/file.md"), "should contain file path");
        assert!(output.contains("Content here."), "should contain content");
    }

    #[test]
    fn format_results_multiple_results_numbered_and_separated() {
        let results = vec![
            make_result("First content.", "Section A", "a.md", 0.1),
            make_result("Second content.", "Section B", "b.md", 0.3),
        ];
        let output = SearchEngine::format_results(&results);
        assert!(output.contains("Result 1"), "should contain Result 1");
        assert!(output.contains("Result 2"), "should contain Result 2");
        assert!(output.contains("---"), "should contain separator");
    }

    #[test]
    fn format_results_empty_heading_context_no_section_line() {
        let results = vec![make_result("Some content.", "", "file.md", 0.5)];
        let output = SearchEngine::format_results(&results);
        assert!(!output.contains("**Section:**"), "empty heading should not produce Section line");
        assert!(output.contains("file.md"), "file path should still appear");
    }

    // -----------------------------------------------------------------------
    // cosine_similarity
    // -----------------------------------------------------------------------

    #[test]
    fn cosine_similarity_identical_unit_vectors() {
        let a = vec![1.0_f32, 0.0, 0.0];
        assert!((cosine_similarity(&a, &a) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_similarity_orthogonal_vectors() {
        let a = vec![1.0_f32, 0.0];
        let b = vec![0.0_f32, 1.0];
        assert!(cosine_similarity(&a, &b).abs() < 1e-6);
    }

    // -----------------------------------------------------------------------
    // compute_heading_boost
    // -----------------------------------------------------------------------

    #[test]
    fn heading_boost_all_terms_match() {
        let terms = vec!["index", "documents"];
        let boost = compute_heading_boost("How to Index Documents", &terms, 0.1);
        assert!((boost - 0.1).abs() < 1e-6, "all terms match → max boost");
    }

    #[test]
    fn heading_boost_partial_match() {
        let terms = vec!["index", "documents"];
        let boost = compute_heading_boost("How to Index Files", &terms, 0.1);
        // "index" matches, "documents" does not → 0.5 * 0.1 = 0.05
        assert!((boost - 0.05).abs() < 1e-6);
    }

    #[test]
    fn heading_boost_no_match() {
        let terms = vec!["index", "documents"];
        let boost = compute_heading_boost("Unrelated Heading", &terms, 0.1);
        assert!(boost.abs() < 1e-6);
    }

    #[test]
    fn heading_boost_zero_max_returns_zero() {
        let terms = vec!["index"];
        let boost = compute_heading_boost("Index", &terms, 0.0);
        assert!(boost.abs() < 1e-6);
    }

    #[test]
    fn heading_boost_empty_terms_returns_zero() {
        let boost = compute_heading_boost("Some Heading", &[], 0.1);
        assert!(boost.abs() < 1e-6);
    }

    // -----------------------------------------------------------------------
    // mmr_rerank
    // -----------------------------------------------------------------------

    #[test]
    fn mmr_rerank_empty_candidates() {
        let result = mmr_rerank(vec![], 5, 0.7);
        assert!(result.is_empty());
    }

    #[test]
    fn mmr_rerank_fewer_candidates_than_top_k() {
        let candidates = vec![
            (0.9_f32, make_candidate(0.1, vec![1.0, 0.0])),
            (0.7_f32, make_candidate(0.3, vec![0.0, 1.0])),
        ];
        let result = mmr_rerank(candidates, 5, 0.7);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn mmr_rerank_selects_top_k() {
        let candidates: Vec<_> = (0..6)
            .map(|i| {
                // Each candidate orthogonal to others (distinct embedding directions)
                let mut emb = vec![0.0_f32; 6];
                emb[i] = 1.0;
                (1.0 - i as f32 * 0.1, make_candidate(i as f32 * 0.1, emb))
            })
            .collect();
        let result = mmr_rerank(candidates, 3, 0.7);
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn mmr_rerank_lambda_one_picks_by_relevance() {
        // With lambda=1.0, MMR degenerates to pure relevance → first pick is highest relevance.
        let candidates = vec![
            (0.6_f32, make_candidate(0.4, vec![1.0, 0.0])),
            (0.9_f32, make_candidate(0.1, vec![0.9, 0.44])), // highest relevance
            (0.3_f32, make_candidate(0.7, vec![0.0, 1.0])),
        ];
        let result = mmr_rerank(candidates, 1, 1.0);
        assert_eq!(result.len(), 1);
        // The chosen item should be the one with relevance 0.9
        assert!((result[0].0 - 0.9).abs() < 1e-5);
    }

    // -----------------------------------------------------------------------
    // compute_rrf_scores
    // -----------------------------------------------------------------------

    fn make_vec_result(chunk_id: i64) -> SearchResultWithEmbedding {
        SearchResultWithEmbedding {
            chunk_id,
            content: String::new(),
            heading_context: String::new(),
            file_path: String::new(),
            score: 0.0,
            embedding: vec![],
        }
    }

    fn make_bm25_result(chunk_id: i64, bm25_rank: usize) -> Bm25Result {
        Bm25Result { chunk_id, bm25_rank }
    }

    #[test]
    fn rrf_vector_only_no_bm25() {
        let vec_results = vec![make_vec_result(1), make_vec_result(2)];
        let (scores, bm25_only) = compute_rrf_scores(&vec_results, &[], 60);

        // Both chunks scored from vector rank only.
        assert!(scores.contains_key(&1));
        assert!(scores.contains_key(&2));
        // Rank 1 gets higher score than rank 2.
        assert!(scores[&1] > scores[&2]);
        // No BM25-only chunks.
        assert!(bm25_only.is_empty());
    }

    #[test]
    fn rrf_overlap_combines_scores() {
        // Chunk 10 appears in both lists; chunk 11 only in BM25.
        let vec_results = vec![make_vec_result(10)];
        let bm25_results = vec![make_bm25_result(10, 1), make_bm25_result(11, 2)];
        let (scores, bm25_only) = compute_rrf_scores(&vec_results, &bm25_results, 60);

        // Chunk 10 has contributions from both lists.
        let expected_10 = 1.0_f32 / (60.0 + 1.0) + 1.0_f32 / (60.0 + 1.0);
        assert!((scores[&10] - expected_10).abs() < 1e-6);

        // Chunk 11 only in BM25.
        let expected_11 = 1.0_f32 / (60.0 + 2.0);
        assert!((scores[&11] - expected_11).abs() < 1e-6);

        // bm25_only should contain only chunk 11.
        assert_eq!(bm25_only, vec![11]);
    }

    #[test]
    fn rrf_bm25_only_empty_vector() {
        let bm25_results = vec![make_bm25_result(5, 1), make_bm25_result(6, 2)];
        let (scores, bm25_only) = compute_rrf_scores(&[], &bm25_results, 60);

        assert_eq!(scores.len(), 2);
        assert_eq!(bm25_only.len(), 2);
        assert!(bm25_only.contains(&5));
        assert!(bm25_only.contains(&6));
    }

    // -----------------------------------------------------------------------
    // sanitize_fts5_query (Phase 4b)
    // -----------------------------------------------------------------------

    #[test]
    fn sanitize_plain_query_unchanged() {
        assert_eq!(sanitize_fts5_query("rust programming"), "rust programming");
    }

    #[test]
    fn sanitize_strips_fts5_operators() {
        // Quotes, parens, wildcards, etc. are removed.
        assert_eq!(sanitize_fts5_query("\"hello world\""), "hello world");
        assert_eq!(sanitize_fts5_query("foo* AND bar"), "foo AND bar");
        assert_eq!(sanitize_fts5_query("(one OR two)"), "one OR two");
    }

    #[test]
    fn sanitize_strips_leading_dashes() {
        assert_eq!(sanitize_fts5_query("-bad token"), "bad token");
        assert_eq!(sanitize_fts5_query("---triple"), "triple");
    }

    #[test]
    fn sanitize_empty_query_returns_empty() {
        assert_eq!(sanitize_fts5_query(""), "");
        assert_eq!(sanitize_fts5_query("   "), "");
    }

    #[test]
    fn sanitize_all_special_chars_returns_empty() {
        assert_eq!(sanitize_fts5_query("\"\"()"), "");
    }
}
