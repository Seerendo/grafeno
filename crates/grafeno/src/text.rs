//! Full-text index: a tokenizer and BM25 ranking over redb tables.
//!
//! The index is updated inside the same write transaction as the graph, so a
//! node and its indexed text are always consistent.

use std::collections::HashMap;

use grafeno_core::{NodeId, Result};
use redb::{ReadableTable, WriteTransaction};
use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;

use crate::Hit;
use crate::store::{POSTINGS, PostingKey, RecordKey, TEXT_LENGTHS, TEXT_STATS, TEXTS, storage};

const MAX_TERM_CHARS: usize = 64;
const K1: f32 = 1.2;
const B: f32 = 0.75;

/// Common English and Spanish words, without accents (terms are compared
/// after removing them).
const STOPWORDS: &[&str] = &[
    // English
    "an", "and", "are", "as", "at", "be", "but", "by", "for", "from", "has", "have", "he", "her",
    "his", "if", "in", "into", "is", "it", "its", "of", "on", "or", "our", "she", "so", "than",
    "that", "the", "their", "them", "then", "there", "these", "they", "this", "to", "was", "we",
    "were", "what", "when", "where", "which", "who", "will", "with", "you", "your",
    // Spanish
    "al", "algo", "como", "con", "de", "del", "el", "ella", "ellos", "en", "entre", "era", "es",
    "esa", "ese", "eso", "esta", "este", "esto", "fue", "ha", "han", "hay", "la", "las", "le",
    "les", "lo", "los", "mas", "me", "mi", "muy", "no", "nos", "para", "pero", "por", "que", "se",
    "si", "sin", "sobre", "son", "su", "sus", "te", "tu", "un", "una", "uno", "unos", "unas", "ya",
    "yo",
];

/// Splits text into lowercase terms without accents ("Canción" → "cancion").
/// Terms of one character, longer than 64, or stopwords are dropped.
///
/// Removing a node's text re-tokenizes the stored text to find its postings,
/// so changing this function requires rebuilding existing indexes.
pub fn tokenize(text: &str) -> Vec<String> {
    let mut terms = Vec::new();
    let mut current = String::new();
    for c in text.nfd() {
        if is_combining_mark(c) {
            continue;
        }
        if c.is_alphanumeric() {
            current.extend(c.to_lowercase());
        } else {
            push_term(&mut current, &mut terms);
        }
    }
    push_term(&mut current, &mut terms);
    terms
}

fn push_term(current: &mut String, terms: &mut Vec<String>) {
    let chars = current.chars().count();
    if (2..=MAX_TERM_CHARS).contains(&chars) && !STOPWORDS.contains(&current.as_str()) {
        terms.push(std::mem::take(current));
    } else {
        current.clear();
    }
}

fn term_frequencies(terms: &[String]) -> HashMap<&str, u32> {
    let mut frequencies = HashMap::new();
    for term in terms {
        *frequencies.entry(term.as_str()).or_insert(0) += 1;
    }
    frequencies
}

/// Indexes `text` for `node`, replacing any previous text.
pub(crate) fn index(tx: &WriteTransaction, ns: &str, node: NodeId, text: &str) -> Result<()> {
    remove(tx, ns, node)?;
    let id = node.as_u128();
    let terms = tokenize(text);
    let mut postings = tx.open_table(POSTINGS).map_err(storage)?;
    for (term, frequency) in term_frequencies(&terms) {
        postings
            .insert((ns, term, id), frequency)
            .map_err(storage)?;
    }
    let length = terms.len() as u32;
    tx.open_table(TEXTS)
        .map_err(storage)?
        .insert((ns, id), text)
        .map_err(storage)?;
    tx.open_table(TEXT_LENGTHS)
        .map_err(storage)?
        .insert((ns, id), length)
        .map_err(storage)?;
    update_stats(tx, ns, 1, i64::from(length))
}

/// Removes the indexed text of `node`. Returns whether there was any.
pub(crate) fn remove(tx: &WriteTransaction, ns: &str, node: NodeId) -> Result<bool> {
    let id = node.as_u128();
    let mut texts = tx.open_table(TEXTS).map_err(storage)?;
    let Some(text) = texts
        .remove((ns, id))
        .map_err(storage)?
        .map(|guard| guard.value().to_owned())
    else {
        return Ok(false);
    };
    drop(texts);
    let terms = tokenize(&text);
    let mut postings = tx.open_table(POSTINGS).map_err(storage)?;
    for term in term_frequencies(&terms).keys() {
        postings.remove((ns, *term, id)).map_err(storage)?;
    }
    drop(postings);
    let length = tx
        .open_table(TEXT_LENGTHS)
        .map_err(storage)?
        .remove((ns, id))
        .map_err(storage)?
        .map(|guard| guard.value())
        .unwrap_or(0);
    update_stats(tx, ns, -1, -i64::from(length))?;
    Ok(true)
}

fn update_stats(tx: &WriteTransaction, ns: &str, documents: i64, terms: i64) -> Result<()> {
    let mut stats = tx.open_table(TEXT_STATS).map_err(storage)?;
    let (current_documents, current_terms) = stats
        .get(ns)
        .map_err(storage)?
        .map(|guard| guard.value())
        .unwrap_or((0, 0));
    let next = (
        current_documents.saturating_add_signed(documents),
        current_terms.saturating_add_signed(terms),
    );
    stats.insert(ns, next).map_err(storage)?;
    Ok(())
}

