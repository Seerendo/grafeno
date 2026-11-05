//! Finding what a text mentions, without a language model: patterns
//! (emails, URLs, handles, hashtags, dates, amounts, identifiers), the
//! names the namespace already knows (a gazetteer), and capitalized
//! phrases that may be new names.
//!
//! Every find carries its position as character offsets (Unicode scalar
//! values, `start..end`) in the original text, and a normalized value.

use std::collections::HashMap;
use std::sync::LazyLock;

use aho_corasick::{AhoCorasick, MatchKind};
use grafeno::NodeId;
use regex::Regex;
use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;

/// What a find is.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "label")]
pub enum Kind {
    Email,
    Url,
    /// `@name`
    Handle,
    /// `#topic`
    Hashtag,
    /// A calendar date, normalized to `YYYY-MM-DD`.
    Date,
    /// A sum of money, normalized to `CUR value` (`USD 1250.50`).
    Amount,
    /// A value of a custom identifier pattern, with the label its nodes take.
    Identifier(String),
    /// A name the namespace knows.
    Name,
    /// Capitalized words that may be a name not yet known.
    Phrase,
}

impl Kind {
    pub fn name(&self) -> String {
        match self {
            Kind::Email => "email".into(),
            Kind::Url => "url".into(),
            Kind::Handle => "handle".into(),
            Kind::Hashtag => "hashtag".into(),
            Kind::Date => "date".into(),
            Kind::Amount => "amount".into(),
            Kind::Identifier(label) => format!("identifier:{label}"),
            Kind::Name => "name".into(),
            Kind::Phrase => "phrase".into(),
        }
    }
}

/// Something a text mentions.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Found {
    pub kind: Kind,
    /// As written.
    pub surface: String,
    /// Character offsets in the text.
    pub start: usize,
    pub end: usize,
    /// Normalized: lowercase email, ISO date, `CUR value`, the name's
    /// normalized form.
    pub value: String,
}

/// A custom identifier: values matching `pattern` become nodes with `label`
/// (an invoice number, a ticket, a passport).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct IdentifierPattern {
    pub label: String,
    pub pattern: String,
}

/// Byte offset → character offset, for one text.
struct Chars(Vec<usize>);

impl Chars {
    fn new(text: &str) -> Self {
        let mut map = vec![0; text.len() + 1];
        let mut count = 0;
        for (byte, _) in text.char_indices() {
            map[byte] = count;
            count += 1;
        }
        map[text.len()] = count;
        // Bytes inside a character map to the one before.
        let mut last = 0;
        for slot in map.iter_mut() {
            if *slot == 0 && last > 0 {
                *slot = last;
            } else {
                last = *slot;
            }
        }
        Self(map)
    }

    fn at(&self, byte: usize) -> usize {
        self.0[byte]
    }
}

static EMAIL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[A-Za-z0-9._%+\-]+@[A-Za-z0-9\-]+(?:\.[A-Za-z0-9\-]+)*\.[A-Za-z]{2,}").unwrap()
});
static URL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)\bhttps?://[^\s<>"'`]+"#).unwrap());
static HANDLE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:^|[^\w@.])(@[A-Za-z0-9_]{2,30})\b").unwrap());
static HASHTAG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:^|[^\w&#])(#[\p{L}\p{N}_]{2,50})").unwrap());
static ISO_DATE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(\d{4})-(\d{2})-(\d{2})\b").unwrap());
static DMY_DATE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(\d{1,2})[/.](\d{1,2})[/.](\d{4})\b").unwrap());
static AMOUNT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(?:(USD|EUR|GBP|PEN|MXN|COP|CLP|ARS|BRL|S/\.?|US\$|\$|€|£)\s?(\d+(?:[.,\u{a0} ]\d{3})*(?:[.,]\d{1,2})?)|(\d+(?:[.,\u{a0} ]\d{3})*(?:[.,]\d{1,2})?)\s?(USD|EUR|GBP|PEN|MXN|COP|CLP|ARS|BRL|dólares|dolares|euros|soles|pesos)\b)",
    )
    .unwrap()
});
static PHRASE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"\b\p{Lu}[\p{Ll}\p{Lu}'’\-]+(?:\s+(?:(?:de|del|la|las|los|y|da|do|dos|van|von|der|di|le|of|the|and)\s+){0,2}\p{Lu}[\p{Ll}\p{Lu}'’\-]+){0,4}",
    )
    .unwrap()
});

