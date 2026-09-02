//! Self-discovering retrieval evaluation.
//!
//! Ground truth comes from the corpus: a sentence lifted from a chunk is a
//! query whose correct answer is the chunk it came from. See
//! `docs/superpowers/specs/2026-09-02-polaris-eval-design.md`.

use sha2::{Digest, Sha256};

/// One corpus sentence selected as an eval query, with the chunk it came from.
#[derive(Debug, Clone, PartialEq)]
pub struct SampledSentence {
    pub chunk_id: i64,
    pub file_path: String,
    pub text: String,
}

/// Shortest and longest sentence worth using as a query. Under four words
/// carries too little signal; over forty is a paragraph that never resembles
/// a question.
const MIN_SENTENCE_WORDS: usize = 4;
const MAX_SENTENCE_WORDS: usize = 40;

/// Split chunk body text into candidate query sentences.
///
/// Markdown chunks carry code fences and table rows, which are not prose and
/// make nonsense queries. They are dropped rather than cleaned.
pub(crate) fn split_sentences(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_fence = false;

    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence || trimmed.starts_with('|') || trimmed.starts_with('#') {
            continue;
        }

        for raw in trimmed.split_inclusive(['.', '!', '?']) {
            let candidate = raw.trim().trim_end_matches(['.', '!', '?']).trim();
            let words = candidate.split_whitespace().count();
            if words >= MIN_SENTENCE_WORDS && words <= MAX_SENTENCE_WORDS {
                out.push(candidate.to_string());
            }
        }
    }
    out
}

/// Pick `n` sentences deterministically by content hash.
///
/// Selection is content-addressed so the same corpus yields the same questions
/// on every run with nothing stored, and so re-chunking preserves the sample —
/// only the chunk a sentence belongs to changes, which is the thing being
/// measured. Ties break on text so the order is total.
pub(crate) fn select_sample(
    mut candidates: Vec<SampledSentence>,
    n: usize,
) -> Vec<SampledSentence> {
    candidates.sort_by(|a, b| {
        sentence_key(&a.text)
            .cmp(&sentence_key(&b.text))
            .then_with(|| a.text.cmp(&b.text))
    });
    candidates.truncate(n);
    candidates
}

fn sentence_key(text: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    hasher.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sent(id: i64, text: &str) -> SampledSentence {
        SampledSentence { chunk_id: id, file_path: "d.md".to_string(), text: text.to_string() }
    }

    #[test]
    fn splits_on_terminators() {
        let got = split_sentences(
            "The index stores every chunk with its heading. \
             Retrieval fuses two ranked lists into one order. \
             Does it work well enough in practice?",
        );
        assert_eq!(got.len(), 3);
        assert!(got[0].starts_with("The index stores"));
    }

    #[test]
    fn drops_too_short_and_too_long() {
        let long = format!("{} end.", "word ".repeat(60));
        let got = split_sentences(&format!("Yes. Only four words here. {long}"));
        assert_eq!(got, vec!["Only four words here".to_string()]);
    }

    #[test]
    fn drops_code_fences_and_tables() {
        let got = split_sentences(
            "```rust\nlet x = compute_everything_here(a, b);\n```\n\
             | col | col |\n\
             This sentence is perfectly ordinary prose to keep.",
        );
        assert_eq!(got.len(), 1);
        assert!(got[0].contains("perfectly ordinary prose"));
    }

    #[test]
    fn sample_is_deterministic() {
        let pool: Vec<SampledSentence> =
            (0..50).map(|i| sent(i, &format!("sentence number {i} of the corpus body"))).collect();
        let a = select_sample(pool.clone(), 10);
        let b = select_sample(pool, 10);
        assert_eq!(a, b);
        assert_eq!(a.len(), 10);
    }

    #[test]
    fn sample_survives_rechunking() {
        // Same sentences, different chunk_ids and grouping — the selected
        // sentence *texts* must not change, only which chunk they belong to.
        let before: Vec<SampledSentence> =
            (0..50).map(|i| sent(i, &format!("sentence number {i} of the corpus body"))).collect();
        let after: Vec<SampledSentence> = (0..50)
            .map(|i| sent(i + 900, &format!("sentence number {i} of the corpus body")))
            .collect();

        let texts = |v: Vec<SampledSentence>| -> Vec<String> {
            v.into_iter().map(|s| s.text).collect()
        };
        assert_eq!(texts(select_sample(before, 10)), texts(select_sample(after, 10)));
    }

    #[test]
    fn sample_returns_everything_when_pool_is_small() {
        let pool: Vec<SampledSentence> =
            (0..3).map(|i| sent(i, &format!("sentence number {i} of the corpus body"))).collect();
        assert_eq!(select_sample(pool, 10).len(), 3);
    }
}
