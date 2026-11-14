//! Which indexed functions a function just written looks like
//! (carrick-cloud#3490).
//!
//! One answerer for the question, native in the scanner and over wasm in the
//! hosted code. At the moment of writing, the new function has no description
//! and no vector, so it is asked by what one parse gives: its name, its body's
//! retrieval tokens and its written signature, as one bag of words. Each
//! indexed function is a bag of the same kind and the bags are scored with
//! Okapi BM25.
//!
//! The bag is the cloud's lexical leg (`lambdas/check-or-upload/lexical.ts`,
//! `tokenize` and `definitionBag`), ported word for word and pinned by
//! `tests/fixtures/similar_bag_parity.json`, which holds that code's own output:
//!
//! - a function's *described* words are its name, its intent sentence when the
//!   index holds one, and its tokens, each split at identifier boundaries;
//!   they set the document length, and a word repeated there counts again;
//! - its signature's words count once however often they are written and add
//!   no length, because a type written five times is one piece of evidence.
//!
//! BM25 takes `k1 = 1.2`, `b = 0.75` and the non-negative idf
//! `ln(1 + (N - df + 0.5) / (df + 0.5))`. The score is divided by the idf mass
//! of the query's words, so it reads 0 to 1 whatever the query's length.
//!
//! The answer has no score floor: a function sharing any word with the query
//! is a candidate, and the cut is [`SIMILAR_ROWS`]. Dropped before ranking: the
//! function's own file, any function the new one calls or that calls it, and
//! rows nobody can import (callbacks the extractor named, the module-scope
//! row). The member named like the new one comes first. Reference:
//! `docs/local-mode-output.md`.

use std::collections::{BTreeSet, HashMap, HashSet};

/// Rows one answer holds at most.
pub const SIMILAR_ROWS: usize = 6;

const K1: f64 = 1.2;
const B: f64 = 0.75;
/// The name the scanner gives a file's module-scope calls.
const MODULE_ROW: &str = "<module>";
/// The suffix the scanner gives a callback it names itself.
const CALLBACK_SUFFIX: &str = "_handler";

/// An indexed function, as the ranker reads it.
pub struct SimilarDoc<'a> {
    /// As the index spells it: `name`, or `Class.member`.
    pub name: &'a str,
    /// An opaque key for the file it sits in: two functions with the same key
    /// are in one file. Callers qualify it with the repo.
    pub file: &'a str,
    pub intent: Option<&'a str>,
    pub tokens: &'a [String],
    pub signature: Option<&'a str>,
    /// Whether other files can import it; breaks a score tie.
    pub exported: bool,
    /// Its first line in the file; breaks a score tie.
    pub line: u32,
    /// The names it calls, as written at each call site.
    pub calls: Vec<&'a str>,
}

/// The function just written.
pub struct SimilarQuery<'a> {
    pub name: &'a str,
    /// The same key as [`SimilarDoc::file`], for the file being edited.
    pub file: &'a str,
    pub tokens: &'a [String],
    pub signature: Option<&'a str>,
    /// The names its body calls. When empty, its tokens stand in for them.
    pub callees: &'a [String],
}

/// One ranked function: its position in the iterator given to
/// [`SimilarIndex::build`], and its score from 0 to 1.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SimilarRow {
    pub doc: usize,
    pub score: f64,
}

/// A function's searchable words: every term in order (the described ones,
/// then the signature's once each) and the length BM25 normalises it on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bag {
    pub terms: Vec<String>,
    pub described_len: usize,
}

/// Lower-cased words of `text`: everything that is not an ASCII letter or digit
/// separates, then each run splits at a lower-to-upper step (`fooBar`), an
/// acronym's end (`XMLHttp`) and a letter-digit step (`base64`).
pub fn tokenize(text: &str) -> Vec<String> {
    let mut bytes: Vec<u8> = text
        .bytes()
        .map(|b| if b.is_ascii_alphanumeric() { b } else { b' ' })
        .collect();
    bytes = split_between(&bytes, |a, b| {
        (a.is_ascii_lowercase() || a.is_ascii_digit()) && b.is_ascii_uppercase()
    });
    bytes = split_acronyms(&bytes);
    bytes = split_between(&bytes, |a, b| a.is_ascii_alphabetic() && b.is_ascii_digit());
    bytes = split_between(&bytes, |a, b| a.is_ascii_digit() && b.is_ascii_alphabetic());
    bytes
        .split(|&b| b == b' ')
        .filter(|word| !word.is_empty())
        .map(|word| word.to_ascii_lowercase())
        .map(|word| String::from_utf8(word).unwrap_or_default())
        .collect()
}