fn currency(code: &str) -> &'static str {
    match code.to_lowercase().trim_end_matches('.') {
        "usd" | "$" | "us$" | "dólares" | "dolares" => "USD",
        "eur" | "€" | "euros" => "EUR",
        "gbp" | "£" => "GBP",
        "pen" | "s/" | "soles" => "PEN",
        "mxn" => "MXN",
        "cop" => "COP",
        "clp" => "CLP",
        "ars" | "pesos" => "ARS",
        "brl" => "BRL",
        _ => "XXX",
    }
}

/// `1.250,50`, `1,250.50`, `1 250` → a number: the last `.` or `,` followed
/// by one or two digits is the decimal point; the others group thousands.
fn amount_value(text: &str) -> Option<f64> {
    let digits: String = text
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '\u{a0}')
        .collect();
    let decimal = digits
        .rfind(['.', ','])
        .filter(|at| digits.len() - at - 1 <= 2);
    let mut out = String::new();
    for (at, c) in digits.char_indices() {
        if c.is_ascii_digit() {
            out.push(c);
        } else if Some(at) == decimal {
            out.push('.');
        }
    }
    out.parse().ok()
}

fn valid_date(y: i64, m: i64, d: i64) -> bool {
    (1..=12).contains(&m) && (1..=31).contains(&d) && (1000..=2999).contains(&y)
}

/// Every pattern in `text`. Dates written `d/m/yyyy` are read day first
/// (with `month_first`, month first).
pub fn patterns(
    text: &str,
    identifiers: &[(IdentifierPattern, Regex)],
    month_first: bool,
) -> Vec<Found> {
    let chars = Chars::new(text);
    let mut out = Vec::new();
    let mut add = |kind: Kind, start: usize, end: usize, value: String| {
        out.push(Found {
            kind,
            surface: text[start..end].to_owned(),
            start: chars.at(start),
            end: chars.at(end),
            value,
        });
    };
    let mut emails = Vec::new();
    for m in EMAIL.find_iter(text) {
        emails.push(m.range());
        add(Kind::Email, m.start(), m.end(), m.as_str().to_lowercase());
    }
    for m in URL.find_iter(text) {
        let trimmed = m
            .as_str()
            .trim_end_matches(['.', ',', ';', ':', ')', ']', '!', '?']);
        add(
            Kind::Url,
            m.start(),
            m.start() + trimmed.len(),
            trimmed.to_owned(),
        );
    }
    for c in HANDLE.captures_iter(text) {
        let m = c.get(1).unwrap();
        if !emails.iter().any(|e| e.contains(&m.start())) {
            add(Kind::Handle, m.start(), m.end(), m.as_str().to_lowercase());
        }
    }
    for c in HASHTAG.captures_iter(text) {
        let m = c.get(1).unwrap();
        add(Kind::Hashtag, m.start(), m.end(), m.as_str().to_lowercase());
    }
    for c in ISO_DATE.captures_iter(text) {
        let n = |i: usize| c[i].parse::<i64>().unwrap_or(0);
        if valid_date(n(1), n(2), n(3)) {
            let m = c.get(0).unwrap();
            add(Kind::Date, m.start(), m.end(), m.as_str().to_owned());
        }
    }
    for c in DMY_DATE.captures_iter(text) {
        let n = |i: usize| c[i].parse::<i64>().unwrap_or(0);
        let (d, mo) = if month_first {
            (n(2), n(1))
        } else {
            (n(1), n(2))
        };
        if valid_date(n(3), mo, d) {
            let m = c.get(0).unwrap();
            add(
                Kind::Date,
                m.start(),
                m.end(),
                format!("{:04}-{mo:02}-{d:02}", n(3)),
            );
        }
    }
    for c in AMOUNT.captures_iter(text) {
        let (code, number) = match (c.get(1), c.get(2), c.get(3), c.get(4)) {
            (Some(code), Some(number), _, _) => (code.as_str(), number.as_str()),
            (_, _, Some(number), Some(code)) => (code.as_str(), number.as_str()),
            _ => continue,
        };
        if let Some(value) = amount_value(number) {
            let m = c.get(0).unwrap();
            let trimmed = m.as_str().trim_end();
            add(
                Kind::Amount,
                m.start(),
                m.start() + trimmed.len(),
                format!("{} {value}", currency(code)),
            );
        }
    }
    for (pattern, regex) in identifiers {
        for m in regex.find_iter(text) {
            add(
                Kind::Identifier(pattern.label.clone()),
                m.start(),
                m.end(),
                m.as_str().to_uppercase(),
            );
        }
    }
    out.sort_by_key(|f| (f.start, std::cmp::Reverse(f.end)));
    out
}

