//! Corpus language analysis for embedding-model selection (spec §4.2).
//!
//! Pure: callers read the files. Only English is ever identified — French,
//! German and every other language are simply "not English".

/// English stopwords whose density marks English prose. Also the English half
/// of `eval::detect_language`.
pub const EN_MARKERS: &[&str] =
    &["the", "and", "of", "to", "is", "in", "for", "with", "that", "are"];

/// A document with fewer prose words than this is ignored: too little to classify.
pub const MIN_WORDS: usize = 100;

/// Share of a document's prose words that are [`EN_MARKERS`] at or above which
/// it counts as English. Separates English prose documentation (lowest measured
/// 0.0875) from French prose (median 0.0029); spec §4.2 records the measured
/// distributions and margins.
pub const ENGLISH_DENSITY: f32 = 0.05;

/// Byte-weighted English share at or above which the English model is kept.
pub const ENGLISH_SHARE_FOR_ENGLISH_MODEL: f32 = 0.90;

/// The model chosen for a corpus that is not predominantly English prose.
pub const MULTILINGUAL_MODEL: &str = "embeddinggemma-300m";

/// Lowercased words, split exactly as `eval::detect_language` has always split
/// them: on anything that is neither alphabetic nor an apostrophe.
pub(crate) fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphabetic() && c != '\'')
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .collect()
}

/// The lines of `body` with fenced code — fence lines included — replaced by
/// empty lines, so prose around a block stays separated by a blank line.
///
/// A chunk carries no record of whether it began inside a code fence, and the
/// chunker splits on a character budget with no fence awareness — so a code
/// block longer than one chunk routinely starts a chunk mid-fence. Assuming
/// `in_fence = false` there inverts the state: the *closing* fence opens one,
/// real prose after it is discarded, and the code before it is kept. An odd
/// fence count is exactly the ambiguous case, and nothing in the text can
/// resolve it, so return nothing rather than guess.
// ponytail: fail closed on ambiguity; the fix is fence-aware chunking, which
// belongs in indexer.rs, not here.
pub(crate) fn blank_code_fences(body: &str) -> Vec<&str> {
    if body.matches("```").count() % 2 != 0 {
        return Vec::new();
    }
    let mut in_fence = false;
    body.lines()
        .map(|line| {
            if line.trim().starts_with("```") {
                in_fence = !in_fence;
                ""
            } else if in_fence {
                ""
            } else {
                line
            }
        })
        .collect()
}

/// Share of `doc`'s prose words that are English markers; `None` when it has
/// fewer than [`MIN_WORDS`] prose words. Fenced code is not prose.
pub fn english_density(doc: &str) -> Option<f32> {
    let prose = blank_code_fences(doc).join("\n");
    let words = words(&prose);
    if words.len() < MIN_WORDS {
        return None;
    }
    let hits = words.iter().filter(|w| EN_MARKERS.contains(&w.as_str())).count();
    Some(hits as f32 / words.len() as f32)
}

/// Byte-weighted share of `docs` that is English, over the documents that clear
/// the word floor; `None` when none does.
pub fn english_share(docs: &[&str]) -> Option<f32> {
    let (mut english, mut counted) = (0usize, 0usize);
    for doc in docs {
        let Some(density) = english_density(doc) else {
            continue;
        };
        counted += doc.len();
        if density >= ENGLISH_DENSITY {
            english += doc.len();
        }
    }
    (counted > 0).then(|| english as f32 / counted as f32)
}

/// `nomic-embed-text-v1.5` for predominantly English prose — or when there is
/// nothing to measure, which is today's behaviour — `embeddinggemma-300m`
/// otherwise.
pub fn choose_model(share: Option<f32>) -> &'static str {
    match share {
        Some(s) if s < ENGLISH_SHARE_FOR_ENGLISH_MODEL => MULTILINGUAL_MODEL,
        _ => crate::embedding::DEFAULT_MODEL_ID,
    }
}

#[cfg(test)]
pub(crate) mod test_fixtures {
    /// English prose, 125 words. `polaris-cli` reads the same file directly
    /// (`../../polaris-core/tests/fixtures/en_prose.md` from its `src/`, one
    /// level further from `src/mcp/`) rather than importing this module.
    pub(crate) const EN_PROSE: &str = include_str!("../tests/fixtures/en_prose.md");

    /// French prose, ~130 words. Read the same way as `EN_PROSE`.
    pub(crate) const FR_PROSE: &str = include_str!("../tests/fixtures/fr_prose.md");