/// A space between every adjacent pair `step` names.
fn split_between(bytes: &[u8], step: fn(u8, u8) -> bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() + 4);
    for (at, &byte) in bytes.iter().enumerate() {
        if at > 0 && step(bytes[at - 1], byte) {
            out.push(b' ');
        }
        out.push(byte);
    }
    out
}

/// A space before the last capital of a run of two or more capitals that a
/// lower-case letter follows: `XMLHttp` reads `XML Http`.
fn split_acronyms(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() + 4);
    let mut at = 0;
    while at < bytes.len() {
        if !bytes[at].is_ascii_uppercase() {
            out.push(bytes[at]);
            at += 1;
            continue;
        }
        let end = at
            + bytes[at..]
                .iter()
                .take_while(|b| b.is_ascii_uppercase())
                .count();
        let cut = end - at >= 2 && bytes.get(end).is_some_and(u8::is_ascii_lowercase);
        out.extend_from_slice(&bytes[at..if cut { end - 1 } else { end }]);
        if cut {
            out.push(b' ');
            out.push(bytes[end - 1]);
        }
        at = end;
    }
    out
}

/// The bag of one function. The same rule builds a document's and a query's
/// words, so a word in a query meets the word in a document.
pub fn bag(name: &str, intent: Option<&str>, tokens: &[String], signature: Option<&str>) -> Bag {
    let mut terms = tokenize(name);
    terms.extend(intent.map(tokenize).unwrap_or_default());
    terms.extend(tokens.iter().flat_map(|token| tokenize(token)));
    let described_len = terms.len();
    let mut seen = HashSet::new();
    for word in signature.map(tokenize).unwrap_or_default() {
        if seen.insert(word.clone()) {
            terms.push(word);
        }
    }
    Bag {
        terms,
        described_len,
    }
}

/// The part after any `Class.`.
fn member(name: &str) -> &str {
    name.rsplit('.').next().unwrap_or(name)
}

/// What the drops read about one indexed function.
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
struct Meta {
    file: u32,
    member: u32,
    exported: bool,
    line: u32,
}

/// Every indexed function, read once and ranked against any number of queries.
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SimilarIndex {
    /// By input position; `None` for a row nobody can import.
    docs: Vec<Option<Meta>>,
    /// By input position: the name as the index spells it.
    names: Vec<String>,
    lengths: Vec<u32>,
    live: usize,
    total_length: u64,
    terms: HashMap<String, u32>,
    /// Per term: (doc, times it appears in that doc's bag).
    postings: Vec<Vec<(u32, u32)>>,
    files: HashMap<String, u32>,
    /// By file id: the key as given, for the tie-break by path.
    file_keys: Vec<String>,
    members: HashMap<String, u32>,
    /// Member name of a callee -> the docs that call it.
    callers: HashMap<u32, Vec<u32>>,
}

fn intern(map: &mut HashMap<String, u32>, key: &str) -> u32 {
    if let Some(&id) = map.get(key) {
        return id;
    }
    let id = map.len() as u32;
    map.insert(key.to_string(), id);
    id
}

