//! The functions a function the edit just wrote may already exist as
//! (carrick-cloud#3490).
//!
//! At edit time the new function has no description and no vector, so it is
//! asked by what one parse gives: its name, its body tokens and its written
//! signature. The ranking is [`carrick_match::SimilarIndex`], the one answerer
//! of the question (BM25 over a bag of each function's name, intent, tokens and
//! signature, no score floor, at most [`carrick_match::SIMILAR_ROWS`] rows).
//! This module reads the blobs the re-check already holds in memory (local
//! blobs and the cached hosted snapshot, every repo), gives each indexed
//! function to the ranker with its own intent and signature when the index has
//! them, and adds what a reader needs beside each row: where it is, whether it
//! is exported, and how many other files call it. No network, no model.
//!
//! A row nobody can import is never a candidate: the callbacks the extractor
//! named itself and the module-scope row. Copies of one file in several
//! services count once. Reference: `docs/local-mode-output.md`
//! (`new_functions[].similar`).
//!
//! A built [`Similar`] owns its rows, so it can be kept on disk and read back,
//! and it ranks the same as the one that was built.

use std::collections::{HashMap, HashSet};

use carrick_match::{SimilarDoc, SimilarIndex, SimilarQuery};

use crate::cloud_storage::CloudRepoData;
use crate::visitor::{FunctionDefinition, MODULE_SCOPE_KEY};

use super::contract::SimilarFunction;

/// The suffix [`crate::visitor::derive_handler_name`] gives a callback it names.
const SYNTHETIC_SUFFIX: &str = "_handler";

/// A function the edit wrote, as one parse of the edited file states it.
#[derive(Debug, Clone)]
pub(crate) struct Query {
    /// As the source spells it: `name`, or `Class.member`.
    pub name: String,
    pub tokens: Vec<String>,
    /// The names its body calls, as written at each call site.
    pub callees: Vec<String>,
    /// The signature its source writes, composed as the index composes its own.
    pub signature: Option<String>,
}

impl Query {
    /// A function with nothing to score but its name.
    pub(crate) fn named(name: &str) -> Self {
        Self {
            name: name.to_string(),
            tokens: Vec::new(),
            callees: Vec::new(),
            signature: None,
        }
    }

    pub(crate) fn of(definition: &FunctionDefinition, callees: Vec<String>) -> Self {
        Self {
            name: definition.name.clone(),
            tokens: definition.tokens.clone(),
            callees,
            signature: Some(
                definition
                    .signature
                    .clone()
                    .unwrap_or_else(|| crate::signature_pass::compose_signature(definition)),
            ),
        }
    }
}

/// One indexed function, with what a row needs beside the ranking. Owned, so a
/// built [`Similar`] can be written to disk and read back.
#[derive(serde::Serialize, serde::Deserialize)]
struct Entry {
    repo: String,
    file: String,
    /// The repo and file together, lower-cased: two functions with one key are
    /// in one file.
    key: String,
    name: String,
    line: u32,
    end_line: u32,
    signature: Option<String>,
    exported: bool,
    intent: Option<String>,
    has_tokens: bool,
    /// How many other files hold a call that names the function.
    called_from: usize,
}

/// Every indexed function the re-check holds, read once per re-check call and
/// ranked against every function the edit wrote.
#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct Similar {
    entries: Vec<Entry>,
    index: SimilarIndex,
}

