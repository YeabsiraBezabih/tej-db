#![allow(clippy::cast_precision_loss)]

use std::collections::HashMap;

/// Inverted full-text search index utilizing Okapi BM25 ranking for hybrid lexical + vector search.
#[derive(Debug, Clone, Default)]
pub struct Bm25Index {
    /// Mapping: term -> HashMap<doc_id, term_frequency>
    inverted_index: HashMap<String, HashMap<u64, u32>>,
    /// Document length (token count) per document ID
    doc_lengths: HashMap<u64, usize>,
    /// Total number of tokens across all indexed documents
    total_tokens: usize,
    /// Okapi BM25 term frequency saturation parameter (default: 1.2)
    k1: f32,
    /// Okapi BM25 document length normalization parameter (default: 0.75)
    b: f32,
}

impl Bm25Index {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inverted_index: HashMap::new(),
            doc_lengths: HashMap::new(),
            total_tokens: 0,
            k1: 1.2,
            b: 0.75,
        }
    }

    /// Tokenizes input text into lowercase alphanumeric tokens.
    #[must_use]
    pub fn tokenize(text: &str) -> Vec<String> {
        text.split(|c: char| !c.is_alphanumeric())
            .filter(|s| !s.is_empty())
            .map(str::to_lowercase)
            .collect()
    }

    /// Indexes a document's text payload with its document ID.
    pub fn add_document(&mut self, doc_id: u64, text: &str) {
        let tokens = Self::tokenize(text);
        let len = tokens.len();
        self.doc_lengths.insert(doc_id, len);
        self.total_tokens += len;

        for token in tokens {
            let entry = self.inverted_index.entry(token).or_default();
            *entry.entry(doc_id).or_insert(0) += 1;
        }
    }

    /// Computes BM25 score for a document against query tokens.
    #[must_use]
    pub fn score_query(&self, query: &str, top_k: usize) -> Vec<(u64, f32)> {
        let tokens = Self::tokenize(query);
        if tokens.is_empty() || self.doc_lengths.is_empty() {
            return Vec::new();
        }

        let num_docs = self.doc_lengths.len() as f32;
        let avg_dl = (self.total_tokens as f32) / num_docs;

        let mut doc_scores: HashMap<u64, f32> = HashMap::new();

        for term in tokens {
            if let Some(postings) = self.inverted_index.get(&term) {
                let df = postings.len() as f32;
                // Standard Okapi BM25 Inverse Document Frequency
                let idf = ((num_docs - df + 0.5) / (df + 0.5) + 1.0).ln();

                for (&doc_id, &tf) in postings {
                    let dl = *self.doc_lengths.get(&doc_id).unwrap_or(&0) as f32;
                    let tf_f = tf as f32;
                    let numerator = tf_f * (self.k1 + 1.0);
                    let denominator = tf_f + self.k1 * (1.0 - self.b + self.b * (dl / avg_dl));
                    let term_score = idf * (numerator / denominator);
                    *doc_scores.entry(doc_id).or_insert(0.0) += term_score;
                }
            }
        }

        let mut results: Vec<(u64, f32)> = doc_scores.into_iter().collect();
        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(top_k);
        results
    }

    /// Hybrid lexical (BM25) + dense vector ranking via Reciprocal Rank Fusion (RRF).
    #[must_use]
    pub fn hybrid_rrf(
        bm25_results: &[(u64, f32)],
        vector_results: &[(u64, f32)],
        k: f32,
        top_k: usize,
    ) -> Vec<(u64, f32)> {
        let mut rrf_scores: HashMap<u64, f32> = HashMap::new();

        for (rank, &(doc_id, _)) in bm25_results.iter().enumerate() {
            let score = 1.0 / (k + (rank as f32) + 1.0);
            *rrf_scores.entry(doc_id).or_insert(0.0) += score;
        }

        for (rank, &(doc_id, _)) in vector_results.iter().enumerate() {
            let score = 1.0 / (k + (rank as f32) + 1.0);
            *rrf_scores.entry(doc_id).or_insert(0.0) += score;
        }

        let mut results: Vec<(u64, f32)> = rrf_scores.into_iter().collect();
        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(top_k);
        results
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bm25_text_ranking() {
        let mut fts = Bm25Index::new();
        fts.add_document(1, "The quick brown fox jumps over the lazy dog");
        fts.add_document(2, "Brown foxes are wild animals in the forest");
        fts.add_document(
            3,
            "Databases use B-Trees and vectorized engines for high performance",
        );

        let res = fts.score_query("brown fox", 2);
        assert_eq!(res.len(), 2);
        assert_eq!(res[0].0, 1); // Document 1 contains both terms
    }

    #[test]
    fn test_hybrid_rrf_fusion() {
        let bm25 = vec![(10, 2.5), (20, 1.8), (30, 0.9)];
        let vec_scores = vec![(10, 0.95), (20, 0.85), (40, 0.75)];

        let fused = Bm25Index::hybrid_rrf(&bm25, &vec_scores, 60.0, 3);
        assert_eq!(fused[0].0, 10); // Ranked #1 in both BM25 and Vector -> Top hybrid result
    }
}
