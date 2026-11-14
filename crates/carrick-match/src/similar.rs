//! Which indexed functions a function just written looks like (carrick-cloud#3490).
//!
//! STUB: the types and signatures only, so the failing tests compile and fail.

/// Rows one answer holds at most.
pub const SIMILAR_ROWS: usize = 6;

/// An indexed function, as the ranker reads it.
pub struct SimilarDoc<'a> {
    pub name: &'a str,
    pub file: &'a str,
    pub intent: Option<&'a str>,
    pub tokens: &'a [String],
    pub signature: Option<&'a str>,
    pub calls: Vec<&'a str>,
}

/// The function just written.
pub struct SimilarQuery<'a> {
    pub name: &'a str,
    pub file: &'a str,
    pub tokens: &'a [String],
    pub signature: Option<&'a str>,
    pub callees: &'a [String],
}

/// One ranked function: its position in the iterator given to
/// [`SimilarIndex::build`], and its score.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SimilarRow {
    pub doc: usize,
    pub score: f64,
}

/// A function's searchable words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bag {
    pub terms: Vec<String>,
    pub described_len: usize,
}

pub fn tokenize(_text: &str) -> Vec<String> {
    Vec::new()
}

pub fn bag(
    _name: &str,
    _intent: Option<&str>,
    _tokens: &[String],
    _signature: Option<&str>,
) -> Bag {
    Bag {
        terms: Vec::new(),
        described_len: 0,
    }
}

pub struct SimilarIndex;

impl SimilarIndex {
    pub fn build<'a>(_docs: impl IntoIterator<Item = SimilarDoc<'a>>) -> Self {
        SimilarIndex
    }

    pub fn rank(&self, _query: &SimilarQuery<'_>) -> Vec<SimilarRow> {
        Vec::new()
    }
}