/// Capitalized words that open sentences or are common words in Spanish
/// and English: not names on their own.
const NOT_NAMES: &[&str] = &[
    "el",
    "la",
    "los",
    "las",
    "un",
    "una",
    "unos",
    "unas",
    "de",
    "del",
    "y",
    "o",
    "en",
    "con",
    "por",
    "para",
    "que",
    "se",
    "su",
    "sus",
    "al",
    "lo",
    "es",
    "hoy",
    "ayer",
    "mañana",
    "manana",
    "este",
    "esta",
    "estos",
    "estas",
    "ese",
    "esa",
    "pero",
    "si",
    "no",
    "ya",
    "muy",
    "más",
    "mas",
    "yo",
    "tú",
    "tu",
    "él",
    "ella",
    "nosotros",
    "ellos",
    "ellas",
    "the",
    "a",
    "an",
    "and",
    "or",
    "in",
    "on",
    "at",
    "to",
    "of",
    "for",
    "with",
    "by",
    "is",
    "it",
    "its",
    "this",
    "that",
    "these",
    "those",
    "but",
    "if",
    "not",
    "we",
    "you",
    "he",
    "she",
    "they",
    "i",
    "my",
    "our",
    "your",
    "today",
    "yesterday",
    "tomorrow",
    "lunes",
    "martes",
    "miércoles",
    "miercoles",
    "jueves",
    "viernes",
    "sábado",
    "sabado",
    "domingo",
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "friday",
    "saturday",
    "sunday",
    "enero",
    "febrero",
    "marzo",
    "abril",
    "mayo",
    "junio",
    "julio",
    "agosto",
    "septiembre",
    "octubre",
    "noviembre",
    "diciembre",
    "january",
    "february",
    "march",
    "april",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
];

/// Capitalized phrases (one to five words, with `de`, `van`, `of`… inside)
/// that may be names: a single word opening a sentence, and common words,
/// are left out.
pub fn phrases(text: &str) -> Vec<Found> {
    let chars = Chars::new(text);
    let mut out = Vec::new();
    for m in PHRASE.find_iter(text) {
        let mut words: Vec<&str> = m.as_str().split_whitespace().collect();
        let mut start = m.start();
        // Leading common words ("El", "The") are not part of the name.
        while let Some(first) = words.first() {
            if NOT_NAMES.contains(&grafeno::normalize_name(first).as_str()) {
                start += text[start..].find(first).unwrap_or(0) + first.len();
                start += text[start..].len() - text[start..].trim_start().len();
                words.remove(0);
            } else {
                break;
            }
        }
        if words.is_empty() {
            continue;
        }
        let opens = |start: usize| {
            let before = text[..start].trim_end();
            before.is_empty() || before.ends_with(['.', '!', '?', ':', '\n', '"', '«', '“'])
        };
        // "Hablamos de Proyecto Atlas": a capitalized word that opens the
        // sentence and is followed by a connector is not part of the name.
        while words.len() > 1
            && opens(start)
            && words[1].chars().next().is_some_and(char::is_lowercase)
        {
            let skip = words
                .iter()
                .skip(1)
                .take_while(|w| w.chars().next().is_some_and(char::is_lowercase))
                .count();
            for word in words.drain(..=skip) {
                start += text[start..].find(word).unwrap_or(0) + word.len();
                start += text[start..].len() - text[start..].trim_start().len();
            }
        }
        if words.is_empty() {
            continue;
        }
        let surface = words.join(" ");
        let opens_sentence = opens(start);
        if words.len() == 1 && (opens_sentence || words[0].chars().count() < 3) {
            continue;
        }
        let end = m.end();
        out.push(Found {
            kind: Kind::Phrase,
            surface: text[start..end].to_owned(),
            start: chars.at(start),
            end: chars.at(end),
            value: grafeno::normalize_name(&surface),
        });
    }
    out
}

