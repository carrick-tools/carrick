//! Stub for the failing-first commit (carrick#4133).
use std::path::Path;

pub const STAMP_FILE: &str = "engine-stamp.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineOrigin {
    EnvVar,
    NextToBinary,
    SourceCheckout,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StampVerdict {
    Ok { stamp: String },
    Warn(String),
    Refuse(String),
}

pub fn hash_inputs(_root: &Path) -> std::io::Result<String> {
    Ok(String::new())
}

pub fn read_stamp(_entry: &Path) -> Option<String> {
    None
}

pub fn check(_entry: &Path, _origin: EngineOrigin) -> StampVerdict {
    StampVerdict::Ok {
        stamp: String::new(),
    }
}
