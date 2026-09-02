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

/// Shortest and longest sentence worth using as a query. Under six words
/// carries too little signal; over forty is a paragraph that never resembles
/// a question.
const MIN_SENTENCE_WORDS: usize = 6;
const MAX_SENTENCE_WORDS: usize = 40;

/// Split chunk body text into candidate query sentences.
///
/// Markdown chunks carry code fences and table rows, which are not prose and
/// make nonsense queries. They are dropped rather than cleaned.
///
/// The indexer inserts '\n' at soft breaks, so wrapped prose arrives as multiple
/// lines of one paragraph. Lines are buffered and flushed into a single paragraph
/// text before splitting on sentence terminators.
pub(crate) fn split_sentences(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_fence = false;
    let mut buffer = String::new();

    for line in body.lines() {
        let trimmed = line.trim();

        // Fence toggle and skip
        if trimmed.starts_with("```") {
            if !buffer.is_empty() {
                flush_paragraph(&mut buffer, &mut out);
            }
            in_fence = !in_fence;
            continue;
        }

        // Skip tables, headings, and content inside fences
        if in_fence || trimmed.starts_with('|') || trimmed.starts_with('#') {
            if !buffer.is_empty() {
                flush_paragraph(&mut buffer, &mut out);
            }
            continue;
        }

        // Blank line flushes the buffer
        if trimmed.is_empty() {
            if !buffer.is_empty() {
                flush_paragraph(&mut buffer, &mut out);
            }
            continue;
        }

        // Accumulate line into paragraph buffer
        if !buffer.is_empty() {
            buffer.push(' ');
        }
        buffer.push_str(trimmed);
    }

    // Flush any remaining buffer at end of input
    if !buffer.is_empty() {
        flush_paragraph(&mut buffer, &mut out);
    }

    out
}

fn flush_paragraph(buffer: &mut String, out: &mut Vec<String>) {
    for raw in buffer.split_inclusive(['.', '!', '?']) {
        let candidate = raw.trim().trim_end_matches(['.', '!', '?']).trim();
        let words = candidate.split_whitespace().count();
        if words >= MIN_SENTENCE_WORDS && words <= MAX_SENTENCE_WORDS {
            out.push(candidate.to_string());
        }
    }
    buffer.clear();
}

/// Pick `n` sentences deterministically by content hash.
///
/// Selection is content-addressed so the same corpus yields the same questions
/// on every run with nothing stored, and so re-chunking preserves the sample —
/// only the chunk a sentence belongs to changes, which is the thing being
/// measured. Ties break on text then chunk_id so the order is total.
pub(crate) fn select_sample(
    mut candidates: Vec<SampledSentence>,
    n: usize,
) -> Vec<SampledSentence> {
    candidates.sort_by(|a, b| {
        sentence_key(&a.text)
            .cmp(&sentence_key(&b.text))
            .then_with(|| a.text.cmp(&b.text))
            .then_with(|| a.chunk_id.cmp(&b.chunk_id))
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
        let got = split_sentences(&format!(
            "Five words are not enough. Exactly six words are kept here. {long}"
        ));
        assert_eq!(got, vec!["Exactly six words are kept here".to_string()]);
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
        let mut reversed = pool.clone();
        reversed.reverse();

        let a = select_sample(pool, 10);
        let b = select_sample(reversed, 10);
        assert_eq!(a, b, "selection must not depend on input order");
        assert_eq!(a.len(), 10);
    }

    #[test]
    fn sample_survives_rechunking() {
        let before: Vec<SampledSentence> =
            (0..50).map(|i| sent(i, &format!("sentence number {i} of the corpus body"))).collect();
        // Same sentences, different chunk ids AND a different order — re-chunking
        // changes both the grouping and the order rows come back in.
        let after: Vec<SampledSentence> = (0..50)
            .rev()
            .map(|i| sent(i + 900, &format!("sentence number {i} of the corpus body")))
            .collect();

        let texts = |v: Vec<SampledSentence>| -> Vec<String> {
            v.into_iter().map(|s| s.text).collect()
        };
        assert_eq!(texts(select_sample(before, 10)), texts(select_sample(after, 10)));
    }

    #[test]
    fn duplicate_text_resolves_by_chunk_id_not_input_order() {
        // chunk_overlap_chars means one sentence can appear verbatim in two
        // adjacent chunks. Whichever chunk wins must be the same every run.
        let a = vec![
            sent(7, "a sentence that appears in two overlapping chunks"),
            sent(3, "a sentence that appears in two overlapping chunks"),
        ];
        let mut b = a.clone();
        b.reverse();
        assert_eq!(select_sample(a, 1)[0].chunk_id, 3);
        assert_eq!(select_sample(b, 1)[0].chunk_id, 3);
    }

    #[test]
    fn joins_wrapped_lines_before_splitting() {
        // The indexer inserts '\n' at every soft break, so a wrapped paragraph
        // arrives as several lines of a single sentence.
        let got = split_sentences(
            "The engine keeps every fragment of the corpus\n\
             together with the heading it was written under.",
        );
        assert_eq!(
            got,
            vec!["The engine keeps every fragment of the corpus together with the heading it was written under".to_string()]
        );
    }

    #[test]
    fn sample_returns_everything_when_pool_is_small() {
        let pool: Vec<SampledSentence> =
            (0..3).map(|i| sent(i, &format!("sentence number {i} of the corpus body"))).collect();
        assert_eq!(select_sample(pool, 10).len(), 3);
    }
}
