//! A route's address as the parts it is composed of (carrick-cloud#3083,
//! slice S1). `docs/reference/address-chain.md` is the reference for the stored shape.
//!
//! An address lists the parts outermost first, ending with the row's own
//! part. Each part says what kind of statement declares it, the `file:line`
//! that states it, and its value: literal text, a constant the scan folded
//! (with the file that declares it), or a value the scan cannot read (with
//! why). An unreadable part is written as unknown, never dropped.
//!
//! The vocabulary names no framework: the store label decides which kind a
//! member is.

use serde::{Deserialize, Serialize};

/// The parts a row's address is composed of, outermost first, ending with
/// the row's own part, and how much of it is read.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Address {
    pub parts: Vec<Part>,
    /// Derived from the parts ([`Reading::of`]), stored so a reader need not
    /// derive it again.
    pub reading: Reading,
}

impl Address {
    /// The address `parts` compose, with its reading.
    pub fn of(parts: Vec<Part>) -> Self {
        let reading = Reading::of(&parts);
        Self { parts, reading }
    }
}

/// One statement an address is composed of.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Part {
    pub kind: PartKind,
    /// Which axis of the operation's key the part extends.
    pub dimension: Dimension,
    /// `file:line`, repo-relative, of the call that states it.
    pub at: String,
    /// The member as written (`mount`, `register`, `get`). Display only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member: Option<String>,
    pub value: Value,
    /// Where the statement was read from.
    pub via: Via,
}

/// What declares a part.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PartKind {
    /// A member that serves another instance's routes under its prefix.
    Mount,
    /// A member that opens a scope whose routes are served under its
    /// prefix.
    Scope,
    /// The row's own call: the path it writes.
    Own,
}

/// The axis of an operation's key a part extends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Dimension {
    Path,
}

/// Where a part was read from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Via {
    /// A call the library store's verified label names.
    Label,
}

/// What a part states.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "value", rename_all = "snake_case")]
pub enum Value {
    /// One entry per value the statement states, each as its pieces.
    Text { values: Vec<Vec<Piece>> },
    /// The member writes nothing there: it adds nothing to the address.
    Unstated,
}

/// One piece of a stated value.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "piece", rename_all = "snake_case")]
pub enum Piece {
    /// Text the source writes at the part's own call.
    Lit { text: String },
    /// A constant the scan folded to text: `from` is the repo-relative file
    /// that declares it.
    Folded { text: String, from: String },
    /// A value the scan does not read as text, as written, with why and the
    /// names it reads (an environment variable's name).
    Unknown {
        written: String,
        reason: UnknownReason,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        reads: Vec<String>,
    },
}

impl Piece {
    /// The text a read piece states; `None` for an unknown one.
    pub fn text(&self) -> Option<&str> {
        match self {
            Piece::Lit { text } | Piece::Folded { text, .. } => Some(text),
            Piece::Unknown { .. } => None,
        }
    }
}

/// Why a piece is unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnknownReason {
    /// An environment read: set when the process starts.
    RuntimeEnv,
    /// A parameter whose callers the scan does not settle.
    Parameter,
    /// A conditional or a default whose ways the scan does not settle.
    Conditional,
    /// Any other value this pass does not read.
    Unread,
}

/// How much of an address is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reading {
    /// Every piece is read.
    Whole,
    /// Every unknown piece comes before the first read one: the address is
    /// known from its readable tail on.
    Headless,
    /// A whole segment after a read piece is unknown.
    Gap,
    /// The row's own part is unknown, or a piece inside a segment is.
    Unread,
}