impl SimilarIndex {
    pub fn build<'a>(docs: impl IntoIterator<Item = SimilarDoc<'a>>) -> Self {
        let mut index = Self {
            docs: Vec::new(),
            names: Vec::new(),
            lengths: Vec::new(),
            live: 0,
            total_length: 0,
            terms: HashMap::new(),
            postings: Vec::new(),
            files: HashMap::new(),
            file_keys: Vec::new(),
            members: HashMap::new(),
            callers: HashMap::new(),
        };
        let mut counts: HashMap<u32, u32> = HashMap::new();
        for doc in docs {
            let position = index.docs.len() as u32;
            let importable = !doc.name.is_empty()
                && !doc.file.is_empty()
                && doc.name != MODULE_ROW
                && !doc.name.ends_with(CALLBACK_SUFFIX);
            index.names.push(doc.name.to_string());
            if !importable {
                index.docs.push(None);
                index.lengths.push(0);
                continue;
            }
            let bag = bag(doc.name, doc.intent, doc.tokens, doc.signature);
            counts.clear();
            for term in &bag.terms {
                let id = match index.terms.get(term) {
                    Some(&id) => id,
                    None => {
                        let id = index.postings.len() as u32;
                        index.terms.insert(term.clone(), id);
                        index.postings.push(Vec::new());
                        id
                    }
                };
                *counts.entry(id).or_default() += 1;
            }
            for (&term, &count) in counts.iter() {
                index.postings[term as usize].push((position, count));
            }
            for call in &doc.calls {
                let callee = intern(&mut index.members, member(call));
                index.callers.entry(callee).or_default().push(position);
            }
            let file = intern(&mut index.files, doc.file);
            if file as usize == index.file_keys.len() {
                index.file_keys.push(doc.file.to_string());
            }
            let meta = Meta {
                file,
                member: intern(&mut index.members, member(doc.name)),
                exported: doc.exported,
                line: doc.line,
            };
            index.docs.push(Some(meta));
            index.lengths.push(bag.described_len as u32);
            index.live += 1;
            index.total_length += bag.described_len as u64;
        }
        // Each posting list in doc order, so one run gives one answer.
        for list in &mut index.postings {
            list.sort_unstable();
        }
        index
    }

    fn meta(&self, doc: usize) -> &Meta {
        self.docs[doc].as_ref().expect("a ranked row is importable")
    }

    /// The functions most like `query`, best first, at most [`SIMILAR_ROWS`].
    pub fn rank(&self, query: &SimilarQuery<'_>) -> Vec<SimilarRow> {
        let words: BTreeSet<String> = bag(query.name, None, query.tokens, query.signature)
            .terms
            .into_iter()
            .collect();
        if words.is_empty() || self.live == 0 {
            return Vec::new();
        }
        let total = self.live as f64;
        let average = (self.total_length as f64 / total).max(1.0);
        let mut sums: HashMap<u32, f64> = HashMap::new();
        let mut mass = 0.0;
        for word in &words {
            let postings = self.terms.get(word).map(|&id| &self.postings[id as usize]);
            let df = postings.map_or(0, Vec::len) as f64;
            let idf = (1.0 + (total - df + 0.5) / (df + 0.5)).ln();
            mass += idf;
            for &(doc, count) in postings.into_iter().flatten() {
                let count = f64::from(count);
                let length = f64::from(self.lengths[doc as usize]);
                *sums.entry(doc).or_default() +=
                    idf * count * (K1 + 1.0) / (count + K1 * (1.0 - B + B * length / average));
            }
        }
        if mass == 0.0 {
            return Vec::new();
        }
        let edited = self.files.get(query.file).copied();
        let mine = self.members.get(member(query.name)).copied();
        // With no call recorded, a token that is a whole member name stands in.
        let called: HashSet<u32> = if query.callees.is_empty() {
            query
                .tokens
                .iter()
                .filter_map(|token| self.members.get(token.as_str()).copied())
                .collect()
        } else {
            query
                .callees
                .iter()
                .filter_map(|call| self.members.get(member(call)).copied())
                .collect()
        };
        let callers: HashSet<u32> = mine
            .and_then(|id| self.callers.get(&id))
            .into_iter()
            .flatten()
            .copied()
            .collect();
        let mut rows: Vec<(bool, SimilarRow)> = sums
            .into_iter()
            .filter_map(|(doc, sum)| {
                let meta = self.docs[doc as usize].as_ref()?;
                let kept = Some(meta.file) != edited
                    && !called.contains(&meta.member)
                    && !callers.contains(&doc);
                kept.then(|| {
                    (
                        Some(meta.member) == mine,
                        SimilarRow {
                            doc: doc as usize,
                            score: sum / mass,
                        },
                    )
                })
            })
            .collect();
        // A total order that reads nothing about the order the index was fed:
        // the same-member row, score, exported, file path, line, then the name.
        // Only a function written twice at one line of one file falls to the
        // input position.
        rows.sort_by(|(same_a, a), (same_b, b)| {
            let (meta_a, meta_b) = (self.meta(a.doc), self.meta(b.doc));
            same_b
                .cmp(same_a)
                .then_with(|| b.score.total_cmp(&a.score))
                .then_with(|| meta_b.exported.cmp(&meta_a.exported))
                .then_with(|| {
                    self.file_keys[meta_a.file as usize].cmp(&self.file_keys[meta_b.file as usize])
                })
                .then_with(|| meta_a.line.cmp(&meta_b.line))
                .then_with(|| self.names[a.doc].cmp(&self.names[b.doc]))
                .then_with(|| a.doc.cmp(&b.doc))
        });
        rows.into_iter()
            .take(SIMILAR_ROWS)
            .map(|(_, row)| row)
            .collect()
    }
}

