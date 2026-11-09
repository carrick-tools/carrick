//! The edit-time lead: which indexed function a function the edit just wrote
//! looks like (carrick-cloud#3249).
//!
//! At edit time the new function has no description and no vector, so the
//! lookup keys on what one parse gives: its name and its body tokens, built by
//! the extractor the index used ([`crate::visitor::FunctionDefinition::tokens`]).
//! It scores them as an IDF-weighted cosine over token sets against every
//! indexed function the re-check already holds in memory (local blobs and the
//! cached hosted snapshot, every repo), outside the edited file. No network,
//! no model.
//!
//! The rule, with the constants in one place so a measure can move them:
//!
//! - a hit passes on a token score of [`LEAD_TOKEN_FLOOR`] or more, or on the
//!   same member name (the part after any `Class.`) with a token score of
//!   [`LEAD_NAME_TOKEN_FLOOR`] or more;
//! - a hit is dropped when either function calls the other: the new body names
//!   the hit among its calls (its tokens when it makes no call), or the hit's
//!   indexed `calls` names the new function;
//! - a hit whose written return type differs from the new function's written
//!   one is demoted: it leads only when no other hit passes, and then the lead
//!   names both types. An unwritten type never demotes;
//! - callbacks the extractor named itself and `<module>` rows are never hits.
//!
//! The features are the tokens (less the `VARIADIC` marker) plus each token's
//! camelCase words longer than two characters, lower-cased; the weights are
//! `ln((N + 1) / (df + 1)) + 1` over the indexed functions, deduplicated by
//! repo, file and name so a file copied into several services counts once.
//! The floors were fitted on that exact instrument; changing a feature or a
//! weight changes what they mean.

use std::collections::{BTreeSet, HashMap, HashSet};

use crate::cloud_storage::CloudRepoData;
use crate::visitor::{FunctionDefinition, MODULE_SCOPE_KEY};

use super::contract::{Lead, LeadBasis, LeadRef};

/// A token score at or above this leads on its own.
pub(super) const LEAD_TOKEN_FLOOR: f64 = 0.70;
/// A token score at or above this leads when the member names match.
pub(super) const LEAD_NAME_TOKEN_FLOOR: f64 = 0.40;
/// Callers a lead names; the total is always given.
const MAX_CALLERS: usize = 3;
/// More functions like the lead that it names.
const MAX_ALSO: usize = 2;
/// The marker the extractor writes for a rest parameter; it is not a word.
const VARIADIC_TOKEN: &str = "VARIADIC";
/// The suffix [`crate::visitor::derive_handler_name`] gives a callback it names.
const SYNTHETIC_SUFFIX: &str = "_handler";

/// A function the edit wrote, as one parse of the edited file states it.
#[derive(Debug, Clone)]
pub(super) struct Query {
    /// As the source spells it: `name`, or `Class.member`.
    pub name: String,
    pub tokens: Vec<String>,
    /// The names its body calls, as written at each call site.
    pub callees: Vec<String>,
    /// Its return type, only when the source writes one.
    pub return_type: Option<String>,
}

impl Query {
    /// A function with nothing to score: it finds no lead.
    pub(super) fn named(name: &str) -> Self {
        Self {
            name: name.to_string(),
            tokens: Vec::new(),
            callees: Vec::new(),
            return_type: None,
        }
    }

    pub(super) fn of(definition: &FunctionDefinition, callees: Vec<String>) -> Self {
        Self {
            name: definition.name.clone(),
            tokens: definition.tokens.clone(),
            callees,
            return_type: definition
                .return_is_explicit
                .then(|| definition.return_type.clone())
                .flatten(),
        }
    }
}

struct Doc<'a> {
    repo: String,
    file: String,
    /// Index into `files`: the repo and file together.
    file_id: u32,
    definition: &'a FunctionDefinition,
    features: Vec<u32>,
}

/// Every indexed function the re-check holds, read once per re-check call and
/// scored against every function the edit wrote.
pub(super) struct LeadIndex<'a> {
    docs: Vec<Doc<'a>>,
    files: HashMap<(String, String), u32>,
    features: HashMap<String, u32>,
    idf: Vec<f64>,
    norms: Vec<f64>,
    postings: Vec<Vec<u32>>,
    /// (repo, file, member name) of a callee -> the docs that call it.
    callers: HashMap<(String, String, String), Vec<u32>>,
}