impl Reading {
    /// The reading of `parts`, the last being the row's own.
    pub fn of(parts: &[Part]) -> Self {
        let pieces_of = |part: &Part| -> Vec<Piece> {
            match &part.value {
                Value::Text { values } => values.iter().flatten().cloned().collect(),
                Value::Unstated => Vec::new(),
            }
        };
        let own_unknown = parts.last().is_some_and(|own| {
            own.kind == PartKind::Own
                && pieces_of(own)
                    .iter()
                    .any(|piece| matches!(piece, Piece::Unknown { .. }))
        });
        if own_unknown {
            return Reading::Unread;
        }
        // Every piece in order, each part's starting a segment of its own.
        let mut flat: Vec<(Piece, bool)> = Vec::new();
        for part in parts {
            for (index, piece) in pieces_of(part).into_iter().enumerate() {
                if piece.text().is_some_and(str::is_empty) {
                    continue;
                }
                flat.push((piece, index == 0));
            }
        }
        let mut read_before = false;
        let mut reading = Reading::Whole;
        for (index, (piece, starts_part)) in flat.iter().enumerate() {
            match piece.text() {
                Some(_) => read_before = true,
                None if !read_before => reading = Reading::Headless,
                None => {
                    // A whole segment: a slash (or a part's start) before it,
                    // and a slash (or a part's start, or the end) after it.
                    let opens = *starts_part
                        || flat[index - 1]
                            .0
                            .text()
                            .is_some_and(|text| text.ends_with('/'));
                    let closes = flat.get(index + 1).is_none_or(|(next, next_starts)| {
                        *next_starts || next.text().is_some_and(|text| text.starts_with('/'))
                    });
                    if !(opens && closes) {
                        return Reading::Unread;
                    }
                    reading = Reading::Gap;
                }
            }
        }
        reading
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lit(text: &str) -> Piece {
        Piece::Lit {
            text: text.to_string(),
        }
    }

    fn env(name: &str) -> Piece {
        Piece::Unknown {
            written: format!("process.env.{name}"),
            reason: UnknownReason::RuntimeEnv,
            reads: vec![name.to_string()],
        }
    }

    fn part(kind: PartKind, pieces: Vec<Piece>) -> Part {
        Part {
            kind,
            dimension: Dimension::Path,
            at: "src/server.ts:1".to_string(),
            member: None,
            value: Value::Text {
                values: vec![pieces],
            },
            via: Via::Label,
        }
    }

    /// The reading is derived from where the unknown pieces sit: none is
    /// whole, all before the first read piece is headless, a whole segment
    /// after one is a gap, and one inside a segment or in the own part is
    /// unread.
    #[test]
    fn the_reading_follows_where_the_unknown_pieces_sit() {
        let own = |text: &str| part(PartKind::Own, vec![lit(text)]);
        let reading = |parts: Vec<Part>| Address::of(parts).reading;
        assert_eq!(
            reading(vec![part(PartKind::Mount, vec![lit("/api")]), own("/x")]),
            Reading::Whole
        );
        assert_eq!(
            reading(vec![part(PartKind::Mount, vec![env("BASE")]), own("/x")]),
            Reading::Headless
        );
        assert_eq!(
            reading(vec![
                part(PartKind::Mount, vec![env("BASE"), lit("v1")]),
                own("/x")
            ]),
            Reading::Headless
        );
        assert_eq!(
            reading(vec![
                part(PartKind::Mount, vec![lit("/api")]),
                part(PartKind::Scope, vec![env("TENANT")]),
                own("/x")
            ]),
            Reading::Gap
        );
        assert_eq!(
            reading(vec![
                part(PartKind::Mount, vec![lit("/api/v"), env("VERSION")]),
                own("/x")
            ]),
            Reading::Unread
        );
        assert_eq!(
            reading(vec![part(PartKind::Own, vec![lit("/x/"), env("TAIL")])]),
            Reading::Unread
        );
    }

    /// An unknown piece is stored with its reason and what it reads, and the
    /// stored shape reads back as written.
    #[test]
    fn an_address_round_trips_and_a_part_keeps_its_unknown_reason() {
        let address = Address::of(vec![
            part(PartKind::Mount, vec![env("ADMIN_BASE")]),
            part(PartKind::Own, vec![lit("/stats")]),
        ]);
        let json = serde_json::to_value(&address).unwrap();
        assert_eq!(
            json["parts"][0]["value"],
            serde_json::json!({
                "value": "text",
                "values": [[{
                    "piece": "unknown",
                    "written": "process.env.ADMIN_BASE",
                    "reason": "runtime_env",
                    "reads": ["ADMIN_BASE"],
                }]],
            })
        );
        assert_eq!(json["reading"], "headless");
        let back: Address = serde_json::from_value(json).unwrap();
        assert_eq!(back, address);
    }
}
