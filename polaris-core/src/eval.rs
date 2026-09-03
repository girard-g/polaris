//! Self-discovering retrieval evaluation.
//!
//! Ground truth comes from the corpus: a sentence lifted from a chunk is a
//! query whose correct answer is the chunk it came from. See
//! `docs/superpowers/specs/2026-09-02-polaris-eval-design.md`.

use sha2::{Digest, Sha256};

use crate::bank::BankConfig;
use crate::bank::Bank;
use crate::db::EvalRunRow;
use crate::error::Result;

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
    // A chunk carries no record of whether it began inside a code fence, and the
    // chunker splits on a character budget with no fence awareness — so a code
    // block longer than one chunk routinely starts a chunk mid-fence. Assuming
    // `in_fence = false` there inverts the state: the *closing* fence opens one,
    // real prose after it is discarded, and the code before it is emitted as
    // queries. An odd fence count is exactly the ambiguous case, and nothing in
    // the chunk can resolve it, so drop the chunk rather than guess.
    // ponytail: fail closed on ambiguity; the fix is fence-aware chunking, which
    // belongs in indexer.rs, not here.
    if body.matches("```").count() % 2 != 0 {
        return Vec::new();
    }

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

/// What one sampled sentence produced. Ranks are 1-based; `None` means the
/// target never appeared in the retrieved window.
#[derive(Debug, Clone)]
pub struct Outcome {
    pub chunk_rank: Option<usize>,
    pub file_rank: Option<usize>,
    /// Score of the source chunk when it was retrieved. Feeds the positive
    /// distribution, so a miss contributes nothing rather than a zero.
    pub score: Option<f32>,
}

/// Retrieval quality over a sample.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct Metrics {
    pub recall_1: f32,
    pub recall_3: f32,
    pub recall_3_file: f32,
    pub mrr: f32,
}

pub(crate) fn metrics(outcomes: &[Outcome]) -> Metrics {
    if outcomes.is_empty() {
        return Metrics { recall_1: 0.0, recall_3: 0.0, recall_3_file: 0.0, mrr: 0.0 };
    }
    let n = outcomes.len() as f32;
    let within = |r: Option<usize>, k: usize| -> bool { matches!(r, Some(v) if v <= k) };

    let recall_1 = outcomes.iter().filter(|o| within(o.chunk_rank, 1)).count() as f32 / n;
    let recall_3 = outcomes.iter().filter(|o| within(o.chunk_rank, 3)).count() as f32 / n;
    let recall_3_file = outcomes.iter().filter(|o| within(o.file_rank, 3)).count() as f32 / n;
    let mrr = outcomes
        .iter()
        .map(|o| o.chunk_rank.map_or(0.0, |r| 1.0 / r as f32))
        .sum::<f32>()
        / n;

    Metrics { recall_1, recall_3, recall_3_file, mrr }
}

/// Nearest-rank percentile. Sorts in place; `p` is in `[0.0, 1.0]`.
/// Returns 0.0 for an empty input rather than panicking.
pub(crate) fn percentile(values: &mut Vec<f32>, p: f64) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = values.len();
    // Nearest rank: the smallest value at or above the p-th position.
    let idx = ((p * n as f64).ceil() as usize).saturating_sub(1).min(n - 1);
    values[idx]
}

/// Recommend `search_min_similarity` as the midpoint between the strongest
/// false confidence and the weakest correct answer.
///
/// `p10` of positives rather than the median: the threshold must clear the weak
/// end of the correct answers, because rejecting those is the failure that makes
/// Polaris look broken rather than mistuned.
///
/// `None` when no probes ran, or when the distributions overlap — an overlap is
/// a real finding about the corpus (commonly an English-dominant model on a
/// non-English corpus) and reporting a number would paper over it.
pub(crate) fn suggest_threshold(
    positives: &mut Vec<f32>,
    probes: &mut Vec<f32>,
) -> Option<f32> {
    if probes.is_empty() || positives.is_empty() {
        return None;
    }
    let floor = percentile(probes, 0.95);
    let weakest_positive = percentile(positives, 0.10);
    if floor >= weakest_positive {
        return None;
    }
    Some((floor + weakest_positive) / 2.0)
}