#[cfg(test)]
thread_local! {
    /// How many times this thread built an index: one per re-check call.
    pub(super) static BUILDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn file_key(repo: &str, file: &str) -> String {
    format!("{}\u{0}{file}", repo.to_lowercase())
}

impl Similar {
    /// Read `blobs`, each labelled with the repo it belongs to.
    pub(crate) fn build<'a>(blobs: impl IntoIterator<Item = (String, &'a CloudRepoData)>) -> Self {
        Self::build_except(blobs, &HashSet::new())
    }

    /// [`Self::build`], leaving out the functions in `except`, keyed on
    /// (lower-cased repo, file as the blob spells it, name): a PR run leaves
    /// out what the PR itself changed or removed.
    pub(crate) fn build_except<'a>(
        blobs: impl IntoIterator<Item = (String, &'a CloudRepoData)>,
        except: &HashSet<(String, String, String)>,
    ) -> Self {
        #[cfg(test)]
        BUILDS.with(|builds| builds.set(builds.get() + 1));
        let mut entries = Vec::new();
        let mut definitions: Vec<&FunctionDefinition> = Vec::new();
        let mut callers: HashMap<(String, String, String), HashSet<String>> = HashMap::new();
        let mut seen: HashSet<(String, String, String)> = HashSet::new();
        for (repo, blob) in blobs {
            let repo_key = repo.to_lowercase();
            for definition in blob.function_definitions.values() {
                if !is_candidate(definition) {
                    continue;
                }
                let file = path_of(definition);
                let at = (repo_key.clone(), file.clone(), definition.name.clone());
                if except.contains(&at) || !seen.insert(at) {
                    continue;
                }
                let key = file_key(&repo, &file);
                for call in &definition.calls {
                    callers
                        .entry((
                            repo_key.clone(),
                            call.file_path.replace('\\', "/"),
                            member(&call.name).to_string(),
                        ))
                        .or_default()
                        .insert(key.clone());
                }
                entries.push(Entry {
                    repo: repo.clone(),
                    file,
                    key,
                    name: definition.name.clone(),
                    line: definition.line_number,
                    end_line: definition.end_line,
                    signature: definition.signature.clone(),
                    exported: definition.is_exported,
                    intent: definition.intent.clone(),
                    has_tokens: !definition.tokens.is_empty(),
                    called_from: 0,
                });
                definitions.push(definition);
            }
        }
        for (entry, definition) in entries.iter_mut().zip(&definitions) {
            let key = (
                entry.repo.to_lowercase(),
                entry.file.clone(),
                member(&definition.name).to_string(),
            );
            entry.called_from = callers.get(&key).map_or(0, |files| {
                files.iter().filter(|caller| **caller != entry.key).count()
            });
        }
        let index =
            SimilarIndex::build(entries.iter().zip(&definitions).map(|(entry, definition)| {
                SimilarDoc {
                    name: &definition.name,
                    file: &entry.key,
                    intent: definition.intent.as_deref(),
                    tokens: &definition.tokens,
                    signature: definition.signature.as_deref(),
                    exported: definition.is_exported,
                    line: definition.line_number,
                    calls: definition
                        .calls
                        .iter()
                        .map(|call| call.name.as_str())
                        .collect(),
                }
            }));
        Self { entries, index }
    }

    /// How many functions the ranker holds, and how many of them carry tokens.
    pub(crate) fn size(&self) -> (usize, usize) {
        let with_tokens = self.entries.iter().filter(|entry| entry.has_tokens).count();
        (self.entries.len(), with_tokens)
    }

    /// The functions most like `query`, written in `file` of `repo`, best first.
    pub(crate) fn similar(&self, query: &Query, repo: &str, file: &str) -> Vec<SimilarFunction> {
        let edited = file_key(repo, &file.replace('\\', "/"));
        let ranked = self.index.rank(&SimilarQuery {
            name: &query.name,
            file: &edited,
            tokens: &query.tokens,
            signature: query.signature.as_deref(),
            callees: &query.callees,
        });
        ranked
            .into_iter()
            .map(|row| {
                let entry = &self.entries[row.doc];
                SimilarFunction {
                    name: entry.name.clone(),
                    repo: entry.repo.clone(),
                    file: entry.file.clone(),
                    line: entry.line,
                    end_line: entry.end_line,
                    score: (row.score * 1000.0).round() / 1000.0,
                    signature: entry.signature.clone(),
                    exported: entry.exported,
                    called_from: entry.called_from,
                    intent: entry.intent.clone(),
                }
            })
            .collect()
    }
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