#[cfg(test)]
thread_local! {
    /// How many times this thread built an index: one per re-check call.
    pub(super) static BUILDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

impl<'a> LeadIndex<'a> {
    /// Read `blobs`, each labelled with the repo it belongs to.
    pub(super) fn build(blobs: impl IntoIterator<Item = (String, &'a CloudRepoData)>) -> Self {
        #[cfg(test)]
        BUILDS.with(|builds| builds.set(builds.get() + 1));
        let mut index = Self {
            docs: Vec::new(),
            files: HashMap::new(),
            features: HashMap::new(),
            idf: Vec::new(),
            norms: Vec::new(),
            postings: Vec::new(),
            callers: HashMap::new(),
        };
        let mut seen: HashSet<(String, String, String)> = HashSet::new();
        for (repo, blob) in blobs {
            let repo_key = repo.to_lowercase();
            for definition in blob.function_definitions.values() {
                if !is_candidate(definition) {
                    continue;
                }
                let file = path_of(definition);
                if !seen.insert((repo_key.clone(), file.clone(), definition.name.clone())) {
                    continue;
                }
                let file_id = index.file_id(&repo_key, &file);
                let features: Vec<u32> = features_of(&definition.tokens)
                    .into_iter()
                    .map(|feature| index.feature_id(feature))
                    .collect();
                let doc = index.docs.len() as u32;
                for &feature in &features {
                    index.postings[feature as usize].push(doc);
                }
                for call in &definition.calls {
                    index
                        .callers
                        .entry((
                            repo_key.clone(),
                            call.file_path.replace('\\', "/"),
                            member(&call.name).to_string(),
                        ))
                        .or_default()
                        .push(doc);
                }
                index.docs.push(Doc {
                    repo: repo.clone(),
                    file,
                    file_id,
                    definition,
                    features,
                });
            }
        }
        let total = index.docs.len() as f64;
        index.idf = index
            .postings
            .iter()
            .map(|docs| weight(total, docs.len()))
            .collect();
        index.norms = index
            .docs
            .iter()
            .map(|doc| {
                doc.features
                    .iter()
                    .map(|&f| index.idf[f as usize].powi(2))
                    .sum::<f64>()
                    .sqrt()
            })
            .collect();
        index
    }

    fn file_id(&mut self, repo: &str, file: &str) -> u32 {
        let next = self.files.len() as u32;
        *self
            .files
            .entry((repo.to_string(), file.to_string()))
            .or_insert(next)
    }

    fn feature_id(&mut self, feature: String) -> u32 {
        let next = self.features.len() as u32;
        let id = *self.features.entry(feature).or_insert(next);
        if id == next {
            self.postings.push(Vec::new());
        }
        id
    }

    /// The lead for `query`, written in `file` of `repo`, or none.
    pub(super) fn lead(&self, query: &Query, repo: &str, file: &str) -> Option<Lead> {
        let edited = self
            .files
            .get(&(repo.to_lowercase(), file.replace('\\', "/")))
            .copied();
        let mut known: Vec<(u32, f64)> = Vec::new();
        let mut squared = 0.0;
        for feature in features_of(&query.tokens) {
            match self.features.get(&feature) {
                Some(&id) => {
                    let idf = self.idf[id as usize];
                    squared += idf * idf;
                    known.push((id, idf));
                }
                // A word no indexed function has: it weighs in the norm only.
                None => squared += weight(self.docs.len() as f64, 0).powi(2),
            }
        }
        let scored = self.scored(&known, squared.sqrt(), &[edited]);
        let passing: Vec<(u32, f64)> = scored
            .into_iter()
            .filter(|&(doc, score)| passes(score, &query.name, &self.docs[doc as usize]))
            .filter(|&(doc, _)| !self.edge(query, doc))
            .collect();
        let demoted = |doc: u32| {
            let theirs = &self.docs[doc as usize].definition;
            match (
                &query.return_type,
                theirs.return_is_explicit,
                &theirs.return_type,
            ) {
                (Some(mine), true, Some(theirs)) => normal_type(mine) != normal_type(theirs),
                _ => false,
            }
        };
        let (doc, score, return_types) = match passing.iter().find(|&&(doc, _)| !demoted(doc)) {
            Some(&(doc, score)) => (doc, score, None),
            None => {
                let &(doc, score) = passing.first()?;
                let theirs = self.docs[doc as usize].definition.return_type.clone();
                (doc, score, Some([query.return_type.clone()?, theirs?]))
            }
        };
        let hit = &self.docs[doc as usize];
        let (callers, callers_total) = self.callers_of(doc);
        Some(Lead {
            name: hit.definition.name.clone(),
            repo: hit.repo.clone(),
            file: hit.file.clone(),
            line: hit.definition.line_number,
            end_line: hit.definition.end_line,
            score,
            basis: if member(&hit.definition.name) == member(&query.name) {
                LeadBasis::Name
            } else {
                LeadBasis::Tokens
            },
            return_types,
            callers,
            callers_total,
            also: self.also(doc, edited),
        })
    }

    /// Every doc sharing a feature with the query, outside `excluded` files,
    /// with its score rounded to three decimals, best first and then by repo,
    /// file and name, so two runs over one index give one order.
    fn scored(&self, known: &[(u32, f64)], norm: f64, excluded: &[Option<u32>]) -> Vec<(u32, f64)> {
        if norm == 0.0 {
            return Vec::new();
        }
        let mut sums: HashMap<u32, f64> = HashMap::new();
        for &(feature, idf) in known {
            for &doc in &self.postings[feature as usize] {
                if excluded.contains(&Some(self.docs[doc as usize].file_id)) {
                    continue;
                }
                *sums.entry(doc).or_default() += idf * idf;
            }
        }
        let mut scored: Vec<(u32, f64)> = sums
            .into_iter()
            .map(|(doc, sum)| {
                let score = sum / (norm * self.norms[doc as usize]);
                (doc, (score * 1000.0).round() / 1000.0)
            })
            .collect();
        scored.sort_by(|&(left, a), &(right, b)| {
            b.total_cmp(&a)
                .then_with(|| self.order(left).cmp(&self.order(right)))
        });
        scored
    }

    fn order(&self, doc: u32) -> (&str, &str, &str) {
        let doc = &self.docs[doc as usize];
        (&doc.repo, &doc.file, &doc.definition.name)
    }

    /// Whether the query and the doc call each other, in either direction.
    fn edge(&self, query: &Query, doc: u32) -> bool {
        let hit = self.docs[doc as usize].definition;
        let theirs = member(&hit.name);
        let forward = if query.callees.is_empty() {
            query.tokens.iter().any(|token| token == theirs)
        } else {
            query.callees.iter().any(|callee| member(callee) == theirs)
        };
        let mine = member(&query.name);
        forward || hit.calls.iter().any(|call| member(&call.name) == mine)
    }

    /// Whether two indexed functions call each other, in either direction.
    fn indexed_edge(&self, left: u32, right: u32) -> bool {
        let calls = |from: u32, to: u32| {
            let to = member(&self.docs[to as usize].definition.name);
            self.docs[from as usize]
                .definition
                .calls
                .iter()
                .any(|call| member(&call.name) == to)
        };
        calls(left, right) || calls(right, left)
    }

    fn callers_of(&self, doc: u32) -> (Vec<LeadRef>, usize) {
        let hit = &self.docs[doc as usize];
        let key = (
            hit.repo.to_lowercase(),
            hit.file.clone(),
            member(&hit.definition.name).to_string(),
        );
        let callers: BTreeSet<(&str, u32, &str)> = self
            .callers
            .get(&key)
            .into_iter()
            .flatten()
            .filter(|&&caller| caller != doc)
            .map(|&caller| {
                let caller = &self.docs[caller as usize];
                (
                    caller.file.as_str(),
                    caller.definition.line_number,
                    caller.definition.name.as_str(),
                )
            })
            .collect();
        let total = callers.len();
        let named = callers
            .into_iter()
            .take(MAX_CALLERS)
            .map(|(file, line, name)| LeadRef {
                name: name.to_string(),
                file: file.to_string(),
                line,
            })
            .collect();
        (named, total)
    }

    /// The same rule run from the hit: more functions like it, outside its
    /// file and the edited one, that neither call nor are called by it.
    fn also(&self, doc: u32, edited: Option<u32>) -> Vec<LeadRef> {
        let hit = &self.docs[doc as usize];
        let known: Vec<(u32, f64)> = hit
            .features
            .iter()
            .map(|&f| (f, self.idf[f as usize]))
            .collect();
        self.scored(
            &known,
            self.norms[doc as usize],
            &[Some(hit.file_id), edited],
        )
        .into_iter()
        .filter(|&(other, score)| passes(score, &hit.definition.name, &self.docs[other as usize]))
        .filter(|&(other, _)| !self.indexed_edge(doc, other))
        .take(MAX_ALSO)
        .map(|(other, _)| {
            let other = &self.docs[other as usize];
            LeadRef {
                name: other.definition.name.clone(),
                file: other.file.clone(),
                line: other.definition.line_number,
            }
        })
        .collect()
    }
}

/// The lead rule on one scored hit.
fn passes(score: f64, name: &str, hit: &Doc<'_>) -> bool {
    score >= LEAD_TOKEN_FLOOR
        || (score >= LEAD_NAME_TOKEN_FLOOR && member(&hit.definition.name) == member(name))
}

/// `ln((N + 1) / (df + 1)) + 1`.
fn weight(total: f64, df: usize) -> f64 {
    ((total + 1.0) / (df as f64 + 1.0)).ln() + 1.0
}

/// A function somebody wrote and can import: not the module-scope row, not a
/// callback the extractor named for itself.
fn is_candidate(definition: &FunctionDefinition) -> bool {
    !definition.name.is_empty()
        && definition.name != MODULE_SCOPE_KEY
        && !definition.name.ends_with(SYNTHETIC_SUFFIX)
        && !definition.file_path.as_os_str().is_empty()
}

fn path_of(definition: &FunctionDefinition) -> String {
    definition.file_path.to_string_lossy().replace('\\', "/")
}

/// The part after any `Class.`.
fn member(name: &str) -> &str {
    name.rsplit('.').next().unwrap_or(name)
}

fn normal_type(text: &str) -> String {
    text.split_whitespace().collect()
}

/// The tokens, less the rest-parameter marker, plus each token's camelCase
/// words longer than two characters, lower-cased and marked `w:`.
fn features_of(tokens: &[String]) -> BTreeSet<String> {
    let mut features = BTreeSet::new();
    for token in tokens {
        if token.is_empty() || token == VARIADIC_TOKEN {
            continue;
        }
        features.insert(token.clone());
        for word in camel_words(token) {
            if word.chars().count() > 2 {
                features.insert(format!("w:{}", word.to_lowercase()));
            }
        }
    }
    features
}

/// The words of a camelCase or PascalCase identifier, as the regex
/// `[A-Z]?[a-z]+|[A-Z]+(?![a-z])|\d+` finds them: `XMLHttpRequest2` reads
/// `XML`, `Http`, `Request`, `2`. Any other character separates words.
fn camel_words(token: &str) -> Vec<&str> {
    let bytes = token.as_bytes();
    let mut words = Vec::new();
    let mut at = 0;
    let run = |from: usize, test: fn(&u8) -> bool| {
        from + bytes[from..].iter().take_while(|b| test(b)).count()
    };
    while at < bytes.len() {
        let byte = bytes[at];
        if byte.is_ascii_lowercase() {
            let end = run(at, u8::is_ascii_lowercase);
            words.push(&token[at..end]);
            at = end;
        } else if byte.is_ascii_uppercase() {
            if bytes.get(at + 1).is_some_and(u8::is_ascii_lowercase) {
                let end = run(at + 1, u8::is_ascii_lowercase);
                words.push(&token[at..end]);
                at = end;
            } else {
                let end = run(at, u8::is_ascii_uppercase);
                // The last capital of a run starts the next word when a
                // lower-case letter follows it.
                let end = if end - at > 1 && bytes.get(end).is_some_and(u8::is_ascii_lowercase) {
                    end - 1
                } else {
                    end
                };
                words.push(&token[at..end]);
                at = end;
            }
        } else if byte.is_ascii_digit() {
            let end = run(at, u8::is_ascii_digit);
            words.push(&token[at..end]);
            at = end;
        } else {
            at += 1;
        }
    }
    words
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camel_words_read_as_the_probe_regex_does() {
        assert_eq!(
            camel_words("XMLHttpRequest2"),
            vec!["XML", "Http", "Request", "2"]
        );
        assert_eq!(camel_words("joinRelative"), vec!["join", "Relative"]);
        assert_eq!(camel_words("API_BASE"), vec!["API", "BASE"]);
        assert_eq!(camel_words("a-b"), vec!["a", "b"]);
    }
}