/// Hash over the corpus contents, used to decide whether two runs are
/// comparable. Sorted first so enumeration order cannot change the result.
pub(crate) fn corpus_fingerprint(docs: &[(String, String)]) -> String {
    let mut sorted: Vec<&(String, String)> = docs.iter().collect();
    sorted.sort();

    let mut hasher = Sha256::new();
    for (path, hash) in sorted {
        hasher.update(path.as_bytes());
        hasher.update(b"\0");
        hasher.update(hash.as_bytes());
        hasher.update(b"\n");
    }
    format!("{:x}", hasher.finalize())
}

/// Snapshot the settings that affect retrieval, stored with the run.
///
/// `polaris.toml` only ever describes the config *now*, so it cannot explain
/// why a run three weeks ago scored differently. Where the database lives is
/// not a retrieval event and is deliberately absent.
pub(crate) fn config_snapshot(cfg: &BankConfig) -> String {
    serde_json::json!({
        "model_id": cfg.model_id,
        "embedding_dim": cfg.embedding_dim,
        "max_chunk_tokens": cfg.max_chunk_tokens,
        "chunk_overlap_chars": cfg.chunk_overlap_chars,
        "mmr_lambda": cfg.mmr_lambda,
        "mmr_candidate_multiplier": cfg.mmr_candidate_multiplier,
        "heading_boost": cfg.heading_boost,
        "rrf_k": cfg.rrf_k,
    })
    .to_string()
}

/// How deep each eval query retrieves. Ranks beyond this count as a miss, so it
/// bounds MRR's denominator as well as recall's window.
const EVAL_TOP_K: usize = 10;

/// Depth at which probe scores are measured.
///
/// The two threshold consumers aggregate differently: the auto-search hook gates
/// on the single top-1 cosine, while the MCP tool takes the max across `top_k`
/// (default 5). Max-over-5 is never below max-over-1, so measuring probes at 1
/// understated the ceiling the MCP gate actually faces and biased the suggested
/// threshold low — re-admitting the off-topic injection the gate exists to stop.
/// Measuring at the MCP default yields a floor valid for both consumers.
const PROBE_TOP_K: usize = 5;

/// Below this many usable sentences the percentiles are noise. The run still
/// completes; the caller is told.
pub const MIN_RELIABLE_SAMPLE: usize = 30;

pub struct EvalOpts {
    pub sample_size: usize,
    /// Empty means "use the built-in set for the detected language".
    pub probes: Vec<String>,
}

pub struct EvalReport {
    pub metrics: Metrics,
    /// Sentences actually evaluated.
    pub sample_size: usize,
    /// Sampled sentences whose search returned no results at all. A sentence
    /// whose source chunk simply ranked outside the window is NOT skipped — it
    /// is a miss, and counts against recall and MRR.
    pub skipped: usize,
    pub positive_p10: f32,
    pub positive_median: f32,
    pub probe_p95: Option<f32>,
    pub suggested_threshold: Option<f32>,
    /// Detected corpus language, or `Unknown` when detection did not run —
    /// which includes the case where the caller supplied explicit probes.
    /// This reports detection, NOT probe availability: `probe_p95.is_some()`
    /// is the signal for whether probes actually ran.
    pub language: Language,
    pub corpus_fingerprint: String,
    pub config_json: String,
    /// The previous run, when one exists.
    pub previous: Option<EvalRunRow>,
    /// True when the previous run measured a different corpus, which makes the
    /// deltas meaningless and suppresses them.
    pub corpus_changed: bool,
}