pub(crate) fn text(
    texts: &impl ReadableTable<RecordKey, &'static str>,
    ns: &str,
    node: NodeId,
) -> Result<Option<String>> {
    Ok(texts
        .get((ns, node.as_u128()))
        .map_err(storage)?
        .map(|guard| guard.value().to_owned()))
}

/// The `limit` best nodes for `query` by BM25, best first.
pub(crate) fn search(
    postings: &impl ReadableTable<PostingKey, u32>,
    lengths: &impl ReadableTable<RecordKey, u32>,
    stats: &impl ReadableTable<&'static str, (u64, u64)>,
    ns: &str,
    query: &str,
    limit: usize,
) -> Result<Vec<Hit>> {
    let Some((documents, total_terms)) = stats.get(ns).map_err(storage)?.map(|g| g.value()) else {
        return Ok(Vec::new());
    };
    if documents == 0 || limit == 0 {
        return Ok(Vec::new());
    }
    let documents = documents as f32;
    let average_length = (total_terms as f32 / documents).max(1.0);
    let mut terms = tokenize(query);
    terms.sort();
    terms.dedup();

    let mut scores: HashMap<u128, f32> = HashMap::new();
    for term in &terms {
        let mut matches = Vec::new();
        for item in postings
            .range((ns, term.as_str(), 0u128)..)
            .map_err(storage)?
        {
            let (key, frequency) = item.map_err(storage)?;
            let (key_ns, key_term, node) = key.value();
            if key_ns != ns || key_term != term {
                break;
            }
            matches.push((node, frequency.value() as f32));
        }
        let frequency_in_docs = matches.len() as f32;
        let idf = (1.0 + (documents - frequency_in_docs + 0.5) / (frequency_in_docs + 0.5)).ln();
        for (node, frequency) in matches {
            let length = lengths
                .get((ns, node))
                .map_err(storage)?
                .map(|guard| guard.value())
                .unwrap_or(0) as f32;
            let norm = K1 * (1.0 - B + B * length / average_length);
            *scores.entry(node).or_insert(0.0) += idf * frequency * (K1 + 1.0) / (frequency + norm);
        }
    }

    let mut hits: Vec<Hit> = scores
        .into_iter()
        .map(|(node, score)| Hit {
            node: NodeId::from_u128(node),
            score,
        })
        .collect();
    hits.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.node.cmp(&b.node)));
    hits.truncate(limit);
    Ok(hits)
}

/// Checks the text index of a namespace against its stored texts. Returns
/// the number of indexed texts.
pub(crate) fn check(
    texts: &impl ReadableTable<RecordKey, &'static str>,
    lengths: &impl ReadableTable<RecordKey, u32>,
    postings: &impl ReadableTable<PostingKey, u32>,
    stats: &impl ReadableTable<&'static str, (u64, u64)>,
    ns: &str,
    nodes: &std::collections::HashSet<u128>,
    problems: &mut Vec<String>,
) -> Result<u64> {
    let (mut documents, mut total_terms, mut expected_postings) = (0u64, 0u64, 0u64);
    for item in texts.range((ns, 0u128)..).map_err(storage)? {
        let (key, value) = item.map_err(storage)?;
        let (key_ns, id) = key.value();
        if key_ns != ns {
            break;
        }
        let node = NodeId::from_u128(id);
        documents += 1;
        if !nodes.contains(&id) {
            problems.push(format!("text of missing node {node}"));
        }
        let terms = tokenize(value.value());
        total_terms += terms.len() as u64;
        let frequencies = term_frequencies(&terms);
        expected_postings += frequencies.len() as u64;
        let length = lengths.get((ns, id)).map_err(storage)?.map(|g| g.value());
        if length != Some(terms.len() as u32) {
            problems.push(format!(
                "text length of node {node} is {length:?}, expected {}",
                terms.len()
            ));
        }
        for (term, frequency) in frequencies {
            let stored = postings
                .get((ns, term, id))
                .map_err(storage)?
                .map(|g| g.value());
            if stored != Some(frequency) {
                problems.push(format!(
                    "posting {term:?} of node {node} is {stored:?}, expected {frequency}"
                ));
            }
        }
    }
    let mut actual_postings = 0u64;
    for item in postings.range((ns, "", 0u128)..).map_err(storage)? {
        let (key, _) = item.map_err(storage)?;
        if key.value().0 != ns {
            break;
        }
        actual_postings += 1;
    }
    if actual_postings != expected_postings {
        problems.push(format!(
            "{actual_postings} postings in the text index, expected {expected_postings}"
        ));
    }
    let mut actual_lengths = 0u64;
    for item in lengths.range((ns, 0u128)..).map_err(storage)? {
        let (key, _) = item.map_err(storage)?;
        if key.value().0 != ns {
            break;
        }
        actual_lengths += 1;
    }
    if actual_lengths != documents {
        problems.push(format!(
            "{actual_lengths} text lengths for {documents} texts"
        ));
    }
    let counted = stats
        .get(ns)
        .map_err(storage)?
        .map(|g| g.value())
        .unwrap_or((0, 0));
    if counted != (documents, total_terms) {
        problems.push(format!(
            "text statistics say {counted:?} (texts, terms), the index has {:?}",
            (documents, total_terms)
        ));
    }
    Ok(documents)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizes_without_accents_or_stopwords() {
        assert_eq!(
            tokenize("La Canción de Ana: ¡Rust y Grafos 2026!"),
            vec!["cancion", "ana", "rust", "grafos", "2026"]
        );
        assert_eq!(
            tokenize("The quick brown fox, it is"),
            vec!["quick", "brown", "fox"]
        );
        assert!(tokenize("a b c").is_empty());
    }
}