    /// German prose, ~110 words. Used only here, so it stays inline.
    pub(crate) const DE_PROSE: &str = "Der Index speichert jeden Abschnitt der Dokumentation zusammen mit der Überschrift, unter der er geschrieben wurde. Wenn eine Frage eintrifft, vergleicht die Suche sie mit jedem gespeicherten Abschnitt und liefert die besten Treffer zuerst. Das Werkzeug ist für Teams gedacht, die Antworten aus ihren eigenen Notizen wollen, ohne sie an einen entfernten Dienst zu schicken. Die Konfigurationsdatei liegt neben dem Projekt, und die Standardwerte sind so gewählt, dass die meisten Projekte sie nie ändern müssen. Wenn die Ergebnisse falsch wirken, misst der Bewertungsbefehl, wie gut der Index Fragen beantwortet, die aus dem Korpus selbst stammen. Dieser Bericht ist ein Hinweis zum Einstellen der Schwelle und kein Versprechen über die Qualität jeder Antwort in der Praxis.";
}

#[cfg(test)]
mod tests {
    use super::test_fixtures::{DE_PROSE, EN_PROSE, FR_PROSE};
    use super::*;

    #[test]
    fn fixtures_clear_the_word_floor() {
        for doc in [EN_PROSE, FR_PROSE, DE_PROSE] {
            assert!(words(doc).len() >= MIN_WORDS, "{} words: {doc}", words(doc).len());
        }
    }

    #[test]
    fn english_prose_is_english() {
        assert!(english_density(EN_PROSE).unwrap() >= ENGLISH_DENSITY);
        assert_eq!(english_share(&[EN_PROSE]), Some(1.0));
        assert_eq!(choose_model(english_share(&[EN_PROSE])), "nomic-embed-text-v1.5");
    }

    #[test]
    fn french_and_german_prose_are_not_english() {
        for doc in [FR_PROSE, DE_PROSE] {
            assert!(english_density(doc).unwrap() < ENGLISH_DENSITY, "{doc}");
        }
        assert_eq!(english_share(&[FR_PROSE, DE_PROSE]), Some(0.0));
        assert_eq!(choose_model(english_share(&[FR_PROSE])), "embeddinggemma-300m");
        assert_eq!(choose_model(english_share(&[DE_PROSE])), "embeddinggemma-300m");
    }

    #[test]
    fn short_documents_are_ignored() {
        let short = "The index stores the chunks and the headings for each of the files.";
        assert_eq!(english_density(short), None);
        assert_eq!(english_share(&[short]), None);
        assert_eq!(english_share(&[short, FR_PROSE]), Some(0.0), "the short doc must not count");
    }

    #[test]
    fn code_only_documents_are_ignored() {
        let code = format!(
            "# Example\n\n```rust\n{}```\n",
            "let value = compute_the_thing(alpha, beta);\n".repeat(40)
        );
        assert!(words(&code).len() >= MIN_WORDS, "long enough to count if code counted");
        assert_eq!(english_density(&code), None);
        assert_eq!(english_share(&[code.as_str()]), None);
    }

    #[test]
    fn share_is_weighted_by_bytes() {
        let big_french = [FR_PROSE; 4].join("\n\n");
        let docs = [EN_PROSE, EN_PROSE, EN_PROSE, big_french.as_str()];
        let en = 3 * EN_PROSE.len();
        let expected = en as f32 / (en + big_french.len()) as f32;
        let share = english_share(&docs).unwrap();
        assert!((share - expected).abs() < 1e-6, "share {share}, expected {expected}");
        assert!(share < ENGLISH_SHARE_FOR_ENGLISH_MODEL, "3 of 4 docs English by count, mostly French by bytes");
        assert_eq!(choose_model(Some(share)), "embeddinggemma-300m");
    }

    #[test]
    fn density_threshold_is_inclusive() {
        let total = 1000;
        let at = (0..=total).find(|k| *k as f32 / total as f32 >= ENGLISH_DENSITY).unwrap();
        let doc = |hits: usize| format!("{}{}", "the ".repeat(hits), "word ".repeat(total - hits));
        assert_eq!(english_share(&[doc(at).as_str()]), Some(1.0));
        assert_eq!(english_share(&[doc(at - 1).as_str()]), Some(0.0));
    }

    #[test]
    fn model_choice_boundary() {
        assert_eq!(choose_model(None), "nomic-embed-text-v1.5");
        assert_eq!(choose_model(Some(1.0)), "nomic-embed-text-v1.5");
        assert_eq!(choose_model(Some(ENGLISH_SHARE_FOR_ENGLISH_MODEL)), "nomic-embed-text-v1.5");
        assert_eq!(choose_model(Some(0.8999)), "embeddinggemma-300m");
        assert_eq!(choose_model(Some(0.0)), "embeddinggemma-300m");
    }

    #[test]
    fn blank_code_fences_blanks_code_and_fences_but_keeps_line_positions() {
        let body = "prose one\n```\ncode\n```\nprose two";
        assert_eq!(blank_code_fences(body), vec!["prose one", "", "", "", "prose two"]);
    }

    #[test]
    fn blank_code_fences_gives_up_on_an_odd_fence_count() {
        assert!(blank_code_fences("code\n```\nprose").is_empty());
    }

    #[test]
    fn words_split_like_detect_language() {
        assert_eq!(words("L'index, c'est THE-best 42x"), vec!["l'index", "c'est", "the", "best", "x"]);
    }
}