/// Text normalized as names are ([`grafeno::normalize_name`]), with the
/// character offset in the original of each normalized character.
pub fn normalize_with_offsets(text: &str) -> (String, Vec<usize>) {
    let mut out = String::new();
    let mut offsets = Vec::new();
    let mut space = false;
    for (at, original) in text.chars().enumerate() {
        for c in std::iter::once(original)
            .nfd()
            .filter(|c| !is_combining_mark(*c))
        {
            if c.is_alphanumeric() {
                if space && !out.is_empty() {
                    out.push(' ');
                    offsets.push(at);
                }
                space = false;
                for lower in c.to_lowercase() {
                    out.push(lower);
                    offsets.push(at);
                }
            } else {
                space = true;
            }
        }
    }
    (out, offsets)
}

/// The names a namespace knows, for finding them in texts.
pub struct Gazetteer {
    automaton: Option<AhoCorasick>,
    names: Vec<String>,
    nodes: Vec<Vec<NodeId>>,
}

impl Gazetteer {
    /// From normalized names and the nodes that have them. Names shorter
    /// than `min_len` characters, or that are common words, are left out.
    pub fn new(entries: impl IntoIterator<Item = (String, NodeId)>, min_len: usize) -> Self {
        let mut by_name: HashMap<String, Vec<NodeId>> = HashMap::new();
        for (name, node) in entries {
            if name.chars().count() < min_len || NOT_NAMES.contains(&name.as_str()) {
                continue;
            }
            let nodes = by_name.entry(name).or_default();
            if !nodes.contains(&node) {
                nodes.push(node);
            }
        }
        let mut names: Vec<String> = by_name.keys().cloned().collect();
        names.sort();
        let nodes = names.iter().map(|name| by_name[name].clone()).collect();
        let automaton = (!names.is_empty()).then(|| {
            AhoCorasick::builder()
                .match_kind(MatchKind::LeftmostLongest)
                .build(&names)
                .expect("names build an automaton")
        });
        Self {
            automaton,
            names,
            nodes,
        }
    }