/// Evaluate retrieval against ground truth derived from the corpus.
pub fn run(bank: &Bank, opts: EvalOpts) -> Result<EvalReport> {
    let mut docs = bank.with_db(|db| db.get_all_document_hashes())?;
    let corpus_fingerprint = corpus_fingerprint(&docs);
    let config_json = config_snapshot(bank.config());

    // Sort before sampling: the row order from get_all_document_hashes is
    // unspecified, and corpus_text feeds detect_language -> probe set ->
    // suggested_threshold. Two runs over an identical corpus must not be able
    // to recommend different thresholds. corpus_fingerprint sorts for the same
    // reason.
    docs.sort();

    // Gather every candidate sentence with the chunk it came from.
    let mut candidates = Vec::new();
    let mut corpus_text = String::new();
    for (path, _) in &docs {
        let chunks = bank.with_db(|db| db.get_chunks_for_document(path))?;
        for chunk in chunks {
            if corpus_text.len() < 20_000 {
                corpus_text.push_str(&chunk.content);
                corpus_text.push('\n');
            }
            for text in split_sentences(&chunk.content) {
                candidates.push(SampledSentence {
                    chunk_id: chunk.id,
                    file_path: path.clone(),
                    text,
                });
            }
        }
    }

    let sample = select_sample(candidates, opts.sample_size);

    // Replay each sentence through the production pipeline. `bank.search`
    // applies the configured heading boost, MMR and RRF — do NOT construct a
    // SearchEngine with heading_boost = 0.0 here. Eval measures the pipeline
    // the user actually runs; a body sentence trips the boost only when its
    // terms appear in its own heading, which is what a real question does too,
    // and disabling it would blind eval to a `heading_boost` regression.
    let mut outcomes = Vec::with_capacity(sample.len());
    let mut skipped = 0usize;
    for s in &sample {
        let results = bank.search(&s.text, crate::SearchOpts { top_k: EVAL_TOP_K })?;
        let chunk_rank = results.iter().position(|r| r.chunk_id == s.chunk_id).map(|i| i + 1);
        let file_rank = results.iter().position(|r| r.file_path == s.file_path).map(|i| i + 1);
        if chunk_rank.is_none() && file_rank.is_none() && results.is_empty() {
            skipped += 1;
            continue;
        }
        let score = chunk_rank.map(|r| results[r - 1].score);
        outcomes.push(Outcome { chunk_rank, file_rank, score });
    }

    // Probes: explicit list wins, otherwise the built-in set for the language.
    let language = if opts.probes.is_empty() {
        detect_language(&corpus_text)
    } else {
        Language::Unknown
    };
    let probe_queries: Vec<String> = if !opts.probes.is_empty() {
        opts.probes.clone()
    } else {
        builtin_probes(language)
            .map(|p| p.iter().map(|s| s.to_string()).collect())
            .unwrap_or_default()
    };

    let mut probe_scores = Vec::with_capacity(probe_queries.len());
    for q in &probe_queries {
        let results = bank.search(q, crate::SearchOpts { top_k: PROBE_TOP_K })?;
        // Max across the set, not `first()` — MMR reranks for diversity, so the
        // head is not the highest-scoring result. This mirrors the MCP gate
        // exactly (`fold(f32::MIN, f32::max)` over its own top_k); measuring the
        // head instead would understate the ceiling the gate computes, which is
        // the whole point of measuring probes at PROBE_TOP_K in the first place.
        // An empty result set contributes nothing rather than a zero.
        if let Some(best) = results
            .iter()
            .map(|r| r.score)
            .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
        {
            probe_scores.push(best);
        }
    }

    let mut positives: Vec<f32> = outcomes.iter().filter_map(|o| o.score).collect();
    let positive_p10 = percentile(&mut positives, 0.10);
    let positive_median = percentile(&mut positives, 0.50);
    let probe_p95 = if probe_scores.is_empty() {
        None
    } else {
        Some(percentile(&mut probe_scores, 0.95))
    };
    let suggested_threshold = suggest_threshold(&mut positives, &mut probe_scores);

    let m = metrics(&outcomes);

    // Read the previous run before writing this one.
    let previous = bank.with_db(|db| db.last_eval_run())?;
    let corpus_changed = previous
        .as_ref()
        .map(|p| p.corpus_fingerprint != corpus_fingerprint)
        .unwrap_or(false);

    let row = EvalRunRow {
        id: 0,
        ts: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0),
        sample_size: outcomes.len() as i64,
        recall_1: m.recall_1,
        recall_3: m.recall_3,
        recall_3_file: m.recall_3_file,
        mrr: m.mrr,
        positive_p10,
        positive_median,
        probe_p95,
        suggested_threshold,
        corpus_fingerprint: corpus_fingerprint.clone(),
        config_json: config_json.clone(),
    };
    // A run that sampled nothing measured nothing. Persisting it would hand the
    // next run a baseline of zeros and manufacture a regression out of it.
    if !outcomes.is_empty() {
        bank.with_db(|db| db.insert_eval_run(&row))?;
    }

    Ok(EvalReport {
        metrics: m,
        sample_size: outcomes.len(),
        skipped,
        positive_p10,
        positive_median,
        probe_p95,
        suggested_threshold,
        language,
        corpus_fingerprint,
        config_json,
        previous,
        corpus_changed,
    })
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
    fn drops_a_chunk_that_starts_inside_a_code_fence() {
        // The chunker splits on a character budget with no fence awareness, so a
        // long code block gives the next chunk a body that opens with the block's
        // remainder and then closes it. Treating that closing fence as an opening
        // one inverts the state: prose after it is dropped and the code before it
        // is emitted as queries.
        let got = split_sentences(
            "let total = compute_everything(alpha, beta, gamma);\n\
             ```\n\
             This ordinary prose sentence follows the code block.",
        );
        assert!(
            got.is_empty(),
            "an odd fence count is ambiguous and must be dropped, got: {got:?}"
        );
    }

    #[test]
    fn balanced_fences_still_yield_surrounding_prose() {
        // The fail-closed rule must not swallow the ordinary balanced case.
        let got = split_sentences(
            "This ordinary prose sentence precedes the code block.\n\
             ```\n\
             let total = compute_everything(alpha, beta, gamma);\n\
             ```\n\
             And this ordinary prose sentence follows the code block.",
        );
        assert_eq!(got.len(), 2, "expected both prose sentences, got: {got:?}");
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

    fn outcome(chunk_rank: Option<usize>, file_rank: Option<usize>) -> Outcome {
        Outcome { chunk_rank, file_rank, score: chunk_rank.map(|_| 0.8) }
    }

    #[test]
    fn metrics_on_perfect_results() {
        let m = metrics(&[outcome(Some(1), Some(1)), outcome(Some(1), Some(1))]);
        assert_eq!(m.recall_1, 1.0);
        assert_eq!(m.recall_3, 1.0);
        assert_eq!(m.mrr, 1.0);
    }

    #[test]
    fn metrics_on_total_miss() {
        let m = metrics(&[outcome(None, None), outcome(None, None)]);
        assert_eq!(m.recall_1, 0.0);
        assert_eq!(m.recall_3, 0.0);
        assert_eq!(m.mrr, 0.0);
    }

    #[test]
    fn recall_3_counts_ranks_two_and_three_but_recall_1_does_not() {
        let m = metrics(&[outcome(Some(3), Some(1)), outcome(Some(1), Some(1))]);
        assert_eq!(m.recall_1, 0.5);
        assert_eq!(m.recall_3, 1.0);
        // MRR = (1/3 + 1/1) / 2
        assert!((m.mrr - 0.666_666_7).abs() < 1e-5);
    }

    #[test]
    fn file_recall_can_exceed_chunk_recall() {
        // Right document, wrong section: the chunking signal.
        let m = metrics(&[outcome(None, Some(1)), outcome(None, Some(2))]);
        assert_eq!(m.recall_3, 0.0);
        assert_eq!(m.recall_3_file, 1.0);
    }

    #[test]
    fn metrics_on_empty_input_are_zero_not_nan() {
        let m = metrics(&[]);
        assert_eq!(m.recall_1, 0.0);
        assert_eq!(m.mrr, 0.0);
    }

    #[test]
    fn percentile_picks_expected_values() {
        let mut v = vec![0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 0.9, 1.0];
        assert!((percentile(&mut v, 0.5) - 0.5).abs() < 1e-6);
        assert!((percentile(&mut v, 0.0) - 0.1).abs() < 1e-6);
        assert!((percentile(&mut v, 1.0) - 1.0).abs() < 1e-6);
        // idx = ceil(p * n) - 1, clamped to [0, n-1], n = 10:
        // p=0.10 -> ceil(1.0) - 1 = 0  -> v[0] = 0.1
        // p=0.95 -> ceil(9.5) - 1 = 9  -> v[9] = 1.0
        assert!((percentile(&mut v, 0.10) - 0.1).abs() < 1e-6);
        assert!((percentile(&mut v, 0.95) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn threshold_sits_between_separated_distributions() {
        let mut positives = vec![0.70, 0.75, 0.78, 0.80, 0.85];
        let mut probes = vec![0.50, 0.52, 0.54, 0.55, 0.56];
        let t = suggest_threshold(&mut positives, &mut probes).expect("separated");
        assert!(t > 0.56 && t < 0.70, "threshold {t} not between the distributions");
    }

    #[test]
    fn threshold_is_none_when_distributions_overlap() {
        let mut positives = vec![0.50, 0.55, 0.60];
        let mut probes = vec![0.58, 0.62, 0.70];
        assert!(suggest_threshold(&mut positives, &mut probes).is_none());
    }

    #[test]
    fn threshold_is_none_without_probes() {
        let mut positives = vec![0.70, 0.80];
        let mut probes: Vec<f32> = vec![];
        assert!(suggest_threshold(&mut positives, &mut probes).is_none());
    }

    #[test]
    fn fingerprint_ignores_input_order() {
        let a = vec![("a.md".to_string(), "h1".to_string()), ("b.md".to_string(), "h2".to_string())];
        let b = vec![("b.md".to_string(), "h2".to_string()), ("a.md".to_string(), "h1".to_string())];
        assert_eq!(corpus_fingerprint(&a), corpus_fingerprint(&b));
    }

    #[test]
    fn fingerprint_changes_when_a_document_changes() {
        let a = vec![("a.md".to_string(), "h1".to_string())];
        let b = vec![("a.md".to_string(), "h2".to_string())];
        assert_ne!(corpus_fingerprint(&a), corpus_fingerprint(&b));
    }

    #[test]
    fn fingerprint_changes_when_a_document_is_added() {
        let a = vec![("a.md".to_string(), "h1".to_string())];
        let b = vec![("a.md".to_string(), "h1".to_string()), ("b.md".to_string(), "h2".to_string())];
        assert_ne!(corpus_fingerprint(&a), corpus_fingerprint(&b));
    }

    #[test]
    fn config_snapshot_records_retrieval_settings_only() {
        let cfg = crate::bank::BankConfig::default();
        let v: serde_json::Value = serde_json::from_str(&config_snapshot(&cfg)).unwrap();

        // Assert the VALUES, not just that the key names appear. A snapshot with
        // two same-typed settings transposed would still contain both keys, and
        // would then name the wrong setting when explaining a regression.
        assert_eq!(v["model_id"], serde_json::json!(cfg.model_id));
        assert_eq!(v["embedding_dim"], serde_json::json!(cfg.embedding_dim));
        assert_eq!(v["max_chunk_tokens"], serde_json::json!(cfg.max_chunk_tokens));
        assert_eq!(v["chunk_overlap_chars"], serde_json::json!(cfg.chunk_overlap_chars));
        assert_eq!(v["mmr_lambda"], serde_json::json!(cfg.mmr_lambda));
        assert_eq!(
            v["mmr_candidate_multiplier"],
            serde_json::json!(cfg.mmr_candidate_multiplier)
        );
        assert_eq!(v["heading_boost"], serde_json::json!(cfg.heading_boost));
        assert_eq!(v["rrf_k"], serde_json::json!(cfg.rrf_k));

        // Where the database lives is not a retrieval event.
        assert!(v.get("index_path").is_none(), "snapshot leaked index_path");
        assert!(v.get("repo_root").is_none(), "snapshot leaked repo_root");
    }
}
