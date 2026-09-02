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

/// Corpus language, only as finely as the built-in probe sets require.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    English,
    French,
    Unknown,
}

impl Language {
    pub fn as_str(&self) -> &'static str {
        match self {
            Language::English => "english",
            Language::French => "french",
            Language::Unknown => "unknown",
        }
    }
}

const EN_MARKERS: &[&str] =
    &["the", "and", "of", "to", "is", "in", "for", "with", "that", "are"];
const FR_MARKERS: &[&str] =
    &["le", "la", "les", "de", "des", "et", "une", "dans", "pour", "est"];

/// Minimum marker hits, and the margin the winner must hold over the runner-up,
/// before a language is claimed. Below either, the answer is `Unknown` — which
/// costs a threshold recommendation but never produces a wrong one.
const MIN_MARKER_HITS: usize = 5;
const MARKER_MARGIN: usize = 2;

/// Identify the corpus language by counting language-marker stopwords.
///
/// This only has to separate the probe sets that ship, so a word-frequency
/// count is enough and avoids a dependency.
pub(crate) fn detect_language(sample_text: &str) -> Language {
    let words: Vec<String> = sample_text
        .split(|c: char| !c.is_alphabetic() && c != '\'')
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .collect();

    let count = |markers: &[&str]| -> usize {
        words.iter().filter(|w| markers.contains(&w.as_str())).count()
    };

    let en = count(EN_MARKERS);
    let fr = count(FR_MARKERS);

    let (winner, best, runner_up) = if en >= fr {
        (Language::English, en, fr)
    } else {
        (Language::French, fr, en)
    };

    if best >= MIN_MARKER_HITS && best >= runner_up.saturating_mul(MARKER_MARGIN) {
        winner
    } else {
        Language::Unknown
    }
}

/// Off-topic probes. Their top-1 scores are what this corpus and model return
/// when no answer exists, which is the quantity the threshold gates on. Subjects
/// are chosen to share no vocabulary with software documentation.
const PROBES_EN: &[&str] = &[
    "how long should sourdough dough rise before baking",
    "what is the capital city of Mongolia",
    "the offside rule in football explained simply",
    "best hiking trails in the Alps for a weekend",
    "how to prune a tomato plant in summer",
    "why does my cat refuse to eat her dinner",
    "which countries border the Black Sea",
    "how to remove a red wine stain from a carpet",
    "the difference between a violin and a viola",
    "when do migratory swallows return to northern Europe",
    "how many players are on a rugby union team",
    "what makes the northern lights appear green",
    "how to fold a fitted bedsheet neatly",
    "the tallest mountain in South America",
    "how long can a tortoise live in captivity",
    "what to feed a puppy in its first month",
    "how deep is the Mariana Trench",
    "the rules for scoring in ten pin bowling",
    "how to tell whether an avocado is ripe",
    "which planet has the most moons",
];

const PROBES_FR: &[&str] = &[
    "combien de temps faut-il laisser lever la pâte à pain",
    "quelle est la capitale de la Mongolie",
    "la règle du hors-jeu au football expliquée simplement",
    "les plus beaux sentiers de randonnée des Alpes",
    "comment tailler un pied de tomate en été",
    "pourquoi mon chat refuse-t-il de manger ses croquettes",
    "quels pays bordent la mer Noire",
    "comment enlever une tache de vin rouge sur un tapis",
    "la différence entre un violon et un alto",
    "quand les hirondelles reviennent-elles en Europe du Nord",
    "combien de joueurs compte une équipe de rugby",
    "pourquoi les aurores boréales sont-elles vertes",
    "comment plier un drap housse correctement",
    "quelle est la plus haute montagne d'Amérique du Sud",
    "combien de temps vit une tortue en captivité",
    "que donner à manger à un chiot le premier mois",
    "quelle est la profondeur de la fosse des Mariannes",
    "comment compter les points au bowling",
    "comment savoir si un avocat est mûr",
    "quelle planète possède le plus de lunes",
];

/// The built-in probe set for a language, or `None` when none ships for it.
pub(crate) fn builtin_probes(lang: Language) -> Option<&'static [&'static str]> {
    match lang {
        Language::English => Some(PROBES_EN),
        Language::French => Some(PROBES_FR),
        Language::Unknown => None,
    }
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

    #[test]
    fn detects_english() {
        let text = "The index stores the chunks and the headings for each of the files \
                    that are found in the directory, and the search returns them.";
        assert_eq!(detect_language(text), Language::English);
    }

    #[test]
    fn detects_french() {
        let text = "Le moteur enregistre les fragments et les titres de chacun des \
                    fichiers que l'on trouve dans le dossier, et la recherche les renvoie.";
        assert_eq!(detect_language(text), Language::French);
    }

    #[test]
    fn unknown_when_no_marker_wins_clearly() {
        assert_eq!(detect_language("cargo build release binary target"), Language::Unknown);
    }

    #[test]
    fn unknown_when_text_is_empty() {
        assert_eq!(detect_language(""), Language::Unknown);
    }

    #[test]
    fn builtin_probes_exist_for_known_languages() {
        assert!(builtin_probes(Language::English).unwrap().len() >= 20);
        assert!(builtin_probes(Language::French).unwrap().len() >= 20);
        assert!(builtin_probes(Language::Unknown).is_none());
    }

    #[test]
    fn probes_avoid_software_vocabulary() {
        // A probe that brushes against software wording stops being off-topic and
        // silently raises the measured floor.
        let banned = [
            "index", "search", "query", "chunk", "server", "config", "database",
            "file", "token", "cache", "build", "deploy", "api", "code",
        ];
        // Whole-word comparison, not substring: "capital" / "capitale" both
        // contain "api", and those are legitimate probes.
        for lang in [Language::English, Language::French] {
            for probe in builtin_probes(lang).unwrap() {
                let lower = probe.to_lowercase();
                let tokens: Vec<&str> = lower
                    .split(|c: char| !c.is_alphanumeric())
                    .filter(|t| !t.is_empty())
                    .collect();
                for word in banned {
                    assert!(!tokens.contains(&word), "probe {probe:?} contains {word:?}");
                }
            }
        }
    }

    #[test]
    fn margin_decides_when_both_languages_are_present() {
        // Marker counts are hand-built so each case lands on a specific side of
        // MARKER_MARGIN. EN_MARKERS and FR_MARKERS share no word, so each token
        // counts for exactly one language.

        // en = 6, fr = 6. A tie can never clear a 2x margin, so a corpus that
        // looks equally like both must not be claimed as either.
        assert_eq!(
            detect_language("the and of to is in le la les de des et"),
            Language::Unknown
        );

        // en = 10, fr = 5. The margin is met exactly, so English is claimed.
        assert_eq!(
            detect_language("the and of to is in for with that are le la les de des"),
            Language::English
        );

        // en = 9, fr = 5. One marker short of the margin: not claimed.
        assert_eq!(
            detect_language("the and of to is in for with that le la les de des"),
            Language::Unknown
        );
    }
}