    pub fn len(&self) -> usize {
        self.names.len()
    }

    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    /// The known names in `text` (whole words only, the longest first where
    /// two overlap), with the nodes that have each.
    pub fn find(&self, text: &str) -> Vec<(Found, Vec<NodeId>)> {
        let Some(automaton) = &self.automaton else {
            return Vec::new();
        };
        let (normalized, offsets) = normalize_with_offsets(text);
        let bytes = normalized.as_bytes();
        let char_at = Chars::new(&normalized);
        let original: Vec<char> = text.chars().collect();
        let mut out = Vec::new();
        for m in automaton.find_iter(&normalized) {
            let boundary_before = m.start() == 0 || bytes[m.start() - 1] == b' ';
            let boundary_after = m.end() == bytes.len() || bytes[m.end()] == b' ';
            if !boundary_before || !boundary_after {
                continue;
            }
            let (first, last) = (char_at.at(m.start()), char_at.at(m.end()) - 1);
            let (start, end) = (offsets[first], offsets[last] + 1);
            let name = &self.names[m.pattern().as_usize()];
            out.push((
                Found {
                    kind: Kind::Name,
                    surface: original[start..end].iter().collect(),
                    start,
                    end,
                    value: name.clone(),
                },
                self.nodes[m.pattern().as_usize()].clone(),
            ));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(found: &[Found], kind: Kind) -> Vec<String> {
        found
            .iter()
            .filter(|f| f.kind == kind)
            .map(|f| f.value.clone())
            .collect()
    }

    #[test]
    fn patterns_are_found_and_normalized() {
        let text = "Escríbele a Ana.Garcia@Acme.com o @ana_g. Ver https://acme.com/a?b=1. \
                    #Proyecto2026 el 03/04/2026 y el 2026-05-01: pagó S/ 1.250,50 y USD 300, \
                    además 2,000.75 dólares. Factura INV-2026-0042.";
        let ids = vec![(
            IdentifierPattern {
                label: "Invoice".into(),
                pattern: r"\bINV-\d{4}-\d{4}\b".into(),
            },
            Regex::new(r"\bINV-\d{4}-\d{4}\b").unwrap(),
        )];
        let found = patterns(text, &ids, false);
        assert_eq!(values(&found, Kind::Email), ["ana.garcia@acme.com"]);
        assert_eq!(values(&found, Kind::Handle), ["@ana_g"]);
        assert_eq!(values(&found, Kind::Url), ["https://acme.com/a?b=1"]);
        assert_eq!(values(&found, Kind::Hashtag), ["#proyecto2026"]);
        assert_eq!(values(&found, Kind::Date), ["2026-04-03", "2026-05-01"]);
        assert_eq!(
            values(&found, Kind::Amount),
            ["PEN 1250.5", "USD 300", "USD 2000.75"]
        );
        assert_eq!(
            values(&found, Kind::Identifier("Invoice".into())),
            ["INV-2026-0042"]
        );
        // Offsets are characters: the accent in "Escríbele" counts once.
        let email = found.iter().find(|f| f.kind == Kind::Email).unwrap();
        let chars: Vec<char> = text.chars().collect();
        assert_eq!(
            chars[email.start..email.end].iter().collect::<String>(),
            "Ana.Garcia@Acme.com"
        );
        assert_eq!(patterns("13/13/2026 y 31/02/x", &[], false).len(), 0);
        assert_eq!(
            values(&patterns("04/03/2026", &[], true), Kind::Date),
            ["2026-04-03"]
        );
    }

    #[test]
    fn phrases_skip_sentence_openers_and_common_words() {
        let found = phrases(
            "Hoy hablé con María José de la Cruz en Lima. Ella trabaja en Acme Corp. El Banco Central subió la tasa.",
        );
        let surfaces: Vec<&str> = found.iter().map(|f| f.surface.as_str()).collect();
        assert_eq!(
            surfaces,
            [
                "María José de la Cruz",
                "Lima",
                "Acme Corp",
                "Banco Central"
            ]
        );
        assert_eq!(found[0].value, "maria jose de la cruz");
        assert!(phrases("Ella llegó.").is_empty());
        let opener: Vec<String> = phrases("Hablamos de Proyecto Atlas.")
            .into_iter()
            .map(|f| f.surface)
            .collect();
        assert_eq!(opener, ["Proyecto Atlas"]);
    }

    #[test]
    fn the_gazetteer_finds_whole_names_with_their_offsets() {
        let a = NodeId::new();
        let b = NodeId::new();
        let c = NodeId::new();
        let gazetteer = Gazetteer::new(
            [
                ("ana garcia".to_owned(), a),
                ("ana".to_owned(), b),
                ("acme".to_owned(), c),
                ("el".to_owned(), c),
            ],
            3,
        );
        let text = "Reunión con ANA GARCÍA de Acme; luego Ana sola. Banana no.";
        let found = gazetteer.find(text);
        let got: Vec<(&str, &str)> = found
            .iter()
            .map(|(f, _)| (f.surface.as_str(), f.value.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                ("ANA GARCÍA", "ana garcia"),
                ("Acme", "acme"),
                ("Ana", "ana")
            ]
        );
        assert_eq!(found[0].1, [a]);
        let chars: Vec<char> = text.chars().collect();
        let (first, _) = &found[0];
        assert_eq!(
            chars[first.start..first.end].iter().collect::<String>(),
            "ANA GARCÍA"
        );
    }
}