/// The ranker over wasm (carrick-cloud#3490 T5): JSON in, JSON out, in the
/// shape of `tests/fixtures/similar_wasm.vectors.json`. The cloud builds one
/// index from an index's function rows and ranks each `code` seed against it.
#[cfg(feature = "wasm")]
mod wasm {
    use serde::Deserialize;
    use wasm_bindgen::prelude::*;

    use super::{SimilarDoc, SimilarIndex, SimilarQuery};

    #[derive(Deserialize)]
    struct Row {
        name: String,
        file: String,
        #[serde(default)]
        intent: Option<String>,
        #[serde(default)]
        tokens: Vec<String>,
        #[serde(default)]
        signature: Option<String>,
        #[serde(default)]
        exported: bool,
        #[serde(default)]
        line: u32,
        #[serde(default)]
        calls: Vec<String>,
    }

    #[derive(Deserialize)]
    struct Query {
        name: String,
        file: String,
        #[serde(default)]
        tokens: Vec<String>,
        #[serde(default)]
        signature: Option<String>,
        #[serde(default)]
        callees: Vec<String>,
    }

    /// An index built once from function rows, ranked against many seeds.
    #[wasm_bindgen]
    pub struct SimilarRanker {
        index: SimilarIndex,
    }

    #[wasm_bindgen]
    impl SimilarRanker {
        /// `rows` is a JSON array of `{name, file, intent?, tokens?,
        /// signature?, exported?, line?, calls?}`. Throws on malformed JSON.
        #[wasm_bindgen(constructor)]
        pub fn from_json(rows: &str) -> Result<SimilarRanker, JsError> {
            let rows: Vec<Row> = serde_json::from_str(rows)?;
            let index = SimilarIndex::build(rows.iter().map(|row| SimilarDoc {
                name: &row.name,
                file: &row.file,
                intent: row.intent.as_deref(),
                tokens: &row.tokens,
                signature: row.signature.as_deref(),
                exported: row.exported,
                line: row.line,
                calls: row.calls.iter().map(String::as_str).collect(),
            }));
            Ok(SimilarRanker { index })
        }

        /// `query` is a JSON `{name, file, tokens?, signature?, callees?}`; the
        /// result is a JSON `[{doc, score}]`, best first, `doc` being the
        /// position of the row in the array given to the constructor. Throws
        /// on malformed JSON.
        #[wasm_bindgen(js_name = rank)]
        pub fn rank_json(&self, query: &str) -> Result<String, JsError> {
            let query: Query = serde_json::from_str(query)?;
            let rows = self.index.rank(&SimilarQuery {
                name: &query.name,
                file: &query.file,
                tokens: &query.tokens,
                signature: query.signature.as_deref(),
                callees: &query.callees,
            });
            let out: Vec<serde_json::Value> = rows
                .iter()
                .map(|row| serde_json::json!({ "doc": row.doc, "score": row.score }))
                .collect();
            Ok(serde_json::to_string(&out)?)
        }
    }
}

#[cfg(feature = "wasm")]
pub use wasm::SimilarRanker;
